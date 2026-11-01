//! Signed arithmetic with Python semantics on [`IntRef`] views.

use crate::nat::{self, MASK, SHIFT};
use crate::{BigInt, IntError, IntRef};
use core::cmp::Ordering;

/// Maximum number of digits of an int (CPython's MAX_LONG_DIGITS on 64-bit).
pub const MAX_DIGITS: usize = (isize::MAX as usize - 64) / 4;

/// Allocate a zeroed digit vector, reporting MemoryError / TooManyDigits instead of aborting.
pub fn alloc_digits(n: usize) -> Result<Vec<u32>, IntError> {
    if n > MAX_DIGITS {
        return Err(IntError::TooManyDigits);
    }
    let mut v: Vec<u32> = Vec::new();
    v.try_reserve_exact(n).map_err(|_| IntError::MemoryError)?;
    v.resize(n, 0);
    Ok(v)
}

#[inline]
fn mk(neg: bool, mag: Vec<u32>) -> BigInt {
    BigInt::from_parts(neg, mag)
}

/// Compare two ints.
pub fn cmp(a: IntRef, b: IntRef) -> Ordering {
    match (a.neg, b.neg) {
        (false, true) => Ordering::Greater,
        (true, false) => Ordering::Less,
        (false, false) => nat::cmp(a.mag, b.mag),
        (true, true) => nat::cmp(b.mag, a.mag),
    }
}

/// `a + b`.
pub fn add(a: IntRef, b: IntRef) -> BigInt {
    if a.neg == b.neg {
        mk(a.neg, nat::add(a.mag, b.mag))
    } else {
        let (d, lt) = nat::abs_diff(a.mag, b.mag);
        mk(if lt { b.neg } else { a.neg }, d)
    }
}

/// `a - b`.
pub fn sub(a: IntRef, b: IntRef) -> BigInt {
    add(a, b.negated())
}

/// `a * b`.
pub fn mul(a: IntRef, b: IntRef) -> BigInt {
    mk(a.neg != b.neg, nat::mul(a.mag, b.mag))
}

/// Signed "into" add: writes |a + b| into `out` (len >= max(len)+1).
/// Returns (negative, normalized length).
pub fn add_into(out: &mut [u32], a: IntRef, b: IntRef) -> (bool, usize) {
    if a.neg == b.neg {
        let n = nat::add_into(out, a.mag, b.mag);
        (a.neg && n > 0, n)
    } else {
        match nat::cmp(a.mag, b.mag) {
            Ordering::Equal => (false, 0),
            Ordering::Greater => {
                let n = nat::sub_into(out, a.mag, b.mag);
                (a.neg, n)
            }
            Ordering::Less => {
                let n = nat::sub_into(out, b.mag, a.mag);
                (b.neg, n)
            }
        }
    }
}

/// Signed "into" subtract (see [`add_into`]).
pub fn sub_into(out: &mut [u32], a: IntRef, b: IntRef) -> (bool, usize) {
    add_into(out, a, b.negated())
}

/// Signed "into" multiply: out.len() >= a.len()+b.len().
pub fn mul_into(out: &mut [u32], a: IntRef, b: IntRef) -> (bool, usize) {
    let n = crate::mul::mul_into(out, a.mag, b.mag);
    (n > 0 && a.neg != b.neg, n)
}

/// Floor division and modulo (Python `divmod`).
pub fn divmod(a: IntRef, b: IntRef) -> Result<(BigInt, BigInt), IntError> {
    if b.mag.is_empty() {
        return Err(IntError::DivisionByZero);
    }
    let (q, r) = nat::divrem(a.mag, b.mag);
    Ok(fix_floor(a.neg, b.neg, q, r, b.mag))
}

fn fix_floor(aneg: bool, bneg: bool, mut q: Vec<u32>, r: Vec<u32>, bmag: &[u32]) -> (BigInt, BigInt) {
    if aneg == bneg {
        (mk(false, q), mk(aneg, r))
    } else if r.is_empty() {
        (mk(true, q), BigInt::zero())
    } else {
        nat::add_in_place(&mut q, &[1]);
        let r2 = nat::sub(bmag, &r);
        (mk(true, q), mk(bneg, r2))
    }
}

/// Floor division `a // b`.
pub fn floordiv(a: IntRef, b: IntRef) -> Result<BigInt, IntError> {
    if b.mag.is_empty() {
        return Err(IntError::DivisionByZero);
    }
    if a.neg == b.neg {
        // No remainder fix needed beyond truncation.
        if b.mag.len() == 1 {
            let (q, _) = nat::divrem_u32(a.mag, b.mag[0]);
            return Ok(mk(false, q));
        }
        let (q, _) = nat::divrem(a.mag, b.mag);
        return Ok(mk(false, q));
    }
    let (q, _) = divmod(a, b)?;
    Ok(q)
}

