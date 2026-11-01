//! Buffer protocol helpers.
use crate::ffi::*;
/// Copy the contents of any object supporting the buffer protocol.
pub fn buffer_bytes(o: &PyObject) -> Option<Vec<u8>> {
    let bp = o.ty().tp_as_buffer;
    unsafe {
        if bp.is_null() || (*bp).bf_getbuffer.is_none() {
            return None;
        }
        let mut view: Py_buffer = std::mem::zeroed();
        if (*bp).bf_getbuffer.unwrap()(o.as_ptr(), &mut view, 0) < 0 {
            crate::errors::clear_exc();
            return None;
        }
        let v = std::slice::from_raw_parts(view.buf as *const u8, view.len as usize).to_vec();
        if let Some(r) = (*bp).bf_releasebuffer {
            r(o.as_ptr(), &mut view);
        }
        if !view.obj.is_null() {
            crate::object::decref(view.obj);
        }
        Some(v)
    }
}
