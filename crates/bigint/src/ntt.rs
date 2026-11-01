//! Number-theoretic transform multiplication for very large operands.
//!
//! Each 30-bit digit is one coefficient. The cyclic convolution is computed modulo three primes
//! p < 2^31 (p = c * 2^k + 1) with 32-bit Montgomery arithmetic (SIMD friendly), then recombined
//! with Garner's CRT. Coefficients of the convolution are < N * 2^60 < p1*p2*p3 (~2^90.5) for
//! N <= 2^26, so the result is exact. Larger products are split by the caller.

use crate::nat::{self, MASK, SHIFT};
use std::sync::{Arc, RwLock};

/// Largest supported transform length (2-adicity of the primes).
pub const MAX_LOG: u32 = 26;
pub const MAX_LEN: usize = 1 << MAX_LOG;

#[derive(Clone, Copy)]
struct Prime {
    p: u32,
    /// -p^-1 mod 2^32
    pneg_inv: u32,
    /// R^2 mod p, R = 2^32
    r2: u32,
    g: u32,
}

const fn neg_inv32(p: u32) -> u32 {
    let mut x: u32 = p; // 3 bits
    let mut i = 0;
    while i < 5 {
        x = x.wrapping_mul(2u32.wrapping_sub(p.wrapping_mul(x)));
        i += 1;
    }
    x.wrapping_neg()
}

const fn r2_of(p: u32) -> u32 {
    let r = (1u64 << 32) % p as u64;
    ((r * r) % p as u64) as u32
}

const fn mk(p: u32, g: u32) -> Prime {
    Prime { p, pneg_inv: neg_inv32(p), r2: r2_of(p), g }
}

const PRIMES: [Prime; 3] = [mk(2013265921, 31), mk(1811939329, 13), mk(469762049, 3)];

/// Montgomery product for a, b < p: returns a*b*2^-32 mod p in [0, p).
#[inline(always)]
fn mont(a: u32, b: u32, p: u32, pni: u32) -> u32 {
    let t = a as u64 * b as u64;
    let m = (t as u32).wrapping_mul(pni);
    let u = ((t + m as u64 * p as u64) >> 32) as u32;
    u.min(u.wrapping_sub(p))
}

#[inline(always)]
fn add_mod(a: u32, b: u32, p: u32) -> u32 {
    let s = a + b;
    s.min(s.wrapping_sub(p))
}

#[inline(always)]
fn sub_mod(a: u32, b: u32, p: u32) -> u32 {
    let d = a.wrapping_sub(b);
    d.min(d.wrapping_add(p))
}

fn to_mont(x: u32, pr: &Prime) -> u32 {
    mont(x % pr.p, pr.r2, pr.p, pr.pneg_inv)
}

fn mont_pow(mut b: u32, mut e: u64, pr: &Prime) -> u32 {
    let mut r = to_mont(1, pr);
    while e > 0 {
        if e & 1 == 1 {
            r = mont(r, b, pr.p, pr.pneg_inv);
        }
        b = mont(b, b, pr.p, pr.pneg_inv);
        e >>= 1;
    }
    r
}

/// Twiddle tables: `fwd[len + j] = w_{2len}^j * R`, `inv[len + j] = w_{2len}^-j * R`.
struct Tables {
    size: usize,
    fwd: Vec<Vec<u32>>,
    inv: Vec<Vec<u32>>,
}

static TABLES: RwLock<Option<Arc<Tables>>> = RwLock::new(None);

