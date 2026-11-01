//! int: arithmetic slots, repr/hash/compare, constructor and core methods.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::int::*;
use crate::types::typeobject::*;
use crate::types::{bool_, float, none, str as pystr, tuple};
use crate::raise;
use rpy_bigint::{BigInt, IntError, ops};
use std::ffi::c_int;

pub use rpy_bigint::IntRef;

#[inline]
pub fn iref(o: &PyObject) -> IntRef<'_> {
    IntRef { neg: sign(o) < 0, mag: digits(o) }
}

pub fn from_big(b: BigInt) -> PyRef {
    from_digits(b.neg, &b.mag)
}

pub fn int_error(e: IntError) -> PyErr {
    use rpy_bigint::ExcKind;
    let msg = e.message();
    match e.kind() {
        ExcKind::ZeroDivisionError => err!(ZeroDivisionError, "{}", msg),
        ExcKind::OverflowError => err!(OverflowError, "{}", msg),
        ExcKind::ValueError => err!(ValueError, "{}", msg),
        ExcKind::MemoryError => memory_error(),
    }
}

pub fn from_decimal_str(neg: bool, decimal: &str) -> PyResult {
    match rpy_bigint::radix::from_digits(decimal.as_bytes(), 10) {
        Some(mag) => Ok(from_digits(neg && !mag.is_empty(), &mag)),
        None => Err(err!(ValueError, "invalid int literal")),
    }
}

pub fn to_decimal_string(o: &PyObject) -> PyResult<String> {
    if let Some(v) = as_i64(o) {
        return Ok(v.to_string());
    }
    let limit = crate::modules::sys::int_max_str_digits();
    if limit > 0 {
        match rpy_bigint::radix::to_decimal_string_limited(iref(o), limit as usize) {
            Ok(s) => return Ok(s),
            Err(IntError::IntToStrDigitLimit { limit }) => raise!(
                ValueError,
                "Exceeds the limit ({}) for integer string conversion; use sys.set_int_max_str_digits() to increase the limit",
                limit
            ),
            Err(e) => return Err(int_error(e)),
        }
    }
    Ok(rpy_bigint::radix::to_string(iref(o), 10))
}

unsafe extern "C" fn int_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(to_decimal_string(&*o).map(|s| pystr::new_str(&s))) }
}
unsafe extern "C" fn int_hash_slot(o: *mut PyObject) -> Py_hash_t {
    unsafe { int_hash(&*o) }
}

pub fn int_cmp(a: &PyObject, b: &PyObject) -> std::cmp::Ordering {
    if is_compact(a) && is_compact(b) {
        return compact_value(a).cmp(&compact_value(b));
    }
    ops::cmp(iref(a), iref(b))
}

unsafe extern "C" fn int_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !is_int(a) || !is_int(b) {
            return none::not_implemented().to_ref().into_raw();
        }
        let c = int_cmp(a, b);
        use std::cmp::Ordering::*;
        let r = match op {
            Py_LT => c == Less,
            Py_LE => c != Greater,
            Py_EQ => c == Equal,
            Py_NE => c != Equal,
            Py_GT => c == Greater,
            _ => c != Less,
        };
        bool_::new_bool(r).into_raw()
    }
}

macro_rules! binop {
    ($name:ident, |$x:ident, $y:ident| small $small:block, |$p:ident, $q:ident| big $big:block) => {
        unsafe extern "C" fn $name(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
            unsafe {
                let (a, b) = (&*a, &*b);
                if !is_int(a) || !is_int(b) {
                    return none::not_implemented().to_ref().into_raw();
                }
                if is_compact(a) && is_compact(b) {
                    let $x = compact_value(a);
                    let $y = compact_value(b);
                    let r: PyResult = (|| $small)();
                    return into_c(r);
                }
                let $p = iref(a);
                let $q = iref(b);
                let r: PyResult = (|| $big)();
                into_c(r)
            }
        }
    };
}

fn zero_div(msg: &str) -> PyErr {
    err!(ZeroDivisionError, "{}", msg)
}

