//! Large-operand arithmetic for base-10^19 naturals: three-prime number theoretic
//! transform multiplication (Montgomery arithmetic, CRT recombination) and
//! Newton-reciprocal division.

use super::nat::{self, Div64, BASE};
use std::cmp::Ordering;

pub const NTT_CUTOFF: usize = 700;
pub const NEWTON_DIV_CUTOFF: usize = 400;

/// NTT-friendly primes below 2^62: c*2^k + 1 with known primitive roots.
const PRIMES: [(u64, u64); 3] = [
    (4179340454199820289, 3), // 29*2^57 + 1
    (2485986994308513793, 5), // 69*2^55 + 1
    (1945555039024054273, 5), // 27*2^56 + 1
];

#[derive(Clone, Copy)]
struct Mont {
    p: u64,
    pinv: u64, // -p^{-1} mod 2^64
    r2: u64,   // 2^128 mod p
}

impl Mont {
    fn new(p: u64) -> Mont {
        // Newton iteration for p^{-1} mod 2^64
        let mut inv: u64 = 1;
        for _ in 0..7 {
            inv = inv.wrapping_mul(2u64.wrapping_sub(p.wrapping_mul(inv)));
        }
        let r = ((1u128 << 64) % p as u128) as u64;
        let r2 = ((r as u128 * r as u128) % p as u128) as u64;
        Mont { p, pinv: inv.wrapping_neg(), r2 }
    }
    #[inline(always)]
    fn redc(&self, t: u128) -> u64 {
        let m = (t as u64).wrapping_mul(self.pinv);
        let u = ((t + m as u128 * self.p as u128) >> 64) as u64;
        if u >= self.p { u - self.p } else { u }
    }
    #[inline(always)]
    fn mul(&self, a: u64, b: u64) -> u64 {
        self.redc(a as u128 * b as u128)
    }
    #[inline(always)]
    fn to(&self, a: u64) -> u64 {
        self.mul(a % self.p, self.r2)
    }
    #[inline(always)]
    fn from(&self, a: u64) -> u64 {
        self.redc(a as u128)
    }
    #[inline(always)]
    fn add(&self, a: u64, b: u64) -> u64 {
        let s = a + b;
        if s >= self.p { s - self.p } else { s }
    }
    #[inline(always)]
    fn sub(&self, a: u64, b: u64) -> u64 {
        if a >= b { a - b } else { a + self.p - b }
    }
    fn pow(&self, a: u64, mut e: u64) -> u64 {
        // a in Montgomery form
        let mut r = self.to(1);
        let mut b = a;
        while e > 0 {
            if e & 1 == 1 {
                r = self.mul(r, b);
            }
            b = self.mul(b, b);
            e >>= 1;
        }
        r
    }
}

fn ntt(a: &mut [u64], m: &Mont, g: u64, invert: bool) {
    let n = a.len();
    // bit reversal
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            a.swap(i, j);
        }
    }
    let gm = m.to(g);
    let mut len = 2;
    let mut tw: Vec<u64> = Vec::with_capacity(n / 2);
    while len <= n {
        let mut w = m.pow(gm, (m.p - 1) / len as u64);
        if invert {
            w = m.pow(w, m.p - 2);
        }
        let half = len / 2;
        tw.clear();
        let mut x = m.to(1);
        for _ in 0..half {
            tw.push(x);
            x = m.mul(x, w);
        }
        let mut i = 0;
        while i < n {
            for k in 0..half {
                let u = a[i + k];
                let v = m.mul(a[i + k + half], tw[k]);
                a[i + k] = m.add(u, v);
                a[i + k + half] = m.sub(u, v);
            }
            i += len;
        }
        len <<= 1;
    }
    if invert {
        let ninv = m.pow(m.to(n as u64), m.p - 2);
        for x in a.iter_mut() {
            *x = m.mul(*x, ninv);
        }
    }
}

fn conv_mod(a: &[u64], b: &[u64], n: usize, prime: (u64, u64), square: bool) -> Vec<u64> {
    let m = Mont::new(prime.0);
    let mut fa = vec![0u64; n];
    for (i, &x) in a.iter().enumerate() {
        fa[i] = m.to(x);
    }
    ntt(&mut fa, &m, prime.1, false);
    if square {
        for x in fa.iter_mut() {
            *x = m.mul(*x, *x);
        }
    } else {
        let mut fb = vec![0u64; n];
        for (i, &x) in b.iter().enumerate() {
            fb[i] = m.to(x);
        }
        ntt(&mut fb, &m, prime.1, false);
        for i in 0..n {
            fa[i] = m.mul(fa[i], fb[i]);
        }
    }
    ntt(&mut fa, &m, prime.1, true);
    for x in fa.iter_mut() {
        *x = m.from(*x);
    }
    fa
}

