//! BaseExceptionGroup / ExceptionGroup (stub; full version pending).
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
pub fn setup_type(tp: *mut PyTypeObject, is_base: bool) {
    let _ = (tp, is_base);
}
/// except* matching: returns (rest, match).
pub fn eg_match(e: &PyObject, t: &PyObject) -> PyResult<(PyRef, PyRef)> {
    if crate::interp::eval::exception_matches(e, t)? {
        let eg = crate::types::exceptions::make_exception(
            crate::types::exceptions::ExceptionGroup(),
            &[crate::types::str::new_str(""), crate::types::list::new_list(vec![e.to_ref()])],
        )?;
        return Ok((crate::types::none::none_ref(), eg));
    }
    Ok((e.to_ref(), crate::types::none::none_ref()))
}
pub fn prep_reraise_star(orig: &PyObject, excs: &PyObject) -> PyResult {
    let _ = (orig, excs);
    raise!(NotImplementedError, "except* re-raise not implemented yet");
}
