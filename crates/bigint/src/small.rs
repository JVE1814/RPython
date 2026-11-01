//! Fast paths on `i64` operands with Python semantics. Functions return `None` when the result
//! does not fit in an i64 (the caller then falls back to the [`crate::ops`] functions); division
//! helpers require a nonzero divisor (check for zero first and raise ZeroDivisionError).

/// `a + b`.
#[inline(always)]
pub fn add(a: i64, b: i64) -> Option<i64> {
    a.checked_add(b)
}

/// `a - b`.
#[inline(always)]
pub fn sub(a: i64, b: i64) -> Option<i64> {
    a.checked_sub(b)
}

/// `a * b`.
#[inline(always)]
pub fn mul(a: i64, b: i64) -> Option<i64> {
    a.checked_mul(b)
}

/// `a * b` as an exact i128 (never overflows).
#[inline(always)]
pub fn mul_wide(a: i64, b: i64) -> i128 {
    a as i128 * b as i128
}

/// Floor division `a // b` (b != 0). None only for `i64::MIN // -1`.
#[inline]
pub fn floordiv(a: i64, b: i64) -> Option<i64> {
    debug_assert!(b != 0);
    let q = a.checked_div(b)?;
    if (a % b != 0) && ((a < 0) != (b < 0)) { Some(q - 1) } else { Some(q) }
}

/// Modulo `a % b` with the sign of `b` (b != 0). Never overflows.
#[inline]
pub fn modulo(a: i64, b: i64) -> i64 {
    debug_assert!(b != 0);
    let r = a.wrapping_rem(b);
    if r != 0 && ((r < 0) != (b < 0)) { r + b } else { r }
}

/// `divmod(a, b)` (b != 0). None only for `i64::MIN, -1`.
#[inline]
pub fn divmod(a: i64, b: i64) -> Option<(i64, i64)> {
    Some((floordiv(a, b)?, modulo(a, b)))
}

/// True division `a / b` (b != 0), correctly rounded.
pub fn truediv(a: i64, b: i64) -> f64 {
    debug_assert!(b != 0);
    const LIM: u64 = 1 << 53;
    if a.unsigned_abs() <= LIM && b.unsigned_abs() <= LIM {
        return a as f64 / b as f64;
    }
    let x = crate::BigInt::from_i64(a);
    let y = crate::BigInt::from_i64(b);
    crate::ops::truediv(x.as_ref(), y.as_ref()).expect("i64 quotient fits a float")
}

/// `a ** e` for e >= 0. None on overflow.
pub fn pow(a: i64, e: u64) -> Option<i64> {
    let e32 = u32::try_from(e).ok();
    match a {
        0 => Some(if e == 0 { 1 } else { 0 }),
        1 => Some(1),
        -1 => Some(if e & 1 == 1 { -1 } else { 1 }),
        _ => a.checked_pow(e32?),
    }
}

/// `pow(a, e, m)` for e >= 0, m != 0 (Python sign rules: result has the sign of m).
pub fn pow_mod(a: i64, e: u64, m: i64) -> i64 {
    debug_assert!(m != 0);
    let mm = m.unsigned_abs() as u128;
    if mm == 1 {
        return 0;
    }
    let mut base = (a as i128).rem_euclid(mm as i128) as u128;
    let mut r: u128 = 1;
    let mut e = e;
    while e > 0 {
        if e & 1 == 1 {
            r = r * base % mm;
        }
        base = base * base % mm;
        e >>= 1;
    }
    let r = r as i128;
    if m < 0 && r != 0 { (r - mm as i128) as i64 } else { r as i64 }
}

/// `a << n` (n >= 0). None on overflow.
#[inline]
pub fn lshift(a: i64, n: u64) -> Option<i64> {
    if a == 0 {
        return Some(0);
    }
    if n >= 63 {
        return None;
    }
    let r = a << n;
    if r >> n == a { Some(r) } else { None }
}

/// Arithmetic `a >> n` (n >= 0).
#[inline]
pub fn rshift(a: i64, n: u64) -> i64 {
    if n >= 64 {
        if a < 0 { -1 } else { 0 }
    } else {
        a >> n
    }
}

/// `int.bit_length()`.
#[inline]
pub fn bit_length(a: i64) -> u32 {
    64 - a.unsigned_abs().leading_zeros()
}

/// `int.bit_count()`.
#[inline]
pub fn bit_count(a: i64) -> u32 {
    a.unsigned_abs().count_ones()
}

/// Python hash.
#[inline]
pub fn hash(a: i64) -> i64 {
    crate::conv::hash_i64(a)
}

/// Correctly rounded float.
#[inline]
pub fn to_f64(a: i64) -> f64 {
    a as f64
}

/// `math.gcd`.
pub fn gcd(a: i64, b: i64) -> u64 {
    crate::math::gcd_u64(a.unsigned_abs(), b.unsigned_abs())
}

/// Does the value fit in a single 30-bit digit (|a| < 2^30)?
#[inline(always)]
pub fn is_single_digit(a: i64) -> bool {
    a.unsigned_abs() < (1 << 30)
}

/// Value of a compact (0 or 1 digit) int.
#[inline(always)]
pub fn compact_value(neg: bool, mag: &[u32]) -> i64 {
    match mag.first() {
        None => 0,
        Some(&d) => {
            if neg { -(d as i64) } else { d as i64 }
        }
    }
}

/// i64 value of a magnitude with at most 2 digits (always fits).
#[inline(always)]
pub fn two_digit_value(neg: bool, mag: &[u32]) -> i64 {
    let m = match mag.len() {
        0 => 0,
        1 => mag[0] as i64,
        _ => mag[0] as i64 | (mag[1] as i64) << 30,
    };
    if neg { -m } else { m }
}
