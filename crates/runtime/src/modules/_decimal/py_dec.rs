//! `_decimal.Decimal`: the Python type, conversions, number protocol, methods,
//! and the arithmetic methods of `Context`.

use super::dec::*;
use super::format as fmt;
use super::nat::{self, Rest};
use super::ops;
use super::py_ctx::{self, add_status, ctx_arg, current, engine, st};
use super::transc;
use crate::abstract_ as ab;
use crate::args::{self, Params};
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::int_methods::{from_big, iref};
use crate::types::typeobject::*;
use crate::types::{bool_, complex, dict, float, int, list, none, str as pystr, tuple};
use std::borrow::Cow;
use std::ffi::c_int;
use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};

#[repr(C)]
pub struct DecObj {
    ob_base: PyObject,
    hash: AtomicIsize,
    pub dec: Dec,
}

#[unsafe(no_mangle)]
pub static mut PyDec_Type: PyTypeObject = PyTypeObject::ZERO;

#[inline]
pub fn decimal_type() -> *mut PyTypeObject {
    &raw mut PyDec_Type
}

#[inline]
pub fn is_dec(o: &PyObject) -> bool {
    o.is_type(decimal_type()) || is_subtype(o.type_ptr(), decimal_type())
}

#[inline]
pub fn dec_of(o: &PyObject) -> &Dec {
    unsafe { &(*(o.as_ptr() as *const DecObj)).dec }
}

pub fn new_dec_typed(tp: *mut PyTypeObject, d: Dec) -> PyResult {
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
        let op = alloc(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        let o = &mut *(op as *mut DecObj);
        std::ptr::write(&mut o.hash, AtomicIsize::new(-1));
        std::ptr::write(&mut o.dec, d);
        Ok(PyRef::from_raw(op))
    }
}

#[inline]
pub fn new_dec(d: Dec) -> PyRef {
    unsafe {
        let tp = decimal_type();
        let op = generic_alloc(tp, 0);
        assert!(!op.is_null(), "out of memory");
        let o = &mut *(op as *mut DecObj);
        std::ptr::write(&mut o.hash, AtomicIsize::new(-1));
        std::ptr::write(&mut o.dec, d);
        PyRef::from_raw(op)
    }
}

unsafe extern "C" fn dec_dealloc(o: *mut PyObject) {
    unsafe {
        let tp = (*o).type_ptr();
        std::ptr::drop_in_place(&mut (*(o as *mut DecObj)).dec);
        generic_free_and_type_decref(o, tp);
    }
}

// ------------------------------- conversions ----------------------------------------

pub fn int_to_dec(o: &PyObject) -> Dec {
    if let Some(v) = int::as_i64(o) {
        return Dec::from_i64(v);
    }
    let s = rpy_bigint::radix::to_string(iref(o), 10);
    let (neg, digits) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.as_str()),
    };
    Dec::finite(neg, nat::from_digits(digits.as_bytes()), 0)
}

/// Python int from a natural number with sign.
pub fn nat_to_int(neg: bool, c: &[u64]) -> PyRef {
    if let Some(v) = nat::to_u128(c) {
        if v <= i128::MAX as u128 {
            let v = v as i128;
            return int::new_int_i128(if neg { -v } else { v });
        }
    }
    let s = nat::to_string(c);
    let mag = rpy_bigint::radix::from_digits(s.as_bytes(), 10).unwrap();
    from_big(rpy_bigint::BigInt::from_parts(neg, mag))
}

pub fn f64_to_dec(f: f64) -> Dec {
    let neg = f.is_sign_negative();
    if f.is_nan() {
        let mut d = Dec::nan();
        d.neg = neg;
        return d;
    }
    if f.is_infinite() {
        return Dec::inf(neg);
    }
    if f == 0.0 {
        return Dec::finite(neg, vec![], 0);
    }
    let bits = f.to_bits();
    let bexp = ((bits >> 52) & 0x7ff) as i64;
    let frac = bits & ((1u64 << 52) - 1);
    let (mut m, mut e) = if bexp == 0 { (frac, -1074i64) } else { (frac | (1u64 << 52), bexp - 1075) };
    let tz = m.trailing_zeros() as i64;
    m >>= tz;
    e += tz;
    if e >= 0 {
        let c = if e < 64 && (m as u128) << e < (1u128 << 126) {
            nat::from_u128((m as u128) << e)
        } else {
            nat::mul(&nat::from_u64(m), &nat::pow(&[2], e as u64))
        };
        Dec::finite(neg, c, 0)
    } else {
        let k = (-e) as u64;
        let c = nat::mul(&nat::from_u64(m), &nat::pow(&[5], k));
        Dec::finite(neg, c, -(k as i64))
    }
}

/// Convert an operand that must be Decimal or int.
#[inline]
pub fn convert_op(o: &PyObject) -> Option<Cow<'_, Dec>> {
    if is_dec(o) {
        Some(Cow::Borrowed(dec_of(o)))
    } else if int::is_int(o) {
        Some(Cow::Owned(int_to_dec(o)))
    } else {
        None
    }
}

pub fn convert_raise(o: &PyObject) -> PyResult<Cow<'_, Dec>> {
    match convert_op(o) {
        Some(d) => Ok(d),
        None => raise!(TypeError, "conversion from {} to Decimal is not supported", tname(o)),
    }
}

/// ASCII form of a numeric string: unicode digits mapped to ASCII, optional
/// whitespace stripping and underscore removal. None for unconvertible chars.
fn str_ascii(o: &PyObject, strip: bool, underscores: bool) -> Option<Vec<u8>> {
    if let Some(s) = pystr::ascii_str(o) {
        let b = s.as_bytes();
        let b = if strip {
            let st = b.iter().position(|c| !c.is_ascii_whitespace() && !is_ascii_space_extra(*c)).unwrap_or(b.len());
            let en = b.iter().rposition(|c| !c.is_ascii_whitespace() && !is_ascii_space_extra(*c)).map_or(st, |i| i + 1);
            &b[st..en.max(st)]
        } else {
            b
        };
        if underscores && b.contains(&b'_') {
            return Some(b.iter().copied().filter(|c| *c != b'_').collect());
        }
        return Some(b.to_vec());
    }
    let cps: Vec<u32> = pystr::str_data(o).iter().collect();
    let mut st = 0;
    let mut en = cps.len();
    if strip {
        while st < en && rpy_unicode::is_space(cps[st]) {
            st += 1;
        }
        while en > st && rpy_unicode::is_space(cps[en - 1]) {
            en -= 1;
        }
    }
    let mut out = Vec::with_capacity(en - st);
    for &c in &cps[st..en] {
        if c < 128 {
            if underscores && c == b'_' as u32 {
                continue;
            }
            out.push(c as u8);
        } else if let Some(d) = rpy_unicode::to_decimal(c) {
            out.push(b'0' + d as u8);
        } else {
            return None;
        }
    }
    Some(out)
}

fn is_ascii_space_extra(c: u8) -> bool {
    // \x1c-\x1f and \x0b are whitespace for str.strip()
    matches!(c, 0x0b | 0x1c..=0x1f)
}

/// Exact result of a parsed value (Decimal(...) semantics): InvalidOperation if the
/// value does not fit the maximum context.
fn exact(d: Dec, status: &mut u32) -> Dec {
    if d.is_nan() {
        if nat::digits(&d.coef) as i64 > MAX_PREC {
            *status |= INVALID_OPERATION;
            return Dec::nan();
        }
        return d;
    }
    if d.is_inf() {
        return d;
    }
    let mut mc = Ctx::max_ctx();
    let r = finalize(d, &mut mc, Rest::Zero);
    if mc.status & (INEXACT | ROUNDED | CLAMPED | OVERFLOW | UNDERFLOW) != 0 {
        *status |= INVALID_OPERATION;
        return Dec::nan();
    }
    r
}

/// Rounded result of a value for context `ctx` (create_decimal semantics).
fn rounded(d: Dec, ctx: &mut Ctx) -> Dec {
    if d.is_nan() {
        if nat::digits(&d.coef) as i64 > ctx.prec - ctx.clamp as i64 {
            ctx.status |= CONVERSION_SYNTAX;
            return Dec::nan();
        }
        return d;
    }
    if d.is_inf() {
        return d;
    }
    finalize(d, ctx, Rest::Zero)
}

