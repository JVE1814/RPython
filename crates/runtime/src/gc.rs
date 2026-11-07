//! Cycle collector.
//!
//! Reference counting (forced by the C ABI) frees acyclic garbage; this module finds
//! unreachable reference cycles. Design (DECISIONS D5):
//! * GC-capable objects live on the allocator's GC pages (or, when large, in a
//!   registry in alloc.rs). Tracking an object only sets its `GC_BIT_TRACKED` bit;
//!   a collection enumerates the candidates by walking those pages (perf2: no list
//!   links, so no per-object 16-byte header and no global list lock).
//! * Generations: objects that survived a collection carry `GC_BIT_OLD`; young
//!   collections only examine the others (references from old objects count as
//!   external, which is conservative).
//! * A collection is stop-the-world. Thread coordination follows HotSpot safepoints:
//!   the collector raises the safepoint bit in every thread's eval breaker; threads
//!   executing Python park in `safepoint_poll`; threads in native/blocked mode count as
//!   safe. If not every thread reaches a safe state quickly, the collection is skipped.
//! * Trial deletion: gc_refs = refcount; subtract references from tracked objects
//!   (tp_traverse); objects with gc_refs > 0 are externally reachable; everything they
//!   reach is reachable; the rest is cyclic garbage.
//! * Garbage with finalizers (tp_finalize / __del__) is finalized first, then the
//!   analysis is repeated on it so resurrected objects survive (PEP 442).
//! * Cycles are broken with tp_clear while the garbage is kept alive by a temporary
//!   reference, then released.

use crate::alloc::{GC_BIT_FINALIZED, GC_BIT_OLD, GC_BIT_TRACKED};
use crate::ffi::*;
use crate::object::*;
use crate::threads::ThreadState;
use std::ffi::{c_int, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering::*};

static TRACKED: AtomicIsize = AtomicIsize::new(0);
static ALLOCS_SINCE: AtomicUsize = AtomicUsize::new(0);
pub static ENABLED: AtomicBool = AtomicBool::new(true);
pub static THRESHOLD: AtomicUsize = AtomicUsize::new(2000);
static COLLECTING: AtomicBool = AtomicBool::new(false);
/// (scale1) Thread id of the thread running `collect_gen` (0: none).
static COLLECTOR: AtomicUsize = AtomicUsize::new(0);
pub static COLLECTIONS: AtomicUsize = AtomicUsize::new(0);
pub static COLLECTED: AtomicUsize = AtomicUsize::new(0);
pub static UNCOLLECTABLE: AtomicUsize = AtomicUsize::new(0);
/// Size of the live set after the last collection (for adaptive triggering).
static LAST_LIVE: AtomicUsize = AtomicUsize::new(0);
static YOUNG_COLLECTIONS: AtomicUsize = AtomicUsize::new(0);
/// Tracked objects right after the last full collection.
static LAST_FULL_LIVE: AtomicUsize = AtomicUsize::new(0);

#[inline]
pub unsafe fn track(op: *mut PyObject) {
    unsafe {
        let o = &*op;
        let bits = o.ob_gc_bits.load(Relaxed);
        if bits & crate::alloc::GC_BIT_PREHEADER == 0 || bits & GC_BIT_TRACKED != 0 {
            return;
        }
        // (re)tracked objects start young
        o.ob_gc_bits.store((bits | GC_BIT_TRACKED) & !GC_BIT_OLD, Relaxed);
        note_tracked(1);
    }
}

// perf-runtime: the tracked/allocation counters are kept per thread and published
// in batches (two global atomic RMWs per tracked object were a contention point for
// multi-threaded allocation and ~10% of a small-object allocation).
#[thread_local]
static mut LOCAL_DELTA: isize = 0;
const DELTA_BATCH: isize = 32;

#[inline(always)]
fn note_tracked(d: isize) {
    unsafe {
        let v = LOCAL_DELTA + d;
        if (-DELTA_BATCH..DELTA_BATCH).contains(&v) {
            LOCAL_DELTA = v;
        } else {
            LOCAL_DELTA = 0;
            publish_delta(v);
        }
    }
}

#[cold]
fn publish_delta(v: isize) {
    TRACKED.fetch_add(v, Relaxed);
    if v > 0 {
        let n = ALLOCS_SINCE.fetch_add(v as usize, Relaxed) + v as usize;
        if n >= trigger_threshold() && ENABLED.load(Relaxed) && !COLLECTING.load(Relaxed) {
            request_collection();
        }
    } else if v < 0 {
        // allocations minus deallocations (like CPython gen0 counts): short-lived
        // objects do not trigger collections
        let _ = ALLOCS_SINCE.fetch_update(Relaxed, Relaxed, |x| Some(x.saturating_sub((-v) as usize)));
    }
}

/// Publish the calling thread's pending counter delta (exact counts for gc module
/// queries; thread exit).
pub fn flush_thread_counts() {
    unsafe {
        let v = LOCAL_DELTA;
        if v != 0 {
            LOCAL_DELTA = 0;
            publish_delta(v);
        }
    }
}

/// (perf4) False until interpreter initialisation (importlib, site) is complete:
/// no automatic collections before (start-up builds long-lived module state, not
/// cyclic garbage, and every collection walks the whole heap).
static STARTED: AtomicBool = AtomicBool::new(false);

