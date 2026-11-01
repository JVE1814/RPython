//! `bool`: subclass of int with two immortal static instances.

use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::int;
use std::ffi::c_int;

#[repr(C)]
pub struct BoolObject {
    pub ob_base: PyObject,
    pub lv_tag: usize,
    pub ob_digit: [u32; 2],
}

#[unsafe(no_mangle)]
pub static mut PyBool_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut _Py_FalseStruct: BoolObject = unsafe { std::mem::zeroed() };
#[unsafe(no_mangle)]
pub static mut _Py_TrueStruct: BoolObject = unsafe { std::mem::zeroed() };

pub fn bool_type() -> *mut PyTypeObject {
    &raw mut PyBool_Type
}
#[inline(always)]
pub fn true_obj() -> &'static PyObject {
    unsafe { &*(&raw const _Py_TrueStruct as *const PyObject) }
}
#[inline(always)]
pub fn false_obj() -> &'static PyObject {
    unsafe { &*(&raw const _Py_FalseStruct as *const PyObject) }
}
#[inline(always)]
pub fn new_bool(b: bool) -> PyRef {
    unsafe { PyRef::from_raw(if b { true_obj() } else { false_obj() }.as_ptr()) }
}
#[inline(always)]
pub fn is_bool(o: &PyObject) -> bool {
    o.is_type(bool_type())
}

unsafe extern "C" fn bool_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        if (*o).is(true_obj()) {
            crate::name!("True").to_ref().into_raw()
        } else {
            crate::name!("False").to_ref().into_raw()
        }
    }
}
unsafe extern "C" fn bool_new(_t: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        if !kw.is_null() && crate::types::dict::dict_len(&*kw) > 0 {
            return into_c(Err(err!(TypeError, "bool() takes no keyword arguments")));
        }
        let a = crate::types::tuple::tuple_items(&*args);
        if a.len() > 1 {
            return into_c(Err(err!(TypeError, "bool expected at most 1 argument, got {}", a.len())));
        }
        if a.is_empty() {
            return new_bool(false).into_raw();
        }
        into_c(crate::abstract_::is_true(a[0]).map(new_bool))
    }
}
fn bool_binop(a: &PyObject, b: &PyObject, f: fn(bool, bool) -> bool, fallback: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject) -> *mut PyObject {
    if is_bool(a) && is_bool(b) {
        return new_bool(f(a.is(true_obj()), b.is(true_obj()))).into_raw();
    }
    unsafe { fallback(a.as_ptr(), b.as_ptr()) }
}
unsafe extern "C" fn bool_and(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { bool_binop(&*a, &*b, |x, y| x & y, (*(*int::int_type()).tp_as_number).nb_and.unwrap()) }
}
unsafe extern "C" fn bool_or(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { bool_binop(&*a, &*b, |x, y| x | y, (*(*int::int_type()).tp_as_number).nb_or.unwrap()) }
}
unsafe extern "C" fn bool_xor(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { bool_binop(&*a, &*b, |x, y| x ^ y, (*(*int::int_type()).tp_as_number).nb_xor.unwrap()) }
}
unsafe extern "C" fn bool_invert(a: *mut PyObject) -> *mut PyObject {
    unsafe {
        if crate::errors::err_occurred() {
            return std::ptr::null_mut();
        }
        let msg = "Bitwise inversion '~' on bool is deprecated and will be removed in Python 3.16. This returns the bitwise inversion of the underlying int object and is usually not what you expect from negating a bool. Use the 'not' operator for boolean negation or ~int(x) if you really want the bitwise inversion of the underlying int.";
        if crate::modules::warnings::warn(crate::types::exceptions::DeprecationWarning(), msg, 1).is_err() {
            return std::ptr::null_mut();
        }
        int::new_int(!((*a).is(true_obj()) as i64)).into_raw()
    }
}
unsafe extern "C" fn bool_dealloc(_o: *mut PyObject) {}
unsafe extern "C" fn bool_nb_bool(o: *mut PyObject) -> c_int {
    unsafe { (*o).is(true_obj()) as c_int }
}

static mut BOOL_NUMBER: PyNumberMethods = PyNumberMethods::ZERO;

pub fn init_types() {
    unsafe {
        let t = &mut *bool_type();
        t.tp_name = c"bool".as_ptr();
        t.tp_base = int::int_type();
        t.tp_basicsize = std::mem::size_of::<PyLongObject>() as isize;
        t.tp_itemsize = 4;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_LONG_SUBCLASS;
        t.tp_repr = Some(bool_repr);
        t.tp_new = Some(bool_new);
        t.tp_dealloc = Some(bool_dealloc);
        let n = &mut *(&raw mut BOOL_NUMBER);
        n.nb_and = Some(bool_and);
        n.nb_or = Some(bool_or);
        n.nb_xor = Some(bool_xor);
        n.nb_invert = Some(bool_invert);
        n.nb_bool = Some(bool_nb_bool);
        t.tp_as_number = n;
        t.tp_doc = c"bool(object=False, /)\n--\n\nConvert a value to True or False using its truth value.\nTrue and False are the only instances of bool, a subclass of int that\ncannot itself be subclassed.".as_ptr();
        for (o, v) in [(&raw mut _Py_FalseStruct, 0u32), (&raw mut _Py_TrueStruct, 1u32)] {
            let p = o as *mut PyObject;
            (*p).ob_type.store(bool_type(), std::sync::atomic::Ordering::Relaxed);
            make_immortal(p);
            (*o).lv_tag = if v == 0 { 1 } else { (1 << 3) | 0 };
            (*o).ob_digit[0] = v;
        }
    }
}
