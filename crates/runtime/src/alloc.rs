//! Object allocation. Phase-1 implementation on top of the system allocator.
//! GC objects carry a pre-header in front of the object:
//!   [managed dict][managed weakref]   (only if Py_TPFLAGS_PREHEADER bits set)
//!   [gc next][gc prev]                (always for GC objects)
//!   PyObject ...
//! (A thread-owned page allocator — TLAB style — replaces malloc later; see PLAN.md.)

use crate::ffi::*;
use crate::object::init_header;
use std::sync::atomic::Ordering::*;

#[repr(C)]
pub struct GcHead {
    pub next: *mut GcHead,
    pub prev: *mut GcHead,
}

pub const GC_HEAD: usize = std::mem::size_of::<GcHead>();
pub const MANAGED: usize = 16;
/// Bit in ob_gc_bits: the allocation has a GC pre-header.
pub const GC_BIT_PREHEADER: u8 = 0x80;
pub const GC_BIT_MANAGED: u8 = 0x40;
pub const GC_BIT_TRACKED: u8 = 0x01;
pub const GC_BIT_FINALIZED: u8 = 0x02;

#[inline]
pub fn type_is_gc(tp: *const PyTypeObject) -> bool {
    unsafe { (*tp).tp_flags & Py_TPFLAGS_HAVE_GC != 0 }
}

#[inline]
fn preheader_for(tp: *const PyTypeObject) -> (usize, u8) {
    unsafe {
        let f = (*tp).tp_flags;
        if f & Py_TPFLAGS_HAVE_GC == 0 {
            return (0, 0);
        }
        if f & Py_TPFLAGS_PREHEADER != 0 {
            (GC_HEAD + MANAGED, GC_BIT_PREHEADER | GC_BIT_MANAGED)
        } else {
            (GC_HEAD, GC_BIT_PREHEADER)
        }
    }
}

/// Allocate `size` zeroed bytes for an object of type `tp` and initialise its header.
pub unsafe fn alloc_object(tp: *mut PyTypeObject, size: usize) -> *mut PyObject {
    unsafe {
        let (pre, bits) = preheader_for(tp);
        let raw = libc::calloc(1, size + pre) as *mut u8;
        if raw.is_null() {
            return std::ptr::null_mut();
        }
        let op = raw.add(pre) as *mut PyObject;
        init_header(op, tp);
        (*op).ob_gc_bits.store(bits, Relaxed);
        op
    }
}

#[inline]
pub unsafe fn preheader_size(op: *mut PyObject) -> usize {
    unsafe {
        let b = (*op).ob_gc_bits.load(Relaxed);
        if b & GC_BIT_PREHEADER == 0 {
            0
        } else if b & GC_BIT_MANAGED != 0 {
            GC_HEAD + MANAGED
        } else {
            GC_HEAD
        }
    }
}

#[inline]
pub unsafe fn has_managed(op: *mut PyObject) -> bool {
    unsafe { (*op).ob_gc_bits.load(Relaxed) & GC_BIT_MANAGED != 0 }
}

/// Address of the managed __dict__ slot (only valid if has_managed).
#[inline]
pub unsafe fn managed_dict_slot(op: *mut PyObject) -> *mut *mut PyObject {
    unsafe { (op as *mut u8).sub(GC_HEAD + MANAGED) as *mut *mut PyObject }
}
#[inline]
pub unsafe fn managed_weakref_slot(op: *mut PyObject) -> *mut *mut PyObject {
    unsafe { (op as *mut u8).sub(GC_HEAD + 8) as *mut *mut PyObject }
}

#[inline]
pub unsafe fn gc_head(op: *mut PyObject) -> *mut GcHead {
    unsafe { (op as *mut u8).sub(GC_HEAD) as *mut GcHead }
}

/// Raw memory start of an object allocation.
#[inline]
pub unsafe fn alloc_start(op: *mut PyObject) -> *mut u8 {
    unsafe { (op as *mut u8).sub(preheader_size(op)) }
}

pub unsafe fn free_object(op: *mut PyObject) {
    unsafe {
        if (*op).ob_gc_bits.load(Relaxed) & GC_BIT_TRACKED != 0 {
            crate::gc::untrack(op);
        }
        libc::free(alloc_start(op) as *mut libc::c_void);
    }
}

pub unsafe fn realloc_object(op: *mut PyObject, new_size: usize) -> *mut PyObject {
    unsafe {
        let pre = preheader_size(op);
        let raw = libc::realloc(alloc_start(op) as *mut libc::c_void, new_size + pre) as *mut u8;
        if raw.is_null() {
            return std::ptr::null_mut();
        }
        raw.add(pre) as *mut PyObject
    }
}

pub unsafe fn raw_malloc(n: usize) -> *mut u8 {
    unsafe { libc::malloc(n.max(1)) as *mut u8 }
}
pub unsafe fn raw_calloc(n: usize) -> *mut u8 {
    unsafe { libc::calloc(1, n.max(1)) as *mut u8 }
}
pub unsafe fn raw_realloc(p: *mut u8, n: usize) -> *mut u8 {
    unsafe { libc::realloc(p as *mut libc::c_void, n.max(1)) as *mut u8 }
}
pub unsafe fn raw_free(p: *mut u8) {
    unsafe { libc::free(p as *mut libc::c_void) }
}
pub unsafe extern "C" fn free_object_c(op: *mut PyObject) {
    unsafe {
        let tp = (*op).type_ptr();
        free_object(op);
        if (*tp).tp_flags & Py_TPFLAGS_HEAPTYPE != 0 {
            crate::object::decref(tp as *mut PyObject);
        }
    }
}