/// End of interpreter start-up: automatic collections are counted from here.
pub fn startup_complete() {
    if !STARTED.swap(true, Relaxed) {
        ALLOCS_SINCE.store(0, Relaxed);
        LAST_LIVE.store(TRACKED.load(Relaxed).max(0) as usize, Relaxed);
    }
}

fn trigger_threshold() -> usize {
    // young collection after THRESHOLD net new tracked objects; 0 disables
    // automatic collection (gc.set_threshold(0)). A collection walks every GC page
    // (perf2: no young list), so the interval also grows with the live set (a quarter
    // of it), which keeps the walking cost per allocation constant.
    if !STARTED.load(Relaxed) {
        return usize::MAX;
    }
    match THRESHOLD.load(Relaxed) {
        0 => usize::MAX,
        t => t.max(LAST_LIVE.load(Relaxed) / 4),
    }
}

#[inline]
pub unsafe fn untrack(op: *mut PyObject) {
    unsafe {
        let o = &*op;
        let bits = o.ob_gc_bits.load(Relaxed);
        if bits & GC_BIT_TRACKED == 0 {
            return;
        }
        o.ob_gc_bits.store(bits & !(GC_BIT_TRACKED | GC_BIT_OLD), Relaxed);
        note_tracked(-1);
    }
}

/// Count one object as untracked whose tracked bit the caller clears itself (the
/// allocator's free fast path, which zeroes all GC bits).
#[inline(always)]
pub fn note_untracked() {
    note_tracked(-1);
}

pub fn is_tracked(op: &PyObject) -> bool {
    op.ob_gc_bits.load(Relaxed) & GC_BIT_TRACKED != 0
}
pub unsafe fn is_finalized(op: *mut PyObject) -> bool {
    unsafe { (*op).ob_gc_bits.load(Relaxed) & GC_BIT_FINALIZED != 0 }
}
pub unsafe fn set_finalized(op: *mut PyObject) {
    unsafe {
        (*op).ob_gc_bits.fetch_or(GC_BIT_FINALIZED, Relaxed);
    }
}
pub fn tracked_count() -> isize {
    flush_thread_counts();
    TRACKED.load(Relaxed)
}

/// Run tp_finalize for an object being deallocated. Returns true if it was resurrected.
pub unsafe fn call_finalizer_from_dealloc(op: *mut PyObject) -> bool {
    unsafe {
        set_finalized(op);
        let o = &*op;
        o.ob_ref_local.store(1, Relaxed);
        o.ob_tid.store(crate::object::thread_id(), Relaxed);
        if let Some(f) = (*o.type_ptr()).tp_finalize {
            f(op);
        }
        let local = o.ob_ref_local.load(Relaxed) - 1;
        o.ob_ref_local.store(local, Relaxed);
        local != 0 || (o.ob_ref_shared.load(Relaxed) >> 2) != 0
    }
}

// ---------------------------------------------------------------- safepoints
//
// Handshake between a thread stopping the world and the other threads (a
// store/load pair on each side, all sequentially consistent):
// * the stopping thread publishes `RELEASE = false`, then reads the mode of every
//   other thread and proceeds once none of them is `InVm`;
// * a thread switching to `InVm` (leaving native/blocked mode, starting, or
//   leaving a safepoint) publishes its mode first and reads `RELEASE` second; if a
//   stop is in progress it falls back to a safe mode and waits.
// One of the two always observes the other's store, so no thread can execute VM
// code while the stopping thread believes the world is stopped. Threads waiting
// at a safepoint are in `Blocked` mode. (The previous scheme compared a count of
// parked threads with a separately sampled count of running ones, which could
// pass while a thread was still running and let it unlink GC list entries during
// a collection.)

/// Threads currently waiting for the world to restart (diagnostics).
static PARKED: AtomicUsize = AtomicUsize::new(0);
static RELEASE: AtomicBool = AtomicBool::new(true);
/// Address of the mode cell of the thread that stopped the world: it never
/// waits for its own stop (e.g. when it leaves a blocking call meanwhile).
static STW_OWNER: AtomicUsize = AtomicUsize::new(0);

/// Ask the current thread to collect at its next eval-breaker check.
fn request_collection() {
    let ts = crate::threads::current();
    ts.c.eval_breaker.fetch_or(crate::threads::EVAL_GC, Release);
}

#[cold]
fn wait_for_release() {
    let t0 = if crate::scalestats::on() { Some(std::time::Instant::now()) } else { None };
    let _stats = StatGuard(t0, &crate::scalestats::PARKED_NS);
    PARKED.fetch_add(1, AcqRel);
    let mut i = 0u32;
    while !RELEASE.load(SeqCst) {
        if i < 64 {
            std::thread::yield_now();
        } else {
            std::thread::sleep(std::time::Duration::from_micros(if i < 512 { 20 } else { 200 }));
        }
        i += 1;
    }
    PARKED.fetch_sub(1, AcqRel);
}

