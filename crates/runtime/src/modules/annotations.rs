//! PEP 649/749 annotation helpers for types and modules (stub level).
use crate::errors::*;
use crate::ffi::PyObject;
use crate::types::{dict, none};
use crate::{name, raise};
pub fn type_get_annotations(o: &PyObject) -> PyResult {
    let d = crate::types::typeobject::type_dict(crate::types::typeobject::as_type(o));
    if let Some(a) = dict::get_item_str(d, name!("__annotations__")) {
        return Ok(a);
    }
    if let Some(f) = dict::get_item_str(d, name!("__annotate__")) {
        if !f.is(none::none()) {
            let one = crate::types::int::new_int(1);
            let r = crate::abstract_::call(&f, &[&one])?;
            dict::set_item(d, name!("__annotations__"), &r)?;
            return Ok(r);
        }
    }
    raise!(AttributeError, "type object '{}' has no attribute '__annotations__'", crate::types::typeobject::type_name(unsafe { &*crate::types::typeobject::as_type(o) }));
}
pub fn type_set_annotations(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let d = crate::types::typeobject::type_dict(crate::types::typeobject::as_type(o));
    match v {
        Some(v) => dict::set_item(d, name!("__annotations__"), v),
        None => {
            if !dict::del_item(d, name!("__annotations__"))? {
                raise!(AttributeError, "__annotations__");
            }
            Ok(())
        }
    }
}
pub fn type_get_annotate(o: &PyObject) -> PyResult {
    let d = crate::types::typeobject::type_dict(crate::types::typeobject::as_type(o));
    Ok(dict::get_item_str(d, name!("__annotate__")).unwrap_or_else(none::none_ref))
}
pub fn type_set_annotate(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let d = crate::types::typeobject::type_dict(crate::types::typeobject::as_type(o));
    match v {
        Some(v) => dict::set_item(d, name!("__annotate__"), v),
        None => raise!(TypeError, "cannot delete __annotate__ attribute"),
    }
}
pub fn module_get_annotations(o: &PyObject) -> PyResult {
    let d = crate::types::module::module_dict(o);
    if let Some(a) = dict::get_item_str(&d, name!("__annotations__")) {
        return Ok(a);
    }
    if let Some(f) = dict::get_item_str(&d, name!("__annotate__")) {
        if !f.is(none::none()) {
            let one = crate::types::int::new_int(1);
            let r = crate::abstract_::call(&f, &[&one])?;
            dict::set_item(&d, name!("__annotations__"), &r)?;
            return Ok(r);
        }
    }
    let r = dict::new_dict();
    dict::set_item(&d, name!("__annotations__"), &r)?;
    Ok(r)
}
pub fn module_set_annotations(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let d = crate::types::module::module_dict(o);
    match v {
        Some(v) => dict::set_item(&d, name!("__annotations__"), v),
        None => {
            if !dict::del_item(&d, name!("__annotations__"))? {
                raise!(AttributeError, "__annotations__");
            }
            Ok(())
        }
    }
}
