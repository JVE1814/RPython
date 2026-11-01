//! Weak references (stub: no weakrefs are created yet).
use crate::ffi::PyObject;
use crate::object::PyRef;
pub unsafe fn clear_weakrefs(o: *mut PyObject) {
    let _ = o;
}
pub unsafe fn clear_weakrefs_at(o: *mut PyObject, list: *mut *mut PyObject) {
    let _ = (o, list);
}
pub fn first_weakref(o: &PyObject) -> Option<PyRef> {
    let _ = o;
    None
}
