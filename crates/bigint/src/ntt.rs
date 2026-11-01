//! Number-theoretic transform multiplication for very large operands.
//!
//! Each 30-bit digit is one coefficient; the cyclic convolution is computed modulo two primes
//! p < 2^62 (p = c * 2^k + 1) using Montgomery arithmetic, then recombined with the CRT. The
//! convolution coefficients are < N * 2^60 < p1 * p2 for any N < 2^60, so the result is exact.

use crate::nat::{self, MASK, SHIFT};
use std::sync::{Arc, RwLock};

#[derive(Clone, Copy)]
struct Prime {
    p: u64,
    /// p^-1 mod 2^64
    pinv: u64,
    /// R^2 mod p (R = 2^64)
    r2: u64,
    /// primitive root
    g: u64,
    /// largest k with 2^k | p-1
    two_adicity: u32,
}

const fn inv64(p: u64) -> u64 {
    // Newton iteration for the inverse modulo 2^64 (p odd).
    let mut x: u64 = p; // correct to 3 bits
    let mut i = 0;
    while i < 6 {
        x = x.wrapping_mul(2u64.wrapping_sub(p.wrapping_mul(x)));
        i += 1;
    }
    x
}

const fn r2_of(p: u64) -> u64 {
    // (2^128) mod p computed with u128.
    let r = (u128::MAX % p as u128 + 1) % p as u128; // 2^128 mod p
    r as u64
}

const fn mk(p: u64, g: u64, k: u32) -> Prime {
    Prime { p, pinv: inv64(p), r2: r2_of(p), g, two_adicity: k }
}

const P1: Prime = mk(4179340454199820289, 3, 57); // 29 * 2^57 + 1
const P2: Prime = mk(2485986994308513793, 5, 55); // 69 * 2^55 + 1
const PRIMES: [Prime; 2] = [P1, P2];

#[inline(always)]
fn mont_mul(a: u64, b: u64, p: u64, pinv: u64) -> u64 {
    let t = a as u128 * b as u128;
    let lo = t as u64;
    let hi = (t >> 64) as u64;
    let m = lo.wrapping_mul(pinv);
    let mp_hi = ((m as u128 * p as u128) >> 64) as u64;
    let (r, borrow) = hi.overflowing_sub(mp_hi);
    if borrow { r.wrapping_add(p) } else { r }
}

#[inline(always)]
fn add_mod(a: u64, b: u64, p: u64) -> u64 {
    let s = a + b; // < 2p < 2^63
    if s >= p { s - p } else { s }
}

#[inline(always)]
fn sub_mod(a: u64, b: u64, p: u64) -> u64 {
    let (d, borrow) = a.overflowing_sub(b);
    if borrow { d.wrapping_add(p) } else { d }
}

fn to_mont(x: u64, pr: &Prime) -> u64 {
    mont_mul(x % pr.p, pr.r2, pr.p, pr.pinv)
}

fn mont_pow(mut base_m: u64, mut e: u64, pr: &Prime) -> u64 {
    let mut r = to_mont(1, pr);
    while e > 0 {
        if e & 1 == 1 {
            r = mont_mul(r, base_m, pr.p, pr.pinv);
        }
        base_m = mont_mul(base_m, base_m, pr.p, pr.pinv);
        e >>= 1;
    }
    r
}

/// Twiddle tables for one prime: `fwd[len + j] = w_{2len}^j`, `inv[len + j] = w_{2len}^-j`
/// (Montgomery form) for every power of two `len < size`.
struct Tables {
    size: usize,
    fwd: [Vec<u64>; 2],
    inv: [Vec<u64>; 2],
}

static TABLES: RwLock<Option<Arc<Tables>>> = RwLock::new(None);

fn build_tables(size: usize) -> Tables {
    let mut fwd: [Vec<u64>; 2] = [Vec::new(), Vec::new()];
    let mut inv: [Vec<u64>; 2] = [Vec::new(), Vec::new()];
    for (pi, pr) in PRIMES.iter().enumerate() {
        let mut f = vec![0u64; size.max(2)];
        let mut iv = vec![0u64; size.max(2)];
        // Top level: len = size/2, root of order size.
        let half = size / 2;
        if half >= 1 {
            let log = size.trailing_zeros();
            assert!(log <= pr.two_adicity);
            let g_m = to_mont(pr.g, pr);
            let w = mont_pow(g_m, (pr.p - 1) >> log, pr);
            let winv = mont_pow(w, pr.p - 2, pr);
            let one = to_mont(1, pr);
            let mut x = one;
            let mut y = one;
            for j in 0..half {
                f[half + j] = x;
                iv[half + j] = y;
                x = mont_mul(x, w, pr.p, pr.pinv);
                y = mont_mul(y, winv, pr.p, pr.pinv);
            }
            let mut len = half / 2;
            while len >= 1 {
                for j in 0..len {
                    f[len + j] = f[2 * len + 2 * j];
                    iv[len + j] = iv[2 * len + 2 * j];
                }
                len /= 2;
            }
        }
        fwd[pi] = f;
        inv[pi] = iv;
    }
    Tables { size, fwd, inv }
}

fn tables_for(size: usize) -> Arc<Tables> {
    {
        let g = TABLES.read().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = g.as_ref() {
            if t.size >= size {
                return t.clone();
            }
        }
    }
    let mut g = TABLES.write().unwrap_or_else(|e| e.into_inner());
    if let Some(t) = g.as_ref() {
        if t.size >= size {
            return t.clone();
        }
    }
    let t = Arc::new(build_tables(size.max(1 << 12)));
    *g = Some(t.clone());
    t
}

