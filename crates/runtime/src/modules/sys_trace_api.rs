//! sys.settrace / setprofile API (pending real implementation).
use crate::errors::*;
use crate::ffi::PyObject;
use crate::types::none;
pub fn settrace(f: &PyObject) -> PyResult {
    let _ = f;
    Ok(none::none_ref())
}
pub fn gettrace() -> PyResult {
    Ok(none::none_ref())
}
pub fn setprofile(f: &PyObject) -> PyResult {
    let _ = f;
    Ok(none::none_ref())
}
pub fn getprofile() -> PyResult {
    Ok(none::none_ref())
}
