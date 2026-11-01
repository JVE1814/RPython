//! Natural numbers in base 10^19 (little-endian `u64` limbs).
//!
//! Every function here works on trimmed slices (no most-significant zero limbs);
//! zero is the empty slice.  Results are returned trimmed.

use std::cmp::Ordering;

pub const BASE: u64 = 10_000_000_000_000_000_000;
pub const LDIG: u64 = 19;

pub const POW10: [u64; 20] = [
    1,
    10,
    100,
    1_000,
    10_000,
    100_000,
    1_000_000,
    10_000_000,
    100_000_000,
    1_000_000_000,
    10_000_000_000,
    100_000_000_000,
    1_000_000_000_000,
    10_000_000_000_000,
    100_000_000_000_000,
    1_000_000_000_000_000,
    10_000_000_000_000_000,
    100_000_000_000_000_000,
    1_000_000_000_000_000_000,
    10_000_000_000_000_000_000,
];

/// Classification of discarded digits relative to one unit of the last kept digit.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum Rest {
    Zero,
    Lt,
    Half,
    Gt,
}

impl Rest {
    /// Combine the classification of a digit string (first discarded digit `d`,
    /// `nz_below` whether anything below it is nonzero).
    #[inline]
    pub fn from_digit(d: u64, nz_below: bool) -> Rest {
        if d == 0 {
            if nz_below { Rest::Lt } else { Rest::Zero }
        } else if d < 5 {
            Rest::Lt
        } else if d == 5 {
            if nz_below { Rest::Gt } else { Rest::Half }
        } else {
            Rest::Gt
        }
    }
    #[inline]
    pub fn nonzero(self) -> bool {
        self != Rest::Zero
    }
}

// ---------------------------------------------------------------------------
// Division of a two-limb value (hi*2^64 + lo, hi < 10^19) by 10^19.
// Möller–Granlund 2-by-1 division with a precomputed reciprocal; 10^19 already
// has its top bit set, so no normalisation shift is needed.
const BASE_INV: u64 = (u128::MAX / (BASE as u128) - (1u128 << 64)) as u64;

#[inline(always)]
pub fn div_base(t: u128) -> (u64, u64) {
    let u1 = (t >> 64) as u64;
    let u0 = t as u64;
    debug_assert!(u1 < BASE);
    let q = (BASE_INV as u128) * (u1 as u128) + t;
    let mut q1 = ((q >> 64) as u64).wrapping_add(1);
    let q0 = q as u64;
    let mut r = u0.wrapping_sub(q1.wrapping_mul(BASE));
    if r > q0 {
        q1 = q1.wrapping_sub(1);
        r = r.wrapping_add(BASE);
    }
    if r >= BASE {
        q1 += 1;
        r -= BASE;
    }
    (q1, r)
}

/// Generic invariant divisor for u128 / u64 where the quotient fits in u64.
#[derive(Clone, Copy)]
pub struct Div64 {
    d: u64, // normalised divisor
    v: u64,
    s: u32,
}

impl Div64 {
    pub fn new(d: u64) -> Div64 {
        let s = d.leading_zeros();
        let dn = d << s;
        let v = (u128::MAX / (dn as u128) - (1u128 << 64)) as u64;
        Div64 { d: dn, v, s }
    }
    /// (q, r) of (hi*2^64+lo) / d; requires hi < d (original divisor).
    #[inline(always)]
    pub fn divrem(&self, hi: u64, lo: u64) -> (u64, u64) {
        let (u1, u0) = if self.s == 0 {
            (hi, lo)
        } else {
            ((hi << self.s) | (lo >> (64 - self.s)), lo << self.s)
        };
        let q = (self.v as u128) * (u1 as u128) + (((u1 as u128) << 64) | u0 as u128);
        let mut q1 = ((q >> 64) as u64).wrapping_add(1);
        let q0 = q as u64;
        let mut r = u0.wrapping_sub(q1.wrapping_mul(self.d));
        if r > q0 {
            q1 = q1.wrapping_sub(1);
            r = r.wrapping_add(self.d);
        }
        if r >= self.d {
            q1 += 1;
            r -= self.d;
        }
        (q1, r >> self.s)
    }
}

