//! Binary -> decimal digit generation for f64.
//!
//! * shortest round-trip digits (repr) are taken from Rust's core float
//!   formatting (Grisu with Dragon fallback, always shortest + correctly
//!   rounded), written into a stack buffer;
//! * exact digit generation for a given number of significant digits or a
//!   given number of digits after the decimal point is done with a small
//!   big-integer routine, rounding half-to-even on the exact binary value.
//!
//! Results use the dtoa convention: value = 0.DIGITS * 10^decpt, digits have
//! no leading zeros and no trailing zeros.  Zero is represented as digits
//! "0" with decpt 1, except for values that *round* to zero in fixed mode,
//! which give empty digits and decpt = -ndigits (matching David Gay's dtoa).

use crate::bignum::{write_u64, Big};
#[cfg(test)]
use core::fmt::Write as _;

/// Maximum number of digits ever produced by the exact generator.
pub(crate) const MAX_DIGITS: usize = 1100;

pub(crate) struct Digits {
    pub buf: [u8; MAX_DIGITS],
    pub len: usize,
    pub decpt: i32,
}

impl Digits {
    #[inline]
    pub(crate) fn new() -> Digits {
        Digits { buf: [0; MAX_DIGITS], len: 0, decpt: 0 }
    }
    #[inline]
    pub(crate) fn digits(&self) -> &[u8] {
        &self.buf[..self.len]
    }
    fn strip_trailing_zeros(&mut self) {
        while self.len > 0 && self.buf[self.len - 1] == b'0' {
            self.len -= 1;
        }
    }
}

/// Small stack buffer implementing `fmt::Write` (test oracle only).
#[cfg(test)]
pub(crate) struct StackBuf<const N: usize> {
    pub buf: [u8; N],
    pub len: usize,
}

#[cfg(test)]
impl<const N: usize> StackBuf<N> {
    #[inline]
    pub(crate) fn new() -> Self {
        StackBuf { buf: [0; N], len: 0 }
    }
    #[inline]
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

#[cfg(test)]
impl<const N: usize> core::fmt::Write for StackBuf<N> {
    #[inline]
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let b = s.as_bytes();
        if self.len + b.len() > N {
            return Err(core::fmt::Error);
        }
        self.buf[self.len..self.len + b.len()].copy_from_slice(b);
        self.len += b.len();
        Ok(())
    }
}

/// Decompose a finite, nonzero, positive double into (m, e) with v = m * 2^e.
#[inline]
pub(crate) fn decompose(x: f64) -> (u64, i32) {
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1u64 << 52) - 1);
    if exp == 0 {
        (frac, -1074)
    } else {
        (frac | (1u64 << 52), exp - 1075)
    }
}

/// Shortest digits that round-trip (Python repr): value = 0.DIGITS * 10^decpt.
pub(crate) struct Short {
    pub buf: [u8; 24],
    pub len: usize,
    pub decpt: i32,
}

