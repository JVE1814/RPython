//! The `cmath` module: complex elementary functions with C99 Annex G special values and
//! Kahan-style branch cuts.

use crate::args::{self, Params};
use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::*;
use crate::raise;
use crate::types::complex_methods::as_ccomplex;
use crate::types::{bool_, complex, float, str as pystr, tuple};
use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, LN_2, PI};

const INF: f64 = f64::INFINITY;
const NAN: f64 = f64::NAN;
const P34: f64 = 0.75 * PI;
const LARGE: f64 = f64::MAX / 4.0;
const SQRT_LARGE: f64 = 6.703903964971299e+153; // sqrt(DBL_MAX / 4)
const LOG_LARGE: f64 = 708.3964185322641; // log(DBL_MAX / 4)
const SQRT_DBL_MIN: f64 = 1.4916681462400413e-154;
const MANT_DIG: i32 = 53;
const SCALE_UP: i32 = 2 * (MANT_DIG / 2) + 1;
const SCALE_DOWN: i32 = -(SCALE_UP + 1) / 2;

type C = (f64, f64);

#[derive(Clone, Copy, PartialEq)]
enum E {
    Ok,
    Dom,
    Range,
}

#[inline]
fn fin(z: C) -> bool {
    z.0.is_finite() && z.1.is_finite()
}

use crate::modules::math::c_ldexp as ldexp;

// ------------------------------------------------------------------ sqrt

fn sqrt_special(x: f64, y: f64) -> C {
    if y.is_infinite() {
        return (INF, y);
    }
    if x.is_nan() {
        return (NAN, NAN);
    }
    if x == -INF {
        if y.is_nan() {
            return (NAN, INF);
        }
        return (0.0, INF.copysign(y));
    }
    if x == INF {
        if y.is_nan() {
            return (INF, NAN);
        }
        return (INF, 0.0f64.copysign(y));
    }
    (NAN, NAN)
}

pub fn c_sqrt(z: C) -> C {
    if !fin(z) {
        return sqrt_special(z.0, z.1);
    }
    if z.0 == 0.0 && z.1 == 0.0 {
        return (0.0, z.1);
    }
    let mut ax = z.0.abs();
    let ay = z.1.abs();
    let s;
    if ax < f64::MIN_POSITIVE && ay < f64::MIN_POSITIVE {
        ax = ldexp(ax, SCALE_UP);
        s = ldexp((ax + ax.hypot(ldexp(ay, SCALE_UP))).sqrt(), SCALE_DOWN);
    } else {
        ax /= 8.0;
        s = 2.0 * (ax + ax.hypot(ay / 8.0)).sqrt();
    }
    let d = ay / (2.0 * s);
    if z.0 >= 0.0 { (s, d.copysign(z.1)) } else { (d, s.copysign(z.1)) }
}

// ------------------------------------------------------------------ exp

fn c_exp(z: C) -> (C, E) {
    let (x, y) = z;
    if !fin(z) {
        let r = if x.is_infinite() && y.is_finite() && y != 0.0 {
            if x > 0.0 {
                (INF.copysign(y.cos()), INF.copysign(y.sin()))
            } else {
                (0.0f64.copysign(y.cos()), 0.0f64.copysign(y.sin()))
            }
        } else if x.is_nan() {
            if y == 0.0 { (NAN, y) } else { (NAN, NAN) }
        } else if x == INF {
            if y == 0.0 { (INF, y) } else { (INF, NAN) }
        } else if x == -INF {
            if y.is_finite() { (0.0, 0.0f64.copysign(y)) } else { (0.0, 0.0) }
        } else {
            (NAN, NAN)
        };
        let e = if y.is_infinite() && (x.is_finite() || x == INF) { E::Dom } else { E::Ok };
        return (r, e);
    }
    let r = if x > LOG_LARGE {
        let l = (x - 1.0).exp();
        (l * y.cos() * std::f64::consts::E, l * y.sin() * std::f64::consts::E)
    } else {
        let l = x.exp();
        (l * y.cos(), l * y.sin())
    };
    let e = if r.0.is_infinite() || r.1.is_infinite() { E::Range } else { E::Ok };
    (r, e)
}

// ------------------------------------------------------------------ log

