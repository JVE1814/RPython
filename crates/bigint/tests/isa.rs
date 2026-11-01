//! Run kernel-heavy operations at every SIMD level against scalar references.
mod common;
use common::*;
use rpy_bigint::{mul, nat, set_simd_limit, MASK};

fn ref_mul(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut acc = vec![0u128; a.len() + b.len() + 1];
    for (i, &x) in a.iter().enumerate() {
        for (j, &y) in b.iter().enumerate() {
            acc[i + j] += x as u128 * y as u128;
        }
    }
    let mut out = Vec::with_capacity(acc.len());
    let mut c = 0u128;
    for v in acc {
        let t = v + c;
        out.push((t as u32) & MASK);
        c = t >> 30;
    }
    nat::normalize(&mut out);
    out
}

#[test]
fn all_simd_levels() {
    let saved = mul::thresholds();
    for level in [0u8, 1, 2] {
        set_simd_limit(level);
        let mut r = Rng::new(40 + level as u64);
        for &(n, m) in &[(5usize, 3usize), (13, 13), (40, 17), (97, 96), (200, 150), (700, 650), (1300, 1250), (2100, 2000)] {
            let a = r.mag(n);
            let b = r.mag(m);
            let p = ref_mul(&a, &b);
            assert_eq!(nat::mul(&a, &b), p, "mul {n}x{m} level {level}");
            assert_eq!(mul::mul_schoolbook(&a, &b), p, "school {n}x{m} level {level}");
            assert_eq!(nat::sqr(&a), ref_mul(&a, &a), "sqr {n} level {level}");
            // force NTT
            mul::set_thresholds(4, 4, 8, 16, 16);
            assert_eq!(nat::mul(&a, &b), p, "ntt {n}x{m} level {level}");
            mul::set_thresholds(saved.0, saved.1, saved.2, saved.3, saved.4);
            let (q, rem) = nat::divrem(&p, &b);
            assert_eq!(q, a);
            assert!(rem.is_empty());
            let mut p1 = p.clone();
            nat::add_in_place(&mut p1, &nat::sub(&b, &[1]));
            let (q, rem) = nat::divrem(&p1, &b);
            assert_eq!(q, a);
            assert_eq!(rem, nat::sub(&b, &[1]));
            assert_eq!(nat::sub(&nat::add(&a, &b), &b), a);
            let d = (r.next() as u32) & MASK;
            assert_eq!(nat::mul_u32(&a, d), ref_mul(&a, &nat::from_u64(d as u64)));
            let sh = r.below(100);
            assert_eq!(nat::shr(&nat::shl(&a, sh), sh), a);
        }
        // decimal round trip exercises everything
        let x = BigInt_from(&mut r, 5000);
        let s = x.to_string();
        assert_eq!(s.parse::<rpy_bigint::BigInt>().unwrap(), x);
    }
    set_simd_limit(2);
}

#[allow(non_snake_case)]
fn BigInt_from(r: &mut Rng, n: usize) -> rpy_bigint::BigInt {
    r.big(n)
}
