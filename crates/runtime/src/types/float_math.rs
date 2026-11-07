//! float numerics shared by float, complex and math: pow with CPython's special cases,
//! floor division/modulo, float -> int conversion, PyNumber_Float.
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::tname;
use crate::types::{float, int, str as pystr};

/// OverflowError(34, 'Numerical result out of range') — what CPython raises from errno ERANGE.
pub fn erange_error() -> PyErr {
    errno_error(crate::types::exceptions::OverflowError(), 34, "Numerical result out of range")
}

/// ValueError(33, 'Numerical argument out of domain') — errno EDOM.
pub fn edom_error() -> PyErr {
    errno_error(crate::types::exceptions::ValueError(), 33, "Numerical argument out of domain")
}

pub fn errno_error(tp: *mut crate::ffi::PyTypeObject, errno: i64, msg: &str) -> PyErr {
    match crate::types::exceptions::make_exception(tp, &[int::new_int(errno), pystr::new_str(msg)]) {
        Ok(e) => set_exc(e),
        Err(e) => return e,
    }
    PyErr
}

#[inline]
fn is_odd_integer(y: f64) -> bool {
    // y is a finite integer with |y| < 2**53 and odd
    y.is_finite() && y.abs() < 9007199254740992.0 && (y % 2.0).abs() == 1.0
}

pub enum PowResult {
    Float(f64),
    /// negative base with fractional exponent: caller computes a complex power
    Complex,
}

/// float ** float with CPython semantics.
pub fn float_pow_value(iv: f64, iw: f64) -> PyResult<PowResult> {
    use PowResult::*;
    if iw == 0.0 {
        return Ok(Float(1.0));
    }
    if iv.is_nan() {
        return Ok(Float(iv));
    }
    if iw.is_nan() {
        return Ok(Float(if iv == 1.0 { 1.0 } else { iw }));
    }
    if iw.is_infinite() {
        let a = iv.abs();
        if a == 1.0 {
            return Ok(Float(1.0));
        }
        return Ok(Float(if (iw > 0.0) == (a > 1.0) { iw.abs() } else { 0.0 }));
    }
    if iv.is_infinite() {
        let odd = is_odd_integer(iw);
        if iw > 0.0 {
            return Ok(Float(if odd { iv } else { iv.abs() }));
        }
        return Ok(Float(if odd { 0.0f64.copysign(iv) } else { 0.0 }));
    }
    if iv == 0.0 {
        let odd = is_odd_integer(iw);
        if iw < 0.0 {
            raise!(ZeroDivisionError, "zero to a negative power");
        }
        return Ok(Float(if odd { iv } else { 0.0 }));
    }
    let mut v = iv;
    let mut negate = false;
    if v < 0.0 {
        if iw != iw.floor() {
            return Ok(Complex);
        }
        v = -v;
        negate = is_odd_integer(iw);
    }
    if v == 1.0 {
        return Ok(Float(if negate { -1.0 } else { 1.0 }));
    }
    let mut ix = v.powf(iw);
    if ix.is_infinite() {
        return Err(erange_error());
    }
    if negate {
        ix = -ix;
    }
    Ok(Float(ix))
}

pub fn float_pow(x: f64, y: f64) -> PyResult {
    match float_pow_value(x, y)? {
        PowResult::Float(r) => Ok(float::new_float(r)),
        PowResult::Complex => {
            let r = crate::types::complex_pow::complex_pow((x, 0.0), (y, 0.0))?;
            Ok(crate::types::complex::new_complex(r.0, r.1))
        }
    }
}

/// CPython's float divmod: (floordiv, mod).
pub fn float_divmod(vx: f64, wx: f64) -> PyResult<(f64, f64)> {
    if wx == 0.0 {
        raise!(ZeroDivisionError, "division by zero");
    }
    let mut m = vx % wx;
    let mut div = (vx - m) / wx;
    if m != 0.0 {
        if (wx < 0.0) != (m < 0.0) {
            m += wx;
            div -= 1.0;
        }
    } else {
        m = 0.0f64.copysign(wx);
    }
    let fd = if div != 0.0 {
        let f = div.floor();
        if div - f > 0.5 { f + 1.0 } else { f }
    } else {
        0.0f64.copysign(vx / wx)
    };
    Ok((fd, m))
}