// ---------------------------------------------------------------------------

#[inline]
pub fn trim(v: &mut Vec<u64>) {
    while let Some(&0) = v.last() {
        v.pop();
    }
}

#[inline]
pub fn u64_digits(x: u64) -> u64 {
    if x == 0 {
        return 1;
    }
    // floor(log10) estimate from bit length
    let bits = 64 - x.leading_zeros() as u64;
    let est = (bits * 1233) >> 12; // ~ bits*log10(2)
    if x >= POW10[est as usize] { est + 1 } else { est.max(1) }
}

#[inline]
pub fn u128_digits(x: u128) -> u64 {
    if x < BASE as u128 {
        u64_digits(x as u64)
    } else {
        let (hi, lo) = div_base(x);
        let _ = lo;
        u64_digits(hi) + LDIG
    }
}

/// Number of decimal digits (0 for zero).
#[inline]
pub fn digits(a: &[u64]) -> u64 {
    match a.len() {
        0 => 0,
        n => (n as u64 - 1) * LDIG + u64_digits(a[n - 1]),
    }
}

#[inline]
pub fn is_zero(a: &[u64]) -> bool {
    a.is_empty()
}

pub fn cmp(a: &[u64], b: &[u64]) -> Ordering {
    if a.len() != b.len() {
        return a.len().cmp(&b.len());
    }
    for i in (0..a.len()).rev() {
        if a[i] != b[i] {
            return a[i].cmp(&b[i]);
        }
    }
    Ordering::Equal
}

pub fn from_u64(x: u64) -> Vec<u64> {
    if x == 0 {
        vec![]
    } else if x < BASE {
        vec![x]
    } else {
        vec![x - BASE, 1]
    }
}

pub fn from_u128(x: u128) -> Vec<u64> {
    let mut v = Vec::new();
    let mut x = x;
    while x != 0 {
        if (x >> 64) < BASE as u128 {
            let (q, r) = div_base(x);
            v.push(r);
            x = q as u128;
        } else {
            v.push((x % BASE as u128) as u64);
            x /= BASE as u128;
        }
    }
    v
}

pub fn to_u128(a: &[u64]) -> Option<u128> {
    match a.len() {
        0 => Some(0),
        1 => Some(a[0] as u128),
        2 => Some(a[0] as u128 + a[1] as u128 * BASE as u128),
        _ => None,
    }
}

pub fn to_u64(a: &[u64]) -> Option<u64> {
    match a.len() {
        0 => Some(0),
        1 => Some(a[0]),
        2 if a[1] == 1 && a[0] <= u64::MAX - BASE => Some(a[0] + BASE),
        _ => None,
    }
}

/// a + b + c with a, b < BASE, c <= 1: (sum mod BASE, carry).
#[inline(always)]
pub fn adc(a: u64, b: u64, c: u64) -> (u64, u64) {
    let t = a + c;
    if t >= BASE - b { (t - (BASE - b), 1) } else { (t + b, 0) }
}

pub fn add(a: &[u64], b: &[u64]) -> Vec<u64> {
    let (a, b) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let mut r = Vec::with_capacity(a.len() + 1);
    let mut carry = 0u64;
    for i in 0..b.len() {
        let (s, c) = adc(a[i], b[i], carry);
        carry = c;
        r.push(s);
    }
    for i in b.len()..a.len() {
        let mut s = a[i] + carry;
        if s >= BASE {
            s -= BASE;
            carry = 1;
        } else {
            carry = 0;
        }
        r.push(s);
    }
    if carry != 0 {
        r.push(carry);
    }
    r
}

/// a += b (in place, a grows as needed), b shifted by `off` limbs.
pub fn add_into(a: &mut Vec<u64>, b: &[u64], off: usize) {
    if a.len() < b.len() + off {
        a.resize(b.len() + off, 0);
    }
    let mut carry = 0u64;
    let mut i = 0;
    while i < b.len() {
        let (s, c) = adc(a[i + off], b[i], carry);
        carry = c;
        a[i + off] = s;
        i += 1;
    }
    let mut j = off + b.len();
    while carry != 0 {
        if j == a.len() {
            a.push(carry);
            break;
        }
        let mut s = a[j] + carry;
        if s >= BASE {
            s -= BASE;
            carry = 1;
        } else {
            carry = 0;
        }
        a[j] = s;
        j += 1;
    }
}

