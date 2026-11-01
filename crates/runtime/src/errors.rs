//! Error handling. Like CPython, the pending exception lives in the thread
//! state; `PyErr` is a zero-sized marker meaning "an exception is set".
//! `PyResult<PyRef>` is therefore pointer-sized and converts to/from the C
//! convention (NULL + exception set) for free.

use crate::ffi::*;
use crate::object::*;
use crate::threads;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub struct PyErr;

pub type PyResult<T = PyRef> = Result<T, PyErr>;

/// Set the current exception (takes ownership of the instance).
pub fn set_exc(exc: PyRef) {
    let ts = threads::current_mut();
    let p = unsafe { &raw mut (*ts).c.current_exception };
    unsafe {
        let old = *p;
        *p = exc.into_raw();
        xdecref(old);
    }
}

#[inline]
pub fn err_occurred() -> bool {
    !threads::current().c.current_exception.is_null()
}

/// Borrow the current exception (or null).
#[inline]
pub fn current_exc_ptr() -> *mut PyObject {
    threads::current().c.current_exception
}

/// Take the current exception out of the thread state.
pub fn take_exc() -> Option<PyRef> {
    let ts = threads::current_mut();
    let p = unsafe { &raw mut (*ts).c.current_exception };
    unsafe {
        let e = *p;
        *p = std::ptr::null_mut();
        PyRef::from_raw_opt(e)
    }
}

pub fn clear_exc() {
    drop(take_exc());
}

/// Create an exception instance `tp(msg)` and set it.
pub fn new_err(tp: *mut PyTypeObject, msg: &str) -> PyErr {
    let m = crate::types::str::new_str(msg);
    set_err_obj(tp, m)
}

/// Raise `tp(arg)` with an arbitrary argument object.
pub fn set_err_obj(tp: *mut PyTypeObject, arg: PyRef) -> PyErr {
    match crate::types::exceptions::make_exception(tp, &[arg]) {
        Ok(e) => set_exc(e),
        Err(_) => {}
    }
    PyErr
}

/// Raise `tp()` with no arguments.
pub fn set_err_noargs(tp: *mut PyTypeObject) -> PyErr {
    match crate::types::exceptions::make_exception(tp, &[]) {
        Ok(e) => set_exc(e),
        Err(_) => {}
    }
    PyErr
}

/// Does the current exception match `tp` (isinstance)?
pub fn exc_matches(tp: *mut PyTypeObject) -> bool {
    let e = current_exc_ptr();
    if e.is_null() {
        return false;
    }
    unsafe { crate::types::typeobject::is_subtype((*e).type_ptr(), tp) }
}

#[macro_export]
macro_rules! exc_type {
    ($name:ident) => {
        unsafe { $crate::types::exceptions::$name() }
    };
}

/// `raise!(TypeError, "fmt {}", x)` — return Err after setting the exception.
#[macro_export]
macro_rules! raise {
    ($name:ident, $($arg:tt)*) => {
        return Err($crate::errors::new_err($crate::types::exceptions::$name(), &format!($($arg)*)))
    };
}

/// `err!(TypeError, "fmt {}", x)` — build a PyErr (sets the exception).
#[macro_export]
macro_rules! err {
    ($name:ident, $($arg:tt)*) => {
        $crate::errors::new_err($crate::types::exceptions::$name(), &format!($($arg)*))
    };
}

pub fn type_error(msg: &str) -> PyErr {
    new_err(crate::types::exceptions::TypeError(), msg)
}
pub fn value_error(msg: &str) -> PyErr {
    new_err(crate::types::exceptions::ValueError(), msg)
}
pub fn overflow_error(msg: &str) -> PyErr {
    new_err(crate::types::exceptions::OverflowError(), msg)
}
pub fn memory_error() -> PyErr {
    set_err_noargs(crate::types::exceptions::MemoryError())
}
pub fn system_error(msg: &str) -> PyErr {
    new_err(crate::types::exceptions::SystemError(), msg)
}

/// Convert a PyResult into the C convention.
#[inline(always)]
pub fn into_c(r: PyResult) -> *mut PyObject {
    match r {
        Ok(o) => o.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Convert a C-convention return into a PyResult.
#[inline(always)]
pub unsafe fn from_c(p: *mut PyObject) -> PyResult {
    if p.is_null() {
        if !err_occurred() {
            return Err(system_error("error return without exception set"));
        }
        Err(PyErr)
    } else {
        Ok(unsafe { PyRef::from_raw(p) })
    }
}

#[inline(always)]
pub fn int_into_c(r: PyResult<()>) -> std::ffi::c_int {
    match r {
        Ok(()) => 0,
        Err(_) => -1,
    }
}
