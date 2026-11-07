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

pub(crate) const CAP: usize = 2048;
pub(crate) static LIST: [AtomicUsize; CAP] = [const { AtomicUsize::new(0) }; CAP];
pub(crate) static LIST_LEN: AtomicUsize = AtomicUsize::new(0);
/// Sequence lock over LIST/LIST_LEN: odd while being rewritten.
pub(crate) static LIST_VER: AtomicU64 = AtomicU64::new(0);
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

/// (scale2) Registered thread states for which `pred` holds (a consistent snapshot of
/// the list, without the `ALL_THREADS` lock unless there are more than CAP threads).
/// The memory of a thread state is never freed, so the result stays dereferenceable;
/// whether it is still the same thread must be checked by the caller.
pub fn find_threads(mut pred: impl FnMut(&ThreadState) -> bool) -> smallvec_like::Found {
    let mut out = smallvec_like::Found::default();
    for_each_thread(|ts| {
        if pred(ts) {
            out.push(ts as *const ThreadState as usize);
        }
    });
    out
}

pub mod smallvec_like {
    /// Up to 4 matches without allocating.
    #[derive(Default)]
    pub struct Found {
        n: usize,
        inline: [usize; 4],
        more: Vec<usize>,
    }
    impl Found {
        pub fn push(&mut self, v: usize) {
            if self.n < 4 {
                self.inline[self.n] = v;
            } else {
                self.more.push(v);
            }
            self.n += 1;
        }
        pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
            self.inline[..self.n.min(4)].iter().copied().chain(self.more.iter().copied())
        }
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
    /// Remaining self re-arms of the eval breaker (see REARM).
    rearm: u32,
    retires: u32,
    /// `retires` at the last eval-breaker visit (changed: still retiring, it comes back by itself).
    last_open: usize,
    /// The thread state this OS thread last retired items under.
    ts: *const ThreadState,
    /// Listed in LOCALS (for collections to take the pending items).
    registered: bool,
    /// The thread has run `thread_exit`: later items go straight to the orphans.
    dead: bool,
}