/// a - b; requires a >= b.
pub fn sub(a: &[u64], b: &[u64]) -> Vec<u64> {
    let mut r = Vec::with_capacity(a.len());
    let mut borrow = 0u64;
    for i in 0..a.len() {
        let bi = if i < b.len() { b[i] } else { 0 } + borrow;
        if a[i] >= bi {
            r.push(a[i] - bi);
            borrow = 0;
        } else {
            r.push(BASE - (bi - a[i]));
            borrow = 1;
        }
    }
    debug_assert!(borrow == 0);
    trim(&mut r);
    r
}

/// a -= b in place (a >= b), b shifted by `off` limbs; result trimmed.
pub fn sub_into(a: &mut Vec<u64>, b: &[u64], off: usize) {
    let mut borrow = 0u64;
    let mut i = 0;
    while i < b.len() || borrow != 0 {
        let bi = if i < b.len() { b[i] } else { 0 } + borrow;
        let x = a[i + off];
        if x >= bi {
            a[i + off] = x - bi;
            borrow = 0;
        } else {
            a[i + off] = BASE - (bi - x);
            borrow = 1;
        }
        i += 1;
    }
    trim(a);
}

pub fn add_small(a: &[u64], m: u64) -> Vec<u64> {
    let mut r = a.to_vec();
    let mut carry = m;
    let mut i = 0;
    while carry != 0 {
        if i == r.len() {
            r.push(carry);
            break;
        }
        let s = r[i] as u128 + carry as u128;
        if s >= BASE as u128 {
            r[i] = (s - BASE as u128) as u64;
            carry = 1;
        } else {
            r[i] = s as u64;
            carry = 0;
        }
        i += 1;
    }
    r
}

pub fn incr(a: &mut Vec<u64>) {
    for x in a.iter_mut() {
        if *x == BASE - 1 {
            *x = 0;
        } else {
            *x += 1;
            return;
        }
    }
    a.push(1);
}

pub fn decr(a: &mut Vec<u64>) {
    for x in a.iter_mut() {
        if *x == 0 {
            *x = BASE - 1;
        } else {
            *x -= 1;
            break;
        }
    }
    trim(a);
}

pub fn mul_small(a: &[u64], m: u64) -> Vec<u64> {
    if m == 0 || a.is_empty() {
        return vec![];
    }
    let mut r = Vec::with_capacity(a.len() + 1);
    let mut carry = 0u64;
    for &x in a {
        let t = x as u128 * m as u128 + carry as u128;
        let (q, rr) = div_base(t);
        r.push(rr);
        carry = q;
    }
    if carry != 0 {
        r.push(carry);
    }
    r
}

/// (a / d, a % d) for 0 < d < 2^64.
pub fn divrem_small(a: &[u64], d: u64) -> (Vec<u64>, u64) {
    let dv = Div64::new(d);
    let mut q = vec![0u64; a.len()];
    let mut rem = 0u64;
    for i in (0..a.len()).rev() {
        // value = rem*BASE + a[i]; rem < d so quotient < BASE fits
        let t = rem as u128 * BASE as u128 + a[i] as u128;
        let (qq, rr) = dv.divrem((t >> 64) as u64, t as u64);
        q[i] = qq;
        rem = rr;
    }
    trim(&mut q);
    (q, rem)
}

pub fn rem_small(a: &[u64], d: u64) -> u64 {
    let dv = Div64::new(d);
    let mut rem = 0u64;
    for i in (0..a.len()).rev() {
        let t = rem as u128 * BASE as u128 + a[i] as u128;
        rem = dv.divrem((t >> 64) as u64, t as u64).1;
    }
    rem
}

// --------------------------- multiplication ----------------------------------

const KARATSUBA_CUTOFF: usize = 40;

