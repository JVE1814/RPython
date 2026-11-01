//! Cases ported from spec/Lib/test (test_long, test_int, test_math_integer, test_pow).
mod common;
use common::*;
use rpy_bigint::{conv, math, ops, BigInt, IntError};

fn pow2(e: u64) -> BigInt {
    BigInt::from_i64(1).shl(e).unwrap()
}

#[test]
fn isqrt_values() {
    let mut vals: Vec<BigInt> = (0..1000).map(|v| b(v)).collect();
    vals.extend((1_000_000 - 1000..1_000_000 + 1000).map(|v| b(v)));
    for e in 60..200 {
        for i in -40..40 {
            vals.push(&pow2(e) + &b(i));
        }
    }
    vals.push(BigInt::from_i64(3).pow_u64(9999).unwrap());
    vals.push(BigInt::from_i64(10).pow_u64(5001).unwrap());
    for v in &vals {
        let s = v.isqrt().unwrap();
        assert!(&s * &s <= *v);
        let s1 = &s + &b(1);
        assert!(*v < &s1 * &s1);
    }
    assert_eq!(b(1729).isqrt().unwrap(), b(41));
    assert_eq!(BigInt::from_i64(10).pow_u64(20).unwrap().isqrt().unwrap(), BigInt::from_i64(10).pow_u64(10).unwrap());
    let w = pow2(100_000).isqrt().unwrap();
    assert_eq!(w.bit_length(), 50_001);
    assert_eq!(w.bit_count(), 1);
}

#[test]
fn gcd_lcm_values() {
    let g = |x: &BigInt, y: &BigInt| math::gcd(x.as_ref(), y.as_ref());
    let l = |x: &BigInt, y: &BigInt| math::lcm(x.as_ref(), y.as_ref());
    for (x, y, e) in [(0, 0, 0), (1, 0, 1), (-1, 0, 1), (0, 1, 1), (0, -1, 1), (7, 1, 1), (7, -1, 1), (-23, 15, 1), (120, 84, 12), (84, -120, 12)] {
        assert_eq!(g(&b(x), &b(y)), b(e));
    }
    assert_eq!(g(&s("1216342683557601535506311712"), &s("436522681849110124616458784")), b(32));
    let x = s("434610456570399902378880679233098819019853229470286994367836600566");
    let y = s("1064502245825115327754847244914921553977");
    for c in [s("652560"), s("576559230871654959816130551884856912003141446781646602790216406874")] {
        let a = &x * &c;
        let bb = &y * &c;
        for (p, q) in [(&a, &bb), (&bb, &a)] {
            for (sp, sq) in [(1, 1), (-1, 1), (1, -1), (-1, -1)] {
                assert_eq!(g(&(p * &b(sp)), &(q * &b(sq))), c);
            }
        }
    }
    for (x, y, e) in [(0, 0, 0), (1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, -1, 0), (7, 1, 7), (7, -1, 7), (-23, 15, 345), (120, 84, 840), (84, -120, 840)] {
        assert_eq!(l(&b(x), &b(y)), b(e));
    }
    assert_eq!(l(&s("1216342683557601535506311712"), &s("436522681849110124616458784")), s("16592536571065866494401400422922201534178938447014944"));
    let x = s("43461045657039990237");
    let y = s("10645022458251153277");
    for c in [s("652560"), s("57655923087165495981")] {
        let a = &x * &c;
        let bb = &y * &c;
        let d = &(&x * &y) * &c;
        assert_eq!(l(&a, &bb), d);
        assert_eq!(l(&(-&a), &bb), d);
        assert_eq!(l(&bb, &(-&a)), d);
        assert_eq!(l(&(-&a), &(-&bb)), d);
    }
}

