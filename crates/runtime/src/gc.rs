//! Cycle collector.
//!
//! Reference counting (forced by the C ABI) frees acyclic garbage; this module finds
//! unreachable reference cycles. Design (DECISIONS D5):
//! * GC-capable objects are linked into a global intrusive list through the 16-byte
//!   pre-header (`alloc::GcHead`) while tracked.
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

use crate::alloc::{GC_BIT_FINALIZED, GC_BIT_TRACKED, GcHead, gc_head};
use crate::ffi::*;
use crate::object::*;
use crate::threads::ThreadState;
use std::collections::HashMap;
use std::ffi::{c_int, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering::*};

struct List {
    head: GcHead,
}
unsafe impl Send for List {}

static LIST: Mutex<Option<Box<List>>> = Mutex::new(None);
static TRACKED: AtomicIsize = AtomicIsize::new(0);
static ALLOCS_SINCE: AtomicUsize = AtomicUsize::new(0);
pub static ENABLED: AtomicBool = AtomicBool::new(true);
pub static THRESHOLD: AtomicUsize = AtomicUsize::new(2000);
static COLLECTING: AtomicBool = AtomicBool::new(false);
pub static COLLECTIONS: AtomicUsize = AtomicUsize::new(0);
pub static COLLECTED: AtomicUsize = AtomicUsize::new(0);
pub static UNCOLLECTABLE: AtomicUsize = AtomicUsize::new(0);
/// Size of the live set after the last collection (for adaptive triggering).
static LAST_LIVE: AtomicUsize = AtomicUsize::new(0);

/// Spin lock protecting the list (critical sections are a few pointer writes; a
/// std Mutex cost a TLS poison check + futex word on every track/untrack).
static LIST_LOCK: AtomicBool = AtomicBool::new(false);
static LIST_HEAD: std::sync::atomic::AtomicPtr<GcHead> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

struct ListGuard;
impl Drop for ListGuard {
    fn drop(&mut self) {
        LIST_LOCK.store(false, Release);
    }
}

#[inline]
fn with_list<R>(f: impl FnOnce(*mut GcHead) -> R) -> R {
    while LIST_LOCK.compare_exchange_weak(false, true, Acquire, Relaxed).is_err() {
        std::hint::spin_loop();
    }
    let _g = ListGuard;
    let mut head = LIST_HEAD.load(Relaxed);
    if head.is_null() {
        let mut g = LIST.lock().unwrap_or_else(|e| e.into_inner());
        let l = g.get_or_insert_with(|| {
            let mut b = Box::new(List { head: GcHead { next: std::ptr::null_mut(), prev: std::ptr::null_mut() } });
            let p = &mut b.head as *mut GcHead;
            b.head.next = p;
            b.head.prev = p;
            b
        });
        head = &mut l.head as *mut GcHead;
        LIST_HEAD.store(head, Relaxed);
    }
    f(head)
}

/// Old generation: objects that survived a collection. New objects are tracked in
/// the young list (`with_list`); young collections examine only the young list
/// (references from old objects count as external, which is conservative), then
/// promote the survivors. Full collections examine both lists.
static OLD_HEAD: std::sync::atomic::AtomicPtr<GcHead> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
static YOUNG_COLLECTIONS: AtomicUsize = AtomicUsize::new(0);
/// Tracked objects right after the last full collection.
static LAST_FULL_LIVE: AtomicUsize = AtomicUsize::new(0);

/// Run `f(young, old)` under the list lock.
fn with_lists<R>(f: impl FnOnce(*mut GcHead, *mut GcHead) -> R) -> R {
    with_list(|young| {
        let mut old = OLD_HEAD.load(Relaxed);
        if old.is_null() {
            let b = Box::leak(Box::new(GcHead { next: std::ptr::null_mut(), prev: std::ptr::null_mut() }));
            let p = b as *mut GcHead;
            b.next = p;
            b.prev = p;
            old = p;
            OLD_HEAD.store(old, Relaxed);
        }
        f(young, old)
    })
}

/// Move every object of the young list to the end of the old list.
fn promote_young() {
    with_lists(|young, old| unsafe {
        if (*young).next == young {
            return;
        }
        let first = (*young).next;
        let last = (*young).prev;
        let old_last = (*old).prev;
        (*old_last).next = first;
        (*first).prev = old_last;
        (*last).next = old;
        (*old).prev = last;
        (*young).next = young;
        (*young).prev = young;
    })
}

