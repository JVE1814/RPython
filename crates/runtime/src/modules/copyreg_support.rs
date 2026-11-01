//! object.__reduce_ex__ / __getstate__ support (stub level).
use crate::errors::*;
use crate::ffi::PyObject;
use crate::raise;
pub fn object_getstate(o: &PyObject, required: bool) -> PyResult {
    let _ = required;
    match crate::types::typeobject::instance_dict_existing(o) {
        Some(d) if crate::types::dict::dict_len(&d) > 0 => crate::types::dict::copy(&d),
        _ => Ok(crate::types::none::none_ref()),
    }
}
pub fn reduce_ex(o: &PyObject, proto: i64) -> PyResult {
    let _ = (o, proto);
    raise!(NotImplementedError, "pickling via object.__reduce_ex__ not implemented yet");
}