#[test]
fn factorial_comb_perm_errors() {
    let f = |v: &BigInt| math::factorial(v.as_ref());
    assert_eq!(f(&b(0)).unwrap(), b(1));
    assert_eq!(f(&b(-1)), Err(IntError::FactorialNegative));
    assert_eq!(f(&(-&BigInt::from_i64(10).pow_u64(1000).unwrap())), Err(IntError::FactorialNegative));
    assert_eq!(f(&BigInt::from_i64(10).pow_u64(100).unwrap()), Err(IntError::FactorialTooLarge));
    let mut total = b(1);
    for i in 1..1000 {
        total = &total * &b(i);
        assert_eq!(f(&b(i)).unwrap(), total);
    }
    let n = pow2(1000);
    let perm = |n: &BigInt, k: &BigInt| math::perm(n.as_ref(), k.as_ref());
    let comb = |n: &BigInt, k: &BigInt| math::comb(n.as_ref(), k.as_ref());
    assert_eq!(perm(&b(1), &b(2)).unwrap(), b(0));
    assert_eq!(perm(&b(1), &n).unwrap(), b(0));
    assert_eq!(perm(&n, &b(0)).unwrap(), b(1));
    assert_eq!(perm(&n, &b(1)).unwrap(), n);
    assert_eq!(perm(&n, &b(2)).unwrap(), &n * &(&n - &b(1)));
    assert_eq!(perm(&n, &n), Err(IntError::PermKTooLarge));
    assert_eq!(perm(&b(-1), &b(1)), Err(IntError::NNegative));
    assert_eq!(perm(&(-&n), &b(1)), Err(IntError::NNegative));
    assert_eq!(perm(&b(1), &b(-1)), Err(IntError::KNegative));
    assert_eq!(comb(&b(1), &b(2)).unwrap(), b(0));
    assert_eq!(comb(&b(1), &n).unwrap(), b(0));
    assert_eq!(comb(&n, &b(0)).unwrap(), b(1));
    assert_eq!(comb(&n, &b(1)).unwrap(), n);
    let c2 = &(&n * &(&n - &b(1))) / &b(2);
    assert_eq!(comb(&n, &b(2)).unwrap(), c2);
    assert_eq!(comb(&n, &n).unwrap(), b(1));
    assert_eq!(comb(&n, &(&n - &b(1))).unwrap(), n);
    assert_eq!(comb(&n, &(&n - &b(2))).unwrap(), c2);
    assert_eq!(comb(&n, &(&n >> 1)), Err(IntError::CombKTooLarge));
    for (nn, kk) in [(1, 1), (1, 0), (0, 0)] {
        assert_eq!(comb(&b(nn), &b(kk)).unwrap(), b(1));
        assert_eq!(perm(&b(nn), &b(kk)).unwrap(), b(1));
    }
    assert_eq!(comb(&b(5), &b(2)).unwrap(), b(10));
    assert_eq!(perm(&b(5), &b(2)).unwrap(), b(20));
    // test_math: perm(n, k) == factorial(n) / factorial(n-k) for small n
    for nn in 0..60i128 {
        for kk in 0..=nn {
            let fnn = f(&b(nn)).unwrap();
            let fk = f(&b(kk)).unwrap();
            let fnk = f(&b(nn - kk)).unwrap();
            assert_eq!(perm(&b(nn), &b(kk)).unwrap(), &fnn / &fnk);
            assert_eq!(comb(&b(nn), &b(kk)).unwrap(), &(&fnn / &fk) / &fnk);
        }
    }
}

#[test]
fn bit_length_and_count() {
    for x in -65000i128..65000 {
        let k = b(x).bit_length();
        let a = x.unsigned_abs();
        assert_eq!(k, 128 - a.leading_zeros() as u64);
        assert_eq!(b(x).bit_count(), a.count_ones() as u64);
    }
    for i in [2u64, 3, 15, 16, 17, 31, 32, 33, 63, 64, 234] {
        let a = pow2(i);
        let one = b(1);
        assert_eq!((&a - &one).bit_length(), i);
        assert_eq!((&one - &a).bit_length(), i);
        assert_eq!(a.bit_length(), i + 1);
        assert_eq!((-&a).bit_length(), i + 1);
        assert_eq!((&a + &one).bit_length(), i + 1);
        assert_eq!((&(-&a) - &one).bit_length(), i + 1);
    }
    for exp in [10u64, 17, 63, 64, 65, 1009, 70234, 1234567] {
        let a = pow2(exp);
        assert_eq!(a.bit_count(), 1);
        assert_eq!((&a - &b(1)).bit_count(), exp);
        assert_eq!((&a ^ &b(63)).bit_count(), 7);
        assert_eq!((&(&a - &b(1)) ^ &b(510)).bit_count(), exp - 8);
    }
}

