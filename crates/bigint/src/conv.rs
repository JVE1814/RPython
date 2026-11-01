//! Conversions: machine integers, floats, bytes; Python hashing.

use crate::nat::{self, MASK, SHIFT};
use crate::{BigInt, IntError, IntRef};
use core::cmp::Ordering;

// ---------------------------------------------------------------------------------------------
// Machine integers
// ---------------------------------------------------------------------------------------------

/// Value as i64 if it fits.
#[inline]
pub fn to_i64(a: IntRef) -> Option<i64> {
    let m = nat::to_u64(a.mag)?;
    if a.neg {
        if m <= 1u64 << 63 { Some((m as i64).wrapping_neg()) } else { None }
    } else if m <= i64::MAX as u64 {
        Some(m as i64)
    } else {
        None
    }
}

/// Value as u64 if it fits (negative values do not fit).
pub fn to_u64(a: IntRef) -> Option<u64> {
    if a.neg {
        return None;
    }
    nat::to_u64(a.mag)
}

/// Value as i128 if it fits.
pub fn to_i128(a: IntRef) -> Option<i128> {
    let m = nat::to_u128(a.mag)?;
    if a.neg {
        if m <= 1u128 << 127 { Some((m as i128).wrapping_neg()) } else { None }
    } else if m <= i128::MAX as u128 {
        Some(m as i128)
    } else {
        None
    }
}

/// Value as u128 if it fits.
pub fn to_u128(a: IntRef) -> Option<u128> {
    if a.neg {
        return None;
    }
    nat::to_u128(a.mag)
}

/// Low 64 bits in two's complement (C `PyLong_AsUnsignedLongLongMask` semantics).
pub fn to_u64_wrapping(a: IntRef) -> u64 {
    let lo = nat::low_u64(a.mag);
    if a.neg { lo.wrapping_neg() } else { lo }
}

/// Value as i32 if it fits.
pub fn to_i32(a: IntRef) -> Option<i32> {
    to_i64(a).and_then(|v| i32::try_from(v).ok())
}

/// Write the digits of `v` into `out` (needs 3 digits); returns (neg, len). Allocation-free.
#[inline]
pub fn i64_to_digits(v: i64, out: &mut [u32; 3]) -> (bool, usize) {
    let mut m = v.unsigned_abs();
    let mut n = 0;
    while m != 0 {
        out[n] = (m as u32) & MASK;
        m >>= SHIFT;
        n += 1;
    }
    (v < 0, n)
}

// ---------------------------------------------------------------------------------------------
// Floats
// ---------------------------------------------------------------------------------------------

/// Truncating conversion from float (`int(x)`).
pub fn from_f64(x: f64) -> Result<BigInt, IntError> {
    if x.is_nan() {
        return Err(IntError::FloatNaN);
    }
    if x.is_infinite() {
        return Err(IntError::FloatInfinity);
    }
    let neg = x < 0.0;
    let ax = x.abs();
    if ax < 1.0 {
        return Ok(BigInt::zero());
    }
    if ax < 18446744073709551616.0 {
        return Ok(BigInt::from_parts(neg, nat::from_u64(ax as u64)));
    }
    let bits = ax.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i64 - 1075;
    let mant = (bits & ((1u64 << 52) - 1)) | (1u64 << 52);
    debug_assert!(exp > 0);
    let m = nat::shl(&nat::from_u64(mant), exp as u64);
    Ok(BigInt::from_parts(neg, m))
}

/// Top 64 bits of the magnitude with a sticky low bit, and the bit length.
/// The returned value `top` satisfies: |a| ~ top * 2^(bitlen - 64), sticky in bit 0.
fn top64_sticky(a: &[u32]) -> (u64, u64) {
    let nbits = nat::bit_length(a);
    if nbits <= 64 {
        return (nat::to_u64(a).unwrap_or_else(|| nat::low_u64(a)), nbits);
    }
    let pos = nbits - 64;
    let mut top = nat::extract_bits(a, pos, 64);
    if nat::any_bits_below(a, pos) {
        top |= 1;
    }
    (top, nbits)
}