/// Modulo `a % b` (result has the sign of `b`).
pub fn modulo(a: IntRef, b: IntRef) -> Result<BigInt, IntError> {
    if b.mag.is_empty() {
        return Err(IntError::ModuloByZero);
    }
    if b.mag.len() == 1 {
        let r = nat::rem_digit(a.mag, b.mag[0]);
        let rv = if r == 0 { Vec::new() } else { vec![r] };
        if a.neg == b.neg || r == 0 {
            return Ok(mk(a.neg, rv));
        }
        return Ok(mk(b.neg, vec![b.mag[0] - r]));
    }
    let (_, r) = divmod(a, b)?;
    Ok(r)
}

/// Truncating division and remainder (C semantics), used by e.g. `math.fmod`-like helpers.
pub fn divrem_trunc(a: IntRef, b: IntRef) -> Result<(BigInt, BigInt), IntError> {
    if b.mag.is_empty() {
        return Err(IntError::DivisionByZero);
    }
    let (q, r) = nat::divrem(a.mag, b.mag);
    Ok((mk(a.neg != b.neg, q), mk(a.neg, r)))
}

/// Floor divmod writing into caller buffers. `q_out.len() >= a.len() - b.len() + 2` (or 1 if
/// a is shorter), `r_out.len() >= b.len()`. Returns (q_neg, q_len, r_neg, r_len).
pub fn divmod_into(
    q_out: &mut [u32],
    r_out: &mut [u32],
    a: IntRef,
    b: IntRef,
) -> Result<(bool, usize, bool, usize), IntError> {
    let (q, r) = divmod(a, b)?;
    q_out[..q.mag.len()].copy_from_slice(&q.mag);
    r_out[..r.mag.len()].copy_from_slice(&r.mag);
    Ok((q.neg, q.mag.len(), r.neg, r.mag.len()))
}

// ---------------------------------------------------------------------------------------------
// True division
// ---------------------------------------------------------------------------------------------

const DBL_MANT_DIG: i64 = 53;
const DBL_MIN_EXP: i64 = -1021;
const DBL_MAX_EXP: i64 = 1024;

/// Correctly rounded true division `a / b`.
pub fn truediv(a: IntRef, b: IntRef) -> Result<f64, IntError> {
    if b.mag.is_empty() {
        return Err(IntError::TrueDivisionByZero);
    }
    let negate = a.neg != b.neg;
    if a.mag.is_empty() {
        return Ok(if negate { -0.0 } else { 0.0 });
    }
    // Fast path: both exactly representable.
    let abits = nat::bit_length(a.mag) as i64;
    let bbits = nat::bit_length(b.mag) as i64;
    if abits <= DBL_MANT_DIG && bbits <= DBL_MANT_DIG {
        let x = nat::to_u64(a.mag).unwrap() as f64;
        let y = nat::to_u64(b.mag).unwrap() as f64;
        let r = x / y;
        return Ok(if negate { -r } else { r });
    }
    // d with 2^(d-1) <= a/b < 2^d
    let mut d = abits - bbits;
    let ge = if d >= 0 {
        cmp_shifted(a.mag, b.mag, d as u64) != Ordering::Less
    } else {
        cmp_shifted(b.mag, a.mag, (-d) as u64) != Ordering::Greater
    };
    if ge {
        d += 1;
    }
    if d > DBL_MAX_EXP {
        return Err(IntError::TrueDivOverflow);
    }
    if d < DBL_MIN_EXP - DBL_MANT_DIG - 1 {
        // a/b < 2^-1076: rounds to zero.
        return Ok(if negate { -0.0 } else { 0.0 });
    }
    let exp = d.max(DBL_MIN_EXP) - DBL_MANT_DIG;
    let (q, r, bb);
    if exp < 0 {
        let aa = nat::shl(a.mag, (-exp) as u64);
        let (q0, r0) = nat::divrem(&aa, b.mag);
        q = q0;
        r = r0;
        bb = b.mag.to_vec();
    } else {
        let bs = nat::shl(b.mag, exp as u64);
        let (q0, r0) = nat::divrem(a.mag, &bs);
        q = q0;
        r = r0;
        bb = bs;
    }
    let mut qv = nat::to_u64(&q).expect("quotient fits");
    // round half to even
    let r2 = nat::shl(&r, 1);
    match nat::cmp(&r2, &bb) {
        Ordering::Greater => qv += 1,
        Ordering::Equal => {
            if qv & 1 == 1 {
                qv += 1
            }
        }
        Ordering::Less => {}
    }
    if d == DBL_MAX_EXP && qv == 1u64 << DBL_MANT_DIG {
        return Err(IntError::TrueDivOverflow);
    }
    let res = ldexp_exact(qv as f64, exp);
    if res.is_infinite() {
        return Err(IntError::TrueDivOverflow);
    }
    Ok(if negate { -res } else { res })
}