fn tuple_to_dec(v: &PyObject) -> PyResult<Dec> {
    let items = if tuple::is_tuple(v) { tuple::tuple_items(v).iter().map(|x| x.to_ref()).collect::<Vec<_>>() } else { ab::collect_iterable(v)? };
    if items.len() != 3 {
        raise!(ValueError, "argument must be a sequence of length 3");
    }
    let sign = &items[0];
    let neg = if int::is_int(sign) {
        match int::as_i64(sign) {
            Some(0) => false,
            Some(1) => true,
            _ => raise!(ValueError, "sign must be an integer with the value 0 or 1"),
        }
    } else {
        raise!(ValueError, "sign must be an integer with the value 0 or 1");
    };
    let digits_obj = &items[1];
    if !(tuple::is_tuple(digits_obj) || list::is_list(digits_obj)) {
        raise!(ValueError, "coefficient must be a tuple of digits");
    }
    let dl = ab::collect_iterable(digits_obj)?;
    let mut digits = Vec::with_capacity(dl.len());
    for x in &dl {
        match if int::is_int(x) { int::as_i64(x) } else { None } {
            Some(d) if (0..=9).contains(&d) => digits.push(b'0' + d as u8),
            _ => raise!(ValueError, "coefficient must be a tuple of digits"),
        }
    }
    let e = &items[2];
    let (kind, exp) = if pystr::is_str(e) {
        match pystr::to_string_lossy(e).as_str() {
            "F" => (Kind::Inf, 0),
            "n" => (Kind::NaN, 0),
            "N" => (Kind::SNaN, 0),
            _ => raise!(ValueError, "string argument in the third position must be 'F', 'n' or 'N'"),
        }
    } else if int::is_int(e) {
        match int::as_i64(e) {
            Some(x) => (Kind::Finite, x),
            None => raise!(OverflowError, "exponent must be an integer"),
        }
    } else {
        raise!(ValueError, "exponent must be an integer");
    };
    Ok(match kind {
        Kind::Inf => Dec::inf(neg),
        Kind::Finite => Dec::finite(neg, nat::from_digits(&digits), exp),
        k => Dec { neg, kind: k, exp: 0, coef: nat::from_digits(&digits) },
    })
}

/// Convert an arbitrary value for Decimal()/create_decimal().
fn value_to_dec(v: &PyObject, ctxo: &PyObject, exact_mode: bool) -> PyResult<Dec> {
    let mut status = 0u32;
    let mut e = engine(ctxo);
    let r = if is_dec(v) {
        let d = dec_of(v).clone();
        if exact_mode { d } else { rounded(d, &mut e) }
    } else if pystr::is_str(v) {
        let parsed = str_ascii(v, exact_mode, exact_mode).and_then(|b| parse(&b));
        match parsed {
            None => {
                status |= CONVERSION_SYNTAX;
                Dec::nan()
            }
            Some(d) => {
                if exact_mode {
                    exact(d, &mut status)
                } else {
                    rounded(d, &mut e)
                }
            }
        }
    } else if int::is_int(v) {
        let d = int_to_dec(v);
        if exact_mode { d } else { finalize(d, &mut e, Rest::Zero) }
    } else if float::is_float(v) {
        add_status(ctxo, FLOAT_OPERATION)?;
        let d = f64_to_dec(float::float_value(v));
        if exact_mode { d } else { rounded(d, &mut e) }
    } else if tuple::is_tuple(v) || list::is_list(v) {
        let d = tuple_to_dec(v)?;
        if exact_mode { exact(d, &mut status) } else { rounded(d, &mut e) }
    } else {
        raise!(TypeError, "conversion from {} to Decimal is not supported", tname(v));
    };
    add_status(ctxo, status | e.status)?;
    Ok(r)
}

unsafe extern "C" fn dec_new(tp: *mut PyTypeObject, a: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let (v, kwn) = args::kwdict_to_vectorcall(&*a, kw);
            let refs: Vec<&PyObject> = v.iter().map(|x| &**x).collect();
            let [value, context] = args::parse::<2>(&Params::new("Decimal", &["value", "context"], 0, 0), &refs, kwn.as_deref())?;
            let ctxo = ctx_arg(context)?;
            let d = match value {
                None => Dec::zero(),
                Some(v) => {
                    if tp == decimal_type() && v.is_type(decimal_type()) {
                        return Ok(v.to_ref());
                    }
                    value_to_dec(v, &ctxo, true)?
                }
            };
            if tp == decimal_type() { Ok(new_dec(d)) } else { new_dec_typed(tp, d) }
        })();
        into_c(r)
    }
}

// ------------------------------- helpers ----------------------------------------

#[inline]
fn run1(c: &PyObject, f: impl FnOnce(&mut Ctx) -> Dec) -> PyResult {
    let mut e = engine(c);
    let r = f(&mut e);
    add_status(c, e.status)?;
    Ok(new_dec(r))
}

#[inline]
fn ni() -> PyResult {
    Ok(none::not_implemented().to_ref())
}

fn dec_to_string(d: &Dec, eng: bool) -> PyResult<String> {
    let caps = match current() {
        Ok(c) => py_ctx::cobj(&c).capitals != 0,
        Err(_) => true,
    };
    Ok(to_string(d, eng, caps))
}

unsafe extern "C" fn dec_str(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(dec_to_string(dec_of(&*o), false).map(|s| pystr::new_str(&s))) }
}

unsafe extern "C" fn dec_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(dec_to_string(dec_of(&*o), false).map(|s| pystr::new_str(&format!("Decimal('{}')", s)))) }
}

/// Integer value (truncated toward zero) as Python int.
fn to_int(d: &Dec) -> PyResult {
    if d.is_nan() {
        raise!(ValueError, "cannot convert NaN to integer");
    }
    if d.is_inf() {
        raise!(OverflowError, "cannot convert Infinity to integer");
    }
    let c = if d.exp >= 0 {
        if d.coef.is_empty() { vec![] } else { nat::shl(&d.coef, d.exp as u64) }
    } else {
        nat::shr_trunc(&d.coef, (-(d.exp as i128)).min(u64::MAX as i128 / 2) as u64)
    };
    Ok(nat_to_int(d.neg && !c.is_empty(), &c))
}

fn to_int_rounded(d: &Dec, r: Round) -> PyResult {
    if d.is_nan() {
        raise!(ValueError, "cannot convert NaN to integer");
    }
    if d.is_inf() {
        raise!(OverflowError, "cannot convert Infinity to integer");
    }
    let x = if d.exp < 0 { ops::rescale_raw(d, 0, r).0 } else { d.clone() };
    to_int(&x)
}

fn to_float(d: &Dec) -> PyResult<f64> {
    if d.is_snan() {
        raise!(ValueError, "cannot convert signaling NaN to float");
    }
    if d.is_nan() {
        return Ok(if d.neg { -f64::NAN } else { f64::NAN });
    }
    if d.is_inf() {
        return Ok(if d.neg { f64::NEG_INFINITY } else { f64::INFINITY });
    }
    if d.coef.is_empty() {
        return Ok(if d.neg { -0.0 } else { 0.0 });
    }
    // fast path: small exactly representable values
    if let Some(c) = nat::to_u64(&d.coef) {
        if c < (1u64 << 53) && d.exp >= -22 && d.exp <= 22 {
            let v = c as f64;
            let r = if d.exp >= 0 { v * 10f64.powi(d.exp as i32) } else { v / 10f64.powi(-d.exp as i32) };
            if d.exp <= 0 || (c as f64) * 10f64.powi(d.exp as i32) < 9007199254740992.0 {
                return Ok(if d.neg { -r } else { r });
            }
        }
    }
    let s = to_string(d, false, true);
    rpy_numfmt::parse::parse_float(&s).map_err(|_| err!(ValueError, "could not convert string to float"))
}

// ------------------------------- number slots ----------------------------------------

macro_rules! binslot {
    ($name:ident, $op:expr) => {
        unsafe extern "C" fn $name(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
            unsafe {
                let r = (|| -> PyResult {
                    let (a, b) = (&*a, &*b);
                    let x = match convert_op(a) {
                        Some(x) => x,
                        None => return ni(),
                    };
                    let y = match convert_op(b) {
                        Some(y) => y,
                        None => return ni(),
                    };
                    let c = current()?;
                    let f: fn(&Dec, &Dec, &mut Ctx) -> Dec = $op;
                    run1(&c, |e| f(&x, &y, e))
                })();
                into_c(r)
            }
        }
    };
}

binslot!(nb_add, ops::add);
binslot!(nb_sub, ops::sub);
binslot!(nb_mul, ops::mul);
binslot!(nb_truediv, ops::div);
binslot!(nb_floordiv, ops::divint);
binslot!(nb_rem, ops::rem);

