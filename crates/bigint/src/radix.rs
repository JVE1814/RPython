//! String conversion in radix 2..=36 (lowercase), subquadratic for huge numbers, and Python's
//! `int(str, base)` parser.

use crate::div::Divisor;

/// Use cached-reciprocal (Barrett) division for powers with at least this many digits.
const POWDIV_BARRETT: usize = 1500;
use crate::nat::{self, MASK, SHIFT};
use crate::{BigInt, IntError, IntRef};

const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// Numeric value of an ASCII digit character (0..=35), or 255 if not a digit.
#[inline]
pub fn digit_value(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'z' => c - b'a' + 10,
        b'A'..=b'Z' => c - b'A' + 10,
        _ => 255,
    }
}

/// Largest power of `radix` that fits in a u32 and its exponent: (radix^k, k).
pub fn chunk_power(radix: u32) -> (u32, u32) {
    let mut p: u64 = radix as u64;
    let mut k = 1;
    while p * radix as u64 <= u32::MAX as u64 {
        p *= radix as u64;
        k += 1;
    }
    (p as u32, k)
}

// ---------------------------------------------------------------------------------------------
// int -> string
// ---------------------------------------------------------------------------------------------

/// Below this many digits use the quadratic base conversion.
const TO_STR_BASECASE: usize = 120;
/// Below this many chunks use quadratic string -> int.
const FROM_STR_BASECASE: usize = 160;

/// Convert a magnitude to little-endian limbs in base `P` (quadratic).
#[inline(always)]
fn to_limbs_generic<const P: u64>(a: &[u32], p_rt: u64, out: &mut Vec<u32>) {
    let p = if P != 0 { P } else { p_rt };
    out.clear();
    for &d in a.iter().rev() {
        let mut carry = d as u64;
        for limb in out.iter_mut() {
            let z = ((*limb as u64) << SHIFT) | carry;
            carry = z / p;
            *limb = (z - carry * p) as u32;
        }
        while carry != 0 {
            out.push((carry % p) as u32);
            carry /= p;
        }
    }
}

fn to_limbs(a: &[u32], p: u32, out: &mut Vec<u32>) {
    match p {
        1_000_000_000 => to_limbs_generic::<1_000_000_000>(a, 0, out),
        _ => to_limbs_generic::<0>(a, p as u64, out),
    }
}

enum PowDiv {
    Plain(Vec<u32>),
    Barrett(Divisor),
}

impl PowDiv {
    fn new(p: Vec<u32>, xlen: usize) -> PowDiv {
        // A cached reciprocal pays off when the power is used for several divisions.
        if p.len() >= POWDIV_BARRETT && xlen >= 6 * p.len() {
            PowDiv::Barrett(Divisor::new(&p))
        } else {
            PowDiv::Plain(p)
        }
    }
    fn value(&self) -> &[u32] {
        match self {
            PowDiv::Plain(p) => p,
            PowDiv::Barrett(d) => &d.orig,
        }
    }
    fn divrem(&self, x: &[u32]) -> (Vec<u32>, Vec<u32>) {
        match self {
            PowDiv::Plain(p) => nat::divrem(x, p),
            PowDiv::Barrett(d) => d.divrem(x),
        }
    }
}

/// Recursive conversion of `x` into exactly `out.len()` limbs (a power of two count, 2^(j+1))
/// using `pows[j] = P^(2^j)`.
fn to_limbs_rec(x: &[u32], j: usize, pows: &[PowDiv], p: u32, out: &mut [u32], tmp: &mut Vec<u32>) {
    if x.len() <= TO_STR_BASECASE || j == usize::MAX {
        to_limbs(x, p, tmp);
        debug_assert!(tmp.len() <= out.len());
        out[..tmp.len()].copy_from_slice(tmp);
        out[tmp.len()..].fill(0);
        return;
    }
    let half = out.len() / 2;
    let (lo, hi) = out.split_at_mut(half);
    if nat::cmp(x, pows[j].value()) == core::cmp::Ordering::Less {
        hi.fill(0);
        to_limbs_rec(x, j.wrapping_sub(1), pows, p, lo, tmp);
        return;
    }
    let (q, r) = pows[j].divrem(x);
    to_limbs_rec(&r, j.wrapping_sub(1), pows, p, lo, tmp);
    to_limbs_rec(&q, j.wrapping_sub(1), pows, p, hi, tmp);
}

