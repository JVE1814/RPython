//! The `math` module (and its `math.integer` submodule).

use crate::abstract_ as ab;
use crate::args::{self, Params};
use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::*;
use crate::raise;
use crate::types::int_methods::{from_big, int_error, iref};
use crate::types::typeobject::tname;
use crate::types::{bool_, float, int, none, str as pystr, tuple};

#[link(name = "m")]
unsafe extern "C" {
    fn erf(x: f64) -> f64;
    fn erfc(x: f64) -> f64;
    fn tgamma(x: f64) -> f64;
    fn lgamma_r(x: f64, sign: *mut i32) -> f64;
    fn ldexp(x: f64, e: i32) -> f64;
    fn frexp(x: f64, e: *mut i32) -> f64;
    fn nextafter(x: f64, y: f64) -> f64;
    fn atanh(x: f64) -> f64;
    fn asinh(x: f64) -> f64;
    fn acosh(x: f64) -> f64;
}

// the C library's inverse hyperbolic functions are accurate near the poles
// (std's formulas lose most digits for atanh close to +-1)
fn c_atanh(x: f64) -> f64 {
    unsafe { atanh(x) }
}
fn c_asinh(x: f64) -> f64 {
    unsafe { asinh(x) }
}
fn c_acosh(x: f64) -> f64 {
    unsafe { acosh(x) }
}

pub fn c_erf(x: f64) -> f64 {
    unsafe { erf(x) }
}
pub fn c_erfc(x: f64) -> f64 {
    unsafe { erfc(x) }
}
pub fn c_ldexp(x: f64, e: i32) -> f64 {
    unsafe { ldexp(x, e) }
}
pub fn c_frexp(x: f64) -> (f64, i32) {
    let mut e = 0;
    let m = unsafe { frexp(x, &mut e) };
    (m, e)
}
pub fn c_nextafter(x: f64, y: f64) -> f64 {
    unsafe { nextafter(x, y) }
}

// ------------------------------------------------------------------ error helpers

fn frepr(x: f64) -> String {
    rpy_numfmt::float_repr(x)
}

fn domain_err(msg: Option<&str>, x: f64) -> PyErr {
    match msg {
        Some(m) => crate::errors::value_error(&m.replace("%s", &frepr(x))),
        None => crate::errors::value_error("math domain error"),
    }
}
fn range_err() -> PyErr {
    crate::errors::overflow_error("math range error")
}

/// PyFloat_AsDouble
#[inline]
pub fn to_f64(o: &PyObject) -> PyResult<f64> {
    if float::is_float(o) {
        return Ok(float::float_value(o));
    }
    ab::float_value(o)
}

/// ASSIGN_DOUBLE as used by hypot/dist: exact floats, ints (with overflow), else __float__.
#[inline]
fn assign_double(o: &PyObject) -> PyResult<f64> {
    if float::is_float_exact(o) {
        return Ok(float::float_value(o));
    }
    if int::is_int_exact(o) {
        return int::to_f64(o);
    }
    ab::float_value(o)
}

fn fl(x: f64) -> PyResult {
    Ok(float::new_float(x))
}

/// math_1: apply f; NaN from non-NaN is a domain error, inf from finite is a range error
/// (OverflowError) if `can_overflow`, else a domain error.
fn math_1(arg: &PyObject, f: fn(f64) -> f64, can_overflow: bool, msg: Option<&str>) -> PyResult {
    let x = to_f64(arg)?;
    let r = f(x);
    if r.is_nan() && !x.is_nan() {
        return Err(domain_err(msg, x));
    }
    if r.is_infinite() && x.is_finite() {
        if can_overflow {
            return Err(range_err());
        }
        return Err(domain_err(msg, x));
    }
    fl(r)
}

/// math_2: binary function with domain/range checks.
fn math_2(a: &[&PyObject], name: &str, f: fn(f64, f64) -> f64) -> PyResult {
    if a.len() != 2 {
        raise!(TypeError, "{} expected 2 arguments, got {}", name, a.len());
    }
    let x = to_f64(a[0])?;
    let y = to_f64(a[1])?;
    let r = f(x, y);
    if r.is_nan() {
        if !x.is_nan() && !y.is_nan() {
            return Err(domain_err(None, x));
        }
    } else if r.is_infinite() && x.is_finite() && y.is_finite() {
        return Err(range_err());
    }
    fl(r)
}

macro_rules! f1 {
    ($name:ident, $f:expr, $ovf:expr, $msg:expr) => {
        fn $name(_m: &PyObject, x: &PyObject) -> PyResult {
            math_1(x, $f, $ovf, $msg)
        }
    };
}

f1!(m_acos, f64::acos, false, Some("expected a number in range from -1 up to 1, got %s"));
f1!(m_acosh, c_acosh, false, Some("expected argument value not less than 1, got %s"));
f1!(m_asin, f64::asin, false, Some("expected a number in range from -1 up to 1, got %s"));
f1!(m_asinh, c_asinh, false, None);
f1!(m_atan, f64::atan, false, None);
f1!(m_atanh, c_atanh, false, Some("expected a number between -1 and 1, got %s"));
f1!(m_cbrt, f64::cbrt, false, None);
f1!(m_cos, f64::cos, false, Some("expected a finite input, got %s"));
f1!(m_cosh, f64::cosh, true, None);
f1!(m_erf, c_erf, false, None);
f1!(m_erfc, c_erfc, false, None);
f1!(m_exp, f64::exp, true, None);
f1!(m_exp2, f64::exp2, true, None);
f1!(m_expm1, f64::exp_m1, true, None);
f1!(m_fabs, f64::abs, false, None);
f1!(m_log1p, f64::ln_1p, false, Some("expected argument value > -1, got %s"));
f1!(m_sin, f64::sin, false, Some("expected a finite input, got %s"));
f1!(m_sinh, f64::sinh, true, None);
f1!(m_sqrt, f64::sqrt, false, Some("expected a nonnegative input, got %s"));
f1!(m_tan, f64::tan, false, Some("expected a finite input, got %s"));
f1!(m_tanh, f64::tanh, false, None);

