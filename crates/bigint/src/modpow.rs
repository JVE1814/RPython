//! Modular exponentiation (magnitudes): single-digit fast path, Montgomery for odd moduli,
//! division-based reduction otherwise.

use crate::div::Divisor;
use crate::nat::{self, MASK, SHIFT};
use core::cmp::Ordering;

/// `base^e mod m` with `base < m`, `m > 1`.
pub fn modpow(base: &[u32], e: &[u32], m: &[u32]) -> Vec<u32> {
    if e.is_empty() {
        return vec![1];
    }
    if base.is_empty() {
        return Vec::new();
    }
    if m.len() == 1 {
        return small_modpow(base, e, m[0]);
    }
    if m[0] & 1 == 1 {
        return mont_modpow(base, e, m);
    }
    plain_modpow(base, e, m)
}

fn ebits(e: &[u32]) -> u64 {
    nat::bit_length(e)
}

fn small_modpow(base: &[u32], e: &[u32], m: u32) -> Vec<u32> {
    let m64 = m as u64;
    let b = nat::rem_digit(base, m) as u64;
    let mut r = 1u64 % m64;
    let nb = ebits(e);
    for i in (0..nb).rev() {
        r = r * r % m64;
        if nat::test_bit(e, i) {
            r = r * b % m64;
        }
    }
    if r == 0 { Vec::new() } else { vec![r as u32] }
}

fn window_size(bits: u64) -> u32 {
    match bits {
        0..=7 => 1,
        8..=36 => 3,
        37..=140 => 4,
        141..=450 => 5,
        _ => 6,
    }
}

/// Generic sliding-window exponentiation driver.
fn sliding_window<F: Fn(&[u32], &[u32]) -> Vec<u32>>(one: Vec<u32>, base: Vec<u32>, e: &[u32], mulmod: F) -> Vec<u32> {
    let nb = ebits(e);
    let k = window_size(nb);
    // precompute odd powers base^1, base^3, ..., base^(2^k - 1)
    let b2 = mulmod(&base, &base);
    let mut table = vec![base];
    for i in 1..(1usize << (k - 1)) {
        let next = mulmod(&table[i - 1], &b2);
        table.push(next);
    }
    let mut r = one;
    let mut i = nb as i64 - 1;
    let mut started = false;
    while i >= 0 {
        if !nat::test_bit(e, i as u64) {
            if started {
                r = mulmod(&r, &r);
            }
            i -= 1;
            continue;
        }
        // find window [l, i] of length <= k ending in a 1 bit
        let mut l = (i - k as i64 + 1).max(0);
        while !nat::test_bit(e, l as u64) {
            l += 1;
        }
        let mut val = 0usize;
        for j in (l..=i).rev() {
            val = (val << 1) | nat::test_bit(e, j as u64) as usize;
        }
        if started {
            for _ in l..=i {
                r = mulmod(&r, &r);
            }
            r = mulmod(&r, &table[val >> 1]);
        } else {
            r = table[val >> 1].clone();
            started = true;
        }
        i = l - 1;
    }
    r
}

fn plain_modpow(base: &[u32], e: &[u32], m: &[u32]) -> Vec<u32> {
    let big = m.len() >= 1500;
    let divisor = if big { Some(Divisor::new(m)) } else { None };
    let reduce = |x: Vec<u32>| -> Vec<u32> {
        if nat::cmp(&x, m) == Ordering::Less {
            return x;
        }
        match &divisor {
            Some(d) => d.divrem(&x).1,
            None => crate::div::divrem_knuth(&x, m).1,
        }
    };
    sliding_window(vec![1], base.to_vec(), e, |a, b| {
        let p = if a.as_ptr() == b.as_ptr() && a.len() == b.len() { nat::sqr(a) } else { nat::mul(a, b) };
        reduce(p)
    })
}

/// Montgomery context for an odd modulus in base 2^30.
struct Mont<'a> {
    m: &'a [u32],
    /// -m^-1 mod 2^30
    minv: u32,
}

impl<'a> Mont<'a> {
    fn new(m: &'a [u32]) -> Self {
        let m0 = m[0] as u64;
        // inverse mod 2^30 by Newton
        let mut x: u64 = m0; // 3 bits
        for _ in 0..5 {
            x = x.wrapping_mul(2u64.wrapping_sub(m0.wrapping_mul(x)));
        }
        let inv = (x as u32) & MASK;
        debug_assert_eq!((inv as u64 * m0) & MASK as u64, 1);
        let minv = (BASE_U - inv) & MASK;
        Mont { m, minv }
    }

    /// REDC of t (len 2n, t < m * R): returns t * R^-1 mod m (fully reduced).
    fn redc(&self, mut t: Vec<u32>) -> Vec<u32> {
        let n = self.m.len();
        t.resize(2 * n + 1, 0);
        for i in 0..n {
            let u = ((t[i] as u64 * self.minv as u64) as u32) & MASK;
            let c = nat::mul_digit_add(&mut t[i..i + n], self.m, u);
            nat::add_digit_assign(&mut t[i + n..], c);
        }
        let mut r = t[n..].to_vec();
        nat::normalize(&mut r);
        if nat::cmp(&r, self.m) != Ordering::Less {
            nat::sub_in_place(&mut r, self.m);
        }
        r
    }

    fn mul(&self, a: &[u32], b: &[u32]) -> Vec<u32> {
        let p = if a.as_ptr() == b.as_ptr() && a.len() == b.len() { nat::sqr(a) } else { nat::mul(a, b) };
        self.redc(p)
    }

    /// x * R mod m
    fn to_mont(&self, x: &[u32]) -> Vec<u32> {
        let n = self.m.len();
        let mut sh = vec![0u32; n];
        sh.extend_from_slice(x);
        nat::normalize(&mut sh);
        nat::divrem(&sh, self.m).1
    }
}

const BASE_U: u32 = 1 << SHIFT;

fn mont_modpow(base: &[u32], e: &[u32], m: &[u32]) -> Vec<u32> {
    let ctx = Mont::new(m);
    let bm = ctx.to_mont(base);
    let one = ctx.to_mont(&[1]);
    let r = sliding_window(one, bm, e, |a, b| ctx.mul(a, b));
    // convert out of Montgomery form
    ctx.redc(r)
}