unsafe extern "C" fn nb_divmod(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let x = match convert_op(&*a) {
                Some(x) => x,
                None => return ni(),
            };
            let y = match convert_op(&*b) {
                Some(y) => y,
                None => return ni(),
            };
            let c = current()?;
            divmod_impl(&c, &x, &y)
        })();
        into_c(r)
    }
}

fn divmod_impl(c: &PyObject, x: &Dec, y: &Dec) -> PyResult {
    let mut e = engine(c);
    let (q, r) = ops::divmod(x, y, &mut e);
    add_status(c, e.status)?;
    Ok(tuple::new_tuple(vec![new_dec(q), new_dec(r)]))
}

unsafe extern "C" fn nb_pow(a: *mut PyObject, b: *mut PyObject, m: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let x = match convert_op(&*a) {
                Some(x) => x,
                None => return ni(),
            };
            let y = match convert_op(&*b) {
                Some(y) => y,
                None => return ni(),
            };
            let c = current()?;
            if (*m).is(none::none()) {
                run1(&c, |e| transc::power(&x, &y, e))
            } else {
                let z = match convert_op(&*m) {
                    Some(z) => z,
                    None => return ni(),
                };
                run1(&c, |e| transc::power_mod(&x, &y, &z, e))
            }
        })();
        into_c(r)
    }
}

macro_rules! unslot {
    ($name:ident, $op:expr) => {
        unsafe extern "C" fn $name(a: *mut PyObject) -> *mut PyObject {
            unsafe {
                let r = (|| -> PyResult {
                    let x = dec_of(&*a);
                    let c = current()?;
                    let f: fn(&Dec, &mut Ctx) -> Dec = $op;
                    run1(&c, |e| f(x, e))
                })();
                into_c(r)
            }
        }
    };
}

unslot!(nb_neg, ops::minus);
unslot!(nb_pos, ops::plus);
unslot!(nb_abs, ops::abs);

unsafe extern "C" fn nb_bool(a: *mut PyObject) -> c_int {
    unsafe { !dec_of(&*a).is_zero() as c_int }
}

unsafe extern "C" fn nb_int(a: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(to_int(dec_of(&*a))) }
}

unsafe extern "C" fn nb_float(a: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(to_float(dec_of(&*a)).map(float::new_float)) }
}

// ------------------------------- comparison & hash ----------------------------------------

static RATIONAL: std::sync::OnceLock<Option<PyRef>> = std::sync::OnceLock::new();

fn rational_cls() -> Option<&'static PyRef> {
    RATIONAL
        .get_or_init(|| {
            let r = (|| -> PyResult<PyRef> {
                let m = crate::import::import_absolute("numbers")?;
                ab::getattr_str(&m, "Rational")
            })();
            match r {
                Ok(c) => Some(c),
                Err(_) => {
                    clear_exc();
                    None
                }
            }
        })
        .as_ref()
}

fn op_result(o: std::cmp::Ordering, op: c_int) -> bool {
    use std::cmp::Ordering::*;
    match op {
        Py_LT => o == Less,
        Py_LE => o != Greater,
        Py_EQ => o == Equal,
        Py_NE => o != Equal,
        Py_GT => o == Greater,
        _ => o != Less,
    }
}

unsafe extern "C" fn dec_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe { into_c(richcompare(&*a, &*b, op)) }
}

fn richcompare(a: &PyObject, b: &PyObject, op: c_int) -> PyResult {
    let x = dec_of(a);
    let eqop = op == Py_EQ || op == Py_NE;
    let mut float_op = false;
    let y: Cow<Dec>;
    let mut rational: Option<(Dec, Dec)> = None;
    if is_dec(b) {
        y = Cow::Borrowed(dec_of(b));
    } else if int::is_int(b) {
        y = Cow::Owned(int_to_dec(b));
    } else if float::is_float(b) {
        float_op = true;
        y = Cow::Owned(f64_to_dec(float::float_value(b)));
    } else if complex::is_complex(b) {
        if !eqop {
            return ni();
        }
        let cv = complex::complex_value(b);
        if cv.imag != 0.0 {
            return Ok(bool_::new_bool(op == Py_NE));
        }
        float_op = true;
        y = Cow::Owned(f64_to_dec(cv.real));
    } else if let Some(rc) = rational_cls().filter(|rc| ab::isinstance(b, rc).unwrap_or(false)) {
        let _ = rc;
        let num = ab::getattr_str(b, "numerator")?;
        let den = ab::getattr_str(b, "denominator")?;
        if !int::is_int(&num) || !int::is_int(&den) {
            return ni();
        }
        rational = Some((int_to_dec(&num), int_to_dec(&den)));
        y = Cow::Owned(Dec::zero());
    } else {
        return ni();
    }
    let c = current()?;
    if float_op {
        if eqop {
            py_ctx::cobj(&c).flags.fetch_or(FLOAT_OPERATION, Relaxed);
        } else {
            add_status(&c, FLOAT_OPERATION)?;
        }
    }
    if x.is_nan() || y.is_nan() {
        if eqop {
            if x.is_snan() || y.is_snan() {
                add_status(&c, INVALID_OPERATION)?;
            }
            return Ok(bool_::new_bool(op == Py_NE));
        }
        add_status(&c, INVALID_OPERATION)?;
        return Ok(bool_::new_bool(false));
    }
    let ord = match rational {
        Some((num, den)) => {
            if x.is_inf() {
                if x.neg { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater }
            } else {
                let lhs = ops::mul_exact(x, &den);
                ops::cmp_num(&lhs, &num)
            }
        }
        None => ops::cmp_num(x, &y),
    };
    Ok(bool_::new_bool(op_result(ord, op)))
}

const HASH_P: u64 = (1u64 << 61) - 1;

fn mulmod(a: u64, b: u64) -> u64 {
    ((a as u128 * b as u128) % HASH_P as u128) as u64
}
fn powmod(mut b: u64, mut e: u64) -> u64 {
    let mut r = 1u64;
    b %= HASH_P;
    while e > 0 {
        if e & 1 == 1 {
            r = mulmod(r, b);
        }
        b = mulmod(b, b);
        e >>= 1;
    }
    r
}

pub fn hash_dec(d: &Dec, o: &PyObject) -> PyResult<isize> {
    match d.kind {
        Kind::SNaN => raise!(TypeError, "Cannot hash a signaling NaN value."),
        Kind::NaN => return Ok(crate::hash::hash_pointer(o.as_ptr() as usize)),
        Kind::Inf => return Ok(if d.neg { -crate::hash::HASH_INF } else { crate::hash::HASH_INF }),
        Kind::Finite => {}
    }
    let mut acc: u64 = 0;
    let bm = BASE_MOD;
    for &l in d.coef.iter().rev() {
        acc = ((acc as u128 * bm as u128 + l as u128) % HASH_P as u128) as u64;
    }
    let e = if d.exp >= 0 { powmod(10, d.exp as u64) } else { powmod(INV10, d.exp.unsigned_abs()) };
    let mut h = mulmod(acc, e) as i64;
    if d.neg {
        h = -h;
    }
    if h == -1 {
        h = -2;
    }
    Ok(h as isize)
}

const BASE_MOD: u64 = nat::BASE % HASH_P;
// inverse of 10 modulo 2^61-1
const INV10: u64 = 2075258708292324556;

unsafe extern "C" fn dec_hash(o: *mut PyObject) -> Py_hash_t {
    unsafe {
        let obj = &*(o as *const DecObj);
        let h = obj.hash.load(Relaxed);
        if h != -1 {
            return h;
        }
        match hash_dec(&obj.dec, &*o) {
            Ok(h) => {
                obj.hash.store(h, Relaxed);
                h
            }
            Err(_) => -1,
        }
    }
}

// ------------------------------- Decimal methods ----------------------------------------

fn kwparse<'a, const N: usize>(fname: &str, names: &'a [&'a str; N], a: &[&'a PyObject], kw: Option<&'a PyObject>) -> PyResult<[Option<&'a PyObject>; N]> {
    args::parse::<N>(&Params::new(fname, names, 0, 0), a, kw)
}

macro_rules! m_unary_ctx {
    ($f:ident, $name:expr, $op:expr) => {
        fn $f(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
            let [c] = kwparse($name, &["context"], a, kw)?;
            let c = ctx_arg(c)?;
            let f: fn(&Dec, &mut Ctx) -> Dec = $op;
            run1(&c, |e| f(dec_of(o), e))
        }
    };
}