fn log_special(x: f64, y: f64) -> C {
    if y.is_infinite() {
        if x.is_nan() {
            return (INF, NAN);
        }
        let im = if x == INF {
            FRAC_PI_4
        } else if x == -INF {
            P34
        } else {
            FRAC_PI_2
        };
        return (INF, im.copysign(y));
    }
    if y.is_nan() {
        if x.is_infinite() {
            return (INF, NAN);
        }
        return (NAN, NAN);
    }
    // y finite, x non-finite
    if x == -INF {
        return (INF, PI.copysign(y));
    }
    if x == INF {
        return (INF, 0.0f64.copysign(y));
    }
    (NAN, NAN)
}

fn c_log(z: C) -> (C, E) {
    if !fin(z) {
        return (log_special(z.0, z.1), E::Ok);
    }
    let ax = z.0.abs();
    let ay = z.1.abs();
    let re;
    if ax > LARGE || ay > LARGE {
        re = (ax / 2.0).hypot(ay / 2.0).ln() + LN_2;
    } else if ax < f64::MIN_POSITIVE && ay < f64::MIN_POSITIVE {
        if ax > 0.0 || ay > 0.0 {
            re = ldexp(ax, MANT_DIG).hypot(ldexp(ay, MANT_DIG)).ln() - MANT_DIG as f64 * LN_2;
        } else {
            return ((-INF, z.1.atan2(z.0)), E::Dom);
        }
    } else {
        let h = ax.hypot(ay);
        if (0.71..=1.73).contains(&h) {
            let am = if ax > ay { ax } else { ay };
            let an = if ax > ay { ay } else { ax };
            re = ((am - 1.0) * (am + 1.0) + an * an).ln_1p() / 2.0;
        } else {
            re = h.ln();
        }
    }
    ((re, z.1.atan2(z.0)), E::Ok)
}

// ------------------------------------------------------------------ inverse trig / hyperbolic

fn acos_special(x: f64, y: f64) -> C {
    if y.is_infinite() {
        if x.is_nan() {
            return (NAN, -y);
        }
        let re = if x == INF {
            FRAC_PI_4
        } else if x == -INF {
            P34
        } else {
            FRAC_PI_2
        };
        return (re, -y);
    }
    if y.is_nan() {
        if x == 0.0 {
            return (FRAC_PI_2, NAN);
        }
        if x.is_infinite() {
            return (NAN, INF);
        }
        return (NAN, NAN);
    }
    if x == -INF {
        return (PI, -INF.copysign(y));
    }
    if x == INF {
        return (0.0, -INF.copysign(y));
    }
    (NAN, NAN)
}

fn c_acos(z: C) -> C {
    if !fin(z) {
        return acos_special(z.0, z.1);
    }
    if z.0.abs() > LARGE || z.1.abs() > LARGE {
        let re = z.1.abs().atan2(z.0);
        let l = (z.0 / 2.0).hypot(z.1 / 2.0).ln() + LN_2 * 2.0;
        let im = if z.0 < 0.0 { -l.copysign(z.1) } else { l.copysign(-z.1) };
        (re, im)
    } else {
        let s1 = c_sqrt((1.0 - z.0, -z.1));
        let s2 = c_sqrt((1.0 + z.0, z.1));
        (2.0 * s1.0.atan2(s2.0), (s2.0 * s1.1 - s2.1 * s1.0).asinh())
    }
}

fn c_acosh(z: C) -> C {
    if !fin(z) {
        if z.0 == 0.0 && z.1.is_nan() {
            return (NAN, FRAC_PI_2);
        }
        return log_special(z.0, z.1);
    }
    if z.0.abs() > LARGE || z.1.abs() > LARGE {
        ((z.0 / 2.0).hypot(z.1 / 2.0).ln() + LN_2 * 2.0, z.1.atan2(z.0))
    } else {
        let s1 = c_sqrt((z.0 - 1.0, z.1));
        let s2 = c_sqrt((z.0 + 1.0, z.1));
        ((s1.0 * s2.0 + s1.1 * s2.1).asinh(), 2.0 * s1.1.atan2(s2.0))
    }
}

fn asinh_special(x: f64, y: f64) -> C {
    if y.is_infinite() {
        if x.is_nan() {
            return (INF, NAN);
        }
        let im = if x.is_infinite() { FRAC_PI_4 } else { FRAC_PI_2 };
        return (INF.copysign(x), im.copysign(y));
    }
    if y.is_nan() {
        if x.is_infinite() {
            return (x, NAN);
        }
        return (NAN, NAN);
    }
    if x.is_infinite() {
        return (x, 0.0f64.copysign(y));
    }
    // x nan, y finite
    if y == 0.0 { (NAN, y) } else { (NAN, NAN) }
}