binop!(int_add, |x, y| small { Ok(new_int(x + y)) }, |p, q| big { Ok(from_big(ops::add(p, q))) });
binop!(int_sub, |x, y| small { Ok(new_int(x - y)) }, |p, q| big { Ok(from_big(ops::sub(p, q))) });
binop!(int_mul, |x, y| small { Ok(new_int_i128(x as i128 * y as i128)) }, |p, q| big { Ok(from_big(ops::mul(p, q))) });
binop!(int_floordiv,
    |x, y| small {
        if y == 0 { Err(zero_div("division by zero")) }
        else { Ok(new_int(rpy_bigint::small::floordiv(x, y).unwrap())) }
    },
    |p, q| big {
        if q.mag.is_empty() { Err(zero_div("division by zero")) }
        else { ops::floordiv(p, q).map(from_big).map_err(int_error) }
    });
binop!(int_mod,
    |x, y| small {
        if y == 0 { Err(zero_div("integer modulo by zero")) } else { Ok(new_int(rpy_bigint::small::modulo(x, y))) }
    },
    |p, q| big {
        if q.mag.is_empty() { Err(zero_div("integer modulo by zero")) }
        else { ops::modulo(p, q).map(from_big).map_err(int_error) }
    });
binop!(int_divmod,
    |x, y| small {
        if y == 0 { Err(zero_div("division by zero")) }
        else {
            let (d, m) = rpy_bigint::small::divmod(x, y).unwrap();
            Ok(tuple::new_tuple(vec![new_int(d), new_int(m)]))
        }
    },
    |p, q| big {
        if q.mag.is_empty() { Err(zero_div("division by zero")) }
        else { ops::divmod(p, q).map(|(d, m)| tuple::new_tuple(vec![from_big(d), from_big(m)])).map_err(int_error) }
    });
binop!(int_truediv,
    |x, y| small {
        if y == 0 { Err(zero_div("division by zero")) } else { Ok(float::new_float(rpy_bigint::small::truediv(x, y))) }
    },
    |p, q| big {
        if q.mag.is_empty() { Err(zero_div("division by zero")) }
        else { ops::truediv(p, q).map(float::new_float).map_err(int_error) }
    });
binop!(int_and, |x, y| small { Ok(new_int(x & y)) }, |p, q| big { Ok(from_big(ops::and(p, q))) });
binop!(int_or, |x, y| small { Ok(new_int(x | y)) }, |p, q| big { Ok(from_big(ops::or(p, q))) });
binop!(int_xor, |x, y| small { Ok(new_int(x ^ y)) }, |p, q| big { Ok(from_big(ops::xor(p, q))) });

fn shift_count(q: IntRef) -> PyResult<Option<u64>> {
    if q.neg {
        raise!(ValueError, "negative shift count");
    }
    Ok(rpy_bigint::conv::to_u64(q))
}
binop!(int_lshift,
    |x, y| small {
        if y < 0 { Err(err!(ValueError, "negative shift count")) }
        else if x == 0 { Ok(new_int(0)) }
        else {
            match rpy_bigint::small::lshift(x, y as u64) {
                Some(v) => Ok(new_int(v)),
                None => ops::lshift(BigInt::from(x).as_ref(), y as u64).map(from_big).map_err(int_error),
            }
        }
    },
    |p, q| big {
        match shift_count(q)? {
            Some(n) => ops::lshift(p, n).map(from_big).map_err(int_error),
            None => ops::lshift_big(p, q).map(from_big).map_err(int_error),
        }
    });
binop!(int_rshift,
    |x, y| small {
        if y < 0 { Err(err!(ValueError, "negative shift count")) } else { Ok(new_int(rpy_bigint::small::rshift(x, y as u64))) }
    },
    |p, q| big {
        match shift_count(q)? {
            Some(n) => Ok(from_big(ops::rshift(p, n))),
            None => ops::rshift_big(p, q).map(from_big).map_err(int_error),
        }
    });

unsafe extern "C" fn int_pow(a: *mut PyObject, b: *mut PyObject, m: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, b, m) = (&*a, &*b, &*m);
        if !is_int(a) || !is_int(b) {
            return none::not_implemented().to_ref().into_raw();
        }
        let r = (|| -> PyResult {
            if !m.is(none::none()) {
                if !is_int(m) {
                    return Ok(none::not_implemented().to_ref());
                }
                if digits(m).is_empty() {
                    raise!(ValueError, "pow() 3rd argument cannot be 0");
                }
                return ops::pow_mod(iref(a), iref(b), iref(m)).map(from_big).map_err(int_error);
            }
            if sign(b) < 0 {
                // float power
                let x = to_f64(a)?;
                let y = to_f64(b)?;
                return crate::types::float_math::float_pow(x, y);
            }
            if is_compact(a) && is_compact(b) {
                if let Some(v) = rpy_bigint::small::pow(compact_value(a), compact_value(b) as u64) {
                    return Ok(new_int(v));
                }
            }
            ops::pow(iref(a), iref(b)).map(from_big).map_err(int_error)
        })();
        into_c(r)
    }
}

