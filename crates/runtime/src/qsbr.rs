//! Safe memory reclamation for lock-free readers (DECISIONS D5/D6; perf-runtime,
//! reworked for multi-core scaling by scale1).
//!
//! Lock-free readers (inline attribute values, dict entries read by the inline caches
//! and JIT code without the object lock) may load a pointer that a concurrent writer
//! is about to release. While more than one thread is attached, writers therefore
//! *retire* such pointers instead of releasing them immediately; a retired item is
//! released once every thread executing Python has passed a quiescent point after the
//! retirement (quiescent-state based reclamation, HotSpot handshake flavour):
//!
//! * a global sequence counter `GLOBAL`;
//! * every thread state publishes the value of `GLOBAL` it saw at its last quiescent
//!   point (`ThreadState::qsbr_seen`; an eval-breaker poll at a backedge/call, which
//!   never happens in the middle of a lock-free read); threads in native/blocked mode
//!   count as quiescent;
//! * retired items collect in a per-thread open batch; the batch is closed (goal =
//!   `GLOBAL.fetch_add(1) + 1`) when it is big enough or after a number of polls, and
//!   closing it asks the other threads to report (sets `EVAL_QSBR` in their eval
//!   breakers, i.e. a handshake);
//! * a closed batch is released when every running thread's published value >= goal.
//!
//! Scalability (scale1): nothing on the per-item or per-poll path takes a lock or
//! allocates. The set of thread states is mirrored into a fixed array guarded by a
//! sequence lock (`publish`, written under the `ALL_THREADS` lock by `note_threads`),
//! so the minimum over the published values is a plain scan; the scan runs at most
//! every `SCAN_EVERY` polls while a batch waits; batches hold up to `BATCH` items so
//! the cross-thread handshake is rare; immortal objects are never retired.
//!
//! With a single attached thread nothing is deferred (and anything still pending is
//! released at the next poll), so single-threaded programs keep exact CPython
//! finalization timing. With several threads, finalizers of retired objects run a few
//! polls later (documented in notes/perf-runtime.md).

use crate::ffi::PyObject;
use crate::threads::ThreadState;
use std::cell::UnsafeCell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::*};

/// Eval-breaker bit: report a quiescent state / process deferred batches.
pub const EVAL_QSBR: usize = 1 << 5;

/// Number of attached thread states.
static LIVE: AtomicUsize = AtomicUsize::new(0);
static GLOBAL: AtomicU64 = AtomicU64::new(1);

/// True while more than one thread state is attached.
#[inline(always)]
pub fn multi_threaded() -> bool {
    LIVE.load(Relaxed) > 1
}

// ---------------------------------------------------------------- thread-state mirror

const CAP: usize = 2048;
static LIST: [AtomicUsize; CAP] = [const { AtomicUsize::new(0) }; CAP];
static LIST_LEN: AtomicUsize = AtomicUsize::new(0);
/// Sequence lock over LIST/LIST_LEN: odd while being rewritten.
static LIST_VER: AtomicU64 = AtomicU64::new(0);
/// More thread states than CAP: scans fall back to the locked `ALL_THREADS` list.
static OVERFLOW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Called (with the `ALL_THREADS` lock held) whenever a thread state is created or
/// retired; `all` is the new list.
pub fn note_threads(all: &[usize]) {
    LIVE.store(all.len(), SeqCst);
    LIST_VER.fetch_add(1, SeqCst);
    if all.len() > CAP {
        OVERFLOW.store(true, SeqCst);
    } else {
        for (i, &t) in all.iter().enumerate() {
            LIST[i].store(t, Relaxed);
        }
        LIST_LEN.store(all.len(), Relaxed);
        OVERFLOW.store(false, SeqCst);
    }
    LIST_VER.fetch_add(1, SeqCst);
}