/// Compare `x` with `y << s`.
fn cmp_shifted(x: &[u32], y: &[u32], s: u64) -> Ordering {
    let ys = nat::shl(y, s);
    nat::cmp(x, &ys)
}

/// `x * 2^e` for an integer-valued `x` with 1 <= |x| < 2^64: rounds once (correctly), gives
/// inf on overflow and a correctly rounded subnormal/zero on underflow.
pub fn ldexp_exact(x: f64, e: i64) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    if e > 1023 {
        if e > 2046 + 64 {
            return x * f64::INFINITY;
        }
        let y = x * pow2f(1023);
        let rest = e - 1023;
        return if rest > 1023 { y * pow2f(1023) * pow2f(rest - 1023) } else { y * pow2f(rest) };
    }
    if e < -1022 {
        if e < -2044 {
            return x * 0.0;
        }
        let y = x * pow2f(e + 1022); // exact: |y| >= 2^-1022
        return y * pow2f(-1022);
    }
    x * pow2f(e)
}

/// 2^e as f64 for -1022 <= e <= 1023.
#[inline]
pub fn pow2f(e: i64) -> f64 {
    debug_assert!((-1022..=1023).contains(&e));
    f64::from_bits(((e + 1023) as u64) << 52)
}

// ---------------------------------------------------------------------------------------------
// Shifts
// ---------------------------------------------------------------------------------------------

/// `a << n`.
pub fn lshift(a: IntRef, n: u64) -> Result<BigInt, IntError> {
    if a.mag.is_empty() {
        return Ok(BigInt::zero());
    }
    let dshift = n / SHIFT as u64;
    let total = (a.mag.len() as u64).checked_add(dshift).and_then(|x| x.checked_add(1));
    let total = match total {
        Some(t) if t <= MAX_DIGITS as u64 => t as usize,
        _ => return Err(IntError::TooManyDigits),
    };
    let mut out = alloc_digits(total)?;
    let len = nat::shl_into(&mut out, a.mag, n);
    out.truncate(len);
    Ok(mk(a.neg, out))
}

/// `a << n` where `n` is an arbitrary int.
pub fn lshift_big(a: IntRef, n: IntRef) -> Result<BigInt, IntError> {
    if n.neg {
        return Err(IntError::NegativeShiftCount);
    }
    if a.mag.is_empty() {
        return Ok(BigInt::zero());
    }
    match nat::to_u64(n.mag) {
        Some(s) => lshift(a, s),
        None => Err(IntError::TooManyDigits),
    }
}

/// Arithmetic right shift `a >> n` (floor division by 2^n).
pub fn rshift(a: IntRef, n: u64) -> BigInt {
    if !a.neg {
        return mk(false, nat::shr(a.mag, n));
    }
    // floor(-m / 2^n) = -ceil(m / 2^n)
    let mut q = nat::shr(a.mag, n);
    if nat::any_bits_below(a.mag, n) {
        nat::add_in_place(&mut q, &[1]);
    }
    mk(true, q)
}

/// `a >> n` where `n` is an arbitrary int.
pub fn rshift_big(a: IntRef, n: IntRef) -> Result<BigInt, IntError> {
    if n.neg {
        return Err(IntError::NegativeShiftCount);
    }
    match nat::to_u64(n.mag) {
        Some(s) => Ok(rshift(a, s)),
        None => Ok(if a.neg { BigInt::from_i64(-1) } else { BigInt::zero() }),
    }
}

// ---------------------------------------------------------------------------------------------
// Bitwise operations (infinite two's complement)
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum BitOp {
    And,
    Or,
    Xor,
}

/// Two's complement digits of `x` sign-extended to `n` digits.
fn twos(x: IntRef, n: usize) -> Vec<u32> {
    let mut v = vec![0u32; n];
    v[..x.mag.len()].copy_from_slice(x.mag);
    if x.neg {
        let mut carry = 1u32;
        for d in v.iter_mut() {
            let t = (*d ^ MASK) + carry;
            *d = t & MASK;
            carry = t >> SHIFT;
        }
    }
    v
}

