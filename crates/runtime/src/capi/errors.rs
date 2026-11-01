//! pyerrors.h: setting, fetching and normalising exceptions, warnings, unraisable
//! exceptions, exception object accessors.

use super::*;
use crate::abstract_ as ab;
use crate::types::exceptions as exc;
use crate::types::typeobject::{self, is_type, type_ref};
use crate::types::{dict, int, none, str as pystr, tuple};
use std::ffi::VaList;

#[inline]
fn is_exc_class(o: &PyObject) -> bool {
    exc::is_exception_class(o)
}

/// Exception currently being handled (sys.exception()), borrowed.
pub(crate) fn handled() -> *mut PyObject {
    crate::interp::eval::handled_exception(crate::threads::current())
}

/// Attach the handled exception as __context__ of `e` (breaking cycles), like raise.
pub(crate) fn chain_context(e: &PyObject) {
    let h = handled();
    if h.is_null() || h == e.as_ptr() || exc::exc_context(e).is_some() {
        return;
    }
    unsafe {
        let mut o = h;
        let mut steps = 0;
        while let Some(c) = exc::exc_context(&*o) {
            if c.as_ptr() == e.as_ptr() {
                exc::set_context_and_cause(&*o, None);
                break;
            }
            o = c.as_ptr();
            steps += 1;
            if steps > 10000 {
                break;
            }
        }
        exc::set_context_and_cause(e, Some(&*h));
    }
}

/// Create an exception instance of `tp` from `value` with PyErr_SetObject rules.
pub(crate) fn instantiate(tp: &PyObject, value: Option<&PyObject>) -> PyResult {
    if let Some(v) = value {
        if exc::is_exception_instance(v) && typeobject::is_subtype(v.type_ptr(), typeobject::as_type(tp)) {
            return Ok(v.to_ref());
        }
    }
    match value {
        None => ab::call(tp, &[]),
        Some(v) if none::is_none(v) => ab::call(tp, &[]),
        Some(v) if tuple::is_tuple(v) => {
            let items = tuple::tuple_items(v);
            ab::call(tp, items)
        }
        Some(v) => ab::call(tp, &[v]),
    }
}

