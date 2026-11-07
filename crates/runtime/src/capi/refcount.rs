//! refcount.h / object.h basics: reference counting entry points used by the inline
//! free-threaded Py_INCREF/Py_DECREF and by the stable ABI.

use super::*;
use std::sync::atomic::Ordering::*;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_IncRef(o: *mut PyObject) {
    unsafe { xincref(o) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_DecRef(o: *mut PyObject) {
    unsafe { xdecref(o) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_IncRef(o: *mut PyObject) {
    unsafe { incref(o) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_DecRef(o: *mut PyObject) {
    unsafe { decref(o) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_Dealloc(o: *mut PyObject) {
    unsafe { dealloc(o) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_DecRefShared(o: *mut PyObject) {
    unsafe { decref_shared(o) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_DecRefSharedDebug(o: *mut PyObject, _file: *const c_char, _line: c_int) {
    unsafe { decref_shared(o) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_MergeZeroLocalRefcount(o: *mut PyObject) {
    unsafe { merge_zero_local_refcount(o) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_NegativeRefcount(file: *const c_char, line: c_int, o: *mut PyObject) {
    unsafe {
        let msg = format!("{}:{}: object at {:p} has negative ref count", cs(file), line, o);
        fatal(&msg);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_INCREF_IncRefTotal() {}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_DECREF_DecRefTotal() {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_REFCNT(o: *mut PyObject) -> Py_ssize_t {
    unsafe {
        let local = (*o).ob_ref_local.load(Relaxed);
        if local == IMMORTAL_REFCNT_LOCAL {
            return (3isize) << 30;
        }
        crate::object::visible_refcnt(&*o)
    }
}

/// _Py_SetRefcnt: mirrors the header's Py_SET_REFCNT for the free-threaded layout.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_SetRefcnt(o: *mut PyObject, n: Py_ssize_t) {
    unsafe {
        let ob = &*o;
        if ob.is_immortal() {
            return;
        }
        if n >= u32::MAX as isize {
            make_immortal(o);
            return;
        }
        if ob.ob_tid.load(Relaxed) == thread_id() {
            ob.ob_ref_local.store(n as u32, Relaxed);
        } else {
            ob.ob_tid.store(0, Relaxed);
            ob.ob_ref_local.store(0, Relaxed);
            ob.ob_ref_shared.store((n << REF_SHARED_SHIFT) | REF_MERGED, Relaxed);
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_NewReference(o: *mut PyObject) {
    unsafe {
        let ob = &*o;
        ob.ob_tid.store(thread_id(), Relaxed);
        ob.ob_ref_local.store(1, Relaxed);
        ob.ob_ref_shared.store(0, Relaxed);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_NewReferenceNoTotal(o: *mut PyObject) {
    unsafe { _Py_NewReference(o) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_ResurrectReference(o: *mut PyObject) {
    unsafe { _Py_NewReference(o) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_ForgetReference(_o: *mut PyObject) {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_NewRef(o: *mut PyObject) -> *mut PyObject {
    unsafe { incref(o) };
    o
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_XNewRef(o: *mut PyObject) -> *mut PyObject {
    unsafe { xincref(o) };
    o
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_Is(x: *mut PyObject, y: *mut PyObject) -> c_int {
    (x == y) as c_int
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_IsNone(x: *mut PyObject) -> c_int {
    (x == crate::types::none::none().as_ptr()) as c_int
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_IsTrue(x: *mut PyObject) -> c_int {
    (x == crate::types::bool_::true_obj().as_ptr()) as c_int
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_IsFalse(x: *mut PyObject) -> c_int {
    (x == crate::types::bool_::false_obj().as_ptr()) as c_int
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_TYPE(o: *mut PyObject) -> *mut PyTypeObject {
    unsafe { (*o).type_ptr() }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_SIZE(o: *mut PyObject) -> Py_ssize_t {
    unsafe { (*(o as *mut PyVarObject)).ob_size }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_IS_TYPE(o: *mut PyObject, t: *mut PyTypeObject) -> c_int {
    unsafe { ((*o).type_ptr() == t) as c_int }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_SET_SIZE(o: *mut PyVarObject, n: Py_ssize_t) {
    unsafe { (*o).ob_size = n }
}

/// Abort the process with a message (Py_FatalError semantics).
pub fn fatal(msg: &str) -> ! {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = writeln!(std::io::stderr(), "Fatal Python error: {}", msg);
    let _ = std::io::stderr().flush();
    std::process::abort();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_FatalError(msg: *const c_char) -> ! {
    unsafe { fatal(&cs(msg)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_FatalErrorFunc(func: *const c_char, msg: *const c_char) -> ! {
    unsafe { fatal(&format!("{}: {}", cs(func), cs(msg))) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_AssertFailed(
    obj: *mut PyObject,
    expr: *const c_char,
    msg: *const c_char,
    file: *const c_char,
    line: c_int,
    function: *const c_char,
) -> ! {
    unsafe {
        let _ = obj;
        fatal(&format!(
            "{}:{}: {}: Assertion \"{}\" failed: {}",
            cs(file),
            line,
            cs(function),
            if expr.is_null() { "".into() } else { cs(expr) },
            if msg.is_null() { "".into() } else { cs(msg) }
        ))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_BreakPoint() {}
