//! Async generator protocol (stub).
use crate::ffi::*;
pub fn setup_asyncgen_type(tp: *mut PyTypeObject) {
    let _ = tp;
}
/// Wrap a value yielded by an async generator (distinguishes it from awaited values).
pub fn wrap_value(v: crate::object::PyRef) -> crate::errors::PyResult {
    Ok(v)
}
