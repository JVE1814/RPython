//! types.SimpleNamespace (`_PyNamespace_Type`).

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::fncommon::{self as fc, opt, targs};
use crate::object::*;
use crate::types::{dict, none, str as pystr, tuple};
use crate::{mdefs, raise};
use std::ffi::{c_int, c_void};

#[repr(C)]
pub struct Namespace {
    ob_base: PyObject,
    dict: *mut PyObject,
}

#[unsafe(no_mangle)]
pub static mut _PyNamespace_Type: PyTypeObject = PyTypeObject::ZERO;

pub fn namespace_type() -> *mut PyTypeObject {
    let t = &raw mut _PyNamespace_Type;
    unsafe {
        if (*t).tp_flags & Py_TPFLAGS_READY == 0 {
            init_type();
            let _ = crate::types::typeobject::type_ready(t);
        }
    }
    t
}

pub fn is_namespace(o: &PyObject) -> bool {
    crate::types::typeobject::is_subtype(o.type_ptr(), namespace_type())
}

fn ns_dict(o: &PyObject) -> PyRef {
    unsafe {
        let n = &mut *(o.as_ptr() as *mut Namespace);
        if n.dict.is_null() {
            n.dict = dict::new_dict().into_raw();
        }
        (*n.dict).to_ref()
    }
}

/// `_PyNamespace_New(kwds)`: a new SimpleNamespace with attributes from `kwds`.
pub fn new_namespace(kwds: Option<&PyObject>) -> PyResult {
    let tp = namespace_type();
    unsafe {
        let op = fc::alloc(tp)?;
        let r = PyRef::from_raw(op);
        let d = ns_dict(&r);
        if let Some(k) = kwds {
            dict::merge_dict(&d, k, true)?;
        }
        Ok(r)
    }
}

unsafe extern "C" fn ns_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let op = fc::alloc(tp)?;
            let r = PyRef::from_raw(op);
            ns_dict(&r);
            Ok(r)
        })())
    }
}

fn check_str_keys(d: &PyObject) -> PyResult<()> {
    for k in dict::keys_vec(d) {
        if !pystr::is_str(&k) {
            raise!(TypeError, "keywords must be strings");
        }
    }
    Ok(())
}

unsafe extern "C" fn ns_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let r = (|| -> PyResult<()> {
            let a = targs(args);
            if a.len() > 1 {
                raise!(TypeError, "SimpleNamespace expected at most 1 positional argument, got {}", a.len());
            }
            let d = ns_dict(&*o);
            if let Some(src) = a.first() {
                let tmp = dict::new_dict();
                crate::types::core_slots::dict_update_any(&tmp, src)?;
                check_str_keys(&tmp)?;
                dict::merge_dict(&d, &tmp, true)?;
            }
            if let Some(k) = opt(kw) {
                check_str_keys(k)?;
                dict::merge_dict(&d, k, true)?;
            }
            Ok(())
        })();
        int_into_c(r)
    }
}

unsafe extern "C" fn ns_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let o = &*o;
            let name = if o.is_type(namespace_type()) { "namespace".to_string() } else { fc::repr_type_name(o) };
            let _g = match fc::ReprGuard::enter(o) {
                Some(g) => g,
                None => return Ok(pystr::new_str(&format!("{}(...)", name))),
            };
            let d = ns_dict(o);
            let mut parts = Vec::new();
            for (k, v) in dict::items_vec(&d) {
                if !pystr::is_str(&k) {
                    continue;
                }
                parts.push(format!("{}={}", pystr::to_string_lossy(&k), ab::repr_string(&v)?));
            }
            Ok(pystr::new_str(&format!("{}({})", name, parts.join(", "))))
        })())
    }
}

unsafe extern "C" fn ns_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if is_namespace(a) && is_namespace(b) && (op == Py_EQ || op == Py_NE) {
            return into_c(ab::rich_compare(&ns_dict(a), &ns_dict(b), op));
        }
        none::not_implemented().to_ref().into_raw()
    }
}

fn ns_reduce(o: &PyObject) -> PyResult {
    Ok(tuple::new_tuple(vec![
        crate::types::typeobject::type_ref(o.type_ptr()),
        tuple::empty_tuple(),
        ns_dict(o),
    ]))
}

fn ns_replace(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let nkw = kw.map_or(0, tuple::tuple_len);
    if args.len() > nkw {
        raise!(TypeError, "__replace__() takes no positional arguments");
    }
    let tp = crate::types::typeobject::type_ref(o.type_ptr());
    let r = ab::call(&tp, &[])?;
    if !is_namespace(&r) {
        raise!(
            TypeError,
            "expect types.SimpleNamespace type, but {}() returned '{}' object",
            crate::types::typeobject::type_fqname(o.type_ptr()),
            fc::tname(&r)
        );
    }
    let d = ns_dict(&r);
    dict::merge_dict(&d, &ns_dict(o), true)?;
    if let Some(k) = kw {
        for (i, name) in tuple::tuple_items(k).iter().enumerate() {
            dict::set_item(&d, name, args[args.len() - nkw + i])?;
        }
    }
    Ok(r)
}

unsafe extern "C" fn ns_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        fc::clear_field(&raw mut (*(o as *mut Namespace)).dict);
        fc::free(o);
    }
}
unsafe extern "C" fn ns_traverse(o: *mut PyObject, v: visitproc, arg: *mut c_void) -> c_int {
    unsafe { fc::visit((*(o as *mut Namespace)).dict, v, arg) }
}
unsafe extern "C" fn ns_clear(o: *mut PyObject) -> c_int {
    unsafe { fc::clear_field(&raw mut (*(o as *mut Namespace)).dict) };
    0
}

unsafe fn init_type() {
    unsafe {
        let t = &raw mut _PyNamespace_Type;
        fc::init_type(t, c"types.SimpleNamespace", std::mem::size_of::<Namespace>(), Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_BASETYPE);
        (*t).tp_new = Some(ns_new);
        (*t).tp_init = Some(ns_init);
        (*t).tp_repr = Some(ns_repr);
        (*t).tp_richcompare = Some(ns_richcompare);
        (*t).tp_dealloc = Some(ns_dealloc);
        (*t).tp_traverse = Some(ns_traverse);
        (*t).tp_clear = Some(ns_clear);
        (*t).tp_setattro = Some(crate::types::typeobject::object_generic_setattro);
        (*t).tp_dictoffset = std::mem::offset_of!(Namespace, dict) as isize;
        (*t).tp_hash = Some(crate::types::slots::hash_not_implemented);
        (*t).tp_doc = c"SimpleNamespace(mapping_or_iterable=(), /, **kwargs)\n--\n\nA simple attribute-based namespace.".as_ptr();
        (*t).tp_methods = mdefs![
            ("__reduce__", NOARGS, ns_reduce, "Return state information for pickling"),
            ("__replace__", FASTKW, ns_replace, "__replace__($self, /, **changes)\n--\n\nReturn a copy of the namespace object with new values for the specified attributes."),
        ];
        (*t).tp_getset = crate::getset_table![("__dict__", fc::get_dict_attr, fc::set_dict_attr)];
    }
}
