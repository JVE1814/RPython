//! Warnings: route to the `warnings` module when available, else print to stderr.
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
pub fn warn(category: *mut PyTypeObject, msg: &str, stacklevel: i32) -> PyResult<()> {
    warn_with_source(category, msg, stacklevel, None)
}
pub fn warn_with_source(category: *mut PyTypeObject, msg: &str, stacklevel: i32, source: Option<&PyObject>) -> PyResult<()> {
    if let Some(wmod) = crate::import::get_loaded_module("warnings") {
        if let Ok(w) = crate::abstract_::getattr_str(&wmod, "warn") {
            let m = crate::types::str::new_str(msg);
            let cat = crate::types::typeobject::type_ref(category);
            let sl = crate::types::int::new_int(stacklevel as i64);
            let src = source.map(|s| s.to_ref()).unwrap_or_else(crate::types::none::none_ref);
            let kwn = crate::types::tuple::new_tuple(vec![crate::types::str::new_str("source")]);
            crate::abstract_::call_kw(&w, &[&m, &cat, &sl, &src], Some(&kwn))?;
            return Ok(());
        }
    }
    eprintln!("{}: {}", crate::types::typeobject::type_name(unsafe { &*category }), msg);
    Ok(())
}
#[allow(dead_code)]
fn _x(_: PyRef) {}
/// warnings.warn_explicit equivalent used for compile-time warnings.
pub fn warn_explicit(category: *mut PyTypeObject, msg: &str, filename: &str, lineno: i32) -> PyResult<()> {
    if let Some(wmod) = crate::import::get_loaded_module("warnings") {
        if let Ok(w) = crate::abstract_::getattr_str(&wmod, "warn_explicit") {
            let m = crate::types::str::new_str(msg);
            let cat = crate::types::typeobject::type_ref(category);
            let f = crate::types::str::new_str(filename);
            let l = crate::types::int::new_int(lineno as i64);
            crate::abstract_::call(&w, &[&m, &cat, &f, &l])?;
            return Ok(());
        }
    }
    eprintln!("{}:{}: {}: {}", filename, lineno, crate::types::typeobject::type_name(unsafe { &*category }), msg);
    Ok(())
}
