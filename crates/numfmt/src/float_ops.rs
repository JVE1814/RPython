//! float.hex, float.fromhex, round, as_integer_ratio.

use crate::dtoa::{self, Digits};
use crate::NumFmtError;

/// Append `float.hex(x)`.
pub fn write_float_hex(out: &mut String, x: f64) {
    if x.is_nan() {
        out.push_str("nan");
        return;
    }
    if x.is_infinite() {
        out.push_str(if x < 0.0 { "-inf" } else { "inf" });
        return;
    }
    if x.is_sign_negative() {
        out.push('-');
    }
    if x == 0.0 {
        out.push_str("0x0.0p+0");
        return;
    }
    let bits = x.to_bits();
    let bexp = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1u64 << 52) - 1);
    let (lead, exp) = if bexp == 0 { (0, -1022) } else { (1, bexp - 1023) };
    out.push_str(if lead == 1 { "0x1." } else { "0x0." });
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for i in (0..13).rev() {
        out.push(HEX[((frac >> (4 * i)) & 0xf) as usize] as char);
    }
    out.push('p');
    if exp < 0 {
        out.push('-');
    } else {
        out.push('+');
    }
    let mut buf = [0u8; 8];
    let n = crate::bignum::write_u64(exp.unsigned_abs() as u64, &mut buf);
    if n == 0 {
        out.push('0');
    } else {
        out.push_str(unsafe { core::str::from_utf8_unchecked(&buf[..n]) });
    }
}

/// `float.hex(x)`.
pub fn float_hex(x: f64) -> String {
    let mut s = String::with_capacity(24);
    write_float_hex(&mut s, x);
    s
}

#[inline]
fn py_isspace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r')
}

fn hexval(c: u8) -> Option<u32> {
    (c as char).to_digit(16)
}

/// `float.fromhex(s)`.
pub fn float_fromhex(s: &str) -> Result<f64, NumFmtError> {
    let invalid = || NumFmtError::value("invalid hexadecimal floating-point string");
    let overflow = || NumFmtError::overflow("hexadecimal value too large to represent as a float");
    let b = s.as_bytes();
    let n = b.len();
    let mut i = 0;
    while i < n && py_isspace(b[i]) {
        i += 1;
    }
    // inf / nan
    {
        let mut j = i;
        let mut neg = false;
        if j < n && (b[j] == b'+' || b[j] == b'-') {
            neg = b[j] == b'-';
            j += 1;
        }
        let ci = |at: usize, w: &[u8]| n >= at + w.len() && b[at..at + w.len()].eq_ignore_ascii_case(w);
        let special = if ci(j, b"inf") {
            j += 3;
            if ci(j, b"inity") {
                j += 5;
            }
            Some(if neg { f64::NEG_INFINITY } else { f64::INFINITY })
        } else if ci(j, b"nan") {
            j += 3;
            Some(f64::from_bits(f64::NAN.to_bits() & !(1 << 63) | ((neg as u64) << 63)))
        } else {
            None
        };
        if let Some(v) = special {
            while j < n && py_isspace(b[j]) {
                j += 1;
            }
            return if j == n { Ok(v) } else { Err(invalid()) };
        }
    }
    let mut neg = false;
    if i < n && (b[i] == b'+' || b[i] == b'-') {
        neg = b[i] == b'-';
        i += 1;
    }
    if i + 1 < n && b[i] == b'0' && (b[i + 1] == b'x' || b[i + 1] == b'X') {
        i += 2;
    }
    let int_start = i;
    while i < n && hexval(b[i]).is_some() {
        i += 1;
    }
    let int_end = i;
    let (mut frac_start, mut frac_end) = (i, i);
    if i < n && b[i] == b'.' {
        i += 1;
        frac_start = i;
        while i < n && hexval(b[i]).is_some() {
            i += 1;
        }
        frac_end = i;
    }
    let ndig_total = (int_end - int_start) + (frac_end - frac_start);
    if ndig_total == 0 {
        return Err(invalid());
    }
    let mut exp: i64 = 0;
    if i < n && (b[i] == b'p' || b[i] == b'P') {
        i += 1;
        let mut eneg = false;
        if i < n && (b[i] == b'+' || b[i] == b'-') {
            eneg = b[i] == b'-';
            i += 1;
        }
        let es = i;
        while i < n && b[i].is_ascii_digit() {
            if exp < 1_000_000_000_000_000 {
                exp = exp * 10 + (b[i] - b'0') as i64;
            }
            i += 1;
        }
        if i == es {
            return Err(invalid());
        }
        if eneg {
            exp = -exp;
        }
    }
    while i < n && py_isspace(b[i]) {
        i += 1;
    }
    if i != n {
        return Err(invalid());
    }
    let sign = |v: f64| if neg { -v } else { v };
    // Iterate over all hex digits (int then frac), skipping leading zeros.
    let digit_at = |k: usize| -> u32 {
        let li = int_end - int_start;
        let c = if k < li { b[int_start + k] } else { b[frac_start + (k - li)] };
        hexval(c).unwrap()
    };
    let mut first = 0;
    while first < ndig_total && digit_at(first) == 0 {
        first += 1;
    }
    if first == ndig_total {
        return Ok(sign(0.0));
    }
    let nd = ndig_total - first; // significant hex digits
    let nfrac = (frac_end - frac_start) as i64;
    // value = D * 2^(exp - 4*nfrac), D = the nd significant digits.
    let k = nd.min(16);
    let mut top: u64 = 0;
    for t in 0..k {
        top = (top << 4) | digit_at(first + t) as u64;
    }
    let mut sticky = false;
    for t in k..nd {
        if digit_at(first + t) != 0 {
            sticky = true;
            break;
        }
    }
    let low_exp = exp - 4 * nfrac + 4 * (nd - k) as i64; // value = (top + rest) * 2^low_exp
    let top_bits = 64 - top.leading_zeros() as i64;
    let e_top = low_exp + top_bits - 1; // value in [2^e_top, 2^(e_top+1))
    if e_top > 1023 {
        return Err(overflow());
    }
    if e_top < -1100 {
        return Ok(sign(0.0));
    }
    let q = (e_top - 52).max(-1074); // ulp exponent of the result
    let shift = q - low_exp; // right shift of top
    let m: u64 = if shift <= 0 {
        top << (-shift) as u32
    } else if shift >= 128 {
        0
    } else {
        let t = top as u128;
        let sh = shift as u32;
        let mut mm = t >> sh;
        let rem = t & ((1u128 << sh) - 1);
        let half = 1u128 << (sh - 1);
        if rem > half || (rem == half && (sticky || (mm & 1) == 1)) {
            mm += 1;
        }
        mm as u64
    };
    // m * 2^q, exact.
    let v = if q >= -1022 {
        (m as f64) * pow2(q as i32)
    } else {
        (m as f64) * pow2(q as i32 + 60) * pow2(-60)
    };
    if v.is_infinite() {
        return Err(overflow());
    }
    Ok(sign(v))
}