/// Adds the time since `.0` (when counting is on) to the counter `.1` when dropped.
struct StatGuard(Option<std::time::Instant>, &'static std::sync::atomic::AtomicU64);
impl Drop for StatGuard {
    fn drop(&mut self) {
        if let Some(t) = self.0 {
            crate::scalestats::add_ns(self.1, t);
        }
    }
}

/// Switch a thread's mode to `InVm` following the handshake above. `safe` is the
/// mode the thread waits in while the world is stopped.
pub fn enter_vm(cell: &crate::threads::ModeCell, safe: crate::threads::ThreadMode) {
    use crate::threads::ThreadMode;
    let safe = if safe == ThreadMode::InVm { ThreadMode::Blocked } else { safe };
    let me = cell as *const _ as usize;
    loop {
        cell.0.store(ThreadMode::InVm as u8, SeqCst);
        if STW_OWNER.load(SeqCst) == me {
            return;
        }
        if RELEASE.load(SeqCst) {
            if !PARK_ALL.load(SeqCst) || FINALIZER.load(SeqCst) == me {
                return;
            }
            cell.0.store(safe as u8, SeqCst);
            park_forever();
        }
        cell.0.store(safe as u8, SeqCst);
        wait_for_release();
    }
}

// ---------------------------------------------------------------- finalization
//
// Once the interpreter starts finalizing, every thread other than the finalizing
// one is stopped for good: it may stay blocked in native code, but as soon as it
// tries to run Python code again (leaving a blocking call, reaching a safepoint)
// it parks forever. The process exits around it.

static PARK_ALL: AtomicBool = AtomicBool::new(false);
static FINALIZER: AtomicUsize = AtomicUsize::new(0);

#[cold]
fn park_forever() -> ! {
    loop {
        std::thread::park();
    }
}

/// Have all other threads been stopped for good (interpreter finalization)?
#[inline]
pub fn others_parked() -> bool {
    PARK_ALL.load(Relaxed)
}

/// Is the calling thread the one finalizing the interpreter?
pub fn is_finalizer_thread() -> bool {
    let p = crate::threads::current_ptr();
    !p.is_null() && FINALIZER.load(Relaxed) == unsafe { &(*p).mode as *const _ as usize }
}

/// Stop every other thread permanently (each parks at its next transition into
/// the VM). Called by the finalizing thread; returns once no other thread runs
/// Python code.
pub fn park_other_threads() {
    let me = crate::threads::current_mut();
    // a collection running on another thread finishes first
    crate::threads::blocking(|| {
        while COLLECTING.swap(true, AcqRel) {
            std::thread::sleep(std::time::Duration::from_micros(100));
        }
    });
    // A thread that never reaches a safepoint (stuck in native code that claims
    // to be in the VM) must not hang the exit: after ~2 s carry on; such a thread
    // parks at its next safepoint poll.
    let mut stopped = false;
    for _ in 0..20 {
        if stop_the_world(me) {
            stopped = true;
            break;
        }
        std::thread::yield_now();
    }
    FINALIZER.store(unsafe { &(*me).mode as *const _ as usize }, SeqCst);
    PARK_ALL.store(true, SeqCst);
    if stopped {
        start_the_world();
    } else {
        for &t in crate::threads::ALL_THREADS.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            if t != me as usize {
                unsafe { &*(t as *const ThreadState) }.c.eval_breaker.fetch_or(crate::threads::EVAL_SAFEPOINT, SeqCst);
            }
        }
    }
    COLLECTING.store(false, Release);
}

/// Is a stop-the-world operation in progress?
#[inline]
pub fn world_stopped() -> bool {
    !RELEASE.load(SeqCst)
}

/// Called by threads at safepoints (backedges, function entry via the eval breaker).
pub fn safepoint_poll(ts: &ThreadState) {
    ts.c.eval_breaker.fetch_and(!crate::threads::EVAL_SAFEPOINT, AcqRel);
    if (RELEASE.load(SeqCst) && !PARK_ALL.load(SeqCst)) || STW_OWNER.load(SeqCst) == &ts.mode as *const _ as usize {
        return;
    }
    if ts.mode.get() != crate::threads::ThreadMode::InVm {
        return; // already counts as stopped
    }
    ts.mode.0.store(crate::threads::ThreadMode::Blocked as u8, SeqCst);
    wait_for_release();
    enter_vm(&ts.mode, crate::threads::ThreadMode::Blocked);
}

