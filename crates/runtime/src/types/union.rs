//! types.UnionType (stub).
use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::PyRef;
pub fn union_or(a: &PyObject, b: &PyObject) -> PyResult {
    let _ = (a, b);
    Ok(crate::types::none::not_implemented().to_ref())
}
#[allow(dead_code)]
fn _u(_: PyRef) {}
