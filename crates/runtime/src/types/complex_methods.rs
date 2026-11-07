//! complex: slots, constructor and methods (3.14+ mixed-mode arithmetic rules).

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::complex::*;
use crate::types::typeobject::*;
use crate::types::{bool_, float, float_methods, int, none, str as pystr, tuple};
use std::ffi::c_int;

fn ni() -> PyResult {
    Ok(none::not_implemented().to_ref())
}

/// An operand of a complex binary operation.
enum Operand {
    Complex(C),
    Real(f64),
}

fn operand(o: &PyObject) -> PyResult<Option<Operand>> {
    if is_complex(o) {
        let c = complex_value(o);
        return Ok(Some(Operand::Complex((c.real, c.imag))));
    }
    if float::is_float(o) {
        return Ok(Some(Operand::Real(float::float_value(o))));
    }
    if int::is_int(o) {
        return Ok(Some(Operand::Real(int::to_f64(o)?)));
    }
    Ok(None)
}

fn cval(o: &Operand) -> C {
    match o {
        Operand::Complex(c) => *c,
        Operand::Real(r) => (*r, 0.0),
    }
}

fn mk(c: C) -> PyResult {
    Ok(new_complex(c.0, c.1))
}

fn binop(a: &PyObject, b: &PyObject, f: fn(Operand, Operand) -> PyResult) -> PyResult {
    let x = match operand(a)? {
        Some(x) => x,
        None => return ni(),
    };
    let y = match operand(b)? {
        Some(y) => y,
        None => return ni(),
    };
    f(x, y)
}

fn add(x: Operand, y: Operand) -> PyResult {
    use Operand::*;
    mk(match (x, y) {
        (Complex(a), Complex(b)) => (a.0 + b.0, a.1 + b.1),
        (Complex(a), Real(r)) => (a.0 + r, a.1),
        (Real(r), Complex(b)) => (r + b.0, b.1),
        (Real(a), Real(b)) => (a + b, 0.0),
    })
}
fn sub(x: Operand, y: Operand) -> PyResult {
    use Operand::*;
    mk(match (x, y) {
        (Complex(a), Complex(b)) => (a.0 - b.0, a.1 - b.1),
        (Complex(a), Real(r)) => (a.0 - r, a.1),
        (Real(r), Complex(b)) => (r - b.0, -b.1),
        (Real(a), Real(b)) => (a - b, 0.0),
    })
}
fn mul(x: Operand, y: Operand) -> PyResult {
    use Operand::*;
    mk(match (x, y) {
        (Complex(a), Complex(b)) => c_prod(a, b),
        (Complex(a), Real(r)) => (a.0 * r, a.1 * r),
        (Real(r), Complex(b)) => (r * b.0, r * b.1),
        (Real(a), Real(b)) => (a * b, 0.0),
    })
}
fn div(x: Operand, y: Operand) -> PyResult {
    use Operand::*;
    let r = match (x, y) {
        (Complex(a), Complex(b)) => c_quot(a, b),
        (Complex(a), Real(r)) => {
            if r == 0.0 {
                None
            } else {
                Some((a.0 / r, a.1 / r))
            }
        }
        (Real(r), Complex(b)) => rc_quot(r, b),
        (Real(a), Real(b)) => c_quot((a, 0.0), (b, 0.0)),
    };
    match r {
        Some(c) => mk(c),
        None => raise!(ZeroDivisionError, "division by zero"),
    }
}

macro_rules! slot2 {
    ($name:ident, $f:ident) => {
        unsafe extern "C" fn $name(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
            unsafe { into_c(binop(&*a, &*b, $f)) }
        }
    };
}
slot2!(complex_add, add);
slot2!(complex_sub, sub);
slot2!(complex_mul, mul);
slot2!(complex_div, div);

unsafe extern "C" fn complex_pow_slot(a: *mut PyObject, b: *mut PyObject, m: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let x = match operand(&*a)? {
                Some(x) => cval(&x),
                None => return ni(),
            };
            let y = match operand(&*b)? {
                Some(y) => cval(&y),
                None => return ni(),
            };
            if !(*m).is(none::none()) {
                raise!(ValueError, "complex modulo");
            }
            mk(crate::types::complex_pow::complex_pow(x, y)?)
        })();
        into_c(r)
    }
}

