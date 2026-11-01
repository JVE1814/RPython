//! PEP 750 Template / Interpolation (stub).
use crate::errors::*;
use crate::ffi::PyObject;
use crate::raise;
pub fn new_template(strings: &PyObject, interps: &PyObject) -> PyResult {
    let _ = (strings, interps);
    raise!(NotImplementedError, "t-strings not implemented yet");
}
pub fn new_interpolation(v: &PyObject, s: &PyObject, conv: u32, spec: Option<&PyObject>) -> PyResult {
    let _ = (v, s, conv, spec);
    raise!(NotImplementedError, "t-strings not implemented yet");
}
