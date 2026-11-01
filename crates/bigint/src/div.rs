//! Division: single digit, Knuth algorithm D, and Newton-reciprocal (Barrett) division for large
//! operands.

use crate::nat::{self, BASE, MASK, SHIFT};
use core::cmp::Ordering;

/// Use the reciprocal-based division when both the divisor and the quotient have at least this
/// many digits.
pub const NEWTON_THRESHOLD: usize = 400000;
/// Use Burnikel-Ziegler when both the divisor and the quotient have at least this many digits.
pub const BZ_THRESHOLD: usize = 250;
/// Reciprocal recursion base size.
const RECIP_BASE: usize = 64;

/// Knuth algorithm D. `b.len() >= 2`, `a >= b`, both normalized.
/// Writes quotient into `q` (len >= a.len() - b.len() + 1) and returns the remainder.
pub fn knuth_divrem(q: &mut [u32], a: &[u32], b: &[u32]) -> Vec<u32> {
    let n = b.len();
    debug_assert!(n >= 2 && a.len() >= n);
    let s = b[n - 1].leading_zeros() - 2; // 32-bit word, 30-bit digits
    let mut v = vec![0u32; n];
    nat::shl_bits_to(&mut v, b, s);
    let mut u = vec![0u32; a.len() + 1];
    let c = nat::shl_bits_to(&mut u, a, s);
    u[a.len()] = c;
    knuth_core(q, &mut u, &v);
    // remainder = u[..n] >> s
    let mut r = vec![0u32; n];
    nat::shr_bits_to(&mut r, &u[..n], s);
    nat::normalize(&mut r);
    r
}

/// Core of algorithm D on a normalized divisor `v` (top digit >= 2^29) and dividend `u`
/// (with one extra leading digit, u[top] < v_top or so). Leaves the remainder in u[..n].
pub fn knuth_core(q: &mut [u32], u: &mut [u32], v: &[u32]) {
    let n = v.len();
    let m = u.len() - n; // number of quotient digits
    let vtop = v[n - 1] as u64;
    let vsec = v[n - 2] as u64;
    let use_kern = n >= nat::KERN_MIN;
    let mut tmp = if use_kern { vec![0u32; n] } else { Vec::new() };
    for j in (0..m).rev() {
        let utop = u[j + n] as u64;
        let unext = u[j + n - 1] as u64;
        let num = (utop << SHIFT) | unext;
        let mut qhat = num / vtop;
        let mut rhat = num - qhat * vtop;
        if qhat > MASK as u64 {
            qhat = MASK as u64;
            rhat = num - qhat * vtop;
        }
        while rhat < BASE as u64 && qhat * vsec > ((rhat << SHIFT) | u[j + n - 2] as u64) {
            qhat -= 1;
            rhat += vtop;
        }
        // multiply and subtract
        let borrow = if use_kern {
            let amt = crate::kern::submul1(&mut tmp, &u[j..j + n], v, qhat as u32);
            u[j..j + n].copy_from_slice(&tmp);
            amt as u32
        } else {
            nat::mul_digit_sub(&mut u[j..j + n], v, qhat as u32)
        };
        let top = u[j + n] as i64 - borrow as i64;
        if top < 0 {
            let c = nat::add_assign(&mut u[j..j + n], v);
            debug_assert_eq!(top + c as i64, 0);
            qhat -= 1;
        }
        u[j + n] = 0;
        if j < q.len() {
            q[j] = qhat as u32;
        }
    }
}

/// Quotient and remainder of normalized `a` by normalized nonzero `b`.
pub fn divrem(a: &[u32], b: &[u32]) -> (Vec<u32>, Vec<u32>) {
    assert!(!b.is_empty(), "division by zero");
    if nat::cmp(a, b) == Ordering::Less {
        return (Vec::new(), a.to_vec());
    }
    if b.len() == 1 {
        let (q, r) = nat::divrem_u32(a, b[0]);
        return (q, if r == 0 { Vec::new() } else { vec![r] });
    }
    if b.len() >= 3 && a.len() <= b.len() + 1 {
        if let Some(res) = divrem_small_quotient(a, b) {
            return res;
        }
    }
    let qlen = a.len() - b.len() + 1;
    if b.len() >= NEWTON_THRESHOLD && qlen >= NEWTON_THRESHOLD {
        return divrem_large(a, b);
    }
    if b.len() >= BZ_THRESHOLD && qlen >= BZ_THRESHOLD {
        return divrem_bz(a, b);
    }
    if qlen + 8 < b.len() / 2 && qlen >= 16 && b.len() >= NEWTON_THRESHOLD {
        return divrem_short_quotient(a, b);
    }
    let mut q = vec![0u32; qlen];
    let r = knuth_divrem(&mut q, a, b);
    nat::normalize(&mut q);
    (q, r)
}