fn mul_school_into(r: &mut [u64], a: &[u64], b: &[u64]) {
    // r must be zeroed, len >= a.len()+b.len()
    for (j, &bj) in b.iter().enumerate() {
        if bj == 0 {
            continue;
        }
        let mut carry = 0u64;
        for (i, &ai) in a.iter().enumerate() {
            let t = ai as u128 * bj as u128 + r[i + j] as u128 + carry as u128;
            let (q, rr) = div_base(t);
            r[i + j] = rr;
            carry = q;
        }
        let mut k = j + a.len();
        while carry != 0 {
            let s = r[k] as u128 + carry as u128; let s = if s >= BASE as u128 { r[k] = (s - BASE as u128) as u64; carry = 1; k += 1; continue } else { s as u64 };
            if s >= BASE {
                r[k] = s - BASE;
                carry = 1;
            } else {
                r[k] = s;
                carry = 0;
            }
            k += 1;
        }
    }
}

fn trimmed(a: &[u64]) -> &[u64] {
    let mut n = a.len();
    while n > 0 && a[n - 1] == 0 {
        n -= 1;
    }
    &a[..n]
}

fn karatsuba(a: &[u64], b: &[u64]) -> Vec<u64> {
    let a = trimmed(a);
    let b = trimmed(b);
    if a.is_empty() || b.is_empty() {
        return vec![];
    }
    let (a, b) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    if b.len() < KARATSUBA_CUTOFF {
        let mut r = vec![0u64; a.len() + b.len()];
        mul_school_into(&mut r, a, b);
        trim(&mut r);
        return r;
    }
    if a.len() >= 2 * b.len() {
        // unbalanced: chop a into pieces of b's size
        let mut r = vec![0u64; a.len() + b.len() + 1];
        let mut off = 0;
        while off < a.len() {
            let end = (off + b.len()).min(a.len());
            let p = karatsuba(&a[off..end], b);
            let mut carry = 0u64;
            let mut i = 0;
            while i < p.len() || carry != 0 {
                let s128 = r[off + i] as u128 + if i < p.len() { p[i] as u128 } else { 0 } + carry as u128; if s128 >= BASE as u128 { r[off + i] = (s128 - BASE as u128) as u64; carry = 1; i += 1; continue; } let s = s128 as u64;
                if s >= BASE {
                    r[off + i] = s - BASE;
                    carry = 1;
                } else {
                    r[off + i] = s;
                    carry = 0;
                }
                i += 1;
            }
            off = end;
        }
        trim(&mut r);
        return r;
    }
    let m = a.len() / 2;
    let (a0, a1) = (trimmed(&a[..m]), &a[m..]);
    let (b0, b1) = if b.len() > m { (trimmed(&b[..m]), &b[m..]) } else { (b, &b[b.len()..]) };
    let z0 = karatsuba(a0, b0);
    let z2 = karatsuba(a1, b1);
    let sa = add(a0, a1);
    let sb = add(b0, b1);
    let mut z1 = karatsuba(&sa, &sb);
    sub_into(&mut z1, &z0, 0);
    sub_into(&mut z1, &z2, 0);
    let mut r = Vec::with_capacity(a.len() + b.len() + 1);
    r.extend_from_slice(&z0);
    add_into(&mut r, &z1, m);
    add_into(&mut r, &z2, 2 * m);
    trim(&mut r);
    r
}

pub fn mul(a: &[u64], b: &[u64]) -> Vec<u64> {
    if a.is_empty() || b.is_empty() {
        return vec![];
    }
    if a.len() == 1 {
        return mul_small(b, a[0]);
    }
    if b.len() == 1 {
        return mul_small(a, b[0]);
    }
    if a.len().min(b.len()) < KARATSUBA_CUTOFF {
        let mut r = vec![0u64; a.len() + b.len()];
        mul_school_into(&mut r, a, b);
        trim(&mut r);
        return r;
    }
    if a.len().min(b.len()) >= super::ntt::NTT_CUTOFF {
        return super::ntt::mul(a, b);
    }
    karatsuba(a, b)
}

pub fn sqr(a: &[u64]) -> Vec<u64> {
    mul(a, a)
}

// ------------------------------ division -------------------------------------

