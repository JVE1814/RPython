//! `_interpchannels`: cross-interpreter channels (placeholder, filled in below).

use crate::errors::*;
use crate::ffi::*;
use crate::object::*;

pub fn end_id_of(_o: &PyObject) -> PyResult<Option<(i64, bool)>> {
    Ok(None)
}
pub fn end_object(_id: i64, _send: bool) -> PyResult {
    Err(system_error("channels not available"))
}
pub fn channel_id_of(_o: &PyObject) -> Option<(i64, i32)> {
    None
}
pub fn channel_id_object(_id: i64, _end: i32) -> PyResult {
    Err(system_error("channels not available"))
}
pub fn interp_destroyed(_interp: i64) {}

pub fn init_module() -> PyResult {
    let (m, _d) = crate::modules::fncommon::new_module("_interpchannels", std::ptr::null_mut(), None)?;
    Ok(m)
}
