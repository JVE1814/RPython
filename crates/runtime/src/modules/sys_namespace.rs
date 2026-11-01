//! Minimal `types.SimpleNamespace` used for `sys.implementation` (system workstream copy;
//! to be replaced by types/namespace.rs from the functional workstream when merged).
//! If another implementation registers itself via `set_namespace_type`, it is used instead.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::{dict, str as pystr, tuple};
use crate::raise;
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicPtr, Ordering::*};

#[repr(C)]
struct NsObject {
    ob_base: PyObject,
    dict: *mut PyObject,
}

static mut NS_TYPE: PyTypeObject = PyTypeObject::ZERO;
static OVERRIDE: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());

pub fn set_namespace_type(tp: *mut PyTypeObject) {
    OVERRIDE.store(tp, Release);
}

pub fn namespace_type() -> *mut PyTypeObject {
    let o = OVERRIDE.load(Acquire);
    if !o.is_null() {
        return o;
    }
    unsafe {
        let tp = &raw mut NS_TYPE;
        if (*tp).tp_flags & Py_TPFLAGS_READY == 0 {
            init_type();
        }
        tp
    }
}

unsafe extern "C" fn ns_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        xdecref((*(op as *mut NsObject)).dict);
        crate::types::typeobject::generic_free_and_type_decref(op, (*op).type_ptr());
    }
}
unsafe extern "C" fn ns_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let d = (*(op as *mut NsObject)).dict;
        if d.is_null() { 0 } else { visit.unwrap()(d, arg) }
    }
}
unsafe extern "C" fn ns_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    unsafe {
        let op = ((*tp).tp_alloc.unwrap())(tp, 0);
        if op.is_null() {
            return op;
        }
        (*(op as *mut NsObject)).dict = dict::new_dict().into_raw();
        op
    }
}
unsafe extern "C" fn ns_init(op: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let r = (|| -> PyResult<()> {
            let a = tuple::tuple_items(&*args);
            let d = &*(*(op as *mut NsObject)).dict;
            if a.len() > 1 {
                raise!(TypeError, "SimpleNamespace expected at most 1 argument, got {}", a.len());
            }
            if let Some(m) = a.first() {
                let tmp = dict::new_dict();
                let upd = ab::getattr_str(&tmp, "update")?;
                ab::call(&upd, &[m])?;
                for (k, v) in dict::items_vec(&tmp) {
                    if !pystr::is_str(&k) {
                        raise!(TypeError, "keywords must be strings, not '{}'", crate::types::typeobject::tname(&k));
                    }
                    dict::set_item(d, &k, &v)?;
                }
            }
            if !kw.is_null() {
                for (k, v) in dict::items_vec(&*kw) {
                    dict::set_item(d, &k, &v)?;
                }
            }
            Ok(())
        })();
        int_into_c(r)
    }
}
unsafe extern "C" fn ns_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        let tp = o.type_ptr();
        let name = if tp == &raw mut NS_TYPE { "namespace".to_string() } else { crate::types::typeobject::type_name(&*tp) };
        if crate::modules::reprlib_guard::enter(o) {
            return pystr::new_str(&format!("{}(...)", name)).into_raw();
        }
        let r = (|| -> PyResult {
            let d = &*(*(op as *mut NsObject)).dict;
            let mut parts = Vec::new();
            for (k, v) in dict::items_vec(d) {
                if pystr::is_str(&k) {
                    parts.push(format!("{}={}", pystr::to_string_lossy(&k), ab::repr_string(&v)?));
                }
            }
            Ok(pystr::new_str(&format!("{}({})", name, parts.join(", "))))
        })();
        crate::modules::reprlib_guard::leave(o);
        into_c(r)
    }
}
unsafe extern "C" fn ns_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let nt = &raw mut NS_TYPE;
        let ok = |o: *mut PyObject| crate::types::typeobject::is_subtype((*o).type_ptr(), nt);
        if (op == Py_EQ || op == Py_NE) && ok(a) && ok(b) {
            let da = &*(*(a as *mut NsObject)).dict;
            let db = &*(*(b as *mut NsObject)).dict;
            return into_c(ab::rich_compare(da, db, op));
        }
        crate::types::none::not_implemented().to_ref().into_raw()
    }
}
fn ns_get_dict(o: &PyObject) -> PyResult {
    unsafe { Ok(PyRef::from_borrowed((*(o.as_ptr() as *mut NsObject)).dict)) }
}
fn ns_reduce(o: &PyObject) -> PyResult {
    let tp = crate::types::typeobject::type_ref(o.type_ptr());
    let d = ns_get_dict(o)?;
    Ok(tuple::new_tuple(vec![tp, tuple::empty_tuple(), d]))
}
fn ns_replace(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let (pos, kws) = crate::args::split(args, kw);
    if !pos.is_empty() {
        raise!(TypeError, "__replace__() takes no positional arguments");
    }
    let tp = crate::types::typeobject::type_ref(o.type_ptr());
    let r = ab::call(&tp, &[])?;
    let nd = ns_get_dict(&r)?;
    dict::merge_dict(&nd, &*ns_get_dict(o)?, true)?;
    for (k, v) in kws {
        dict::set_item(&nd, k, v)?;
    }
    Ok(r)
}

fn init_type() {
    unsafe {
        let t = &mut *(&raw mut NS_TYPE);
        t.tp_name = c"types.SimpleNamespace".as_ptr();
        t.tp_basicsize = std::mem::size_of::<NsObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_BASETYPE;
        t.tp_dictoffset = std::mem::offset_of!(NsObject, dict) as isize;
        t.tp_dealloc = Some(ns_dealloc);
        t.tp_traverse = Some(ns_traverse);
        t.tp_new = Some(ns_new);
        t.tp_init = Some(ns_init);
        t.tp_repr = Some(ns_repr);
        t.tp_richcompare = Some(ns_richcompare);
        t.tp_alloc = Some(crate::types::typeobject::generic_alloc);
        t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
        t.tp_setattro = Some(crate::types::typeobject::object_generic_setattro);
        t.tp_doc = c"A simple attribute-based namespace.".as_ptr();
        t.tp_methods = crate::mdefs![("__reduce__", NOARGS, ns_reduce), ("__replace__", FASTKW, ns_replace)];
        t.tp_getset = crate::getset_table![("__dict__", ns_get_dict)];
        let _ = crate::types::typeobject::type_ready(&raw mut NS_TYPE);
    }
}

/// Create a namespace with the given attributes.
pub fn new_namespace(items: &[(&str, PyRef)]) -> PyResult {
    let ns = ab::call(crate::types::typeobject::type_obj(namespace_type()), &[])?;
    for (k, v) in items {
        ab::setattr(&ns, &pystr::new_str(k), v)?;
    }
    Ok(ns)
}