/// Magnitude -> little-endian limbs in base P = radix^k (subquadratic for large inputs).
fn mag_to_limbs(a: &[u32], p: u32) -> Vec<u32> {
    let mut tmp = Vec::new();
    if a.len() <= TO_STR_BASECASE {
        to_limbs(a, p, &mut tmp);
        return tmp;
    }
    // pows[j] = P^(2^j) for j = 0..=J, where J is the smallest index with a < pows[J]^2.
    let abits = nat::bit_length(a);
    let mut raw: Vec<Vec<u32>> = vec![nat::from_u64(p as u64)];
    loop {
        let last = raw.last().unwrap();
        let bl = nat::bit_length(last);
        if 2 * bl - 2 >= abits {
            break;
        }
        let sq = nat::sqr(last);
        if nat::cmp(a, &sq) == core::cmp::Ordering::Less {
            break;
        }
        raw.push(sq);
    }
    let pows: Vec<PowDiv> = raw.into_iter().map(|p| PowDiv::new(p, a.len())).collect();
    let j = pows.len() - 1;
    let mut out = vec![0u32; 1usize << (j + 1)];
    to_limbs_rec(a, j, &pows, p, &mut out, &mut tmp);
    let n = nat::trimmed_len(&out);
    out.truncate(n);
    out
}

fn limbs_to_string(limbs: &[u32], radix: u32, k: u32, neg: bool, prefix: &str) -> String {
    if limbs.is_empty() {
        let mut s = String::from(prefix);
        s.push('0');
        return s;
    }
    let mut s: Vec<u8> = Vec::with_capacity(limbs.len() * k as usize + 2 + prefix.len());
    if neg {
        s.push(b'-');
    }
    s.extend_from_slice(prefix.as_bytes());
    // top limb without padding
    let top = *limbs.last().unwrap();
    let mut buf = [0u8; 32];
    let mut n = 0;
    let mut t = top;
    while t != 0 {
        buf[n] = DIGITS[(t % radix) as usize];
        t /= radix;
        n += 1;
    }
    for i in (0..n).rev() {
        s.push(buf[i]);
    }
    for &limb in limbs[..limbs.len() - 1].iter().rev() {
        let start = s.len();
        s.resize(start + k as usize, b'0');
        let mut t = limb;
        let mut i = k as usize;
        if radix == 10 {
            while t != 0 {
                i -= 1;
                s[start + i] = b'0' + (t % 10) as u8;
                t /= 10;
            }
        } else {
            while t != 0 {
                i -= 1;
                s[start + i] = DIGITS[(t % radix) as usize];
                t /= radix;
            }
        }
    }
    // SAFETY: only ASCII bytes were written.
    unsafe { String::from_utf8_unchecked(s) }
}

fn pow2_to_string(a: &[u32], bits_per: u32, neg: bool, prefix: &str) -> String {
    let nbits = nat::bit_length(a);
    if nbits == 0 {
        let mut s = String::from(prefix);
        s.push('0');
        return s;
    }
    let ndig = nbits.div_ceil(bits_per as u64) as usize;
    let mut s: Vec<u8> = Vec::with_capacity(ndig + 1 + prefix.len());
    if neg {
        s.push(b'-');
    }
    s.extend_from_slice(prefix.as_bytes());
    let mask = (1u64 << bits_per) - 1;
    for i in (0..ndig).rev() {
        let v = nat::extract_bits(a, i as u64 * bits_per as u64, bits_per) & mask;
        s.push(DIGITS[v as usize]);
    }
    unsafe { String::from_utf8_unchecked(s) }
}

/// Convert to a string in `radix` (2..=36): lowercase digits, leading '-' for negatives, no
/// prefix.
pub fn to_string(a: IntRef, radix: u32) -> String {
    to_string_prefixed(a, radix, "")
}

/// Like [`to_string`] but inserts `prefix` (e.g. "0x") after the sign.
pub fn to_string_prefixed(a: IntRef, radix: u32, prefix: &str) -> String {
    assert!((2..=36).contains(&radix));
    if radix.is_power_of_two() {
        return pow2_to_string(a.mag, radix.trailing_zeros(), a.neg, prefix);
    }
    if let Some(v) = nat::to_u64(a.mag) {
        let mut buf = [0u8; 72];
        let mut n = 0;
        let mut t = v;
        if t == 0 {
            buf[0] = b'0';
            n = 1;
        }
        while t != 0 {
            buf[n] = DIGITS[(t % radix as u64) as usize];
            t /= radix as u64;
            n += 1;
        }
        let mut s = String::with_capacity(n + 1 + prefix.len());
        if a.neg {
            s.push('-');
        }
        s.push_str(prefix);
        for i in (0..n).rev() {
            s.push(buf[i] as char);
        }
        return s;
    }
    let (p, k) = chunk_power(radix);
    let limbs = mag_to_limbs(a.mag, p);
    limbs_to_string(&limbs, radix, k, a.neg, prefix)
}