fn c_asinh(z: C) -> C {
    if !fin(z) {
        return asinh_special(z.0, z.1);
    }
    if z.0.abs() > LARGE || z.1.abs() > LARGE {
        let l = (z.0 / 2.0).hypot(z.1 / 2.0).ln() + LN_2 * 2.0;
        let re = if z.1 >= 0.0 { l.copysign(z.0) } else { -l.copysign(-z.0) };
        (re, z.1.atan2(z.0.abs()))
    } else {
        let s1 = c_sqrt((1.0 + z.1, -z.0));
        let s2 = c_sqrt((1.0 - z.1, z.0));
        ((s1.0 * s2.1 - s2.0 * s1.1).asinh(), z.1.atan2(s1.0 * s2.0 - s1.1 * s2.1))
    }
}

fn c_asin(z: C) -> C {
    let s = c_asinh((-z.1, z.0));
    (s.1, -s.0)
}

fn atanh_special(x: f64, y: f64) -> C {
    if y.is_infinite() {
        if x.is_nan() {
            return (0.0, FRAC_PI_2.copysign(y));
        }
        return (0.0f64.copysign(x), FRAC_PI_2.copysign(y));
    }
    if y.is_nan() {
        if x.is_infinite() {
            return (0.0f64.copysign(x), NAN);
        }
        if x == 0.0 {
            return (x, NAN);
        }
        return (NAN, NAN);
    }
    if x.is_infinite() {
        return (0.0f64.copysign(x), FRAC_PI_2.copysign(y));
    }
    (NAN, NAN)
}

fn c_atanh(z: C) -> (C, E) {
    if !fin(z) {
        return (atanh_special(z.0, z.1), E::Ok);
    }
    if z.0 < 0.0 {
        let (r, e) = c_atanh((-z.0, -z.1));
        return ((-r.0, -r.1), e);
    }
    let ay = z.1.abs();
    if z.0 > SQRT_LARGE || ay > SQRT_LARGE {
        let h = (z.0 / 2.0).hypot(z.1 / 2.0);
        ((z.0 / 4.0 / h / h, -FRAC_PI_2.copysign(-z.1)), E::Ok)
    } else if z.0 == 1.0 && ay < SQRT_DBL_MIN {
        if ay == 0.0 {
            ((INF, z.1), E::Dom)
        } else {
            ((-(ay.sqrt() / ay.hypot(2.0).sqrt()).ln(), (2.0f64.atan2(-ay) / 2.0).copysign(z.1)), E::Ok)
        }
    } else {
        let re = (4.0 * z.0 / ((1.0 - z.0) * (1.0 - z.0) + ay * ay)).ln_1p() / 4.0;
        let im = -(-2.0 * z.1).atan2((1.0 - z.0) * (1.0 + z.0) - ay * ay) / 2.0;
        ((re, im), E::Ok)
    }
}

fn c_atan(z: C) -> (C, E) {
    let (s, e) = c_atanh((-z.1, z.0));
    ((s.1, -s.0), e)
}

// ------------------------------------------------------------------ hyperbolic

fn c_cosh(z: C) -> (C, E) {
    let (x, y) = z;
    if !fin(z) {
        let r = if x.is_infinite() && y.is_finite() && y != 0.0 {
            if x > 0.0 {
                (INF.copysign(y.cos()), INF.copysign(y.sin()))
            } else {
                (INF.copysign(y.cos()), -INF.copysign(y.sin()))
            }
        } else if x.is_infinite() {
            if y == 0.0 {
                (INF, if x > 0.0 { y } else { -y })
            } else {
                (INF, NAN)
            }
        } else if x.is_nan() {
            if y == 0.0 { (NAN, 0.0) } else { (NAN, NAN) }
        } else if x == 0.0 {
            (NAN, 0.0)
        } else {
            (NAN, NAN)
        };
        let e = if y.is_infinite() && !x.is_nan() { E::Dom } else { E::Ok };
        return (r, e);
    }
    let r = if x.abs() > LOG_LARGE {
        let xm1 = x - 1.0f64.copysign(x);
        (y.cos() * xm1.cosh() * std::f64::consts::E, y.sin() * xm1.sinh() * std::f64::consts::E)
    } else {
        (y.cos() * x.cosh(), y.sin() * x.sinh())
    };
    let e = if r.0.is_infinite() || r.1.is_infinite() { E::Range } else { E::Ok };
    (r, e)
}

