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
            let flags = cur & REF_SHARED_FLAG_MASK;
            let queue = (cur >> REF_SHARED_SHIFT) <= 0 && flags != REF_MERGED && flags != REF_QUEUED;
            let target = if queue {
                // The reference being dropped is part of the owner's local count.
                // Hand it to the owner's merge queue instead of decrementing: the
                // queue entry keeps the object alive until the owner merges (a
                // queued object must never be freed before the queue is drained).
                (cur & !REF_SHARED_FLAG_MASK) | REF_QUEUED
            } else {
                cur - (1 << REF_SHARED_SHIFT)
            };
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

/// Take a new reference to `op` unless it is already being deallocated, for callers
/// that reach the object through a pointer that does not own a reference (weakref
/// lists, caches). The object's memory must stay valid during the call (the caller
/// holds the lock its deallocator must take first). Races with the owner's final
/// decref are decided by the CAS on the shared field: the owner gives up an object
/// with references from other threads through a CAS too (merge), and frees directly
/// only when the shared field is exactly 0, which this function never increments.
/// So the shared field must be non-zero for other threads to succeed while the owner
/// holds only local references: `enable_try_incref` sets the MAYBE_WEAKREF flag.
pub unsafe fn try_incref(op: *mut PyObject) -> bool {
    unsafe {
        let o = &*op;
        if o.is_immortal() {
            return true;
        }
        if o.ob_tid.load(Relaxed) == thread_id() {
            let local = o.ob_ref_local.load(Relaxed);
            if local == 0 {
                return false; // the owner (this thread) is deallocating it
            }
            o.ob_ref_local.store(local + 1, Relaxed);
            return true;
        }
        let mut cur = o.ob_ref_shared.load(Relaxed);
        loop {
            if cur == 0 || ((cur & REF_SHARED_FLAG_MASK) == REF_MERGED && (cur >> REF_SHARED_SHIFT) <= 0) {
                return false;
            }
            match o.ob_ref_shared.compare_exchange_weak(cur, cur + (1 << REF_SHARED_SHIFT), AcqRel, Relaxed) {
                Ok(_) => return true,
                Err(x) => cur = x,
            }
        }
    }
}

/// Allow `try_incref` from other threads while `op`'s shared field would be 0.
pub unsafe fn enable_try_incref(op: *mut PyObject) {
    unsafe {
        let o = &*op;
        if !o.is_immortal() {
            let _ = o.ob_ref_shared.compare_exchange(0, REF_MAYBE_WEAKREF, Relaxed, Relaxed);
        }
    }
}

/// Merge an object's local count into the shared count (owner thread only,
/// or any thread at a safepoint when the owner has exited).
pub unsafe fn explicit_merge(op: *mut PyObject) {
    unsafe { merge_with_delta(op, 0) }
}

/// Merge an object taken from a merge queue: the queue entry owned one reference
/// (see `decref_shared`), which is released by the merge.
pub unsafe fn merge_queued(op: *mut PyObject) {
    unsafe { merge_with_delta(op, -1) }
}