unsafe extern "C" fn int_neg(a: *mut PyObject) -> *mut PyObject {
    unsafe {
        let a = &*a;
        if is_compact(a) {
            return new_int(-compact_value(a)).into_raw();
        }
        from_big(ops::neg(iref(a))).into_raw()
    }
}
unsafe extern "C" fn int_pos(a: *mut PyObject) -> *mut PyObject {
    unsafe { exact_copy(&*a).into_raw() }
}
unsafe extern "C" fn int_abs(a: *mut PyObject) -> *mut PyObject {
    unsafe {
        let a = &*a;
        if sign(a) < 0 { int_neg(a.as_ptr()) } else { exact_copy(a).into_raw() }
    }
}
unsafe extern "C" fn int_invert(a: *mut PyObject) -> *mut PyObject {
    unsafe {
        let a = &*a;
        if is_compact(a) {
            return new_int(!compact_value(a)).into_raw();
        }
        from_big(ops::invert(iref(a))).into_raw()
    }
}
unsafe extern "C" fn int_bool(a: *mut PyObject) -> c_int {
    unsafe { (sign(&*a) != 0) as c_int }
}
unsafe extern "C" fn int_int(a: *mut PyObject) -> *mut PyObject {
    unsafe { exact_copy(&*a).into_raw() }
}
unsafe extern "C" fn int_float(a: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(to_f64(&*a).map(float::new_float)) }
}

/// Copy to an exact int (identity for exact ints).
pub fn exact_copy(a: &PyObject) -> PyRef {
    if is_int_exact(a) {
        return a.to_ref();
    }
    from_digits(sign(a) < 0, digits(a))
}

/// Parse an int from a str/bytes with base (int(s, base)).
pub fn parse_int_obj(s: &PyObject, base: i64) -> PyResult {
    let text: Vec<u8> = if pystr::is_str(s) {
        // map Unicode digits / whitespace to ASCII
        let mut v = Vec::with_capacity(pystr::str_len(s));
        for c in pystr::str_data(s).iter() {
            if c < 128 {
                v.push(c as u8);
            } else if let Some(d) = rpy_unicode::to_decimal(c) {
                v.push(b'0' + d as u8);
            } else if rpy_unicode::is_space(c) {
                v.push(b' ');
            } else {
                v.push(0xff);
            }
        }
        v
    } else {
        crate::types::bytes::bytes_like_data(s).unwrap_or_default()
    };
    let limit = crate::modules::sys::int_max_str_digits();
    match rpy_bigint::radix::parse_int(&text, base as u32, limit as usize) {
        Ok(b) => Ok(from_big(b)),
        Err(IntError::InvalidLiteral { base }) => {
            let r = ab::repr_string(s)?;
            raise!(ValueError, "invalid literal for int() with base {}: {}", base, r);
        }
        Err(IntError::StrToIntDigitLimit { digits, limit }) => raise!(
            ValueError,
            "Exceeds the limit ({}) for integer string conversion: value has {} digits; use sys.set_int_max_str_digits() to increase the limit",
            limit,
            digits
        ),
        Err(e) => Err(int_error(e)),
    }
}

/// int(x) conversion (PyNumber_Long).
pub fn number_long(o: &PyObject) -> PyResult {
    if is_int_exact(o) {
        return Ok(o.to_ref());
    }
    unsafe {
        let nb = o.ty().tp_as_number;
        if !nb.is_null() {
            if let Some(f) = (*nb).nb_int {
                let r = from_c(f(o.as_ptr()))?;
                if !is_int(&r) {
                    raise!(TypeError, "__int__ returned non-int (type {})", tname(&r));
                }
                if !is_int_exact(&r) {
                    crate::modules::warnings::warn(
                        crate::types::exceptions::DeprecationWarning(),
                        &format!("__int__ returned non-int (type {}).  The ability to return an instance of a strict subclass of int is deprecated, and may be removed in a future version of Python.", tname(&r)),
                        1,
                    )?;
                    return Ok(exact_copy(&r));
                }
                return Ok(r);
            }
            if (*nb).nb_index.is_some() {
                return Ok(exact_copy(&*ab::index(o)?));
            }
        }
    }
    if let Some(t) = ab::lookup_special(o, crate::name!("__trunc__"))? {
        let _ = t;
    }
    if pystr::is_str(o) || crate::types::bytes::is_bytes(o) || crate::types::bytearray::is_bytearray(o) {
        return parse_int_obj(o, 10);
    }
    if let Some(b) = crate::buffer::buffer_bytes(o) {
        let bo = crate::types::bytes::new_bytes(&b);
        return parse_int_obj(&bo, 10);
    }
    raise!(TypeError, "int() argument must be a string, a bytes-like object or a real number, not '{}'", tname(o));
}

