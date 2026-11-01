//! __format__ implementations bridging to rpy-numfmt.
use crate::errors::*;
use crate::ffi::PyObject;
use crate::types::{int, str as pystr};
use crate::raise;
use rpy_numfmt::{FormatSpec, IntPresentation, Locale};

pub fn numfmt_error(e: rpy_numfmt::NumFmtError) -> PyErr {
    use rpy_numfmt::ErrorKind as K;
    match e.kind {
        K::ValueError => err!(ValueError, "{}", e.msg),
        K::OverflowError => err!(OverflowError, "{}", e.msg),
        K::TypeError => err!(TypeError, "{}", e.msg),
        K::MemoryError => memory_error(),
    }
}

fn spec_str(spec: &PyObject) -> PyResult<String> {
    if !pystr::is_str(spec) {
        raise!(TypeError, "format() argument 2 must be str, not {}", crate::types::typeobject::tname(spec));
    }
    Ok(pystr::to_string_lossy(spec))
}

pub fn format_int(o: &PyObject, spec: &PyObject) -> PyResult {
    let s = spec_str(spec)?;
    if s.is_empty() {
        return crate::abstract_::str(o);
    }
    let tname = crate::types::typeobject::tname(o);
    let fs = FormatSpec::parse_int(&s, &tname).map_err(numfmt_error)?;
    let pres = rpy_numfmt::format::int_presentation(&fs, &tname).map_err(numfmt_error)?;
    let mut out = String::new();
    match pres {
        IntPresentation::Radix(r) => {
            let mag = int::int_methods_mag_string(o, r);
            rpy_numfmt::format_int(&mut out, int::sign(o) < 0, &mag, &fs, &Locale::c()).map_err(numfmt_error)?;
        }
        IntPresentation::Char => {
            rpy_numfmt::format_int_char(&mut out, int::as_i64(o), &fs).map_err(numfmt_error)?;
        }
        IntPresentation::Float => {
            let f = int::to_f64(o)?;
            rpy_numfmt::format::format_float_spec(&mut out, f, &fs, &Locale::c(), &tname).map_err(numfmt_error)?;
        }
    }
    Ok(pystr::new_str(&out))
}

pub fn format_float(v: f64, spec: &PyObject, tname: &str) -> PyResult {
    let s = spec_str(spec)?;
    let r = rpy_numfmt::format_float(v, &s, tname).map_err(numfmt_error)?;
    Ok(pystr::new_str(&r))
}

pub fn format_str(o: &PyObject, spec: &PyObject) -> PyResult {
    let s = spec_str(spec)?;
    if s.is_empty() {
        return Ok(crate::abstract_::str(o)?);
    }
    let text = pystr::to_string_lossy(o);
    let r = rpy_numfmt::format_str(&text, &s, &crate::types::typeobject::tname(o)).map_err(numfmt_error)?;
    Ok(pystr::new_str(&r))
}