fn c_sinh(z: C) -> (C, E) {
    let (x, y) = z;
    if !fin(z) {
        let r = if x.is_infinite() && y.is_finite() && y != 0.0 {
            if x > 0.0 {
                (INF.copysign(y.cos()), INF.copysign(y.sin()))
            } else {
                (-INF.copysign(y.cos()), INF.copysign(y.sin()))
            }
        } else if x.is_infinite() {
            if y == 0.0 { (x, y) } else { (x, NAN) }
        } else if x.is_nan() {
            if y == 0.0 { (NAN, y) } else { (NAN, NAN) }
        } else if x == 0.0 {
            (x, NAN)
        } else {
            (NAN, NAN)
        };
        let e = if y.is_infinite() && !x.is_nan() { E::Dom } else { E::Ok };
        return (r, e);
    }
    let r = if x.abs() > LOG_LARGE {
        let xm1 = x - 1.0f64.copysign(x);
        (y.cos() * xm1.sinh() * std::f64::consts::E, y.sin() * xm1.cosh() * std::f64::consts::E)
    } else {
        (y.cos() * x.sinh(), y.sin() * x.cosh())
    };
    let e = if r.0.is_infinite() || r.1.is_infinite() { E::Range } else { E::Ok };
    (r, e)
}

fn c_tanh(z: C) -> (C, E) {
    let (x, y) = z;
    if !fin(z) {
        let r = if x.is_infinite() && y.is_finite() && y != 0.0 {
            (1.0f64.copysign(x), 0.0f64.copysign(2.0 * y.sin() * y.cos()))
        } else if x.is_infinite() {
            if y == 0.0 {
                (1.0f64.copysign(x), y)
            } else {
                (1.0f64.copysign(x), 0.0)
            }
        } else if x.is_nan() {
            if y == 0.0 { (NAN, y) } else { (NAN, NAN) }
        } else if x == 0.0 {
            (x, NAN)
        } else {
            (NAN, NAN)
        };
        let e = if y.is_infinite() && x.is_finite() { E::Dom } else { E::Ok };
        return (r, e);
    }
    let r = if x.abs() > LOG_LARGE {
        (1.0f64.copysign(x), 4.0 * y.sin() * y.cos() * (-2.0 * x.abs()).exp())
    } else {
        let tx = x.tanh();
        let ty = y.tan();
        let cx = 1.0 / x.cosh();
        let txty = tx * ty;
        let denom = 1.0 + txty * txty;
        (tx * (1.0 + ty * ty) / denom, ((ty / denom) * cx) * cx)
    };
    (r, E::Ok)
}

fn c_cos(z: C) -> (C, E) {
    c_cosh((-z.1, z.0))
}
fn c_sin(z: C) -> (C, E) {
    let (s, e) = c_sinh((-z.1, z.0));
    ((s.1, -s.0), e)
}
fn c_tan(z: C) -> (C, E) {
    let (s, e) = c_tanh((-z.1, z.0));
    ((s.1, -s.0), e)
}

// ------------------------------------------------------------------ rect / polar

fn c_rect(r: f64, phi: f64) -> (C, E) {
    if !r.is_finite() || !phi.is_finite() {
        let z = if r.is_infinite() && phi.is_finite() && phi != 0.0 {
            if r > 0.0 {
                (INF.copysign(phi.cos()), INF.copysign(phi.sin()))
            } else {
                (-INF.copysign(phi.cos()), -INF.copysign(phi.sin()))
            }
        } else if r.is_infinite() {
            if phi == 0.0 {
                (r, if r > 0.0 { phi } else { -phi })
            } else {
                (INF, NAN)
            }
        } else if r.is_nan() {
            if phi == 0.0 { (NAN, phi) } else { (NAN, NAN) }
        } else if r == 0.0 {
            (0.0, 0.0)
        } else {
            (NAN, NAN)
        };
        let e = if r != 0.0 && !r.is_nan() && phi.is_infinite() { E::Dom } else { E::Ok };
        return (z, e);
    }
    if phi == 0.0 {
        return ((r, r * phi), E::Ok);
    }
    ((r * phi.cos(), r * phi.sin()), E::Ok)
}

// ------------------------------------------------------------------ Python wrappers

fn finish(r: C, e: E) -> PyResult {
    match e {
        E::Dom => raise!(ValueError, "math domain error"),
        E::Range => raise!(OverflowError, "math range error"),
        E::Ok => Ok(complex::new_complex(r.0, r.1)),
    }
}

