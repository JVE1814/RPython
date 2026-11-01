//! ModuleSpec creation for Rust-loaded modules (uses importlib when available).
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::PyRef;
use crate::types::{bool_, dict, none, str as pystr};

/// Build a ModuleSpec through importlib (None if importlib is not installed).
pub fn make_spec(name: &str, origin: Option<&str>, is_pkg: bool) -> PyResult<Option<PyRef>> {
    let bs = match crate::import::get_loaded_module("_frozen_importlib_external") {
        Some(b) => b,
        None => return Ok(None),
    };
    let origin = match origin {
        Some(o) => o,
        None => return Ok(None),
    };
    let loader_cls = ab::getattr_str(&bs, "SourceFileLoader")?;
    let n = pystr::new_str(name);
    let o = pystr::new_str(origin);
    let loader = ab::call(&loader_cls, &[&n, &o])?;
    let f = ab::getattr_str(&bs, "spec_from_file_location")?;
    let kwn = crate::types::tuple::new_tuple(vec![pystr::new_str("loader")]);
    let spec = ab::call_kw(&f, &[&n, &o, &loader], Some(&kwn))?;
    if is_pkg {
        let _ = bool_::new_bool(true);
    }
    Ok(Some(spec))
}

pub fn mark_initialized(d: &PyObject) {
    if let Some(spec) = dict::get_item_rs(d, "__spec__") {
        if !spec.is(none::none()) && ab::setattr(&spec, &pystr::new_str("_initializing"), bool_::false_obj()).is_err() {
            clear_exc();
        }
    }
}

/// Set `__loader__` (and `__spec__ = None`) for a script run as __main__.
pub fn set_main_loader(d: &PyObject, path: &str) -> PyResult<()> {
    if let Some(bs) = crate::import::get_loaded_module("_frozen_importlib_external") {
        let cls = ab::getattr_str(&bs, "SourceFileLoader")?;
        let l = ab::call(&cls, &[&pystr::new_str("__main__"), &pystr::new_str(path)])?;
        dict::set_item_str(d, "__loader__", &l)?;
    }
    Ok(())
}
