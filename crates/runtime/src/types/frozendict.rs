//! `frozendict` (PEP 814, new in 3.15): immutable, hashable mapping. The object
//! layout starts with `PyDictObject` (so `PyDict_GET_SIZE` works on it) followed by
//! the cached hash; storage is shared with dict.rs. Not a dict subclass.
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::dict_methods as dm;
use crate::types::typeobject::*;
use crate::types::{dict, dictview, none, str as pystr, tuple};
use std::ffi::c_int;

#[repr(C)]
pub struct PyFrozenDictObject {
    pub base: PyDictObject,
    pub ma_hash: Py_hash_t,
}

#[unsafe(no_mangle)]
pub static mut PyFrozenDict_Type: PyTypeObject = PyTypeObject::ZERO;

#[inline(always)]
pub fn frozendict_type() -> *mut PyTypeObject {
    &raw mut PyFrozenDict_Type
}
#[inline(always)]
pub fn is_frozendict_exact(o: &PyObject) -> bool {
    o.is_type(frozendict_type())
}
#[inline]
pub fn is_frozendict(o: &PyObject) -> bool {
    o.is_type(frozendict_type()) || (is_heap_type(o.type_ptr()) && is_subtype(o.type_ptr(), frozendict_type()))
}
fn fd(o: &PyObject) -> &mut PyFrozenDictObject {
    unsafe { &mut *(o.as_ptr() as *mut PyFrozenDictObject) }
}

fn alloc_fd(tp: *mut PyTypeObject) -> PyResult {
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
        let op = alloc(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        dict::init_dict_storage(op);
        (*(op as *mut PyFrozenDictObject)).ma_hash = -1;
        Ok(PyRef::from_raw(op))
    }
}

/// PyFrozenDict_New: a frozendict built from an optional mapping/iterable.
pub fn new_frozendict(src: Option<&PyObject>) -> PyResult {
    let r = alloc_fd(frozendict_type())?;
    if let Some(s) = src {
        dm::dict_update_any(&r, s)?;
    }
    Ok(r)
}

unsafe extern "C" fn fd_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let a = tuple::tuple_items(&*args);
            if a.len() > 1 {
                raise!(TypeError, "frozendict expected at most 1 argument, got {}", a.len());
            }
            let has_kw = !kw.is_null() && dict::dict_len(&*kw) > 0;
            if tp == frozendict_type() && !has_kw {
                if let Some(x) = a.first() {
                    if is_frozendict_exact(x) {
                        return Ok(x.to_ref());
                    }
                }
            }
            let r = alloc_fd(tp)?;
            if let Some(src) = a.first() {
                dm::dict_update_any(&r, src)?;
            }
            if has_kw {
                dict::merge_dict(&r, &*kw, true)?;
            }
            Ok(r)
        })();
        into_c(r)
    }
}

unsafe extern "C" fn fd_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        let tn = tname(o);
        if dict::dict_len(o) == 0 {
            return pystr::new_str(&format!("{}()", tn)).into_raw();
        }
        let r = (|| -> PyResult {
            match dm::repr_body(o)? {
                None => Ok(pystr::new_str(&format!("{}(...)", tn))),
                Some(b) => Ok(pystr::new_str(&format!("{}({})", tn, pystr::to_string_lossy(&b)))),
            }
        })();
        into_c(r)
    }
}

fn compute_hash(o: &PyObject) -> PyResult<isize> {
    let ts = crate::threads::current();
    crate::interp::enter_recursive_call(ts, " while hashing a frozendict")?;
    let r = (|| {
        let items = dict::items_hash_vec(o);
        let mut hs = Vec::with_capacity(items.len());
        for (_, v, kh) in &items {
            let vh = ab::hash(v)?;
            let mut th = crate::hash::TupleHasher::new(2);
            th.add(*kh);
            th.add(vh);
            hs.push(th.finish());
        }
        Ok(crate::types::set::frozenset_hash_of(hs.into_iter(), items.len()))
    })();
    crate::interp::leave_recursive_call(ts);
    r
}
unsafe extern "C" fn fd_hash(o: *mut PyObject) -> Py_hash_t {
    unsafe {
        let o = &*o;
        let f = fd(o);
        if f.ma_hash != -1 {
            return f.ma_hash;
        }
        match compute_hash(o) {
            Ok(h) => {
                f.ma_hash = h;
                h
            }
            Err(_) => -1,
        }
    }
}
unsafe extern "C" fn fd_len(o: *mut PyObject) -> Py_ssize_t {
    unsafe { dict::dict_len(&*o) as isize }
}
unsafe extern "C" fn fd_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        into_c(dm::anydict_getitem(o, &*k, is_frozendict_exact(o)))
    }
}
unsafe extern "C" fn fd_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe { into_c(dm::anydict_richcompare(&*a, &*b, op)) }
}
unsafe extern "C" fn fd_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        dict::free_storage(op);
        let tp = (*op).type_ptr();
        generic_free_and_type_decref(op, tp);
    }
}
fn fd_or(a: &PyObject, b: &PyObject) -> PyResult {
    if !dict::is_anydict(a) || !dict::is_anydict(b) {
        return Ok(none::not_implemented().to_ref());
    }
    if is_frozendict_exact(a) && dict::dict_len(b) == 0 {
        return Ok(a.to_ref());
    }
    if is_frozendict_exact(b) && dict::dict_len(a) == 0 && is_frozendict(a) {
        return Ok(b.to_ref());
    }
    let r = alloc_fd(frozendict_type())?;
    dict::merge_dict(&r, a, true)?;
    dm::dict_update_any(&r, b)?;
    Ok(r)
}
fn copy_m(o: &PyObject) -> PyResult {
    if is_frozendict_exact(o) {
        return Ok(o.to_ref());
    }
    let r = alloc_fd(frozendict_type())?;
    dict::merge_dict(&r, o, true)?;
    Ok(r)
}
fn fromkeys_m(cls: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("fromkeys", args, 1, 2)?;
    dm::fromkeys_impl(cls, args[0], args.get(1).copied().unwrap_or(none::none()))
}
fn getnewargs_m(o: &PyObject) -> PyResult {
    Ok(tuple::new_tuple(vec![dict::copy(o)?]))
}
fn contains_m(o: &PyObject, k: &PyObject) -> PyResult {
    Ok(crate::types::bool_::new_bool(dict::contains(o, k)?))
}
unsafe extern "C" fn fd_contains(o: *mut PyObject, k: *mut PyObject) -> c_int {
    unsafe {
        match dict::contains(&*o, &*k) {
            Ok(b) => b as c_int,
            Err(_) => -1,
        }
    }
}

