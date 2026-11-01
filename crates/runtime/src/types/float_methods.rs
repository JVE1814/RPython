//! float: slots, constructor and methods.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::float::*;
use crate::types::float_math::*;
use crate::types::typeobject::*;
use crate::types::{bool_, dict, int, int_methods, none, str as pystr, tuple};
use std::ffi::c_int;

fn ni() -> *mut PyObject {
    none::not_implemented().to_ref().into_raw()
}

fn cmp_result(c: std::cmp::Ordering, op: c_int) -> bool {
    use std::cmp::Ordering::*;
    match op {
        Py_LT => c == Less,
        Py_LE => c != Greater,
        Py_EQ => c == Equal,
        Py_NE => c != Equal,
        Py_GT => c == Greater,
        _ => c != Less,
    }
}

unsafe extern "C" fn float_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe { pystr::new_str(&rpy_numfmt::float_repr(float_value(&*o))).into_raw() }
}
unsafe extern "C" fn float_hash_slot(o: *mut PyObject) -> Py_hash_t {
    unsafe { float_hash(&*o) }
}

/// CONVERT_TO_DOUBLE: float or int operand, else None (NotImplemented).
#[inline]
pub fn to_double(o: &PyObject) -> PyResult<Option<f64>> {
    if is_float(o) {
        return Ok(Some(float_value(o)));
    }
    if int::is_int(o) {
        return int::to_f64(o).map(Some);
    }
    Ok(None)
}

/// Exact comparison of a float with an int (op applied as `x op i`).
pub fn compare_float_int(x: f64, i: &PyObject, op: c_int) -> bool {
    if x.is_nan() {
        return op == Py_NE;
    }
    if x.is_infinite() {
        let c = if x > 0.0 { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Less };
        return cmp_result(c, op);
    }
    if int::is_compact(i) {
        // |i| < 2**30: exactly representable
        let y = int::compact_value(i) as f64;
        return cmp_result(x.partial_cmp(&y).unwrap(), op);
    }
    let c = rpy_bigint::conv::cmp_f64(int_methods::iref(i), x).map(|c| c.reverse()).unwrap_or(std::cmp::Ordering::Equal);
    cmp_result(c, op)
}

unsafe extern "C" fn float_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !is_float(a) {
            return ni();
        }
        let x = float_value(a);
        let r = if is_float(b) {
            let y = float_value(b);
            match op {
                Py_LT => x < y,
                Py_LE => x <= y,
                Py_EQ => x == y,
                Py_NE => x != y,
                Py_GT => x > y,
                _ => x >= y,
            }
        } else if int::is_int(b) {
            compare_float_int(x, b, op)
        } else {
            return ni();
        };
        bool_::new_bool(r).into_raw()
    }
}

macro_rules! fbin {
    ($name:ident, |$x:ident, $y:ident| $body:expr) => {
        unsafe extern "C" fn $name(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
            unsafe {
                let (a, b) = (&*a, &*b);
                let $x = match to_double(a) {
                    Ok(Some(v)) => v,
                    Ok(None) => return ni(),
                    Err(_) => return std::ptr::null_mut(),
                };
                let $y = match to_double(b) {
                    Ok(Some(v)) => v,
                    Ok(None) => return ni(),
                    Err(_) => return std::ptr::null_mut(),
                };
                let r: PyResult = $body;
                into_c(r)
            }
        }
    };
}
fbin!(float_add, |x, y| Ok(new_float(x + y)));
fbin!(float_sub, |x, y| Ok(new_float(x - y)));
fbin!(float_mul, |x, y| Ok(new_float(x * y)));
fbin!(float_div, |x, y| if y == 0.0 { Err(err!(ZeroDivisionError, "division by zero")) } else { Ok(new_float(x / y)) });
fbin!(float_floordiv, |x, y| float_divmod(x, y).map(|(d, _)| new_float(d)));
fbin!(float_rem, |x, y| float_mod(x, y).map(new_float));
fbin!(float_divmod_slot, |x, y| float_divmod(x, y).map(|(d, m)| tuple::new_tuple(vec![new_float(d), new_float(m)])));

unsafe extern "C" fn float_pow_slot(a: *mut PyObject, b: *mut PyObject, m: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        let x = match to_double(a) {
            Ok(Some(v)) => v,
            Ok(None) => return ni(),
            Err(_) => return std::ptr::null_mut(),
        };
        let y = match to_double(b) {
            Ok(Some(v)) => v,
            Ok(None) => return ni(),
            Err(_) => return std::ptr::null_mut(),
        };
        if !(*m).is(none::none()) {
            return into_c(Err(err!(TypeError, "pow() 3rd argument not allowed unless all arguments are integers")));
        }
        into_c(float_pow(x, y))
    }
}
unsafe extern "C" fn float_neg(a: *mut PyObject) -> *mut PyObject {
    unsafe { new_float(-float_value(&*a)).into_raw() }
}
unsafe extern "C" fn float_pos(a: *mut PyObject) -> *mut PyObject {
    unsafe { float_float(a) }
}
unsafe extern "C" fn float_abs(a: *mut PyObject) -> *mut PyObject {
    unsafe { new_float(float_value(&*a).abs()).into_raw() }
}
unsafe extern "C" fn float_bool(a: *mut PyObject) -> c_int {
    unsafe { (float_value(&*a) != 0.0) as c_int }
}
unsafe extern "C" fn float_int(a: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(float_to_int(float_value(&*a))) }
}
unsafe extern "C" fn float_float(a: *mut PyObject) -> *mut PyObject {
    unsafe {
        let a = &*a;
        if is_float_exact(a) { a.to_ref().into_raw() } else { new_float(float_value(a)).into_raw() }
    }
}