/// Division with a quotient below 2^30 (`a.len() <= b.len() + 1`, `b.len() >= 3`, `a >= b`):
/// estimate the quotient from the top bits, then one fused multiply-subtract pass and rare
/// corrections. Returns None if the quotient estimate does not fit one digit.
pub fn divrem_small_quotient(a: &[u32], b: &[u32]) -> Option<(Vec<u32>, Vec<u32>)> {
    let n = b.len();
    let lb = nat::bit_length(b);
    let shift = lb.saturating_sub(64);
    let bt = nat::extract_bits(b, shift, 64) as u128;
    let at = (nat::extract_bits(a, shift + 64, 64) as u128) << 64 | nat::extract_bits(a, shift, 64) as u128;
    let qe = at / bt;
    if qe > (MASK as u128) - 4 {
        return None;
    }
    let mut q = qe as u32;
    let mut r = vec![0u32; n];
    let amount = crate::kern::submul1(&mut r, &a[..n], b, q);
    let mut top: i64 = if a.len() > n { a[n] as i64 } else { 0 } - amount as i64;
    while top < 0 {
        top += nat::add_assign(&mut r, b) as i64;
        q -= 1;
    }
    while top > 0 || nat::cmp_same_len(&r, b) != Ordering::Less {
        top -= nat::sub_assign(&mut r, b) as i64;
        q += 1;
    }
    nat::normalize(&mut r);
    let qv = if q == 0 { Vec::new() } else { vec![q] };
    Some((qv, r))
}

/// Division where the quotient is much shorter than the divisor: estimate the quotient from
/// truncated operands, then correct.
fn divrem_short_quotient(a: &[u32], b: &[u32]) -> (Vec<u32>, Vec<u32>) {
    let qlen = a.len() - b.len() + 1;
    // Keep qlen+2 digits of the divisor.
    let keep = qlen + 2;
    let s = b.len() - keep;
    let at = &a[s..];
    let bt = &b[s..];
    let (mut q, _) = divrem(at, bt);
    // q >= true quotient, and q - true <= 2 (bt >= B^(qlen+1) > q).
    let prod = nat::mul(&q, b);
    let mut r_neg;
    let mut r;
    if nat::cmp(&prod, a) == Ordering::Greater {
        r = nat::sub(&prod, a);
        r_neg = true;
    } else {
        r = nat::sub(a, &prod);
        r_neg = false;
    }
    while r_neg {
        // r = -(r) + b
        nat::sub_digit_assign(&mut q, 1);
        nat::normalize(&mut q);
        if nat::cmp(&r, b) == Ordering::Greater {
            nat::sub_in_place(&mut r, b);
        } else {
            nat::rsub_in_place(&mut r, b);
            r_neg = false;
        }
    }
    while nat::cmp(&r, b) != Ordering::Less {
        nat::sub_in_place(&mut r, b);
        nat::add_in_place(&mut q, &[1]);
    }
    (q, r)
}

/// A divisor with a precomputed reciprocal for repeated Barrett division.
#[derive(Clone, Debug)]
pub struct Divisor {
    /// Normalization shift (bits) applied to the original divisor.
    pub shift: u32,
    /// The original divisor (normalized vector).
    pub orig: Vec<u32>,
    /// Shifted divisor, top digit >= 2^29.
    pub d: Vec<u32>,
    /// Approximately floor(B^(2n) / d) (never larger, within a few units), n = d.len().
    pub mu: Vec<u32>,
}