// ------------------------------------------------------------------ gamma family

const GAMMA_MSG: &str = "expected a noninteger or positive integer, got %s";

pub fn gamma_value(x: f64) -> Result<f64, bool> {
    // Err(false) = domain error, Err(true) = range error
    if x.is_nan() {
        return Ok(x);
    }
    if x.is_infinite() {
        return if x > 0.0 { Ok(x) } else { Err(false) };
    }
    if x == 0.0 || (x < 0.0 && x == x.floor()) {
        return Err(false);
    }
    let r = unsafe { tgamma(x) };
    if r.is_infinite() {
        return Err(true);
    }
    Ok(r)
}

pub fn lgamma_value(x: f64) -> Result<f64, bool> {
    if x.is_nan() {
        return Ok(x);
    }
    if x.is_infinite() {
        return Ok(f64::INFINITY);
    }
    if x <= 0.0 && x == x.floor() {
        return Err(false);
    }
    let mut s = 0;
    let r = unsafe { lgamma_r(x, &mut s) };
    if r.is_infinite() {
        return Err(true);
    }
    Ok(r)
}

fn m_gamma(_m: &PyObject, x: &PyObject) -> PyResult {
    let v = to_f64(x)?;
    match gamma_value(v) {
        Ok(r) => fl(r),
        Err(false) => Err(domain_err(Some(GAMMA_MSG), v)),
        Err(true) => Err(range_err()),
    }
}
fn m_lgamma(_m: &PyObject, x: &PyObject) -> PyResult {
    let v = to_f64(x)?;
    match lgamma_value(v) {
        Ok(r) => fl(r),
        Err(false) => Err(domain_err(Some(GAMMA_MSG), v)),
        Err(true) => Err(range_err()),
    }
}

// ------------------------------------------------------------------ rounding

fn m_floor(_m: &PyObject, x: &PyObject) -> PyResult {
    if float::is_float_exact(x) {
        return crate::types::float_math::float_to_int(float::float_value(x).floor());
    }
    if int::is_int_exact(x) {
        return Ok(x.to_ref());
    }
    if let Some(f) = ab::lookup_special(x, crate::name!("__floor__"))? {
        return ab::call0(&f);
    }
    let v = to_f64(x)?;
    crate::types::float_math::float_to_int(v.floor())
}
fn m_ceil(_m: &PyObject, x: &PyObject) -> PyResult {
    if float::is_float_exact(x) {
        return crate::types::float_math::float_to_int(float::float_value(x).ceil());
    }
    if int::is_int_exact(x) {
        return Ok(x.to_ref());
    }
    if let Some(f) = ab::lookup_special(x, crate::name!("__ceil__"))? {
        return ab::call0(&f);
    }
    let v = to_f64(x)?;
    crate::types::float_math::float_to_int(v.ceil())
}
fn m_trunc(_m: &PyObject, x: &PyObject) -> PyResult {
    if float::is_float_exact(x) {
        return crate::types::float_math::float_to_int(float::float_value(x));
    }
    match ab::lookup_special(x, crate::name!("__trunc__"))? {
        Some(f) => ab::call0(&f),
        None => raise!(TypeError, "type {} doesn't define __trunc__ method", tname(x)),
    }
}

// ------------------------------------------------------------------ two-argument functions

fn m_atan2(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    math_2(a, "atan2", f64::atan2)
}
fn m_copysign(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    math_2(a, "copysign", f64::copysign)
}
fn fmod_value(x: f64, y: f64) -> f64 {
    if y.is_infinite() && x.is_finite() {
        return x;
    }
    x % y
}
fn m_fmod(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    math_2(a, "fmod", fmod_value)
}
pub fn remainder_value(x: f64, y: f64) -> f64 {
    if x.is_finite() && y.is_finite() {
        if y == 0.0 {
            return f64::NAN;
        }
        let absx = x.abs();
        let absy = y.abs();
        let m = absx % absy;
        let c = absy - m;
        let r = if m < c {
            m
        } else if m > c {
            -c
        } else {
            m - 2.0 * ((0.5 * (absx - m)) % absy)
        };
        return 1.0f64.copysign(x) * r;
    }
    if x.is_nan() {
        return x;
    }
    if y.is_nan() {
        return y;
    }
    if x.is_infinite() {
        return f64::NAN;
    }
    x
}
fn m_remainder(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    math_2(a, "remainder", remainder_value)
}
fn m_fmax(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("fmax", a, 2, 2)?;
    let x = to_f64(a[0])?;
    let y = to_f64(a[1])?;
    fl(x.max(y))
}
fn m_fmin(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("fmin", a, 2, 2)?;
    let x = to_f64(a[0])?;
    let y = to_f64(a[1])?;
    fl(x.min(y))
}

/// math.pow semantics (Ok(value) or Err(is_range)).
pub fn pow_value(x: f64, y: f64) -> Result<f64, bool> {
    if !x.is_finite() || !y.is_finite() {
        let r = if x.is_nan() {
            if y == 0.0 { 1.0 } else { x }
        } else if y.is_nan() {
            if x == 1.0 { 1.0 } else { y }
        } else if x.is_infinite() {
            let odd_y = y.is_finite() && (y.abs() % 2.0) == 1.0;
            if y > 0.0 {
                if odd_y { x } else { x.abs() }
            } else if y == 0.0 {
                1.0
            } else if odd_y {
                0.0f64.copysign(x)
            } else {
                0.0
            }
        } else {
            // y infinite
            if x.abs() == 1.0 {
                1.0
            } else if y > 0.0 && x.abs() > 1.0 {
                y
            } else if y < 0.0 && x.abs() < 1.0 {
                -y
            } else {
                0.0
            }
        };
        return Ok(r);
    }
    let r = x.powf(y);
    if !r.is_finite() {
        if r.is_nan() {
            return Err(false);
        }
        if x == 0.0 {
            return Err(false);
        }
        return Err(true);
    }
    Ok(r)
}
fn m_pow(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("pow", a, 2, 2)?;
    let x = to_f64(a[0])?;
    let y = to_f64(a[1])?;
    match pow_value(x, y) {
        Ok(r) => fl(r),
        Err(false) => Err(domain_err(None, x)),
        Err(true) => Err(range_err()),
    }
}