fn bitop(a: IntRef, b: IntRef, op: BitOp) -> BigInt {
    // Fast path: both non-negative.
    if !a.neg && !b.neg {
        let (x, y) = if a.mag.len() >= b.mag.len() { (a.mag, b.mag) } else { (b.mag, a.mag) };
        let mut v: Vec<u32>;
        match op {
            BitOp::And => {
                v = y.iter().zip(x.iter()).map(|(p, q)| p & q).collect();
            }
            BitOp::Or => {
                v = x.to_vec();
                for (d, s) in v.iter_mut().zip(y.iter()) {
                    *d |= s;
                }
            }
            BitOp::Xor => {
                v = x.to_vec();
                for (d, s) in v.iter_mut().zip(y.iter()) {
                    *d ^= s;
                }
            }
        }
        nat::normalize(&mut v);
        return mk(false, v);
    }
    let n = a.mag.len().max(b.mag.len()) + 1;
    let ta = twos(a, n);
    let tb = twos(b, n);
    let rneg = match op {
        BitOp::And => a.neg & b.neg,
        BitOp::Or => a.neg | b.neg,
        BitOp::Xor => a.neg ^ b.neg,
    };
    let mut r: Vec<u32> = match op {
        BitOp::And => ta.iter().zip(tb.iter()).map(|(p, q)| p & q).collect(),
        BitOp::Or => ta.iter().zip(tb.iter()).map(|(p, q)| p | q).collect(),
        BitOp::Xor => ta.iter().zip(tb.iter()).map(|(p, q)| p ^ q).collect(),
    };
    if rneg {
        // magnitude = two's complement negation
        let mut carry = 1u32;
        for d in r.iter_mut() {
            let t = (*d ^ MASK) + carry;
            *d = t & MASK;
            carry = t >> SHIFT;
        }
    }
    nat::normalize(&mut r);
    mk(rneg, r)
}

/// `a & b`.
pub fn and(a: IntRef, b: IntRef) -> BigInt {
    bitop(a, b, BitOp::And)
}
/// `a | b`.
pub fn or(a: IntRef, b: IntRef) -> BigInt {
    bitop(a, b, BitOp::Or)
}
/// `a ^ b`.
pub fn xor(a: IntRef, b: IntRef) -> BigInt {
    bitop(a, b, BitOp::Xor)
}
/// `~a` = -(a+1).
pub fn invert(a: IntRef) -> BigInt {
    if a.neg {
        // ~(-m) = m - 1
        mk(false, nat::sub(a.mag, &[1]))
    } else {
        mk(true, nat::add(a.mag, &[1]))
    }
}

// ---------------------------------------------------------------------------------------------
// Power
// ---------------------------------------------------------------------------------------------

/// `a ** e` for a non-negative exponent. Negative exponents return `NegativeExponent`
/// (Python computes a float result in that case).
pub fn pow(a: IntRef, e: IntRef) -> Result<BigInt, IntError> {
    if e.neg {
        return Err(IntError::NegativeExponent);
    }
    if e.mag.is_empty() {
        return Ok(BigInt::from_i64(1));
    }
    if a.mag.is_empty() {
        return Ok(BigInt::zero());
    }
    let odd = e.mag[0] & 1 == 1;
    if a.mag == [1] {
        return Ok(mk(a.neg && odd, vec![1]));
    }
    let ev = match nat::to_u64(e.mag) {
        Some(v) => v,
        None => return Err(IntError::MemoryError),
    };
    pow_u64(a, ev)
}

/// `a ** e` for a u64 exponent.
pub fn pow_u64(a: IntRef, e: u64) -> Result<BigInt, IntError> {
    if e == 0 {
        return Ok(BigInt::from_i64(1));
    }
    if a.mag.is_empty() {
        return Ok(BigInt::zero());
    }
    let neg = a.neg && (e & 1 == 1);
    if a.mag == [1] {
        return Ok(mk(neg, vec![1]));
    }
    let bits = nat::bit_length(a.mag);
    // Result bit length is in ((bits-1)*e, bits*e].
    let est = (bits as u128 - 1) * e as u128;
    if est / SHIFT as u128 > MAX_DIGITS as u128 {
        return Err(IntError::TooManyDigits);
    }
    if est > (1u128 << 40) {
        return Err(IntError::MemoryError);
    }
    // Power of two base: shift.
    let tz = nat::trailing_zeros(a.mag).unwrap();
    if tz + 1 == bits {
        let r = lshift(BigInt::from_i64(1).as_ref(), tz * e)?;
        return Ok(mk(neg, r.mag));
    }
    // Strip factors of two: a = odd * 2^tz.
    let odd: Vec<u32> = if tz > 0 { nat::shr(a.mag, tz) } else { a.mag.to_vec() };
    let mut result = pow_mag(&odd, e);
    if tz > 0 {
        result = nat::shl(&result, tz * e);
    }
    Ok(mk(neg, result))
}