fn mulmod(a: u64, b: u64, p: u64) -> u64 {
    ((a as u128 * b as u128) % p as u128) as u64
}

fn powmod(mut a: u64, mut e: u64, p: u64) -> u64 {
    let mut r = 1u64;
    a %= p;
    while e > 0 {
        if e & 1 == 1 {
            r = mulmod(r, a, p);
        }
        a = mulmod(a, a, p);
        e >>= 1;
    }
    r
}

pub fn mul(a: &[u64], b: &[u64]) -> Vec<u64> {
    let need = a.len() + b.len() - 1;
    let n = need.next_power_of_two();
    let square = std::ptr::eq(a, b) || (a.len() == b.len() && a == b);
    let r1 = conv_mod(a, b, n, PRIMES[0], square);
    let r2 = conv_mod(a, b, n, PRIMES[1], square);
    let r3 = conv_mod(a, b, n, PRIMES[2], square);
    let (p1, p2, p3) = (PRIMES[0].0, PRIMES[1].0, PRIMES[2].0);
    let inv_p1_mod_p2 = powmod(p1 % p2, p2 - 2, p2);
    let p1p2_mod_p3 = mulmod(p1 % p3, p2 % p3, p3);
    let inv_p1p2_mod_p3 = powmod(p1p2_mod_p3, p3 - 2, p3);
    let p1p2: u128 = p1 as u128 * p2 as u128;
    let d = Div64::new(BASE);
    let mut out = Vec::with_capacity(need + 3);
    // carry as 3-word number (w2, w1, w0)
    let mut c: [u64; 3] = [0, 0, 0];
    for i in 0..need {
        let x1 = r1[i];
        let t2 = mulmod((r2[i] + p2 - x1 % p2) % p2, inv_p1_mod_p2, p2);
        // x12 = x1 + p1*t2 < p1*p2
        let x12: u128 = x1 as u128 + p1 as u128 * t2 as u128;
        let x12_mod_p3 = (x12 % p3 as u128) as u64;
        let t3 = mulmod((r3[i] + p3 - x12_mod_p3) % p3, inv_p1p2_mod_p3, p3);
        // value = x12 + p1p2 * t3  (192-bit)
        let lo = (p1p2 as u64) as u128 * t3 as u128;
        let hi = (p1p2 >> 64) * t3 as u128;
        // sum = lo + (hi << 64) + x12 + carry
        let mut w0: u128 = (lo as u64) as u128;
        let mut w1: u128 = (lo >> 64) + (hi as u64) as u128;
        let mut w2: u128 = hi >> 64;
        w0 += (x12 as u64) as u128 + c[0] as u128;
        w1 += (x12 >> 64) + c[1] as u128 + (w0 >> 64);
        w0 &= u64::MAX as u128;
        w2 += c[2] as u128 + (w1 >> 64);
        w1 &= u64::MAX as u128;
        let (w2, w1, w0) = (w2 as u64, w1 as u64, w0 as u64);
        // divide (w2,w1,w0) by BASE
        let q2 = w2 / BASE;
        let r = w2 % BASE;
        let (q1, r) = d.divrem(r, w1);
        let (q0, r) = d.divrem(r, w0);
        out.push(r);
        c = [q0, q1, q2];
    }
    while c != [0, 0, 0] {
        let q2 = c[2] / BASE;
        let r = c[2] % BASE;
        let (q1, r) = d.divrem(r, c[1]);
        let (q0, r) = d.divrem(r, c[0]);
        out.push(r);
        c = [q0, q1, q2];
    }
    nat::trim(&mut out);
    out
}

// ------------------------------ Newton division --------------------------------

/// Approximately floor(BASE^(len(v)+k) / v), v trimmed with nonzero top limb.
fn recip(v: &[u64], k: usize) -> Vec<u64> {
    let n = v.len();
    if k <= 40 || n <= 2 {
        let mut num = vec![0u64; n + k + 1];
        num[n + k] = 1;
        return nat::divrem_knuth(&num, v).0;
    }
    let h = k / 2 + 2;
    let take = n.min(h + 2);
    let vt = &v[n - take..];
    let x = recip(vt, h); // ~ BASE^(take+h)/vt
    // x0 ~ BASE^(n+k)/v  = BASE^(take+k)/vt approximately
    let mut x0 = vec![0u64; k - h];
    x0.extend_from_slice(&x);
    nat::trim(&mut x0);
    // e = BASE^(n+k) - v*x0
    let vx = nat::mul(v, &x0);
    let mut one = vec![0u64; n + k + 1];
    one[n + k] = 1;
    let (neg, e) = match nat::cmp(&one, &vx) {
        Ordering::Less => (true, nat::sub(&vx, &one)),
        _ => (false, nat::sub(&one, &vx)),
    };
    let corr = nat::mul(&x0, &e);
    let corr = nat::shr_trunc_limbs(&corr, n + k);
    if neg { nat::sub(&x0, &corr) } else { nat::add(&x0, &corr) }
}