impl Divisor {
    /// Precompute a reciprocal for divisor `b` (normalized, len >= 2).
    pub fn new(b: &[u32]) -> Divisor {
        let n = b.len();
        assert!(n >= 1);
        let s = b[n - 1].leading_zeros() - 2;
        let mut d = vec![0u32; n];
        nat::shl_bits_to(&mut d, b, s);
        let mu = recip_approx(&d);
        Divisor { shift: s, orig: b.to_vec(), d, mu }
    }

    /// Quotient and remainder of `a` by this divisor.
    pub fn divrem(&self, a: &[u32]) -> (Vec<u32>, Vec<u32>) {
        if nat::cmp(a, &self.orig) == Ordering::Less {
            return (Vec::new(), a.to_vec());
        }
        let n = self.d.len();
        let mut ash = vec![0u32; a.len() + 1];
        let c = nat::shl_bits_to(&mut ash, a, self.shift);
        ash[a.len()] = c;
        nat::normalize(&mut ash);
        let len = ash.len();
        let nchunks = len.div_ceil(n);
        let mut q = vec![0u32; nchunks * n];
        let mut r: Vec<u32> = Vec::new();
        for ci in (0..nchunks).rev() {
            let lo = ci * n;
            let hi = ((ci + 1) * n).min(len);
            // x = r * B^n + chunk
            let mut x = vec![0u32; n + r.len()];
            x[..hi - lo].copy_from_slice(&ash[lo..hi]);
            x[n..].copy_from_slice(&r);
            nat::normalize(&mut x);
            let (qi, ri) = self.barrett(&x);
            q[lo..lo + qi.len()].copy_from_slice(&qi);
            r = ri;
        }
        nat::normalize(&mut q);
        // unshift remainder
        let r = nat::shr(&r, self.shift as u64);
        (q, r)
    }

    /// Barrett step: x < d * B^n (normalized x). Returns (q, r) with q < B^n.
    fn barrett(&self, x: &[u32]) -> (Vec<u32>, Vec<u32>) {
        let n = self.d.len();
        if nat::cmp(x, &self.d) == Ordering::Less {
            return (Vec::new(), x.to_vec());
        }
        let q1 = if x.len() > n - 1 { &x[n - 1..] } else { &[][..] };
        let q2 = nat::mul(q1, &self.mu);
        let mut q3 = if q2.len() > n + 1 { q2[n + 1..].to_vec() } else { Vec::new() };
        let prod = nat::mul(&q3, &self.d);
        // r = x - prod (prod <= x)
        debug_assert!(nat::cmp(&prod, x) != Ordering::Greater);
        let mut r = nat::sub(x, &prod);
        let mut guard = 0;
        while nat::cmp(&r, &self.d) != Ordering::Less {
            nat::sub_in_place(&mut r, &self.d);
            nat::add_in_place(&mut q3, &[1]);
            guard += 1;
            debug_assert!(guard <= 64);
        }
        (q3, r)
    }
}

/// floor(B^(2n) / d) for a normalized `d` (top digit >= 2^29), n = d.len() (exact).
pub fn recip(d: &[u32]) -> Vec<u32> {
    let n = d.len();
    let mut big = vec![0u32; 2 * n + 1];
    big[2 * n] = 1;
    let mut x1 = recip_approx(d);
    let t1 = nat::mul(d, &x1);
    match nat::cmp(&t1, &big) {
        Ordering::Equal => x1,
        Ordering::Less => {
            let diff = nat::sub(&big, &t1);
            let (q, _) = divrem(&diff, d);
            nat::add_in_place(&mut x1, &q);
            x1
        }
        Ordering::Greater => {
            let diff = nat::sub(&t1, &big);
            let (mut q, r) = divrem(&diff, d);
            if !r.is_empty() {
                nat::add_in_place(&mut q, &[1]);
            }
            nat::sub_in_place(&mut x1, &q);
            x1
        }
    }
}