fn build_tables(size: usize) -> Tables {
    let mut fwd = Vec::new();
    let mut inv = Vec::new();
    for pr in PRIMES.iter() {
        let mut f = vec![0u32; size];
        let mut iv = vec![0u32; size];
        let half = size / 2;
        let log = size.trailing_zeros();
        let g_m = to_mont(pr.g, pr);
        let w = mont_pow(g_m, ((pr.p - 1) >> log) as u64, pr);
        let winv = mont_pow(w, (pr.p - 2) as u64, pr);
        let one = to_mont(1, pr);
        let (mut x, mut y) = (one, one);
        for j in 0..half {
            f[half + j] = x;
            iv[half + j] = y;
            x = mont(x, w, pr.p, pr.pneg_inv);
            y = mont(y, winv, pr.p, pr.pneg_inv);
        }
        let mut len = half / 2;
        while len >= 1 {
            for j in 0..len {
                f[len + j] = f[2 * len + 2 * j];
                iv[len + j] = iv[2 * len + 2 * j];
            }
            len /= 2;
        }
        fwd.push(f);
        inv.push(iv);
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

macro_rules! transforms {
    ($dif:ident, $dit:ident, $pointwise:ident, $scale:ident $(, #[$attr:meta])?) => {
        /// Forward DIF transform (natural in, bit-reversed out).
        $(#[$attr])?
        unsafe fn $dif(a: &mut [u32], tw: &[u32], p: u32, pni: u32) {
            let n = a.len();
            let mut len = n / 2;
            while len >= 1 {
                let w = &tw[len..2 * len];
                for block in a.chunks_exact_mut(2 * len) {
                    let (x, y) = block.split_at_mut(len);
                    for ((xj, yj), &wj) in x.iter_mut().zip(y.iter_mut()).zip(w.iter()) {
                        let u = *xj;
                        let v = *yj;
                        *xj = add_mod(u, v, p);
                        *yj = mont(sub_mod(u, v, p), wj, p, pni);
                    }
                }
                len /= 2;
            }
        }

        /// Inverse DIT transform (bit-reversed in, natural out), unscaled.
        $(#[$attr])?
        unsafe fn $dit(a: &mut [u32], tw: &[u32], p: u32, pni: u32) {
            let n = a.len();
            let mut len = 1;
            while len < n {
                let w = &tw[len..2 * len];
                for block in a.chunks_exact_mut(2 * len) {
                    let (x, y) = block.split_at_mut(len);
                    for ((xj, yj), &wj) in x.iter_mut().zip(y.iter_mut()).zip(w.iter()) {
                        let u = *xj;
                        let v = mont(*yj, wj, p, pni);
                        *xj = add_mod(u, v, p);
                        *yj = sub_mod(u, v, p);
                    }
                }
                len *= 2;
            }
        }

        $(#[$attr])?
        unsafe fn $pointwise(a: &mut [u32], b: &[u32], p: u32, pni: u32) {
            for (x, &y) in a.iter_mut().zip(b.iter()) {
                *x = mont(*x, y, p, pni);
            }
        }

        $(#[$attr])?
        unsafe fn $scale(a: &mut [u32], c: u32, p: u32, pni: u32) {
            for x in a.iter_mut() {
                *x = mont(*x, c, p, pni);
            }
        }
    };
}

transforms!(dif_generic, dit_generic, pw_generic, scale_generic);
#[cfg(target_arch = "x86_64")]
transforms!(dif_avx2, dit_avx2, pw_avx2, scale_avx2, #[target_feature(enable = "avx2")]);
#[cfg(target_arch = "x86_64")]
transforms!(dif_avx512, dit_avx512, pw_avx512, scale_avx512, #[target_feature(enable = "avx512f,avx512bw,avx512vl,avx2")]);

#[derive(Clone, Copy, PartialEq)]
enum Isa {
    Generic,
    Avx2,
    Avx512,
}

fn isa() -> Isa {
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("avx512f")
            && std::arch::is_x86_feature_detected!("avx512bw")
            && std::arch::is_x86_feature_detected!("avx512vl")
        {
            return Isa::Avx512;
        }
        if std::arch::is_x86_feature_detected!("avx2") {
            return Isa::Avx2;
        }
    }
    Isa::Generic
}

fn dif(isa: Isa, a: &mut [u32], tw: &[u32], pr: &Prime) {
    unsafe {
        match isa {
            #[cfg(target_arch = "x86_64")]
            Isa::Avx512 => dif_avx512(a, tw, pr.p, pr.pneg_inv),
            #[cfg(target_arch = "x86_64")]
            Isa::Avx2 => dif_avx2(a, tw, pr.p, pr.pneg_inv),
            _ => dif_generic(a, tw, pr.p, pr.pneg_inv),
        }
    }
}

fn dit(isa: Isa, a: &mut [u32], tw: &[u32], pr: &Prime) {
    unsafe {
        match isa {
            #[cfg(target_arch = "x86_64")]
            Isa::Avx512 => dit_avx512(a, tw, pr.p, pr.pneg_inv),
            #[cfg(target_arch = "x86_64")]
            Isa::Avx2 => dit_avx2(a, tw, pr.p, pr.pneg_inv),
            _ => dit_generic(a, tw, pr.p, pr.pneg_inv),
        }
    }
}

fn pointwise(isa: Isa, a: &mut [u32], b: &[u32], pr: &Prime) {
    unsafe {
        match isa {
            #[cfg(target_arch = "x86_64")]
            Isa::Avx512 => pw_avx512(a, b, pr.p, pr.pneg_inv),
            #[cfg(target_arch = "x86_64")]
            Isa::Avx2 => pw_avx2(a, b, pr.p, pr.pneg_inv),
            _ => pw_generic(a, b, pr.p, pr.pneg_inv),
        }
    }
}

fn scale(isa: Isa, a: &mut [u32], c: u32, pr: &Prime) {
    unsafe {
        match isa {
            #[cfg(target_arch = "x86_64")]
            Isa::Avx512 => scale_avx512(a, c, pr.p, pr.pneg_inv),
            #[cfg(target_arch = "x86_64")]
            Isa::Avx2 => scale_avx2(a, c, pr.p, pr.pneg_inv),
            _ => scale_generic(a, c, pr.p, pr.pneg_inv),
        }
    }
}

/// Load digits reduced modulo p (digits < 2^30 may exceed the smallest prime).
fn load(dst: &mut [u32], src: &[u32], p: u32) {
    for (d, &s) in dst.iter_mut().zip(src.iter()) {
        let s = s.min(s.wrapping_sub(p));
        *d = s.min(s.wrapping_sub(p));
    }
    for d in dst[src.len()..].iter_mut() {
        *d = 0;
    }
}

/// Convolution modulo prime `pi` (result natural order in `fa`).
fn conv_mod(isa: Isa, fa: &mut [u32], fb: &mut [u32], a: &[u32], b: Option<&[u32]>, t: &Tables, pi: usize) {
    let pr = &PRIMES[pi];
    let n = fa.len();
    load(fa, a, pr.p);
    dif(isa, fa, &t.fwd[pi], pr);
    match b {
        Some(b) => {
            load(fb, b, pr.p);
            dif(isa, fb, &t.fwd[pi], pr);
            pointwise(isa, fa, fb, pr);
        }
        None => {
            fb.copy_from_slice(fa);
            pointwise(isa, fa, fb, pr);
        }
    }
    dit(isa, fa, &t.inv[pi], pr);
    // scale by n^-1 * R (cancels the R^-1 of the pointwise Montgomery product)
    let ninv = mont_pow(to_mont((n as u64 % pr.p as u64) as u32, pr), (pr.p - 2) as u64, pr); // n^-1 R
    let c = mont(ninv, pr.r2, pr.p, pr.pneg_inv); // n^-1 R^2
    scale(isa, fa, c, pr);
}

fn modinv(a: u64, m: u64) -> u64 {
    // m prime
    let mut r = 1u64;
    let mut b = a % m;
    let mut e = m - 2;
    while e > 0 {
        if e & 1 == 1 {
            r = r * b % m;
        }
        b = b * b % m;
        e >>= 1;
    }
    r
}

fn crt_combine(r: &[Vec<u32>], len: usize) -> Vec<u32> {
    let p1 = PRIMES[0].p as u64;
    let p2 = PRIMES[1].p as u64;
    let p3 = PRIMES[2].p as u64;
    let pr2 = &PRIMES[1];
    let pr3 = &PRIMES[2];
    // Montgomery-form constants so that mont(x, c) = x * k mod p.
    let inv_p1_mod_p2 = to_mont(modinv(p1 % p2, p2) as u32, pr2);
    let inv_p1p2_mod_p3 = to_mont(modinv((p1 * p2) % p3, p3) as u32, pr3);
    let p1_mod_p3 = (p1 % p3) as u32;
    let p1_mod_p3_m = to_mont(p1_mod_p3, pr3);
    let p1p2 = p1 as u128 * p2 as u128;
    let mut out = vec![0u32; len + 4];
    let mut carry: u128 = 0;
    for i in 0..len {
        let x1 = r[0][i];
        let x1_2 = if (x1 as u64) >= p2 { x1 - p2 as u32 } else { x1 };
        let v2 = mont(sub_mod(r[1][i], x1_2, p2 as u32), inv_p1_mod_p2, p2 as u32, pr2.pneg_inv);
        // v3 = (r3 - x1 - v2*p1) / (p1 p2) mod p3
        let x1_3 = (x1 as u64 % p3) as u32;
        let v2_3 = if (v2 as u64) >= p3 { (v2 as u64 % p3) as u32 } else { v2 };
        let t = sub_mod(sub_mod(r[2][i], x1_3, p3 as u32), mont(v2_3, p1_mod_p3_m, p3 as u32, pr3.pneg_inv), p3 as u32);
        let v3 = mont(t, inv_p1p2_mod_p3, p3 as u32, pr3.pneg_inv);
        let x = x1 as u128 + v2 as u128 * p1 as u128 + v3 as u128 * p1p2;
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
    let _ = p1_mod_p3;
    nat::normalize(&mut out);
    out
}

/// `a * b` via NTT (inputs normalized, nonempty, a.len() + b.len() <= MAX_LEN).
pub fn mul(a: &[u32], b: &[u32]) -> Vec<u32> {
    let len = a.len() + b.len();
    assert!(len <= MAX_LEN);
    let n = len.next_power_of_two();
    let t = tables_for(n);
    let isa = isa();
    let mut res = Vec::with_capacity(3);
    let mut fb = vec![0u32; n];
    for pi in 0..3 {
        let mut fa = vec![0u32; n];
        conv_mod(isa, &mut fa, &mut fb, a, Some(b), &t, pi);
        res.push(fa);
    }
    crt_combine(&res, len)
}

/// `a * a` via NTT.
pub fn sqr(a: &[u32]) -> Vec<u32> {
    let len = 2 * a.len();
    assert!(len <= MAX_LEN);
    let n = len.next_power_of_two();
    let t = tables_for(n);
    let isa = isa();
    let mut res = Vec::with_capacity(3);
    let mut fb = vec![0u32; n];
    for pi in 0..3 {
        let mut fa = vec![0u32; n];
        conv_mod(isa, &mut fa, &mut fb, a, None, &t, pi);
        res.push(fa);
    }
    crt_combine(&res, len)
}
