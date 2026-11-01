//! Instances of str subclasses (non-compact "legacy" layout with separate data buffer).
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::str as pystr;
pub fn new_str_subclass(tp: *mut PyTypeObject, s: &PyObject) -> PyResult {
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(crate::types::typeobject::generic_alloc);
        let op = alloc(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        let len = pystr::str_len(s);
        let kind = pystr::str_kind(s);
        let ascii = pystr::str_is_ascii(s);
        let nbytes = (len + 1) * kind as usize;
        let buf = crate::alloc::raw_calloc(nbytes);
        std::ptr::copy_nonoverlapping(pystr::str_data_ptr(s), buf, len * kind as usize);
        let u = &mut *(op as *mut PyUnicodeObject);
        u._base._base.length = len as isize;
        u._base._base.hash = -1;
        u._base._base.state = (kind << 8) | if ascii { 1 << 12 } else { 0 };
        u._base.utf8 = if ascii { buf as *mut _ } else { std::ptr::null_mut() };
        u._base.utf8_length = if ascii { len as isize } else { 0 };
        u.data = buf as *mut _;
        Ok(PyRef::from_raw(op))
    }
}