/// Approximate reciprocal: a value <= floor(B^(2n) / d) and within a few units of it, by
/// Newton iteration from below (each step's truncations only decrease the value).
pub fn recip_approx(d: &[u32]) -> Vec<u32> {
    let n = d.len();
    let mut big = vec![0u32; 2 * n + 1];
    big[2 * n] = 1;
    if n <= RECIP_BASE {
        if n == 1 {
            let (q, _) = nat::divrem_u32(&big, d[0]);
            return q;
        }
        let mut q = vec![0u32; n + 2];
        knuth_divrem(&mut q, &big, d);
        nat::normalize(&mut q);
        return q;
    }
    let h = n / 2 + 1;
    let k = n - h;
    let dh = &d[k..];
    let mh = recip_approx(dh);
    // t = d * x0 where x0 = mh * B^k
    let dm = nat::mul(d, &mh);
    let mut t = vec![0u32; k];
    t.extend_from_slice(&dm);
    nat::normalize(&mut t);
    // e = B^(2n) - t (signed)
    let (e, e_neg) = nat::abs_diff(&big, &t);
    let e_neg = e_neg; // true when t > B^(2n)
    // corr = mh * e / B^(2n-k); the low n-2 digits of e contribute < 1 unit, drop them.
    let s = n - 2;
    let e_hi: &[u32] = if e.len() > s { &e[s..] } else { &[][..] };
    let me = nat::mul(&mh, e_hi);
    let shift_digits = 2 * n - k - s;
    let corr: Vec<u32> = if me.len() > shift_digits { me[shift_digits..].to_vec() } else { Vec::new() };
    let mut x1 = vec![0u32; k];
    x1.extend_from_slice(&mh);
    nat::normalize(&mut x1);
    // Newton from either side lands at or below the true value (exact arithmetic); truncation
    // of the correction can only raise x1 when it is subtracted, so subtract 2 extra units.
    if e_neg {
        let mut c = corr;
        nat::add_in_place(&mut c, &[2]);
        if nat::cmp(&x1, &c) != Ordering::Greater {
            return divrem_knuth(&big, d).0;
        }
        nat::sub_in_place(&mut x1, &c);
        return x1;
    }
    nat::add_in_place(&mut x1, &corr);
    x1
}

/// Large division via a one-off reciprocal.
pub fn divrem_large(a: &[u32], b: &[u32]) -> (Vec<u32>, Vec<u32>) {
    Divisor::new(b).divrem(a)
}

/// Knuth-only division (for testing).
pub fn divrem_knuth(a: &[u32], b: &[u32]) -> (Vec<u32>, Vec<u32>) {
    if nat::cmp(a, b) == Ordering::Less {
        return (Vec::new(), a.to_vec());
    }
    if b.len() == 1 {
        let (q, r) = nat::divrem_u32(a, b[0]);
        return (q, if r == 0 { Vec::new() } else { vec![r] });
    }
    let mut q = vec![0u32; a.len() - b.len() + 1];
    let r = knuth_divrem(&mut q, a, b);
    nat::normalize(&mut q);
    (q, r)
}

// ---------------------------------------------------------------------------------------------
// Burnikel-Ziegler recursive division
// ---------------------------------------------------------------------------------------------

/// Recursion base size (digits) for Burnikel-Ziegler.
const BZ_LIMIT: usize = 80;

/// Number formed by `lo` (zero-padded to `lo_len` digits) followed by `hi`.
fn concat(lo: &[u32], lo_len: usize, hi: &[u32]) -> Vec<u32> {
    let hi = nat::trim(hi);
    if hi.is_empty() {
        return nat::trim(lo).to_vec();
    }
    let mut v = Vec::with_capacity(lo_len + hi.len());
    v.extend_from_slice(&lo[..lo.len().min(lo_len)]);
    v.resize(lo_len, 0);
    v.extend_from_slice(hi);
    v
}

/// Divide `a < b * B^n` by `b` with exactly `n` digits and top digit >= 2^29.
fn div2n1n(a: &[u32], b: &[u32], n: usize) -> (Vec<u32>, Vec<u32>) {
    let a = nat::trim(a);
    if nat::cmp(a, b) == Ordering::Less {
        return (Vec::new(), a.to_vec());
    }
    if n <= BZ_LIMIT || a.len() <= n + 2 {
        return divrem_knuth(a, b);
    }
    if n % 2 == 1 {
        // pad by one digit
        let a2 = concat(&[], 1, a);
        let b2 = concat(&[], 1, b);
        let (q, r) = div2n1n(&a2, &b2, n + 1);
        let r = if r.is_empty() { r } else { r[1..].to_vec() };
        return (q, r);
    }
    let h = n / 2;
    let (b2, b1) = b.split_at(h);
    let a_hi = if a.len() > h { &a[h..] } else { &[][..] };
    let (q1, r) = div3n2n(a_hi, b, b1, b2, h);
    let x = concat(&a[..h.min(a.len())], h, &r);
    let (q2, s) = div3n2n(&x, b, b1, b2, h);
    (concat(&q2, h, &q1), s)
}