/// (a / b, a % b); b nonzero.
pub fn divrem(a: &[u64], b: &[u64]) -> (Vec<u64>, Vec<u64>) {
    assert!(!b.is_empty(), "division by zero");
    if cmp(a, b) == Ordering::Less {
        return (vec![], a.to_vec());
    }
    if b.len() == 1 {
        let (q, r) = divrem_small(a, b[0]);
        return (q, if r == 0 { vec![] } else { vec![r] });
    }
    if b.len() >= super::ntt::NEWTON_DIV_CUTOFF && a.len() - b.len() >= super::ntt::NEWTON_DIV_CUTOFF {
        return super::ntt::divrem_newton(a, b);
    }
    divrem_knuth(a, b)
}

/// Schoolbook long division (Knuth D) in base 10^19.
pub fn divrem_knuth(a: &[u64], b: &[u64]) -> (Vec<u64>, Vec<u64>) {
    let n = b.len();
    let m = a.len() - n;
    // normalise so that top limb of b >= BASE/2
    let f = BASE / (b[n - 1] + 1);
    let (mut u, v) = if f > 1 { (mul_small(a, f), mul_small(b, f)) } else { (a.to_vec(), b.to_vec()) };
    u.resize(a.len() + 1, 0);
    debug_assert!(v.len() == n);
    let vtop = v[n - 1] as u128;
    let vsec = v[n - 2] as u128;
    let mut q = vec![0u64; m + 1];
    for j in (0..=m).rev() {
        let num = u[j + n] as u128 * BASE as u128 + u[j + n - 1] as u128;
        let mut qhat = num / vtop;
        let mut rhat = num - qhat * vtop;
        while qhat >= BASE as u128 || qhat * vsec > rhat * BASE as u128 + u[j + n - 2] as u128 {
            qhat -= 1;
            rhat += vtop;
            if rhat >= BASE as u128 {
                break;
            }
        }
        // multiply and subtract
        let qh = qhat as u64;
        let mut borrow = 0u64;
        let mut carry = 0u64;
        for i in 0..n {
            let p = qh as u128 * v[i] as u128 + carry as u128;
            let (ph, pl) = div_base(p);
            carry = ph;
            let sub = pl + borrow;
            if u[i + j] >= sub {
                u[i + j] -= sub;
                borrow = 0;
            } else {
                u[i + j] = BASE - (sub - u[i + j]);
                borrow = 1;
            }
        }
        let sub = carry + borrow;
        let negative = u[j + n] < sub;
        u[j + n] = u[j + n].wrapping_sub(sub);
        let mut qd = qh;
        if negative {
            // add back
            qd -= 1;
            let mut c = 0u64;
            for i in 0..n {
                let s128 = u[i + j] as u128 + v[i] as u128 + c as u128; if s128 >= BASE as u128 { u[i + j] = (s128 - BASE as u128) as u64; c = 1; continue; } let s = s128 as u64;
                if s >= BASE {
                    u[i + j] = s - BASE;
                    c = 1;
                } else {
                    u[i + j] = s;
                    c = 0;
                }
            }
            u[j + n] = u[j + n].wrapping_add(c);
        }
        q[j] = qd;
    }
    trim(&mut q);
    u.truncate(n);
    trim(&mut u);
    let r = if f > 1 { divrem_small(&u, f).0 } else { u };
    (q, r)
}

// ------------------------------ digit shifts -------------------------------------

/// 10^n
pub fn pow10(n: u64) -> Vec<u64> {
    let limbs = (n / LDIG) as usize;
    let mut v = vec![0u64; limbs + 1];
    v[limbs] = POW10[(n % LDIG) as usize];
    v
}

/// a * 10^n
pub fn shl(a: &[u64], n: u64) -> Vec<u64> {
    if a.is_empty() {
        return vec![];
    }
    let limbs = (n / LDIG) as usize;
    let k = (n % LDIG) as usize;
    let mut r = Vec::with_capacity(a.len() + limbs + 1);
    r.resize(limbs, 0);
    if k == 0 {
        r.extend_from_slice(a);
    } else {
        let p = POW10[k];
        let q = POW10[LDIG as usize - k];
        let mut carry = 0u64;
        for &x in a {
            r.push((x % q) * p + carry);
            carry = x / q;
        }
        if carry != 0 {
            r.push(carry);
        }
    }
    r
}

