//! Division: single digit, Knuth algorithm D, and Newton-reciprocal (Barrett) division for large
//! operands.

use crate::nat::{self, BASE, MASK, SHIFT};
use core::cmp::Ordering;

/// Use the reciprocal-based division when both the divisor and the quotient have at least this
/// many digits.
pub const NEWTON_THRESHOLD: usize = 400;
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
        let borrow = nat::mul_digit_sub(&mut u[j..j + n], v, qhat as u32);
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
    /// floor(B^(2n) / d), n = d.len().
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
        let mu = recip(&d);
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
            debug_assert!(guard <= 3);
        }
        (q3, r)
    }
}

/// floor(B^(2n) / d) for a normalized `d` (top digit >= 2^29), n = d.len().
pub fn recip(d: &[u32]) -> Vec<u32> {
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
    let mh = recip(dh);
    // t = d * x0 where x0 = mh * B^k
    let dm = nat::mul(d, &mh);
    let mut t = vec![0u32; k];
    t.extend_from_slice(&dm);
    nat::normalize(&mut t);
    // e = B^(2n) - t (signed)
    let (e, e_neg) = nat::abs_diff(&big, &t);
    let e_neg = e_neg; // true when t > B^(2n)
    // corr = mh * e / B^(2n-k)
    let me = nat::mul(&mh, &e);
    let shift_digits = 2 * n - k;
    let corr: Vec<u32> = if me.len() > shift_digits { me[shift_digits..].to_vec() } else { Vec::new() };
    let mut x1 = vec![0u32; k];
    x1.extend_from_slice(&mh);
    nat::normalize(&mut x1);
    if e_neg {
        if nat::cmp(&x1, &corr) == Ordering::Greater {
            nat::sub_in_place(&mut x1, &corr);
        }
    } else {
        nat::add_in_place(&mut x1, &corr);
    }
    // exact fix-up
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