pub fn divrem_newton(a: &[u64], b: &[u64]) -> (Vec<u64>, Vec<u64>) {
    let m = a.len();
    let n = b.len();
    let k = m - n + 2;
    let r = recip(b, k); // ~ BASE^(n+k)/b
    let prod = nat::mul(a, &r);
    let mut q = nat::shr_trunc_limbs(&prod, n + k);
    let qb = nat::mul(&q, b);
    let mut rem;
    let mut iters = 0;
    if nat::cmp(&qb, a) == Ordering::Greater {
        let mut over = nat::sub(&qb, a);
        // q too large
        loop {
            nat::decr(&mut q);
            if nat::cmp(&over, b) != Ordering::Greater {
                rem = nat::sub(b, &over);
                break;
            }
            over = nat::sub(&over, b);
            iters += 1;
            if iters > 64 {
                return nat::divrem_knuth(a, b);
            }
        }
    } else {
        rem = nat::sub(a, &qb);
    }
    while nat::cmp(&rem, b) != Ordering::Less {
        rem = nat::sub(&rem, b);
        nat::incr(&mut q);
        iters += 1;
        if iters > 64 {
            return nat::divrem_knuth(a, b);
        }
    }
    (q, rem)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_prime(n: u64) -> bool {
        if n < 2 {
            return false;
        }
        for &p in &[2u64, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37] {
            if n % p == 0 {
                return n == p;
            }
        }
        let mut d = n - 1;
        let mut s = 0;
        while d % 2 == 0 {
            d /= 2;
            s += 1;
        }
        'outer: for &a in &[2u64, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37] {
            let mut x = powmod(a, d, n);
            if x == 1 || x == n - 1 {
                continue;
            }
            for _ in 0..s - 1 {
                x = mulmod(x, x, n);
                if x == n - 1 {
                    continue 'outer;
                }
            }
            return false;
        }
        true
    }

    #[test]
    fn primes_and_roots() {
        for &(p, g) in &PRIMES {
            assert!(is_prime(p));
            assert!(p < (1u64 << 62));
            // g must be a generator: g^((p-1)/q) != 1 for prime factors q of p-1
            let mut f = p - 1;
            let mut q = 2;
            let mut factors = vec![];
            while q * q <= f {
                if f % q == 0 {
                    factors.push(q);
                    while f % q == 0 {
                        f /= q;
                    }
                }
                q += 1;
            }
            if f > 1 {
                factors.push(f);
            }
            for q in factors {
                assert_ne!(powmod(g, (p - 1) / q, p), 1, "p={p} g={g} q={q}");
            }
        }
    }

    fn rnd(n: usize, seed: &mut u64) -> Vec<u64> {
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            v.push((*seed >> 1) % BASE);
        }
        nat::trim(&mut v);
        if v.is_empty() {
            v.push(1);
        }
        v
    }

    #[test]
    fn ntt_matches_school() {
        let mut seed = 12345u64;
        for &(la, lb) in &[(1usize, 1usize), (3, 5), (100, 77), (1000, 900), (1500, 2000)] {
            let a = rnd(la, &mut seed);
            let b = rnd(lb, &mut seed);
            let r1 = mul(&a, &b);
            let mut r2 = vec![0u64; a.len() + b.len()];
            // schoolbook reference
            for (j, &bj) in b.iter().enumerate() {
                let mut carry = 0u64;
                for (i, &ai) in a.iter().enumerate() {
                    let t = ai as u128 * bj as u128 + r2[i + j] as u128 + carry as u128;
                    r2[i + j] = (t % BASE as u128) as u64;
                    carry = (t / BASE as u128) as u64;
                }
                r2[j + a.len()] += carry;
            }
            nat::trim(&mut r2);
            assert_eq!(r1, r2, "la={la} lb={lb}");
            assert_eq!(nat::mul(&a, &b), r2);
        }
    }

    #[test]
    fn newton_div() {
        let mut seed = 777u64;
        for &(la, lb) in &[(2000usize, 900usize), (3000, 1200), (1700, 450)] {
            let a = rnd(la, &mut seed);
            let b = rnd(lb, &mut seed);
            let (q, r) = divrem_newton(&a, &b);
            let (q2, r2) = nat::divrem_knuth(&a, &b);
            assert_eq!(q, q2);
            assert_eq!(r, r2);
        }
    }
}
