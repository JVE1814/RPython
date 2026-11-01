//! None, NotImplemented, Ellipsis singletons (static immortal objects).

use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::str as pystr;
use std::ffi::c_int;

#[unsafe(no_mangle)]
pub static mut _Py_NoneStruct: PyObject = unsafe { std::mem::zeroed() };
#[unsafe(no_mangle)]
pub static mut _Py_NotImplementedStruct: PyObject = unsafe { std::mem::zeroed() };
#[unsafe(no_mangle)]
pub static mut _Py_EllipsisObject: PyObject = unsafe { std::mem::zeroed() };
#[unsafe(no_mangle)]
pub static mut _PyNone_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut _PyNotImplemented_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyEllipsis_Type: PyTypeObject = PyTypeObject::ZERO;

#[inline(always)]
pub fn none() -> &'static PyObject {
    unsafe { &*(&raw const _Py_NoneStruct) }
}
#[inline(always)]
pub fn none_ref() -> PyRef {
    unsafe { PyRef::from_raw(&raw mut _Py_NoneStruct) }
}
#[inline(always)]
pub fn not_implemented() -> &'static PyObject {
    unsafe { &*(&raw const _Py_NotImplementedStruct) }
}
#[inline(always)]
pub fn ellipsis() -> &'static PyObject {
    unsafe { &*(&raw const _Py_EllipsisObject) }
}
#[inline(always)]
pub fn is_none(o: &PyObject) -> bool {
    o.is(none())
}

unsafe extern "C" fn none_repr(_o: *mut PyObject) -> *mut PyObject {
    crate::name!("None").to_ref().into_raw()
}
unsafe extern "C" fn ni_repr(_o: *mut PyObject) -> *mut PyObject {
    crate::name!("NotImplemented").to_ref().into_raw()
}
unsafe extern "C" fn ell_repr(_o: *mut PyObject) -> *mut PyObject {
    crate::name!("Ellipsis").to_ref().into_raw()
}
unsafe extern "C" fn none_bool(_o: *mut PyObject) -> c_int {
    0
}
unsafe extern "C" fn ni_bool(_o: *mut PyObject) -> c_int {
    let _ = err!(TypeError, "NotImplemented should not be used in a boolean context");
    -1
}
unsafe extern "C" fn none_hash(_o: *mut PyObject) -> Py_hash_t {
    0xFCA86420
}
unsafe extern "C" fn no_dealloc(_o: *mut PyObject) {}

fn singleton_new(name: &str, args: *mut PyObject, kw: *mut PyObject, v: &PyObject) -> *mut PyObject {
    unsafe {
        let n = crate::types::tuple::tuple_len(&*args) + if kw.is_null() { 0 } else { crate::types::dict::dict_len(&*kw) };
        if n > 0 {
            return into_c(Err(err!(TypeError, "{} takes no arguments", name)));
        }
    }
    v.to_ref().into_raw()
}
unsafe extern "C" fn none_new(_t: *mut PyTypeObject, a: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    singleton_new("NoneType", a, k, none())
}
unsafe extern "C" fn ni_new(_t: *mut PyTypeObject, a: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    singleton_new("NotImplementedType", a, k, not_implemented())
}
unsafe extern "C" fn ell_new(_t: *mut PyTypeObject, a: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    singleton_new("ellipsis", a, k, ellipsis())
}
fn ni_reduce(_o: &PyObject) -> PyResult {
    Ok(pystr::new_str("NotImplemented"))
}
fn ell_reduce(_o: &PyObject) -> PyResult {
    Ok(pystr::new_str("Ellipsis"))
}

static mut NONE_NUMBER: PyNumberMethods = PyNumberMethods::ZERO;
static mut NI_NUMBER: PyNumberMethods = PyNumberMethods::ZERO;

pub fn init_types() {
    unsafe {
        let t = &mut *(&raw mut _PyNone_Type);
        t.tp_name = c"NoneType".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT;
        t.tp_dealloc = Some(no_dealloc);
        t.tp_repr = Some(none_repr);
        t.tp_hash = Some(none_hash);
        t.tp_new = Some(none_new);
        (*(&raw mut NONE_NUMBER)).nb_bool = Some(none_bool);
        t.tp_as_number = &raw mut NONE_NUMBER;
        t.tp_doc = c"The type of the None singleton.".as_ptr();

        let t = &mut *(&raw mut _PyNotImplemented_Type);
        t.tp_name = c"NotImplementedType".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT;
        t.tp_dealloc = Some(no_dealloc);
        t.tp_repr = Some(ni_repr);
        t.tp_new = Some(ni_new);
        (*(&raw mut NI_NUMBER)).nb_bool = Some(ni_bool);
        t.tp_as_number = &raw mut NI_NUMBER;
        t.tp_methods = crate::mdefs![("__reduce__", NOARGS, ni_reduce)];
        t.tp_doc = c"The type of the NotImplemented singleton.".as_ptr();

        let t = &mut *(&raw mut PyEllipsis_Type);
        t.tp_name = c"ellipsis".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT;
        t.tp_dealloc = Some(no_dealloc);
        t.tp_repr = Some(ell_repr);
        t.tp_new = Some(ell_new);
        t.tp_methods = crate::mdefs![("__reduce__", NOARGS, ell_reduce)];
        t.tp_doc = c"The type of the Ellipsis singleton.".as_ptr();

        for (o, tp) in [
            (&raw mut _Py_NoneStruct, &raw mut _PyNone_Type),
            (&raw mut _Py_NotImplementedStruct, &raw mut _PyNotImplemented_Type),
            (&raw mut _Py_EllipsisObject, &raw mut PyEllipsis_Type),
        ] {
            (*o).ob_type.store(tp, std::sync::atomic::Ordering::Relaxed);
            make_immortal(o);
        }
    }
}
