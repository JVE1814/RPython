//! os.fspath / filesystem name helpers.
use crate::errors::*;
use crate::ffi::PyObject;
use crate::types::str as pystr;
use crate::raise;
pub fn fsdecode(o: &PyObject) -> PyResult<String> {
    if pystr::is_str(o) {
        return Ok(pystr::to_string_lossy(o));
    }
    if crate::types::bytes::is_bytes(o) {
        return Ok(String::from_utf8_lossy(crate::types::bytes::bytes_data(o)).into_owned());
    }
    if let Some(f) = crate::abstract_::lookup_special(o, crate::name!("__fspath__"))? {
        let r = crate::abstract_::call(&f, &[])?;
        return fsdecode(&r);
    }
    raise!(TypeError, "expected str, bytes or os.PathLike object, not {}", crate::types::typeobject::tname(o));
}
