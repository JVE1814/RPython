//! `mappingproxy`: read-only proxy around a mapping (types.MappingProxyType).
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::*;
use crate::types::{dict, list, none, str as pystr, tuple};
use std::ffi::{c_int, c_void};

#[repr(C)]
pub struct MappingProxy {
    ob_base: PyObject,
    mapping: *mut PyObject,
}

#[unsafe(no_mangle)]
pub static mut PyDictProxy_Type: PyTypeObject = PyTypeObject::ZERO;

pub fn mappingproxy_type() -> *mut PyTypeObject {
    &raw mut PyDictProxy_Type
}
pub fn is_mappingproxy(o: &PyObject) -> bool {
    o.is_type(mappingproxy_type())
}
fn m(o: &PyObject) -> &PyObject {
    unsafe { &*(*(o.as_ptr() as *mut MappingProxy)).mapping }
}
/// The wrapped mapping.
pub fn proxy_mapping(o: &PyObject) -> PyRef {
    m(o).to_ref()
}

fn check_mapping(mapping: &PyObject) -> PyResult<()> {
    let tp = mapping.ty();
    let is_map = !tp.tp_as_mapping.is_null() && unsafe { (*tp.tp_as_mapping).mp_subscript.is_some() };
    if !is_map || list::is_list(mapping) || tuple::is_tuple(mapping) {
        raise!(TypeError, "mappingproxy() argument must be a mapping, not {}", tname(mapping));
    }
    Ok(())
}

/// New proxy (no type check; callers pass a dict or a checked mapping).
pub fn new_mappingproxy(mapping: &PyObject) -> PyResult {
    unsafe {
        let op = crate::alloc::alloc_object(mappingproxy_type(), std::mem::size_of::<MappingProxy>());
        (*(op as *mut MappingProxy)).mapping = mapping.to_ref().into_raw();
        crate::gc::track(op);
        Ok(PyRef::from_raw(op))
    }
}

unsafe extern "C" fn mp_new(_tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let a = tuple::tuple_items(&*args);
            if a.len() > 1 {
                raise!(TypeError, "mappingproxy() takes at most 1 argument ({} given)", a.len());
            }
            let mut kwval: Option<PyRef> = None;
            if !kw.is_null() {
                for (k, v) in dict::items_vec(&*kw) {
                    if pystr::to_string_lossy(&k) == "mapping" && a.is_empty() && kwval.is_none() {
                        kwval = Some(v);
                    } else {
                        raise!(TypeError, "mappingproxy() got an unexpected keyword argument '{}'", pystr::to_string_lossy(&k));
                    }
                }
            }
            let mapping = match a.first().copied().or(kwval.as_deref()) {
                Some(x) => x,
                None => raise!(TypeError, "mappingproxy() missing required argument 'mapping' (pos 1)"),
            };
            check_mapping(mapping)?;
            new_mappingproxy(mapping)
        })();
        into_c(r)
    }
}
unsafe extern "C" fn mp_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        xdecref((*(o as *mut MappingProxy)).mapping);
        crate::alloc::free_object(o);
    }
}
unsafe extern "C" fn mp_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let p = (*(o as *mut MappingProxy)).mapping;
        if p.is_null() { 0 } else { visit.unwrap()(p, arg) }
    }
}
unsafe extern "C" fn mp_len(o: *mut PyObject) -> Py_ssize_t {
    unsafe {
        match ab::length(m(&*o)) {
            Ok(n) => n as isize,
            Err(_) => -1,
        }
    }
}
unsafe extern "C" fn mp_getitem(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(ab::getitem(m(&*o), &*k)) }
}
unsafe extern "C" fn mp_contains(o: *mut PyObject, k: *mut PyObject) -> c_int {
    unsafe {
        let mm = m(&*o);
        let r = if dict::is_dict_exact(mm) { dict::contains(mm, &*k) } else { ab::contains(mm, &*k) };
        match r {
            Ok(b) => b as c_int,
            Err(_) => -1,
        }
    }
}
unsafe extern "C" fn mp_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(ab::get_iter(m(&*o))) }
}
unsafe extern "C" fn mp_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c((|| Ok(pystr::new_str(&format!("mappingproxy({})", ab::repr_string(m(&*o))?))))()) }
}
unsafe extern "C" fn mp_str(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(ab::str(m(&*o))) }
}
unsafe extern "C" fn mp_hash(o: *mut PyObject) -> Py_hash_t {
    unsafe {
        match ab::hash(m(&*o)) {
            Ok(h) => h,
            Err(_) => -1,
        }
    }
}
unsafe extern "C" fn mp_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        // only equality is defined; it compares the underlying mappings, a dict
        // being lent out as a copy so that the other side cannot mutate it
        if op != Py_EQ && op != Py_NE {
            return none::not_implemented().to_ref().into_raw();
        }
        let r = (|| -> PyResult {
            let mine = m(&*a);
            let mine = if crate::types::dict::is_dict_exact(mine) { crate::types::dict::copy(mine)? } else { mine.to_ref() };
            let other = if is_mappingproxy(&*b) { m(&*b).to_ref() } else { (*b).to_ref() };
            ab::rich_compare(&mine, &other, op)
        })();
        into_c(r)
    }
}
fn mp_or(a: &PyObject, b: &PyObject) -> PyResult {
    let a2 = if is_mappingproxy(a) { m(a).to_ref() } else { a.to_ref() };
    let b2 = if is_mappingproxy(b) { m(b).to_ref() } else { b.to_ref() };
    ab::binary_op(&a2, &b2, ab::NbSlot::Or)
}
fn mp_ior(a: &PyObject, b: &PyObject) -> PyResult {
    let _ = b;
    raise!(TypeError, "'|=' is not supported by {}; use '|' instead", tname(a));
}
fn get_m(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("get", args, 1, 2)?;
    let d = args.get(1).copied().unwrap_or(none::none());
    ab::call_method_str(m(o), "get", &[args[0], d])
}
fn keys_m(o: &PyObject) -> PyResult {
    ab::call_method_str(m(o), "keys", &[])
}
fn values_m(o: &PyObject) -> PyResult {
    ab::call_method_str(m(o), "values", &[])
}
fn items_m(o: &PyObject) -> PyResult {
    ab::call_method_str(m(o), "items", &[])
}
fn copy_m(o: &PyObject) -> PyResult {
    ab::call_method_str(m(o), "copy", &[])
}
fn reversed_m(o: &PyObject) -> PyResult {
    let r = crate::builtins::builtin("reversed")?;
    ab::call(&r, &[m(o)])
}
fn getitem_m(o: &PyObject, k: &PyObject) -> PyResult {
    ab::getitem(m(o), k)
}
fn class_getitem_m(cls: &PyObject, item: &PyObject) -> PyResult {
    crate::types::containers_reg::generic_alias(cls, item)
}