m_unary_ctx!(m_exp, "exp", transc::exp);
m_unary_ctx!(m_ln, "ln", transc::ln);
m_unary_ctx!(m_log10, "log10", transc::log10);
m_unary_ctx!(m_logb, "logb", ops::logb);
m_unary_ctx!(m_logical_invert, "logical_invert", ops::logical_invert);
m_unary_ctx!(m_next_minus, "next_minus", ops::next_minus);
m_unary_ctx!(m_next_plus, "next_plus", ops::next_plus);
m_unary_ctx!(m_normalize, "normalize", ops::reduce);
m_unary_ctx!(m_sqrt, "sqrt", transc::sqrt);

macro_rules! m_binary_ctx {
    ($f:ident, $name:expr, $op:expr) => {
        fn $f(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
            let [other, c] = kwparse($name, &["other", "context"], a, kw)?;
            let c = ctx_arg(c)?;
            let other = match other {
                Some(x) => x,
                None => raise!(TypeError, "{}() missing required argument 'other' (pos 1)", $name),
            };
            let y = convert_raise(other)?;
            let f: fn(&Dec, &Dec, &mut Ctx) -> Dec = $op;
            run1(&c, |e| f(dec_of(o), &y, e))
        }
    };
}

m_binary_ctx!(m_compare, "compare", ops::compare);
m_binary_ctx!(m_compare_signal, "compare_signal", ops::compare_signal);
m_binary_ctx!(m_max, "max", ops::max);
m_binary_ctx!(m_max_mag, "max_mag", ops::max_mag);
m_binary_ctx!(m_min, "min", ops::min);
m_binary_ctx!(m_min_mag, "min_mag", ops::min_mag);
m_binary_ctx!(m_next_toward, "next_toward", ops::next_toward);
m_binary_ctx!(m_remainder_near, "remainder_near", ops::rem_near);
m_binary_ctx!(m_rotate, "rotate", ops::rotate);
m_binary_ctx!(m_scaleb, "scaleb", ops::scaleb);
m_binary_ctx!(m_shift, "shift", ops::shift);
m_binary_ctx!(m_logical_and, "logical_and", |a, b, c| ops::logical_op(a, b, b'&', c));
m_binary_ctx!(m_logical_or, "logical_or", |a, b, c| ops::logical_op(a, b, b'|', c));
m_binary_ctx!(m_logical_xor, "logical_xor", |a, b, c| ops::logical_op(a, b, b'^', c));
m_binary_ctx!(m_compare_total, "compare_total", |a, b, _c| ops::compare_total(a, b));
m_binary_ctx!(m_compare_total_mag, "compare_total_mag", |a, b, _c| ops::compare_total_mag(a, b));
m_binary_ctx!(m_copy_sign, "copy_sign", |a, b, _c| ops::copy_sign(a, b));

fn m_same_quantum(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [other, c] = kwparse("same_quantum", &["other", "context"], a, kw)?;
    ctx_arg(c)?;
    let other = match other {
        Some(x) => x,
        None => raise!(TypeError, "same_quantum() missing required argument 'other' (pos 1)"),
    };
    let y = convert_raise(other)?;
    Ok(bool_::new_bool(ops::same_quantum(dec_of(o), &y)))
}

fn m_fma(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [other, third, c] = kwparse("fma", &["other", "third", "context"], a, kw)?;
    let c = ctx_arg(c)?;
    let (Some(other), Some(third)) = (other, third) else {
        raise!(TypeError, "fma() missing required argument");
    };
    let y = convert_raise(other)?;
    let z = convert_raise(third)?;
    run1(&c, |e| ops::fma(dec_of(o), &y, &z, e))
}

fn rounding_arg(r: Option<&PyObject>, c: &PyObject) -> PyResult<Round> {
    match r {
        None => Ok(py_ctx::round_of(py_ctx::cobj(c))),
        Some(x) if x.is(none::none()) => Ok(py_ctx::round_of(py_ctx::cobj(c))),
        Some(x) => {
            if !pystr::is_str(x) {
                raise!(TypeError, "invalid rounding mode");
            }
            py_ctx::round_from_obj(x)
        }
    }
}

fn m_quantize(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [exp, rnd, c] = kwparse("quantize", &["exp", "rounding", "context"], a, kw)?;
    let c = ctx_arg(c)?;
    let r = rounding_arg(rnd, &c)?;
    let Some(exp) = exp else {
        raise!(TypeError, "quantize() missing required argument 'exp' (pos 1)");
    };
    let y = convert_raise(exp)?;
    run1(&c, |e| ops::quantize(dec_of(o), &y, r, e))
}

fn integral(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>, name: &str, exact: bool) -> PyResult {
    let [rnd, c] = kwparse(name, &["rounding", "context"], a, kw)?;
    let c = ctx_arg(c)?;
    let r = rounding_arg(rnd, &c)?;
    run1(&c, |e| ops::to_integral(dec_of(o), r, exact, e))
}
fn m_to_integral(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    integral(o, a, kw, "to_integral", false)
}
fn m_to_integral_value(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    integral(o, a, kw, "to_integral_value", false)
}
fn m_to_integral_exact(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    integral(o, a, kw, "to_integral_exact", true)
}

fn m_is_normal(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [c] = kwparse("is_normal", &["context"], a, kw)?;
    let c = ctx_arg(c)?;
    Ok(bool_::new_bool(ops::is_normal(dec_of(o), &engine(&c))))
}
fn m_is_subnormal(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [c] = kwparse("is_subnormal", &["context"], a, kw)?;
    let c = ctx_arg(c)?;
    Ok(bool_::new_bool(ops::is_subnormal(dec_of(o), &engine(&c))))
}
fn m_number_class(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [c] = kwparse("number_class", &["context"], a, kw)?;
    let c = ctx_arg(c)?;
    Ok(pystr::new_str(ops::number_class(dec_of(o), &engine(&c))))
}
fn m_to_eng_string(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [c] = kwparse("to_eng_string", &["context"], a, kw)?;
    let c = ctx_arg(c)?;
    Ok(pystr::new_str(&to_string(dec_of(o), true, py_ctx::cobj(&c).capitals != 0)))
}

macro_rules! m_pred {
    ($f:ident, |$d:ident| $e:expr) => {
        fn $f(o: &PyObject) -> PyResult {
            let $d = dec_of(o);
            Ok(bool_::new_bool($e))
        }
    };
}
m_pred!(m_is_canonical, |_d| true);
m_pred!(m_is_finite, |d| d.is_finite());
m_pred!(m_is_infinite, |d| d.is_inf());
m_pred!(m_is_nan, |d| d.is_nan());
m_pred!(m_is_qnan, |d| d.is_qnan());
m_pred!(m_is_snan, |d| d.is_snan());
m_pred!(m_is_signed, |d| d.neg);
m_pred!(m_is_zero, |d| d.is_zero());

fn m_adjusted(o: &PyObject) -> PyResult {
    Ok(int::new_int(dec_of(o).adjusted()))
}
fn m_canonical(o: &PyObject) -> PyResult {
    Ok(o.to_ref())
}
fn m_radix(_o: &PyObject) -> PyResult {
    Ok(new_dec(Dec::from_i64(10)))
}
fn m_copy_abs(o: &PyObject) -> PyResult {
    Ok(new_dec(dec_of(o).abs()))
}
fn m_copy_negate(o: &PyObject) -> PyResult {
    Ok(new_dec(dec_of(o).negated()))
}
fn m_self(o: &PyObject) -> PyResult {
    Ok(o.to_ref())
}
fn m_deepcopy(o: &PyObject, _memo: &PyObject) -> PyResult {
    Ok(o.to_ref())
}
fn g_real(o: &PyObject) -> PyResult {
    Ok(o.to_ref())
}
fn g_imag(_o: &PyObject) -> PyResult {
    Ok(new_dec(Dec::zero()))
}

fn m_as_tuple(o: &PyObject) -> PyResult {
    let d = dec_of(o);
    let sign = int::new_int(d.neg as i64);
    let digits: Vec<PyRef> = match d.kind {
        Kind::Inf => vec![int::new_int(0)],
        Kind::Finite => nat::to_digit_vec(&d.coef).into_iter().map(|x| int::new_int(x as i64)).collect(),
        _ => {
            if d.coef.is_empty() {
                vec![]
            } else {
                nat::to_digit_vec(&d.coef).into_iter().map(|x| int::new_int(x as i64)).collect()
            }
        }
    };
    let exp: PyRef = match d.kind {
        Kind::Inf => pystr::new_str("F"),
        Kind::NaN => pystr::new_str("n"),
        Kind::SNaN => pystr::new_str("N"),
        Kind::Finite => int::new_int(d.exp),
    };
    let t = tuple::new_tuple(vec![sign, tuple::new_tuple(digits), exp]);
    match &st().decimal_tuple {
        Some(dt) => ab::call(dt, tuple::tuple_items(&t)),
        None => Ok(t),
    }
}

