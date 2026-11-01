//! complex power (CPython semantics: small integral exponents by repeated squaring,
//! otherwise polar form), with errno-like error reporting.
use crate::errors::*;
use crate::types::complex::{c_prod, c_quot, C};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PowErr {
    None,
    Dom,
    Range,
}

fn c_powu(x: C, n: u64) -> C {
    let mut r = (1.0, 0.0);
    let mut p = x;
    let mut mask: u64 = 1;
    while mask > 0 && n >= mask {
        if n & mask != 0 {
            r = c_prod(r, p);
        }
        mask <<= 1;
        p = c_prod(p, p);
    }
    r
}

fn c_powi(x: C, n: i64) -> (C, PowErr) {
    if n > 0 {
        (c_powu(x, n as u64), PowErr::None)
    } else {
        match c_quot((1.0, 0.0), c_powu(x, n.unsigned_abs())) {
            Some(r) => (r, PowErr::None),
            None => ((0.0, 0.0), PowErr::Dom),
        }
    }
}

/// _Py_c_pow
pub fn c_pow(a: C, b: C) -> (C, PowErr) {
    if b.0 == 0.0 && b.1 == 0.0 {
        return ((1.0, 0.0), PowErr::None);
    }
    if a.0 == 0.0 && a.1 == 0.0 {
        if b.1 != 0.0 || b.0 < 0.0 {
            return ((0.0, 0.0), PowErr::Dom);
        }
        return ((0.0, 0.0), PowErr::None);
    }
    let vabs = a.0.hypot(a.1);
    let mut len = vabs.powf(b.0);
    let at = a.1.atan2(a.0);
    let mut phase = at * b.0;
    if b.1 != 0.0 {
        len /= (at * b.1).exp();
        phase += b.1 * vabs.ln();
    }
    let r = (len * phase.cos(), len * phase.sin());
    let e = if r.0.is_infinite() || r.1.is_infinite() { PowErr::Range } else { PowErr::None };
    (r, e)
}

/// complex ** complex as used by the `**` operator.
pub fn complex_pow(a: C, b: C) -> PyResult<C> {
    let (p, e) = if b.1 == 0.0 && b.0 == b.0.floor() && b.0.abs() <= 100.0 {
        let (p, e) = c_powi(a, b.0 as i64);
        let e = if e == PowErr::None && (p.0.is_infinite() || p.1.is_infinite()) { PowErr::Range } else { e };
        (p, e)
    } else {
        c_pow(a, b)
    };
    match e {
        PowErr::Dom => crate::raise!(ZeroDivisionError, "zero to a negative or complex power"),
        PowErr::Range => crate::raise!(OverflowError, "complex exponentiation"),
        PowErr::None => Ok(p),
    }
}