unsafe extern "C" fn complex_neg(a: *mut PyObject) -> *mut PyObject {
    unsafe {
        let c = complex_value(&*a);
        new_complex(-c.real, -c.imag).into_raw()
    }
}
unsafe extern "C" fn complex_pos(a: *mut PyObject) -> *mut PyObject {
    unsafe { exact_complex(&*a).into_raw() }
}
unsafe extern "C" fn complex_abs(a: *mut PyObject) -> *mut PyObject {
    unsafe {
        let c = complex_value(&*a);
        match c_abs((c.real, c.imag)) {
            Ok(r) => float::new_float(r).into_raw(),
            Err(()) => into_c(Err(err!(OverflowError, "absolute value too large"))),
        }
    }
}
unsafe extern "C" fn complex_bool(a: *mut PyObject) -> c_int {
    unsafe {
        let c = complex_value(&*a);
        (c.real != 0.0 || c.imag != 0.0) as c_int
    }
}

pub fn exact_complex(a: &PyObject) -> PyRef {
    if is_complex_exact(a) {
        return a.to_ref();
    }
    let c = complex_value(a);
    new_complex(c.real, c.imag)
}

unsafe extern "C" fn complex_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let c = complex_value(&*o);
        pystr::new_str(&rpy_numfmt::complex_repr(c.real, c.imag)).into_raw()
    }
}

pub fn complex_hash_value(re: f64, im: f64, o: &PyObject) -> isize {
    let p = crate::hash::hash_pointer(o.as_ptr() as usize);
    let hr = crate::hash::hash_double(re, p) as usize;
    let hi = crate::hash::hash_double(im, p) as usize;
    let h = hr.wrapping_add(1000003usize.wrapping_mul(hi)) as isize;
    if h == -1 { -2 } else { h }
}
unsafe extern "C" fn complex_hash(o: *mut PyObject) -> Py_hash_t {
    unsafe {
        let c = complex_value(&*o);
        complex_hash_value(c.real, c.imag, &*o)
    }
}

unsafe extern "C" fn complex_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if op != Py_EQ && op != Py_NE {
            return none::not_implemented().to_ref().into_raw();
        }
        let c = complex_value(a);
        let eq = if int::is_int(b) {
            c.imag == 0.0 && float_methods::compare_float_int(c.real, b, Py_EQ)
        } else if float::is_float(b) {
            c.imag == 0.0 && c.real == float::float_value(b)
        } else if is_complex(b) {
            let d = complex_value(b);
            c.real == d.real && c.imag == d.imag
        } else {
            return none::not_implemented().to_ref().into_raw();
        };
        bool_::new_bool(if op == Py_EQ { eq } else { !eq }).into_raw()
    }
}

/// Call `__complex__` if defined. Ok(None) if there is no such method.
fn try_complex_special(o: &PyObject) -> PyResult<Option<PyRef>> {
    let f = match ab::lookup_special(o, crate::name!("__complex__"))? {
        Some(f) => f,
        None => return Ok(None),
    };
    let r = ab::call0(&f)?;
    if !is_complex(&r) {
        raise!(TypeError, "__complex__ returned non-complex (type {})", tname(&r));
    }
    if !is_complex_exact(&r) {
        crate::modules::warnings::warn(
            crate::types::exceptions::DeprecationWarning(),
            &format!(
                "__complex__ returned non-complex (type {}).  The ability to return an instance of a strict subclass of complex is deprecated, and may be removed in a future version of Python.",
                tname(&r)
            ),
            1,
        )?;
    }
    Ok(Some(r))
}

fn has_float_or_index(o: &PyObject) -> bool {
    let nb = o.ty().tp_as_number;
    !nb.is_null() && unsafe { (*nb).nb_float.is_some() || (*nb).nb_index.is_some() }
}