static mut FD_SEQ: PySequenceMethods = PySequenceMethods::ZERO;
static mut FD_MAP: PyMappingMethods = PyMappingMethods::ZERO;
static mut FD_NUM: PyNumberMethods = PyNumberMethods::ZERO;

pub fn init_types() {
    unsafe {
        let t = &mut *frozendict_type();
        t.tp_name = c"frozendict".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyFrozenDictObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_MAPPING;
        t.tp_dealloc = Some(fd_dealloc);
        t.tp_traverse = Some(dict::dict_traverse);
        t.tp_repr = Some(fd_repr);
        t.tp_hash = Some(fd_hash);
        t.tp_richcompare = Some(fd_richcompare);
        t.tp_iter = Some(dm::dict_iter);
        t.tp_new = Some(fd_new);
        t.tp_alloc = Some(generic_alloc);
        t.tp_free = Some(generic_free);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_doc = c"frozendict() -> empty frozendict\nfrozendict(mapping) -> frozendict initialized from a mapping object's\n    (key, value) pairs\nfrozendict(iterable) -> frozendict initialized as if via:\n    d = {}\n    for k, v in iterable:\n        d[k] = v\nfrozendict(**kwargs) -> frozendict initialized with the name=value pairs\n    in the keyword argument list.  For example:  frozendict(one=1, two=2)".as_ptr();
        let s = &mut *(&raw mut FD_SEQ);
        s.sq_contains = Some(fd_contains);
        t.tp_as_sequence = s;
        let m = &mut *(&raw mut FD_MAP);
        m.mp_length = Some(fd_len);
        m.mp_subscript = Some(fd_subscript);
        t.tp_as_mapping = m;
        let n = &mut *(&raw mut FD_NUM);
        n.nb_or = Some(crate::methods::binary_shim(fd_or));
        t.tp_as_number = n;
        t.tp_methods = crate::mdefs![
            ("__contains__", O, contains_m, "__contains__($self, key, /)\n--\n\nTrue if the dictionary has the specified key, else False."; METH_COEXIST),
            ("__sizeof__", NOARGS, dm::sizeof_m, "__sizeof__($self, /)\n--\n\nReturn the size of the frozendict in memory, in bytes."),
            ("__reversed__", NOARGS, dm::reversed_m, "__reversed__($self, /)\n--\n\nReturn a reverse iterator over the frozendict keys."),
            ("__class_getitem__", O, dm::class_getitem_m, "See PEP 585"; METH_CLASS),
            ("__getnewargs__", NOARGS, getnewargs_m),
            ("get", FAST, dm::get_m, "get($self, key, default=None, /)\n--\n\nReturn the value for key if key is in the dictionary, else default."),
            ("keys", NOARGS, dm::keys_m, "keys($self, /)\n--\n\nReturn a set-like object providing a view on the frozendict's keys."),
            ("values", NOARGS, dm::values_m, "values($self, /)\n--\n\nReturn an object providing a view on the frozendict's values."),
            ("items", NOARGS, dm::items_m, "items($self, /)\n--\n\nReturn a set-like object providing a view on the frozendict's items."),
            ("copy", NOARGS, copy_m, "copy($self, /)\n--\n\nReturn a shallow copy of the frozendict."),
            ("fromkeys", FAST, fromkeys_m, "fromkeys($type, iterable, value=None, /)\n--\n\nCreate a new frozendict with keys from iterable and values set to value."; METH_CLASS),
        ];
        let _ = dictview::Kind::Keys;
    }
}