/// Divide `a` (< b * B^h, up to 3h digits) by `b = [b1 b2]` (2h digits).
fn div3n2n(a: &[u32], b: &[u32], b1: &[u32], b2: &[u32], h: usize) -> (Vec<u32>, Vec<u32>) {
    let a = nat::trim(a);
    let a12: &[u32] = if a.len() > h { &a[h..] } else { &[][..] };
    let a1: &[u32] = if a.len() > 2 * h { &a[2 * h..] } else { &[][..] };
    let a3 = &a[..h.min(a.len())];
    let (mut q, r1) = if nat::cmp(a1, nat::trim(b1)) == Ordering::Less {
        div2n1n(a12, b1, h)
    } else {
        // q = B^h - 1, r1 = a12 - q*b1 = a12 - b1*B^h + b1
        let q = vec![MASK; h];
        let mut t = nat::add(a12, b1);
        let b1s = concat(&[], h, b1);
        nat::sub_in_place(&mut t, &b1s);
        (q, t)
    };
    let d = nat::mul(&q, nat::trim(b2));
    let rhat = concat(a3, h, &r1);
    if nat::cmp(&rhat, &d) != Ordering::Less {
        let r = nat::sub(&rhat, &d);
        return (q, r);
    }
    // negative: r = d - rhat (magnitude); add b until non-negative
    let mut neg = nat::sub(&d, &rhat);
    loop {
        nat::sub_digit_assign(&mut q, 1);
        nat::normalize(&mut q);
        if nat::cmp(&neg, b) != Ordering::Greater {
            let r = nat::sub(b, &neg);
            return (q, r);
        }
        nat::sub_in_place(&mut neg, b);
    }
}

/// Burnikel-Ziegler division of normalized `a` by normalized `b` (b.len() >= 2).
pub fn divrem_bz(a: &[u32], b: &[u32]) -> (Vec<u32>, Vec<u32>) {
    if nat::cmp(a, b) == Ordering::Less {
        return (Vec::new(), a.to_vec());
    }
    let m = b.len();
    // block size n = k * 2^j >= m with k <= BZ_LIMIT
    let mut j = 0;
    while (m >> j) > BZ_LIMIT {
        j += 1;
    }
    let k = m.div_ceil(1 << j);
    let n = k << j;
    let pad = n - m;
    let s = b[m - 1].leading_zeros() - 2;
    // normalized, padded copies
    let mut bn = vec![0u32; n];
    nat::shl_bits_to(&mut bn[pad..], b, s);
    let mut an = vec![0u32; pad + a.len() + 1];
    let c = nat::shl_bits_to(&mut an[pad..], a, s);
    an[pad + a.len()] = c;
    nat::normalize(&mut an);
    // blocks of n digits; ensure the top block is < bn
    let mut t = an.len().div_ceil(n);
    let top = &an[(t - 1) * n..];
    if nat::cmp_unnorm(top, &bn) != Ordering::Less {
        t += 1;
    }
    let mut r: Vec<u32> = if (t - 1) * n < an.len() { nat::trim(&an[(t - 1) * n..]).to_vec() } else { Vec::new() };
    let mut q = vec![0u32; t * n];
    for i in (0..t - 1).rev() {
        let x = concat(&an[i * n..((i + 1) * n).min(an.len())], n, &r);
        let (qi, ri) = div2n1n(&x, &bn, n);
        q[i * n..i * n + qi.len()].copy_from_slice(&qi);
        r = ri;
    }
    nat::normalize(&mut q);
    // undo padding and shift
    let r = if r.len() > pad { nat::shr(&r[pad..], s as u64) } else { Vec::new() };
    (q, r)
}