/// Decimation-in-frequency forward transform (natural order in, bit-reversed out).
fn ntt_dif(a: &mut [u64], tw: &[u64], pr: &Prime) {
    let n = a.len();
    let p = pr.p;
    let pinv = pr.pinv;
    let mut len = n / 2;
    while len >= 1 {
        let w = &tw[len..2 * len];
        for block in a.chunks_exact_mut(2 * len) {
            let (x, y) = block.split_at_mut(len);
            for j in 0..len {
                let u = x[j];
                let v = y[j];
                x[j] = add_mod(u, v, p);
                y[j] = mont_mul(sub_mod(u, v, p), w[j], p, pinv);
            }
        }
        len /= 2;
    }
}

/// Decimation-in-time inverse transform (bit-reversed in, natural out), unscaled.
fn ntt_dit(a: &mut [u64], tw: &[u64], pr: &Prime) {
    let n = a.len();
    let p = pr.p;
    let pinv = pr.pinv;
    let mut len = 1;
    while len < n {
        let w = &tw[len..2 * len];
        for block in a.chunks_exact_mut(2 * len) {
            let (x, y) = block.split_at_mut(len);
            for j in 0..len {
                let u = x[j];
                let v = mont_mul(y[j], w[j], p, pinv);
                x[j] = add_mod(u, v, p);
                y[j] = sub_mod(u, v, p);
            }
        }
        len *= 2;
    }
}

fn load(dst: &mut [u64], src: &[u32]) {
    for (d, &s) in dst.iter_mut().zip(src.iter()) {
        *d = s as u64;
    }
    for d in dst[src.len()..].iter_mut() {
        *d = 0;
    }
}

/// Convolution of a and b modulo prime `pi`; result (natural order) in `fa`.
fn conv_mod(fa: &mut [u64], fb: Option<&mut [u64]>, a: &[u32], b: &[u32], t: &Tables, pi: usize) {
    let pr = &PRIMES[pi];
    let n = fa.len();
    load(fa, a);
    ntt_dif(fa, &t.fwd[pi], pr);
    match fb {
        Some(fb) => {
            load(fb, b);
            ntt_dif(fb, &t.fwd[pi], pr);
            for (x, &y) in fa.iter_mut().zip(fb.iter()) {
                *x = mont_mul(*x, y, pr.p, pr.pinv);
            }
        }
        None => {
            for x in fa.iter_mut() {
                *x = mont_mul(*x, *x, pr.p, pr.pinv);
            }
        }
    }
    ntt_dit(fa, &t.inv[pi], pr);
    // scale by n^-1 * R^2 (undo Montgomery factor from pointwise product).
    let ninv = mont_pow(to_mont(n as u64, pr), pr.p - 2, pr); // (n^-1) R
    // mont_mul(x, c) must equal x * n^-1 * R (undoing the R^-1 of the pointwise product).
    let c = mont_mul(ninv, pr.r2, pr.p, pr.pinv); // n^-1 * R^2
    for x in fa.iter_mut() {
        *x = mont_mul(*x, c, pr.p, pr.pinv);
    }
}

fn crt_combine(r1: &[u64], r2: &[u64], len: usize) -> Vec<u32> {
    let p1 = P1.p;
    let p2 = P2.p;
    // c = p1^-1 mod p2 in Montgomery form, so mont_mul(d, c) = d * p1^-1 mod p2.
    let p1_mod_p2 = p1 % p2;
    let p1m = to_mont(p1_mod_p2, &P2);
    let p1inv_m = mont_pow(p1m, p2 - 2, &P2); // (p1^-1) R
    let c = p1inv_m; // mont_mul(d, c) = d * p1^-1 mod p2
    let mut out = vec![0u32; len + 5];
    let mut carry: u128 = 0;
    for i in 0..len {
        let x1 = r1[i];
        let x1m = if x1 >= p2 { x1 - p2 } else { x1 };
        let d = sub_mod(r2[i], x1m, p2);
        let k = mont_mul(d, c, p2, P2.pinv);
        let x = x1 as u128 + p1 as u128 * k as u128;
        carry += x;
        out[i] = (carry as u32) & MASK;
        carry >>= SHIFT;
    }
    let mut i = len;
    while carry != 0 {
        out[i] = (carry as u32) & MASK;
        carry >>= SHIFT;
        i += 1;
    }
    nat::normalize(&mut out);
    out
}

/// `a * b` via NTT (inputs normalized, nonempty).
pub fn mul(a: &[u32], b: &[u32]) -> Vec<u32> {
    let len = a.len() + b.len();
    let n = len.next_power_of_two();
    let t = tables_for(n);
    let mut res: Vec<Vec<u64>> = Vec::with_capacity(2);
    let mut fb = vec![0u64; n];
    for pi in 0..2 {
        let mut fa = vec![0u64; n];
        conv_mod(&mut fa, Some(&mut fb), a, b, &t, pi);
        res.push(fa);
    }
    crt_combine(&res[0], &res[1], len)
}

/// `a * a` via NTT.
pub fn sqr(a: &[u32]) -> Vec<u32> {
    let len = 2 * a.len();
    let n = len.next_power_of_two();
    let t = tables_for(n);
    let mut res: Vec<Vec<u64>> = Vec::with_capacity(2);
    for pi in 0..2 {
        let mut fa = vec![0u64; n];
        conv_mod(&mut fa, None, a, a, &t, pi);
        res.push(fa);
    }
    crt_combine(&res[0], &res[1], len)
}
