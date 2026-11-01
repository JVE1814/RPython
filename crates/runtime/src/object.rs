//! Object header operations: biased reference counting (the protocol the
//! free-threaded headers implement inline), owned references, allocation.

use crate::ffi::*;
use std::ptr::NonNull;
use std::sync::atomic::Ordering::*;

/// The thread id used by the biased refcount protocol: the TCB self pointer
/// (`fs:0` on x86_64 Linux), identical to the inline `_Py_ThreadId()` in the headers.
#[inline(always)]
pub fn thread_id() -> usize {
    let tid: usize;
    #[cfg(target_arch = "x86_64")]
    unsafe {
        std::arch::asm!("mov {}, qword ptr fs:[0]", out(reg) tid, options(nostack, readonly, preserves_flags));
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        std::arch::asm!("mrs {}, tpidr_el0", out(reg) tid, options(nostack, readonly, preserves_flags));
    }
    tid
}

impl PyObject {
    #[inline(always)]
    pub fn as_ptr(&self) -> *mut PyObject {
        self as *const PyObject as *mut PyObject
    }
    #[inline(always)]
    pub fn ty(&self) -> &PyTypeObject {
        unsafe { &*self.ob_type.load(Relaxed) }
    }
    #[inline(always)]
    pub fn type_ptr(&self) -> *mut PyTypeObject {
        self.ob_type.load(Relaxed)
    }
    #[inline(always)]
    pub fn is_type(&self, t: *const PyTypeObject) -> bool {
        self.ob_type.load(Relaxed) as *const _ == t
    }
    #[inline(always)]
    pub fn is(&self, other: &PyObject) -> bool {
        std::ptr::eq(self, other)
    }
    #[inline(always)]
    pub fn is_immortal(&self) -> bool {
        self.ob_ref_local.load(Relaxed) == IMMORTAL_REFCNT_LOCAL
    }
    /// Current total reference count (approximate under races).
    pub fn refcnt(&self) -> isize {
        let local = self.ob_ref_local.load(Relaxed);
        if local == IMMORTAL_REFCNT_LOCAL {
            return (3isize << 30) as isize;
        }
        let shared = self.ob_ref_shared.load(Relaxed);
        local as isize + (shared >> REF_SHARED_SHIFT)
    }
    /// Return a new owned reference to this object.
    #[inline(always)]
    pub fn to_ref(&self) -> PyRef {
        unsafe {
            incref(self.as_ptr());
            PyRef::from_raw(self.as_ptr())
        }
    }
    #[inline(always)]
    pub unsafe fn cast<T>(&self) -> &T {
        unsafe { &*(self as *const PyObject as *const T) }
    }
    #[inline(always)]
    pub fn cast_ptr<T>(&self) -> *mut T {
        self as *const PyObject as *mut T
    }
}

#[inline(always)]
pub unsafe fn incref(op: *mut PyObject) {
    unsafe {
        let o = &*op;
        let local = o.ob_ref_local.load(Relaxed);
        let new_local = local.wrapping_add(1);
        if new_local == 0 {
            return; // immortal
        }
        if o.ob_tid.load(Relaxed) == thread_id() {
            o.ob_ref_local.store(new_local, Relaxed);
        } else {
            o.ob_ref_shared.fetch_add(1 << REF_SHARED_SHIFT, Relaxed);
        }
    }
}

#[inline(always)]
pub unsafe fn xincref(op: *mut PyObject) {
    if !op.is_null() {
        unsafe { incref(op) }
    }
}

#[inline(always)]
pub unsafe fn decref(op: *mut PyObject) {
    unsafe {
        let o = &*op;
        let local = o.ob_ref_local.load(Relaxed);
        if local == IMMORTAL_REFCNT_LOCAL {
            return;
        }
        if o.ob_tid.load(Relaxed) == thread_id() {
            let local = local - 1;
            o.ob_ref_local.store(local, Relaxed);
            if local == 0 {
                merge_zero_local_refcount(op);
            }
        } else {
            decref_shared(op);
        }
    }
}

#[inline(always)]
pub unsafe fn xdecref(op: *mut PyObject) {
    if !op.is_null() {
        unsafe { decref(op) }
    }
}

/// Called by the owning thread when its local count drops to zero.
#[cold]
pub unsafe fn merge_zero_local_refcount(op: *mut PyObject) {
    unsafe {
        let o = &*op;
        let shared = o.ob_ref_shared.load(Acquire);
        if shared == 0 {
            dealloc(op);
            return;
        }
        // Give up ownership; merge into the shared field.
        o.ob_tid.store(0, Relaxed);
        let mut cur = shared;
        loop {
            let new = cur | REF_MERGED;
            match o.ob_ref_shared.compare_exchange_weak(cur, new, AcqRel, Acquire) {
                Ok(_) => {
                    if (new >> REF_SHARED_SHIFT) == 0 {
                        dealloc(op);
                    }
                    return;
                }
                Err(x) => cur = x,
            }
        }
    }
}