/// 2^e for -1022 <= e <= 1023.
fn pow2(e: i32) -> f64 {
    f64::from_bits(((e + 1023) as u64) << 52)
}

/// `float.as_integer_ratio()` decomposition: returns (negative, mantissa,
/// exp) with |x| = mantissa * 2^exp and mantissa odd (or 0 with exp 0).
/// The ratio is (±mantissa << exp, 1) for exp >= 0, else (±mantissa, 1 << -exp).
pub fn float_as_integer_ratio(x: f64) -> Result<(bool, u64, i32), NumFmtError> {
    if x.is_infinite() {
        return Err(NumFmtError::overflow("cannot convert Infinity to integer ratio"));
    }
    if x.is_nan() {
        return Err(NumFmtError::value("cannot convert NaN to integer ratio"));
    }
    if x == 0.0 {
        return Ok((false, 0, 0));
    }
    let (m, e) = dtoa::decompose(x.abs());
    let tz = m.trailing_zeros();
    Ok((x < 0.0, m >> tz, e + tz as i32))
}

/// `round(x)` with no ndigits: round half to even, returned as an integral
/// float for the caller to convert to an int.
pub fn float_round_to_int(x: f64) -> Result<f64, NumFmtError> {
    if x.is_infinite() {
        return Err(NumFmtError::overflow("cannot convert float infinity to integer"));
    }
    if x.is_nan() {
        return Err(NumFmtError::value("cannot convert float NaN to integer"));
    }
    Ok(x.round_ties_even())
}

/// `round(x, ndigits)` for a float (correctly rounded, half to even on the
/// exact value).  `ndigits` should be the Python int clamped to i64.
pub fn float_round(x: f64, ndigits: i64) -> Result<f64, NumFmtError> {
    const NDIGITS_MAX: i64 = 323;
    const NDIGITS_MIN: i64 = -308;
    if !x.is_finite() || x == 0.0 {
        return Ok(x);
    }
    if ndigits > NDIGITS_MAX {
        return Ok(x);
    }
    if ndigits < NDIGITS_MIN {
        return Ok(0.0 * x);
    }
    let mut d = Digits::new();
    dtoa::fixed(x.abs(), ndigits, &mut d);
    if d.len == 0 {
        return Ok(0.0 * x);
    }
    // value = 0.DIGITS * 10^decpt
    let mut s = String::with_capacity(d.len + 8);
    s.push_str(unsafe { core::str::from_utf8_unchecked(d.digits()) });
    s.push('e');
    s.push_str(&(d.decpt as i64 - d.len as i64).to_string());
    let v: f64 = s.parse().unwrap_or(f64::INFINITY);
    if v.is_infinite() {
        return Err(NumFmtError::overflow("rounded value too large to represent"));
    }
    Ok(if x < 0.0 { -v } else { v })
}
