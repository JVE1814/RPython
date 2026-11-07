//! Safe memory reclamation for lock-free readers (DECISIONS D5/D6; perf-runtime).
//!
//! Lock-free readers (inline attribute values, dict entries read by the inline caches
//! and JIT code without the object lock) may load a pointer that a concurrent writer
//! is about to release. While more than one thread is attached, writers therefore
//! *retire* such pointers instead of releasing them immediately; a retired item is
//! released once every thread executing Python has passed a quiescent point after the
//! retirement (quiescent-state based reclamation, HotSpot handshake flavour):
//!
//! * a global sequence counter `GLOBAL`;
//! * every thread publishes the value of `GLOBAL` it saw at its last quiescent point
//!   (an eval-breaker poll at a backedge/call, which never happens in the middle of a
//!   lock-free read); threads in native/blocked mode count as quiescent;
//! * retired items collect in a per-thread open batch; the batch is closed (goal =
//!   `GLOBAL.fetch_add(1) + 1`) when it is big enough or after a number of polls, and
//!   closing it asks the other threads to report (sets `EVAL_QSBR` in their eval
//!   breakers, i.e. a handshake);
//! * a closed batch is released when every running thread's published value >= goal.
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

/// Called whenever a thread state is created or retired; `n` = attached states.
pub fn note_threads(n: usize) {
    LIVE.store(n, SeqCst);
}

/// Per-thread record, registered on first report.
struct Rec {
    ts: usize,
    seen: AtomicU64,
}

static RECS: Mutex<Vec<&'static Rec>> = Mutex::new(Vec::new());

enum Item {
    Decref(*mut PyObject),
    Free(*mut u8, unsafe fn(*mut u8)),
}

struct Local {
    rec: *const Rec,
    open: Vec<Item>,
    /// Closed batches waiting for their goal.
    closed: Vec<(u64, Vec<Item>)>,
    polls: u32,
}

#[thread_local]
static LOCAL: UnsafeCell<Local> = UnsafeCell::new(Local { rec: std::ptr::null(), open: Vec::new(), closed: Vec::new(), polls: 0 });

/// Items retired by threads that exited before their batches matured.
static ORPHANS: Mutex<Vec<(u64, Vec<ItemSend>)>> = Mutex::new(Vec::new());
struct ItemSend(Item);
unsafe impl Send for ItemSend {}

/// Close the open batch once it holds this many items ...
const BATCH: usize = 128;
/// ... or after this many eval-breaker polls.
const POLLS: u32 = 64;
static PENDING_TOTAL: AtomicUsize = AtomicUsize::new(0);

#[inline(always)]
fn local() -> &'static mut Local {
    unsafe { &mut *LOCAL.get() }
}

fn my_rec() -> &'static Rec {
    let l = local();
    if l.rec.is_null() {
        let ts = crate::threads::current_ptr() as usize;
        let r: &'static Rec = Box::leak(Box::new(Rec { ts, seen: AtomicU64::new(GLOBAL.load(SeqCst)) }));
        RECS.lock().unwrap().push(r);
        l.rec = r;
    }
    unsafe { &*l.rec }
}

#[inline(always)]
fn me() -> *const ThreadState {
    crate::threads::current_ptr()
}

