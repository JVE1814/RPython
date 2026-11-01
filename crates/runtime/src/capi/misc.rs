//! Assorted: signals (PyErr_CheckSignals), os helpers (PyOS_*), sys (PySys_*),
//! recursion guards, version info.

use super::*;
use crate::abstract_ as ab;
use crate::types::{bytes, str as pystr};

/// Run pending signal handlers (PyErr_CheckSignals).
pub(crate) fn check_signals() -> PyResult<()> {
    crate::modules::signal::run_pending_handlers(crate::threads::current())
}

/// os.fspath semantics.
pub(crate) fn fspath(o: &PyObject) -> PyResult {
    if pystr::is_str(o) || bytes::is_bytes(o) {
        return Ok(o.to_ref());
    }
    match ab::lookup_special(o, crate::name!("__fspath__"))? {
        Some(f) => {
            let r = ab::call(&f, &[])?;
            if pystr::is_str(&r) || bytes::is_bytes(&r) {
                Ok(r)
            } else {
                Err(type_error(&format!(
                    "expected {}.__fspath__() to return str or bytes, not {}",
                    tname(o),
                    tname(&r)
                )))
            }
        }
        None => Err(type_error(&format!("expected str, bytes or os.PathLike object, not {}", tname(o)))),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_CheckSignals() -> c_int {
    ret_i(check_signals())
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_FSPath(o: *mut PyObject) -> *mut PyObject {
    unsafe { ret(fspath(&*o)) }
}