fn arg(o: &PyObject) -> PyResult<C> {
    as_ccomplex(o)
}

macro_rules! cfun {
    ($name:ident, $f:expr) => {
        fn $name(_m: &PyObject, z: &PyObject) -> PyResult {
            let z = arg(z)?;
            let (r, e) = $f(z);
            finish(r, e)
        }
    };
}
cfun!(m_sqrt, |z| (c_sqrt(z), E::Ok));
cfun!(m_exp, c_exp);
cfun!(m_acos, |z| (c_acos(z), E::Ok));
cfun!(m_acosh, |z| (c_acosh(z), E::Ok));
cfun!(m_asin, |z| (c_asin(z), E::Ok));
cfun!(m_asinh, |z| (c_asinh(z), E::Ok));
cfun!(m_atan, c_atan);
cfun!(m_atanh, c_atanh);
cfun!(m_cos, c_cos);
cfun!(m_cosh, c_cosh);
cfun!(m_sin, c_sin);
cfun!(m_sinh, c_sinh);
cfun!(m_tan, c_tan);
cfun!(m_tanh, c_tanh);

fn m_log(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("log", &["z", "base"], 1, 1);
    let [z, base] = args::parse::<2>(&P, a, kw)?;
    let z = arg(z.unwrap())?;
    let (mut r, e) = c_log(z);
    if e != E::Ok {
        return finish(r, e);
    }
    if let Some(b) = base {
        let b = arg(b)?;
        let (lb, e2) = c_log(b);
        if e2 != E::Ok {
            return finish(lb, e2);
        }
        match complex::c_quot(r, lb) {
            Some(q) => r = q,
            None => raise!(ValueError, "math domain error"),
        }
    }
    finish(r, E::Ok)
}
fn m_log10(_m: &PyObject, z: &PyObject) -> PyResult {
    let z = arg(z)?;
    let (r, e) = c_log(z);
    finish((r.0 / std::f64::consts::LN_10, r.1 / std::f64::consts::LN_10), e)
}
fn m_phase(_m: &PyObject, z: &PyObject) -> PyResult {
    let z = arg(z)?;
    Ok(float::new_float(z.1.atan2(z.0)))
}
fn m_polar(_m: &PyObject, z: &PyObject) -> PyResult {
    let z = arg(z)?;
    let phi = z.1.atan2(z.0);
    let r = match complex::c_abs(z) {
        Ok(r) => r,
        Err(()) => raise!(OverflowError, "math range error"),
    };
    Ok(tuple::new_tuple(vec![float::new_float(r), float::new_float(phi)]))
}
fn m_rect(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("rect", a, 2, 2)?;
    let r = crate::modules::math::to_f64(a[0])?;
    let phi = crate::modules::math::to_f64(a[1])?;
    let (z, e) = c_rect(r, phi);
    finish(z, e)
}
fn m_isfinite(_m: &PyObject, z: &PyObject) -> PyResult {
    let z = arg(z)?;
    Ok(bool_::new_bool(fin(z)))
}
fn m_isnan(_m: &PyObject, z: &PyObject) -> PyResult {
    let z = arg(z)?;
    Ok(bool_::new_bool(z.0.is_nan() || z.1.is_nan()))
}
fn m_isinf(_m: &PyObject, z: &PyObject) -> PyResult {
    let z = arg(z)?;
    Ok(bool_::new_bool(z.0.is_infinite() || z.1.is_infinite()))
}
fn m_isclose(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("isclose", &["a", "b", "rel_tol", "abs_tol"], 2, 2).kwonly(2);
    let [x, y, rt, at] = args::parse::<4>(&P, a, kw)?;
    let x = arg(x.unwrap())?;
    let y = arg(y.unwrap())?;
    let rel_tol = match rt {
        Some(v) => crate::modules::math::to_f64(v)?,
        None => 1e-9,
    };
    let abs_tol = match at {
        Some(v) => crate::modules::math::to_f64(v)?,
        None => 0.0,
    };
    if rel_tol < 0.0 || abs_tol < 0.0 {
        raise!(ValueError, "tolerances must be non-negative");
    }
    if x.0 == y.0 && x.1 == y.1 {
        return Ok(bool_::new_bool(true));
    }
    if x.0.is_infinite() || x.1.is_infinite() || y.0.is_infinite() || y.1.is_infinite() {
        return Ok(bool_::new_bool(false));
    }
    let abs = |z: C| complex::c_abs(z).unwrap_or(INF);
    let diff = abs((x.0 - y.0, x.1 - y.1));
    Ok(bool_::new_bool(diff <= rel_tol * abs(y) || diff <= rel_tol * abs(x) || diff <= abs_tol))
}

