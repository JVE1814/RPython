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
    // Like a generational heuristic: collect after THRESHOLD new objects, but also
    // at least a quarter of the live set (amortises full collections).
    THRESHOLD.load(Relaxed).max(LAST_LIVE.load(Relaxed) / 4)
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

static PARKED: AtomicUsize = AtomicUsize::new(0);
static RELEASE: AtomicBool = AtomicBool::new(true);

/// Ask the current thread to collect at its next eval-breaker check.
fn request_collection() {
    let ts = crate::threads::current();
    ts.c.eval_breaker.fetch_or(crate::threads::EVAL_GC, Release);
}

/// Called by threads at safepoints (backedges, function entry via the eval breaker).
pub fn safepoint_poll(ts: &ThreadState) {
    if RELEASE.load(Acquire) {
        ts.c.eval_breaker.fetch_and(!crate::threads::EVAL_SAFEPOINT, AcqRel);
        return;
    }
    PARKED.fetch_add(1, AcqRel);
    while !RELEASE.load(Acquire) {
        std::thread::yield_now();
    }
    PARKED.fetch_sub(1, AcqRel);
    ts.c.eval_breaker.fetch_and(!crate::threads::EVAL_SAFEPOINT, AcqRel);
}

/// Bring all other threads to a safepoint. Returns false if they did not arrive in time.
fn stop_the_world(me: *mut ThreadState) -> bool {
    let threads: Vec<usize> = crate::threads::ALL_THREADS.lock().unwrap().clone();
    let others: Vec<&ThreadState> = threads
        .iter()
        .filter(|&&t| t != me as usize)
        .map(|&t| unsafe { &*(t as *const ThreadState) })
        .filter(|t| t.alive())
        .collect();
    if others.is_empty() {
        return true;
    }
    RELEASE.store(false, Release);
    for t in         // keep garbage alive while we work on it
        let garbage: Vec<PyRef> = unreachable.iter().map(|&o| PyRef::from_borrowed(o)).collect();
        // 0. weakrefs with callbacks to garbage are cleared first (their callbacks run
        //    unless the weakref itself is garbage), so finalizers cannot observe them
        crate::types::weakref::clear_weakrefs_for_gc(&unreachable, true);
thers {
        t.c.eval_breaker.fetch_or(crate::threads::EVAL_SAFEPOINT, Release);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(50);
    loop {
        let running = others.iter().filter(|t| t.mode.get() == crate::threads::ThreadMode::InVm).count();
        if PARKED.load(Acquire) >= running {
            return true;
        }
        if std::time::Instant::now() > deadline {
            start_the_world();
            return false;
        }
        std::thread::yield_now();
    }
}

fn start_the_world() {
    RELEASE.store(true, Release);
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

struct Graph {
    objs: Vec<*mut PyObject>,
    index: HashMap<usize, usize, std::hash::BuildHasherDefault<PtrHasher>>,
    refs: Vec<isize>,
    reachable: Vec<bool>,
}

thread_local! {
    static CUR: std::cell::Cell<*mut Graph> = const { std::cell::Cell::new(std::ptr::null_mut()) };
}

unsafe extern "C" fn visit_decref(op: *mut PyObject, _arg: *mut c_void) -> c_int {
    CUR.with(|c| {
        let g = unsafe { &mut *c.get() };
        if let Some(&i) = g.index.get(&(op as usize)) {
            g.refs[i] -= 1;
        }
    });
    0
}

unsafe extern "C" fn visit_mark(op: *mut PyObject, arg: *mut c_void) -> c_int {
    CUR.with(|c| {
        let g = unsafe { &mut *c.get() };
        if let Some(&i) = g.index.get(&(op as usize)) {
            if !g.reachable[i] {
                g.reachable[i] = true;
                let stack = unsafe { &mut *(arg as *mut Vec<usize>) };
                stack.push(i);
            }
        }
    });
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

/// Compute the unreachable subset of `objs`.
unsafe fn find_unreachable(objs: Vec<*mut PyObject>) -> Vec<*mut PyObject> {
    unsafe {
        let mut g = Graph {
            index: objs.iter().enumerate().map(|(i, &o)| (o as usize, i)).collect(),
            refs: objs.iter().map(|&o| total_refcnt(&*o)).collect(),
            reachable: vec![false; objs.len()],
            objs,
        };
        CUR.with(|c| c.set(&mut g as *mut Graph));
        for i in 0..g.objs.len() {
            traverse(g.objs[i], Some(visit_decref), std::ptr::null_mut());
        }
        let mut stack: Vec<usize> = Vec::new();
        for i in 0..g.objs.len() {
            if g.refs[i] > 0 && !g.reachable[i] {
                g.reachable[i] = true;
                stack.push(i);
            }
        }
        while let Some(i) = stack.pop() {
            let op = g.objs[i];
            traverse(op, Some(visit_mark), &mut stack as *mut Vec<usize> as *mut c_void);
        }
        CUR.with(|c| c.set(std::ptr::null_mut()));
        g.objs.iter().zip(g.reachable.iter()).filter(|(_, r)| !**r).map(|(o, _)| *o).collect()
    }
}

fn snapshot() -> Vec<*mut PyObject> {
    with_list(|head| unsafe {
        let mut v = Vec::with_capacity(TRACKED.load(Relaxed).max(0) as usize);
        let mut h = (*head).next;
        while h != head {
            v.push((h as *mut u8).add(std::mem::size_of::<GcHead>()) as *mut PyObject);
            h = (*h).next;
        }
        v
    })
}

/// Run a full collection; returns the number of unreachable objects found.
pub fn collect() -> usize {
    if COLLECTING.swap(true, AcqRel) {
        return 0;
    }
    let me = crate::threads::current_mut();
    let ok = stop_the_world(me);
    if !ok {
        COLLECTING.store(false, Release);
        return 0;
    }
    let n = unsafe { collect_stopped() };
    start_the_world();
    ALLOCS_SINCE.store(0, Relaxed);
    LAST_LIVE.store(TRACKED.load(Relaxed).max(0) as usize, Relaxed);
    COLLECTIONS.fetch_add(1, Relaxed);
    COLLECTING.store(false, Release);
    n
}

unsafe fn collect_stopped() -> usize {
    unsafe {
        let saved_exc = crate::errors::take_exc();
        let objs = snapshot();
        let unreachable = find_unreachable(objs);
        if unreachable.is_empty() {
            if let Some(e) = saved_exc {
                crate::errors::set_exc(e);
            }
            return 0;
        }
        // keep garbage alive while we work on it
        let garbage: Vec<PyRef> = unreachable.iter().map(|&o| PyRef::from_borrowed(o)).collect();
        // 1. finalizers (each object finalized at most once)
        let mut finalized_any = false;
        for g in &garbage {
            let op = g.as_ptr();
            let tp = (*op).type_ptr();
            if (*tp).tp_finalize.is_some() && !is_finalized(op) {
                set_finalized(op);
                (*tp).tp_finalize.unwrap()(op);
                if crate::errors::err_occurred() {
                    crate::modules::sys::write_unraisable_with_obj("Exception ignored in finalizer", Some(g));
                }
                finalized_any = true;
            }
        }
        // 2. if finalizers ran, objects may have been resurrected: recompute
        let to_clear: Vec<PyRef> = if finalized_any {
            let still: Vec<*mut PyObject> = garbage.iter().map(|g| g.as_ptr()).filter(|&o| is_tracked(&*o)).collect();
            // the temporary refs held in `garbage` count as external: discount them
            for &o in &still {
                adjust(o, -1);
            }
            let unreachable2 = find_unreachable(still.clone());
            for &o in &still {
                adjust(o, 1);
            }
            unreachable2.iter().map(|&o| PyRef::from_borrowed(o)).collect()
        } else {
            garbage.clone()
        };
        let n = to_clear.len();
        // 3. the remaining weak references to garbage are cleared before tp_clear
        {
            let ptrs: Vec<*mut PyObject> = to_clear.iter().map(|g| g.as_ptr()).collect();
            crate::types::weakref::clear_weakrefs_for_gc(&ptrs, false);
        }
        // 4. break cycles
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
        if let Some(e) = saved_exc {
            crate::errors::set_exc(e);
        }
        n
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
        collect();
    }
}

/// All tracked objects (for gc.get_objects / get_referrers).
pub fn all_tracked() -> Vec<PyRef> {
    let v = snapshot();
    v.into_iter().map(|o| unsafe { PyRef::from_borrowed(o) }).collect()
}