fn m_as_integer_ratio(o: &PyObject) -> PyResult {
    let d = dec_of(o);
    if d.is_nan() {
        raise!(ValueError, "cannot convert NaN to integer ratio");
    }
    if d.is_inf() {
        raise!(OverflowError, "cannot convert Infinity to integer ratio");
    }
    if d.exp >= 0 {
        let c = if d.coef.is_empty() { vec![] } else { nat::shl(&d.coef, d.exp as u64) };
        return Ok(tuple::new_tuple(vec![nat_to_int(d.neg, &c), int::new_int(1)]));
    }
    if d.coef.is_empty() {
        return Ok(tuple::new_tuple(vec![int::new_int(0), int::new_int(1)]));
    }
    // reduce c / 10^k by removing common factors of 2 and 5
    let k = (-(d.exp as i128)) as u64;
    let mut c = d.coef.clone();
    let mut twos = 0u64;
    while twos < k && nat::rem_small(&c, 2) == 0 {
        // divide by the largest power of two available at once
        let tz = {
            let r = nat::rem_small(&c, 1u64 << 32);
            if r == 0 { 32 } else { r.trailing_zeros() as u64 }
        };
        let s = tz.min(k - twos).min(32);
        c = nat::divrem_small(&c, 1u64 << s).0;
        twos += s;
    }
    let mut fives = 0u64;
    while fives < k && nat::rem_small(&c, 5) == 0 {
        c = nat::divrem_small(&c, 5).0;
        fives += 1;
    }
    // denominator 2^(k-twos) * 5^(k-fives)
    let den = nat::mul(&nat::pow(&[2], k - twos), &nat::pow(&[5], k - fives));
    Ok(tuple::new_tuple(vec![nat_to_int(d.neg, &c), nat_to_int(false, &den)]))
}

fn m_from_float(cls: &PyObject, f: &PyObject) -> PyResult {
    let d = if float::is_float(f) {
        f64_to_dec(float::float_value(f))
    } else if int::is_int(f) {
        int_to_dec(f)
    } else {
        raise!(TypeError, "argument must be int or float");
    };
    finish_classmethod(cls, d)
}

fn m_from_number(cls: &PyObject, n: &PyObject) -> PyResult {
    let d = if is_dec(n) {
        dec_of(n).clone()
    } else if float::is_float(n) {
        f64_to_dec(float::float_value(n))
    } else if int::is_int(n) && !pystr::is_str(n) {
        int_to_dec(n)
    } else {
        raise!(TypeError, "conversion from {} to Decimal is not supported", tname(n));
    };
    finish_classmethod(cls, d)
}

fn finish_classmethod(cls: &PyObject, d: Dec) -> PyResult {
    let r = new_dec(d);
    if as_type(cls) == decimal_type() {
        Ok(r)
    } else {
        ab::call(cls, &[&r])
    }
}

fn m_round(o: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("__round__", a, 0, 1)?;
    let d = dec_of(o);
    if a.is_empty() || a[0].is(none::none()) {
        return to_int_rounded(d, Round::HalfEven);
    }
    let n = a[0];
    if !int::is_int(n) {
        raise!(TypeError, "optional arg must be an integer");
    }
    let nv = match int::as_i64(n) {
        Some(v) => v,
        None => raise!(OverflowError, "Python int too large to convert to C ssize_t"),
    };
    let c = current()?;
    let exp = -(nv as i128);
    run1(&c, |e| {
        if exp < MIN_ETINY as i128 * 2 || exp > MAX_EMAX as i128 * 2 {
            e.status |= INVALID_OPERATION;
            return Dec::nan();
        }
        let q = Dec::from_u64(false, 1, exp as i64);
        let r = e.round;
        ops::quantize(d, &q, r, e)
    })
}

fn m_floor(o: &PyObject) -> PyResult {
    to_int_rounded(dec_of(o), Round::Floor)
}
fn m_ceil(o: &PyObject) -> PyResult {
    to_int_rounded(dec_of(o), Round::Ceiling)
}
fn m_trunc(o: &PyObject) -> PyResult {
    to_int(dec_of(o))
}
fn m_complex(o: &PyObject) -> PyResult {
    let f = to_float(dec_of(o))?;
    Ok(complex::new_complex(f, 0.0))
}
fn m_reduce(o: &PyObject) -> PyResult {
    let s = to_string(dec_of(o), false, true);
    Ok(tuple::new_tuple(vec![type_ref(o.type_ptr()), tuple::new_tuple(vec![pystr::new_str(&s)])]))
}
fn m_sizeof(o: &PyObject) -> PyResult {
    let d = dec_of(o);
    let base = unsafe { (*o.type_ptr()).tp_basicsize as i64 };
    Ok(int::new_int(base + 8 * d.coef.len() as i64))
}

fn locale_from_dict(d: &PyObject) -> PyResult<fmt::LocaleInfo> {
    let get = |k: &str| -> PyResult<PyRef> {
        match ab::getitem(d, &pystr::new_str(k)) {
            Ok(v) => Ok(v),
            Err(e) => Err(e),
        }
    };
    let dp = get("decimal_point")?;
    let ts = get("thousands_sep")?;
    let gr = get("grouping")?;
    let dps = pystr::to_string_lossy(&*ab::str(&dp)?);
    let tss = pystr::to_string_lossy(&*ab::str(&ts)?);
    if dps.len() > 4 || dps.chars().count() > 1 {
        raise!(ValueError, "invalid override dict");
    }
    if tss.len() > 4 || tss.chars().count() > 1 {
        raise!(ValueError, "invalid override dict");
    }
    let mut grouping = vec![];
    if pystr::is_str(&gr) {
        for c in pystr::to_string_lossy(&gr).chars() {
            let v = c as u32;
            if v > fmt::CHAR_MAX {
                raise!(ValueError, "invalid override dict");
            }
            grouping.push(v);
        }
    } else {
        for x in ab::collect_iterable(&gr)? {
            let v = ab::as_i64(&x)?;
            if v < 0 || v as u32 > fmt::CHAR_MAX {
                grouping.push(fmt::CHAR_MAX);
            } else {
                grouping.push(v as u32);
            }
        }
    }
    Ok(fmt::LocaleInfo { decimal_point: dps, thousands_sep: tss, grouping })
}

fn m_format(o: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("__format__", a, 1, 2)?;
    let spec = a[0];
    if !pystr::is_str(spec) {
        raise!(TypeError, "format spec must be a string, not {}", tname(spec));
    }
    let override_ = if a.len() > 1 && !a[1].is(none::none()) {
        if !dict::is_dict(a[1]) {
            raise!(TypeError, "optional argument must be a dict");
        }
        locale_from_dict(a[1])?;
        Some(a[1])
    } else {
        None
    };
    let specs = pystr::to_string_lossy(spec);
    let parsed = match fmt::parse_spec(&specs, true) {
        Ok(p) => p,
        Err(m) => raise!(ValueError, "{}", m),
    };
    if parsed.ty == Some('N') {
        crate::modules::warnings::warn(crate::types::exceptions::DeprecationWarning(), "Format specifier 'N' is deprecated", 1)?;
    }
    let c = current()?;
    let co = py_ctx::cobj(&c);
    let locale = if matches!(parsed.ty, Some('n') | Some('N')) {
        Some(match override_ {
            Some(d) => locale_from_dict(d)?,
            None => {
                let lc = (|| -> PyResult<PyRef> {
                    let m = crate::import::import_absolute("locale")?;
                    ab::call0(&*ab::getattr_str(&m, "localeconv")?)
                })();
                match lc {
                    Ok(d) => locale_from_dict(&d)?,
                    Err(_) => {
                        clear_exc();
                        fmt::LocaleInfo { decimal_point: ".".into(), thousands_sep: "".into(), grouping: vec![] }
                    }
                }
            }
        })
    } else {
        None
    };
    let s = fmt::format_dec(dec_of(o), &parsed, co.capitals != 0, py_ctx::round_of(co), locale.as_ref());
    Ok(pystr::new_str(&s))
}

// ------------------------------- Context arithmetic methods ----------------------------------------