pub fn init_cmath() -> PyResult {
    let m = crate::types::module::new_module("cmath")?;
    let d = crate::types::module::module_dict(&m);
    crate::types::dict::set_item_str(&d, "__doc__", &pystr::new_str("This module provides access to mathematical functions for complex\nnumbers."))?;
    let methods = crate::mdefs![
        ("acos", O, m_acos, "acos($module, z, /)\n--\n\nReturn the arc cosine of z."),
        ("acosh", O, m_acosh, "acosh($module, z, /)\n--\n\nReturn the inverse hyperbolic cosine of z."),
        ("asin", O, m_asin, "asin($module, z, /)\n--\n\nReturn the arc sine of z."),
        ("asinh", O, m_asinh, "asinh($module, z, /)\n--\n\nReturn the inverse hyperbolic sine of z."),
        ("atan", O, m_atan, "atan($module, z, /)\n--\n\nReturn the arc tangent of z."),
        ("atanh", O, m_atanh, "atanh($module, z, /)\n--\n\nReturn the inverse hyperbolic tangent of z."),
        ("cos", O, m_cos, "cos($module, z, /)\n--\n\nReturn the cosine of z."),
        ("cosh", O, m_cosh, "cosh($module, z, /)\n--\n\nReturn the hyperbolic cosine of z."),
        ("exp", O, m_exp, "exp($module, z, /)\n--\n\nReturn the exponential value e**z."),
        ("isclose", FASTKW, m_isclose, "isclose($module, /, a, b, *, rel_tol=1e-09, abs_tol=0.0)\n--\n\nDetermine whether two complex numbers are close in value."),
        ("isfinite", O, m_isfinite, "isfinite($module, z, /)\n--\n\nReturn True if both the real and imaginary parts of z are finite, else False."),
        ("isinf", O, m_isinf, "isinf($module, z, /)\n--\n\nChecks if the real or imaginary part of z is infinite."),
        ("isnan", O, m_isnan, "isnan($module, z, /)\n--\n\nChecks if the real or imaginary part of z not a number (NaN)."),
        ("log", FASTKW, m_log, "log($module, z, base=<unrepresentable>, /)\n--\n\nlog(z[, base]) -> the logarithm of z to the given base.\n\nIf the base is not specified, returns the natural logarithm (base e) of z."),
        ("log10", O, m_log10, "log10($module, z, /)\n--\n\nReturn the base-10 logarithm of z."),
        ("phase", O, m_phase, "phase($module, z, /)\n--\n\nReturn argument, also known as the phase angle, of a complex."),
        ("polar", O, m_polar, "polar($module, z, /)\n--\n\nConvert a complex from rectangular coordinates to polar coordinates.\n\nr is the distance from 0 and phi the phase angle."),
        ("rect", FAST, m_rect, "rect($module, r, phi, /)\n--\n\nConvert from polar coordinates to rectangular coordinates."),
        ("sin", O, m_sin, "sin($module, z, /)\n--\n\nReturn the sine of z."),
        ("sinh", O, m_sinh, "sinh($module, z, /)\n--\n\nReturn the hyperbolic sine of z."),
        ("sqrt", O, m_sqrt, "sqrt($module, z, /)\n--\n\nReturn the square root of z."),
        ("tan", O, m_tan, "tan($module, z, /)\n--\n\nReturn the tangent of z."),
        ("tanh", O, m_tanh, "tanh($module, z, /)\n--\n\nReturn the hyperbolic tangent of z."),
    ];
    crate::builtins::add_methods_to_dict(&d, methods, Some(&m))?;
    let setf = |k: &str, v: f64| crate::types::dict::set_item_str(&d, k, &float::new_float(v));
    setf("pi", PI)?;
    setf("e", std::f64::consts::E)?;
    setf("tau", std::f64::consts::TAU)?;
    setf("inf", INF)?;
    setf("nan", NAN)?;
    crate::types::dict::set_item_str(&d, "infj", &complex::new_complex(0.0, INF))?;
    crate::types::dict::set_item_str(&d, "nanj", &complex::new_complex(0.0, NAN))?;
    Ok(m)
}