// ------------------------------------------------------------------ logarithms

fn loghelper(arg: &PyObject, f: fn(f64) -> f64) -> PyResult<f64> {
    if int::is_int(arg) {
        if int::sign(arg) <= 0 {
            raise!(ValueError, "expected a positive input");
        }
        match int::to_f64(arg) {
            Ok(x) => return Ok(f(x)),
            Err(_) => {
                if !exc_matches(crate::types::exceptions::OverflowError()) {
                    return Err(PyErr);
                }
                clear_exc();
                let (m, e) = rpy_bigint::conv::frexp(iref(arg));
                return Ok(f(m) + f(2.0) * e as f64);
            }
        }
    }
    let x = match to_f64(arg) {
        Ok(x) => x,
        // too large for a float: an integer-like object takes the exact path above
        Err(_) if !int::is_int(arg) && exc_matches(crate::types::exceptions::OverflowError()) && ab::has_index(arg) => {
            clear_exc();
            let i = ab::index(arg)?;
            return loghelper(&i, f);
        }
        Err(e) => return Err(e),
    };
    let r = f(x);
    if (r.is_nan() && !x.is_nan()) || (r.is_infinite() && x.is_finite()) || (x.is_infinite() && x < 0.0) {
        return Err(domain_err(Some("expected a positive input, got %s"), x));
    }
    Ok(r)
}
fn m_log(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("log", a, 1, 2)?;
    let num = loghelper(a[0], f64::ln)?;
    if a.len() == 1 {
        return fl(num);
    }
    let den = loghelper(a[1], f64::ln)?;
    if den == 0.0 {
        raise!(ZeroDivisionError, "division by zero");
    }
    fl(num / den)
}
fn m_log2(_m: &PyObject, x: &PyObject) -> PyResult {
    fl(loghelper(x, f64::log2)?)
}
fn m_log10(_m: &PyObject, x: &PyObject) -> PyResult {
    fl(loghelper(x, f64::log10)?)
}

// ------------------------------------------------------------------ misc float functions

fn m_degrees(_m: &PyObject, x: &PyObject) -> PyResult {
    fl(to_f64(x)? * (180.0 / std::f64::consts::PI))
}
fn m_radians(_m: &PyObject, x: &PyObject) -> PyResult {
    fl(to_f64(x)? * (std::f64::consts::PI / 180.0))
}
fn m_isfinite(_m: &PyObject, x: &PyObject) -> PyResult {
    Ok(bool_::new_bool(to_f64(x)?.is_finite()))
}
fn m_isinf(_m: &PyObject, x: &PyObject) -> PyResult {
    Ok(bool_::new_bool(to_f64(x)?.is_infinite()))
}
fn m_isnan(_m: &PyObject, x: &PyObject) -> PyResult {
    Ok(bool_::new_bool(to_f64(x)?.is_nan()))
}
fn m_isnormal(_m: &PyObject, x: &PyObject) -> PyResult {
    Ok(bool_::new_bool(to_f64(x)?.is_normal()))
}
fn m_issubnormal(_m: &PyObject, x: &PyObject) -> PyResult {
    Ok(bool_::new_bool(to_f64(x)?.is_subnormal()))
}
fn m_signbit(_m: &PyObject, x: &PyObject) -> PyResult {
    Ok(bool_::new_bool(to_f64(x)?.is_sign_negative()))
}
fn m_frexp(_m: &PyObject, x: &PyObject) -> PyResult {
    let v = to_f64(x)?;
    let (m, e) = if v.is_nan() || v.is_infinite() || v == 0.0 { (v, 0) } else { c_frexp(v) };
    Ok(tuple::new_tuple(vec![float::new_float(m), int::new_int(e as i64)]))
}
fn m_modf(_m: &PyObject, x: &PyObject) -> PyResult {
    let v = to_f64(x)?;
    let (f, i) = if v.is_infinite() {
        (0.0f64.copysign(v), v)
    } else if v.is_nan() {
        (v, v)
    } else {
        let i = v.trunc();
        ((v - i).copysign(v), i)
    };
    Ok(tuple::new_tuple(vec![float::new_float(f), float::new_float(i)]))
}
fn m_ldexp(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("ldexp", a, 2, 2)?;
    let x = to_f64(a[0])?;
    let i = a[1];
    if !int::is_int(i) {
        raise!(TypeError, "Expected an int as second argument to ldexp.");
    }
    let exp: i64 = match int::as_i64(i) {
        Some(v) => v,
        None => {
            if int::sign(i) < 0 {
                i64::MIN
            } else {
                i64::MAX
            }
        }
    };
    let r = if x == 0.0 || !x.is_finite() {
        x
    } else if exp > i32::MAX as i64 {
        return Err(range_err());
    } else if exp < i32::MIN as i64 {
        0.0f64.copysign(x)
    } else {
        let r = c_ldexp(x, exp as i32);
        if r.is_infinite() {
            return Err(range_err());
        }
        r
    };
    fl(r)
}
fn m_ulp(_m: &PyObject, x: &PyObject) -> PyResult {
    let v = to_f64(x)?;
    if v.is_nan() {
        return fl(v);
    }
    let v = v.abs();
    if v.is_infinite() {
        return fl(v);
    }
    let x2 = c_nextafter(v, f64::INFINITY);
    if x2.is_infinite() {
        let x2 = c_nextafter(v, f64::NEG_INFINITY);
        return fl(v - x2);
    }
    fl(x2 - v)
}
fn m_nextafter(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("nextafter", &["x", "y", "steps"], 2, 2).kwonly(1);
    let [x, y, steps] = args::parse::<3>(&P, a, kw)?;
    let x = to_f64(x.unwrap())?;
    let y = to_f64(y.unwrap())?;
    let steps = match steps {
        None => return fl(c_nextafter(x, y)),
        Some(s) if s.is(none::none()) => return fl(c_nextafter(x, y)),
        Some(s) => s,
    };
    let s = ab::index(steps)?;
    if int::sign(&s) < 0 {
        raise!(ValueError, "steps must be a non-negative integer");
    }
    let usteps = rpy_bigint::conv::to_u64(iref(&s)).unwrap_or(u64::MAX);
    if usteps == 0 {
        return fl(x);
    }
    if x.is_nan() {
        return fl(x);
    }
    if y.is_nan() {
        return fl(y);
    }
    let mut ux = x.to_bits();
    let uy = y.to_bits();
    if ux == uy {
        return fl(x);
    }
    const SIGN: u64 = 1 << 63;
    let ax = ux & !SIGN;
    let ay = uy & !SIGN;
    if (ux ^ uy) & SIGN != 0 {
        if ax + ay <= usteps {
            return fl(y);
        } else if ax < usteps {
            return fl(f64::from_bits((uy & SIGN) | (usteps - ax)));
        } else {
            ux -= usteps;
            return fl(f64::from_bits(ux));
        }
    } else if ax > ay {
        if ax - ay >= usteps {
            ux -= usteps;
            return fl(f64::from_bits(ux));
        }
        fl(y)
    } else {
        if ay - ax >= usteps {
            ux += usteps;
            return fl(f64::from_bits(ux));
        }
        fl(y)
    }
}
fn m_fma(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("fma", a, 3, 3)?;
    let x = to_f64(a[0])?;
    let y = to_f64(a[1])?;
    let z = to_f64(a[2])?;
    let r = x.mul_add(y, z);
    if r.is_finite() {
        return fl(r);
    }
    if r.is_nan() {
        if !x.is_nan() && !y.is_nan() && !z.is_nan() {
            raise!(ValueError, "invalid operation in fma");
        }
    } else if x.is_finite() && y.is_finite() && z.is_finite() {
        raise!(OverflowError, "overflow in fma");
    }
    fl(r)
}
fn m_isclose(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("isclose", &["a", "b", "rel_tol", "abs_tol"], 2, 2).kwonly(2);
    let [x, y, rt, at] = args::parse::<4>(&P, a, kw)?;
    let x = to_f64(x.unwrap())?;
    let y = to_f64(y.unwrap())?;
    let rel_tol = match rt {
        Some(v) => to_f64(v)?,
        None => 1e-9,
    };
    let abs_tol = match at {
        Some(v) => to_f64(v)?,
        None => 0.0,
    };
    if rel_tol < 0.0 || abs_tol < 0.0 {
        raise!(ValueError, "tolerances must be non-negative");
    }
    if x == y {
        return Ok(bool_::new_bool(true));
    }
    if x.is_infinite() || y.is_infinite() {
        return Ok(bool_::new_bool(false));
    }
    let diff = (y - x).abs();
    Ok(bool_::new_bool(diff <= (rel_tol * y).abs() || diff <= (rel_tol * x).abs() || diff <= abs_tol))
}

