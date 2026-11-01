//! Kernel cross-checks against simple sequential reference implementations.
mod common;
use common::*;
use rpy_bigint::{div, nat, MASK};

fn ref_add(a: &[u32], b: &[u32]) -> Vec<u32> {
    let n = a.len().max(b.len());
    let mut out = Vec::new();
    let mut c = 0u64;
    for i in 0..n {
        let s = *a.get(i).unwrap_or(&0) as u64 + *b.get(i).unwrap_or(&0) as u64 + c;
        out.push((s & MASK as u64) as u32);
        c = s >> 30;
    }
    out.push(c as u32);
    nat::normalize(&mut out);
    out
}

fn ref_sub(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut br = 0i64;
    for i in 0..a.len() {
        let s = a[i] as i64 - *b.get(i).unwrap_or(&0) as i64 - br;
        out.push((s.rem_euclid(1 << 30)) as u32);
        br = if s < 0 { 1 } else { 0 };
    }
    assert_eq!(br, 0);
    nat::normalize(&mut out);
    out
}

fn ref_mul1(a: &[u32], d: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut c = 0u64;
    for &x in a {
        let t = x as u64 * d as u64 + c;
        out.push((t & MASK as u64) as u32);
        c = t >> 30;
    }
    while c > 0 {
        out.push((c & MASK as u64) as u32);
        c >>= 30;
    }
    nat::normalize(&mut out);
    out
}

fn patterned(r: &mut Rng, n: usize) -> Vec<u32> {
    let mode = r.below(6);
    let mut v: Vec<u32> = (0..n)
        .map(|_| match mode {
            0 => MASK,
            1 => 0,
            2 => {
                if r.bool() {
                    MASK
                } else {
                    0
                }
            }
            3 => MASK - (r.below(3) as u32),
            4 => r.below(3) as u32,
            _ => (r.next() as u32) & MASK,
        })
        .collect();
    if n > 0 && v[n - 1] == 0 {
        v[n - 1] = 1;
    }
    v
}

#[test]
fn add_sub_kernels() {
    let mut r = Rng::new(31);
    for _ in 0..20000 {
        let n = 1 + r.below(80) as usize;
        let m = 1 + r.below(n as u64) as usize;
        let a = patterned(&mut r, n);
        let b = patterned(&mut r, m);
        let s = nat::add(&a, &b);
        assert_eq!(s, ref_add(&a, &b), "add {a:?} {b:?}");
        assert_eq!(nat::sub(&s, &b), a);
        assert_eq!(nat::sub(&s, &a), nat::trim(&b).to_vec());
        if nat::cmp(&a, &b) != core::cmp::Ordering::Less {
            assert_eq!(nat::sub(&a, &b), ref_sub(&a, &b), "sub {a:?} {b:?}");
        }
    }
}

#[test]
fn mul1_and_submul() {
    let mut r = Rng::new(32);
    for _ in 0..20000 {
        let n = 1 + r.below(80) as usize;
        let a = patterned(&mut r, n);
        let d = match r.below(4) {
            0 => MASK,
            1 => 1 + r.below(10) as u32,
            2 => (r.next() as u32) | 0x8000_0000,
            _ => (r.next() as u32) & MASK,
        };
        assert_eq!(nat::mul_u32(&a, d), ref_mul1(&a, d), "mul1 d={d}");
        // mul_digit_add / mul_digit_sub
        let d = d & MASK;
        let mut acc = patterned(&mut r, n + 1);
        acc[n] = 0;
        let orig = acc.clone();
        let c = nat::mul_digit_add(&mut acc[..n], &a, d);
        acc[n] += c;
        let mut expect = ref_mul1(&a, d);
        nat::add_in_place(&mut expect, nat::trim(&orig));
        assert_eq!(nat::trim(&acc), &expect[..]);
        // subtract back
        let br = nat::mul_digit_sub(&mut acc[..n], &a, d);
        acc[n] -= br;
        assert_eq!(nat::trim(&acc), nat::trim(&orig));
    }
}

#[test]
fn small_quotient_division() {
    let mut r = Rng::new(33);
    for _ in 0..20000 {
        let m = 3 + r.below(60) as usize;
        let b = patterned(&mut r, m);
        let q = (r.next() as u32) & if r.bool() { 15 } else { MASK };
        let rl = 1 + r.below(m as u64) as usize;
        let rem = patterned(&mut r, rl);
        let rem = if nat::cmp(&rem, &b) == core::cmp::Ordering::Less { rem } else { nat::sub(&rem, &[]) .into_iter().take(m - 1).collect::<Vec<_>>() };
        let mut rem = rem;
        nat::normalize(&mut rem);
        if nat::cmp(&rem, &b) != core::cmp::Ordering::Less {
            continue;
        }
        let mut a = nat::mul_u32(&b, q);
        nat::add_in_place(&mut a, &rem);
        if nat::cmp(&a, &b) == core::cmp::Ordering::Less {
            continue;
        }
        let (gq, gr) = nat::divrem(&a, &b);
        assert_eq!(gq, nat::from_u64(q as u64), "quotient");
        assert_eq!(gr, rem, "remainder");
        assert_eq!((gq.clone(), gr.clone()), div::divrem_knuth(&a, &b));
    }
}

