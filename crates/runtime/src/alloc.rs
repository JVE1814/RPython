//! Object allocation. Small objects come from the thread-caching page allocator
//! (`tlab.rs`, HotSpot TLAB style); large ones from the system allocator.
//! GC objects carry a pre-header in front of the object:
//!   [managed dict][managed weakref]   (only if Py_TPFLAGS_PREHEADER bits set)
//!   [gc next][gc prev]                (always for GC objects)
//!   PyObject ...

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
/// Bits in ob_gc_bits (private to this runtime).
pub const GC_BIT_PREHEADER: u8 = 0x80;
pub const GC_BIT_MANAGED: u8 = 0x40;
/// Allocation came from the system allocator (not a tlab page).
pub const GC_BIT_LARGE: u8 = 0x20;
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

#[inline]
fn raw_obj_alloc(total: usize) -> (*mut u8, u8) {
    if total <= crate::tlab::MAX_SMALL {
        let p = crate::tlab::alloc_small(total);
        if !p.is_null() {
            return (p, 0);
        }
    }
    (unsafe { libc::calloc(1, total) as *mut u8 }, GC_BIT_LARGE)
}

/// Allocate `size` zeroed bytes for an object of type `tp` and initialise its header.
#[inline]
pub unsafe fn alloc_object(tp: *mut PyTypeObject, size: usize) -> *mut PyObject {
    unsafe {
        let (pre, bits) = preheader_for(tp);
        let (raw, large) = raw_obj_alloc(size + pre);
        if raw.is_null() {
            return std::ptr::null_mut();
        }
        let op = raw.add(pre) as *mut PyObject;
        init_header(op, tp);
        (*op).ob_gc_bits.store(bits | large, Relaxed);
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

#[inline]
pub unsafe fn free_object(op: *mut PyObject) {
    unsafe {
        let bits = (*op).ob_gc_bits.load(Relaxed);
        if bits & GC_BIT_TRACKED != 0 {
            crate::gc::untrack(op);
        }
        let start = alloc_start(op);
        if bits & GC_BIT_LARGE != 0 {
            libc::free(start as *mut libc::c_void);
        } else {
            crate::tlab::free_small(start);
        }
    }
}

pub unsafe fn realloc_object(op: *mut PyObject, new_size: usize) -> *mut PyObject {
    unsafe {
        let pre = preheader_size(op);
        let bits = (*op).ob_gc_bits.load(Relaxed);
        let start = alloc_start(op);
        let total = new_size + pre;
        if bits & GC_BIT_LARGE != 0 {
            let raw = libc::realloc(start as *mut libc::c_void, total) as *mut u8;
            if raw.is_null() {
                return std::ptr::null_mut();
            }
            return raw.add(pre) as *mut PyObject;
        }
        let old = crate::tlab::small_size_of(start);
        if total <= old {
            return op;
        }
        let (raw, large) = raw_obj_alloc(total);
        if raw.is_null() {
            return std::ptr::null_mut();
        }
        std::ptr::copy_nonoverlapping(start, raw, old);
        crate::tlab::free_small(start);
        let nop = raw.add(pre) as *mut PyObject;
        (*nop).ob_gc_bits.store((bits & !GC_BIT_LARGE) | large, Relaxed);
        nop
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
