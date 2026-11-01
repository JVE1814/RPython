//! dict methods & helpers (stub level).
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::PyObject;
use crate::types::{dict, str as pystr, tuple};
use crate::raise;

pub fn raise_key_error(k: &PyObject) {
    let _ = set_err_obj(crate::types::exceptions::KeyError(), if tuple::is_tuple(k) { tuple::new_tuple(vec![k.to_ref()]) } else { k.to_ref() });
}
/// d.update(o) for mappings (has keys()) or iterable of pairs.
pub fn dict_update_from(d: &PyObject, o: &PyObject) -> PyResult<()> {
    if dict::is_dict(o) {
        return dict::merge_dict(d, o, true);
    }
    let keys = ab::call_method_str(o, "keys", &[])?;
    for k in ab::collect_iterable(&keys)? {
        let v = ab::getitem(o, &k)?;
        dict::set_item(d, &k, &v)?;
    }
    Ok(())
}
/// Merge **kwargs mapping into d, raising on duplicates (f(**a, **b)).
pub fn dict_merge_kwargs(d: &PyObject, o: &PyObject, func: *mut PyObject) -> PyResult<()> {
    let fname = || -> String {
        if func.is_null() {
            return "function".into();
        }
        let f = unsafe { &*func };
        match ab::getattr_opt(f, crate::name!("__qualname__")) {
            Ok(Some(q)) if pystr::is_str(&q) => format!("{}()", pystr::to_string_lossy(&q)),
            _ => format!("{} object", crate::types::typeobject::tname(f)),
        }
    };
    let pairs: Vec<(crate::object::PyRef, crate::object::PyRef)> = if dict::is_dict(o) {
        dict::items_vec(o)
    } else {
        let keys = match ab::getattr_opt(o, crate::name!("keys"))? {
            Some(k) => ab::call(&k, &[])?,
            None => raise!(TypeError, "{} argument after ** must be a mapping, not {}", fname(), crate::types::typeobject::tname(o)),
        };
        let mut v = Vec::new();
        for k in ab::collect_iterable(&keys)? {
            let val = ab::getitem(o, &k)?;
            v.push((k, val));
        }
        v
    };
    for (k, v) in pairs {
        if !pystr::is_str(&k) {
            raise!(TypeError, "keywords must be strings");
        }
        if dict::contains(d, &k)? {
            raise!(TypeError, "{} got multiple values for keyword argument '{}'", fname(), pystr::to_string_lossy(&k));
        }
        dict::set_item(d, &k, &v)?;
    }
    Ok(())
}