// ------------------------------------------------------------------ hypot / dist

#[inline]
fn dl_mul(x: f64, y: f64) -> (f64, f64) {
    let hi = x * y;
    let lo = x.mul_add(y, -hi);
    (hi, lo)
}
#[inline]
fn dl_fast_sum(a: f64, b: f64) -> (f64, f64) {
    let hi = a + b;
    let lo = (a - hi) + b;
    (hi, lo)
}

/// Accurate Euclidean norm of non-negative finite values (CPython's vector_norm).
pub fn vector_norm(vec: &mut [f64], max: f64, found_nan: bool) -> f64 {
    let n = vec.len();
    if max.is_infinite() {
        return max;
    }
    if found_nan {
        return f64::NAN;
    }
    if max == 0.0 || n <= 1 {
        return max;
    }
    let (_, max_e) = c_frexp(max);
    if max_e < -1023 {
        for v in vec.iter_mut() {
            *v /= f64::MIN_POSITIVE;
        }
        return f64::MIN_POSITIVE * vector_norm(vec, max / f64::MIN_POSITIVE, found_nan);
    }
    let scale = c_ldexp(1.0, -max_e);
    let mut csum = 1.0;
    let mut frac1 = 0.0;
    let mut frac2 = 0.0;
    for &v in vec.iter() {
        let x = v * scale;
        let pr = dl_mul(x, x);
        let sm = dl_fast_sum(csum, pr.0);
        csum = sm.0;
        frac1 += pr.1;
        frac2 += sm.1;
    }
    let mut h = (csum - 1.0 + (frac1 + frac2)).sqrt();
    let pr = dl_mul(-h, h);
    let sm = dl_fast_sum(csum, pr.0);
    csum = sm.0;
    frac1 += pr.1;
    frac2 += sm.1;
    let x = csum - 1.0 + (frac1 + frac2);
    h += x / (2.0 * h);
    h / scale
}

fn m_hypot(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    let mut v = Vec::with_capacity(a.len());
    let mut max = 0.0f64;
    let mut found_nan = false;
    for o in a {
        let x = assign_double(o)?.abs();
        found_nan |= x.is_nan();
        if x > max {
            max = x;
        }
        v.push(x);
    }
    fl(vector_norm(&mut v, max, found_nan))
}

fn m_dist(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("dist", a, 2, 2)?;
    let p = if tuple::is_tuple(a[0]) { a[0].to_ref() } else { ab::sequence_tuple(a[0])? };
    let q = if tuple::is_tuple(a[1]) { a[1].to_ref() } else { ab::sequence_tuple(a[1])? };
    let pi = tuple::tuple_items(&p);
    let qi = tuple::tuple_items(&q);
    if pi.len() != qi.len() {
        raise!(ValueError, "both points must have the same number of dimensions");
    }
    let mut v = Vec::with_capacity(pi.len());
    let mut max = 0.0f64;
    let mut found_nan = false;
    for (x, y) in pi.iter().zip(qi.iter()) {
        let px = assign_double(x)?;
        let qx = assign_double(y)?;
        let d = (px - qx).abs();
        found_nan |= d.is_nan();
        if d > max {
            max = d;
        }
        v.push(d);
    }
    fl(vector_norm(&mut v, max, found_nan))
}