unsafe extern "C" fn int_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        static P: crate::args::Params = crate::args::Params::new("int", &["x", "base"], 1, 0);
        let r = (|| -> PyResult {
            let (all, kwn) = crate::args::kwdict_to_vectorcall(&*args, kw);
            let refs: Vec<&PyObject> = all.iter().map(|r| &**r).collect();
            let [x, base] = crate::args::parse::<2>(&P, &refs, kwn.as_deref())?;
            let v = match (x, base) {
                (None, None) => new_int(0),
                (None, Some(_)) => raise!(TypeError, "int() missing string argument"),
                (Some(x), None) => number_long(x)?,
                (Some(x), Some(b)) => {
                    let b = as_i64(&*ab::index(b)?).unwrap_or(-1);
                    if b != 0 && !(2..=36).contains(&b) {
                        raise!(ValueError, "int() base must be >= 2 and <= 36, or 0");
                    }
                    if !(pystr::is_str(x) || crate::types::bytes::is_bytes(x) || crate::types::bytearray::is_bytearray(x)) {
                        raise!(TypeError, "int() can't convert non-string with explicit base");
                    }
                    parse_int_obj(x, b)?
                }
            };
            if tp == int_type() {
                return Ok(v);
            }
            // subclass: copy digits into an instance of tp
            let n = digits(&v).len();
            let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
            let op = alloc(tp, n.max(1) as isize);
            if op.is_null() {
                return Err(PyErr);
            }
            let d = digits_mut(op, n.max(1));
            d[..n].copy_from_slice(digits(&v));
            set_sign_and_ndigits(op, sign(&v), n);
            Ok(PyRef::from_raw(op))
        })();
        into_c(r)
    }
}

fn int_bit_length(o: &PyObject) -> PyResult {
    let d = digits(o);
    if d.is_empty() {
        return Ok(new_int(0));
    }
    let top = *d.last().unwrap();
    Ok(new_int(((d.len() - 1) * 30) as i64 + (32 - top.leading_zeros()) as i64))
}
fn int_bit_count(o: &PyObject) -> PyResult {
    Ok(new_int(digits(o).iter().map(|d| d.count_ones() as i64).sum()))
}
fn int_conjugate(o: &PyObject) -> PyResult {
    Ok(exact_copy(o))
}
fn int_is_integer(_o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(true))
}
fn int_as_integer_ratio(o: &PyObject) -> PyResult {
    Ok(tuple::new_tuple(vec![exact_copy(o), new_int(1)]))
}
fn int_getnewargs(o: &PyObject) -> PyResult {
    Ok(tuple::new_tuple(vec![exact_copy(o)]))
}
fn int_trunc(o: &PyObject) -> PyResult {
    Ok(exact_copy(o))
}
fn int_round(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("__round__", args, 0, 1)?;
    match args.first() {
        None => Ok(exact_copy(o)),
        Some(n) if n.is(none::none()) => Ok(exact_copy(o)),
        Some(n) => {
            let n = ab::index(n)?;
            if sign(&n) >= 0 {
                return Ok(exact_copy(o));
            }
            let k = match rpy_bigint::conv::to_u64(ops::neg(iref(&n)).as_ref()) {
                Some(k) => k,
                None => return Ok(new_int(0)),
            };
            Ok(from_big(ops::round_neg_ndigits(iref(o), k)))
        }
    }
}
fn int_sizeof(o: &PyObject) -> PyResult {
    Ok(new_int(o.ty().tp_basicsize as i64 + 4 * digits(o).len().max(1) as i64))
}

