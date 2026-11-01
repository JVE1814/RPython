//! Runtime-level warning helpers (PyErr_WarnEx / PyErr_WarnExplicit); they go
//! through the `_warnings` implementation (modules/_warnings.rs).
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;

pub fn warn(category: *mut PyTypeObject, msg: &str, stacklevel: i32) -> PyResult<()> {
    warn_with_source(category, msg, stacklevel, None)
}

pub fn warn_with_source(category: *mut PyTypeObject, msg: &str, stacklevel: i32, source: Option<&PyObject>) -> PyResult<()> {
    let m = crate::types::str::new_str(msg);
    crate::modules::_warnings::warn_ex(category, &m, stacklevel as i64, source)
}

#[allow(dead_code)]
fn _x(_: PyRef) {}

/// warnings.warn_explicit equivalent used for compile-time warnings.
pub fn warn_explicit(category: *mut PyTypeObject, msg: &str, filename: &str, lineno: i32) -> PyResult<()> {
    let m = crate::types::str::new_str(msg);
    let f = crate::types::str::new_str(filename);
    crate::modules::_warnings::warn_explicit(category, &m, &f, lineno as i64, None, None)
}