/// Bring all other threads to a safepoint. Returns false if they did not arrive in
/// time (the world is running again then).
fn stop_the_world(me: *mut ThreadState) -> bool {
    let t0 = if crate::scalestats::on() {
        crate::scalestats::STW_COUNT.fetch_add(1, Relaxed);
        Some(std::time::Instant::now())
    } else {
        None
    };
    let _stats = StatGuard(t0, &crate::scalestats::STW_WAIT_NS);
    STW_OWNER.store(unsafe { &(*me).mode as *const _ as usize }, SeqCst);
    RELEASE.store(false, SeqCst);
    // read the thread list only after publishing the stop: a thread registered
    // later observes it when it enters the VM
    let threads: Vec<usize> = crate::threads::ALL_THREADS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let others: Vec<&ThreadState> = threads
        .iter()
        .filter(|&&t| t != me as usize)
        .map(|&t| unsafe { &*(t as *const ThreadState) })
        .collect();
    for t in &others {
        t.c.eval_breaker.fetch_or(crate::threads::EVAL_SAFEPOINT, SeqCst);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
    let mut i = 0u32;
    loop {
        let running = others.iter().any(|t| t.alive() && t.mode.0.load(SeqCst) == crate::threads::ThreadMode::InVm as u8);
        if !running {
            return true;
        }
        if std::time::Instant::now() > deadline {
            start_the_world();
            return false;
        }
        if i < 64 {
            std::thread::yield_now();
        } else {
            std::thread::sleep(std::time::Duration::from_micros(20));
        }
        i += 1;
    }
}

fn start_the_world() {
    STW_OWNER.store(0, SeqCst);
    RELEASE.store(true, SeqCst);
}

// ---------------------------------------------------------------- deferred release
//
// Storage that other threads may still be reading without a lock (e.g. the
// buffer of a bytearray that was just resized) is released only after every
// thread has passed a safepoint: lock-free reads never span a safepoint. The
// next collection's stop-the-world is such a point (DECISIONS D5). A finer
// grained mechanism (per-thread quiescent states) can replace this; the API
// stays `defer_decref`.

static DEFERRED: Mutex<Vec<usize>> = Mutex::new(Vec::new());
static MATURE: Mutex<Vec<usize>> = Mutex::new(Vec::new());
static DEFERRED_COUNT: AtomicUsize = AtomicUsize::new(0);
const DEFERRED_TRIGGER: usize = 4096;

/// Release a reference once no thread can still be reading the object through
/// a pointer loaded before this call. With a single thread it is released now.
pub unsafe fn defer_decref(op: *mut PyObject) {
    if op.is_null() {
        return;
    }
    if !crate::threads::multi_threaded() {
        unsafe { decref(op) };
        return;
    }
    DEFERRED.lock().unwrap_or_else(|e| e.into_inner()).push(op as usize);
    if DEFERRED_COUNT.fetch_add(1, Relaxed) + 1 >= DEFERRED_TRIGGER {
        DEFERRED_COUNT.store(0, Relaxed);
        // force a collection soon even if few GC objects are allocated
        ALLOCS_SINCE.fetch_add(THRESHOLD.load(Relaxed).max(1), Relaxed);
        request_collection();
    }
}

/// While the world is stopped: everything deferred so far may be released.
fn mature_deferred() {
    let v = std::mem::take(&mut *DEFERRED.lock().unwrap_or_else(|e| e.into_inner()));
    DEFERRED_COUNT.store(0, Relaxed);
    MATURE.lock().unwrap_or_else(|e| e.into_inner()).extend(v);
}

/// With the world running again: release what matured.
fn release_mature() {
    let v = std::mem::take(&mut *MATURE.lock().unwrap_or_else(|e| e.into_inner()));
    for op in v {
        unsafe { decref(op as *mut PyObject) };
    }
}

// ---------------------------------------------------------------- collection

fn total_refcnt(o: &PyObject) -> isize {
    let local = o.ob_ref_local.load(Relaxed);
    if local == IMMORTAL_REFCNT_LOCAL {
        return isize::MAX / 2;
    }
    local as isize + (o.ob_ref_shared.load(Relaxed) >> REF_SHARED_SHIFT)
        - if crate::sharded::is_sharded(o) || unsafe { crate::object::has_type_id(o.as_ptr()) } { crate::object::TYPE_BIAS } else { 0 }
}

/// Multiplicative hash for object addresses (SipHash dominated collection time).
#[derive(Default)]
pub struct PtrHasher(u64);

/// Hash set / map of object addresses with the cheap address hasher.
pub type PtrSet = std::collections::HashSet<usize, std::hash::BuildHasherDefault<PtrHasher>>;
pub type PtrMap<V> = std::collections::HashMap<usize, V, std::hash::BuildHasherDefault<PtrHasher>>;
impl std::hash::Hasher for PtrHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ b as u64).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
    }
    fn write_usize(&mut self, v: usize) {
        self.0 = ((v as u64) >> 4).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

/// Trial-deletion state kept in the objects themselves while a collection runs
/// (perf3; the world is stopped): `ob_tid` temporarily holds
/// `gc_refs << 3 | reachable << 2 | GRAPH_TAG`. Real owner ids are aligned thread
/// pointers or 0, so the tag cannot collide and marks exactly the objects under
/// examination. Subtracting internal references (multiples of 8) never disturbs the
/// low bits. The owners are restored before returning. The mark stack is passed to
/// the C visitors through their `arg`.
struct Graph {
    stack: Vec<*mut PyObject>,
}

const GRAPH_TAG: usize = 3;
const REACHABLE: usize = 4;
const REFS_SHIFT: u32 = 3;

/// The collection state word of `op` if it is under examination.
#[inline(always)]
unsafe fn graph_word<'a>(op: *mut PyObject) -> Option<&'a std::sync::atomic::AtomicUsize> {
    unsafe {
        // (perf5) the tag alone identifies the examined objects: owner words of all
        // other objects (tracked or not) are aligned thread pointers or 0
        let t = &(*op).ob_tid;
        if t.load(Relaxed) & GRAPH_TAG == GRAPH_TAG { Some(t) } else { None }
    }
}

/// `op` non-null.
#[inline(always)]
unsafe fn decref_visit(op: *mut PyObject) {
    unsafe {
        if let Some(w) = graph_word(op) {
            w.store(w.load(Relaxed).wrapping_sub(1 << REFS_SHIFT), Relaxed);
        }
    }
}

/// `op` non-null.
#[inline(always)]
unsafe fn mark_visit(op: *mut PyObject, stack: &mut Vec<*mut PyObject>) {
    unsafe {
        let w = &(*op).ob_tid;
        let v = w.load(Relaxed);
        if v & (GRAPH_TAG | REACHABLE) == GRAPH_TAG {
            w.store(v | REACHABLE, Relaxed);
            stack.push(op);
        }
    }
}

unsafe extern "C" fn visit_decref(op: *mut PyObject, _arg: *mut c_void) -> c_int {
    if !op.is_null() {
        unsafe { decref_visit(op) };
    }
    0
}

unsafe extern "C" fn visit_mark(op: *mut PyObject, arg: *mut c_void) -> c_int {
    if !op.is_null() {
        unsafe { mark_visit(op, &mut (*(arg as *mut Graph)).stack) };
    }
    0
}

unsafe fn traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) {
    unsafe {
        let tp = (*op).type_ptr();
        if let Some(t) = (*tp).tp_traverse {
            t(op, visit, arg);
        }
    }
}