macro_rules! c_unary {
    ($f:ident, $name:expr, $op:expr) => {
        fn $f(o: &PyObject, a: &[&PyObject]) -> PyResult {
            args::check_pos($name, a, 1, 1)?;
            let x = convert_raise(a[0])?;
            let f: fn(&Dec, &mut Ctx) -> Dec = $op;
            run1(o, |e| f(&x, e))
        }
    };
}
c_unary!(c_abs, "abs", ops::abs);
c_unary!(c_exp, "exp", transc::exp);
c_unary!(c_ln, "ln", transc::ln);
c_unary!(c_log10, "log10", transc::log10);
c_unary!(c_logb, "logb", ops::logb);
c_unary!(c_logical_invert, "logical_invert", ops::logical_invert);
c_unary!(c_minus, "minus", ops::minus);
c_unary!(c_plus, "plus", ops::plus);
c_unary!(c_next_minus, "next_minus", ops::next_minus);
c_unary!(c_next_plus, "next_plus", ops::next_plus);
c_unary!(c_normalize, "normalize", ops::reduce);
c_unary!(c_sqrt, "sqrt", transc::sqrt);
c_unary!(c_apply, "_apply", ops::apply);
c_unary!(c_copy_abs, "copy_abs", |x, _| x.abs());
c_unary!(c_copy_negate, "copy_negate", |x, _| x.negated());
c_unary!(c_copy_decimal, "copy_decimal", |x, _| x.clone());
c_unary!(c_to_integral, "to_integral", |x, c| {
    let r = c.round;
    ops::to_integral(x, r, false, c)
});
c_unary!(c_to_integral_value, "to_integral_value", |x, c| {
    let r = c.round;
    ops::to_integral(x, r, false, c)
});
c_unary!(c_to_integral_exact, "to_integral_exact", |x, c| {
    let r = c.round;
    ops::to_integral(x, r, true, c)
});

macro_rules! c_binary {
    ($f:ident, $name:expr, $op:expr) => {
        fn $f(o: &PyObject, a: &[&PyObject]) -> PyResult {
            args::check_pos($name, a, 2, 2)?;
            let x = convert_raise(a[0])?;
            let y = convert_raise(a[1])?;
            let f: fn(&Dec, &Dec, &mut Ctx) -> Dec = $op;
            run1(o, |e| f(&x, &y, e))
        }
    };
}
c_binary!(c_add, "add", ops::add);
c_binary!(c_subtract, "subtract", ops::sub);
c_binary!(c_multiply, "multiply", ops::mul);
c_binary!(c_divide, "divide", ops::div);
c_binary!(c_divide_int, "divide_int", ops::divint);
c_binary!(c_remainder, "remainder", ops::rem);
c_binary!(c_remainder_near, "remainder_near", ops::rem_near);
c_binary!(c_compare, "compare", ops::compare);
c_binary!(c_compare_signal, "compare_signal", ops::compare_signal);
c_binary!(c_compare_total, "compare_total", |a, b, _| ops::compare_total(a, b));
c_binary!(c_compare_total_mag, "compare_total_mag", |a, b, _| ops::compare_total_mag(a, b));
c_binary!(c_copy_sign, "copy_sign", |a, b, _| ops::copy_sign(a, b));
c_binary!(c_max, "max", ops::max);
c_binary!(c_max_mag, "max_mag", ops::max_mag);
c_binary!(c_min, "min", ops::min);
c_binary!(c_min_mag, "min_mag", ops::min_mag);
c_binary!(c_next_toward, "next_toward", ops::next_toward);
c_binary!(c_rotate, "rotate", ops::rotate);
c_binary!(c_scaleb, "scaleb", ops::scaleb);
c_binary!(c_shift, "shift", ops::shift);
c_binary!(c_logical_and, "logical_and", |a, b, c| ops::logical_op(a, b, b'&', c));
c_binary!(c_logical_or, "logical_or", |a, b, c| ops::logical_op(a, b, b'|', c));
c_binary!(c_logical_xor, "logical_xor", |a, b, c| ops::logical_op(a, b, b'^', c));
c_binary!(c_quantize, "quantize", |a, b, c| {
    let r = c.round;
    ops::quantize(a, b, r, c)
});

fn c_divmod(o: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("divmod", a, 2, 2)?;
    let x = convert_raise(a[0])?;
    let y = convert_raise(a[1])?;
    divmod_impl(o, &x, &y)
}

fn c_same_quantum(o: &PyObject, a: &[&PyObject]) -> PyResult {
    let _ = o;
    args::check_pos("same_quantum", a, 2, 2)?;
    let x = convert_raise(a[0])?;
    let y = convert_raise(a[1])?;
    Ok(bool_::new_bool(ops::same_quantum(&x, &y)))
}

fn c_fma(o: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("fma", a, 3, 3)?;
    let x = convert_raise(a[0])?;
    let y = convert_raise(a[1])?;
    let z = convert_raise(a[2])?;
    run1(o, |e| ops::fma(&x, &y, &z, e))
}

fn c_power(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [x, y, m] = kwparse("power", &["a", "b", "modulo"], a, kw)?;
    let (Some(x), Some(y)) = (x, y) else {
        raise!(TypeError, "power() missing required argument");
    };
    let x = convert_raise(x)?;
    let y = convert_raise(y)?;
    match m {
        Some(m) if !m.is(none::none()) => {
            let z = convert_raise(m)?;
            run1(o, |e| transc::power_mod(&x, &y, &z, e))
        }
        _ => run1(o, |e| transc::power(&x, &y, e)),
    }
}

macro_rules! c_pred {
    ($f:ident, $name:expr, |$d:ident, $c:ident| $e:expr) => {
        fn $f(o: &PyObject, a: &[&PyObject]) -> PyResult {
            args::check_pos($name, a, 1, 1)?;
            let x = convert_raise(a[0])?;
            let $d: &Dec = &x;
            let $c = engine(o);
            let _ = &$c;
            Ok(bool_::new_bool($e))
        }
    };
}
c_pred!(c_is_finite, "is_finite", |d, _c| d.is_finite());
c_pred!(c_is_infinite, "is_infinite", |d, _c| d.is_inf());
c_pred!(c_is_nan, "is_nan", |d, _c| d.is_nan());
c_pred!(c_is_qnan, "is_qnan", |d, _c| d.is_qnan());
c_pred!(c_is_snan, "is_snan", |d, _c| d.is_snan());
c_pred!(c_is_signed, "is_signed", |d, _c| d.neg);
c_pred!(c_is_zero, "is_zero", |d, _c| d.is_zero());
c_pred!(c_is_normal, "is_normal", |d, c| ops::is_normal(d, &c));
c_pred!(c_is_subnormal, "is_subnormal", |d, c| ops::is_subnormal(d, &c));

fn c_is_canonical(_o: &PyObject, a: &PyObject) -> PyResult {
    if !is_dec(a) {
        raise!(TypeError, "argument must be a Decimal");
    }
    Ok(bool_::new_bool(true))
}
fn c_canonical(_o: &PyObject, a: &PyObject) -> PyResult {
    if !is_dec(a) {
        raise!(TypeError, "argument must be a Decimal");
    }
    Ok(a.to_ref())
}
fn c_number_class(o: &PyObject, a: &PyObject) -> PyResult {
    let x = convert_raise(a)?;
    Ok(pystr::new_str(ops::number_class(&x, &engine(o))))
}
fn c_to_sci_string(o: &PyObject, a: &PyObject) -> PyResult {
    let x = convert_raise(a)?;
    Ok(pystr::new_str(&to_string(&x, false, py_ctx::cobj(o).capitals != 0)))
}
fn c_to_eng_string(o: &PyObject, a: &PyObject) -> PyResult {
    let x = convert_raise(a)?;
    Ok(pystr::new_str(&to_string(&x, true, py_ctx::cobj(o).capitals != 0)))
}

fn c_create_decimal(o: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("create_decimal", a, 0, 1)?;
    let d = match a.first() {
        None => Dec::zero(),
        Some(v) => value_to_dec(v, o, false)?,
    };
    Ok(new_dec(d))
}

fn c_create_decimal_from_float(o: &PyObject, f: &PyObject) -> PyResult {
    let d = if float::is_float(f) {
        f64_to_dec(float::float_value(f))
    } else if int::is_int(f) {
        int_to_dec(f)
    } else {
        raise!(TypeError, "argument must be int or float");
    };
    run1(o, |e| rounded(d, e))
}