// ------------------------------------------------------------------ fsum

/// Exact sum of partials (Shewchuk), correctly rounded. Errors as CPython's fsum.
pub struct FSum {
    p: Vec<f64>,
    special_sum: f64,
    inf_sum: f64,
}
impl FSum {
    pub fn new() -> Self {
        FSum { p: Vec::new(), special_sum: 0.0, inf_sum: 0.0 }
    }
    /// Returns Err(()) on intermediate overflow.
    pub fn add(&mut self, xin: f64) -> Result<(), ()> {
        let xsave = xin;
        let mut x = xin;
        let mut i = 0;
        let n = self.p.len();
        for j in 0..n {
            let mut y = self.p[j];
            if x.abs() < y.abs() {
                std::mem::swap(&mut x, &mut y);
            }
            let hi = x + y;
            let yr = hi - x;
            let lo = y - yr;
            if lo != 0.0 {
                self.p[i] = lo;
                i += 1;
            }
            x = hi;
        }
        self.p.truncate(i);
        if x != 0.0 {
            if !x.is_finite() {
                if xsave.is_finite() {
                    return Err(());
                }
                if xsave.is_infinite() {
                    self.inf_sum += xsave;
                }
                self.special_sum += xsave;
                self.p.clear();
            } else {
                self.p.push(x);
            }
        }
        Ok(())
    }
    /// Ok(sum) or Err(()) for "-inf + inf".
    pub fn result(&self) -> Result<f64, ()> {
        if self.special_sum != 0.0 {
            if self.inf_sum.is_nan() {
                return Err(());
            }
            return Ok(self.special_sum);
        }
        let p = &self.p;
        let mut n = p.len();
        let mut hi = 0.0;
        if n > 0 {
            n -= 1;
            hi = p[n];
            let mut lo = 0.0;
            while n > 0 {
                let x = hi;
                n -= 1;
                let y = p[n];
                hi = x + y;
                let yr = hi - x;
                lo = y - yr;
                if lo != 0.0 {
                    break;
                }
            }
            if n > 0 && ((lo < 0.0 && p[n - 1] < 0.0) || (lo > 0.0 && p[n - 1] > 0.0)) {
                let y = lo * 2.0;
                let x = hi + y;
                let yr = x - hi;
                if y == yr {
                    hi = x;
                }
            }
        }
        Ok(hi)
    }
}

fn m_fsum(_m: &PyObject, seq: &PyObject) -> PyResult {
    let it = ab::get_iter(seq)?;
    let mut s = FSum::new();
    while let Some(item) = ab::iter_next(&it)? {
        let x = to_f64(&item)?;
        if s.add(x).is_err() {
            raise!(OverflowError, "intermediate overflow in fsum");
        }
    }
    match s.result() {
        Ok(v) => fl(v),
        Err(()) => raise!(ValueError, "-inf + inf in fsum"),
    }
}

// ------------------------------------------------------------------ prod / sumprod

fn mul(a: &PyObject, b: &PyObject) -> PyResult {
    ab::binary_op(a, b, ab::NbSlot::Mul)
}
fn add(a: &PyObject, b: &PyObject) -> PyResult {
    ab::binary_op(a, b, ab::NbSlot::Add)
}

fn m_prod(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("prod", &["iterable", "start"], 1, 1).kwonly(1);
    let [iterable, start] = args::parse::<2>(&P, a, kw)?;
    let it = ab::get_iter(iterable.unwrap())?;
    let mut result = match start {
        Some(s) => s.to_ref(),
        None => int::new_int(1),
    };
    if int::is_int_exact(&result) {
        if let Some(mut acc) = int::as_i64(&result) {
            loop {
                let item = match ab::iter_next(&it)? {
                    Some(x) => x,
                    None => return Ok(int::new_int(acc)),
                };
                if int::is_int_exact(&item) || bool_::is_bool(&item) {
                    if let Some(b) = int::as_i64(&item) {
                        if let Some(p) = acc.checked_mul(b) {
                            acc = p;
                            continue;
                        }
                    }
                }
                result = mul(&int::new_int(acc), &item)?;
                break;
            }
        }
    }
    if float::is_float_exact(&result) {
        let mut f = float::float_value(&result);
        loop {
            let item = match ab::iter_next(&it)? {
                Some(x) => x,
                None => return fl(f),
            };
            if float::is_float_exact(&item) {
                f *= float::float_value(&item);
                continue;
            }
            if int::is_int_exact(&item) || bool_::is_bool(&item) {
                if let Some(v) = int::as_i64(&item) {
                    if v.unsigned_abs() < (1u64 << 53) {
                        f *= v as f64;
                        continue;
                    }
                }
            }
            result = mul(&float::new_float(f), &item)?;
            break;
        }
    }
    while let Some(item) = ab::iter_next(&it)? {
        result = mul(&result, &item)?;
    }
    Ok(result)
}

/// Exact dot-product accumulator: products split by FMA and summed with fsum partials.
struct ExactDot {
    s: FSum,
    in_use: bool,
}
impl ExactDot {
    fn try_add(&mut self, x: f64, y: f64) -> bool {
        let (hi, lo) = dl_mul(x, y);
        if !hi.is_finite() {
            return false;
        }
        let saved = self.s.p.clone();
        if self.s.add(hi).is_err() || self.s.add(lo).is_err() || self.s.special_sum != 0.0 {
            self.s.p = saved;
            self.s.special_sum = 0.0;
            self.s.inf_sum = 0.0;
            return false;
        }
        match self.s.result() {
            Ok(v) if v.is_finite() => {}
            _ => {
                self.s.p = saved;
                return false;
            }
        }
        self.in_use = true;
        true
    }
    fn take(&mut self) -> f64 {
        let v = self.s.result().unwrap_or(f64::NAN);
        self.s = FSum::new();
        self.in_use = false;
        v
    }
}

fn is_exact_int(o: &PyObject) -> bool {
    int::is_int_exact(o)
}
fn float_or_int_as_double(o: &PyObject) -> Option<f64> {
    if int::is_int_exact(o) || bool_::is_bool(o) {
        let r = int::to_f64(o);
        if r.is_err() {
            clear_exc();
        }
        return r.ok();
    }
    None
}