/// Decimal string with CPython's `sys.int_max_str_digits` check (`limit == 0` disables it).
/// The digit count excludes the sign.
pub fn to_decimal_string_limited(a: IntRef, limit: usize) -> Result<String, IntError> {
    if limit > 0 {
        let bits = nat::bit_length(a.mag);
        if bits > 0 {
            // Lower bound on the number of decimal digits: floor((bits-1)*log10(2)) + 1.
            let lower = ((bits - 1) as f64 * core::f64::consts::LOG10_2 * (1.0 - 1e-12)) as u64 + 1;
            if lower > limit as u64 {
                return Err(IntError::IntToStrDigitLimit { limit });
            }
        }
    }
    let s = to_string(a, 10);
    if limit > 0 {
        let nd = s.len() - a.neg as usize;
        if nd > limit {
            return Err(IntError::IntToStrDigitLimit { limit });
        }
    }
    Ok(s)
}

// ---------------------------------------------------------------------------------------------
// string -> int
// ---------------------------------------------------------------------------------------------

/// Parse a pure digit string (ASCII digits valid for `radix`, nothing else) into a magnitude.
/// Returns None on an invalid character or empty input.
pub fn from_digits(s: &[u8], radix: u32) -> Option<Vec<u32>> {
    if s.is_empty() || !(2..=36).contains(&radix) {
        return None;
    }
    for &c in s {
        if digit_value(c) as u32 >= radix {
            return None;
        }
    }
    Some(from_valid_digits(s, radix))
}