/// Correctly rounded (round-half-even) conversion to float. OverflowError if too large.
pub fn to_f64(a: IntRef) -> Result<f64, IntError> {
    if a.mag.len() <= 2 {
        let m = nat::to_u64(a.mag).unwrap() as f64; // exact for < 2^60? rounds correctly
        return Ok(if a.neg { -m } else { m });
    }
    let (top, nbits) = top64_sticky(a.mag);
    if nbits > 1024 {
        return Err(IntError::FloatOverflow);
    }
    // u64 -> f64 conversion rounds to nearest-even; the sticky bit is below the rounding bit.
    let f = top as f64;
    let shift = nbits as i64 - 64;
    let r = if shift <= 0 { f } else { crate::ops::ldexp_exact(f, shift) };
    if r.is_infinite() {
        return Err(IntError::FloatOverflow);
    }
    Ok(if a.neg { -r } else { r })
}

/// frexp-style decomposition: returns (m, e) with 0.5 <= |m| < 1 (m = 0.0 for zero) and
/// value ~= m * 2^e, where m is the correctly rounded 53-bit mantissa. Never overflows; useful
/// for `math.log` of huge ints.
pub fn frexp(a: IntRef) -> (f64, u64) {
    if a.mag.is_empty() {
        return (0.0, 0);
    }
    let (top, nbits) = top64_sticky(a.mag);
    // top has exactly min(nbits, 64) significant bits
    let k = nbits.min(64) as i64;
    let f = top as f64; // correctly rounded
    // m = f / 2^k ; rounding could have produced 2^k exactly.
    let mut m = f * crate::ops::pow2f(-k);
    let mut e = nbits;
    if m >= 1.0 {
        m *= 0.5;
        e += 1;
    }
    (if a.neg { -m } else { m }, e)
}

/// Natural logarithm of a positive int (for math.log of huge ints). MathDomain for <= 0.
pub fn ln(a: IntRef) -> Result<f64, IntError> {
    if a.neg || a.mag.is_empty() {
        return Err(IntError::MathDomain);
    }
    if let Ok(f) = to_f64(a) {
        return Ok(f.ln());
    }
    let (m, e) = frexp(a);
    Ok(m.ln() + e as f64 * core::f64::consts::LN_2)
}

/// Exact comparison of an int with a float. Returns None if `f` is NaN.
pub fn cmp_f64(a: IntRef, f: f64) -> Option<Ordering> {
    if f.is_nan() {
        return None;
    }
    if f.is_infinite() {
        return Some(if f > 0.0 { Ordering::Less } else { Ordering::Greater });
    }
    let asign = a.signum();
    let fsign = if f > 0.0 { 1 } else if f < 0.0 { -1 } else { 0 };
    if asign != fsign {
        return Some(asign.cmp(&fsign));
    }
    if asign == 0 {
        return Some(Ordering::Equal);
    }
    // Same nonzero sign. Compare magnitudes.
    let af = f.abs();
    let nbits = nat::bit_length(a.mag);
    let fexp = if af >= 1.0 { ((af.to_bits() >> 52) & 0x7ff) as i64 - 1022 } else { 0 }; // bits of int part
    let mag_ord = if af < 1.0 {
        Ordering::Greater // |a| >= 1 > |f|
    } else if (nbits as i64) != fexp {
        (nbits as i64).cmp(&fexp)
    } else {
        let fi = af.trunc();
        let frac = af - fi;
        let fib = from_f64(fi).unwrap();
        match nat::cmp(a.mag, &fib.mag) {
            Ordering::Equal => {
                if frac > 0.0 { Ordering::Less } else { Ordering::Equal }
            }
            o => o,
        }
    };
    Some(if asign > 0 { mag_ord } else { mag_ord.reverse() })
}

// ---------------------------------------------------------------------------------------------
// Hash
// ---------------------------------------------------------------------------------------------

/// 2^61 - 1
pub const HASH_MODULUS: u64 = (1u64 << 61) - 1;

/// Python `hash(int)`: value mod (2^61 - 1) with sign; -1 maps to -2.
pub fn hash(a: IntRef) -> i64 {
    let mut x: u64 = 0;
    for &d in a.mag.iter().rev() {
        x = ((x << SHIFT) & HASH_MODULUS) | (x >> (61 - SHIFT));
        x += d as u64;
        if x >= HASH_MODULUS {
            x -= HASH_MODULUS;
        }
    }
    let mut h = x as i64;
    if a.neg {
        h = -h;
    }
    if h == -1 { -2 } else { h }
}

