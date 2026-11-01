//! `complex` core.
use crate::ffi::*;
use crate::object::*;
#[unsafe(no_mangle)]
pub static mut PyComplex_Type: PyTypeObject = PyTypeObject::ZERO;
pub fn complex_type() -> *mut PyTypeObject {
    &raw mut PyComplex_Type
}
pub fn is_complex(o: &PyObject) -> bool {
    o.is_type(complex_type()) || crate::types::typeobject::is_subtype(o.type_ptr(), complex_type())
}
pub fn new_complex(real: f64, imag: f64) -> PyRef {
    unsafe {
        let op = crate::alloc::alloc_object(complex_type(), std::mem::size_of::<PyComplexObject>());
        (*(op as *mut PyComplexObject)).cval = Py_complex { real, imag };
        PyRef::from_raw(op)
    }
}
pub fn complex_value(o: &PyObject) -> Py_complex {
    unsafe { (*(o.as_ptr() as *const PyComplexObject)).cval }
}