/// PyErr_SetObject in Rust form.
pub(crate) fn set_object(tp: *mut PyObject, value: *mut PyObject) {
    unsafe {
        if tp.is_null() || !is_exc_class(&*tp) {
            let r = if tp.is_null() { "NULL".to_string() } else { ab::repr_string(&*tp).unwrap_or_default() };
            let _ = system_error(&format!("_PyErr_SetObject: exception {} is not a BaseException subclass", r));
            return;
        }
        match instantiate(&*tp, opt(value)) {
            Ok(e) => {
                chain_context(&e);
                set_exc(e);
            }
            Err(_) => {}
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SetObject(tp: *mut PyObject, value: *mut PyObject) {
    set_object(tp, value)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SetNone(tp: *mut PyObject) {
    set_object(tp, std::ptr::null_mut())
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SetString(tp: *mut PyObject, msg: *const c_char) {
    unsafe {
        match cstr_obj(msg) {
            Ok(s) => set_object(tp, s.as_ptr()),
            Err(_) => {}
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_Occurred() -> *mut PyObject {
    let e = current_exc_ptr();
    if e.is_null() { e } else { unsafe { (*e).type_ptr() as *mut PyObject } }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_Clear() {
    clear_exc()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_GetRaisedException() -> *mut PyObject {
    ret_opt(take_exc())
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SetRaisedException(e: *mut PyObject) {
    unsafe {
        match PyRef::from_raw_opt(e) {
            Some(e) => set_exc(e),
            None => clear_exc(),
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_Fetch(t: *mut *mut PyObject, v: *mut *mut PyObject, tb: *mut *mut PyObject) {
    unsafe {
        match take_exc() {
            None => {
                *t = std::ptr::null_mut();
                *v = std::ptr::null_mut();
                *tb = std::ptr::null_mut();
            }
            Some(e) => {
                *t = type_ref(e.type_ptr()).into_raw();
                *tb = exc::exc_traceback(&e).map_or(std::ptr::null_mut(), |x| x.into_raw());
                *v = e.into_raw();
            }
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_Restore(t: *mut PyObject, v: *mut PyObject, tb: *mut PyObject) {
    unsafe {
        let tr = PyRef::from_raw_opt(t);
        let vr = PyRef::from_raw_opt(v);
        let tbr = PyRef::from_raw_opt(tb);
        let Some(tr) = tr else {
            clear_exc();
            return;
        };
        let e = match vr {
            Some(v) if exc::is_exception_instance(&v) && typeobject::is_subtype(v.type_ptr(), typeobject::as_type(&tr)) => v,
            other => {
                if !is_exc_class(&tr) {
                    let _ = system_error("PyErr_Restore: exception is not a BaseException subclass");
                    return;
                }
                // unnormalized: normalise now (we store only instances)
                let saved = take_exc();
                match instantiate(&tr, other.as_deref()) {
                    Ok(e) => e,
                    Err(_) => {
                        drop(saved);
                        return;
                    }
                }
            }
        };
        if let Some(tb) = tbr {
            if !none::is_none(&tb) {
                exc::set_exc_traceback(&e, Some(&tb));
            }
        }
        set_exc(e);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_NormalizeException(t: *mut *mut PyObject, v: *mut *mut PyObject, tb: *mut *mut PyObject) {
    unsafe {
        if (*t).is_null() {
            return;
        }
        let tp = &**t;
        let val = *v;
        if !val.is_null() && exc::is_exception_instance(&*val) && is_type(tp)
            && typeobject::is_subtype((*val).type_ptr(), typeobject::as_type(tp))
        {
            return;
        }
        if !is_exc_class(tp) {
            return;
        }
        let saved = take_exc();
        match instantiate(tp, opt(val)) {
            Ok(e) => {
                if let Some(s) = saved {
                    set_exc(s);
                }
                xdecref(val);
                *v = std::ptr::null_mut();
                let new_t = type_ref(e.type_ptr()).into_raw();
                decref(*t);
                *t = new_t;
                *v = e.into_raw();
            }
            Err(_) => {
                // replace with the error raised while normalising
                let ne = take_exc();
                if let Some(s) = saved {
                    drop(s);
                }
                if let Some(ne) = ne {
                    xdecref(*v);
                    decref(*t);
                    *t = type_ref(ne.type_ptr()).into_raw();
                    if !tb.is_null() && (*tb).is_null() {
                        *tb = exc::exc_traceback(&ne).map_or(std::ptr::null_mut(), |x| x.into_raw());
                    }
                    *v = ne.into_raw();
                }
            }
        }
    }
}

pub(crate) fn given_matches(given: &PyObject, exc_: &PyObject) -> bool {
    if tuple::is_tuple(exc_) {
        return tuple::tuple_items(exc_).iter().any(|e| given_matches(given, e));
    }
    let g = if exc::is_exception_instance(given) { typeobject::type_obj(given.type_ptr()) } else { given };
    if is_exc_class(g) && is_exc_class(exc_) {
        return typeobject::is_subtype(typeobject::as_type(g), typeobject::as_type(exc_));
    }
    g.is(exc_)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_GivenExceptionMatches(given: *mut PyObject, e: *mut PyObject) -> c_int {
    unsafe {
        if given.is_null() || e.is_null() {
            return 0;
        }
        given_matches(&*given, &*e) as c_int
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_ExceptionMatches(e: *mut PyObject) -> c_int {
    unsafe { PyErr_GivenExceptionMatches(PyErr_Occurred(), e) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_NoMemory() -> *mut PyObject {
    let _ = memory_error();
    std::ptr::null_mut()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_BadArgument() -> c_int {
    let _ = type_error("bad argument type for built-in operation");
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_BadInternalCall() {
    let _ = bad_internal();
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyErr_BadInternalCall(file: *const c_char, line: c_int) {
    unsafe {
        let _ = system_error(&format!("{}:{}: bad argument to internal function", cs(file), line));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_Format(tp: *mut PyObject, fmt: *const c_char, mut args: ...) -> *mut PyObject {
    unsafe { PyErr_FormatV(tp, fmt, args) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_FormatV(tp: *mut PyObject, fmt: *const c_char, mut ap: VaList) -> *mut PyObject {
    unsafe {
        // clear the current exception first (CPython semantics: the format may call code)
        clear_exc();
        if let Ok(msg) = super::printf::format_v(fmt, &mut ap) {
            set_object(tp, msg.as_ptr());
        }
        std::ptr::null_mut()
    }
}

// ---------------------------------------------------------------- errno

unsafe fn set_from_errno_impl(tp: *mut PyObject, f1: *mut PyObject, f2: *mut PyObject) -> *mut PyObject {
    unsafe {
        let errno = *libc::__errno_location();
        if errno == libc::EINTR && crate::capi::misc::check_signals().is_err() {
            return std::ptr::null_mut();
        }
        let msg = if errno == 0 {
            "Error".to_string()
        } else {
            CStr::from_ptr(libc::strerror(errno)).to_string_lossy().into_owned()
        };
        let mut args: Vec<PyRef> = vec![int::new_int(errno as i64), pystr::new_str(&msg)];
        if !f1.is_null() {
            args.push((*f1).to_ref());
            if !f2.is_null() {
                args.push(none::none_ref()); // winerror
                args.push((*f2).to_ref());
            }
        }
        let t = tuple::new_tuple(args);
        set_object(tp, t.as_ptr());
        std::ptr::null_mut()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SetFromErrno(tp: *mut PyObject) -> *mut PyObject {
    unsafe { set_from_errno_impl(tp, std::ptr::null_mut(), std::ptr::null_mut()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SetFromErrnoWithFilenameObject(tp: *mut PyObject, f: *mut PyObject) -> *mut PyObject {
    unsafe { set_from_errno_impl(tp, f, std::ptr::null_mut()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SetFromErrnoWithFilenameObjects(tp: *mut PyObject, f: *mut PyObject, f2: *mut PyObject) -> *mut PyObject {
    unsafe { set_from_errno_impl(tp, f, f2) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SetFromErrnoWithFilename(tp: *mut PyObject, f: *const c_char) -> *mut PyObject {
    unsafe {
        let saved = *libc::__errno_location();
        if f.is_null() {
            return set_from_errno_impl(tp, std::ptr::null_mut(), std::ptr::null_mut());
        }
        let name = match crate::capi::unicode::decode_fs(CStr::from_ptr(f).to_bytes()) {
            Ok(n) => n,
            Err(_) => return std::ptr::null_mut(),
        };
        *libc::__errno_location() = saved;
        set_from_errno_impl(tp, name.as_ptr(), std::ptr::null_mut())
    }
}

// ---------------------------------------------------------------- ImportError

unsafe fn set_import_error_impl(tp: *mut PyObject, msg: *mut PyObject, name: *mut PyObject, path: *mut PyObject) -> *mut PyObject {
    unsafe {
        if tp.is_null() || !is_type(&*tp) || !typeobject::is_subtype(tp as *mut PyTypeObject, exc::ImportError()) {
            let _ = type_error("expected a subclass of ImportError");
            return std::ptr::null_mut();
        }
        if msg.is_null() {
            let _ = type_error("expected a message argument");
            return std::ptr::null_mut();
        }
        let r = (|| -> PyResult<PyRef> {
            let kw = dict::new_dict();
            dict::set_item_str(&kw, "name", opt(name).unwrap_or(none::none()))?;
            dict::set_item_str(&kw, "path", opt(path).unwrap_or(none::none()))?;
            ab::call_with_kwargs_dict(&*tp, &[&*msg], Some(&kw))
        })();
        if let Ok(e) = r {
            set_exc(e);
        }
        std::ptr::null_mut()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SetImportError(msg: *mut PyObject, name: *mut PyObject, path: *mut PyObject) -> *mut PyObject {
    unsafe { set_import_error_impl(exc::ImportError() as *mut PyObject, msg, name, path) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SetImportErrorSubclass(tp: *mut PyObject, msg: *mut PyObject, name: *mut PyObject, path: *mut PyObject) -> *mut PyObject {
    unsafe { set_import_error_impl(tp, msg, name, path) }
}

// ---------------------------------------------------------------- new exception classes

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_NewException(name: *const c_char, base: *mut PyObject, d: *mut PyObject) -> *mut PyObject {
    unsafe { PyErr_NewExceptionWithDoc(name, std::ptr::null(), base, d) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_NewExceptionWithDoc(name: *const c_char, doc: *const c_char, base: *mut PyObject, d: *mut PyObject) -> *mut PyObject {
    unsafe {
        ret((|| -> PyResult {
            let full = cs(name).into_owned();
            let Some(dot) = full.rfind('.') else {
                return Err(system_error("PyErr_NewException: name must be module.class"));
            };
            let (module, cls) = (&full[..dot], &full[dot + 1..]);
            let ns = match opt(d) {
                Some(d) => d.to_ref(),
                None => dict::new_dict(),
            };
            if dict::get_item_rs(&ns, "__module__").is_none() {
                dict::set_item_str(&ns, "__module__", &pystr::new_str(module))?;
            }
            if !doc.is_null() {
                dict::set_item_str(&ns, "__doc__", &*(cstr_obj(doc)?))?;
            }
            let bases = match opt(base) {
                None => tuple::new_tuple(vec![type_ref(exc::Exception())]),
                Some(b) if tuple::is_tuple(b) => b.to_ref(),
                Some(b) => tuple::new_tuple(vec![b.to_ref()]),
            };
            let tt = typeobject::type_obj(typeobject::type_type());
            ab::call(tt, &[&pystr::new_str(cls), &bases, &ns])
        })())
    }
}

// ---------------------------------------------------------------- handled exception

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_GetHandledException() -> *mut PyObject {
    let h = handled();
    if h.is_null() {
        return h;
    }
    unsafe { (*h).to_ref().into_raw() }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SetHandledException(e: *mut PyObject) {
    unsafe {
        let ts = crate::threads::current();
        let item = ts.c.exc_info;
        if item.is_null() {
            return;
        }
        let old = (*item).exc_value;
        (*item).exc_value = if e.is_null() || none::is_none(&*e) { std::ptr::null_mut() } else { (*e).to_ref().into_raw() };
        xdecref(old);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_GetExcInfo(t: *mut *mut PyObject, v: *mut *mut PyObject, tb: *mut *mut PyObject) {
    unsafe {
        let h = handled();
        if h.is_null() {
            *t = std::ptr::null_mut();
            *v = std::ptr::null_mut();
            *tb = std::ptr::null_mut();
            return;
        }
        *t = type_ref((*h).type_ptr()).into_raw();
        *v = (*h).to_ref().into_raw();
        *tb = exc::exc_traceback(&*h).map_or(std::ptr::null_mut(), |x| x.into_raw());
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SetExcInfo(t: *mut PyObject, v: *mut PyObject, tb: *mut PyObject) {
    unsafe {
        PyErr_SetHandledException(v);
        xdecref(t);
        xdecref(v);
        xdecref(tb);
    }
}

// ---------------------------------------------------------------- exception accessors

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyException_GetTraceback(e: *mut PyObject) -> *mut PyObject {
    unsafe { ret_opt(exc::exc_traceback(&*e)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyException_SetTraceback(e: *mut PyObject, tb: *mut PyObject) -> c_int {
    unsafe { ret_i(ab::setattr(&*e, crate::name!("__traceback__"), opt(tb).unwrap_or(none::none()))) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyException_GetCause(e: *mut PyObject) -> *mut PyObject {
    unsafe {
        let c = exc::exc(&*e).cause;
        if c.is_null() { c } else { (*c).to_ref().into_raw() }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyException_SetCause(e: *mut PyObject, c: *mut PyObject) {
    unsafe {
        exc::set_exc_cause(&*e, opt(c));
        xdecref(c);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyException_GetContext(e: *mut PyObject) -> *mut PyObject {
    unsafe { ret_opt(exc::exc_context(&*e)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyException_SetContext(e: *mut PyObject, c: *mut PyObject) {
    unsafe {
        exc::set_context_and_cause(&*e, opt(c));
        xdecref(c);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyException_GetArgs(e: *mut PyObject) -> *mut PyObject {
    unsafe { exc::exc_args(&*e).into_raw() }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyException_SetArgs(e: *mut PyObject, a: *mut PyObject) {
    unsafe {
        let _ = ab::setattr(&*e, crate::name!("args"), &*a);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyExceptionClass_Name(t: *mut PyObject) -> *const c_char {
    unsafe { (*(t as *mut PyTypeObject)).tp_name }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyErr_ChainExceptions1(e: *mut PyObject) {
    unsafe {
        let Some(e) = PyRef::from_raw_opt(e) else { return };
        match take_exc() {
            Some(cur) => {
                if cur.as_ptr() != e.as_ptr() {
                    exc::set_context_and_cause(&cur, Some(&e));
                }
                set_exc(cur);
            }
            None => set_exc(e),
        }
    }
}

// ---------------------------------------------------------------- UnicodeError accessors

fn uerr(o: *mut PyObject) -> &'static mut exc::UnicodeErrorExc {
    unsafe { &mut *(o as *mut exc::UnicodeErrorExc) }
}
unsafe fn uerr_check(o: *mut PyObject, tp: *mut PyTypeObject) -> bool {
    unsafe {
        if o.is_null() || !typeobject::is_subtype((*o).type_ptr(), tp) {
            let _ = type_error(&format!(
                "expecting a {} object, got {}",
                typeobject::type_name(&*tp),
                if o.is_null() { "NULL".into() } else { tname(&*o) }
            ));
            return false;
        }
        true
    }
}
unsafe fn uerr_get_obj(o: *mut PyObject, field: *mut PyObject, what: &str) -> *mut PyObject {
    unsafe {
        let _ = o;
        if field.is_null() {
            let _ = type_error(&format!("{} attribute not set", what));
            return std::ptr::null_mut();
        }
        (*field).to_ref().into_raw()
    }
}
fn obj_len(o: &PyObject) -> isize {
    if pystr::is_str(o) {
        pystr::str_len(o) as isize
    } else {
        ab::length(o).map(|x| x as isize).unwrap_or(0)
    }
}
macro_rules! unicode_err_api {
    ($tp:ident, $getenc:ident, $getobj:ident, $getstart:ident, $setstart:ident, $getend:ident, $setend:ident, $getreason:ident, $setreason:ident) => {
        unicode_err_api!(@common $tp, $getobj, $getstart, $setstart, $getend, $setend, $getreason, $setreason);
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $getenc(o: *mut PyObject) -> *mut PyObject {
            unsafe {
                if !uerr_check(o, exc::$tp()) { return std::ptr::null_mut(); }
                uerr_get_obj(o, uerr(o).encoding, "encoding")
            }
        }
    };
    (@common $tp:ident, $getobj:ident, $getstart:ident, $setstart:ident, $getend:ident, $setend:ident, $getreason:ident, $setreason:ident) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $getobj(o: *mut PyObject) -> *mut PyObject {
            unsafe {
                if !uerr_check(o, exc::$tp()) { return std::ptr::null_mut(); }
                uerr_get_obj(o, uerr(o).object, "object")
            }
        }
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $getstart(o: *mut PyObject, start: *mut Py_ssize_t) -> c_int {
            unsafe {
                if !uerr_check(o, exc::$tp()) { return -1; }
                let u = uerr(o);
                if u.object.is_null() { let _ = type_error("object attribute not set"); return -1; }
                let len = obj_len(&*u.object);
                let mut s = u.start;
                if s < 0 { s = 0; }
                if s >= len { s = if len == 0 { 0 } else { len - 1 }; }
                *start = s;
                0
            }
        }
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $setstart(o: *mut PyObject, start: Py_ssize_t) -> c_int {
            unsafe {
                if !uerr_check(o, exc::$tp()) { return -1; }
                uerr(o).start = start;
                0
            }
        }
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $getend(o: *mut PyObject, end: *mut Py_ssize_t) -> c_int {
            unsafe {
                if !uerr_check(o, exc::$tp()) { return -1; }
                let u = uerr(o);
                if u.object.is_null() { let _ = type_error("object attribute not set"); return -1; }
                let len = obj_len(&*u.object);
                let mut e = u.end;
                if e < 1 { e = 1; }
                if e > len { e = len; }
                *end = e;
                0
            }
        }
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $setend(o: *mut PyObject, end: Py_ssize_t) -> c_int {
            unsafe {
                if !uerr_check(o, exc::$tp()) { return -1; }
                uerr(o).end = end;
                0
            }
        }
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $getreason(o: *mut PyObject) -> *mut PyObject {
            unsafe {
                if !uerr_check(o, exc::$tp()) { return std::ptr::null_mut(); }
                uerr_get_obj(o, uerr(o).reason, "reason")
            }
        }
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $setreason(o: *mut PyObject, reason: *const c_char) -> c_int {
            unsafe {
                if !uerr_check(o, exc::$tp()) { return -1; }
                let r = match cstr_obj(reason) { Ok(r) => r, Err(_) => return -1 };
                let u = uerr(o);
                let old = u.reason;
                u.reason = r.into_raw();
                xdecref(old);
                0
            }
        }
    };
}
unicode_err_api!(UnicodeEncodeError, PyUnicodeEncodeError_GetEncoding, PyUnicodeEncodeError_GetObject,
    PyUnicodeEncodeError_GetStart, PyUnicodeEncodeError_SetStart, PyUnicodeEncodeError_GetEnd,
    PyUnicodeEncodeError_SetEnd, PyUnicodeEncodeError_GetReason, PyUnicodeEncodeError_SetReason);
unicode_err_api!(UnicodeDecodeError, PyUnicodeDecodeError_GetEncoding, PyUnicodeDecodeError_GetObject,
    PyUnicodeDecodeError_GetStart, PyUnicodeDecodeError_SetStart, PyUnicodeDecodeError_GetEnd,
    PyUnicodeDecodeError_SetEnd, PyUnicodeDecodeError_GetReason, PyUnicodeDecodeError_SetReason);
unicode_err_api!(@common UnicodeTranslateError, PyUnicodeTranslateError_GetObject,
    PyUnicodeTranslateError_GetStart, PyUnicodeTranslateError_SetStart, PyUnicodeTranslateError_GetEnd,
    PyUnicodeTranslateError_SetEnd, PyUnicodeTranslateError_GetReason, PyUnicodeTranslateError_SetReason);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicodeDecodeError_Create(
    encoding: *const c_char,
    object: *const c_char,
    length: Py_ssize_t,
    start: Py_ssize_t,
    end: Py_ssize_t,
    reason: *const c_char,
) -> *mut PyObject {
    unsafe {
        ret((|| -> PyResult {
            let data = crate::types::bytes::new_bytes(std::slice::from_raw_parts(object as *const u8, length as usize));
            let args = [cstr_obj(encoding)?, data, int::new_int(start as i64), int::new_int(end as i64), cstr_obj(reason)?];
            let refs: Vec<&PyObject> = args.iter().map(|a| &**a).collect();
            ab::call(typeobject::type_obj(exc::UnicodeDecodeError()), &refs)
        })())
    }
}

// ---------------------------------------------------------------- warnings

unsafe fn warn_obj(category: *mut PyObject, msg: &PyObject, stacklevel: Py_ssize_t, source: Option<&PyObject>) -> c_int {
    unsafe {
        let cat = if category.is_null() { exc::RuntimeWarning() as *mut PyObject } else { category };
        let r = (|| -> PyResult<()> {
            let w = crate::import::import_absolute("warnings")?;
            let f = ab::getattr_str(&w, "warn")?;
            let kwn = tuple::new_tuple(vec![pystr::new_str("source")]);
            let sl = int::new_int(stacklevel as i64);
            ab::call_kw(&f, &[msg, &*cat, &sl, source.unwrap_or(none::none())], Some(&kwn))?;
            Ok(())
        })();
        ret_i(r)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_WarnEx(category: *mut PyObject, msg: *const c_char, stacklevel: Py_ssize_t) -> c_int {
    unsafe {
        let m = match cstr_obj(msg) {
            Ok(m) => m,
            Err(_) => return -1,
        };
        warn_obj(category, &m, stacklevel, None)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_WarnFormat(category: *mut PyObject, stacklevel: Py_ssize_t, fmt: *const c_char, mut args: ...) -> c_int {
    unsafe {
        let m = match super::printf::format_v(fmt, &mut args) {
            Ok(m) => m,
            Err(_) => return -1,
        };
        warn_obj(category, &m, stacklevel, None)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_ResourceWarning(source: *mut PyObject, stacklevel: Py_ssize_t, fmt: *const c_char, mut args: ...) -> c_int {
    unsafe {
        let m = match super::printf::format_v(fmt, &mut args) {
            Ok(m) => m,
            Err(_) => return -1,
        };
        warn_obj(exc::ResourceWarning() as *mut PyObject, &m, stacklevel, opt(source))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_WarnExplicitObject(
    category: *mut PyObject,
    message: *mut PyObject,
    filename: *mut PyObject,
    lineno: c_int,
    module: *mut PyObject,
    registry: *mut PyObject,
) -> c_int {
    unsafe {
        let cat = if category.is_null() { exc::RuntimeWarning() as *mut PyObject } else { category };
        ret_i((|| -> PyResult<()> {
            let w = crate::import::import_absolute("warnings")?;
            let f = ab::getattr_str(&w, "warn_explicit")?;
            let ln = int::new_int(lineno as i64);
            ab::call(&f, &[&*message, &*cat, &*filename, &ln, opt(module).unwrap_or(none::none()), opt(registry).unwrap_or(none::none())])?;
            Ok(())
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_WarnExplicit(
    category: *mut PyObject,
    text: *const c_char,
    filename: *const c_char,
    lineno: c_int,
    module: *const c_char,
    registry: *mut PyObject,
) -> c_int {
    unsafe {
        let m = match cstr_obj(text) {
            Ok(m) => m,
            Err(_) => return -1,
        };
        let f = match crate::capi::unicode::decode_fs(CStr::from_ptr(filename).to_bytes()) {
            Ok(m) => m,
            Err(_) => return -1,
        };
        let md = if module.is_null() {
            None
        } else {
            match cstr_obj(module) {
                Ok(m) => Some(m),
                Err(_) => return -1,
            }
        };
        PyErr_WarnExplicitObject(category, m.as_ptr(), f.as_ptr(), lineno, opt_ptr(&md), registry)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_WarnExplicitFormat(
    category: *mut PyObject,
    filename: *const c_char,
    lineno: c_int,
    module: *const c_char,
    registry: *mut PyObject,
    fmt: *const c_char,
    mut args: ...
) -> c_int {
    unsafe {
        let m = match super::printf::format_v(fmt, &mut args) {
            Ok(m) => m,
            Err(_) => return -1,
        };
        let f = match crate::capi::unicode::decode_fs(CStr::from_ptr(filename).to_bytes()) {
            Ok(m) => m,
            Err(_) => return -1,
        };
        let md = if module.is_null() {
            None
        } else {
            match cstr_obj(module) {
                Ok(m) => Some(m),
                Err(_) => return -1,
            }
        };
        PyErr_WarnExplicitObject(category, m.as_ptr(), f.as_ptr(), lineno, opt_ptr(&md), registry)
    }
}

// ---------------------------------------------------------------- unraisable / display

pub(crate) fn write_unraisable_msg(msg: Option<PyRef>, obj: *mut PyObject) {
    let Some(e) = take_exc() else { return };
    let r = (|| -> PyResult<()> {
        let hook = crate::modules::sys::get_sys_attr("unraisablehook");
        let tb = exc::exc_traceback(&e).unwrap_or_else(none::none_ref);
        let t = type_ref(e.type_ptr());
        let m = msg.clone().unwrap_or_else(none::none_ref);
        let o = unsafe { opt(obj) }.map(|o| o.to_ref()).unwrap_or_else(none::none_ref);
        if let Some(hook) = hook {
            // build a simple namespace-like object: types.SimpleNamespace if available
            let ns = match crate::import::import_absolute("types").and_then(|t| ab::getattr_str(&t, "SimpleNamespace")) {
                Ok(sn) => {
                    let kw = dict::new_dict();
                    dict::set_item_str(&kw, "exc_type", &t)?;
                    dict::set_item_str(&kw, "exc_value", &e)?;
                    dict::set_item_str(&kw, "exc_traceback", &tb)?;
                    dict::set_item_str(&kw, "err_msg", &m)?;
                    dict::set_item_str(&kw, "object", &o)?;
                    ab::call_with_kwargs_dict(&sn, &[], Some(&kw))?
                }
                Err(_) => {
                    clear_exc();
                    return Err(PyErr);
                }
            };
            ab::call(&hook, &[&ns])?;
            return Ok(());
        }
        Err(PyErr)
    })();
    if r.is_err() {
        clear_exc();
        let text = match &msg {
            Some(m) => pystr::to_string_lossy(m),
            None => "Exception ignored in".to_string(),
        };
        let o = unsafe { opt(obj) };
        let line = match o {
            Some(o) => format!("{}: {}\n", text, ab::repr_string(o).unwrap_or_else(|_| "<object repr() failed>".into())),
            None => format!("{}:\n", text),
        };
        clear_exc();
        crate::modules::sys::write_stderr(&line);
        crate::modules::traceback_print::print_exception(&e);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_WriteUnraisable(obj: *mut PyObject) {
    let msg = if obj.is_null() { None } else { Some(pystr::new_str("Exception ignored in")) };
    write_unraisable_msg(msg, obj)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_FormatUnraisable(fmt: *const c_char, mut args: ...) {
    unsafe {
        let e = take_exc();
        let msg = if fmt.is_null() {
            None
        } else {
            match super::printf::format_v(fmt, &mut args) {
                Ok(m) => Some(m),
                Err(_) => {
                    clear_exc();
                    None
                }
            }
        };
        if let Some(e) = e {
            set_exc(e);
        }
        write_unraisable_msg(msg, std::ptr::null_mut())
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_DisplayException(e: *mut PyObject) {
    unsafe {
        crate::modules::traceback_print::print_exception(&*e);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_Display(_t: *mut PyObject, v: *mut PyObject, _tb: *mut PyObject) {
    unsafe {
        if !v.is_null() {
            crate::modules::traceback_print::print_exception(&*v);
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_PrintEx(set_sys_last_vars: c_int) {
    unsafe {
        let Some(e) = take_exc() else { return };
        if exc::is_exception_instance(&e) && typeobject::is_subtype(e.type_ptr(), exc::SystemExit()) {
            let code = crate::handle_system_exit(&e);
            std::process::exit(code);
        }
        if set_sys_last_vars != 0 {
            let _ = crate::modules::sys::set_sys_attr("last_exc", &e);
            let _ = crate::modules::sys::set_sys_attr("last_type", &type_ref(e.type_ptr()));
            let _ = crate::modules::sys::set_sys_attr("last_value", &e);
            let tb = exc::exc_traceback(&e).unwrap_or_else(none::none_ref);
            let _ = crate::modules::sys::set_sys_attr("last_traceback", &tb);
        }
        set_exc(e);
        crate::print_exception_and_clear();
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_Print() {
    unsafe { PyErr_PrintEx(1) }
}

// ---------------------------------------------------------------- syntax errors

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_ProgramTextObject(filename: *mut PyObject, lineno: c_int) -> *mut PyObject {
    unsafe {
        if filename.is_null() || lineno <= 0 {
            return std::ptr::null_mut();
        }
        let f = pystr::to_string_lossy(&*filename);
        let Ok(data) = std::fs::read(&f) else { return std::ptr::null_mut() };
        let text = String::from_utf8_lossy(&data);
        match text.split_inclusive('\n').nth(lineno as usize - 1) {
            Some(l) => pystr::new_str(l).into_raw(),
            None => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_ProgramText(filename: *const c_char, lineno: c_int) -> *mut PyObject {
    unsafe {
        if filename.is_null() {
            return std::ptr::null_mut();
        }
        let f = pystr::new_str(&cs(filename));
        PyErr_ProgramTextObject(f.as_ptr(), lineno)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_RangedSyntaxLocationObject(
    filename: *mut PyObject,
    lineno: c_int,
    col_offset: c_int,
    end_lineno: c_int,
    end_col_offset: c_int,
) {
    unsafe {
        let Some(e) = take_exc() else { return };
        let set = |n: &str, v: PyRef| {
            let _ = ab::setattr(&e, &pystr::new_str(n), &v);
            clear_exc();
        };
        set("lineno", int::new_int(lineno as i64));
        if col_offset >= 0 {
            set("offset", int::new_int(col_offset as i64 + 1));
        }
        if end_lineno >= 0 {
            set("end_lineno", int::new_int(end_lineno as i64));
        }
        if end_col_offset >= 0 {
            set("end_offset", int::new_int(end_col_offset as i64 + 1));
        }
        if !filename.is_null() {
            set("filename", (*filename).to_ref());
            if typeobject::is_subtype(e.type_ptr(), exc::SyntaxError()) {
                let t = PyErr_ProgramTextObject(filename, lineno);
                if !t.is_null() {
                    set("text", PyRef::from_raw(t));
                }
            }
        }
        set_exc(e);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SyntaxLocationObject(filename: *mut PyObject, lineno: c_int, col_offset: c_int) {
    unsafe { PyErr_RangedSyntaxLocationObject(filename, lineno, col_offset, -1, -1) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SyntaxLocationEx(filename: *const c_char, lineno: c_int, col_offset: c_int) {
    unsafe {
        let f = if filename.is_null() { None } else { crate::capi::unicode::decode_fs(CStr::from_ptr(filename).to_bytes()).ok() };
        PyErr_SyntaxLocationObject(opt_ptr(&f), lineno, col_offset)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SyntaxLocation(filename: *const c_char, lineno: c_int) {
    unsafe { PyErr_SyntaxLocationEx(filename, lineno, -1) }
}

// ---------------------------------------------------------------- traceback

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyTraceBack_Here(frame: *mut PyObject) -> c_int {
    unsafe {
        let Some(e) = take_exc() else { return 0 };
        let prev = exc::exc_traceback(&e);
        let lineno = crate::capi::funcs::PyFrame_GetLineNumber(frame);
        let tb = crate::types::traceback::new_traceback(prev.as_deref(), &*frame, -1, lineno);
        exc::set_exc_traceback(&e, Some(&tb));
        set_exc(e);
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyTraceBack_Print(tb: *mut PyObject, f: *mut PyObject) -> c_int {
    unsafe {
        if tb.is_null() {
            return 0;
        }
        ret_i((|| -> PyResult<()> {
            let m = crate::import::import_absolute("traceback")?;
            let pt = ab::getattr_str(&m, "print_tb")?;
            let kwn = tuple::new_tuple(vec![pystr::new_str("file")]);
            ab::call_kw(&pt, &[&*tb, &*f], Some(&kwn))?;
            Ok(())
        })())
    }
}