#[inline]
pub unsafe fn track(op: *mut PyObject) {
    unsafe {
        let o = &*op;
        let bits = o.ob_gc_bits.load(Relaxed);
        if bits & crate::alloc::GC_BIT_PREHEADER == 0 || bits & GC_BIT_TRACKED != 0 {
            return;
        }
        let h = gc_head(op);
        with_list(|head| {
            (*h).next = head;
            (*h).prev = (*head).prev;
            (*(*head).prev).next = h;
            (*head).prev = h;
        });
        o.ob_gc_bits.fetch_or(GC_BIT_TRACKED, Relaxed);
        TRACKED.fetch_add(1, Relaxed);
        let n = ALLOCS_SINCE.fetch_add(1, Relaxed) + 1;
        if n >= trigger_threshold() && ENABLED.load(Relaxed) && !COLLECTING.load(Relaxed) {
            request_collection();
        }
    }
}

fn trigger_threshold() -> usize {
    // young collection after THRESHOLD net new tracked objects
    let _ = LAST_LIVE.load(Relaxed);
    THRESHOLD.load(Relaxed)
}

#[inline]
pub unsafe fn untrack(op: *mut PyObject) {
    unsafe {
        let o = &*op;
        if o.ob_gc_bits.load(Relaxed) & GC_BIT_TRACKED == 0 {
            return;
        }
        let h = gc_head(op);
        with_list(|_| {
            (*(*h).prev).next = (*h).next;
            (*(*h).next).prev = (*h).prev;
            (*h).next = std::ptr::null_mut();
            (*h).prev = std::ptr::null_mut();
        });
        o.ob_gc_bits.fetch_and(!GC_BIT_TRACKED, Relaxed);
        TRACKED.fetch_sub(1, Relaxed);
        // allocations minus deallocations (like CPython gen0 counts): short-lived
        // objects do not trigger collections
        let _ = ALLOCS_SINCE.fetch_update(Relaxed, Relaxed, |v| v.checked_sub(1));
    }
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

/// Switch a thread's mode to `InVm` following the handshake above. `safe` is the
/// mode the thread waits in while the world is stopped.
pub fn enter_vm(cell: &crate::threads::ModeCell, safe: crate::threads::ThreadMode) {
    use crate::threads::ThreadMode;
    let safe = if safe == ThreadMode::InVm { ThreadMode::Blocked } else { safe };
    loop {
        cell.0.store(ThreadMode::InVm as u8, SeqCst);
        if RELEASE.load(SeqCst) || STW_OWNER.load(SeqCst) == cell as *const _ as usize {
            return;
        }
        cell.0.store(safe as u8, SeqCst);
        wait_for_release();
    }
}

/// Is a stop-the-world operation in progress?
#[inline]
pub fn world_stopped() -> bool {
    !RELEASE.load(SeqCst)
}

/// Called by threads at safepoints (backedges, function entry via the eval breaker).
pub fn safepoint_poll(ts: &ThreadState) {
    ts.c.eval_breaker.fetch_and(!crate::threads::EVAL_SAFEPOINT, AcqRel);
    if RELEASE.load(SeqCst) || STW_OWNER.load(SeqCst) == &ts.mode as *const _ as usize {
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
}

/// Multiplicative hash for object addresses (SipHash dominated collection time).
#[derive(Default)]
struct PtrHasher(u64);
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

/// Trial-deletion graph. Membership and the dense index of an object are stored in
/// its GC pre-header while a collection runs (`prev` temporarily holds
/// `index << 2 | 3`; real link pointers are 8-aligned, so the tag cannot collide),
/// which avoids a hash map lookup per visited reference. The links are restored
/// before returning. The graph is passed to the visitors through their `arg`.
struct Graph {
    objs: Vec<*mut PyObject>,
    saved_prev: Vec<*mut GcHead>,
    refs: Vec<isize>,
    reachable: Vec<bool>,
    stack: Vec<usize>,
}

const GRAPH_TAG: usize = 3;

#[inline(always)]
unsafe fn graph_index(op: *mut PyObject) -> Option<usize> {
    unsafe {
        if op.is_null() || (*op).ob_gc_bits.load(Relaxed) & GC_BIT_TRACKED == 0 {
            return None;
        }
        let p = (*gc_head(op)).prev as usize;
        if p & GRAPH_TAG == GRAPH_TAG { Some(p >> 2) } else { None }
    }
}

unsafe extern "C" fn visit_decref(op: *mut PyObject, arg: *mut c_void) -> c_int {
    unsafe {
        if let Some(i) = graph_index(op) {
            let g = &mut *(arg as *mut Graph);
            g.refs[i] -= 1;
        }
    }
    0
}

unsafe extern "C" fn visit_mark(op: *mut PyObject, arg: *mut c_void) -> c_int {
    unsafe {
        if let Some(i) = graph_index(op) {
            let g = &mut *(arg as *mut Graph);
            if !g.reachable[i] {
                g.reachable[i] = true;
                g.stack.push(i);
            }
        }
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

/// Compute the unreachable subset of `objs` (all tracked, no duplicates).
unsafe fn find_unreachable(objs: Vec<*mut PyObject>) -> Vec<*mut PyObject> {
    unsafe {
        let n = objs.len();
        let mut g = Graph {
            saved_prev: Vec::with_capacity(n),
            refs: objs.iter().map(|&o| total_refcnt(&*o)).collect(),
            reachable: vec![false; n],
            stack: Vec::new(),
            objs,
        };
        for (i, &o) in g.objs.iter().enumerate() {
            let h = gc_head(o);
            g.saved_prev.push((*h).prev);
            (*h).prev = ((i << 2) | GRAPH_TAG) as *mut GcHead;
        }
        let gp = &mut g as *mut Graph as *mut c_void;
        for i in 0..n {
            traverse(g.objs[i], Some(visit_decref), gp);
        }
        for i in 0..n {
            if g.refs[i] > 0 && !g.reachable[i] {
                g.reachable[i] = true;
                g.stack.push(i);
            }
        }
        while let Some(i) = g.stack.pop() {
            let op = g.objs[i];
            traverse(op, Some(visit_mark), gp);
        }
        for (i, &o) in g.objs.iter().enumerate() {
            (*gc_head(o)).prev = g.saved_prev[i];
        }
        g.objs.iter().zip(g.reachable.iter()).filter(|(_, r)| !**r).map(|(o, _)| *o).collect()
    }
}

fn snapshot() -> Vec<*mut PyObject> {
    snapshot_gen(false)
}

/// Tracked objects of the young generation (young_only) or of both generations.
fn snapshot_gen(young_only: bool) -> Vec<*mut PyObject> {
    with_lists(|young, old| unsafe {
        let mut v = Vec::with_capacity(if young_only { 1024 } else { TRACKED.load(Relaxed).max(0) as usize });
        let lists: &[*mut GcHead] = if young_only { &[young] } else { &[young, old] };
        for &head in lists {
            let mut h = (*head).next;
            while h != head {
                v.push((h as *mut u8).add(std::mem::size_of::<GcHead>()) as *mut PyObject);
                h = (*h).next;
            }
        }
        v
    })
}

/// Run a full collection; returns the number of unreachable objects found.
pub fn collect() -> usize {
    collect_gen(false)
}

fn collect_gen(young_only: bool) -> usize {
    if COLLECTING.swap(true, AcqRel) {
        return 0;
    }
    let me = crate::threads::current_mut();
    let saved_exc = crate::errors::take_exc();
    let n = unsafe { collect_phases(me, young_only) };
    release_mature();
    if let Some(e) = saved_exc {
        crate::errors::set_exc(e);
    }
    if let Some(n) = n {
        ALLOCS_SINCE.store(0, Relaxed);
        LAST_LIVE.store(TRACKED.load(Relaxed).max(0) as usize, Relaxed);
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
    COLLECTING.store(false, Release);
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
        // ---- phase 1 (stopped): find the unreachable objects
        let objs = snapshot_gen(young_only);
        let unreachable = find_unreachable(objs);
        promote_young();
        if unreachable.is_empty() {
            start_the_world();
            return Some(0);
        }
        // keep the garbage alive while we work on it
        let garbage: Vec<PyRef> = unreachable.iter().map(|&o| PyRef::from_borrowed(o)).collect();
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
            for g in &garbage {
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
        // ---- phase 4 (running): break the cycles
        let n = to_clear.len();
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
        Some(n)
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

/// All tracked objects (for gc.get_objects / get_referrers).
pub fn all_tracked() -> Vec<PyRef> {
    let v = snapshot();
    v.into_iter().map(|o| unsafe { PyRef::from_borrowed(o) }).collect()
}

// ---------------------------------------------------------------- fork support
//
// fork() keeps only the calling thread.  Before forking, the other threads
// are brought to a safepoint (so that none of them is inside the collector or
// in the middle of a GC list update), and the forking thread holds the list
// spin lock across the fork.  See crate::fork.

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
    while LIST_LOCK.compare_exchange_weak(false, true, Acquire, Relaxed).is_err() {
        std::hint::spin_loop();
    }
    stopped
}

/// Undo fork_prepare() in the parent.
pub fn fork_parent(stopped: bool) {
    LIST_LOCK.store(false, Release);
    if stopped {
        start_the_world();
    }
    COLLECTING.store(false, Release);
}

/// Reset the collector state in the child: the other threads are gone.
pub fn fork_child() {
    LIST_LOCK.store(false, Release);
    PARKED.store(0, Release);
    STW_OWNER.store(0, SeqCst);
    RELEASE.store(true, Release);
    COLLECTING.store(false, Release);
}
