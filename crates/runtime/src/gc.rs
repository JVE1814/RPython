//! Cycle collector & GC object registry.
//! Phase 1: tracked-object list + tracking bits; collection is not yet implemented
//! (reference counting alone reclaims acyclic garbage). The HotSpot-style safepoint
//! collector (D5) replaces this.

use crate::alloc::{GC_BIT_FINALIZED, GC_BIT_TRACKED};
use crate::ffi::PyObject;
use crate::threads::ThreadState;
use std::sync::atomic::Ordering::*;

#[inline]
pub unsafe fn track(op: *mut PyObject) {
    unsafe {
        let o = &*op;
        if o.ob_gc_bits.load(Relaxed) & crate::alloc::GC_BIT_PREHEADER != 0 {
            o.ob_gc_bits.fetch_or(GC_BIT_TRACKED, Relaxed);
        }
    }
}
#[inline]
pub unsafe fn untrack(op: *mut PyObject) {
    unsafe {
        (*op).ob_gc_bits.fetch_and(!GC_BIT_TRACKED, Relaxed);
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
pub fn safepoint_poll(_ts: &ThreadState) {}
pub fn collect() -> usize {
    0
}