/// Magnitude power by left-to-right binary exponentiation.
pub fn pow_mag(a: &[u32], e: u64) -> Vec<u32> {
    if e == 0 {
        return vec![1];
    }
    if a.len() == 1 && a[0] < (1 << 15) && e <= 2 {
        return nat::from_u64((a[0] as u64).pow(e as u32));
    }
    // Small single-digit base: accumulate powers that fit in a u32 multiplier.
    let top = 63 - e.leading_zeros();
    let mut r = a.to_vec();
    for i in (0..top).rev() {
        r = nat::sqr(&r);
        if (e >> i) & 1 == 1 {
            r = if a.len() == 1 { nat::mul_u32(&r, a[0]) } else { nat::mul(&r, a) };
        }
    }
    r
}

/// Modular inverse of `a` modulo `m` (m > 0), or None if not invertible.
pub fn mod_inverse(a: &[u32], m: &[u32]) -> Option<Vec<u32>> {
    // Extended Euclid on magnitudes tracking signed coefficient of a.
    // Invariant: r0 = s0 * a (mod m), r1 = s1 * a (mod m).
    let a_red = if nat::cmp(a, m) != Ordering::Less { nat::divrem(a, m).1 } else { a.to_vec() };
    let mut r0 = m.to_vec();
    let mut r1 = a_red;
    let mut s0 = BigInt::zero();
    let mut s1 = BigInt::from_i64(1);
    while !r1.is_empty() {
        let (q, r) = nat::divrem(&r0, &r1);
        let qs = BigInt::from_parts(false, q);
        let s2 = &s0 - &(&qs * &s1);
        r0 = r1;
        r1 = r;
        s0 = s1;
        s1 = s2;
    }
    if r0 != [1] {
        return None;
    }
    // s0 * a = 1 mod m; normalize into [0, m)
    let mm = BigInt::from_parts(false, m.to_vec());
    let inv = modulo(s0.as_ref(), mm.as_ref()).ok()?;
    Some(inv.mag)
}

/// `pow(a, e, m)` with Python semantics.
pub fn pow_mod(a: IntRef, e: IntRef, m: IntRef) -> Result<BigInt, IntError> {
    if m.mag.is_empty() {
        return Err(IntError::PowModulusZero);
    }
    let mmag = m.mag;
    if mmag == [1] {
        return Ok(BigInt::zero());
    }
    let mpos = BigInt::from_parts(false, mmag.to_vec());
    // base reduced into [0, |m|)
    let mut base = modulo(a, mpos.as_ref())?.mag;
    if e.neg {
        base = match mod_inverse(&base, mmag) {
            Some(v) => v,
            None => return Err(IntError::NotInvertible),
        };
    }
    let r = crate::modpow::modpow(&base, e.mag, mmag);
    if m.neg && !r.is_empty() {
        Ok(mk(true, nat::sub(mmag, &r)))
    } else {
        Ok(mk(false, r))
    }
}

/// Integer absolute value.
pub fn abs(a: IntRef) -> BigInt {
    mk(false, a.mag.to_vec())
}

/// Negation.
pub fn neg(a: IntRef) -> BigInt {
    mk(!a.neg, a.mag.to_vec())
}

/// `round(a, -n)` for n >= 0: round to a multiple of 10^n, ties to even (Python semantics).
pub fn round_neg_ndigits(a: IntRef, n: u64) -> BigInt {
    if n == 0 || a.mag.is_empty() {
        return a.to_owned();
    }
    // 10^n > 2|a| implies the result is 0 (|a| < 10^n / 2).
    let abits = nat::bit_length(a.mag);
    if (n as f64) * core::f64::consts::LOG2_10 > abits as f64 + 2.0 {
        return BigInt::zero();
    }
    let p = pow_u64(BigInt::from_i64(10).as_ref(), n).expect("small power");
    let (mut q, r) = divmod(a, p.as_ref()).expect("nonzero");
    let r2 = nat::shl(&r.mag, 1);
    let c = nat::cmp(&r2, &p.mag);
    let q_odd = q.mag.first().is_some_and(|d| d & 1 == 1);
    if c == Ordering::Greater || (c == Ordering::Equal && q_odd) {
        q = add(q.as_ref(), BigInt::from_i64(1).as_ref());
    }
    mul(q.as_ref(), p.as_ref())
}
