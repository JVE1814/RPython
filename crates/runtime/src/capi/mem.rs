//! pymem.h / objimpl.h: raw, "mem" and "object" allocator domains (with the
//! PyMem_SetAllocator hook table), object initialisation and GC object allocation.

use super::*;
use crate::alloc;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering::*};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PyMemAllocatorEx {
    pub ctx: *mut c_void,
    pub malloc: Option<unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void>,
    pub calloc: Option<unsafe extern "C" fn(*mut c_void, usize, usize) -> *mut c_void>,
    pub realloc: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, usize) -> *mut c_void>,
    pub free: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
}

#[repr(C)]
pub struct PyObjectArenaAllocator {
    pub ctx: *mut c_void,
    pub alloc: Option<unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void>,
    pub free: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, usize)>,
}

// ---------------------------------------------------------------- default allocators

unsafe extern "C" fn raw_malloc(_: *mut c_void, n: usize) -> *mut c_void {
    unsafe { libc::malloc(n.max(1)) }
}
unsafe extern "C" fn raw_calloc(_: *mut c_void, n: usize, s: usize) -> *mut c_void {
    unsafe {
        if n == 0 || s == 0 {
            return libc::calloc(1, 1);
        }
        libc::calloc(n, s)
    }
}
unsafe extern "C" fn raw_realloc(_: *mut c_void, p: *mut c_void, n: usize) -> *mut c_void {
    unsafe { libc::realloc(p, n.max(1)) }
}
unsafe extern "C" fn raw_free(_: *mut c_void, p: *mut c_void) {
    unsafe { libc::free(p) }
}

/// Object domain: small blocks come from the tlab allocator (like pymalloc), larger
/// ones from the system allocator. `tlab::owns` tells them apart on free/realloc, so
/// PyObject_Free also accepts objects allocated by PyType_GenericAlloc.
unsafe extern "C" fn obj_malloc(_: *mut c_void, n: usize) -> *mut c_void {
    unsafe {
        if n <= crate::tlab::MAX_SMALL {
            let p = crate::tlab::alloc_small(n.max(1));
            if !p.is_null() {
                return p as *mut c_void;
            }
        }
        libc::malloc(n.max(1))
    }
}
unsafe extern "C" fn obj_calloc(c: *mut c_void, n: usize, s: usize) -> *mut c_void {
    unsafe {
        let total = match n.checked_mul(s) {
            Some(t) => t,
            None => return std::ptr::null_mut(),
        };
        if total <= crate::tlab::MAX_SMALL {
            return obj_malloc(c, total); // tlab blocks are zeroed
        }
        libc::calloc(total, 1)
    }
}
unsafe extern "C" fn obj_realloc(c: *mut c_void, p: *mut c_void, n: usize) -> *mut c_void {
    unsafe {
        if p.is_null() {
            return obj_malloc(c, n);
        }
        if crate::tlab::owns(p as *const u8) {
            let old = crate::tlab::small_size_of(p as *mut u8);
            if n <= old && n * 2 >= old.min(64) {
                return p;
            }
            let np = obj_malloc(c, n);
            if np.is_null() {
                return np;
            }
            std::ptr::copy_nonoverlapping(p as *const u8, np as *mut u8, old.min(n));
            crate::tlab::free_small(p as *mut u8);
            return np;
        }
        libc::realloc(p, n.max(1))
    }
}
unsafe extern "C" fn obj_free(_: *mut c_void, p: *mut c_void) {
    unsafe {
        if p.is_null() {
            return;
        }
        if crate::tlab::owns(p as *const u8) {
            if crate::tlab::is_gc_block(p as *mut u8) {
                // a GC object released with PyObject_Free instead of PyObject_GC_Del
                alloc::free_object(p as *mut PyObject);
            } else {
                crate::tlab::free_small(p as *mut u8);
            }
        } else {
            libc::free(p);
        }
    }
}

const DEFAULT_RAW: PyMemAllocatorEx = PyMemAllocatorEx {
    ctx: std::ptr::null_mut(),
    malloc: Some(raw_malloc),
    calloc: Some(raw_calloc),
    realloc: Some(raw_realloc),
    free: Some(raw_free),
};
const DEFAULT_OBJ: PyMemAllocatorEx = PyMemAllocatorEx {
    ctx: std::ptr::null_mut(),
    malloc: Some(obj_malloc),
    calloc: Some(obj_calloc),
    realloc: Some(obj_realloc),
    free: Some(obj_free),
};

/// [raw, mem, obj]
static mut ALLOCATORS: [PyMemAllocatorEx; 3] = [DEFAULT_RAW, DEFAULT_RAW, DEFAULT_OBJ];
static HOOKED: AtomicBool = AtomicBool::new(false);