pub fn exact_float(a: &PyObject) -> PyRef {
    if is_float_exact(a) { a.to_ref() } else { new_float(float_value(a)) }
}

unsafe extern "C" fn float_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let float_init = (*float_type()).tp_init;
            if !kw.is_null()
                && dict::dict_len(&*kw) > 0
                && (tp == float_type() || (*tp).tp_init.map(|f| f as usize) == float_init.map(|f| f as usize))
            {
                raise!(TypeError, "float() takes no keyword arguments");
            }
            let a = tuple::tuple_items(&*args);
            if a.len() > 1 {
                raise!(TypeError, "float expected at most 1 argument, got {}", a.len());
            }
            let v = match a.first() {
                None => new_float(0.0),
                Some(x) => number_float_obj(x)?,
            };
            if tp == float_type() {
                return Ok(v);
            }
            new_float_typed(tp, float_value(&v))
        })();
        into_c(r)
    }
}

fn float_format_m(o: &PyObject, spec: &PyObject) -> PyResult {
    if !pystr::is_str(spec) {
        raise!(TypeError, "__format__() argument must be str, not {}", tname(spec));
    }
    if pystr::str_len(spec) == 0 {
        return ab::str(o);
    }
    crate::types::format_impl::format_float(float_value(o), spec, &tname(o))
}
fn float_is_integer(o: &PyObject) -> PyResult {
    let x = float_value(o);
    Ok(bool_::new_bool(x.is_finite() && x.floor() == x))
}
fn float_trunc_m(o: &PyObject) -> PyResult {
    float_to_int(float_value(o))
}
fn float_floor_m(o: &PyObject) -> PyResult {
    float_to_int(float_value(o).floor())
}
fn float_ceil_m(o: &PyObject) -> PyResult {
    float_to_int(float_value(o).ceil())
}
pub fn round_float_to_int(x: f64) -> PyResult {
    let r = rpy_numfmt::float_round_to_int(x).map_err(crate::types::format_impl::numfmt_error)?;
    float_to_int(r)
}
fn float_round_m(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("__round__", &["ndigits"], 0, 0);
    let [nd] = crate::args::parse::<1>(&P, args, kw)?;
    let x = float_value(o);
    match nd.filter(|a| !a.is(none::none())) {
        None => round_float_to_int(x),
        Some(n) => {
            let n = ab::index_as_ssize(n, None)? as i64;
            rpy_numfmt::float_round(x, n).map(new_float).map_err(crate::types::format_impl::numfmt_error)
        }
    }
}
fn float_conjugate(o: &PyObject) -> PyResult {
    Ok(exact_float(o))
}
fn float_getnewargs(o: &PyObject) -> PyResult {
    Ok(tuple::new_tuple(vec![exact_float(o)]))
}
fn float_hex(o: &PyObject) -> PyResult {
    Ok(pystr::new_str(&rpy_numfmt::float_hex(float_value(o))))
}
fn float_as_integer_ratio(o: &PyObject) -> PyResult {
    let (neg, m, e) = rpy_numfmt::float_as_integer_ratio(float_value(o)).map_err(crate::types::format_impl::numfmt_error)?;
    let mut num = rpy_bigint::BigInt::from(m);
    let mut den = rpy_bigint::BigInt::from(1u64);
    if e >= 0 {
        num = rpy_bigint::ops::lshift(num.as_ref(), e as u64).map_err(int_methods::int_error)?;
    } else {
        den = rpy_bigint::ops::lshift(den.as_ref(), (-e) as u64).map_err(int_methods::int_error)?;
    }
    if neg {
        num = rpy_bigint::ops::neg(num.as_ref());
    }
    Ok(tuple::new_tuple(vec![int_methods::from_big(num), int_methods::from_big(den)]))
}
fn float_fromhex(cls: &PyObject, s: &PyObject) -> PyResult {
    if !pystr::is_str(s) {
        raise!(TypeError, "bad argument type for built-in operation");
    }
    let text = pystr::to_string_lossy(s);
    let v = rpy_numfmt::float_fromhex(&text).map_err(crate::types::format_impl::numfmt_error)?;
    let f = new_float(v);
    if cls.is(type_obj(float_type())) {
        return Ok(f);
    }
    ab::call(cls, &[&f])
}
fn float_from_number(cls: &PyObject, n: &PyObject) -> PyResult {
    if is_float_exact(n) && cls.is(type_obj(float_type())) {
        return Ok(n.to_ref());
    }
    let x = ab::float_value(n)?;
    let f = new_float(x);
    if cls.is(type_obj(float_type())) {
        return Ok(f);
    }
    ab::call(cls, &[&f])
}
fn float_getformat(_cls: &PyObject, s: &PyObject) -> PyResult {
    if !pystr::is_str(s) {
        raise!(TypeError, "__getformat__() argument must be str, not {}", tname(s));
    }
    let t = pystr::to_string_lossy(s);
    if t != "double" && t != "float" {
        raise!(ValueError, "__getformat__() argument 1 must be 'double' or 'float'");
    }
    Ok(pystr::new_str("IEEE, little-endian"))
}
fn get_real(o: &PyObject) -> PyResult {
    Ok(exact_float(o))
}
fn get_imag(_o: &PyObject) -> PyResult {
    Ok(new_float(0.0))
}