fn m_sumprod(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("sumprod", a, 2, 2)?;
    let p_it = ab::get_iter(a[0])?;
    let q_it = ab::get_iter(a[1])?;
    let mut total = int::new_int(0);
    let mut int_path = true;
    let mut int_total: i64 = 0;
    let mut int_in_use = false;
    let mut flt_path = true;
    let mut flt = ExactDot { s: FSum::new(), in_use: false };
    loop {
        let pi = ab::iter_next(&p_it)?;
        let qi = ab::iter_next(&q_it)?;
        let finished = match (&pi, &qi) {
            (None, None) => true,
            (Some(_), Some(_)) => false,
            _ => raise!(ValueError, "Inputs are not the same length"),
        };
        if int_path {
            if !finished {
                let (p, q) = (pi.as_ref().unwrap(), qi.as_ref().unwrap());
                if is_exact_int(p) && is_exact_int(q) {
                    if let (Some(x), Some(y)) = (int::as_i64(p), int::as_i64(q)) {
                        if let Some(s) = x.checked_mul(y).and_then(|pr| int_total.checked_add(pr)) {
                            int_total = s;
                            int_in_use = true;
                            continue;
                        }
                    }
                }
            }
            int_path = false;
            if int_in_use {
                total = add(&total, &int::new_int(int_total))?;
                int_total = 0;
                int_in_use = false;
            }
        }
        if flt_path {
            if !finished {
                let (p, q) = (pi.as_ref().unwrap(), qi.as_ref().unwrap());
                let pf = float::is_float_exact(p);
                let qf = float::is_float_exact(q);
                let pair = if pf && qf {
                    Some((float::float_value(p), float::float_value(q)))
                } else if pf {
                    float_or_int_as_double(q).map(|y| (float::float_value(p), y))
                } else if qf {
                    float_or_int_as_double(p).map(|x| (x, float::float_value(q)))
                } else {
                    None
                };
                if let Some((x, y)) = pair {
                    if flt.try_add(x, y) {
                        continue;
                    }
                }
            }
            flt_path = false;
            if flt.in_use {
                let v = flt.take();
                total = add(&total, &float::new_float(v))?;
            }
        }
        if finished {
            break;
        }
        let term = mul(pi.as_ref().unwrap(), qi.as_ref().unwrap())?;
        total = add(&total, &term)?;
    }
    let _ = int_total;
    Ok(total)
}

// ------------------------------------------------------------------ integer functions

fn m_gcd(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    if a.is_empty() {
        return Ok(int::new_int(0));
    }
    let first = ab::index(a[0])?;
    let mut res = rpy_bigint::ops::abs(iref(&first));
    for x in &a[1..] {
        let x = ab::index(x)?;
        if res.mag.len() == 1 && res.mag[0] == 1 {
            continue;
        }
        res = rpy_bigint::math::gcd(res.as_ref(), iref(&x));
    }
    Ok(from_big(res))
}
fn m_lcm(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    if a.is_empty() {
        return Ok(int::new_int(1));
    }
    let first = ab::index(a[0])?;
    let mut res = rpy_bigint::ops::abs(iref(&first));
    for x in &a[1..] {
        let x = ab::index(x)?;
        if res.mag.is_empty() {
            continue;
        }
        res = rpy_bigint::math::lcm(res.as_ref(), iref(&x));
    }
    Ok(from_big(res))
}
fn m_isqrt(_m: &PyObject, n: &PyObject) -> PyResult {
    let n = ab::index(n)?;
    rpy_bigint::math::isqrt(iref(&n)).map(from_big).map_err(int_error)
}
fn m_factorial(_m: &PyObject, n: &PyObject) -> PyResult {
    let n = ab::index(n)?;
    rpy_bigint::math::factorial(iref(&n)).map(from_big).map_err(int_error)
}
fn m_comb(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("comb", a, 2, 2)?;
    let n = ab::index(a[0])?;
    let k = ab::index(a[1])?;
    rpy_bigint::math::comb(iref(&n), iref(&k)).map(from_big).map_err(int_error)
}
fn m_perm(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("perm", a, 1, 2)?;
    let n = ab::index(a[0])?;
    if a.len() == 1 || a[1].is(none::none()) {
        if int::sign(&n) < 0 {
            raise!(ValueError, "n must be a non-negative integer");
        }
        return rpy_bigint::math::factorial(iref(&n)).map(from_big).map_err(int_error);
    }
    let k = ab::index(a[1])?;
    rpy_bigint::math::perm(iref(&n), iref(&k)).map(from_big).map_err(int_error)
}

// ------------------------------------------------------------------ module

fn integer_methods() -> *mut crate::ffi::PyMethodDef {
    crate::mdefs![
        ("comb", FAST, m_comb, "comb($module, n, k, /)\n--\n\nNumber of ways to choose k items from n items without repetition and without order.\n\nEvaluates to n! / (k! * (n - k)!) when k <= n and evaluates\nto zero when k > n.\n\nAlso called the binomial coefficient because it is equivalent\nto the coefficient of k-th term in polynomial expansion of the\nexpression (1 + x)**n.\n\nRaises TypeError if either of the arguments are not integers.\nRaises ValueError if either of the arguments are negative."),
        ("factorial", O, m_factorial, "factorial($module, n, /)\n--\n\nFind n!."),
        ("gcd", FAST, m_gcd, "gcd($module, /, *integers)\n--\n\nGreatest Common Divisor."),
        ("isqrt", O, m_isqrt, "isqrt($module, n, /)\n--\n\nReturn the integer part of the square root of the input."),
        ("lcm", FAST, m_lcm, "lcm($module, /, *integers)\n--\n\nLeast Common Multiple."),
        ("perm", FAST, m_perm, "perm($module, n, k=None, /)\n--\n\nNumber of ways to choose k items from n items without repetition and with order.\n\nEvaluates to n! / (n - k)! when k <= n and evaluates\nto zero when k > n.\n\nIf k is not specified or is None, then k defaults to n\nand the function returns n!.\n\nRaises TypeError if either of the arguments are not integers.\nRaises ValueError if either of the arguments are negative."),
    ]
}