impl Short {
    #[inline]
    pub(crate) fn digits(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

include!(concat!(env!("OUT_DIR"), "/g_table.rs"));

const K_MIN: i32 = -324;
const C_MIN: u64 = 1 << 52;
const Q_MIN: i32 = -1074;

#[inline]
fn flog10pow2(q: i32) -> i32 {
    ((q as i64 * 661_971_961_083) >> 41) as i32
}
#[inline]
fn flog10_three_quarters_pow2(q: i32) -> i32 {
    ((q as i64 * 661_971_961_083 - 274_743_187_321) >> 41) as i32
}
#[inline]
fn flog2pow10(e: i32) -> i32 {
    ((e as i64 * 913_124_641_741) >> 38) as i32
}

/// Schubfach's r_o'(g cp 2^-127) with g = g1 2^63 + g0: an approximate
/// round-to-odd that deliberately ignores the lowest 64 bits of g0*cp
/// (this is what absorbs the error of the rounded-up table entry).
#[inline]
fn rop(g: u128, cp: u64) -> u64 {
    const MASK_63: u64 = (1u64 << 63) - 1;
    let g1 = (g >> 63) as u64;
    let g0 = (g as u64) & MASK_63;
    let x1 = ((g0 as u128 * cp as u128) >> 64) as u64;
    let y = g1 as u128 * cp as u128;
    let y0 = y as u64;
    let y1 = (y >> 64) as u64;
    let z = (y0 >> 1) + x1;
    let vbp = y1 + (z >> 63);
    vbp | (((z & MASK_63) + MASK_63) >> 63)
}

/// Schubfach (R. Giulietti): shortest decimal f * 10^e in the rounding
/// interval of v = c * 2^q, closest to v (ties to even).
#[inline]
fn schubfach_to_decimal(q: i32, c: u64) -> (u64, i32) {
    let out = c & 1;
    let cb = c << 2;
    let cbr = cb + 2;
    let (cbl, k) = if c != C_MIN || q == Q_MIN {
        (cb - 2, flog10pow2(q))
    } else {
        (cb - 1, flog10_three_quarters_pow2(q))
    };
    let h = q + flog2pow10(-k) + 2;
    let g = G_TABLE[(k - K_MIN) as usize];
    let vb = rop(g, cb << h);
    let vbl = rop(g, cbl << h);
    let vbr = rop(g, cbr << h);
    let s = vb >> 2;
    // The reference algorithm uses `s >= 100` (Java wants at least two
    // digits); Python wants the true shortest, so also try the coarser
    // grid for two-digit s (tiny subnormals).
    if s >= 10 {
        let sp10 = 10 * (s / 10);
        let tp10 = sp10 + 10;
        let upin = vbl + out <= sp10 << 2;
        let wpin = (tp10 << 2) + out <= vbr;
        if upin != wpin {
            return (if upin { sp10 } else { tp10 }, k);
        }
    }
    let t = s + 1;
    let uin = vbl + out <= s << 2;
    let win = (t << 2) + out <= vbr;
    if uin != win {
        return (if uin { s } else { t }, k);
    }
    let cmp = vb as i64 - ((s + t) << 1) as i64;
    (if cmp < 0 || (cmp == 0 && s & 1 == 0) { s } else { t }, k)
}

/// Shortest round-trip decimal for a finite, positive double: (f, e) with
/// value = f * 10^e (f may have trailing zeros).
#[inline]
fn shortest_fe(x: f64) -> (u64, i32) {
    let bits = x.to_bits();
    let t = bits & ((1u64 << 52) - 1);
    let bq = ((bits >> 52) & 0x7ff) as i32;
    if bq != 0 {
        let mq = 1075 - bq;
        let c = C_MIN | t;
        if 0 < mq && mq < 53 {
            let f = c >> mq;
            if f << mq == c {
                return (f, 0);
            }
        }
        schubfach_to_decimal(-mq, c)
    } else {
        schubfach_to_decimal(Q_MIN, t)
    }
}

/// Shortest digits that round-trip (Python repr).  `x` must be finite and
/// non-negative.
#[inline]
pub(crate) fn shortest(x: f64) -> Short {
    let mut out = Short { buf: [0; 24], len: 0, decpt: 1 };
    if x == 0.0 {
        out.buf[0] = b'0';
        out.len = 1;
        return out;
    }
    let (mut f, e) = shortest_fe(x);
    let mut e = e;
    while f % 10 == 0 {
        f /= 10;
        e += 1;
    }
    let n = write_u64(f, &mut out.buf);
    out.len = n;
    out.decpt = n as i32 + e;
    out
}

/// Reference implementation based on Rust's core formatting (test oracle).
#[cfg(test)]
pub(crate) fn shortest_core(x: f64, out: &mut Digits) {
    if x == 0.0 {
        out.buf[0] = b'0';
        out.len = 1;
        out.decpt = 1;
        return;
    }
    let mut sb: StackBuf<40> = StackBuf::new();
    let _ = write!(sb, "{:e}", x);
    let s = sb.as_bytes();
    let mut n = 0;
    let mut i = 0;
    while i < s.len() && s[i] != b'e' {
        if s[i] != b'.' {
            out.buf[n] = s[i];
            n += 1;
        }
        i += 1;
    }
    i += 1;
    let mut neg = false;
    if i < s.len() && s[i] == b'-' {
        neg = true;
        i += 1;
    }
    let mut e: i32 = 0;
    while i < s.len() {
        e = e * 10 + (s[i] - b'0') as i32;
        i += 1;
    }
    if neg {
        e = -e;
    }
    out.len = n;
    out.decpt = e + 1;
    out.strip_trailing_zeros();
}

/// floor(k * log10(2)) for |k| < 2^14.
#[inline]
fn floor_log10_pow2(k: i32) -> i32 {
    ((k as i64 * 78913) >> 18) as i32
}

/// Compute N = round_half_even(m * 2^e * 10^p) as a big integer.
fn round_scaled(m: u64, e: i32, p: i32) -> Big {
    // Work with T = floor(2 * m * 2^e * 10^p) and an exactness flag.
    let mut x = Big::from_u64(m);
    x.shl(1);
    let twos = e + p;
    let mut inexact = false;
    if p > 0 {
        x.mul_pow5(p as u32);
    }
    if twos > 0 {
        x.shl(twos as u32);
    } else if twos < 0 {
        inexact |= x.shr_sticky((-twos) as u32);
    }
    if p < 0 {
        inexact |= x.div_pow5_sticky((-p) as u32);
    }
    // now x = floor(2 * v * 10^p)
    let half = x.is_odd();
    x.shr_sticky(1);
    if half && (inexact || x.is_odd()) {
        x.add_small(1);
    }
    x
}

/// Fast path of `round_scaled` when everything fits in u128.
#[inline]
fn round_scaled_fast(m: u64, e: i32, p: i32) -> Option<u64> {
    // Handles 0 <= p <= 19 and -127 < e+p <= 0 style inputs.
    if !(0..=19).contains(&p) {
        return None;
    }
    let twos = e + p;
    if twos > 0 || twos < -120 {
        return None;
    }
    let pow10 = 10u128.pow(p as u32);
    // m < 2^53, 5^p <= 5^19 < 2^45: product < 2^98; times 2^p (p<=19) < 2^117
    let v = m as u128 * pow10; // = m * 5^p * 2^p
    let sh = (-e) as u32; // v * 2^e = v >> -e   (e + p <= 0 and p>=0 => -e >= p >= 0)
    if sh >= 128 {
        return None;
    }
    let q = if sh == 0 { v } else { v >> sh };
    let rem = if sh == 0 { 0 } else { v & ((1u128 << sh) - 1) };
    let mut q = q;
    if sh > 0 {
        let half = 1u128 << (sh - 1);
        if rem > half || (rem == half && (q & 1) == 1) {
            q += 1;
        }
    }
    u64::try_from(q).ok()
}

fn set_from_big(mut n: Big, out: &mut Digits) {
    out.len = n.write_decimal(&mut out.buf);
}

fn set_from_u64(n: u64, out: &mut Digits) {
    out.len = write_u64(n, &mut out.buf);
}

/// Exact digits with `ndigits` digits after the decimal point (dtoa mode 3).
/// `ndigits` may be negative (rounding to tens, hundreds, ...).  `x` must be
/// finite and non-negative.  Digits beyond the exact expansion are not
/// produced (callers pad with zeros).
pub(crate) fn fixed(x: f64, ndigits: i64, out: &mut Digits) {
    if x == 0.0 {
        out.buf[0] = b'0';
        out.len = 1;
        out.decpt = 1;
        return;
    }
    let (m, e) = decompose(x);
    // Beyond -e fractional digits the expansion is exact (all zeros).
    let exact_limit = if e < 0 { -e as i64 } else { 0 };
    let p = ndigits.min(exact_limit);
    // Very negative p: result is certainly zero (x < 2^1024 < 10^309).
    if p < -330 {
        out.len = 0;
        out.decpt = (-ndigits).clamp(i32::MIN as i64 / 2, i32::MAX as i64 / 2) as i32;
        return;
    }
    let p = p as i32;
    if let Some(n) = round_scaled_fast(m, e, p) {
        set_from_u64(n, out);
    } else {
        set_from_big(round_scaled(m, e, p), out);
    }
    if out.len == 0 {
        out.decpt = (-ndigits).clamp(i32::MIN as i64 / 2, i32::MAX as i64 / 2) as i32;
        return;
    }
    out.decpt = out.len as i32 - p;
    out.strip_trailing_zeros();
}

/// Exact digits with `n` (>= 1) significant digits (dtoa mode 2).  `x` must
/// be finite and non-negative.
pub(crate) fn significant(x: f64, n: usize, out: &mut Digits) {
    if x == 0.0 {
        out.buf[0] = b'0';
        out.len = 1;
        out.decpt = 1;
        return;
    }
    let n = n.clamp(1, 780) as i32; // a double has at most 767 significant digits
    let (m, e) = decompose(x);
    let bitlen = 64 - m.leading_zeros() as i32;
    let mut est = floor_log10_pow2(e + bitlen - 1); // E in {est, est+1}
    loop {
        let p = n - 1 - est;
        if let Some(v) = round_scaled_fast(m, e, p) {
            set_from_u64(v, out);
        } else {
            set_from_big(round_scaled(m, e, p), out);
        }
        if out.len as i32 > n {
            est += 1;
            continue;
        }
        out.decpt = est + 1;
        break;
    }
    out.strip_trailing_zeros();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(x: f64, n: usize) -> (String, i32) {
        let mut d = Digits::new();
        significant(x, n, &mut d);
        (String::from_utf8(d.digits().to_vec()).unwrap(), d.decpt)
    }
    fn fix(x: f64, n: i64) -> (String, i32) {
        let mut d = Digits::new();
        fixed(x, n, &mut d);
        (String::from_utf8(d.digits().to_vec()).unwrap(), d.decpt)
    }
    fn short(x: f64) -> (String, i32) {
        let d = shortest(x);
        (String::from_utf8(d.digits().to_vec()).unwrap(), d.decpt)
    }
    fn short_core(x: f64) -> (String, i32) {
        let mut d = Digits::new();
        shortest_core(x, &mut d);
        (String::from_utf8(d.digits().to_vec()).unwrap(), d.decpt)
    }

    /// Schubfach vs Rust's core shortest formatting.
    #[test]
    fn shortest_vs_core() {
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        let check = |x: f64| {
            if x.is_finite() && x > 0.0 {
                let (a, b) = (short(x), short_core(x));
                if a != b {
                    // Allowed only for an exact tie between two shortest
                    // candidates: Gay's dtoa (CPython) then picks the even
                    // last digit, Rust's core rounds up.
                    let (exact, _) = sig(x, 800);
                    assert_eq!(a.0.len(), b.0.len(), "x={:e}", x);
                    assert_eq!(a.1, b.1);
                    assert_eq!(exact.len(), a.0.len() + 1, "x={:e} not a tie", x);
                    assert!(exact.ends_with('5'), "x={:e} not a tie", x);
                    assert_eq!(&exact[..a.0.len()], &a.0[..], "x={:e}", x);
                    assert!((a.0.as_bytes()[a.0.len() - 1] - b'0') % 2 == 0, "x={:e}", x);
                }
            }
        };
        let n = if cfg!(debug_assertions) { 300_000 } else { 3_000_000 };
        for _ in 0..n {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            check(f64::from_bits(state & 0x7fff_ffff_ffff_ffff));
        }
        // powers of two (irregular spacing) and neighbours
        for e in 1..2047u64 {
            let b = e << 52;
            for d in [0u64, 1, 2, 3] {
                check(f64::from_bits(b + d));
                check(f64::from_bits(b - d));
            }
        }
        // subnormals
        for t in 1..50_000u64 {
            check(f64::from_bits(t));
            check(f64::from_bits((1u64 << 52) - t));
        }
        // integers and short decimals
        for i in 1..50_000u64 {
            check(i as f64);
            check(i as f64 / 1000.0);
            check(i as f64 * 1e10);
            check(1.0 / i as f64);
        }
        for p in -325..309 {
            check(10f64.powi(p));
            check(format!("1e{}", p).parse().unwrap());
            check(format!("9.999999999999999e{}", p).parse().unwrap());
            check(format!("5e{}", p).parse().unwrap());
        }
        check(f64::MAX);
        check(f64::MIN_POSITIVE);
    }

    #[test]
    fn digits_basic() {
        assert_eq!(short(1.0), ("1".into(), 1));
        assert_eq!(short(0.1), ("1".into(), 0));
        assert_eq!(short(1e16), ("1".into(), 17));
        assert_eq!(short(123.456), ("123456".into(), 3));
        assert_eq!(short(5e-324), ("5".into(), -323));
        assert_eq!(sig(0.125, 2), ("12".into(), 0));
        assert_eq!(sig(0.375, 2), ("38".into(), 0));
        assert_eq!(sig(9.5, 1), ("1".into(), 2));
        assert_eq!(sig(1e300, 17), ("10000000000000001".into(), 301));
        assert_eq!(sig(1e300, 16), ("1".into(), 301));
        assert_eq!(sig(1e300, 30), ("10000000000000000525047602552".into(), 301));
        assert_eq!(fix(0.125, 2), ("12".into(), 0));
        assert_eq!(fix(2.5, 0), ("2".into(), 1));
        assert_eq!(fix(0.001, 1), ("".into(), -1));
        assert_eq!(fix(1234.5678, 1), ("12346".into(), 4));
        assert_eq!(fix(25.0, -1), ("2".into(), 2));
        assert_eq!(fix(35.0, -1), ("4".into(), 2));
        assert_eq!(fix(1e49, 0), ("9999999999999999464902769475481793196872414789632".into(), 49));
    }

    /// Compare against Rust's core formatting (correctly rounded, half-even)
    /// on many pseudo-random doubles.
    #[test]
    fn exact_vs_core() {
        let mut state: u64 = 0x1234_5678_9abc_def1;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for i in 0..20000 {
            let bits = next() & 0x7fff_ffff_ffff_ffff;
            let x = f64::from_bits(bits);
            if !x.is_finite() {
                continue;
            }
            let n = (i % 40) + 1;
            let core = format!("{:.*e}", n - 1, x);
            let (d, dp) = sig(x, n);
            // rebuild from core
            let (mant, exp) = core.split_once('e').unwrap();
            let cd: String = mant.chars().filter(|c| *c != '.').collect();
            let cd = cd.trim_end_matches('0');
            let cexp: i32 = exp.parse().unwrap();
            if x != 0.0 {
                assert_eq!((d.as_str(), dp), (cd, cexp + 1), "x={:e} n={}", x, n);
            }
            if x < 1e30 {
                let p = (i % 30) as usize;
                let core = format!("{:.*}", p, x);
                let (d, dp) = fix(x, p as i64);
                // build string from digits
                let s = crate::float_fmt::tests_support::fixed_string(&d, dp, p);
                assert_eq!(s, core, "x={:e} p={}", x, p);
            }
        }
    }
}
