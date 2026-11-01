//! `bytearray` core.
use crate::ffi::*;
use crate::object::*;
#[unsafe(no_mangle)]
pub static mut PyByteArray_Type: PyTypeObject = PyTypeObject::ZERO;
pub fn bytearray_type() -> *mut PyTypeObject {
    &raw mut PyByteArray_Type
}
pub fn is_bytearray(o: &PyObject) -> bool {
    o.is_type(bytearray_type()) || crate::types::typeobject::is_subtype(o.type_ptr(), bytearray_type())
}
pub fn bytearray_data(o: &PyObject) -> &[u8] {
    unsafe {
        let b = &*(o.as_ptr() as *const PyByteArrayObject);
        if b.ob_start.is_null() {
            return &[];
        }
        std::slice::from_raw_parts(b.ob_start, b.ob_base.ob_size as usize)
    }
}