/// perf3: `traverse` with the visitor inlined for the containers that make up most
/// of a heap (exact lists and tuples: their slots are exactly their items); other
/// types go through their tp_traverse with the C visitor. The world is stopped.
#[inline(always)]
unsafe fn traverse_inline(op: *mut PyObject, visit: visitproc, arg: *mut c_void, mut f: impl FnMut(*mut PyObject)) {
    unsafe {
        let tp = (*op).type_ptr();
        if tp == crate::types::list::list_type() {
            let (items, n) = crate::types::list::raw_items(op);
            for i in 0..n {
                let it = *items.add(i);
                if !it.is_null() {
                    f(it);
                }
            }
        } else if tp == crate::types::tuple::tuple_type() {
            let n = crate::object::var_size(&*op) as usize;
            let base = crate::types::tuple::item_ptr(op);
            for i in 0..n {
                let it = *base.add(i);
                if !it.is_null() {
                    f(it);
                }
            }
        } else if let Some(t) = (*tp).tp_traverse {
            t(op, visit, arg);
        }
    }
}

/// (perf5) Roots for `find_unreachable`: the locals (and function, globals, builtins,
/// locals mapping) of every frame running on a registered thread. The world is
/// stopped, so every thread is at a safepoint with a consistent frame chain. Only
/// the locals part of the slots is read: compiled code does not keep `stack_top`
/// current, while local slots always hold owned references or NULL.
unsafe fn mark_from_frames(stack: &mut Vec<*mut PyObject>) {
    unsafe {
        // the current interpreter's sys.modules and sys (raw pointers: reference
        // counts must not change while owner words hold collection state)
        let it = crate::subinterp::current();
        for p in [it.modules.load(Acquire), it.sys.load(Acquire)] {
            if !p.is_null() {
                mark_visit(p, stack);
            }
        }
        let threads: Vec<usize> = crate::threads::ALL_THREADS.lock().unwrap_or_else(|e| e.into_inner()).clone();
        for t in threads {
            let ts = &*(t as *const ThreadState);
            if !ts.alive() {
                continue;
            }
            let mut f = ts.frame.get();
            let mut guard = 0usize;
            while !f.is_null() && guard < 1 << 20 {
                let fr = &*f;
                let n = fr.nlocalsplus.min(fr.slots.len());
                for &p in &fr.slots[..n] {
                    if !p.is_null() {
                        mark_visit(p, stack);
                    }
                }
                for p in [fr.func, fr.globals, fr.builtins, fr.locals] {
                    if !p.is_null() {
                        mark_visit(p, stack);
                    }
                }
                f = fr.prev;
                guard += 1;
            }
        }
    }
}

/// Compute the unreachable subset of `objs` (all tracked, no duplicates).
unsafe fn find_unreachable(objs: Vec<*mut PyObject>) -> Vec<*mut PyObject> {
    unsafe {
        // refcounts are taken before any tid is overwritten (the per-thread type
        // counting reads the owner word)
        let refs: Vec<isize> = objs.iter().map(|&o| total_refcnt(&*o)).collect();
        let mut saved_tid: Vec<usize> = Vec::with_capacity(objs.len());
        for (&o, &r) in objs.iter().zip(refs.iter()) {
            let t = &(*o).ob_tid;
            saved_tid.push(t.load(Relaxed));
            t.store(((r.clamp(0, 1 << 56) as usize) << REFS_SHIFT) | GRAPH_TAG, Relaxed);
        }
        drop(refs);
        let mut g = Graph { stack: Vec::with_capacity(objs.len()) };
        let gp = &mut g as *mut Graph as *mut c_void;
        // (perf5) first everything reachable from the threads' running frames: those
        // objects are alive whatever their counts say, and their references need not
        // be subtracted (each of their edges is then visited once instead of twice)
        mark_from_frames(&mut g.stack);
        while let Some(op) = g.stack.pop() {
            traverse_inline(op, Some(visit_mark), gp, |it| mark_visit(it, &mut (*(gp as *mut Graph)).stack));
        }
        // subtract the references the other examined objects hold among themselves
        for &op in &objs {
            if (*op).ob_tid.load(Relaxed) & REACHABLE == 0 {
                traverse_inline(op, Some(visit_decref), gp, |it| decref_visit(it));
            }
        }
        // anything still referenced from outside is reachable, and so is everything
        // it reaches
        for &op in &objs {
            let w = &(*op).ob_tid;
            let v = w.load(Relaxed);
            if (v as isize) >> REFS_SHIFT > 0 && v & REACHABLE == 0 {
                w.store(v | REACHABLE, Relaxed);
                g.stack.push(op);
            }
        }
        while let Some(op) = g.stack.pop() {
            traverse_inline(op, Some(visit_mark), gp, |it| mark_visit(it, &mut (*(gp as *mut Graph)).stack));
        }
        let mut unreachable = Vec::new();
        for (&o, &tid) in objs.iter().zip(saved_tid.iter()) {
            let w = &(*o).ob_tid;
            if w.load(Relaxed) & REACHABLE == 0 {
                unreachable.push(o);
            }
            w.store(tid, Relaxed);
        }
        unreachable
    }
}