/// Call `f` for every registered thread state (a consistent snapshot of the list).
fn for_each_thread(mut f: impl FnMut(&ThreadState)) {
    loop {
        let v0 = LIST_VER.load(SeqCst);
        if v0 & 1 == 1 {
            std::hint::spin_loop();
            continue;
        }
        if OVERFLOW.load(SeqCst) {
            let threads: Vec<usize> = crate::threads::ALL_THREADS.lock().unwrap_or_else(|e| e.into_inner()).clone();
            for t in threads {
                f(unsafe { &*(t as *const ThreadState) });
            }
            return;
        }
        // Thread-state memory is never freed (dead states are recycled), so reading a
        // pointer of a concurrently retired state is harmless; the version check
        // below decides whether the snapshot was consistent.
        let n = LIST_LEN.load(Relaxed).min(CAP);
        let mut buf = [0usize; 64];
        if n <= buf.len() {
            for (i, b) in buf.iter_mut().enumerate().take(n) {
                *b = LIST[i].load(Relaxed);
            }
            if LIST_VER.load(SeqCst) != v0 {
                continue;
            }
            for &t in &buf[..n] {
                f(unsafe { &*(t as *const ThreadState) });
            }
            return;
        }
        let v: Vec<usize> = (0..n).map(|i| LIST[i].load(Relaxed)).collect();
        if LIST_VER.load(SeqCst) != v0 {
            continue;
        }
        for t in v {
            f(unsafe { &*(t as *const ThreadState) });
        }
        return;
    }
}

// ---------------------------------------------------------------- per-thread batches

enum Item {
    Decref(*mut PyObject),
    Free(*mut u8, unsafe fn(*mut u8)),
}

struct Local {
    open: Vec<Item>,
    /// Closed batches waiting for their goal (goals increase).
    closed: std::collections::VecDeque<(u64, Vec<Item>)>,
    polls: u32,
    /// Last computed minimum over the other threads' published values.
    known_min: u64,
    /// Polls left until the next scan of the other threads.
    scan_in: u32,
}

#[thread_local]
static LOCAL: UnsafeCell<Local> = UnsafeCell::new(Local {
    open: Vec::new(),
    closed: std::collections::VecDeque::new(),
    polls: 0,
    known_min: 0,
    scan_in: 0,
});

/// Items retired by threads that exited before their batches matured.
static ORPHANS: Mutex<Vec<(u64, Vec<ItemSend>)>> = Mutex::new(Vec::new());
/// Smallest goal among the orphaned batches (u64::MAX: none).
static ORPHAN_MIN: AtomicU64 = AtomicU64::new(u64::MAX);
struct ItemSend(Item);
unsafe impl Send for ItemSend {}

/// Close the open batch once it holds this many items ...
const BATCH: usize = 1024;
/// ... or after this many eval-breaker polls.
const POLLS: u32 = 1024;
/// While a closed batch waits, rescan the other threads every this many polls.
const SCAN_EVERY: u32 = 16;
static PENDING_TOTAL: AtomicUsize = AtomicUsize::new(0);

#[inline(always)]
fn local() -> &'static mut Local {
    unsafe { &mut *LOCAL.get() }
}

#[inline(always)]
fn me() -> *const ThreadState {
    crate::threads::current_ptr()
}

/// Initial published value for a new thread state: it cannot hold pointers that
/// were retired before it existed.
pub fn initial_seen() -> u64 {
    GLOBAL.load(SeqCst)
}

#[inline(always)]
fn report(t: *const ThreadState) {
    if !t.is_null() {
        unsafe { (*t).qsbr_seen.store(GLOBAL.load(SeqCst), SeqCst) };
    }
}

/// Release `p` (one reference) once no thread can still be reading it lock-free.
#[inline]
pub unsafe fn decref_deferred(p: *mut PyObject) {
    unsafe {
        if !multi_threaded() {
            crate::object::decref(p);
            return;
        }
        if (*p).is_immortal() {
            return;
        }
    }
    retire(Item::Decref(p));
}

/// Store `new` (an owned reference or null) into an object field that other threads
/// read without a lock (getters taking a new reference to the current value), and
/// release the previous value once no such reader can still hold the old pointer.
pub unsafe fn swap_field(slot: *mut *mut PyObject, new: *mut PyObject) {
    unsafe {
        let old = std::sync::atomic::AtomicPtr::from_ptr(slot).swap(new, AcqRel);
        if !old.is_null() {
            decref_deferred(old);
        }
    }
}

/// Free raw storage `p` with `f` once no thread can still be reading it lock-free.
pub unsafe fn free_deferred(p: *mut u8, f: unsafe fn(*mut u8)) {
    if !multi_threaded() {
        unsafe { f(p) };
        return;
    }
    retire(Item::Free(p, f));
}

#[inline]
fn retire(it: Item) {
    let l = local();
    if l.open.is_empty() {
        retire_first();
    }
    l.open.push(it);
    if l.open.len() >= BATCH {
        retire_full();
    }
}