pub fn context_methods() -> Vec<PyMethodDef> {
    use crate::methods::{def, fast, fastkw, o as m_o};
    let u = |name: &str, f: *const std::ffi::c_void, doc: &str| def(name, f, METH_FASTCALL, &format!("{}($self, x, /)\n--\n\n{}", name, doc));
    let b = |name: &str, f: *const std::ffi::c_void, doc: &str| def(name, f, METH_FASTCALL, &format!("{}($self, x, y, /)\n--\n\n{}", name, doc));
    let o1 = |name: &str, f: *const std::ffi::c_void, doc: &str| def(name, f, METH_O, &format!("{}($self, x, /)\n--\n\n{}", name, doc));
    vec![
        u("abs", fast(c_abs), "Absolute value, rounded to the context."),
        u("exp", fast(c_exp), "Exponential function, correctly rounded (always ROUND_HALF_EVEN)."),
        u("ln", fast(c_ln), "Natural logarithm, correctly rounded (always ROUND_HALF_EVEN)."),
        u("log10", fast(c_log10), "Base-10 logarithm, correctly rounded (always ROUND_HALF_EVEN)."),
        u("logb", fast(c_logb), "Adjusted exponent of the operand as a Decimal (-Infinity for zero)."),
        u("logical_invert", fast(c_logical_invert), "Digit-wise NOT of a logical operand (digits 0/1), padded to the precision."),
        u("minus", fast(c_minus), "Negation, rounded to the context."),
        u("plus", fast(c_plus), "The operand rounded to the context."),
        u("next_minus", fast(c_next_minus), "Largest number representable in the context that is below the operand."),
        u("next_plus", fast(c_next_plus), "Smallest number representable in the context that is above the operand."),
        u("normalize", fast(c_normalize), "Round to the context and strip trailing zeros from the coefficient."),
        u("sqrt", fast(c_sqrt), "Square root, correctly rounded to the context precision."),
        u("_apply", fast(c_apply), "The operand rounded to the context."),
        u("copy_abs", fast(c_copy_abs), "Copy with the sign cleared; no rounding and no signals."),
        u("copy_negate", fast(c_copy_negate), "Copy with the sign flipped; no rounding and no signals."),
        u("copy_decimal", fast(c_copy_decimal), "Copy of the operand; no rounding and no signals."),
        u("to_integral", fast(c_to_integral), "Round to an integer; same as to_integral_value()."),
        u("to_integral_value", fast(c_to_integral_value), "Round to an integer without signaling Inexact or Rounded."),
        u("to_integral_exact", fast(c_to_integral_exact), "Round to an integer, signaling Inexact and Rounded when appropriate."),
        u("is_finite", fast(c_is_finite), "True unless the operand is infinite or a NaN."),
        u("is_infinite", fast(c_is_infinite), "True for positive or negative infinity."),
        u("is_nan", fast(c_is_nan), "True for quiet and signaling NaNs."),
        u("is_qnan", fast(c_is_qnan), "True for a quiet NaN."),
        u("is_snan", fast(c_is_snan), "True for a signaling NaN."),
        u("is_signed", fast(c_is_signed), "True if the sign is negative (including -0 and -NaN)."),
        u("is_zero", fast(c_is_zero), "True for a zero of either sign."),
        u("is_normal", fast(c_is_normal), "True for finite nonzero operands whose adjusted exponent is at least Emin."),
        u("is_subnormal", fast(c_is_subnormal), "True for finite nonzero operands whose adjusted exponent is below Emin."),
        o1("is_canonical", m_o(c_is_canonical), "Always True."),
        o1("canonical", m_o(c_canonical), "The operand itself (every Decimal is canonical)."),
        o1("number_class", m_o(c_number_class), "Name of the operand's class, e.g. '+Normal', '-Zero', 'sNaN'."),
        o1("to_sci_string", m_o(c_to_sci_string), "String form in scientific notation (what str() gives)."),
        o1("to_eng_string", m_o(c_to_eng_string), "String form with an exponent that is a multiple of three."),
        b("add", fast(c_add), "Sum, rounded to the context."),
        b("subtract", fast(c_subtract), "Difference, rounded to the context."),
        b("multiply", fast(c_multiply), "Product, rounded to the context."),
        b("divide", fast(c_divide), "Quotient, rounded to the context."),
        b("divide_int", fast(c_divide_int), "Integer part of the quotient (truncated)."),
        b("divmod", fast(c_divmod), "Pair (divide_int, remainder)."),
        b("remainder", fast(c_remainder), "Remainder of truncating integer division; sign follows the dividend."),
        b("remainder_near", fast(c_remainder_near), "Remainder after division with the quotient rounded to the nearest integer."),
        b("compare", fast(c_compare), "Numeric comparison returning Decimal -1, 0 or 1 (NaN if a NaN is involved)."),
        b("compare_signal", fast(c_compare_signal), "Like compare(), but any NaN operand signals InvalidOperation."),
        b("compare_total", fast(c_compare_total), "Total ordering comparison on the representation (Decimal -1, 0 or 1)."),
        b("compare_total_mag", fast(c_compare_total_mag), "compare_total() applied to the absolute values."),
        b("copy_sign", fast(c_copy_sign), "Copy of the first operand carrying the sign of the second."),
        b("max", fast(c_max), "The numerically larger operand, rounded to the context."),
        b("max_mag", fast(c_max_mag), "The operand with the larger absolute value, rounded to the context."),
        b("min", fast(c_min), "The numerically smaller operand, rounded to the context."),
        b("min_mag", fast(c_min_mag), "The operand with the smaller absolute value, rounded to the context."),
        b("next_toward", fast(c_next_toward), "Next representable number from the first operand in the direction of the second."),
        b("rotate", fast(c_rotate), "Rotate the coefficient digits (within the precision) by the given count."),
        b("scaleb", fast(c_scaleb), "Add the integral second operand to the exponent."),
        b("shift", fast(c_shift), "Shift the coefficient digits (within the precision) by the given count."),
        b("logical_and", fast(c_logical_and), "Digit-wise AND of two logical operands."),
        b("logical_or", fast(c_logical_or), "Digit-wise OR of two logical operands."),
        b("logical_xor", fast(c_logical_xor), "Digit-wise XOR of two logical operands."),
        b("quantize", fast(c_quantize), "Round to the exponent of the second operand."),
        b("same_quantum", fast(c_same_quantum), "True if both operands have the same exponent (or are both NaN / both infinite)."),
        def("fma", fast(c_fma), METH_FASTCALL, "fma($self, x, y, z, /)\n--\n\nMultiply exactly, then add the third operand with a single rounding."),
        def("power", fastkw(c_power), METH_FASTCALL | METH_KEYWORDS, "power($self, /, a, b, modulo=None)\n--\n\nFirst operand raised to the second; with modulo, modular exponentiation of integers."),
        def("create_decimal", fast(c_create_decimal), METH_FASTCALL, "create_decimal($self, num=\"0\", /)\n--\n\nConvert num to a Decimal and round it to this context."),
        def("create_decimal_from_float", m_o(c_create_decimal_from_float), METH_O, "create_decimal_from_float($self, f, /)\n--\n\nConvert a float (or int) exactly, then round it to this context."),
    ]
}

// ------------------------------- type setup ----------------------------------------

static mut DEC_NUM: PyNumberMethods = PyNumberMethods::ZERO;

const S_CTX: &str = "($self, /, context=None)\n--\n\n";
const S_OTHER: &str = "($self, /, other, context=None)\n--\n\n";
const S_NONE: &str = "($self, /)\n--\n\n";