fn snapshot() -> Vec<*mut PyObject> {
    snapshot_gen(false)
}

/// Tracked objects of the young generation (young_only) or of both generations, found
/// by walking the GC pages and the large-object registry. The world must be stopped
/// (or the process single-threaded).
fn snapshot_gen(young_only: bool) -> Vec<*mut PyObject> {
    let mut v = Vec::with_capacity(if young_only { 1024 } else { TRACKED.load(Relaxed).max(0) as usize });
    let mask = if young_only { GC_BIT_TRACKED | GC_BIT_OLD } else { GC_BIT_TRACKED };
    let mut take = |op: *mut PyObject| unsafe {
        if (*op).ob_gc_bits.load(Relaxed) & mask == GC_BIT_TRACKED {
            v.push(op);
        }
    };
    unsafe { crate::tlab::for_each_gc_block(&mut take) };
    crate::alloc::for_each_large_gc(&mut take);
    v
}

/// The examined objects that survive become old.
fn promote(objs: &[*mut PyObject]) {
    for &o in objs {
        unsafe {
            let b = &(*o).ob_gc_bits;
            b.store(b.load(Relaxed) | GC_BIT_OLD, Relaxed);
        }
    }
}

static DEAD_SHARDED: Mutex<Vec<PyRef>> = Mutex::new(Vec::new());
static QSBR_TAKEN: Mutex<Vec<crate::qsbr::ItemSendBatch>> = Mutex::new(Vec::new());

/// Run a full collection; returns the number of unreachable objects found.
pub fn collect() -> usize {
    collect_gen(false)
}

fn collect_gen(young_only: bool) -> usize {
    if COLLECTING.swap(true, AcqRel) {
        return 0;
    }
    COLLECTOR.store(crate::object::thread_id(), Release);
    let me = crate::threads::current_mut();
    let saved_exc = crate::errors::take_exc();
    let t0 = std::time::Instant::now();
    let n = unsafe { collect_phases(me, young_only) };
    if crate::scalestats::on() {
        crate::scalestats::GC_COUNT.fetch_add(1, Relaxed);
        crate::scalestats::add_ns(&crate::scalestats::GC_NS, t0);
    }
    release_mature();
    if let Some(e) = saved_exc {
        crate::errors::set_exc(e);
    }
    if n.is_some() {
        crate::gc_support::DURATION_NS.fetch_add((t0.elapsed().as_nanos() as usize).max(1), Relaxed);
        ALLOCS_SINCE.store(0, Relaxed);
        LAST_LIVE.store(TRACKED.load(Relaxed).max(0) as usize, Relaxed);
        crate::gc_support::LONG_LIVED_TOTAL.store(TRACKED.load(Relaxed).max(0) as usize + crate::gc_support::UNTRACKED_TUPLES.swap(0, Relaxed), Relaxed);
        if young_only {
            YOUNG_COLLECTIONS.fetch_add(1, Relaxed);
        } else {
            YOUNG_COLLECTIONS.store(0, Relaxed);
            LAST_FULL_LIVE.store(TRACKED.load(Relaxed).max(0) as usize, Relaxed);
        }
        COLLECTIONS.fetch_add(1, Relaxed);
    } else {
        // the world could not be stopped (a thread is busy in native code that
        // does not poll): retry after some more allocations
        ALLOCS_SINCE.store(THRESHOLD.load(Relaxed) / 2, Relaxed);
    }
    COLLECTOR.store(0, Release);
    COLLECTING.store(false, Release);
    // (scale1) sharded objects found dead and orphaned QSBR items, released only now:
    // their deallocation can run finalizers and weakref callbacks, which may use the
    // gc module (gc.get_objects waits for COLLECTING: a self-deadlock before)
    let saved_exc = crate::errors::take_exc();
    let dead = std::mem::take(&mut *DEAD_SHARDED.lock().unwrap_or_else(|e| e.into_inner()));
    drop(dead);
    let taken = std::mem::take(&mut *QSBR_TAKEN.lock().unwrap_or_else(|e| e.into_inner()));
    crate::qsbr::release_taken(taken);
    if let Some(e) = saved_exc {
        crate::errors::set_exc(e);
    }
    n.unwrap_or(0)
}