#[thread_local]
static LOCAL: UnsafeCell<Local> = UnsafeCell::new(Local {
    open: Vec::new(),
    closed: std::collections::VecDeque::new(),
    polls: 0,
    known_min: 0,
    scan_in: 0,
    rearm: 0,
    retires: 0,
    last_open: 0,
    ts: std::ptr::null(),
    registered: false,
    dead: false,
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
const POLLS: u32 = 256;
/// Eval-breaker visits a thread re-arms itself for while it has pending items; after
/// that it comes back only when it retires more, another thread closes a batch, or a
/// collection stops the world (which releases everything retired before it).
const REARM: u32 = 300;
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

/// (tail4) Release `p` (an owned reference), just unpublished from a structure other
/// threads read lock-free (with a store at least Release-ordered, before this call):
/// at once when every other thread is quiescent (see `others_quiescent`), else
/// through QSBR. Must not be called with a leaf lock held (may run a deallocator).
pub unsafe fn release_unpublished(p: *mut PyObject) {
    unsafe {
        if !multi_threaded() || (*p).is_immortal() {
            crate::object::decref(p);
            return;
        }
        std::sync::atomic::fence(SeqCst);
        if others_quiescent() {
            crate::object::decref(p);
            return;
        }
        decref_deferred(p);
    }
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

// ---------------------------------------------------------------- attribute storage (scale2)
//
// Values stored in an object's attribute storage (inline values of `shape.rs`,
// `__slots__` members) are read lock-free. A value replaced there only needs the
// deferred release if some thread other than the object's owner may be in the middle of
// such a read. Such readers announce themselves once per object: before their first
// lock-free read of the object's attribute storage they set the sticky `XREAD` bit in its
// `ob_flags` (a locked RMW followed by a SeqCst fence). A writer swaps the value out with
// a SeqCst RMW and then loads the flags (SeqCst): if it is the owner and sees the bit
// clear, every non-owner reader's bit-setting comes after its swap in the single total
// order, so such a reader's slot load sees the new value and the old one can be released
// at once. Readers that are the owner thread need nothing (same thread as the writer);
// writers that are not the owner always defer. Objects whose owner gave up ownership
// (`ob_tid == 0`) have no owner: every access counts as a non-owner one.

/// `ob_flags` bit: a thread other than the owner has read the attribute storage
/// lock-free (bits 0-2 belong to the C API, 5-7 to `sharded.rs`, 8-15 to `shape.rs`).
pub const XREAD: u16 = 1 << 4;

/// Announce a lock-free read of `o`'s attribute storage by the calling thread (call
/// before loading the slot).
#[inline(always)]
pub fn note_attr_read(o: &PyObject) {
    if o.ob_tid.load(Relaxed) != crate::object::thread_id() && o.ob_flags.load(Relaxed) & XREAD == 0 {
        mark_xread(o);
    }
}

#[cold]
#[inline(never)]
fn mark_xread(o: &PyObject) {
    o.ob_flags.fetch_or(XREAD, SeqCst);
    std::sync::atomic::fence(SeqCst);
}

/// Release `old` (an owned reference or null), just swapped out of the attribute
/// storage of `o` by a SeqCst RMW: at once when no other thread can be reading it
/// lock-free (see above), else through QSBR. Must not be called with a leaf lock held
/// (the release may run a deallocator).
#[inline(always)]
pub unsafe fn release_attr(o: *mut PyObject, old: *mut PyObject) {
    unsafe {
        if old.is_null() {
            return;
        }
        if !multi_threaded() || ((*o).ob_tid.load(Relaxed) == crate::object::thread_id() && (*o).ob_flags.load(SeqCst) & XREAD == 0) || others_quiescent() {
            crate::object::decref(old);
            return;
        }
        decref_deferred(old);
    }
}

/// (scale2) Is every other thread quiescent right now (not executing Python: blocked in
/// a wait, in native code, parked)? Then none of them is in the middle of a lock-free
/// read, and a pointer the caller unpublished with a SeqCst RMW (before this call) can
/// be released at once: a thread that enters the VM afterwards stores its mode
/// (SeqCst) before reading anything, so it sees the new value (Dekker with
/// `gc::enter_vm`). The common case of a writer whose helper threads are idle (an event
/// loop next to blocked executor threads) then finalizes values promptly.
///
/// (scale3) Cheap when the answer is no (the common case on a hot path while other
/// threads run): the scan stops at the first running thread, starting with the one
/// found running last time (thread-local hint), and needs no snapshot of the list; a
/// yes is only given for a version-consistent scan of the whole list.
pub fn others_quiescent() -> bool {
    #[thread_local]
    static mut HINT: usize = 0;
    let me = me() as usize;
    let running = |t: usize| -> bool {
        let ts = unsafe { &*(t as *const ThreadState) };
        t != me && t != 0 && ts.alive() && ts.mode.0.load(SeqCst) == crate::threads::ThreadMode::InVm as u8
    };
    loop {
        let v0 = LIST_VER.load(SeqCst);
        if v0 & 1 == 1 {
            std::hint::spin_loop();
            continue;
        }
        if OVERFLOW.load(SeqCst) {
            let mut quiet = true;
            for_each_thread(|ts| {
                if quiet && running(ts as *const ThreadState as usize) {
                    quiet = false;
                }
            });
            return quiet;
        }
        // (thread-state memory is never freed: reading a stale entry is harmless)
        let n = LIST_LEN.load(Relaxed).min(CAP);
        let h = unsafe { HINT };
        if h < n && running(LIST[h].load(Relaxed)) {
            return false;
        }
        for i in 0..n {
            if i != h && running(LIST[i].load(Relaxed)) {
                unsafe { HINT = i };
                return false;
            }
        }
        if LIST_VER.load(SeqCst) == v0 {
            return true;
        }
    }
}

/// As `swap_field`, for callers that hold no lock (the old value may be released at
/// once, which can run a deallocator): released at once when every other thread is
/// quiescent, else through QSBR.
pub unsafe fn swap_field_unlocked(slot: *mut *mut PyObject, new: *mut PyObject) {
    unsafe {
        let old = std::sync::atomic::AtomicPtr::from_ptr(slot).swap(new, SeqCst);
        if old.is_null() {
            return;
        }
        if !multi_threaded() || others_quiescent() {
            crate::object::decref(old);
        } else {
            decref_deferred(old);
        }
    }
}

/// Store `new` (an owned reference or null) into attribute storage `slot` of `o`
/// (readers use `note_attr_read`) and release the previous value (`release_attr`).
#[inline(always)]
pub unsafe fn swap_attr(o: *mut PyObject, slot: *mut *mut PyObject, new: *mut PyObject) {
    unsafe {
        let old = std::sync::atomic::AtomicPtr::from_ptr(slot).swap(new, SeqCst);
        release_attr(o, old);
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
        if l.dead {
            return retire_after_exit(it);
        }
        retire_first();
    }
    l.open.push(it);
    // Nothing is released here: the caller may hold locks, and releasing runs
    // deallocators (Python code). Releases happen at the next eval-breaker poll.
    if l.open.len() >= BATCH {
        retire_full();
    } else {
        l.retires = l.retires.wrapping_add(1);
        if l.retires & 63 == 0 {
            come_back_soon(l);
        }
    }
}

/// Have this thread visit `on_eval_breaker` at its next poll(s).
#[cold]
fn come_back_soon(l: &mut Local) {
    l.rearm = l.rearm.max(POLLS + 8);
    let t = me();
    if !t.is_null() {
        unsafe { (*t).c.eval_breaker.fetch_or(EVAL_QSBR, Release) };
    }
}

/// Collections take the pending items of every thread (`take_all_for_gc`).
static LOCALS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

#[cold]
fn retire_first() {
    let l = local();
    if !l.registered {
        LOCALS.lock().unwrap_or_else(|e| e.into_inner()).push(LOCAL.get() as usize);
        l.registered = true;
    }
    // come back at the next polls to close the batch
    l.rearm = REARM;
    let t = me();
    l.ts = t;
    if !t.is_null() {
        unsafe { (*t).c.eval_breaker.fetch_or(EVAL_QSBR, Release) };
    }
    if l.open.capacity() == 0 {
        l.open.reserve(BATCH);
    }
}

/// An item retired by a thread after its `thread_exit` (teardown): an orphan of its own.
#[cold]
fn retire_after_exit(it: Item) {
    let goal = GLOBAL.fetch_add(1, SeqCst) + 1;
    PENDING_TOTAL.fetch_add(1, Relaxed);
    let mut o = ORPHANS.lock().unwrap_or_else(|e| e.into_inner());
    o.push((goal, vec![ItemSend(it)]));
    ORPHAN_MIN.store(o.iter().map(|b| b.0).min().unwrap_or(u64::MAX), Relaxed);
}

#[cold]
fn retire_full() {
    close_batch();
    let l = local();
    come_back_soon(l);
    l.scan_in = 0;
}

/// The world is stopped for a collection: every thread is at a quiescent point, so
/// everything retired so far, by any thread, can be released. The items are taken
/// here and released by the caller once the world runs again (`release_taken`).
/// This bounds how long items wait when their thread stopped polling for them.
pub fn take_all_for_gc() -> Vec<ItemSendBatch> {
    let mut out = Vec::new();
    // Each thread's items are released by the thread itself (its own objects: cheap
    // owner-side frees) at its next eval-breaker visit: everything retired so far gets
    // a goal that every thread has passed, since all of them are stopped right now.
    let goal = GLOBAL.fetch_add(1, SeqCst) + 1;
    let locals = LOCALS.lock().unwrap_or_else(|e| e.into_inner());
    for &lp in locals.iter() {
        let l = unsafe { &mut *(lp as *mut Local) };
        if !l.open.is_empty() {
            let items = std::mem::take(&mut l.open);
            PENDING_TOTAL.fetch_add(items.len(), Relaxed);
            l.closed.push_back((goal, items));
        }
        l.polls = 0;
        if !l.closed.is_empty() {
            l.known_min = l.known_min.max(goal);
            l.rearm = l.rearm.max(8);
            if !l.ts.is_null() {
                unsafe { (*l.ts).c.eval_breaker.fetch_or(EVAL_QSBR, Release) };
            }
        }
    }
    drop(locals);
    let mut o = ORPHANS.lock().unwrap_or_else(|e| e.into_inner());
    for (_, items) in o.drain(..) {
        out.push(ItemSendBatch(items.into_iter().map(|i| i.0).collect(), true));
    }
    ORPHAN_MIN.store(u64::MAX, Relaxed);
    out
}

/// Items taken by `take_all_for_gc` (`.1`: counted in PENDING_TOTAL).
pub struct ItemSendBatch(Vec<Item>, bool);
impl ItemSendBatch {
    /// (scale3, diagnostics) Number of items.
    pub fn len(&self) -> usize {
        self.0.len()
    }
}
unsafe impl Send for ItemSendBatch {}

/// Release what `take_all_for_gc` took (world running).
pub fn release_taken(v: Vec<ItemSendBatch>) {
    for b in v {
        if b.1 {
            unsafe { release(b.0) };
        } else {
            for it in b.0 {
                match it {
                    Item::Decref(p) => unsafe { crate::object::decref(p) },
                    Item::Free(p, f) => unsafe { f(p) },
                }
            }
        }
    }
}

fn close_batch() {
    let l = local();
    l.polls = 0;
    l.rearm = REARM;
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

/// (scale2) The calling thread is about to wait (lock acquire, sleep, join) and may not
/// poll for a long time: close its open batch and release what has matured (with the
/// other threads blocked too, that is everything), so that objects it retired are not
/// kept alive for the whole wait (a weakref callback another thread waits for, a
/// reference cycle through a frame...). Called where running deallocators is safe
/// (the same places that run signal handlers). `spin`: a long wait (thread join,
/// interpreter teardown); otherwise only matured batches are released.
pub fn before_wait(spin: bool) {
    let l = local();
    if l.open.is_empty() && l.closed.is_empty() {
        return;
    }
    if !spin {
        // short waits (lock and queue waits of busy producer/consumer threads): only
        // release what has matured; closing a batch here would send a handshake to
        // every thread at each wait
        if !l.closed.is_empty() {
            poll(true);
        }
        return;
    }
    if !l.open.is_empty() {
        close_batch();
    }
    poll(true);
    // Threads running Python report within microseconds (closing the batch asked them
    // to); blocked ones already count as quiescent. Give them a moment rather than
    // keeping the items for the whole wait.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2);
    while !local().closed.is_empty() && std::time::Instant::now() < deadline {
        std::thread::yield_now();
        poll(true);
    }
}

/// Eval-breaker handler (`EVAL_QSBR`): this thread is at a quiescent point.
pub fn on_eval_breaker(ts: &ThreadState) {
    ts.c.eval_breaker.fetch_and(!EVAL_QSBR, AcqRel);
    report(ts);
    let l = local();
    // (tail4) everything retired so far was unpublished before now: if no other
    // thread is executing Python at this moment, none is in the middle of a
    // lock-free read of it, so it can go at once (finalizers then run close to
    // where CPython runs them, instead of after a batch interval)
    if (!l.open.is_empty() || !l.closed.is_empty()) && multi_threaded() && others_quiescent() {
        let open = std::mem::take(&mut l.open);
        let closed: Vec<(u64, Vec<Item>)> = l.closed.drain(..).collect();
        l.polls = 0;
        for it in open {
            match it {
                Item::Decref(p) => unsafe { crate::object::decref(p) },
                Item::Free(p, f) => unsafe { f(p) },
            }
        }
        for (_, items) in closed {
            unsafe { release(items) };
        }
    }
    let l = local();
    if !l.open.is_empty() {
        l.polls += 1;
        if l.polls >= POLLS || !multi_threaded() {
            close_batch();
        }
    }
    poll(false);
    let l = local();
    // A thread that is retiring quickly comes back by itself (every 64 retires and
    // whenever a batch fills up): re-arming at every poll would only cost time.
    let progressing = l.retires as usize != l.last_open;
    l.last_open = l.retires as usize;
    if (!l.closed.is_empty() || !l.open.is_empty() || ORPHAN_MIN.load(Relaxed) != u64::MAX) && l.rearm > 0 && !progressing {
        // not yet matured: check again at the next poll (a bounded number of times)
        l.rearm -= 1;
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
    if l.registered {
        // (scale1) under the LOCALS lock: a collection may be taking items right now
        let me = LOCAL.get() as usize;
        LOCALS.lock().unwrap_or_else(|e| e.into_inner()).retain(|&x| x != me);
        l.registered = false;
    }
    l.dead = true;
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

/// (stability3) Retired items handed out by `take_for_gc`.
pub struct Taken(Vec<Item>);

/// Anything retired (by any thread, or orphaned) and not yet released?
pub fn has_pending_for_gc() -> bool {
    if PENDING_TOTAL.load(Relaxed) > 0 {
        return true;
    }
    let locals = LOCALS.lock().unwrap_or_else(|e| e.into_inner());
    locals.iter().any(|&lp| !unsafe { &*(lp as *const Local) }.open.is_empty())
}

/// (stability3) Take every retired item of every thread and every orphaned batch.
/// Only call with the world stopped: no thread is then inside a lock-free read, so
/// all of them are due. Unlike `take_all_for_gc` (whose items go back to their
/// threads, or are released after the collection), the caller releases these itself
/// with `release_taken_now` once the world runs again and *before* analysing the
/// heap: a reference that a thread dropped while other threads existed (e.g. `del x`
/// of a global) must not keep a cycle alive, and a thread blocked for a long time
/// (one left over by an earlier test) would otherwise hold its items until it wakes.
pub fn take_for_gc() -> Taken {
    let mut out: Vec<Item> = Vec::new();
    let locals = LOCALS.lock().unwrap_or_else(|e| e.into_inner());
    for &lp in locals.iter() {
        let l = unsafe { &mut *(lp as *mut Local) };
        let open = std::mem::take(&mut l.open);
        PENDING_TOTAL.fetch_add(open.len(), Relaxed);
        out.extend(open);
        for (_, items) in l.closed.drain(..) {
            out.extend(items);
        }
        l.polls = 0;
    }
    drop(locals);
    let mut o = ORPHANS.lock().unwrap_or_else(|e| e.into_inner());
    for (_, items) in o.drain(..) {
        out.extend(items.into_iter().map(|i| i.0));
    }
    ORPHAN_MIN.store(u64::MAX, Relaxed);
    Taken(out)
}

/// Release what `take_for_gc` returned (world running).
pub fn release_taken_now(t: Taken) {
    if !t.0.is_empty() {
        unsafe { release(t.0) };
    }
}

/// Number of retired items not yet released (diagnostics/tests).
pub fn pending() -> usize {
    PENDING_TOTAL.load(Relaxed) + local().open.len()
}
