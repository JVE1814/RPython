//! Functions that the public headers no longer declare but that the stable ABI still
//! exports (old extensions link against them; test_stable_abi_ctypes looks them up).
//! Each keeps the documented behaviour of the removed API on top of the current one.

use super::*;
use crate::abstract_ as ab;
use crate::types::{dict, int, list, none, str as pystr};
use std::ffi::c_void;
use std::sync::Mutex;

/// `_Py_SwappedOp[op]`: the comparison with swapped operands (LT<->GT, LE<->GE).
#[unsafe(no_mangle)]
pub static _Py_SwappedOp: [c_int; 6] = [4, 5, 2, 3, 0, 1];

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_CheckRecursiveCall(where_: *const c_char) -> c_int {
    unsafe {
        if super::ceval::Py_EnterRecursiveCall(where_) != 0 {
            return -1;
        }
        super::ceval::Py_LeaveRecursiveCall();
        0
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyThreadState_Prealloc(interp: *mut c_void) -> *mut c_void {
    unsafe { super::pystate::PyThreadState_New(interp) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyThreadState_Init(_ts: *mut c_void) {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyState_AddModule(m: *mut PyObject, def: *mut PyModuleDef) -> c_int {
    unsafe { super::modsupport::PyState_AddModule(m, def) }
}

// ---------------------------------------------------------------- legacy GIL calls

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_ThreadsInitialized() -> c_int {
    1
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_AcquireLock() {}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_ReleaseLock() {}

// ---------------------------------------------------------------- path configuration

/// A NUL-terminated wide string that stays valid for the life of the process
/// (the old getters returned static buffers).
fn leaked_wide(s: &str) -> *mut libc::wchar_t {
    static KEEP: Mutex<Vec<usize>> = Mutex::new(Vec::new());
    let mut v: Vec<libc::wchar_t> = s.chars().map(|c| c as libc::wchar_t).collect();
    v.push(0);
    let p = Box::leak(v.into_boxed_slice()).as_mut_ptr();
    KEEP.lock().unwrap_or_else(|e| e.into_inner()).push(p as usize);
    p
}
fn sys_str(attr: &str) -> String {
    match crate::modules::sys::get_sys_attr(attr) {
        Some(v) if pystr::is_str(&v) => pystr::to_string_lossy(&v),
        _ => String::new(),
    }
}
unsafe fn wide_arg(w: *const libc::wchar_t) -> PyResult {
    unsafe { from_c(super::unicode::PyUnicode_FromWideChar(w, -1)) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_GetProgramName() -> *mut libc::wchar_t {
    leaked_wide(&sys_str("executable"))
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_GetProgramFullPath() -> *mut libc::wchar_t {
    leaked_wide(&sys_str("executable"))
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_GetPrefix() -> *mut libc::wchar_t {
    leaked_wide(&sys_str("prefix"))
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_GetExecPrefix() -> *mut libc::wchar_t {
    leaked_wide(&sys_str("exec_prefix"))
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_GetPythonHome() -> *mut libc::wchar_t {
    match std::env::var("PYTHONHOME") {
        Ok(h) if !h.is_empty() => leaked_wide(&h),
        _ => std::ptr::null_mut(),
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_GetPath() -> *mut libc::wchar_t {
    let parts: Vec<String> = match crate::modules::sys::get_sys_attr("path") {
        Some(p) if list::is_list(&p) => list::list_to_vec(&p).iter().filter(|x| pystr::is_str(x)).map(|x| pystr::to_string_lossy(x)).collect(),
        _ => Vec::new(),
    };
    leaked_wide(&parts.join(":"))
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_SetPath(path: *const libc::wchar_t) {
    unsafe { PySys_SetPath(path) }
}

// ---------------------------------------------------------------- sys helpers

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySys_SetPath(path: *const libc::wchar_t) {
    unsafe {
        let r = (|| -> PyResult<()> {
            let s = if path.is_null() { pystr::new_str("") } else { wide_arg(path)? };
            let items: Vec<PyRef> = pystr::to_string_lossy(&s).split(':').map(pystr::new_str).collect();
            crate::modules::sys::set_sys_attr("path", &list::new_list(items))
        })();
        if r.is_err() {
            clear_exc();
        }
    }
}
fn warnoptions() -> PyResult {
    match crate::modules::sys::get_sys_attr("warnoptions") {
        Some(w) if list::is_list(&w) => Ok(w),
        _ => {
            let l = list::new_list(vec![]);
            crate::modules::sys::set_sys_attr("warnoptions", &l)?;
            Ok(l)
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySys_ResetWarnOptions() {
    if let Ok(w) = warnoptions() {
        if ab::call_method_str(&w, "clear", &[]).is_err() {
            clear_exc();
        }
    } else {
        clear_exc();
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySys_HasWarnOptions() -> c_int {
    match crate::modules::sys::get_sys_attr("warnoptions") {
        Some(w) if list::is_list(&w) => (list::list_len(&w) > 0) as c_int,
        _ => 0,
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySys_AddWarnOptionUnicode(opt: *mut PyObject) {
    unsafe {
        if opt.is_null() || warnoptions().map(|w| list::append(&w, (*opt).to_ref())).is_err() {
            clear_exc();
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySys_AddWarnOption(opt: *const libc::wchar_t) {
    unsafe {
        match wide_arg(opt) {
            Ok(s) => PySys_AddWarnOptionUnicode(s.as_ptr()),
            Err(_) => clear_exc(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySys_AddXOption(opt: *const libc::wchar_t) {
    unsafe {
        let r = (|| -> PyResult<()> {
            let w = wide_arg(opt)?;
            let s = pystr::to_string_lossy(&w);
            let xo = crate::modules::sys::get_sys_attr("_xoptions").ok_or_else(|| system_error("no sys._xoptions"))?;
            match s.split_once('=') {
                Some((k, v)) => dict::set_item_str(&xo, k, &pystr::new_str(v)),
                None => dict::set_item_str(&xo, &s, &crate::types::bool_::new_bool(true)),
            }
        })();
        if r.is_err() {
            clear_exc();
        }
    }
}

// ---------------------------------------------------------------- old buffer protocol

unsafe fn old_buffer(o: *mut PyObject, writable: bool, buf: *mut *mut c_void, len: *mut Py_ssize_t) -> c_int {
    unsafe {
        if o.is_null() || buf.is_null() || len.is_null() {
            let _ = bad_internal();
            return -1;
        }
        let mut view: Py_buffer = std::mem::zeroed();
        let flags = if writable { super::buffer::PYBUF_WRITABLE } else { 0 };
        if super::buffer::PyObject_GetBuffer(o, &mut view, flags) < 0 {
            return -1;
        }
        // the old API hands out the pointer without keeping the export alive
        *buf = view.buf;
        *len = view.len;
        super::buffer::PyBuffer_Release(&mut view);
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_CheckReadBuffer(o: *mut PyObject) -> c_int {
    unsafe {
        if o.is_null() || super::buffer::PyObject_CheckBuffer(o) == 0 {
            return 0;
        }
        let mut view: Py_buffer = std::mem::zeroed();
        if super::buffer::PyObject_GetBuffer(o, &mut view, 0) < 0 {
            clear_exc();
            return 0;
        }
        super::buffer::PyBuffer_Release(&mut view);
        1
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_AsCharBuffer(o: *mut PyObject, buf: *mut *const c_char, len: *mut Py_ssize_t) -> c_int {
    unsafe { old_buffer(o, false, buf as *mut *mut c_void, len) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_AsReadBuffer(o: *mut PyObject, buf: *mut *const c_void, len: *mut Py_ssize_t) -> c_int {
    unsafe { old_buffer(o, false, buf as *mut *mut c_void, len) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_AsWriteBuffer(o: *mut PyObject, buf: *mut *mut c_void, len: *mut Py_ssize_t) -> c_int {
    unsafe { old_buffer(o, true, buf, len) }
}

// ---------------------------------------------------------------- old unicode API

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_InternImmortal(p: *mut *mut PyObject) {
    unsafe { super::unicode::PyUnicode_InternInPlace(p) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_GetSize(o: *mut PyObject) -> Py_ssize_t {
    unsafe {
        if o.is_null() || !pystr::is_str(&*o) {
            let _ = bad_internal();
            return -1;
        }
        // in wchar_t units (UCS-4 here)
        pystr::str_len(&*o) as Py_ssize_t
    }
}
unsafe fn codec_call(func: &str, o: *mut PyObject, encoding: *const c_char, errors: *const c_char, want: Option<bool>) -> *mut PyObject {
    unsafe {
        ret((|| -> PyResult {
            let o = super::nn(o)?;
            let codecs = crate::import::import_absolute("_codecs")?;
            let f = ab::getattr_str(&codecs, func)?;
            let enc = if encoding.is_null() { pystr::new_str("utf-8") } else { cstr_obj(encoding)? };
            let err = if errors.is_null() { pystr::new_str("strict") } else { cstr_obj(errors)? };
            let r = ab::call(&f, &[o, &enc, &err])?;
            // Some(true): the result must be str, Some(false): must be bytes
            match want {
                Some(true) if !pystr::is_str(&r) => Err(type_error(&format!("'{}' codec returned {} instead of str", pystr::to_string_lossy(&enc), tname(&r)))),
                Some(false) if !crate::types::bytes::is_bytes(&r) => Err(type_error(&format!("'{}' codec returned {} instead of bytes", pystr::to_string_lossy(&enc), tname(&r)))),
                _ => Ok(r),
            }
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsEncodedObject(o: *mut PyObject, encoding: *const c_char, errors: *const c_char) -> *mut PyObject {
    unsafe { codec_call("encode", o, encoding, errors, None) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsEncodedUnicode(o: *mut PyObject, encoding: *const c_char, errors: *const c_char) -> *mut PyObject {
    unsafe { codec_call("encode", o, encoding, errors, Some(true)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsDecodedObject(o: *mut PyObject, encoding: *const c_char, errors: *const c_char) -> *mut PyObject {
    unsafe { codec_call("decode", o, encoding, errors, None) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsDecodedUnicode(o: *mut PyObject, encoding: *const c_char, errors: *const c_char) -> *mut PyObject {
    unsafe { codec_call("decode", o, encoding, errors, Some(true)) }
}

#[allow(dead_code)]
fn _unused() -> (PyRef, PyRef) {
    (none::none_ref(), int::new_int(0))
}