/// The collection proper. Only graph analysis and bookkeeping happen while the
/// world is stopped; finalizers, weakref callbacks and tp_clear (all of which can
/// run arbitrary Python code, take locks other threads hold, or start and join
/// threads) run with the world restarted. Garbage stays alive through the
/// references held here, and a second stopped phase re-checks it after
/// finalizers for resurrection. None if the world could not be stopped.
unsafe fn collect_phases(me: *mut ThreadState, young_only: bool) -> Option<usize> {
    unsafe {
        if !stop_the_world(me) {
            return None;
        }
        // every thread is at a safepoint: references deferred before now can go
        mature_deferred();
        crate::object::merge_type_counts();
        crate::sharded::merge_all();
        // (scale1) everything retired through QSBR before this stop can go
        QSBR_TAKEN.lock().unwrap_or_else(|e| e.into_inner()).extend(crate::qsbr::take_all_for_gc());
        // ---- phase 1 (stopped): find the unreachable objects
        let mut objs = snapshot_gen(young_only);
        crate::gc_support::untrack_atomic_tuples(&mut objs);
        crate::gc_support::CANDIDATES.fetch_add(objs.len(), Relaxed);
        promote(&objs);
        let mut unreachable = find_unreachable(objs);
        // cycles with legacy finalizers (tp_del) are left alone and reported
        let legacy = crate::gc_support::split_legacy_finalizers(&mut unreachable);
        if unreachable.is_empty() {
            DEAD_SHARDED.lock().unwrap_or_else(|e| e.into_inner()).extend(crate::sharded::dead_untracked());
            start_the_world();
            if legacy.is_empty() {
                return Some(0);
            }
            let u = crate::gc_support::report_uncollectable(legacy);
            UNCOLLECTABLE.fetch_add(u, Relaxed);
            return Some(u);
        }
        let n_legacy = if legacy.is_empty() { 0 } else { legacy.len() };
        let mut legacy = Some(legacy);
        // keep the garbage alive while we work on it
        let garbage: Vec<PyRef> = unreachable.iter().map(|&o| PyRef::from_borrowed(o)).collect();
        // unreachable types stop per-thread counting (their bias goes; `garbage` keeps
        // them alive meanwhile)
        for &o in &unreachable {
            if crate::object::has_type_id(o) {
                crate::object::release_type_id(o as *mut PyTypeObject, true);
            } else if crate::sharded::is_sharded(&*o) {
                crate::sharded::unshard(o);
            }
            // (scale1) garbage becomes ownerless: the references dropped while it is
            // cleared (by this thread, on behalf of the threads that created it) then
            // go to the shared field instead of the owners' merge queues
            crate::object::explicit_merge(o);
        }
        // (after the unsharding above: the registry must not keep entries for objects
        // that are about to be freed)
        DEAD_SHARDED.lock().unwrap_or_else(|e| e.into_inner()).extend(crate::sharded::dead_untracked());
        let needs_finalize = garbage.iter().any(|g| {
            let op = g.as_ptr();
            (*(*op).type_ptr()).tp_finalize.is_some() && !is_finalized(op)
        });
        // weakrefs with callbacks are cleared before finalizers run
        let early = crate::types::weakref::detach_weakrefs_for_gc(&unreachable, true);
        let to_clear: Vec<PyRef> = if !needs_finalize && early.is_empty() {
            // nothing can run before tp_clear, so nothing can resurrect the
            // garbage: detach the remaining weakrefs while still stopped
            let late = crate::types::weakref::detach_weakrefs_for_gc(&unreachable, false);
            start_the_world();
            late.finish();
            garbage.clone()
        } else {
            start_the_world();
            // ---- phase 2 (running): callbacks and finalizers
            early.finish();
            for i in crate::gc_support::finalization_order(&garbage) {
                let g = &garbage[i];
                let op = g.as_ptr();
                let tp = (*op).type_ptr();
                if (*tp).tp_finalize.is_some() && !is_finalized(op) {
                    set_finalized(op);
                    (*tp).tp_finalize.unwrap()(op);
                    if crate::errors::err_occurred() {
                        crate::modules::sys::write_unraisable_with_obj("Exception ignored in finalizer", Some(g));
                    }
                }
            }
            // ---- phase 3 (stopped): what is still garbage after the finalizers?
            if !stop_the_world(me) {
                // try again in a later collection (finalizers will not run twice)
                return Some(0);
            }
            crate::sharded::merge_all();
            let still: Vec<*mut PyObject> = garbage.iter().map(|g| g.as_ptr()).filter(|&o| is_tracked(&*o)).collect();
            // the references held in `garbage` count as external: discount them
            for &o in &still {
                adjust(o, -1);
            }
            let unreachable2 = find_unreachable(still.clone());
            for &o in &still {
                adjust(o, 1);
            }
            let late = crate::types::weakref::detach_weakrefs_for_gc(&unreachable2, false);
            let v: Vec<PyRef> = unreachable2.iter().map(|&o| PyRef::from_borrowed(o)).collect();
            start_the_world();
            late.finish();
            v
        };
        // ---- phase 4 (running): break the cycles (gc.DEBUG_SAVEALL keeps them)
        if let Some(l) = legacy.take().filter(|l| !l.is_empty()) {
            UNCOLLECTABLE.fetch_add(crate::gc_support::report_uncollectable(l), Relaxed);
        }
        let countable = crate::gc_support::countable(&to_clear);
        let n = countable.len();
        if crate::gc_support::save_all(&countable) {
            drop(to_clear);
            drop(garbage);
            COLLECTED.fetch_add(n, Relaxed);
            return Some(n + n_legacy);
        }
        for g in &to_clear {
            let op = g.as_ptr();
            if !is_tracked(&*op) {
                continue;
            }
            let tp = (*op).type_ptr();
            if let Some(clear) = (*tp).tp_clear {
                clear(op);
                if crate::errors::err_occurred() {
                    crate::modules::sys::write_unraisable_with_obj("Exception ignored in tp_clear", Some(g));
                }
            }
        }
        drop(to_clear);
        drop(garbage);
        COLLECTED.fetch_add(n, Relaxed);
        Some(n + n_legacy)
    }
}

