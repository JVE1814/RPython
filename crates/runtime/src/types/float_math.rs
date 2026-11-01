//! float helpers: pow with CPython's special cases, float() conversion.
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::*;
use crate::types::{float, int, str as pystr};
use crate::raise;
pub fn float_pow(x: f64, y: f64) -> PyResult {
    if y == 0.0 {
        return Ok(float::new_float(1.0));
    }
    if x.is_nan() {
        return Ok(float::new_float(if y == 0.0 { 1.0 } else { x }));
    }
    if x == 0.0 && y < 0.0 && y.is_finite() {
        raise!(ZeroDivisionError, "zero to a negative power");
    }
    if x < 0.0 && y.is_finite() && y != y.floor() {
        let r = crate::types::complex_pow::complex_pow((x, 0.0), (y, 0.0))?;
        return Ok(crate::types::complex::new_complex(r.0, r.1));
    }
    let r = x.powf(y);
    if r.is_infinite() && x.is_finite() && y.is_finite() {
        raise!(OverflowError, "(34, 'Numerical result out of range')");
    }
    Ok(float::new_float(r))
}
pub fn number_float(o: &PyObject) -> PyResult<f64> {
    if float::is_float(o) {
        return Ok(float::float_value(o));
    }
    if pystr::is_str(o) {
        let s: String = pystr::str_data(o)
            .iter()
            .map(|c| {
                if let Some(d) = rpy_unicode::to_decimal(c) {
                    (b'0' + d as u8) as char
                } else if rpy_unicode::is_space(c) {
                    ' '
                } else {
                    char::from_u32(c).unwrap_or('\u{fffd}')
                }
            })
            .collect();
        return match rpy_numfmt::parse_float(&s) {
            Ok(v) => Ok(v),
            Err(_) => raise!(ValueError, "could not convert string to float: {}", ab::repr_string(o)?),
        };
    }
    if let Some(b) = crate::types::bytes::bytes_like_data(o) {
        if crate::types::bytes::is_bytes(o) || crate::types::bytearray::is_bytearray(o) || crate::buffer::buffer_bytes(o).is_some() {
            return match rpy_numfmt::parse_float_bytes(&b) {
                Ok(v) => Ok(v),
                Err(_) => raise!(ValueError, "could not convert string to float: {}", ab::repr_string(o)?),
            };
        }
    }
    if int::is_int(o) && o.ty().tp_as_number.is_null() {
        return int::to_f64(o);
    }
    let nb = o.ty().tp_as_number;
    unsafe {
        if nb.is_null() || ((*nb).nb_float.is_none() && (*nb).nb_index.is_none()) {
            raise!(TypeError, "float() argument must be a string or a real number, not '{}'", crate::types::typeobject::tname(o));
        }
    }
    ab::float_value(o)
}
#[allow(dead_code)]
fn _x(_: PyRef) {}
