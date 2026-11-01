//! `float` core. Methods/arithmetic live in `float_methods.rs`.

use crate::alloc;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;

#[unsafe(no_mangle)]
pub static mut PyFloat_Type: PyTypeObject = PyTypeObject::ZERO;

#[inline(always)]
pub fn float_type() -> *mut PyTypeObject {
    &raw mut PyFloat_Type
}
#[inline(always)]
pub fn is_float(o: &PyObject) -> bool {
    o.is_type(float_type()) || crate::types::typeobject::is_subtype(o.type_ptr(), float_type())
}
#[inline(always)]
pub fn is_float_exact(o: &PyObject) -> bool {
    o.is_type(float_type())
}
#[inline(always)]
pub fn float_value(o: &PyObject) -> f64 {
    unsafe { (*(o.as_ptr() as *const PyFloatObject)).ob_fval }
}

#[inline]
pub fn new_float(v: f64) -> PyRef {
    unsafe {
        let op = alloc::alloc_object(float_type(), std::mem::size_of::<PyFloatObject>());
        (*(op as *mut PyFloatObject)).ob_fval = v;
        PyRef::from_raw(op)
    }
}

pub fn new_float_typed(tp: *mut PyTypeObject, v: f64) -> PyResult {
    if tp == float_type() {
        return Ok(new_float(v));
    }
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(crate::types::typeobject::generic_alloc);
        let op = alloc(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        (*(op as *mut PyFloatObject)).ob_fval = v;
        Ok(PyRef::from_raw(op))
    }
}

pub fn float_hash(o: &PyObject) -> isize {
    crate::hash::hash_double(float_value(o), crate::hash::hash_pointer(o.as_ptr() as usize))
}
