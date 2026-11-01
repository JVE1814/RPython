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