/// Release `p` (one reference) once no thread can still be reading it lock-free.
#[inline]
pub unsafe fn decref_deferred(p: *mut PyObject) {
    if !multi_threaded() {
        unsafe { crate::object::decref(p) };
        return;
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

#[cold]
fn retire(it: Item) {
    let l = local();
    if l.open.is_empty() {
        // come back at the next polls to close the batch
        let t = me();
        if !t.is_null() {
            unsafe { (*t).c.eval_breaker.fetch_or(EVAL_QSBR, Release) };
        }
    }
    l.open.push(it);
    if l.open.len() >= BATCH {
        close_batch();
        poll();
    }
}

fn close_batch() {
    let l = local();
    l.polls = 0;
    if l.open.is_empty() {
        return;
    }
    let items = std::mem::take(&mut l.open);
    PENDING_TOTAL.fetch_add(items.len(), Relaxed);
    let goal = GLOBAL.fetch_add(1, SeqCst) + 1;
    l.closed.push((goal, items));
    // handshake: ask every other thread to report a quiescent state soon
    let me = me() as usize;
    for t in crate::threads::ALL_THREADS.lock().unwrap().iter() {
        let ts = unsafe { &*(*t as *const ThreadState) };
        if *t != me && ts.alive() {
            ts.c.eval_breaker.fetch_or(EVAL_QSBR, Release);
        }
    }
}

/// Smallest sequence value published by a running thread other than the caller.
fn min_seen() -> u64 {
    let me = me() as usize;
    let threads: Vec<usize> = crate::threads::ALL_THREADS.lock().unwrap().clone();
    let recs = RECS.lock().unwrap();
    let mut m = u64::MAX;
    for t in threads {
        if t == me {
            continue;
        }
        let ts = unsafe { &*(t as *const ThreadState) };
        if !ts.alive() || ts.mode.get() != crate::threads::ThreadMode::InVm {
            continue;
        }
        // a thread that never reported has seen nothing yet
        let seen = recs.iter().find(|r| r.ts == t).map_or(0, |r| r.seen.load(Acquire));
        m = m.min(seen);
    }
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

/// Process matured batches of the calling thread (and orphans).
fn poll() {
    let l = local();
    if l.closed.is_empty() && PENDING_TOTAL.load(Relaxed) == 0 {
        return;
    }
    let m = if multi_threaded() { min_seen() } else { u64::MAX };
    let mut ready = Vec::new();
    l.closed.retain_mut(|(goal, items)| {
        if *goal <= m {
            ready.push(std::mem::take(items));
            false
        } else {
            true
        }
    });
    let orphans: Vec<Vec<ItemSend>> = {
        let mut o = ORPHANS.lock().unwrap();
        let mut out = Vec::new();
        o.retain_mut(|(goal, items)| {
            if *goal <= m {
                out.push(std::mem::take(items));
                false
            } else {
                true
            }
        });
        out
    };
    for b in ready {
        unsafe { release(b) };
    }
    for b in orphans {
        unsafe { release(b.into_iter().map(|i| i.0).collect()) };
    }
}

/// Eval-breaker handler (`EVAL_QSBR`): this thread is at a quiescent point.
pub fn on_eval_breaker(ts: &ThreadState) {
    ts.c.eval_breaker.fetch_and(!EVAL_QSBR, AcqRel);
    let r = my_rec();
    r.seen.store(GLOBAL.load(SeqCst), Release);
    let l = local();
    if !l.open.is_empty() {
        l.polls += 1;
        if l.polls >= POLLS || !multi_threaded() {
            close_batch();
        }
    }
    poll();
    let l = local();
    if !l.closed.is_empty() || !l.open.is_empty() {
        // not yet matured: check again at the next poll
        ts.c.eval_breaker.fetch_or(EVAL_QSBR, Release);
    }
}

/// The calling thread is exiting: hand pending batches over, unregister and let the
/// remaining threads re-check (they may be alone now).
pub fn thread_exit() {
    let l = local();
    if !l.rec.is_null() || !l.open.is_empty() {
        close_batch();
        let closed = std::mem::take(&mut l.closed);
        if !closed.is_empty() {
            let mut o = ORPHANS.lock().unwrap();
            for (g, items) in closed {
                o.push((g, items.into_iter().map(ItemSend).collect()));
            }
        }
    }
    if !l.rec.is_null() {
        let r = l.rec;
        RECS.lock().unwrap().retain(|&x| !std::ptr::eq(x, r));
        l.rec = std::ptr::null();
        unsafe { drop(Box::from_raw(r as *mut Rec)) };
    }
    let me = me() as usize;
    for t in crate::threads::ALL_THREADS.lock().unwrap().iter() {
        if *t != me {
            let ts = unsafe { &*(*t as *const ThreadState) };
            ts.c.eval_breaker.fetch_or(EVAL_QSBR, Release);
        }
    }
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
    let r = my_rec();
    r.seen.store(GLOBAL.load(SeqCst), Release);
    if !multi_threaded() {
        close_batch();
    }
    poll();
}

/// Number of retired items not yet released (diagnostics/tests).
pub fn pending() -> usize {
    PENDING_TOTAL.load(Relaxed) + local().open.len()
}