/// Temporarily adjust a refcount without triggering deallocation.
unsafe fn adjust(op: *mut PyObject, delta: isize) {
    unsafe {
        let o = &*op;
        if o.is_immortal() {
            return;
        }
        o.ob_ref_shared.fetch_add(delta << REF_SHARED_SHIFT, Relaxed);
    }
}

/// Called from the eval breaker when a collection was requested on this thread.
pub fn maybe_collect(ts: &ThreadState) {
    ts.c.eval_breaker.fetch_and(!crate::threads::EVAL_GC, AcqRel);
    if ENABLED.load(Relaxed) && ALLOCS_SINCE.load(Relaxed) >= trigger_threshold() {
        // full collection every 10 young ones, or when the heap grew by half since
        // the last full collection (cyclic garbage that reached the old generation)
        let tracked = TRACKED.load(Relaxed).max(0) as usize;
        let full = YOUNG_COLLECTIONS.load(Relaxed) >= 10 && tracked > LAST_FULL_LIVE.load(Relaxed) + LAST_FULL_LIVE.load(Relaxed) / 2;
        collect_gen(!full);
    } else if !DEFERRED.lock().unwrap_or_else(|e| e.into_inner()).is_empty() {
        // collections are disabled (or not due): still release deferred storage
        flush_deferred();
    }
}

/// A stop-the-world handshake that only releases deferred references.
pub fn flush_deferred() {
    if COLLECTING.swap(true, AcqRel) {
        return;
    }
    let me = crate::threads::current_mut();
    if stop_the_world(me) {
        mature_deferred();
        start_the_world();
    }
    COLLECTING.store(false, Release);
    release_mature();
}

/// (jit6) Wait until every other thread has passed a safepoint (or is outside the VM).
/// Used once when MULTI_THREADED was set by a self-registering thread
/// (threads::MT_HANDSHAKE): afterwards no thread is still inside a fast path that
/// assumed a single thread. The caller must not be counted as running Python code.
pub fn handshake_safepoint() {
    let me = crate::threads::current_mut();
    if me.is_null() {
        return;
    }
    let mut spins = 0u32;
    while COLLECTING.compare_exchange(false, true, AcqRel, Acquire).is_err() {
        // a collection stops the world itself, which is just as good
        std::thread::sleep(std::time::Duration::from_micros(100));
        spins += 1;
        if spins > 20000 {
            return;
        }
    }
    for _ in 0..40 {
        if stop_the_world(me) {
            start_the_world();
            break;
        }
    }
    COLLECTING.store(false, Release);
}

/// All tracked objects (for gc.get_objects / get_referrers).
pub fn all_tracked() -> Vec<PyRef> {
    if !crate::threads::multi_threaded() {
        let v = snapshot();
        return v.into_iter().map(|o| unsafe { PyRef::from_borrowed(o) }).collect();
    }
    // the heap walk needs the other threads parked (they allocate and free on the
    // pages being walked); take references before they run again
    // (scale1) called by the collecting thread itself (a finalizer or weakref callback
    // run by the collection, with the world running again): it already holds
    // COLLECTING; waiting for it would never end
    let own = COLLECTOR.load(Acquire) == crate::object::thread_id();
    while !own && COLLECTING.compare_exchange(false, true, AcqRel, Acquire).is_err() {
        crate::threads::blocking(|| std::thread::sleep(std::time::Duration::from_micros(200)));
    }
    let me = crate::threads::current_mut();
    let mut stopped = false;
    for _ in 0..40 {
        if stop_the_world(me) {
            stopped = true;
            break;
        }
    }
    let v: Vec<PyRef> = snapshot().into_iter().map(|o| unsafe { PyRef::from_borrowed(o) }).collect();
    if stopped {
        start_the_world();
    }
    if !own {
        COLLECTING.store(false, Release);
    }
    v
}

// ---------------------------------------------------------------- fork support
//
// fork() keeps only the calling thread.  Before forking, the other threads
// are brought to a safepoint (so that none of them is inside the collector or
// the allocator).  See crate::fork.

/// Stop every other thread for fork().  Returns whether they all arrived
/// (threads blocked in system calls count as stopped).
pub fn fork_prepare() -> bool {
    // no collection may run concurrently
    while COLLECTING.compare_exchange(false, true, AcqRel, Acquire).is_err() {
        crate::threads::blocking(|| std::thread::sleep(std::time::Duration::from_micros(200)));
    }
    let me = crate::threads::current_mut();
    let mut stopped = false;
    for _ in 0..40 {
        if stop_the_world(me) {
            stopped = true;
            break;
        }
    }
    stopped
}

/// Undo fork_prepare() in the parent.
pub fn fork_parent(stopped: bool) {
    if stopped {
        start_the_world();
    }
    COLLECTING.store(false, Release);
}

/// Reset the collector state in the child: the other threads are gone.
pub fn fork_child() {
    PARKED.store(0, Release);
    STW_OWNER.store(0, SeqCst);
    RELEASE.store(true, Release);
    COLLECTING.store(false, Release);
}