#[test]
fn round_negative_ndigits() {
    let r = |x: i128, n: u64| ops::round_neg_ndigits(b(x).as_ref(), n);
    let table = [(0, 0), (1, 0), (2, 0), (3, 0), (4, 0), (5, 0), (6, 10), (7, 10), (8, 10), (9, 10), (10, 10), (11, 10), (12, 10), (13, 10), (14, 10), (15, 20), (16, 20), (17, 20), (18, 20), (19, 20)];
    for offset in (-520i128..520).step_by(20) {
        for (k, v) in table {
            assert_eq!(r(k + offset, 1), b(v + offset));
        }
    }
    for (x, n, e) in [(-150, 2, -200), (-149, 2, -100), (-51, 2, -100), (-50, 2, 0), (-49, 2, 0), (-1, 2, 0), (0, 2, 0), (1, 2, 0), (49, 2, 0), (50, 2, 0), (51, 2, 100), (149, 2, 100), (150, 2, 200), (250, 2, 200), (251, 2, 300), (172500, 3, 172000), (173500, 3, 174000)] {
        assert_eq!(r(x, n), b(e), "round({x}, -{n})");
    }
    let pi = 31415926535i128;
    let expect = [31415926540i128, 31415926500, 31415927000, 31415930000, 31415900000, 31416000000, 31420000000, 31400000000, 31000000000, 30000000000, 0, 0];
    for (i, e) in expect.iter().enumerate() {
        assert_eq!(r(pi, i as u64 + 1), b(*e));
    }
    assert_eq!(r(pi, 999), b(0));
    for k in 10..100u64 {
        let x = &BigInt::from_i64(10).pow_u64(k).unwrap() + &b(324678);
        assert_eq!(ops::round_neg_ndigits(x.as_ref(), 3), &BigInt::from_i64(10).pow_u64(k).unwrap() + &b(325000));
    }
}

#[test]
fn hash_spec() {
    let m = (1i128 << 61) - 1;
    assert_eq!(b(-1).hash(), -2);
    assert_eq!(b(0).hash(), 0);
    assert_eq!(b(10).hash(), 10);
    assert_eq!(b(m - 2).hash(), (m - 2) as i64);
    assert_eq!(b(m - 1).hash(), (m - 1) as i64);
    assert_eq!(b(m).hash(), 0);
    assert_eq!(b(m + 1).hash(), 1);
    assert_eq!(b(-m - 2).hash(), -2);
    assert_eq!(b(-m - 1).hash(), -2);
    assert_eq!(b(-m).hash(), 0);
    assert_eq!(b(-m + 1).hash(), (-m + 1) as i64);
    assert_eq!(conv::hash_i64(-1), -2);
    assert_eq!(conv::hash_i64(i64::MIN), b(i64::MIN as i128).hash());
}