static mut MP_SEQ: PySequenceMethods = PySequenceMethods::ZERO;
static mut MP_MAP: PyMappingMethods = PyMappingMethods::ZERO;
static mut MP_NUM: PyNumberMethods = PyNumberMethods::ZERO;

pub fn init_types() {
    unsafe {
        let t = &mut *mappingproxy_type();
        t.tp_name = c"mappingproxy".as_ptr();
        t.tp_basicsize = std::mem::size_of::<MappingProxy>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_MAPPING;
        t.tp_dealloc = Some(mp_dealloc);
        t.tp_traverse = Some(mp_traverse);
        t.tp_repr = Some(mp_repr);
        t.tp_str = Some(mp_str);
        t.tp_hash = Some(mp_hash);
        t.tp_richcompare = Some(mp_richcompare);
        t.tp_iter = Some(mp_iter);
        t.tp_new = Some(mp_new);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_doc = c"mappingproxy(mapping)\n--\n\nRead-only proxy of a mapping.".as_ptr();
        let s = &mut *(&raw mut MP_SEQ);
        s.sq_contains = Some(mp_contains);
        t.tp_as_sequence = s;
        let mm = &mut *(&raw mut MP_MAP);
        mm.mp_length = Some(mp_len);
        mm.mp_subscript = Some(mp_getitem);
        t.tp_as_mapping = mm;
        let n = &mut *(&raw mut MP_NUM);
        n.nb_or = Some(crate::methods::binary_shim(mp_or));
        n.nb_inplace_or = Some(crate::methods::binary_shim(mp_ior));
        t.tp_as_number = n;
        t.tp_methods = crate::mdefs![
            ("__getitem__", O, getitem_m, "__getitem__($self, key, /)\n--\n\nReturn self[key]."; METH_COEXIST),
            ("get", FAST, get_m, "get($self, key, default=None, /)\n--\n\nReturn the value for key if key is in the mapping, else default."),
            ("keys", NOARGS, keys_m, "D.keys() -> a set-like object providing a view on D's keys"),
            ("values", NOARGS, values_m, "D.values() -> an object providing a view on D's values"),
            ("items", NOARGS, items_m, "D.items() -> a set-like object providing a view on D's items"),
            ("copy", NOARGS, copy_m, "D.copy() -> a shallow copy of D"),
            ("__class_getitem__", O, class_getitem_m, "See PEP 585"; METH_CLASS),
            ("__reversed__", NOARGS, reversed_m, "D.__reversed__() -> reverse iterator"),
        ];
    }
}
