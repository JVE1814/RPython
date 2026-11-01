//! mappingproxy (stub: returns a copy of the mapping).
use crate::errors::*;
use crate::ffi::PyObject;
pub fn new_mappingproxy(m: &PyObject) -> PyResult {
    crate::types::dict::copy(m)
}
