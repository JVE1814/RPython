//! PEP 649/749 annotations of classes and modules.
//!
//! Classes: the compiler stores the class-body annotate function as `__annotate_func__`
//! in the class namespace (an explicit `__annotate__` in the body wins); evaluated
//! annotations are cached as `__annotations_cache__`. Old-style (`from __future__ import
//! annotations`) classes have a real `__annotations__` dict in their namespace.
//! Modules: `__annotate__` / `__annotations__` live in the module dict; annotations are
//! not cached while the module is still initializing.
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::{as_type, is_heap_type, type_dict, type_name};
use crate::types::{dict, int, none};
use crate::{name, raise};

fn tp_name(tp: *mut PyTypeObject) -> String {
    unsafe { std::ffi::CStr::from_ptr((*tp).tp_name).to_string_lossy().into_owned() }
}

fn bind_descr(v: PyRef, owner: &PyObject) -> PyResult {
    if let Some(get) = v.ty().tp_descr_get {
        return unsafe { from_c(get(v.as_ptr(), std::ptr::null_mut(), owner.as_ptr())) };
    }
    Ok(v)
}

fn call_annotate_value(f: &PyObject) -> PyResult {
    let one = int::new_int(1);
    let r = ab::call(f, &[&one])?;
    if !dict::is_dict(&r) {
        raise!(TypeError, "__annotate__() must return a dict, not {}", type_name(r.ty()));
    }
    Ok(r)
}

fn immutable(tp: *mut PyTypeObject) -> bool {
    unsafe { (*tp).tp_flags & Py_TPFLAGS_IMMUTABLETYPE != 0 || !is_heap_type(tp) }
}

pub fn type_get_annotations(o: &PyObject) -> PyResult {
    let tp = as_type(o);
    if !is_heap_type(tp) {
        raise!(AttributeError, "type object '{}' has no attribute '__annotations__'", tp_name(tp));
    }
    let d = type_dict(tp);
    let a = dict::get_item_str(d, name!("__annotations__")).or_else(|| dict::get_item_str(d, name!("__annotations_cache__")));
    if let Some(a) = a {
        return bind_descr(a, o);
    }
    let annotate = type_get_annotate(o)?;
    let r = if ab::is_callable(&annotate) { call_annotate_value(&annotate)? } else { dict::new_dict() };
    dict::set_item(d, name!("__annotations_cache__"), &r)?;
    Ok(r)
}

pub fn type_set_annotations(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let tp = as_type(o);
    if immutable(tp) {
        let what = if v.is_some() { "set" } else { "delete" };
        raise!(TypeError, "cannot {} '__annotations__' attribute of immutable type '{}'", what, tp_name(tp));
    }
    let d = type_dict(tp);
    match v {
        Some(v) => {
            if dict::get_item_str(d, name!("__annotations__")).is_some() {
                dict::set_item(d, name!("__annotations__"), v)?;
            } else {
                dict::set_item(d, name!("__annotations_cache__"), v)?;
            }
        }
        None => {
            if !dict::del_item(d, name!("__annotations_cache__"))? && !dict::del_item(d, name!("__annotations__"))? {
                raise!(AttributeError, "__annotations__");
            }
        }
    }
    dict::del_item(d, name!("__annotate_func__"))?;
    dict::del_item(d, name!("__annotate__"))?;
    crate::types::slots::type_modified(tp);
    Ok(())
}

pub fn type_get_annotate(o: &PyObject) -> PyResult {
    let tp = as_type(o);
    if !is_heap_type(tp) {
        raise!(AttributeError, "type object '{}' has no attribute '__annotate__'", tp_name(tp));
    }
    let d = type_dict(tp);
    let a = dict::get_item_str(d, name!("__annotate__")).or_else(|| dict::get_item_str(d, name!("__annotate_func__")));
    match a {
        Some(a) => bind_descr(a, o),
        None => {
            dict::set_item(d, name!("__annotate_func__"), none::none())?;
            Ok(none::none_ref())
        }
    }
}

pub fn type_set_annotate(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let tp = as_type(o);
    let v = match v {
        None => raise!(TypeError, "cannot delete __annotate__ attribute"),
        Some(v) => v,
    };
    if immutable(tp) {
        raise!(TypeError, "cannot set '__annotate__' attribute of immutable type '{}'", tp_name(tp));
    }
    if !v.is(none::none()) && !ab::is_callable(v) {
        raise!(TypeError, "__annotate__ must be callable or None");
    }
    let d = type_dict(tp);
    dict::set_item(d, name!("__annotate__"), v)?;
    if !v.is(none::none()) {
        dict::del_item(d, name!("__annotations_cache__"))?;
    }
    crate::types::slots::type_modified(tp);
    Ok(())
}

fn module_initializing(d: &PyObject) -> bool {
    if let Some(spec) = dict::get_item_str(d, name!("__spec__")) {
        if spec.is(none::none()) {
            return false;
        }
        match ab::getattr_opt(&spec, name!("_initializing")) {
            Ok(Some(v)) => return ab::is_true(&v).unwrap_or_else(|_| {
                clear_exc();
                false
            }),
            Ok(None) => return false,
            Err(_) => {
                clear_exc();
                return false;
            }
        }
    }
    false
}

fn module_dict_checked(o: &PyObject) -> PyResult {
    let d = ab::getattr(o, name!("__dict__"))?;
    if !dict::is_dict(&d) {
        raise!(TypeError, "<module>.__dict__ is not a dictionary");
    }
    Ok(d)
}

pub fn module_get_annotations(o: &PyObject) -> PyResult {
    let d = module_dict_checked(o)?;
    if let Some(a) = dict::get_item_str(&d, name!("__annotations__")) {
        return Ok(a);
    }
    let initializing = module_initializing(&crate::types::module::module_dict(o));
    let r = match dict::get_item_str(&d, name!("__annotate__")) {
        Some(f) if ab::is_callable(&f) => call_annotate_value(&f)?,
        _ => dict::new_dict(),
    };
    if !initializing {
        dict::set_item(&d, name!("__annotations__"), &r)?;
    }
    Ok(r)
}

pub fn module_set_annotations(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let d = module_dict_checked(o)?;
    match v {
        Some(v) => dict::set_item(&d, name!("__annotations__"), v)?,
        None => {
            if !dict::del_item(&d, name!("__annotations__"))? {
                raise!(AttributeError, "__annotations__");
            }
        }
    }
    dict::del_item(&d, name!("__annotate__"))?;
    Ok(())
}

pub fn module_get_annotate(o: &PyObject) -> PyResult {
    let d = module_dict_checked(o)?;
    match dict::get_item_str(&d, name!("__annotate__")) {
        Some(a) => Ok(a),
        None => {
            dict::set_item(&d, name!("__annotate__"), none::none())?;
            Ok(none::none_ref())
        }
    }
}

pub fn module_set_annotate(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        None => raise!(TypeError, "cannot delete __annotate__ attribute"),
        Some(v) => v,
    };
    let d = module_dict_checked(o)?;
    if !v.is(none::none()) && !ab::is_callable(v) {
        raise!(TypeError, "__annotate__ must be callable or None");
    }
    dict::set_item(&d, name!("__annotate__"), v)?;
    if !v.is(none::none()) {
        dict::del_item(&d, name!("__annotations__"))?;
    }
    Ok(())
}