fn complex_from_string(tp: *mut PyTypeObject, s: &PyObject) -> PyResult {
    let text = crate::types::float_math::transform_decimal_and_space(s);
    match rpy_numfmt::parse::parse_complex_bytes(&text) {
        Ok((re, im)) => new_complex_typed(tp, re, im),
        Err(_) => raise!(ValueError, "complex() arg is a malformed string"),
    }
}

fn dep_warn(which: &str, o: &PyObject) -> PyResult<()> {
    crate::modules::warnings::warn(
        crate::types::exceptions::DeprecationWarning(),
        &format!("complex() argument '{}' must be a real number, not {}", which, type_fqname(o.type_ptr())),
        1,
    )
}

/// `real_kw`: the real part was passed by keyword (a complex value there is deprecated
/// even without an imaginary part).
pub fn complex_new_impl(tp: *mut PyTypeObject, r: Option<&PyObject>, i: Option<&PyObject>, real_kw: bool) -> PyResult {
    let zero = int::new_int(0);
    let r = r.unwrap_or(&zero);
    if is_complex_exact(r) && i.is_none() && !real_kw && tp == complex_type() {
        return Ok(r.to_ref());
    }
    if pystr::is_str(r) {
        if i.is_some() {
            raise!(TypeError, "complex() argument 'real' must be a real number, not {}", type_fqname(r.type_ptr()));
        }
        return complex_from_string(tp, r);
    }
    if let Some(i) = i {
        if pystr::is_str(i) {
            raise!(TypeError, "complex() argument 'imag' must be a real number, not {}", type_fqname(i.type_ptr()));
        }
    }
    let (cr, cr_is_complex): (C, bool) = match try_complex_special(r)? {
        Some(t) => {
            let v = complex_value(&t);
            ((v.real, v.imag), true)
        }
        None => {
            if is_complex(r) {
                let v = complex_value(r);
                ((v.real, v.imag), true)
            } else {
                if !has_float_or_index(r) {
                    if i.is_none() {
                        raise!(TypeError, "complex() argument must be a string or a number, not {}", type_fqname(r.type_ptr()));
                    }
                    raise!(TypeError, "complex() argument 'real' must be a real number, not {}", type_fqname(r.type_ptr()));
                }
                ((ab::float_value(r)?, 0.0), false)
            }
        }
    };
    if cr_is_complex && (i.is_some() || real_kw) {
        dep_warn("real", r)?;
    }
    let (mut re, mut im);
    match i {
        None => {
            re = cr.0;
            im = if cr_is_complex { cr.1 } else { 0.0 };
        }
        Some(i) => {
            if is_complex(i) {
                dep_warn("imag", i)?;
                let ci = complex_value(i);
                re = cr.0 - ci.imag;
                im = ci.real;
            } else {
                if !has_float_or_index(i) {
                    raise!(TypeError, "complex() argument 'imag' must be a real number, not {}", type_fqname(i.type_ptr()));
                }
                re = cr.0;
                im = ab::float_value(i)?;
            }
            if cr_is_complex {
                im += cr.1;
            }
        }
    }
    let _ = (&mut re, &mut im);
    new_complex_typed(tp, re, im)
}

unsafe extern "C" fn complex_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        static P: crate::args::Params = crate::args::Params::new("complex", &["real", "imag"], 0, 0);
        let r = (|| -> PyResult {
            let (all, kwn) = crate::args::kwdict_to_vectorcall(&*args, kw);
            let refs: Vec<&PyObject> = all.iter().map(|r| &**r).collect();
            let [re, im] = crate::args::parse::<2>(&P, &refs, kwn.as_deref())?;
            let real_kw = re.is_some() && crate::types::tuple::tuple_len(&*args) == 0;
            complex_new_impl(tp, re, im, real_kw)
        })();
        into_c(r)
    }
}

/// PyComplex_AsCComplex
pub fn as_ccomplex(o: &PyObject) -> PyResult<C> {
    if is_complex(o) {
        let c = complex_value(o);
        return Ok((c.real, c.imag));
    }
    if let Some(t) = try_complex_special(o)? {
        let c = complex_value(&t);
        return Ok((c.real, c.imag));
    }
    Ok((ab::float_value(o)?, 0.0))
}