#[test]
fn pow_spec() {
    // test_pow.powtest-like checks
    for i in -1000i128..1000 {
        assert_eq!(b(i).pow_u64(0).unwrap(), b(1));
        assert_eq!(b(i).pow_u64(1).unwrap(), b(i));
    }
    assert_eq!(b(0).pow_u64(1).unwrap(), b(0));
    assert_eq!(b(1).pow_u64(1).unwrap(), b(1));
    for i in -3i128..3 {
        let mut expect = b(1);
        for j in 0..20u64 {
            assert_eq!(b(i).pow_u64(j).unwrap(), expect);
            expect = &expect * &b(i);
        }
    }
    // pow(i, j, k) consistency
    for i in -10i128..10 {
        for j in 0..7u64 {
            for k in -7i128..8 {
                if k == 0 {
                    continue;
                }
                let full = b(i).pow_u64(j).unwrap();
                assert_eq!(b(i).pow_mod(&b(j as i128), &b(k)).unwrap(), full.modulo(&b(k)).unwrap(), "pow({i},{j},{k})");
            }
        }
    }
    // test_bug705231: (-1) ** huge
    let eps = b(-1);
    assert_eq!(eps.pow(&pow2(1000)).unwrap(), b(1));
    assert_eq!(eps.pow(&(&pow2(1000) + &b(1))).unwrap(), b(-1));
    // test_negative_exponent
    for a in -50i128..50 {
        for m in -50i128..50 {
            if m == 0 {
                continue;
            }
            let res = b(a).pow_mod(&b(-1), &b(m));
            let g = math::gcd(b(a).as_ref(), b(m).as_ref());
            if g == b(1) {
                let inv = res.unwrap();
                assert_eq!(inv, inv.modulo(&b(m)).unwrap());
                assert_eq!((&inv * &b(a)).modulo(&b(m)).unwrap(), b(1).modulo(&b(m)).unwrap(), "a={a} m={m}");
                let inv2 = b(a).pow_mod(&b(-2), &b(m)).unwrap();
                assert_eq!(inv2, (&inv * &inv).modulo(&b(m)).unwrap());
            } else {
                assert_eq!(res, Err(IntError::NotInvertible), "a={a} m={m}");
            }
        }
    }
    // test_big_exp: pow(2, 50000, 1<<64 + 1) via both paths
    let m = &pow2(64) + &b(1);
    let full = b(2).pow_u64(50000).unwrap().modulo(&m).unwrap();
    assert_eq!(b(2).pow_mod(&b(50000), &m).unwrap(), full);
}

#[test]
fn int_parsing_spec() {
    use rpy_bigint::radix::parse_int;
    let v = |s: &str, base: u32| parse_int(s.as_bytes(), base, 0).unwrap();
    let cases = [
        ("100000000000000000000000000000000", 2), ("102002022201221111211", 3), ("10000000000000000", 4), ("32244002423141", 5),
        ("1550104015504", 6), ("211301422354", 7), ("40000000000", 8), ("12068657454", 9), ("4294967296", 10), ("1904440554", 11),
        ("9ba461594", 12), ("535a79889", 13), ("2ca5b7464", 14), ("1a20dcd81", 15), ("100000000", 16), ("a7ffda91", 17),
        ("704he7g4", 18), ("4f5aff66", 19), ("3723ai4g", 20), ("281d55i4", 21), ("1fj8b184", 22), ("1606k7ic", 23),
        ("mb994ag", 24), ("hek2mgl", 25), ("dnchbnm", 26), ("b28jpdm", 27), ("8pfgih4", 28), ("76beigg", 29),
        ("5qmcpqg", 30), ("4q0jto4", 31), ("4000000", 32), ("3aokq94", 33), ("2qhxjli", 34), ("2br45qb", 35), ("1z141z4", 36),
    ];
    for (s, base) in cases {
        assert_eq!(v(s, base), b(4294967296), "{s} base {base}");
        assert_eq!(b(4294967296).to_str_radix(base), s);
    }
    assert_eq!(v("1_0_1_0_1_0_1_0_1_0_1_0_1_0_1_0_1_0_1_0_1_0_1_0_1_0_1_0_1_0_1", 2), b(0b1010101010101010101010101010101));
    assert_eq!(v("1_2_3_4_5_6_7_0_1_2_3", 8), b(0o12345670123));
    assert_eq!(v("1_2_3_4_5_6_7_8_9", 16), b(0x123456789));
    assert_eq!(v("9223372036854775807", 10), b(i64::MAX as i128));
    assert_eq!(v("-9223372036854775808", 10), b(i64::MIN as i128));
    assert_eq!(v(&"1".repeat(600), 10).to_string(), "1".repeat(600));
    // large decimal through the subquadratic path
    let p = pow2(100_000);
    let sp = p.to_string();
    assert_eq!(v(&sp, 10), p);
    assert_eq!(v(&format!(" -{sp}"), 10), -&p);
    assert_eq!(v(&format!(" +{sp} "), 10), p);
    assert!(parse_int(format!("{sp}z").as_bytes(), 10, 0).is_err());
    assert!(parse_int(format!("{sp}_").as_bytes(), 10, 0).is_err());
    assert!(parse_int(format!("_{sp}").as_bytes(), 10, 0).is_err());
}