pub fn init_types() -> PyResult<()> {
    unsafe {
        let t = &mut *decimal_type();
        t.tp_name = c"decimal.Decimal".as_ptr();
        t.tp_basicsize = std::mem::size_of::<DecObj>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_IMMUTABLETYPE;
        t.tp_new = Some(dec_new);
        t.tp_dealloc = Some(dec_dealloc);
        t.tp_alloc = Some(generic_alloc);
        t.tp_repr = Some(dec_repr);
        t.tp_str = Some(dec_str);
        t.tp_hash = Some(dec_hash);
        t.tp_richcompare = Some(dec_richcompare);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_doc = c"Decimal(value='0', context=None)\n--\n\nAn exact decimal number: sign, integer coefficient and power-of-ten exponent,\nor an infinity or NaN.  value may be a str, int, float, Decimal or a\n(sign, digits, exponent) tuple; the conversion is exact and the context only\ndecides whether an invalid value raises.".as_ptr();
        let n = &mut *(&raw mut DEC_NUM);
        n.nb_add = Some(nb_add);
        n.nb_subtract = Some(nb_sub);
        n.nb_multiply = Some(nb_mul);
        n.nb_true_divide = Some(nb_truediv);
        n.nb_floor_divide = Some(nb_floordiv);
        n.nb_remainder = Some(nb_rem);
        n.nb_divmod = Some(nb_divmod);
        n.nb_power = Some(nb_pow);
        n.nb_negative = Some(nb_neg);
        n.nb_positive = Some(nb_pos);
        n.nb_absolute = Some(nb_abs);
        n.nb_bool = Some(nb_bool);
        n.nb_int = Some(nb_int);
        n.nb_float = Some(nb_float);
        t.tp_as_number = n;
        use crate::methods::{def, fastkw, noargs, o as m_o, fast};
        let kwm = |name: &str, f: *const std::ffi::c_void, sig: &str, doc: &str| {
            def(name, f, METH_FASTCALL | METH_KEYWORDS, &format!("{}{}{}", name, sig, doc))
        };
        let nam = |name: &str, f: *const std::ffi::c_void, doc: &str| def(name, f, METH_NOARGS, &format!("{}{}{}", name, S_NONE, doc));
        let methods = vec![
            kwm("exp", fastkw(m_exp), S_CTX, "Exponential function, correctly rounded (always ROUND_HALF_EVEN)."),
            kwm("ln", fastkw(m_ln), S_CTX, "Natural logarithm, correctly rounded (always ROUND_HALF_EVEN)."),
            kwm("log10", fastkw(m_log10), S_CTX, "Base-10 logarithm, correctly rounded (always ROUND_HALF_EVEN)."),
            kwm("logb", fastkw(m_logb), S_CTX, "Adjusted exponent of the operand as a Decimal (-Infinity for zero)."),
            kwm("logical_invert", fastkw(m_logical_invert), S_CTX, "Digit-wise NOT of a logical operand (digits 0/1), padded to the precision."),
            kwm("next_minus", fastkw(m_next_minus), S_CTX, "Largest number representable in the context that is below the operand."),
            kwm("next_plus", fastkw(m_next_plus), S_CTX, "Smallest number representable in the context that is above the operand."),
            kwm("normalize", fastkw(m_normalize), S_CTX, "Round to the context and strip trailing zeros from the coefficient."),
            kwm("sqrt", fastkw(m_sqrt), S_CTX, "Square root, correctly rounded to the context precision."),
            kwm("is_normal", fastkw(m_is_normal), S_CTX, "True for finite nonzero operands whose adjusted exponent is at least Emin."),
            kwm("is_subnormal", fastkw(m_is_subnormal), S_CTX, "True for finite nonzero operands whose adjusted exponent is below Emin."),
            kwm("number_class", fastkw(m_number_class), S_CTX, "Name of the operand's class, e.g. '+Normal', '-Zero', 'sNaN'."),
            kwm("to_eng_string", fastkw(m_to_eng_string), S_CTX, "String form with an exponent that is a multiple of three."),
            kwm("compare", fastkw(m_compare), S_OTHER, "Numeric comparison returning Decimal -1, 0 or 1 (NaN if a NaN is involved)."),
            kwm("compare_signal", fastkw(m_compare_signal), S_OTHER, "Like compare(), but any NaN operand signals InvalidOperation."),
            kwm("max", fastkw(m_max), S_OTHER, "The numerically larger operand, rounded to the context."),
            kwm("max_mag", fastkw(m_max_mag), S_OTHER, "The operand with the larger absolute value, rounded to the context."),
            kwm("min", fastkw(m_min), S_OTHER, "The numerically smaller operand, rounded to the context."),
            kwm("min_mag", fastkw(m_min_mag), S_OTHER, "The operand with the smaller absolute value, rounded to the context."),
            kwm("next_toward", fastkw(m_next_toward), S_OTHER, "Next representable number from the first operand in the direction of the second."),
            kwm("remainder_near", fastkw(m_remainder_near), S_OTHER, "Remainder after division with the quotient rounded to the nearest integer."),
            kwm("rotate", fastkw(m_rotate), S_OTHER, "Rotate the coefficient digits (within the precision) by the given count."),
            kwm("scaleb", fastkw(m_scaleb), S_OTHER, "Add the integral second operand to the exponent."),
            kwm("shift", fastkw(m_shift), S_OTHER, "Shift the coefficient digits (within the precision) by the given count."),
            kwm("logical_and", fastkw(m_logical_and), S_OTHER, "Digit-wise AND of two logical operands."),
            kwm("logical_or", fastkw(m_logical_or), S_OTHER, "Digit-wise OR of two logical operands."),
            kwm("logical_xor", fastkw(m_logical_xor), S_OTHER, "Digit-wise XOR of two logical operands."),
            kwm("compare_total", fastkw(m_compare_total), S_OTHER, "Total ordering comparison on the representation (Decimal -1, 0 or 1)."),
            kwm("compare_total_mag", fastkw(m_compare_total_mag), S_OTHER, "compare_total() applied to the absolute values."),
            kwm("copy_sign", fastkw(m_copy_sign), S_OTHER, "Copy of the first operand carrying the sign of the second."),
            kwm("same_quantum", fastkw(m_same_quantum), S_OTHER, "True if both operands have the same exponent (or are both NaN / both infinite)."),
            kwm("fma", fastkw(m_fma), "($self, /, other, third, context=None)\n--\n\n", "Multiply exactly, then add the third operand with a single rounding."),
            kwm("quantize", fastkw(m_quantize), "($self, /, exp, rounding=None, context=None)\n--\n\n", "Round to the exponent of the second operand."),
            kwm("to_integral", fastkw(m_to_integral), "($self, /, rounding=None, context=None)\n--\n\n", "Round to an integer; same as to_integral_value()."),
            kwm("to_integral_value", fastkw(m_to_integral_value), "($self, /, rounding=None, context=None)\n--\n\n", "Round to an integer without signaling Inexact or Rounded."),
            kwm("to_integral_exact", fastkw(m_to_integral_exact), "($self, /, rounding=None, context=None)\n--\n\n", "Round to an integer, signaling Inexact and Rounded when appropriate."),
            nam("adjusted", noargs(m_adjusted), "Exponent of the most significant digit."),
            nam("as_tuple", noargs(m_as_tuple), "The (sign, digits, exponent) triple as a DecimalTuple."),
            nam("as_integer_ratio", noargs(m_as_integer_ratio), "Exact value as a reduced (numerator, denominator) pair of ints."),
            nam("canonical", noargs(m_canonical), "The operand itself (every Decimal is canonical)."),
            nam("conjugate", noargs(m_self), "The operand itself."),
            nam("copy_abs", noargs(m_copy_abs), "Copy with the sign cleared; no rounding and no signals."),
            nam("copy_negate", noargs(m_copy_negate), "Copy with the sign flipped; no rounding and no signals."),
            nam("radix", noargs(m_radix), "Decimal(10)."),
            nam("is_canonical", noargs(m_is_canonical), "Always True."),
            nam("is_finite", noargs(m_is_finite), "True unless the operand is infinite or a NaN."),
            nam("is_infinite", noargs(m_is_infinite), "True for positive or negative infinity."),
            nam("is_nan", noargs(m_is_nan), "True for quiet and signaling NaNs."),
            nam("is_qnan", noargs(m_is_qnan), "True for a quiet NaN."),
            nam("is_snan", noargs(m_is_snan), "True for a signaling NaN."),
            nam("is_signed", noargs(m_is_signed), "True if the sign is negative (including -0 and -NaN)."),
            nam("is_zero", noargs(m_is_zero), "True for a zero of either sign."),
            def("from_float", m_o(m_from_float), METH_O | METH_CLASS, "from_float($type, f, /)\n--\n\nExact Decimal for a float or int."),
            def("from_number", m_o(m_from_number), METH_O | METH_CLASS, "from_number($type, number, /)\n--\n\nExact Decimal for an int, float or Decimal."),
            def("__round__", fast(m_round), METH_FASTCALL, ""),
            def("__ceil__", noargs(m_ceil), METH_NOARGS, ""),
            def("__floor__", noargs(m_floor), METH_NOARGS, ""),
            def("__trunc__", noargs(m_trunc), METH_NOARGS, ""),
            def("__complex__", noargs(m_complex), METH_NOARGS, ""),
            def("__copy__", noargs(m_self), METH_NOARGS, ""),
            def("__deepcopy__", m_o(m_deepcopy), METH_O, ""),
            def("__format__", fast(m_format), METH_FASTCALL, ""),
            def("__reduce__", noargs(m_reduce), METH_NOARGS, ""),
            def("__sizeof__", noargs(m_sizeof), METH_NOARGS, ""),
        ];
        t.tp_methods = crate::methods::table(methods);
        t.tp_getset = crate::getset_table![("real", g_real), ("imag", g_imag)];
        type_ready(decimal_type())?;
    }
    Ok(())
}
