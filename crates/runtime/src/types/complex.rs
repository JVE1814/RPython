//! `complex` core: layout, constructors and the arithmetic kernels
//! (C11 Annex G style multiplication/division with recovery of infinities and zeros).
use crate::ffi::*;
use crate::object::*;
#[unsafe(no_mangle)]
pub static mut PyComplex_Type: PyTypeObject = PyTypeObject::ZERO;
#[inline(always)]
pub fn complex_type() -> *mut PyTypeObject {
    &raw mut PyComplex_Type
}
#[inline]
pub fn is_complex(o: &PyObject) -> bool {
    o.is_type(complex_type()) || crate::types::typeobject::is_subtype(o.type_ptr(), complex_type())
}
#[inline]
pub fn is_complex_exact(o: &PyObject) -> bool {
    o.is_type(complex_type())
}
pub fn new_complex(real: f64, imag: f64) -> PyRef {
    unsafe {
        let op = crate::alloc::alloc_object(complex_type(), std::mem::size_of::<PyComplexObject>());
        (*(op as *mut PyComplexObject)).cval = Py_complex { real, imag };
        PyRef::from_raw(op)
    }
}
pub fn new_complex_typed(tp: *mut PyTypeObject, real: f64, imag: f64) -> crate::errors::PyResult {
    if tp == complex_type() {
        return Ok(new_complex(real, imag));
    }
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(crate::types::typeobject::generic_alloc);
        let op = alloc(tp, 0);
        if op.is_null() {
            return Err(crate::errors::PyErr);
        }
        (*(op as *mut PyComplexObject)).cval = Py_complex { real, imag };
        Ok(PyRef::from_raw(op))
    }
}
#[inline]
pub fn complex_value(o: &PyObject) -> Py_complex {
    unsafe { (*(o.as_ptr() as *const PyComplexObject)).cval }
}

pub type C = (f64, f64);

#[inline]
fn cs(x: f64, y: f64) -> f64 {
    // copysign(isinf(x) ? 1.0 : 0.0, x)
    (if x.is_infinite() { 1.0f64 } else { 0.0 }).copysign(y)
}

/// complex * complex (Annex G _Cmultd).
pub fn c_prod(z: C, w: C) -> C {
    let (mut a, mut b) = z;
    let (mut c, mut d) = w;
    let ac = a * c;
    let bd = b * d;
    let ad = a * d;
    let bc = b * c;
    let mut x = ac - bd;
    let mut y = ad + bc;
    if x.is_nan() && y.is_nan() {
        let mut recalc = false;
        if a.is_infinite() || b.is_infinite() {
            a = cs(a, a);
            b = cs(b, b);
            if c.is_nan() {
                c = 0.0f64.copysign(c);
            }
            if d.is_nan() {
                d = 0.0f64.copysign(d);
            }
            recalc = true;
        }
        if c.is_infinite() || d.is_infinite() {
            c = cs(c, c);
            d = cs(d, d);
            if a.is_nan() {
                a = 0.0f64.copysign(a);
            }
            if b.is_nan() {
                b = 0.0f64.copysign(b);
            }
            recalc = true;
        }
        if !recalc && (ac.is_infinite() || bd.is_infinite() || ad.is_infinite() || bc.is_infinite()) {
            if a.is_nan() {
                a = 0.0f64.copysign(a);
            }
            if b.is_nan() {
                b = 0.0f64.copysign(b);
            }
            if c.is_nan() {
                c = 0.0f64.copysign(c);
            }
            if d.is_nan() {
                d = 0.0f64.copysign(d);
            }
            recalc = true;
        }
        if recalc {
            x = f64::INFINITY * (a * c - b * d);
            y = f64::INFINITY * (a * d + b * c);
        }
    }
    (x, y)
}

/// complex / complex; None on division by zero.
pub fn c_quot(a: C, b: C) -> Option<C> {
    let abs_br = b.0.abs();
    let abs_bi = b.1.abs();
    let mut r: C;
    if abs_br >= abs_bi {
        if abs_br == 0.0 {
            return None;
        }
        let ratio = b.1 / b.0;
        let denom = b.0 + b.1 * ratio;
        r = ((a.0 + a.1 * ratio) / denom, (a.1 - a.0 * ratio) / denom);
    } else if abs_bi >= abs_br {
        let ratio = b.0 / b.1;
        let denom = b.0 * ratio + b.1;
        r = ((a.0 * ratio + a.1) / denom, (a.1 * ratio - a.0) / denom);
    } else {
        r = (f64::NAN, f64::NAN);
    }
    if r.0.is_nan() && r.1.is_nan() {
        if (a.0.is_infinite() || a.1.is_infinite()) && b.0.is_finite() && b.1.is_finite() {
            let x = cs(a.0, a.0);
            let y = cs(a.1, a.1);
            r = (f64::INFINITY * (x * b.0 + y * b.1), f64::INFINITY * (y * b.0 - x * b.1));
        } else if (abs_br.is_infinite() || abs_bi.is_infinite()) && a.0.is_finite() && a.1.is_finite() {
            let x = cs(b.0, b.0);
            let y = cs(b.1, b.1);
            r = (0.0 * (a.0 * x + a.1 * y), 0.0 * (a.1 * x - a.0 * y));
        }
    }
    Some(r)
}

/// real / complex; None on division by zero.
pub fn rc_quot(a: f64, b: C) -> Option<C> {
    let abs_br = b.0.abs();
    let abs_bi = b.1.abs();
    let mut r: C;
    if abs_br >= abs_bi {
        if abs_br == 0.0 {
            return None;
        }
        let ratio = b.1 / b.0;
        let denom = b.0 + b.1 * ratio;
        r = (a / denom, (-a * ratio) / denom);
    } else if abs_bi >= abs_br {
        let ratio = b.0 / b.1;
        let denom = b.0 * ratio + b.1;
        r = ((a * ratio) / denom, (-a) / denom);
    } else {
        r = (f64::NAN, f64::NAN);
    }
    if r.0.is_nan() && r.1.is_nan() {
        if a.is_infinite() && b.0.is_finite() && b.1.is_finite() {
            let x = cs(a, a);
            r = (f64::INFINITY * (x * b.0), f64::INFINITY * (-x * b.1));
        } else if (abs_br.is_infinite() || abs_bi.is_infinite()) && a.is_finite() {
            let x = cs(b.0, b.0);
            let y = cs(b.1, b.1);
            r = (0.0 * (a * x), 0.0 * (-a * y));
        }
    }
    Some(r)
}

/// abs(z): Ok(value) or Err(()) on overflow.
pub fn c_abs(z: C) -> Result<f64, ()> {
    if !z.0.is_finite() || !z.1.is_finite() {
        if z.0.is_infinite() {
            return Ok(z.0.abs());
        }
        if z.1.is_infinite() {
            return Ok(z.1.abs());
        }
        return Ok(f64::NAN);
    }
    let r = z.0.hypot(z.1);
    if r.is_infinite() { Err(()) } else { Ok(r) }
}