fn complex_from_number(cls: &PyObject, n: &PyObject) -> PyResult {
    let is_base = cls.is(type_obj(complex_type()));
    if is_complex_exact(n) && is_base {
        return Ok(n.to_ref());
    }
    let c = as_ccomplex(n)?;
    let r = new_complex(c.0, c.1);
    if is_base {
        return Ok(r);
    }
    ab::call(cls, &[&r])
}

fn complex_conjugate(o: &PyObject) -> PyResult {
    let c = complex_value(o);
    Ok(new_complex(c.real, -c.imag))
}
fn complex_getnewargs(o: &PyObject) -> PyResult {
    let c = complex_value(o);
    Ok(tuple::new_tuple(vec![float::new_float(c.real), float::new_float(c.imag)]))
}
fn complex_complex(o: &PyObject) -> PyResult {
    Ok(exact_complex(o))
}
fn complex_format(o: &PyObject, spec: &PyObject) -> PyResult {
    if !pystr::is_str(spec) {
        raise!(TypeError, "__format__() argument must be str, not {}", tname(spec));
    }
    if pystr::str_len(spec) == 0 {
        return ab::str(o);
    }
    let c = complex_value(o);
    let s = pystr::to_string_lossy(spec);
    let r = rpy_numfmt::format_complex(c.real, c.imag, &s, &tname(o)).map_err(crate::types::format_impl::numfmt_error)?;
    Ok(pystr::new_str(&r))
}
fn get_real(o: &PyObject) -> PyResult {
    Ok(float::new_float(complex_value(o).real))
}
fn get_imag(o: &PyObject) -> PyResult {
    Ok(float::new_float(complex_value(o).imag))
}

static mut COMPLEX_NUM: PyNumberMethods = PyNumberMethods::ZERO;

pub fn init_types() {
    unsafe {
        let t = &mut *complex_type();
        t.tp_name = c"complex".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyComplexObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE;
        t.tp_repr = Some(complex_repr);
        t.tp_hash = Some(complex_hash);
        t.tp_richcompare = Some(complex_richcompare);
        t.tp_new = Some(complex_new);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_doc = c"complex(real=0, imag=0)\n--\n\nCreate a complex number from a string or numbers.\n\nIf a string is given, parse it as a complex number.\nIf a single number is given, convert it to a complex number.\nIf the 'real' or 'imag' arguments are given, create a complex number\nwith the specified real and imaginary components.".as_ptr();
        let n = &mut *(&raw mut COMPLEX_NUM);
        n.nb_add = Some(complex_add);
        n.nb_subtract = Some(complex_sub);
        n.nb_multiply = Some(complex_mul);
        n.nb_true_divide = Some(complex_div);
        n.nb_power = Some(complex_pow_slot);
        n.nb_negative = Some(complex_neg);
        n.nb_positive = Some(complex_pos);
        n.nb_absolute = Some(complex_abs);
        n.nb_bool = Some(complex_bool);
        t.tp_as_number = n;
        t.tp_methods = crate::mdefs![
            ("conjugate", NOARGS, complex_conjugate, "conjugate($self, /)\n--\n\nReturn the complex conjugate of its argument. (3-4j).conjugate() == 3+4j."),
            ("__complex__", NOARGS, complex_complex, "__complex__($self, /)\n--\n\nConvert this value to exact type complex."),
            ("__format__", O, complex_format, "__format__($self, format_spec, /)\n--\n\nConvert to a string according to format_spec."),
            ("__getnewargs__", NOARGS, complex_getnewargs),
            ("from_number", O, complex_from_number, "from_number($type, number, /)\n--\n\nConvert number to a complex floating-point number."; METH_CLASS),
        ];
        // plain read-only members, as in CPython (complex.real is a member_descriptor)
        t.tp_members = crate::member_table![
            ("real", Py_T_DOUBLE, std::mem::offset_of!(PyComplexObject, cval) + std::mem::offset_of!(Py_complex, real), Py_READONLY; "the real part of a complex number"),
            ("imag", Py_T_DOUBLE, std::mem::offset_of!(PyComplexObject, cval) + std::mem::offset_of!(Py_complex, imag), Py_READONLY; "the imaginary part of a complex number"),
        ];
    }
}
