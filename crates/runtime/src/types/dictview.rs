//! dict_keys / dict_values / dict_items views (core).
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::*;
use crate::types::{bool_, dict, list, set, str as pystr, tuple};
use std::ffi::c_int;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Keys,
    Values,
    Items,
}
#[repr(C)]
pub struct DictView {
    ob_base: PyObject,
    dict: *mut PyObject,
}
pub static mut KEYS_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut VALUES_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut ITEMS_TYPE: PyTypeObject = PyTypeObject::ZERO;
fn tp(k: Kind) -> *mut PyTypeObject {
    match k {
        Kind::Keys => &raw mut KEYS_TYPE,
        Kind::Values => &raw mut VALUES_TYPE,
        Kind::Items => &raw mut ITEMS_TYPE,
    }
}
fn kind_of(o: &PyObject) -> Kind {
    if o.is_type(&raw mut KEYS_TYPE) {
        Kind::Keys
    } else if o.is_type(&raw mut VALUES_TYPE) {
        Kind::Values
    } else {
        Kind::Items
    }
}
fn d(o: &PyObject) -> &PyObject {
    unsafe { &*(*(o.as_ptr() as *mut DictView)).dict }
}
pub fn new_view(dct: &PyObject, k: Kind) -> PyResult {
    unsafe {
        let op = crate::alloc::alloc_object(tp(k), std::mem::size_of::<DictView>());
        (*(op as *mut DictView)).dict = dct.to_ref().into_raw();
        crate::gc::track(op);
        Ok(PyRef::from_raw(op))
    }
}
pub fn view_items(o: &PyObject) -> Vec<PyRef> {
    let dd = d(o);
    match kind_of(o) {
        Kind::Keys => dict::keys_vec(dd),
        Kind::Values => dict::items_vec(dd).into_iter().map(|(_, v)| v).collect(),
        Kind::Items => dict::items_vec(dd).into_iter().map(|(k, v)| tuple::tuple2(k, v)).collect(),
    }
}
unsafe extern "C" fn view_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        xdecref((*(o as *mut DictView)).dict);
        crate::alloc::free_object(o);
    }
}
unsafe extern "C" fn view_len(o: *mut PyObject) -> Py_ssize_t {
    unsafe { dict::dict_len(d(&*o)) as isize }
}
unsafe extern "C" fn view_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let l = list::new_list(view_items(&*o));
        into_c(ab::get_iter(&l))
    }
}
unsafe extern "C" fn view_contains(o: *mut PyObject, k: *mut PyObject) -> c_int {
    unsafe {
        let o = &*o;
        let k = &*k;
        let r = match kind_of(o) {
            Kind::Keys => dict::contains(d(o), k),
            Kind::Items => (|| {
                if !tuple::is_tuple(k) || tuple::tuple_len(k) != 2 {
                    return Ok(false);
                }
                match dict::get_item(d(o), tuple::tuple_get(k, 0))? {
                    Some(v) => ab::rich_eq(&v, tuple::tuple_get(k, 1)),
                    None => Ok(false),
                }
            })(),
            Kind::Values => (|| {
                for v in view_items(o) {
                    if ab::rich_eq(&v, k)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            })(),
        };
        match r {
            Ok(b) => b as c_int,
            Err(_) => -1,
        }
    }
}
unsafe extern "C" fn view_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        let r = (|| -> PyResult {
            let mut parts = Vec::new();
            for it in view_items(o) {
                parts.push(ab::repr_string(&it)?);
            }
            Ok(pystr::new_str(&format!("{}([{}])", tname(o), parts.join(", "))))
        })();
        into_c(r)
    }
}
unsafe extern "C" fn view_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if kind_of(a) == Kind::Values {
            return crate::types::none::not_implemented().to_ref().into_raw();
        }
        let sa = match set::new_set_from_vec(view_items(a)) {
            Ok(s) => s,
            Err(_) => return std::ptr::null_mut(),
        };
        let sb = if set::is_anyset(b) {
            b.to_ref()
        } else if b.is_type(&raw mut KEYS_TYPE) || b.is_type(&raw mut ITEMS_TYPE) {
            match set::new_set_from_vec(view_items(b)) {
                Ok(s) => s,
                Err(_) => return std::ptr::null_mut(),
            }
        } else {
            return crate::types::none::not_implemented().to_ref().into_raw();
        };
        into_c(ab::rich_compare(&sa, &sb, op))
    }
}
fn mapping(o: &PyObject) -> PyResult {
    crate::types::mappingproxy::new_mappingproxy(d(o))
}
static mut SEQ: PySequenceMethods = PySequenceMethods::ZERO;
pub fn init_types() {
    unsafe {
        let s = &mut *(&raw mut SEQ);
        s.sq_length = Some(view_len);
        s.sq_contains = Some(view_contains);
        for (t, n) in [(&raw mut KEYS_TYPE, c"dict_keys"), (&raw mut VALUES_TYPE, c"dict_values"), (&raw mut ITEMS_TYPE, c"dict_items")] {
            let t = &mut *t;
            t.tp_name = n.as_ptr();
            t.tp_basicsize = std::mem::size_of::<DictView>() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
            t.tp_dealloc = Some(view_dealloc);
            t.tp_iter = Some(view_iter);
            t.tp_repr = Some(view_repr);
            t.tp_richcompare = Some(view_richcompare);
            t.tp_as_sequence = s;
            t.tp_getattro = Some(object_generic_getattro);
            t.tp_getset = crate::getset_table![("mapping", mapping)];
        }
    }
}
pub fn ready() -> PyResult<()> {
    for t in [&raw mut KEYS_TYPE, &raw mut VALUES_TYPE, &raw mut ITEMS_TYPE] {
        type_ready(t)?;
    }
    Ok(())
}
#[allow(dead_code)]
fn _b() -> PyRef {
    bool_::new_bool(true)
}