/// Parse digits already validated for `radix`.
fn from_valid_digits(s: &[u8], radix: u32) -> Vec<u32> {
    if s.len() <= 12 || (radix <= 10 && s.len() <= 19) {
        // fits in u64 (36^12 < 2^63, 10^19 < 2^64)
        let mut v: u64 = 0;
        for &c in s {
            v = v * radix as u64 + digit_value(c) as u64;
        }
        return nat::from_u64(v);
    }
    if radix.is_power_of_two() {
        let bits_per = radix.trailing_zeros();
        let total = s.len() as u64 * bits_per as u64;
        let mut mag = vec![0u32; (total / SHIFT as u64) as usize + 2];
        let mut acc: u64 = 0;
        let mut accbits = 0u32;
        let mut di = 0;
        for &c in s.iter().rev() {
            acc |= (digit_value(c) as u64) << accbits;
            accbits += bits_per;
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
        return mag;
    }
    let (p, k) = chunk_power(radix);
    // chunk values, most significant first; first chunk may be shorter
    let k = k as usize;
    let first = s.len() % k;
    let nchunks = s.len() / k + (first > 0) as usize;
    let mut chunks: Vec<u32> = Vec::with_capacity(nchunks);
    let mut pos = 0;
    let parse = |part: &[u8]| -> u32 {
        let mut v: u32 = 0;
        for &c in part {
            v = v * radix + digit_value(c) as u32;
        }
        v
    };
    if first > 0 {
        chunks.push(parse(&s[..first]));
        pos = first;
    }
    while pos < s.len() {
        chunks.push(parse(&s[pos..pos + k]));
        pos += k;
    }
    let first_pow = if first > 0 { (radix as u64).pow(first as u32) as u32 } else { p };
    chunks_to_mag(&chunks, p, first_pow)
}

/// Quadratic combination of chunks (most significant first).
fn chunks_basecase(chunks: &[u32], p: u32) -> Vec<u32> {
    let mut v: Vec<u32> = Vec::with_capacity(chunks.len() + 2);
    for &c in chunks {
        nat::mul_add_u32_in_place(&mut v, p, c);
    }
    v
}

fn chunks_to_mag(chunks: &[u32], p: u32, _first_pow: u32) -> Vec<u32> {
    // Leading chunk is just a smaller number; its weight is irrelevant (it is the most
    // significant), so every chunk is treated as a base-P digit.
    let n = chunks.len();
    if n <= FROM_STR_BASECASE {
        return chunks_basecase(chunks, p);
    }
    // pows[j] = P^(2^j)
    let mut pows: Vec<Vec<u32>> = vec![nat::from_u64(p as u64)];
    while (1usize << pows.len()) < n {
        let next = nat::sqr(pows.last().unwrap());
        pows.push(next);
    }
    chunks_rec(chunks, p, &pows)
}

fn chunks_rec(chunks: &[u32], p: u32, pows: &[Vec<u32>]) -> Vec<u32> {
    let n = chunks.len();
    if n <= FROM_STR_BASECASE {
        return chunks_basecase(chunks, p);
    }
    // low part: largest power of two < n chunks
    let j = (usize::BITS - 1 - (n - 1).leading_zeros()) as usize; // 2^j < n <= 2^(j+1)
    let low_len = 1usize << j;
    let (hi, lo) = chunks.split_at(n - low_len);
    let h = chunks_rec(hi, p, pows);
    let l = chunks_rec(lo, p, pows);
    let mut r = nat::mul(&h, &pows[j]);
    nat::add_in_place(&mut r, &l);
    r
}

#[inline]
fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// Python `int(s, base)` for an ASCII byte string. The runtime should first map non-ASCII
/// Unicode decimal digits to ASCII and Unicode whitespace to b' ' (as CPython's
/// `_PyUnicode_TransformDecimalAndSpaceToASCII` does); any remaining non-ASCII byte is invalid.
///
/// Handles surrounding whitespace, a sign, `0x`/`0o`/`0b` prefixes (with base 0 or the matching
/// base), single underscores between digits (and after a prefix), and base 0's leading-zero
/// rule. `max_str_digits` is CPython's `sys.int_max_str_digits` (0 = unlimited); it applies only
/// to non-power-of-two bases and counts digits (not sign/underscores/whitespace).
///
/// Errors: `InvalidBase`, `InvalidLiteral { base }` (base as given), `StrToIntDigitLimit`.
pub fn parse_int(s: &[u8], base: u32, max_str_digits: usize) -> Result<BigInt, IntError> {
    if base != 0 && !(2..=36).contains(&base) {
        return Err(IntError::InvalidBase);
    }
    let invalid = Err(IntError::InvalidLiteral { base });
    let mut start = 0;
    let mut end = s.len();
    while start < end && is_space(s[start]) {
        start += 1;
    }
    while end > start && is_space(s[end - 1]) {
        end -= 1;
    }
    let mut t = &s[start..end];
    let mut neg = false;
    if let Some(&c) = t.first() {
        if c == b'+' || c == b'-' {
            neg = c == b'-';
            t = &t[1..];
        }
    }
    let mut radix = base;
    let mut had_prefix = false;
    if t.len() >= 2 && t[0] == b'0' {
        let p = t[1].to_ascii_lowercase();
        let pb = match p {
            b'x' => 16,
            b'o' => 8,
            b'b' => 2,
            _ => 0,
        };
        if pb != 0 && (base == 0 || base == pb) {
            radix = pb;
            had_prefix = true;
            t = &t[2..];
        }
    }
    let mut leading_zero_rule = false;
    if radix == 0 {
        radix = 10;
        leading_zero_rule = true;
    }
    // Validate digits and underscores.
    if t.is_empty() {
        return invalid;
    }
    let mut ndigits = 0usize;
    let mut prev_underscore = !had_prefix; // a leading underscore is allowed only after a prefix
    let mut has_underscore = false;
    for &c in t {
        if c == b'_' {
            if prev_underscore {
                return invalid;
            }
            prev_underscore = true;
            has_underscore = true;
        } else {
            if digit_value(c) as u32 >= radix {
                return invalid;
            }
            prev_underscore = false;
            ndigits += 1;
        }
    }
    if prev_underscore || ndigits == 0 {
        return invalid;
    }
    let digits: std::borrow::Cow<[u8]> = if has_underscore {
        std::borrow::Cow::Owned(t.iter().copied().filter(|&c| c != b'_').collect())
    } else {
        std::borrow::Cow::Borrowed(t)
    };
    if leading_zero_rule && digits[0] == b'0' && digits.iter().any(|&c| c != b'0') {
        return invalid;
    }
    if max_str_digits > 0 && !radix.is_power_of_two() && ndigits > max_str_digits {
        return Err(IntError::StrToIntDigitLimit { digits: ndigits, limit: max_str_digits });
    }
    let mag = from_valid_digits(&digits, radix);
    Ok(BigInt::from_parts(neg, mag))
}