/// `byteorder` argument of to_bytes/from_bytes: Ok(true) for little-endian.
fn byteorder_arg(fname: &str, b: Option<&PyObject>) -> PyResult<bool> {
    match b {
        None => Ok(false),
        Some(b) => {
            if !pystr::is_str(b) {
                raise!(TypeError, "{}() argument 'byteorder' must be str, not {}", fname, tname(b));
            }
            if pystr::str_eq_ascii(b, "little") {
                Ok(true)
            } else if pystr::str_eq_ascii(b, "big") {
                Ok(false)
            } else {
                raise!(ValueError, "byteorder must be either 'little' or 'big'");
            }
        }
    }
}

/// PyObject_Bytes-like conversion used by int.from_bytes.
fn bytes_of(b: &PyObject) -> PyResult<Vec<u8>> {
    if crate::types::bytes::is_bytes(b) {
        return Ok(crate::types::bytes::bytes_data(b).to_vec());
    }
    if let Some(f) = ab::lookup_special(b, crate::name!("__bytes__"))? {
        let r = ab::call0(&f)?;
        if !crate::types::bytes::is_bytes(&r) {
            raise!(TypeError, "__bytes__ returned non-bytes (type {})", tname(&r));
        }
        return Ok(crate::types::bytes::bytes_data(&r).to_vec());
    }
    if let Some(d) = crate::types::bytes::bytes_like_data(b) {
        return Ok(d);
    }
    if pystr::is_str(b) || is_int(b) {
        raise!(TypeError, "cannot convert '{}' object to bytes", tname(b));
    }
    let it = match ab::get_iter(b) {
        Ok(it) => it,
        Err(_) => {
            if exc_matches(crate::types::exceptions::TypeError()) {
                clear_exc();
                raise!(TypeError, "cannot convert '{}' object to bytes", tname(b));
            }
            return Err(PyErr);
        }
    };
    let mut out = Vec::new();
    while let Some(x) = ab::iter_next(&it)? {
        let n = ab::index(&x)?;
        match as_i64(&n) {
            Some(v) if (0..256).contains(&v) => out.push(v as u8),
            _ => raise!(ValueError, "bytes must be in range(0, 256)"),
        }
    }
    Ok(out)
}
fn int_format(o: &PyObject, spec: &PyObject) -> PyResult {
    crate::types::format_impl::format_int(o, spec)
}
fn int_to_bytes(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("to_bytes", &["length", "byteorder", "signed"], 0, 0).kwonly(1);
    let [length, byteorder, signed] = crate::args::parse::<3>(&P, args, kw)?;
    let length = match length {
        Some(l) => ab::as_i64(l)?,
        None => 1,
    };
    if length < 0 {
        raise!(ValueError, "length argument must be non-negative");
    }
    let little = byteorder_arg("to_bytes", byteorder)?;
    let signed = match signed {
        Some(s) => ab::is_true(s)?,
        None => false,
    };
    match rpy_bigint::conv::to_bytes(iref(o), length as usize, little, signed) {
        Ok(b) => Ok(crate::types::bytes::new_bytes(&b)),
        Err(e) => Err(int_error(e)),
    }
}
fn int_from_bytes(cls: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("from_bytes", &["bytes", "byteorder", "signed"], 0, 1).kwonly(1);
    let [b, byteorder, signed] = crate::args::parse::<3>(&P, args, kw)?;
    let b = b.unwrap();
    let data = bytes_of(b)?;
    let little = byteorder_arg("from_bytes", byteorder)?;
    let signed = match signed {
        Some(s) => ab::is_true(s)?,
        None => false,
    };
    let v = from_big(rpy_bigint::conv::from_bytes(&data, little, signed));
    if cls.is(type_obj(int_type())) {
        return Ok(v);
    }
    ab::call(cls, &[&v])
}
fn get_real(o: &PyObject) -> PyResult {
    Ok(exact_copy(o))
}
fn get_imag(_o: &PyObject) -> PyResult {
    Ok(new_int(0))
}
fn get_denominator(_o: &PyObject) -> PyResult {
    Ok(new_int(1))
}

static mut INT_AS_NUMBER: PyNumberMethods = PyNumberMethods::ZERO;

