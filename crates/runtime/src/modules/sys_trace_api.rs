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
pub fn settrace_all(f: &PyObject) -> PyResult {
    settrace(f)
}
pub fn setprofile_all(f: &PyObject) -> PyResult {
    setprofile(f)
}
pub fn call_tracing(f: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::abstract_::call(f, args)
}
pub fn install_monitoring(_d: &PyObject) -> PyResult<()> {
    Ok(())
}
