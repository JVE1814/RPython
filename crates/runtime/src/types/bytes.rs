//! `bytes` core (methods in bytes_methods.rs, pending).
use crate::alloc;
use crate::ffi::*;
use crate::object::*;

#[unsafe(no_mangle)]
pub static mut PyBytes_Type: PyTypeObject = PyTypeObject::ZERO;
pub fn bytes_type() -> *mut PyTypeObject {
    &raw mut PyBytes_Type
}
pub fn is_bytes(o: &PyObject) -> bool {
    o.ty().tp_flags & Py_TPFLAGS_BYTES_SUBCLASS != 0
}
const HEADER: usize = std::mem::offset_of!(PyBytesObject, ob_sval);

pub fn alloc_bytes(n: usize) -> PyRef {
    unsafe {
        let op = alloc::alloc_object(bytes_type(), HEADER + n + 1);
        set_var_size(op, n as isize);
        (*(op as *mut PyBytesObject)).ob_shash = -1;
        PyRef::from_raw(op)
    }
}
pub fn new_bytes(b: &[u8]) -> PyRef {
    let r = alloc_bytes(b.len());
    unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), bytes_ptr(&r), b.len()) };
    r
}
#[inline]
pub fn bytes_ptr(o: &PyObject) -> *mut u8 {
    unsafe { (o.as_ptr() as *mut u8).add(HEADER) }
}
#[inline]
pub fn bytes_data(o: &PyObject) -> &[u8] {
    unsafe { std::slice::from_raw_parts(bytes_ptr(o), var_size(o) as usize) }
}
/// Data of any bytes-like object (bytes, bytearray, memoryview...) as an owned Vec.
pub fn bytes_like_data(o: &PyObject) -> Option<Vec<u8>> {
    if is_bytes(o) {
        return Some(bytes_data(o).to_vec());
    }
    if crate::types::bytearray::is_bytearray(o) {
        return Some(crate::types::bytearray::bytearray_data(o).to_vec());
    }
    crate::buffer::buffer_bytes(o)
}
pub fn bytes_hash(o: &PyObject) -> isize {
    crate::hash::hash_bytes(bytes_data(o))
}