#[inline(always)]
unsafe fn dom(d: usize) -> &'static PyMemAllocatorEx {
    unsafe { &(*(&raw const ALLOCATORS))[d] }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMem_GetAllocator(domain: c_int, a: *mut PyMemAllocatorEx) {
    unsafe {
        if (0..3).contains(&domain) {
            *a = *dom(domain as usize);
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMem_SetAllocator(domain: c_int, a: *mut PyMemAllocatorEx) {
    unsafe {
        if (0..3).contains(&domain) {
            (*(&raw mut ALLOCATORS))[domain as usize] = *a;
            HOOKED.store(true, Release);
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMem_SetupDebugHooks() {}

/// Runtime-internal buffers (bytes payloads and the like) are not carved out of
/// the hookable allocator domains. While a C extension has replaced the object
/// allocator (e.g. a failure-injecting test hook), those allocations ask the
/// installed hook first, so that a hook refusing memory turns into MemoryError.
/// Returns true when the hook refused `n` bytes.
pub fn hooked_alloc_refuses(n: usize) -> bool {
    if !HOOKED.load(Acquire) {
        return false;
    }
    unsafe {
        let a = *dom(2);
        let default_malloc: unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void = obj_malloc;
        let (Some(m), Some(f)) = (a.malloc, a.free) else { return false };
        if m as usize == default_malloc as usize {
            return false;
        }
        let p = m(a.ctx, n.max(1));
        if p.is_null() {
            return true;
        }
        f(a.ctx, p);
        false
    }
}

macro_rules! domain_fns {
    ($d:expr, $m:ident, $c:ident, $r:ident, $f:ident) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $m(n: usize) -> *mut c_void {
            if n > isize::MAX as usize {
                return std::ptr::null_mut();
            }
            unsafe {
                let a = dom($d);
                let p = a.malloc.unwrap()(a.ctx, n);
                if crate::modules::_tracemalloc::TRACING.load(Relaxed) {
                    crate::modules::_tracemalloc::track(p as *const u8, n);
                }
                p
            }
        }
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $c(n: usize, s: usize) -> *mut c_void {
            if s != 0 && n > isize::MAX as usize / s {
                return std::ptr::null_mut();
            }
            unsafe {
                let a = dom($d);
                let p = a.calloc.unwrap()(a.ctx, n, s);
                if crate::modules::_tracemalloc::TRACING.load(Relaxed) {
                    crate::modules::_tracemalloc::track(p as *const u8, n * s);
                }
                p
            }
        }
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $r(p: *mut c_void, n: usize) -> *mut c_void {
            if n > isize::MAX as usize {
                return std::ptr::null_mut();
            }
            unsafe {
                let a = dom($d);
                let np = a.realloc.unwrap()(a.ctx, p, n);
                if crate::modules::_tracemalloc::TRACING.load(Relaxed) {
                    crate::modules::_tracemalloc::retrack(p as *const u8, np as *const u8, n);
                }
                np
            }
        }
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $f(p: *mut c_void) {
            unsafe {
                if crate::modules::_tracemalloc::TRACING.load(Relaxed) {
                    crate::modules::_tracemalloc::untrack(p as *const u8);
                }
                let a = dom($d);
                a.free.unwrap()(a.ctx, p)
            }
        }
    };
}
domain_fns!(0, PyMem_RawMalloc, PyMem_RawCalloc, PyMem_RawRealloc, PyMem_RawFree);
domain_fns!(1, PyMem_Malloc, PyMem_Calloc, PyMem_Realloc, PyMem_Free);
domain_fns!(2, PyObject_Malloc, PyObject_Calloc, PyObject_Realloc, PyObject_Free);

static mut ARENA: PyObjectArenaAllocator = PyObjectArenaAllocator { ctx: std::ptr::null_mut(), alloc: Some(arena_alloc), free: Some(arena_free) };
unsafe extern "C" fn arena_alloc(_: *mut c_void, n: usize) -> *mut c_void {
    unsafe {
        let p = libc::mmap(std::ptr::null_mut(), n, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0);
        if p == libc::MAP_FAILED { std::ptr::null_mut() } else { p }
    }
}
unsafe extern "C" fn arena_free(_: *mut c_void, p: *mut c_void, n: usize) {
    unsafe {
        libc::munmap(p, n);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GetArenaAllocator(a: *mut PyObjectArenaAllocator) {
    unsafe {
        (*a).ctx = (*(&raw const ARENA)).ctx;
        (*a).alloc = (*(&raw const ARENA)).alloc;
        (*a).free = (*(&raw const ARENA)).free;
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_SetArenaAllocator(a: *mut PyObjectArenaAllocator) {
    unsafe {
        let d = &mut *(&raw mut ARENA);
        d.ctx = (*a).ctx;
        d.alloc = (*a).alloc;
        d.free = (*a).free;
    }
}

// PyTraceMalloc_Track/Untrack live in modules/_tracemalloc.rs.

// ---------------------------------------------------------------- object init

/// Mark memory obtained from PyObject_Malloc so that the runtime's `free_object`
/// returns it to the right allocator.
#[inline]
unsafe fn fix_alloc_bits(op: *mut PyObject) {
    unsafe {
        let large = if crate::tlab::owns(op as *const u8) { 0 } else { alloc::GC_BIT_LARGE };
        (*op).ob_gc_bits.store(large, Relaxed);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Init(op: *mut PyObject, tp: *mut PyTypeObject) -> *mut PyObject {
    unsafe {
        if op.is_null() {
            let _ = memory_error();
            return op;
        }
        init_header(op, tp);
        fix_alloc_bits(op);
        op
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_InitVar(op: *mut PyVarObject, tp: *mut PyTypeObject, n: Py_ssize_t) -> *mut PyVarObject {
    unsafe {
        if op.is_null() {
            let _ = memory_error();
            return op;
        }
        PyObject_Init(op as *mut PyObject, tp);
        (*op).ob_size = n;
        op
    }
}

#[inline]
fn var_size(tp: *mut PyTypeObject, n: Py_ssize_t) -> usize {
    unsafe {
        let s = (*tp).tp_basicsize as usize + (n.max(0) as usize) * (*tp).tp_itemsize as usize;
        (s + 7) & !7
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_New(tp: *mut PyTypeObject) -> *mut PyObject {
    unsafe {
        let op = alloc::alloc_object(tp, var_size(tp, 0));
        if op.is_null() {
            let _ = memory_error();
        }
        op
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_NewVar(tp: *mut PyTypeObject, n: Py_ssize_t) -> *mut PyVarObject {
    unsafe {
        let op = alloc::alloc_object(tp, var_size(tp, n));
        if op.is_null() {
            let _ = memory_error();
            return op as *mut PyVarObject;
        }
        set_var_size(op, n);
        if (*tp).tp_flags & Py_TPFLAGS_TUPLE_SUBCLASS != 0 {
            // (jit6) tuples of every type cache their hash (types/tuple.rs `tuple_hash`)
            (*(op as *mut PyTupleObject)).ob_hash = -1;
        }
        op as *mut PyVarObject
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_GC_New(tp: *mut PyTypeObject) -> *mut PyObject {
    unsafe { _PyObject_New(tp) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_GC_NewVar(tp: *mut PyTypeObject, n: Py_ssize_t) -> *mut PyVarObject {
    unsafe {
        if n < 0 {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        _PyObject_NewVar(tp, n)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_Object_GC_NewWithExtraData(tp: *mut PyTypeObject, extra: usize) -> *mut PyObject {
    unsafe {
        let op = alloc::alloc_object(tp, var_size(tp, 0) + ((extra + 7) & !7));
        if op.is_null() {
            let _ = memory_error();
        }
        op
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_GC_Resize(op: *mut PyVarObject, n: Py_ssize_t) -> *mut PyVarObject {
    unsafe {
        let tp = (*(op as *mut PyObject)).type_ptr();
        let np = alloc::realloc_object(op as *mut PyObject, var_size(tp, n));
        if np.is_null() {
            let _ = memory_error();
            return std::ptr::null_mut();
        }
        set_var_size(np, n);
        np as *mut PyVarObject
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GC_Del(op: *mut c_void) {
    unsafe {
        if !op.is_null() {
            alloc::free_object(op as *mut PyObject)
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GC_Track(op: *mut c_void) {
    unsafe { crate::gc::track(op as *mut PyObject) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GC_UnTrack(op: *mut c_void) {
    unsafe { crate::gc::untrack(op as *mut PyObject) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GC_IsTracked(op: *mut PyObject) -> c_int {
    unsafe { (alloc::type_is_gc((*op).type_ptr()) && crate::gc::is_tracked(&*op)) as c_int }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GC_IsFinalized(op: *mut PyObject) -> c_int {
    unsafe { (alloc::type_is_gc((*op).type_ptr()) && crate::gc::is_finalized(op)) as c_int }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_IS_GC(op: *mut PyObject) -> c_int {
    unsafe {
        let tp = (*op).type_ptr();
        if !alloc::type_is_gc(tp) {
            return 0;
        }
        match (*tp).tp_is_gc {
            Some(f) => f(op),
            None => 1,
        }
    }
}

// ---------------------------------------------------------------- gc module API

static GC_ENABLED: AtomicBool = AtomicBool::new(true);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyGC_Enable() -> c_int {
    GC_ENABLED.swap(true, AcqRel) as c_int
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyGC_Disable() -> c_int {
    GC_ENABLED.swap(false, AcqRel) as c_int
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyGC_IsEnabled() -> c_int {
    GC_ENABLED.load(Acquire) as c_int
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyGC_Collect() -> Py_ssize_t {
    crate::gc::collect() as Py_ssize_t
}

pub type gcvisitobjects_t = Option<unsafe extern "C" fn(*mut PyObject, *mut c_void) -> c_int>;
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_GC_VisitObjects(cb: gcvisitobjects_t, arg: *mut c_void) {
    // visits every object tracked by the collector (a snapshot; references held
    // while the callback runs); a zero result stops the walk
    let Some(cb) = cb else { return };
    for o in crate::gc::all_tracked() {
        if unsafe { cb(o.as_ptr(), arg) } == 0 {
            break;
        }
    }
}