pub fn init_math_integer() -> PyResult {
    let m = crate::types::module::new_module("math.integer")?;
    let d = crate::types::module::module_dict(&m);
    crate::types::dict::set_item_str(&d, "__doc__", &pystr::new_str("Integer specific mathematics functions"))?;
    crate::builtins::add_methods_to_dict(&d, integer_methods(), Some(&m))?;
    Ok(m)
}

pub fn init_math() -> PyResult {
    let m = crate::types::module::new_module("math")?;
    let d = crate::types::module::module_dict(&m);
    crate::types::dict::set_item_str(&d, "__doc__", &pystr::new_str("This module provides access to the mathematical functions\ndefined by the C standard."))?;
    let methods = crate::mdefs![
        ("acos", O, m_acos, "acos($module, x, /)\n--\n\nReturn the arc cosine (measured in radians) of x.\n\nThe result is between 0 and pi."),
        ("acosh", O, m_acosh, "acosh($module, x, /)\n--\n\nReturn the inverse hyperbolic cosine of x."),
        ("asin", O, m_asin, "asin($module, x, /)\n--\n\nReturn the arc sine (measured in radians) of x.\n\nThe result is between -pi/2 and pi/2."),
        ("asinh", O, m_asinh, "asinh($module, x, /)\n--\n\nReturn the inverse hyperbolic sine of x."),
        ("atan", O, m_atan, "atan($module, x, /)\n--\n\nReturn the arc tangent (measured in radians) of x.\n\nThe result is between -pi/2 and pi/2."),
        ("atan2", FAST, m_atan2, "atan2($module, y, x, /)\n--\n\nReturn the arc tangent (measured in radians) of y/x.\n\nUnlike atan(y/x), the signs of both x and y are considered."),
        ("atanh", O, m_atanh, "atanh($module, x, /)\n--\n\nReturn the inverse hyperbolic tangent of x."),
        ("cbrt", O, m_cbrt, "cbrt($module, x, /)\n--\n\nReturn the cube root of x."),
        ("ceil", O, m_ceil, "ceil($module, x, /)\n--\n\nReturn the ceiling of x as an Integral.\n\nThis is the smallest integer >= x."),
        ("copysign", FAST, m_copysign, "copysign($module, x, y, /)\n--\n\nReturn a float with the magnitude (absolute value) of x but the sign of y.\n\nOn platforms that support signed zeros, copysign(1.0, -0.0)\nreturns -1.0.\n"),
        ("cos", O, m_cos, "cos($module, x, /)\n--\n\nReturn the cosine of x (measured in radians)."),
        ("cosh", O, m_cosh, "cosh($module, x, /)\n--\n\nReturn the hyperbolic cosine of x."),
        ("degrees", O, m_degrees, "degrees($module, x, /)\n--\n\nConvert angle x from radians to degrees."),
        ("dist", FAST, m_dist, "dist($module, p, q, /)\n--\n\nReturn the Euclidean distance between two points p and q.\n\nThe points should be specified as sequences (or iterables) of\ncoordinates.  Both inputs must have the same dimension.\n\nRoughly equivalent to:\n    sqrt(sum((px - qx) ** 2.0 for px, qx in zip(p, q)))"),
        ("erf", O, m_erf, "erf($module, x, /)\n--\n\nError function at x."),
        ("erfc", O, m_erfc, "erfc($module, x, /)\n--\n\nComplementary error function at x."),
        ("exp", O, m_exp, "exp($module, x, /)\n--\n\nReturn e raised to the power of x."),
        ("exp2", O, m_exp2, "exp2($module, x, /)\n--\n\nReturn 2 raised to the power of x."),
        ("expm1", O, m_expm1, "expm1($module, x, /)\n--\n\nReturn exp(x)-1.\n\nThis function avoids the loss of precision involved in the direct evaluation of exp(x)-1 for small x."),
        ("fabs", O, m_fabs, "fabs($module, x, /)\n--\n\nReturn the absolute value of the float x."),
        ("floor", O, m_floor, "floor($module, x, /)\n--\n\nReturn the floor of x as an Integral.\n\nThis is the largest integer <= x."),
        ("fma", FAST, m_fma, "fma($module, x, y, z, /)\n--\n\nFused multiply-add operation.\n\nCompute (x * y) + z with a single round."),
        ("fmax", FAST, m_fmax, "fmax($module, x, y, /)\n--\n\nReturn the larger of two floating-point arguments."),
        ("fmin", FAST, m_fmin, "fmin($module, x, y, /)\n--\n\nReturn the smaller of two floating-point arguments."),
        ("fmod", FAST, m_fmod, "fmod($module, x, y, /)\n--\n\nReturn fmod(x, y), according to platform C.\n\nx % y may differ."),
        ("frexp", O, m_frexp, "frexp($module, x, /)\n--\n\nReturn the mantissa and exponent of x, as pair (m, e).\n\nm is a float and e is an int, such that x = m * 2.**e.\nIf x is 0, m and e are both 0.  Else 0.5 <= abs(m) < 1.0."),
        ("fsum", O, m_fsum, "fsum($module, seq, /)\n--\n\nReturn an accurate floating-point sum of values in the iterable seq.\n\nAssumes IEEE-754 floating-point arithmetic."),
        ("gamma", O, m_gamma, "gamma($module, x, /)\n--\n\nGamma function at x."),
        ("hypot", FAST, m_hypot, "hypot(*coordinates) -> value\n\nMultidimensional Euclidean distance from the origin to a point.\n\nRoughly equivalent to:\n    sqrt(sum(x**2 for x in coordinates))\n\nFor a two dimensional point (x, y), gives the hypotenuse\nusing the Pythagorean theorem:  sqrt(x*x + y*y).\n\nFor example, the hypotenuse of a 3/4/5 right triangle is:\n\n    >>> hypot(3.0, 4.0)\n    5.0\n"),
        ("isclose", FASTKW, m_isclose, "isclose($module, /, a, b, *, rel_tol=1e-09, abs_tol=0.0)\n--\n\nDetermine whether two floating-point numbers are close in value.\n\n  rel_tol\n    maximum difference for being considered \"close\", relative to the\n    magnitude of the input values\n  abs_tol\n    maximum difference for being considered \"close\", regardless of the\n    magnitude of the input values\n\nReturn True if a is close in value to b, and False otherwise.\n\nFor the values to be considered close, the difference between them\nmust be smaller than at least one of the tolerances.\n\n-inf, inf and NaN behave similarly to the IEEE 754 Standard.  That\nis, NaN is not close to anything, even itself.  inf and -inf are\nonly close to themselves."),
        ("isfinite", O, m_isfinite, "isfinite($module, x, /)\n--\n\nReturn True if x is neither an infinity nor a NaN, and False otherwise."),
        ("isinf", O, m_isinf, "isinf($module, x, /)\n--\n\nReturn True if x is a positive or negative infinity, and False otherwise."),
        ("isnan", O, m_isnan, "isnan($module, x, /)\n--\n\nReturn True if x is a NaN (not a number), and False otherwise."),
        ("isnormal", O, m_isnormal, "isnormal($module, x, /)\n--\n\nReturn True if x is normal, and False otherwise."),
        ("issubnormal", O, m_issubnormal, "issubnormal($module, x, /)\n--\n\nReturn True if x is subnormal, and False otherwise."),
        ("ldexp", FAST, m_ldexp, "ldexp($module, x, i, /)\n--\n\nReturn x * (2**i).\n\nThis is essentially the inverse of frexp()."),
        ("lgamma", O, m_lgamma, "lgamma($module, x, /)\n--\n\nNatural logarithm of absolute value of Gamma function at x."),
        ("log", FAST, m_log, "log(x, [base=math.e])\nReturn the logarithm of x to the given base.\n\nIf the base is not specified, returns the natural logarithm (base e) of x."),
        ("log10", O, m_log10, "log10($module, x, /)\n--\n\nReturn the base 10 logarithm of x."),
        ("log1p", O, m_log1p, "log1p($module, x, /)\n--\n\nReturn the natural logarithm of 1+x (base e).\n\nThe result is computed in a way which is accurate for x near zero."),
        ("log2", O, m_log2, "log2($module, x, /)\n--\n\nReturn the base 2 logarithm of x."),
        ("modf", O, m_modf, "modf($module, x, /)\n--\n\nReturn the fractional and integer parts of x.\n\nBoth results carry the sign of x and are floats."),
        ("nextafter", FASTKW, m_nextafter, "nextafter($module, x, y, /, *, steps=None)\n--\n\nReturn the floating-point value the given number of steps after x towards y.\n\nIf steps is not specified or is None, it defaults to 1.\n\nRaises a TypeError, if x or y is not a double, or if steps is not an integer.\nRaises ValueError if steps is negative."),
        ("pow", FAST, m_pow, "pow($module, x, y, /)\n--\n\nReturn x**y (x to the power of y)."),
        ("prod", FASTKW, m_prod, "prod($module, iterable, /, *, start=1)\n--\n\nCalculate the product of all the elements in the input iterable.\n\nThe default start value for the product is 1.\n\nWhen the iterable is empty, return the start value.  This function is\nintended specifically for use with numeric values and may reject\nnon-numeric types."),
        ("radians", O, m_radians, "radians($module, x, /)\n--\n\nConvert angle x from degrees to radians."),
        ("remainder", FAST, m_remainder, "remainder($module, x, y, /)\n--\n\nDifference between x and the closest integer multiple of y.\n\nReturn x - n*y where n*y is the closest integer multiple of y.\nIn the case where x is exactly halfway between two multiples of\ny, the nearest even value of n is used. The result is always exact."),
        ("signbit", O, m_signbit, "signbit($module, x, /)\n--\n\nReturn True if the sign of x is negative and False otherwise."),
        ("sin", O, m_sin, "sin($module, x, /)\n--\n\nReturn the sine of x (measured in radians)."),
        ("sinh", O, m_sinh, "sinh($module, x, /)\n--\n\nReturn the hyperbolic sine of x."),
        ("sqrt", O, m_sqrt, "sqrt($module, x, /)\n--\n\nReturn the square root of x."),
        ("sumprod", FAST, m_sumprod, "sumprod($module, p, q, /)\n--\n\nReturn the sum of products of values from two iterables p and q.\n\nRoughly equivalent to:\n\n    sum(map(operator.mul, p, q, strict=True))\n\nFor float and mixed int/float inputs, the intermediate products\nand sums are computed with extended precision."),
        ("tan", O, m_tan, "tan($module, x, /)\n--\n\nReturn the tangent of x (measured in radians)."),
        ("tanh", O, m_tanh, "tanh($module, x, /)\n--\n\nReturn the hyperbolic tangent of x."),
        ("trunc", O, m_trunc, "trunc($module, x, /)\n--\n\nTruncates the Real x to the nearest Integral toward 0.\n\nUses the __trunc__ magic method."),
        ("ulp", O, m_ulp, "ulp($module, x, /)\n--\n\nReturn the value of the least significant bit of the float x."),
    ];
    crate::builtins::add_methods_to_dict(&d, methods, Some(&m))?;
    crate::builtins::add_methods_to_dict(&d, integer_methods(), Some(&m))?;
    let set = |k: &str, v: f64| crate::types::dict::set_item_str(&d, k, &float::new_float(v));
    set("pi", std::f64::consts::PI)?;
    set("e", std::f64::consts::E)?;
    set("tau", std::f64::consts::TAU)?;
    set("inf", f64::INFINITY)?;
    set("nan", f64::NAN)?;
    // math.integer submodule
    let sub = match crate::import::get_loaded_module("math.integer") {
        Some(s) => s,
        None => {
            let s = init_math_integer()?;
            crate::types::dict::set_item_str(&crate::modules::sys::modules_dict(), "math.integer", &s)?;
            s
        }
    };
    crate::types::dict::set_item_str(&d, "integer", &sub)?;
    Ok(m)
}