/// a / BASE^k (drop k low limbs).
pub fn shr_trunc_limbs(a: &[u64], k: usize) -> Vec<u64> {
    if k >= a.len() {
        vec![]
    } else {
        a[k..].to_vec()
    }
}

/// Digit (0..9) at decimal position i (0 = least significant).
#[inline]
pub fn digit_at(a: &[u64], i: u64) -> u64 {
    let l = (i / LDIG) as usize;
    if l >= a.len() {
        return 0;
    }
    (a[l] / POW10[(i % LDIG) as usize]) % 10
}

/// Whether any digit below position i (exclusive) is nonzero.
pub fn nonzero_below(a: &[u64], i: u64) -> bool {
    let l = (i / LDIG) as usize;
    let k = (i % LDIG) as usize;
    let full = l.min(a.len());
    if a[..full].iter().any(|&x| x != 0) {
        return true;
    }
    if l < a.len() && k > 0 && a[l] % POW10[k] != 0 {
        return true;
    }
    false
}

/// a / 10^n (truncated) together with the classification of the discarded digits.
pub fn shr(a: &[u64], n: u64) -> (Vec<u64>, Rest) {
    if n == 0 {
        return (a.to_vec(), Rest::Zero);
    }
    let nd = digits(a);
    if n > nd {
        return (vec![], if a.is_empty() { Rest::Zero } else { Rest::Lt });
    }
    let d = digit_at(a, n - 1);
    let rest = Rest::from_digit(d, nonzero_below(a, n - 1));
    (shr_trunc(a, n), rest)
}

/// a / 10^n truncated.
pub fn shr_trunc(a: &[u64], n: u64) -> Vec<u64> {
    let limbs = (n / LDIG) as usize;
    if limbs >= a.len() {
        return vec![];
    }
    let k = (n % LDIG) as usize;
    let src = &a[limbs..];
    let mut r = Vec::with_capacity(src.len());
    if k == 0 {
        r.extend_from_slice(src);
    } else {
        let p = POW10[k];
        let q = POW10[LDIG as usize - k];
        for i in 0..src.len() {
            let hi = if i + 1 < src.len() { (src[i + 1] % p) * q } else { 0 };
            r.push(src[i] / p + hi);
        }
    }
    trim(&mut r);
    r
}

/// a mod 10^n
pub fn low_digits(a: &[u64], n: u64) -> Vec<u64> {
    let limbs = (n / LDIG) as usize;
    let k = (n % LDIG) as usize;
    if limbs >= a.len() {
        return a.to_vec();
    }
    let mut r = a[..limbs].to_vec();
    if k > 0 {
        r.push(a[limbs] % POW10[k]);
    }
    trim(&mut r);
    r
}

/// Number of trailing decimal zeros (a nonzero).
pub fn trailing_zeros(a: &[u64]) -> u64 {
    let mut n = 0;
    for &x in a {
        if x == 0 {
            n += LDIG;
        } else {
            let mut x = x;
            while x % 10 == 0 {
                x /= 10;
                n += 1;
            }
            return n;
        }
    }
    n
}

// ------------------------------ misc -------------------------------------

/// Integer square root: floor(sqrt(a)).
pub fn isqrt(a: &[u64]) -> Vec<u64> {
    if a.is_empty() {
        return vec![];
    }
    if let Some(v) = to_u128(a) {
        return from_u128(isqrt_u128(v));
    }
    // Newton iteration from above, starting with an estimate that is >= the root.
    let nd = digits(a);
    // estimate from top digits using f64
    let top_digits = nd.min(30);
    let top = shr_trunc(a, nd - top_digits);
    let topv = to_u128(&top).unwrap() as f64;
    let rest_exp = nd - top_digits; // a ~ topv * 10^rest_exp
    let mut x: Vec<u64>;
    if rest_exp % 2 == 0 {
        let s = topv.sqrt() * (1.0 + 1e-12) + 1.0;
        x = shl(&from_u128(s as u128), rest_exp / 2);
    } else {
        let s = (topv * 10.0).sqrt() * (1.0 + 1e-12) + 1.0;
        x = shl(&from_u128(s as u128), (rest_exp - 1) / 2);
    }
    // ensure x >= sqrt(a)
    loop {
        let (q, _) = divrem(a, &x);
        // y = (x + a/x) / 2
        let s = add(&x, &q);
        let (y, _) = divrem_small(&s, 2);
        if cmp(&y, &x) != Ordering::Less {
            break;
        }
        x = y;
    }
    // x is floor(sqrt(a)) once the sequence stops decreasing (when started above)
    while cmp(&mul(&x, &x), a) == Ordering::Greater {
        decr(&mut x);
    }
    loop {
        let mut x1 = x.clone();
        incr(&mut x1);
        if cmp(&mul(&x1, &x1), a) != Ordering::Greater {
            x = x1;
        } else {
            break;
        }
    }
    x
}

