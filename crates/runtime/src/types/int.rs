//! `int` core: CPython's PyLongObject layout (30-bit digits, lv_tag = ndigits<<3 | sign).
//! Arithmetic lives in `int_methods.rs` (uses the `rpy-bigint` crate for big values).

use crate::alloc;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;

#[unsafe(no_mangle)]
pub static mut PyLong_Type: PyTypeObject = PyTypeObject::ZERO;

#[inline(always)]
pub fn int_type() -> *mut PyTypeObject {
    &raw mut PyLong_Type
}
#[inline(always)]
pub fn is_int(o: &PyObject) -> bool {
    o.ty().tp_flags & Py_TPFLAGS_LONG_SUBCLASS != 0
}
#[inline(always)]
pub fn is_int_exact(o: &PyObject) -> bool {
    o.is_type(int_type())
}

pub const SHIFT: u32 = 30;
pub const MASK: u32 = (1 << SHIFT) - 1;
const SIGN_POS: usize = 0;
const SIGN_ZERO: usize = 1;
const SIGN_NEG: usize = 2;
const DIGITS_OFFSET: usize = std::mem::offset_of!(PyLongObject, ob_digit);

#[inline(always)]
fn lv_tag(o: &PyObject) -> usize {
    unsafe { (*(o.as_ptr() as *const PyLongObject)).lv_tag }
}

#[inline(always)]
pub fn ndigits(o: &PyObject) -> usize {
    lv_tag(o) >> 3
}

/// -1, 0 or 1.
#[inline(always)]
pub fn sign(o: &PyObject) -> i32 {
    1 - (lv_tag(o) & 3) as i32
}

#[inline(always)]
pub fn digits(o: &PyObject) -> &[u32] {
    unsafe {
        std::slice::from_raw_parts((o.as_ptr() as *const u8).add(DIGITS_OFFSET) as *const u32, ndigits(o))
    }
}

#[inline(always)]
pub fn is_compact(o: &PyObject) -> bool {
    lv_tag(o) < (2 << 3)
}

/// Value of a compact int (|v| < 2**30).
#[inline(always)]
pub fn compact_value(o: &PyObject) -> i64 {
    let s = sign(o) as i64;
    let d = unsafe { *((o.as_ptr() as *const u8).add(DIGITS_OFFSET) as *const u32) } as i64;
    s * d
}

/// Allocate an int of type `tp` with `n` digits (zeroed), sign set later.
pub unsafe fn alloc_int_of(tp: *mut PyTypeObject, n: usize) -> *mut PyObject {
    unsafe {
        let basic = (*tp).tp_basicsize as usize;
        let size = basic.max(DIGITS_OFFSET + 4) + n.saturating_sub(1) * 4;
        let size = size.max(DIGITS_OFFSET + 4);
        alloc::alloc_object(tp, size)
    }
}

pub unsafe fn set_sign_and_ndigits(op: *mut PyObject, sign: i32, n: usize) {
    unsafe {
        let s = match sign {
            1 => SIGN_POS,
            0 => SIGN_ZERO,
            _ => SIGN_NEG,
        };
        (*(op as *mut PyLongObject)).lv_tag = (n << 3) | s;
    }
}

pub unsafe fn digits_mut(op: *mut PyObject, n: usize) -> &'static mut [u32] {
    unsafe { std::slice::from_raw_parts_mut((op as *mut u8).add(DIGITS_OFFSET) as *mut u32, n) }
}

const SMALL_MIN: i64 = -5;
const SMALL_MAX: i64 = 1025;
static SMALL: [std::sync::atomic::AtomicPtr<PyObject>; (SMALL_MAX - SMALL_MIN + 1) as usize] =
    [const { std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()) }; (SMALL_MAX - SMALL_MIN + 1) as usize];

pub fn init_small_ints() {
    for v in SMALL_MIN..=SMALL_MAX {
        let o = make_int_i64(int_type(), v);
        unsafe { make_immortal(o) };
        SMALL[(v - SMALL_MIN) as usize].store(o, std::sync::atomic::Ordering::Release);
    }
}

fn make_int_i64(tp: *mut PyTypeObject, v: i64) -> *mut PyObject {
    let neg = v < 0;
    let mut a = v.unsigned_abs();
    let mut ds = [0u32; 3];
    let mut n = 0;
    while a != 0 {
        ds[n] = (a & MASK as u64) as u32;
        a >>= SHIFT;
        n += 1;
    }
    unsafe {
        let op = alloc_int_of(tp, n.max(1));
        let d = digits_mut(op, n.max(1));
        d[..n].copy_from_slice(&ds[..n]);
        set_sign_and_ndigits(op, if n == 0 { 0 } else if neg { -1 } else { 1 }, n);
        op
    }
}