/// Decrement from a thread that does not own the object.
#[cold]
pub unsafe fn decref_shared(op: *mut PyObject) {
    unsafe {
        let o = &*op;
        let mut cur = o.ob_ref_shared.load(Relaxed);
        loop {
            let new = cur - (1 << REF_SHARED_SHIFT);
            let mut target = new;
            let mut queue = false;
            if (new >> REF_SHARED_SHIFT) < 0 && (cur & REF_SHARED_FLAG_MASK) != REF_MERGED
                && (cur & REF_SHARED_FLAG_MASK) != REF_QUEUED
            {
                // The owner still holds local references; ask it to merge.
                target = (new & !REF_SHARED_FLAG_MASK) | REF_QUEUED;
                queue = true;
            }
            match o.ob_ref_shared.compare_exchange_weak(cur, target, AcqRel, Relaxed) {
                Ok(_) => {
                    if queue {
                        crate::threads::queue_object_for_merge(op);
                    } else if (target & REF_SHARED_FLAG_MASK) == REF_MERGED
                        && (target >> REF_SHARED_SHIFT) == 0
                    {
                        dealloc(op);
                    }
                    return;
                }
                Err(x) => cur = x,
            }
        }
    }
}

/// Merge an object's local count into the shared count (owner thread only,
/// or any thread at a safepoint when the owner has exited).
pub unsafe fn explicit_merge(op: *mut PyObject) {
    unsafe {
        let o = &*op;
        if o.is_immortal() {
            return;
        }
        let local = o.ob_ref_local.load(Relaxed) as isize;
        o.ob_ref_local.store(0, Relaxed);
        o.ob_tid.store(0, Relaxed);
        let mut cur = o.ob_ref_shared.load(Relaxed);
        loop {
            let new = ((cur >> REF_SHARED_SHIFT) + local) << REF_SHARED_SHIFT | REF_MERGED;
            match o.ob_ref_shared.compare_exchange_weak(cur, new, AcqRel, Relaxed) {
                Ok(_) => {
                    if (new >> REF_SHARED_SHIFT) == 0 {
                        dealloc(op);
                    }
                    return;
                }
                Err(x) => cur = x,
            }
        }
    }
}

#[inline]
pub unsafe fn dealloc(op: *mut PyObject) {
    unsafe {
        let tp = (*op).type_ptr();
        match (*tp).tp_dealloc {
            Some(f) => f(op),
            None => crate::alloc::free_object(op),
        }
    }
}

pub unsafe fn make_immortal(op: *mut PyObject) {
    unsafe {
        let o = &*op;
        o.ob_tid.store(0, Relaxed);
        o.ob_ref_local.store(IMMORTAL_REFCNT_LOCAL, Relaxed);
        o.ob_ref_shared.store(0, Relaxed);
    }
}

/// An owned (strong) reference. Clone = incref, Drop = decref.
#[repr(transparent)]
pub struct PyRef(NonNull<PyObject>);

unsafe impl Send for PyRef {}
unsafe impl Sync for PyRef {}

impl PyRef {
    /// Take ownership of a new reference.
    #[inline(always)]
    pub unsafe fn from_raw(p: *mut PyObject) -> PyRef {
        debug_assert!(!p.is_null());
        PyRef(unsafe { NonNull::new_unchecked(p) })
    }
    #[inline(always)]
    pub unsafe fn from_raw_opt(p: *mut PyObject) -> Option<PyRef> {
        NonNull::new(p).map(PyRef)
    }
    /// Create an owned reference from a borrowed pointer (increfs).
    #[inline(always)]
    pub unsafe fn from_borrowed(p: *mut PyObject) -> PyRef {
        unsafe {
            incref(p);
            PyRef::from_raw(p)
        }
    }
    #[inline(always)]
    pub fn into_raw(self) -> *mut PyObject {
        let p = self.0.as_ptr();
        std::mem::forget(self);
        p
    }
    #[inline(always)]
    pub fn as_ptr(&self) -> *mut PyObject {
        self.0.as_ptr()
    }
}

impl Clone for PyRef {
    #[inline(always)]
    fn clone(&self) -> PyRef {
        unsafe { incref(self.0.as_ptr()) };
        PyRef(self.0)
    }
}

impl Drop for PyRef {
    #[inline(always)]
    fn drop(&mut self) {
        unsafe { decref(self.0.as_ptr()) }
    }
}

impl std::ops::Deref for PyRef {
    type Target = PyObject;
    #[inline(always)]
    fn deref(&self) -> &PyObject {
        unsafe { self.0.as_ref() }
    }
}

impl std::fmt::Debug for PyRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<{} object at {:p}>", crate::types::typeobject::type_name(self.ty()), self.as_ptr())
    }
}

pub fn opt_ptr(o: &Option<PyRef>) -> *mut PyObject {
    match o {
        Some(r) => r.as_ptr(),
        None => std::ptr::null_mut(),
    }
}

/// Initialise the header of freshly allocated memory.
#[inline(always)]
pub unsafe fn init_header(op: *mut PyObject, tp: *mut PyTypeObject) {
    unsafe {
        let o = &*op;
        o.ob_tid.store(thread_id(), Relaxed);
        o.ob_flags.store(0, Relaxed);
        o.ob_mutex.store(0, Relaxed);
        o.ob_gc_bits.store(0, Relaxed);
        o.ob_ref_local.store(1, Relaxed);
        o.ob_ref_shared.store(0, Relaxed);
        o.ob_type.store(tp, Relaxed);
        if (*tp).tp_flags & Py_TPFLAGS_HEAPTYPE != 0 {
            incref(tp as *mut PyObject);
        }
    }
}

#[inline(always)]
pub fn var_size(o: &PyObject) -> isize {
    unsafe { (*(o as *const PyObject as *const PyVarObject)).ob_size }
}
#[inline(always)]
pub unsafe fn set_var_size(o: *mut PyObject, n: isize) {
    unsafe { (*(o as *mut PyVarObject)).ob_size = n }
}
