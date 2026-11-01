//! PEP 667 FrameLocalsProxy (stub: returns a snapshot dict for now).
use crate::errors::*;
use crate::ffi::PyObject;
pub fn new_proxy(frame_obj: &PyObject) -> PyResult {
    let f = crate::types::frameobject::frame_of(frame_obj).ok_or_else(|| crate::errors::system_error("no frame"))?;
    crate::types::frameobject::locals_dict(f)
}