#[test]
fn bz_division() {
    let mut r = Rng::new(34);
    for _ in 0..300 {
        let m = 2 + r.below(700) as usize;
        let n = m + r.below(1500) as usize;
        let b = patterned(&mut r, m);
        let a = patterned(&mut r, n);
        let got = div::divrem_bz(&a, &b);
        let expect = div::divrem_knuth(&a, &b);
        assert_eq!(got, expect, "bz {n}/{m}");
    }
    // exact multiples and remainder b-1
    for _ in 0..100 {
        let m = 100 + r.below(400) as usize;
        let b = patterned(&mut r, m);
        let ql = 50 + r.below(600) as usize;
        let q = patterned(&mut r, ql);
        let mut a = nat::mul(&b, &q);
        assert_eq!(div::divrem_bz(&a, &b), (q.clone(), vec![]));
        nat::add_in_place(&mut a, &nat::sub(&b, &[1]));
        assert_eq!(div::divrem_bz(&a, &b), (q, nat::sub(&b, &[1])));
    }
}

#[test]
fn mul_threshold_sweep() {
    use rpy_bigint::mul;
    // Force every algorithm (including NTT) on small sizes and compare with schoolbook.
    let saved = mul::thresholds();
    let mut r = Rng::new(35);
    let configs = [(4, 4, 8, 16, 16), (8, 8, 1000, 40, 40), (16, 16, 24, 100000, 100000), (2, 2, 9, 3000, 3000)];
    let mut cases = Vec::new();
    for _ in 0..40 {
        let n = 1 + r.below(400) as usize;
        let m = 1 + r.below(400) as usize;
        cases.push((patterned(&mut r, n), patterned(&mut r, m)));
    }
    let refs: Vec<Vec<u32>> = cases.iter().map(|(a, b)| mul::mul_schoolbook(a, b)).collect();
    let srefs: Vec<Vec<u32>> = cases.iter().map(|(a, _)| mul::mul_schoolbook(a, a)).collect();
    for c in configs {
        mul::set_thresholds(c.0, c.1, c.2, c.3, c.4);
        for (i, (a, b)) in cases.iter().enumerate() {
            assert_eq!(nat::mul(a, b), refs[i], "config {c:?} case {i}");
            assert_eq!(mul::mul_recursive(a, b), refs[i], "config {c:?} case {i} rec");
            assert_eq!(nat::sqr(a), srefs[i], "config {c:?} sqr {i}");
            assert_eq!(mul::sqr_recursive(a), srefs[i], "config {c:?} sqr rec {i}");
        }
    }
    mul::set_thresholds(saved.0, saved.1, saved.2, saved.3, saved.4);
}

#[test]
fn reciprocal_bounds() {
    let mut r = Rng::new(36);
    for _ in 0..60 {
        let n = 2 + r.below(900) as usize;
        let mut d = patterned(&mut r, n);
        d[n - 1] |= 1 << 29; // normalized
        let approx = div::recip_approx(&d);
        let mut big = vec![0u32; 2 * n + 1];
        big[2 * n] = 1;
        let exact = div::divrem_knuth(&big, &d).0;
        assert_eq!(div::recip(&d), exact);
        assert!(nat::cmp(&approx, &exact) != core::cmp::Ordering::Greater, "overestimate n={n}");
        let diff = nat::sub(&exact, &approx);
        assert!(nat::cmp(&diff, &[8]) == core::cmp::Ordering::Less, "error too large n={n}: {diff:?}");
    }
    // divrem_large on assorted shapes
    for _ in 0..40 {
        let m = 2 + r.below(1200) as usize;
        let n = m + r.below(3000) as usize;
        let b = patterned(&mut r, m);
        let a = patterned(&mut r, n);
        assert_eq!(div::divrem_large(&a, &b), div::divrem_knuth(&a, &b), "large {n}/{m}");
    }
}

#[test]
fn short_quotient_shapes() {
    let mut r = Rng::new(37);
    for _ in 0..60 {
        let m = 500 + r.below(2500) as usize;
        let qlen = 32 + r.below((m / 4 - 32) as u64) as usize;
        let b = patterned(&mut r, m);
        let a = patterned(&mut r, m + qlen - 1);
        assert_eq!(nat::divrem(&a, &b), div::divrem_knuth(&a, &b), "{}/{}", a.len(), m);
        // exact multiple
        let q = patterned(&mut r, qlen);
        let p = nat::mul(&q, &b);
        assert_eq!(nat::divrem(&p, &b), (q, vec![]));
    }
}
