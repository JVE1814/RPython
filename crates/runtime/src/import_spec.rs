//! ModuleSpec creation for Rust-loaded modules (uses importlib when available).
use crate::errors::*;
use crate::object::PyRef;
pub fn make_spec(name: &str, origin: Option<&str>, is_pkg: bool) -> PyResult<Option<PyRef>> {
    let _ = (name, origin, is_pkg);
    Ok(None)
}
pub fn mark_initialized(d: &crate::ffi::PyObject) {
    let _ = d;
}