#[cold]
fn retire_first() {
    // come back at the next polls to close the batch
    let t = me();
    if !t.is_null() {
        unsafe { (*t).c.eval_breaker.fetch_or(EVAL_QSBR, Release) };
    }
    let l = local();
    if l.open.capacity() == 0 {
        l.open.reserve(BATCH);
    }
}

#[cold]
fn retire_full() {
    close_batch();
    poll(true);
}

fn close_batch() {
    let l = local();
    l.polls = 0;
    if l.open.is_empty() {
        return;
    }
    let items = std::mem::replace(&mut l.open, Vec::new());
    if crate::scalestats::on() {
        crate::scalestats::QSBR_BATCHES.fetch_add(1, Relaxed);
        crate::scalestats::QSBR_ITEMS.fetch_add(items.len() as u64, Relaxed);
    }
    PENDING_TOTAL.fetch_add(items.len(), Relaxed);
    let goal = GLOBAL.fetch_add(1, SeqCst) + 1;
    l.closed.push_back((goal, items));
    // (no report here: a batch can be closed in the middle of an operation that holds
    // pointers read without a lock; only eval-breaker polls are quiescent points)
    // handshake: ask every other thread to report a quiescent state soon
    let me = me() as usize;
    for_each_thread(|ts| {
        if ts as *const ThreadState as usize != me && ts.alive() {
            ts.c.eval_breaker.fetch_or(EVAL_QSBR, Release);
        }
    });
}

/// Smallest sequence value published by a running thread other than the caller.
fn min_seen() -> u64 {
    let me = me() as usize;
    let mut m = u64::MAX;
    for_each_thread(|ts| {
        if ts as *const ThreadState as usize == me {
            return;
        }
        if !ts.alive() || ts.mode.get() != crate::threads::ThreadMode::InVm {
            return;
        }
        m = m.min(ts.qsbr_seen.load(SeqCst));
    });
    m
}

unsafe fn release(items: Vec<Item>) {
    PENDING_TOTAL.fetch_sub(items.len(), Relaxed);
    for it in items {
        match it {
            Item::Decref(p) => unsafe { crate::object::decref(p) },
            Item::Free(p, f) => unsafe { f(p) },
        }
    }
}

/// Process matured batches of the calling thread (and orphans). `scan`: recompute
/// the minimum now (otherwise only when the countdown expires).
fn poll(scan: bool) {
    let l = local();
    let orphan_goal = ORPHAN_MIN.load(Relaxed);
    let first_goal = l.closed.front().map_or(u64::MAX, |b| b.0);
    let need = first_goal.min(orphan_goal);
    if need == u64::MAX {
        return;
    }
    if need > l.known_min {
        if !multi_threaded() {
            l.known_min = u64::MAX;
        } else if scan || l.scan_in == 0 {
            l.scan_in = SCAN_EVERY;
            // Capped by GLOBAL as read before the scan: a thread skipped because it
            // was blocked is only known to be quiescent for batches closed so far.
            let g = GLOBAL.load(SeqCst);
            l.known_min = min_seen().min(g);
        } else {
            l.scan_in -= 1;
        }
    }
    let m = l.known_min;
    if !multi_threaded() {
        // alone now: nobody can read anything retired; start afresh next time
        l.known_min = 0;
    }
    let mut ready = Vec::new();
    while let Some(b) = l.closed.front() {
        if b.0 > m {
            break;
        }
        ready.push(l.closed.pop_front().unwrap().1);
    }
    let mut orphan_ready: Vec<Vec<ItemSend>> = Vec::new();
    if orphan_goal <= m {
        let mut o = ORPHANS.lock().unwrap_or_else(|e| e.into_inner());
        o.retain_mut(|(goal, items)| {
            if *goal <= m {
                orphan_ready.push(std::mem::take(items));
                false
            } else {
                true
            }
        });
        ORPHAN_MIN.store(o.iter().map(|b| b.0).min().unwrap_or(u64::MAX), Relaxed);
    }
    for b in ready {
        unsafe { release(b) };
    }
    for b in orphan_ready {
        unsafe { release(b.into_iter().map(|i| i.0).collect()) };
    }
}

