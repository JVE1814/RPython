//! memoryview and buffer protocol helpers (stub).
use crate::errors::*;
use crate::ffi::*;
use crate::raise;
use std::ffi::c_int;
pub fn memoryview_from_object_flags(o: &PyObject, flags: c_int) -> PyResult {
    let _ = (o, flags);
    raise!(NotImplementedError, "memoryview not implemented yet");
}
pub fn release_buffer_wrapper(o: &PyObject, b: &PyObject) -> PyResult {
    let _ = (o, b);
    raise!(NotImplementedError, "memoryview not implemented yet");
}
pub unsafe fn slot_getbuffer_via_dunder(o: &PyObject, view: *mut Py_buffer, flags: c_int) -> c_int {
    let _ = (o, view, flags);
    let _ = err!(NotImplementedError, "__buffer__ not implemented yet");
    -1
}
pub unsafe fn slot_releasebuffer_via_dunder(o: &PyObject, view: *mut Py_buffer) {
    let _ = (o, view);
}