pub fn init_types() {
    unsafe {
        let n = &mut *(&raw mut INT_AS_NUMBER);
        n.nb_add = Some(int_add);
        n.nb_subtract = Some(int_sub);
        n.nb_multiply = Some(int_mul);
        n.nb_remainder = Some(int_mod);
        n.nb_divmod = Some(int_divmod);
        n.nb_power = Some(int_pow);
        n.nb_negative = Some(int_neg);
        n.nb_positive = Some(int_pos);
        n.nb_absolute = Some(int_abs);
        n.nb_bool = Some(int_bool);
        n.nb_invert = Some(int_invert);
        n.nb_lshift = Some(int_lshift);
        n.nb_rshift = Some(int_rshift);
        n.nb_and = Some(int_and);
        n.nb_xor = Some(int_xor);
        n.nb_or = Some(int_or);
        n.nb_int = Some(int_int);
        n.nb_float = Some(int_float);
        n.nb_floor_divide = Some(int_floordiv);
        n.nb_true_divide = Some(int_truediv);
        n.nb_index = Some(int_int);
        let t = &mut *int_type();
        t.tp_name = c"int".as_ptr();
        t.tp_basicsize = std::mem::offset_of!(PyLongObject, ob_digit) as isize;
        t.tp_itemsize = 4;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_LONG_SUBCLASS;
        t.tp_doc = c"int([x]) -> integer\nint(x, base=10) -> integer\n\nConvert a number or a string to an integer; with no argument return 0.\n\nNumbers are converted with their __int__() method (floats are truncated\ntoward zero).  A str, bytes or bytearray x is parsed as an integer literal\nin the given base (2 to 36, default 10); base 0 reads the base from the\nliteral's prefix, as in Python source:\n\n>>> int('0x1f', base=0)\n31".as_ptr();
        t.tp_repr = Some(int_repr);
        t.tp_hash = Some(int_hash_slot);
        t.tp_richcompare = Some(int_richcompare);
        t.tp_as_number = n;
        t.tp_new = Some(int_new);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_alloc = Some(generic_alloc);
        t.tp_free = Some(generic_free);
        t.tp_methods = crate::mdefs![
            ("bit_length", NOARGS, int_bit_length, "bit_length($self, /)\n--\n\nReturn how many bits are needed to write abs(self) in binary.\n\n>>> bin(100)\n'0b1100100'\n>>> (100).bit_length()\n7"),
            ("bit_count", NOARGS, int_bit_count, "bit_count($self, /)\n--\n\nReturn how many 1 bits there are in abs(self) written in binary.\n\n>>> bin(22)\n'0b10110'\n>>> (22).bit_count()\n3"),
            ("conjugate", NOARGS, int_conjugate, "Returns self, the complex conjugate of any int."),
            ("is_integer", NOARGS, int_is_integer, "Returns True. Exists for duck type compatibility with float.is_integer."),
            ("as_integer_ratio", NOARGS, int_as_integer_ratio, "as_integer_ratio($self, /)\n--\n\nReturn the pair (self, 1): an exact fraction with a positive denominator.\n\n>>> (7).as_integer_ratio()\n(7, 1)\n>>> (-3).as_integer_ratio()\n(-3, 1)\n>>> (0).as_integer_ratio()\n(0, 1)"),
            ("to_bytes", FASTKW, int_to_bytes, "to_bytes($self, /, length=1, byteorder='big', *, signed=False)\n--\n\nReturn an array of bytes representing an integer."),
            ("from_bytes", FASTKW, int_from_bytes, "from_bytes($type, /, bytes, byteorder='big', *, signed=False)\n--\n\nReturn the integer represented by the given array of bytes."; METH_CLASS),
            ("__trunc__", NOARGS, int_trunc, "Truncating an Integral returns itself."),
            ("__floor__", NOARGS, int_trunc, "Flooring an Integral returns itself."),
            ("__ceil__", NOARGS, int_trunc, "Ceiling of an Integral returns itself."),
            ("__round__", FAST, int_round, "__round__($self, ndigits=None, /)\n--\n\nRound to the given number of decimal digits; the result is always an int."),
            ("__getnewargs__", NOARGS, int_getnewargs),
            ("__format__", O, int_format, "__format__($self, format_spec, /)\n--\n\nConvert to a string according to format_spec."),
            ("__sizeof__", NOARGS, int_sizeof, "Returns size in memory, in bytes."),
        ];
        t.tp_getset = crate::getset_table![
            ("real", get_real; "the real part of a complex number"),
            ("imag", get_imag; "the imaginary part of a complex number"),
            ("numerator", get_real; "the numerator of a rational number in lowest terms"),
            ("denominator", get_denominator; "the denominator of a rational number in lowest terms"),
        ];
    }
}