/// Eval-breaker handler (`EVAL_QSBR`): this thread is at a quiescent point.
pub fn on_eval_breaker(ts: &ThreadState) {
    ts.c.eval_breaker.fetch_and(!EVAL_QSBR, AcqRel);
    report(ts);
    let l = local();
    if !l.open.is_empty() {
        l.polls += 1;
        if l.polls >= POLLS || !multi_threaded() {
            close_batch();
        }
    }
    poll(false);
    let l = local();
    if !l.closed.is_empty() || !l.open.is_empty() || ORPHAN_MIN.load(Relaxed) != u64::MAX {
        // not yet matured: check again at the next poll
        ts.c.eval_breaker.fetch_or(EVAL_QSBR, Release);
    }
}

/// The calling thread is exiting: hand pending batches over and let the remaining
/// threads re-check (they may be alone now).
pub fn thread_exit() {
    let l = local();
    if !l.open.is_empty() {
        close_batch();
    }
    let closed = std::mem::take(&mut l.closed);
    if std::env::var_os("RPY_QSBR_DEBUG").is_some() {
        eprintln!("qsbr: thread {:p} exits with {} closed batches (goals {:?}), GLOBAL={}", me(), closed.len(),
            closed.iter().map(|b| b.0).collect::<Vec<_>>(), GLOBAL.load(SeqCst));
    }
    if !closed.is_empty() {
        let mut o = ORPHANS.lock().unwrap_or_else(|e| e.into_inner());
        for (g, items) in closed {
            o.push((g, items.into_iter().map(ItemSend).collect()));
        }
        ORPHAN_MIN.store(o.iter().map(|b| b.0).min().unwrap_or(u64::MAX), Relaxed);
    }
    l.known_min = 0;
    l.scan_in = 0;
    let me = me() as usize;
    for_each_thread(|ts| {
        if ts as *const ThreadState as usize != me {
            ts.c.eval_breaker.fetch_or(EVAL_QSBR, Release);
        }
    });
}

/// The caller is at a quiescent point and wants deferred releases done as far as
/// possible right now (thread join: references the joined thread dropped while other
/// threads existed sit in batches it handed over at exit; `Thread.join()` followed by
/// a weakref check must see them released). With no other attached thread, every
/// pending item of this thread and every orphaned batch is released immediately.
pub fn quiesce_now() {
    if crate::threads::current_ptr().is_null() {
        return;
    }
    report(me());
    if std::env::var_os("RPY_QSBR_DEBUG").is_some() {
        eprintln!("qsbr: quiesce mt={} pending={} orphans={}", multi_threaded(), PENDING_TOTAL.load(SeqCst), ORPHAN_MIN.load(SeqCst));
    }
    if !multi_threaded() {
        close_batch();
        poll(true);
        return;
    }
    // Other threads are running: ask them to report a quiescent state and wait a
    // little (the caller is blocking in join anyway) for the orphaned batches of the
    // joined thread to mature. Threads in native/blocked mode count as quiescent, so
    // this only waits for threads executing Python, which poll at backedges and calls.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(50);
    loop {
        poll(true);
        let pending_orphans = ORPHAN_MIN.load(SeqCst) != u64::MAX;
        if !pending_orphans || std::time::Instant::now() >= deadline {
            if pending_orphans && std::env::var_os("RPY_QSBR_DEBUG").is_some() {
                eprintln!("qsbr: orphans left; LIVE={} GLOBAL={}", LIVE.load(SeqCst), GLOBAL.load(SeqCst));
                for_each_thread(|ts| {
                    eprintln!("  ts {:#x} alive={} mode={:?} seen={} me={} tid={} native={} frame={:?}", ts as *const ThreadState as usize,
                        ts.alive(), ts.mode.get(), ts.qsbr_seen.load(SeqCst), std::ptr::eq(ts, me()), ts.c.thread_id, ts.c.native_thread_id, ts.frame.get());
                });
            }
            break;
        }
        let me = me() as usize;
        for_each_thread(|ts| {
            if ts as *const ThreadState as usize != me {
                ts.c.eval_breaker.fetch_or(EVAL_QSBR, Release);
            }
        });
        crate::threads::blocking(|| std::thread::sleep(std::time::Duration::from_micros(200)));
        report(me as *const ThreadState);
    }
}

/// Number of retired items not yet released (diagnostics/tests).
pub fn pending() -> usize {
    PENDING_TOTAL.load(Relaxed) + local().open.len()
}