unsafe fn merge_with_delta(op: *mut PyObject, delta: isize) {
    unsafe {
        let o = &*op;
        if o.is_immortal() {
            return;
        }
        let local = o.ob_ref_local.load(Relaxed) as isize + delta;
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

/// Deallocation nesting depth beyond which GC objects are deferred (the "trashcan":
/// freeing a deeply nested container must not recurse once per level on the C stack).
const TRASHCAN_DEPTH: u32 = 64;

thread_local! {
    static DEALLOC_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static TRASH: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[inline]
unsafe fn call_dealloc(op: *mut PyObject) {
    unsafe {
        let tp = (*op).type_ptr();
        match (*tp).tp_dealloc {
            Some(f) => f(op),
            None => crate::alloc::free_object(op),
        }
    }
}

#[inline]
pub unsafe fn dealloc(op: *mut PyObject) {
    unsafe {
        let is_gc = (*(*op).type_ptr()).tp_flags & crate::ffi::Py_TPFLAGS_HAVE_GC != 0;
        if !is_gc {
            return call_dealloc(op);
        }
        let Ok(depth) = DEALLOC_DEPTH.try_with(|d| d.get()) else {
            return call_dealloc(op); // thread is exiting
        };
        if depth >= TRASHCAN_DEPTH {
            // defer: the outermost dealloc on this thread frees it
            crate::gc::untrack(op);
            TRASH.with(|t| t.borrow_mut().push(op as usize));
            return;
        }
        DEALLOC_DEPTH.with(|d| d.set(depth + 1));
        call_dealloc(op);
        if depth == 0 {
            // drain deferred objects (each may defer more)
            loop {
                let next = TRASH.with(|t| t.borrow_mut().pop());
                match next {
                    Some(p) => call_dealloc(p as *mut PyObject),
                    None => break,
                }
            }
        }
        DEALLOC_DEPTH.with(|d| d.set(depth));
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
            incref_type(tp);
        }
    }
}

// ---------------------------------------------------------------- per-thread type refcounts
//
// Every instance holds a reference to its heap type, so threads that allocate and free
// instances of one class hammer the type's shared refcount (an atomic on a cache line
// all cores fight over). Heap types therefore get a small id (`unique_id`) and, while
// they have one, references taken and dropped by instance creation/destruction on
// threads other than the type's owner go to a plain per-thread counter array instead
// (perf2; CPython's free-threaded build does the same). The true count is the sum of
// the object's fields and every thread's counter, minus a large bias (`TYPE_BIAS`)
// that a type with an id carries in its shared field.
//
// The bias must be large, not one reference: an instance's type reference is often
// taken through one path and dropped through the other. Extension deallocs drop it
// with a plain Py_DECREF(tp) (heap-type bases such as Cython classes built from specs),
// `__class__` assignment and C code use plain counts, while generic allocation counts
// per thread. Between merges the fields alone can therefore fall by any number of
// instances; with a one-reference bias they reached zero and the type was freed while
// instances still used it (pandas: Timestamp freed under live Timestamps). With a bias
// far above any real count the fields never reach zero, so a type with an id is only
// ever freed by the cycle collector (heap types are in a cycle through their MRO
// anyway). Counters are merged into the type at every collection (world stopped) and
// at thread exit; the collector discounts the bias, and when it finds the type
// unreachable it gives up the id and the bias.

/// Bias held in the shared field of a type with per-thread counting (in references).
pub const TYPE_BIAS: isize = 1 << 50;

struct TypeCounts {
    /// Every live thread's counter array (owner key, array).
    arrays: Vec<(usize, *mut Vec<isize>)>,
    /// id -> type (0 = free slot); ids start at 1.
    types: Vec<usize>,
    free_ids: Vec<usize>,
}
unsafe impl Send for TypeCounts {}
static TYPE_COUNTS: std::sync::Mutex<TypeCounts> =
    std::sync::Mutex::new(TypeCounts { arrays: Vec::new(), types: Vec::new(), free_ids: Vec::new() });

#[thread_local]
static mut MY_COUNTS: *mut Vec<isize> = std::ptr::null_mut();

#[inline(always)]
unsafe fn type_id(tp: *mut PyTypeObject) -> usize {
    unsafe { (*(tp as *mut crate::ffi::PyHeapTypeObject)).unique_id as usize }
}

/// Give heap type `tp` an id (enables per-thread counting). Called by type creation.
pub unsafe fn assign_type_id(tp: *mut PyTypeObject) {
    unsafe {
        if (*tp).tp_flags & Py_TPFLAGS_HEAPTYPE == 0 || type_id(tp) != 0 {
            return;
        }
        let mut g = TYPE_COUNTS.lock().unwrap_or_else(|e| e.into_inner());
        let id = match g.free_ids.pop() {
            Some(i) => i,
            None => {
                if g.types.is_empty() {
                    g.types.push(0);
                }
                g.types.push(0);
                g.types.len() - 1
            }
        };
        g.types[id] = tp as usize;
        (*tp.cast::<PyObject>()).ob_ref_shared.fetch_add(TYPE_BIAS << REF_SHARED_SHIFT, Relaxed);
        (*(tp as *mut crate::ffi::PyHeapTypeObject)).unique_id = id as isize;
    }
}

/// Fold every thread's counters into the types (world stopped, or the caller is the
/// only thread).
pub fn merge_type_counts() {
    let g = TYPE_COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    for &(_, arr) in g.arrays.iter() {
        unsafe { merge_array(&g, &mut *arr) };
    }
}

unsafe fn merge_array(g: &TypeCounts, arr: &mut Vec<isize>) {
    for (id, c) in arr.iter_mut().enumerate() {
        if *c != 0 {
            let tp = g.types.get(id).copied().unwrap_or(0);
            if tp != 0 {
                unsafe { (*(tp as *mut PyObject)).ob_ref_shared.fetch_add(*c << REF_SHARED_SHIFT, Relaxed) };
            }
            *c = 0;
        }
    }
}

/// The calling thread is exiting: fold its counters in and forget its array.
pub fn thread_exit_type_counts() {
    unsafe {
        let mine = MY_COUNTS;
        if mine.is_null() {
            return;
        }
        let mut g = TYPE_COUNTS.lock().unwrap_or_else(|e| e.into_inner());
        merge_array(&g, &mut *mine);
        g.arrays.retain(|&(_, a)| a != mine);
        MY_COUNTS = std::ptr::null_mut();
        drop(Box::from_raw(mine));
    }
}

/// Is `op` a type with per-thread counting (its fields include the bias)?
#[inline]
pub unsafe fn has_type_id(op: *mut PyObject) -> bool {
    unsafe {
        let t = (*op).type_ptr();
        ((*t).tp_flags & Py_TPFLAGS_TYPE_SUBCLASS != 0)
            && (*(op as *mut PyTypeObject)).tp_flags & Py_TPFLAGS_HEAPTYPE != 0
            && type_id(op as *mut PyTypeObject) != 0
    }
}

/// Reference count of `o` as Python and C code should see it (sys.getrefcount,
/// Py_REFCNT): without the per-thread-counting bias of a type.
pub fn visible_refcnt(o: &PyObject) -> isize {
    let n = o.refcnt();
    if !o.is_immortal() && unsafe { has_type_id(o.as_ptr()) } {
        n - TYPE_BIAS
    } else {
        n
    }
}

/// Stop per-thread counting for `tp` (collector found it unreachable, or it is being
/// deallocated): drop the bias and recycle the id. Counters must have been merged, and
/// with `drop_bias` the world must be stopped and the caller must hold a reference.
pub unsafe fn release_type_id(tp: *mut PyTypeObject, drop_bias: bool) {
    unsafe {
        let id = type_id(tp);
        if id == 0 {
            return;
        }
        {
            let mut g = TYPE_COUNTS.lock().unwrap_or_else(|e| e.into_inner());
            if g.types.get(id).copied() == Some(tp as usize) {
                g.types[id] = 0;
                g.free_ids.push(id);
            }
            (*(tp as *mut crate::ffi::PyHeapTypeObject)).unique_id = 0;
        }
        if drop_bias {
            // fold the owner's local count into the shared field first so that removing
            // the bias cannot leave a negative shared count next to a positive local one
            let o = &*(tp as *mut PyObject);
            explicit_merge(tp as *mut PyObject);
            o.ob_ref_shared.fetch_sub((TYPE_BIAS - 1) << REF_SHARED_SHIFT, AcqRel);
            decref(tp as *mut PyObject);
        }
    }
}

#[cold]
unsafe fn counts_slot(id: usize) -> *mut isize {
    unsafe {
        if MY_COUNTS.is_null() {
            let b = Box::into_raw(Box::new(Vec::<isize>::new()));
            TYPE_COUNTS.lock().unwrap_or_else(|e| e.into_inner()).arrays.push((thread_id(), b));
            MY_COUNTS = b;
        }
        let v = &mut *MY_COUNTS;
        if v.len() <= id {
            // resized while holding the registry lock: the collector may be reading it
            let _g = TYPE_COUNTS.lock().unwrap_or_else(|e| e.into_inner());
            v.resize((id + 1).next_power_of_two().max(64), 0);
        }
        v.as_mut_ptr().add(id)
    }
}

/// Reference to heap type `tp` taken on behalf of a new instance.
#[inline(always)]
pub unsafe fn incref_type(tp: *mut PyTypeObject) {
    unsafe {
        let id = type_id(tp);
        let o = &*(tp as *mut PyObject);
        if id == 0 || o.ob_tid.load(Relaxed) == thread_id() {
            return incref(tp as *mut PyObject);
        }
        let v = MY_COUNTS;
        if !v.is_null() && id < (*v).len() {
            *(*v).as_mut_ptr().add(id) += 1;
        } else {
            *counts_slot(id) += 1;
        }
    }
}

/// Drop a reference to heap type `tp` held by a dying instance.
#[inline(always)]
pub unsafe fn decref_type(tp: *mut PyTypeObject) {
    unsafe {
        let id = type_id(tp);
        let o = &*(tp as *mut PyObject);
        if id == 0 || o.ob_tid.load(Relaxed) == thread_id() {
            return decref(tp as *mut PyObject);
        }
        let v = MY_COUNTS;
        if !v.is_null() && id < (*v).len() {
            *(*v).as_mut_ptr().add(id) -= 1;
        } else {
            *counts_slot(id) -= 1;
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