pub fn float_mod(vx: f64, wx: f64) -> PyResult<f64> {
    if wx == 0.0 {
        raise!(ZeroDivisionError, "float modulo by zero");
    }
    let mut m = vx % wx;
    if m != 0.0 {
        if (wx < 0.0) != (m < 0.0) {
            m += wx;
        }
    } else {
        m = 0.0f64.copysign(wx);
    }
    Ok(m)
}

/// PyLong_FromDouble (truncating).
pub fn float_to_int(x: f64) -> PyResult {
    if x.is_nan() {
        raise!(ValueError, "cannot convert float NaN to integer");
    }
    if x.is_infinite() {
        raise!(OverflowError, "cannot convert float infinity to integer");
    }
    if x.abs() < 9.2e18 {
        return Ok(int::new_int(x.trunc() as i64));
    }
    match rpy_bigint::conv::from_f64(x) {
        Ok(b) => Ok(crate::types::int_methods::from_big(b)),
        Err(e) => Err(crate::types::int_methods::int_error(e)),
    }
}

/// Map a str's code points to ASCII for numeric parsing
/// (CPython's _PyUnicode_TransformDecimalAndSpaceToASCII).
pub fn transform_decimal_and_space(o: &PyObject) -> Vec<u8> {
    let d = pystr::str_data(o);
    let mut v = Vec::with_capacity(d.len());
    for c in d.iter() {
        if c < 128 {
            v.push(c as u8);
        } else if rpy_unicode::is_space(c) {
            v.push(b' ');
        } else if let Some(dg) = rpy_unicode::to_decimal(c) {
            v.push(b'0' + dg as u8);
        } else {
            v.push(b'?');
            break;
        }
    }
    v
}

/// The ASCII text of a str/bytes/bytearray/buffer argument for float()/complex() parsing.
pub fn numeric_text(o: &PyObject) -> Option<Vec<u8>> {
    if pystr::is_str(o) {
        return Some(transform_decimal_and_space(o));
    }
    if crate::types::bytes::is_bytes(o) || crate::types::bytearray::is_bytearray(o) {
        return crate::types::bytes::bytes_like_data(o);
    }
    crate::buffer::buffer_bytes(o)
}

/// PyFloat_FromString semantics on a value: str, bytes, bytearray or buffer.
pub fn float_from_string(o: &PyObject) -> PyResult<f64> {
    let text = match numeric_text(o) {
        Some(t) => t,
        None => raise!(TypeError, "float() argument must be a string or a real number, not '{}'", tname(o)),
    };
    match rpy_numfmt::parse_float_bytes(&text) {
        Ok(v) => Ok(v),
        Err(_) => raise!(ValueError, "could not convert string to float: {}", ab::repr_string(o)?),
    }
}

/// PyNumber_Float, returning the value.
pub fn number_float(o: &PyObject) -> PyResult<f64> {
    number_float_obj(o).map(|f| float::float_value(&f))
}

/// PyNumber_Float: returns an exact float.
pub fn number_float_obj(o: &PyObject) -> PyResult {
    if float::is_float_exact(o) {
        return Ok(o.to_ref());
    }
    unsafe {
        let nb = o.ty().tp_as_number;
        if !nb.is_null() {
            if let Some(f) = (*nb).nb_float {
                let r = from_c(f(o.as_ptr()))?;
                if float::is_float_exact(&r) {
                    return Ok(r);
                }
                if !float::is_float(&r) {
                    raise!(TypeError, "{}.__float__ returned non-float (type {})", tname(o), tname(&r));
                }
                crate::modules::warnings::warn(
                    crate::types::exceptions::DeprecationWarning(),
                    &format!(
                        "{}.__float__ returned non-float (type {}).  The ability to return an instance of a strict subclass of float is deprecated, and may be removed in a future version of Python.",
                        tname(o),
                        tname(&r)
                    ),
                    1,
                )?;
                return Ok(float::new_float(float::float_value(&r)));
            }
            if (*nb).nb_index.is_some() {
                let i = ab::index(o)?;
                return Ok(float::new_float(int::to_f64(&i)?));
            }
        }
    }
    if float::is_float(o) {
        return Ok(float::new_float(float::float_value(o)));
    }
    float_from_string(o).map(float::new_float)
}

/// PyFloat_AsDouble for builtins that require a real number (math functions etc.).
pub fn as_double(o: &PyObject) -> PyResult<f64> {
    if float::is_float(o) {
        return Ok(float::float_value(o));
    }
    if int::is_int(o) && int::is_int_exact(o) {
        return int::to_f64(o);
    }
    ab::float_value(o)
}

#[allow(dead_code)]
fn _unused(_: PyRef) {}