#[inline]
pub fn new_int(v: i64) -> PyRef {
    if v >= SMALL_MIN && v <= SMALL_MAX {
        let p = SMALL[(v - SMALL_MIN) as usize].load(std::sync::atomic::Ordering::Relaxed);
        if !p.is_null() {
            return unsafe { PyRef::from_raw(p) }; // immortal: no incref needed
        }
    }
    unsafe { PyRef::from_raw(make_int_i64(int_type(), v)) }
}

pub fn new_int_usize(v: usize) -> PyRef {
    if v <= i64::MAX as usize { new_int(v as i64) } else { new_int_u128(v as u128) }
}

pub fn new_int_i128(v: i128) -> PyRef {
    if v >= i64::MIN as i128 && v <= i64::MAX as i128 {
        return new_int(v as i64);
    }
    let neg = v < 0;
    let mag = v.unsigned_abs();
    from_u128_mag(neg, mag)
}

pub fn new_int_u128(v: u128) -> PyRef {
    if v <= i64::MAX as u128 {
        return new_int(v as i64);
    }
    from_u128_mag(false, v)
}

fn from_u128_mag(neg: bool, mut a: u128) -> PyRef {
    let mut ds = Vec::with_capacity(5);
    while a != 0 {
        ds.push((a & MASK as u128) as u32);
        a >>= SHIFT;
    }
    from_digits(neg, &ds)
}

/// Build an int from sign + base-2**30 magnitude digits (normalises).
pub fn from_digits(neg: bool, ds: &[u32]) -> PyRef {
    from_digits_typed(int_type(), neg, ds)
}

pub fn from_digits_typed(tp: *mut PyTypeObject, neg: bool, ds: &[u32]) -> PyRef {
    let mut n = ds.len();
    while n > 0 && ds[n - 1] == 0 {
        n -= 1;
    }
    if tp == int_type() && n <= 2 {
        let mut v: i64 = 0;
        for i in (0..n).rev() {
            v = (v << SHIFT) | ds[i] as i64;
        }
        return new_int(if neg { -v } else { v });
    }
    unsafe {
        let op = alloc_int_of(tp, n.max(1));
        let d = digits_mut(op, n.max(1));
        d[..n].copy_from_slice(&ds[..n]);
        set_sign_and_ndigits(op, if n == 0 { 0 } else if neg { -1 } else { 1 }, n);
        PyRef::from_raw(op)
    }
}

/// The value as i64 if it fits.
#[inline]
pub fn as_i64(o: &PyObject) -> Option<i64> {
    if is_compact(o) {
        return Some(compact_value(o));
    }
    let ds = digits(o);
    if ds.len() > 3 {
        return None;
    }
    let mut v: i128 = 0;
    for &d in ds.iter().rev() {
        v = (v << SHIFT) | d as i128;
    }
    if sign(o) < 0 {
        v = -v;
    }
    if v >= i64::MIN as i128 && v <= i64::MAX as i128 { Some(v as i64) } else { None }
}

pub fn as_i128(o: &PyObject) -> Option<i128> {
    let ds = digits(o);
    if ds.len() > 5 {
        return None;
    }
    let mut v: u128 = 0;
    for &d in ds.iter().rev() {
        if v >> (128 - SHIFT) != 0 {
            return None; // shifting would lose high bits
        }
        v = (v << SHIFT) | d as u128;
    }
    if v > i128::MAX as u128 {
        return if sign(o) < 0 && v == 1u128 << 127 { Some(i128::MIN) } else { None };
    }
    Some(if sign(o) < 0 { -(v as i128) } else { v as i128 })
}

/// Bool singletons live in bool.rs but share this layout.
pub fn int_hash(o: &PyObject) -> isize {
    if is_compact(o) {
        let v = compact_value(o);
        return if v == -1 { -2 } else { v as isize };
    }
    let m = crate::hash::MODULUS;
    let mut x: u64 = 0;
    for &d in digits(o).iter().rev() {
        // x = (x * 2**30 + d) mod (2**61 - 1)
        x = ((x << SHIFT) & m) | (x >> (crate::hash::MODULUS_BITS - SHIFT));
        x += d as u64;
        if x >= m {
            x -= m;
        }
    }
    let mut h = x as i64;
    if sign(o) < 0 {
        h = -h;
    }
    if h == -1 {
        h = -2;
    }
    h as isize
}

/// Convert to f64 (correctly rounded); Err on overflow.
pub fn to_f64(o: &PyObject) -> PyResult<f64> {
    if is_compact(o) {
        return Ok(compact_value(o) as f64);
    }
    if let Some(v) = as_i64(o) {
        return Ok(v as f64);
    }
    match rpy_bigint::conv::to_f64(rpy_bigint::IntRef { neg: sign(o) < 0, mag: digits(o) }) {
        Ok(f) => Ok(f),
        Err(_) => Err(overflow_error("int too large to convert to float")),
    }
}

/// Magnitude digits as a lowercase string in the given radix.
pub fn int_methods_mag_string(o: &PyObject, radix: u32) -> String {
    let r = rpy_bigint::IntRef { neg: false, mag: digits(o) };
    rpy_bigint::radix::to_string(r, radix)
}