/// Python hash of an i64.
#[inline]
pub fn hash_i64(v: i64) -> i64 {
    let m = v.unsigned_abs() % HASH_MODULUS;
    let h = if v < 0 { -(m as i64) } else { m as i64 };
    if h == -1 { -2 } else { h }
}

// ---------------------------------------------------------------------------------------------
// Bytes
// ---------------------------------------------------------------------------------------------

/// Little-endian bytes of a magnitude, truncated/zero-extended to `length`.
fn mag_le_bytes(mag: &[u32], length: usize) -> Vec<u8> {
    let mut out = vec![0u8; length];
    let mut acc: u64 = 0;
    let mut accbits = 0u32;
    let mut i = 0usize;
    for &d in mag {
        acc |= (d as u64) << accbits;
        accbits += SHIFT;
        while accbits >= 8 {
            if i < length {
                out[i] = acc as u8;
            }
            i += 1;
            acc >>= 8;
            accbits -= 8;
        }
    }
    if accbits > 0 && i < length {
        out[i] = acc as u8;
    }
    out
}

/// `int.to_bytes(length, byteorder, signed=signed)`.
pub fn to_bytes(a: IntRef, length: usize, little: bool, signed: bool) -> Result<Vec<u8>, IntError> {
    if a.neg && !signed {
        return Err(IntError::NegativeToUnsigned);
    }
    if length == 0 {
        return if a.mag.is_empty() { Ok(Vec::new()) } else { Err(IntError::IntTooBig) };
    }
    let max_bits = 8 * length as u64 - signed as u64;
    let need = if a.neg { nat::bit_length(&nat::sub(a.mag, &[1])) } else { nat::bit_length(a.mag) };
    if need > max_bits {
        return Err(IntError::IntTooBig);
    }
    let mut out = mag_le_bytes(a.mag, length);
    if a.neg {
        let mut carry = 1u16;
        for b in out.iter_mut() {
            let t = (!*b) as u16 + carry;
            *b = t as u8;
            carry = t >> 8;
        }
    }
    if !little {
        out.reverse();
    }
    Ok(out)
}

/// `int.from_bytes(bytes, byteorder, signed=signed)`.
pub fn from_bytes(bytes: &[u8], little: bool, signed: bool) -> BigInt {
    let n = bytes.len();
    if n == 0 {
        return BigInt::zero();
    }
    let byte_at = |i: usize| -> u8 { if little { bytes[i] } else { bytes[n - 1 - i] } };
    let ndig = (n * 8).div_ceil(SHIFT as usize);
    let mut mag = vec![0u32; ndig];
    let mut acc: u64 = 0;
    let mut accbits = 0u32;
    let mut di = 0usize;
    for i in 0..n {
        acc |= (byte_at(i) as u64) << accbits;
        accbits += 8;
        if accbits >= SHIFT {
            mag[di] = (acc as u32) & MASK;
            di += 1;
            acc >>= SHIFT;
            accbits -= SHIFT;
        }
    }
    if accbits > 0 {
        mag[di] = acc as u32;
    }
    nat::normalize(&mut mag);
    if signed && byte_at(n - 1) & 0x80 != 0 {
        // value = raw - 2^(8n)
        let m = nat::sub(&nat::pow2(8 * n as u64), &mag);
        return BigInt::from_parts(true, m);
    }
    BigInt::from_parts(false, mag)
}

/// Number of bytes needed for `to_bytes` with `signed` (useful for `PyLong_AsNativeBytes`).
pub fn byte_length(a: IntRef, signed: bool) -> usize {
    if a.mag.is_empty() {
        return if signed { 1 } else { 0 };
    }
    let bits = if a.neg {
        // two's complement of -m needs bit_length(m-1)+1 bits
        let m1 = nat::sub(a.mag, &[1]);
        nat::bit_length(&m1) + 1
    } else {
        nat::bit_length(a.mag) + if signed { 1 } else { 0 }
    };
    bits.div_ceil(8) as usize
}