static mut FLOAT_NUM: PyNumberMethods = PyNumberMethods::ZERO;

pub fn init_types() {
    unsafe {
        let t = &mut *float_type();
        t.tp_name = c"float".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyFloatObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE;
        t.tp_repr = Some(float_repr);
        t.tp_hash = Some(float_hash_slot);
        t.tp_richcompare = Some(float_richcompare);
        t.tp_new = Some(float_new);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_doc = c"float(x=0, /)\n--\n\nConvert a string or number to a floating-point number, if possible.".as_ptr();
        let n = &mut *(&raw mut FLOAT_NUM);
        n.nb_add = Some(float_add);
        n.nb_subtract = Some(float_sub);
        n.nb_multiply = Some(float_mul);
        n.nb_true_divide = Some(float_div);
        n.nb_floor_divide = Some(float_floordiv);
        n.nb_remainder = Some(float_rem);
        n.nb_divmod = Some(float_divmod_slot);
        n.nb_power = Some(float_pow_slot);
        n.nb_negative = Some(float_neg);
        n.nb_positive = Some(float_pos);
        n.nb_absolute = Some(float_abs);
        n.nb_bool = Some(float_bool);
        n.nb_int = Some(float_int);
        n.nb_float = Some(float_float);
        t.tp_as_number = n;
        t.tp_methods = crate::mdefs![
            ("conjugate", NOARGS, float_conjugate, "conjugate($self, /)\n--\n\nReturn self, the complex conjugate of any float."),
            ("__trunc__", NOARGS, float_trunc_m, "__trunc__($self, /)\n--\n\nReturn the Integral closest to x between 0 and x."),
            ("__floor__", NOARGS, float_floor_m, "__floor__($self, /)\n--\n\nReturn the floor as an Integral."),
            ("__ceil__", NOARGS, float_ceil_m, "__ceil__($self, /)\n--\n\nReturn the ceiling as an Integral."),
            ("__round__", FASTKW, float_round_m, "__round__($self, ndigits=None, /)\n--\n\nReturn the Integral closest to x, rounding half toward even.\n\nWhen an argument is passed, work like built-in round(x, ndigits)."),
            ("as_integer_ratio", NOARGS, float_as_integer_ratio, "as_integer_ratio($self, /)\n--\n\nReturn a pair of integers, whose ratio is exactly equal to the original float.\n\nThe ratio is in lowest terms and has a positive denominator.  Raise\nOverflowError on infinities and a ValueError on NaNs.\n\n>>> (10.0).as_integer_ratio()\n(10, 1)\n>>> (0.0).as_integer_ratio()\n(0, 1)\n>>> (-.25).as_integer_ratio()\n(-1, 4)"),
            ("fromhex", O, float_fromhex, "fromhex($type, string, /)\n--\n\nCreate a floating-point number from a hexadecimal string.\n\n>>> float.fromhex('0x1.ffffp10')\n2047.984375\n>>> float.fromhex('-0x1p-1074')\n-5e-324"; METH_CLASS),
            ("from_number", O, float_from_number, "from_number($type, number, /)\n--\n\nConvert real number to a floating-point number."; METH_CLASS),
            ("hex", NOARGS, float_hex, "hex($self, /)\n--\n\nReturn a hexadecimal representation of a floating-point number.\n\n>>> (-0.1).hex()\n'-0x1.999999999999ap-4'\n>>> 3.14159.hex()\n'0x1.921f9f01b866ep+1'"),
            ("is_integer", NOARGS, float_is_integer, "is_integer($self, /)\n--\n\nReturn True if the float is an integer."),
            ("__getnewargs__", NOARGS, float_getnewargs),
            ("__getformat__", O, float_getformat, "__getformat__($type, typestr, /)\n--\n\nYou probably don't want to use this function.\n\n  typestr\n    Must be 'double' or 'float'.\n\nIt exists mainly to be used in Python's test suite.\n\nThis function returns whichever of 'unknown', 'IEEE, big-endian' or 'IEEE,\nlittle-endian' best describes the format of floating-point numbers used by the\nC type named by typestr."; METH_CLASS),
            ("__format__", O, float_format_m, "__format__($self, format_spec, /)\n--\n\nFormats the float according to format_spec."),
        ];
        t.tp_getset = crate::getset_table![
            ("real", get_real; "the real part of a complex number"),
            ("imag", get_imag; "the imaginary part of a complex number"),
        ];
    }
}