pub fn isqrt_u128(v: u128) -> u128 {
    if v == 0 {
        return 0;
    }
    let mut x = (v as f64).sqrt() as u128;
    // fix up
    while x > 0 && x.checked_mul(x).map_or(true, |s| s > v) {
        x -= 1;
    }
    while (x + 1).checked_mul(x + 1).map_or(false, |s| s <= v) {
        x += 1;
    }
    x
}

/// a^e by binary powering.
pub fn pow(a: &[u64], mut e: u64) -> Vec<u64> {
    let mut result = vec![1u64];
    let mut base = a.to_vec();
    while e > 0 {
        if e & 1 == 1 {
            result = mul(&result, &base);
        }
        e >>= 1;
        if e > 0 {
            base = sqr(&base);
        }
    }
    result
}

/// Decimal digit string (no leading zeros; "0" for zero).
pub fn to_string(a: &[u64]) -> String {
    if a.is_empty() {
        return "0".to_string();
    }
    let mut s = String::with_capacity(a.len() * 19);
    s.push_str(&a[a.len() - 1].to_string());
    for i in (0..a.len() - 1).rev() {
        let t = a[i].to_string();
        for _ in t.len()..19 {
            s.push('0');
        }
        s.push_str(&t);
    }
    s
}

/// Parse a string of ASCII digits (leading zeros allowed).
pub fn from_digits(s: &[u8]) -> Vec<u64> {
    let mut v = Vec::with_capacity(s.len() / 19 + 1);
    let mut end = s.len();
    while end > 0 {
        let start = end.saturating_sub(19);
        let mut x = 0u64;
        for &c in &s[start..end] {
            x = x * 10 + (c - b'0') as u64;
        }
        v.push(x);
        end = start;
    }
    trim(&mut v);
    v
}

/// Digits as a vector of u8 values, most significant first ("0" -> [0]).
pub fn to_digit_vec(a: &[u64]) -> Vec<u8> {
    to_string(a).bytes().map(|c| c - b'0').collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn basics() {
        assert_eq!(u64_digits(9), 1);
        assert_eq!(u64_digits(10), 2);
        assert_eq!(u64_digits(BASE - 1), 19);
        assert_eq!(u64_digits(u64::MAX), 20);
        let a = from_digits(b"123456789012345678901234567890123456789");
        assert_eq!(to_string(&a), "123456789012345678901234567890123456789");
        let b = from_digits(b"987654321098765432109876543210");
        let p = mul(&a, &b);
        let (q, r) = divrem(&p, &b);
        assert_eq!(to_string(&q), to_string(&a));
        assert!(r.is_empty());
        assert_eq!(to_string(&shl(&a, 7)), "1234567890123456789012345678901234567890000000");
        let (s, rest) = shr(&a, 5);
        assert_eq!(to_string(&s), "1234567890123456789012345678901234");
        assert_eq!(rest, Rest::Gt);
        for s in ["10000000000000000000000000000000000000000000000000", "123456789012345678901234567890123456789012345678901234567890", "99999999999999999999999999999999999999999"] {
            let a = from_digits(s.as_bytes());
            let r = isqrt(&a);
            assert!(cmp(&mul(&r, &r), &a) != Ordering::Greater);
            let r1 = add_small(&r, 1);
            assert!(cmp(&mul(&r1, &r1), &a) == Ordering::Greater);
        }
    }
}
