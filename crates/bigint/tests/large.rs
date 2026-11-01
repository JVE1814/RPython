//! Large-operand tests: algorithm cross-checks and algebraic identities.
mod common;
use common::*;
use rpy_bigint::{div, math, mul, nat, ops, radix, BigInt, IntError};

#[test]
fn mul_algorithms_agree() {
    let mut r = Rng::new(11);
    let sizes = [1usize, 2, 3, 15, 16, 17, 30, 47, 48, 49, 63, 64, 65, 100, 159, 160, 161, 250, 333, 500, 777, 1000];
    for &n in &sizes {
        for &m in &sizes {
            if m > n {
                continue;
            }
            let a = r.mag(n);
            let bb = r.mag(m);
            let reference = mul::mul_schoolbook(&a, &bb);
            assert_eq!(mul::mul_recursive(&a, &bb), reference, "rec {n}x{m}");
            assert_eq!(mul::mul_recursive(&bb, &a), reference, "rec {m}x{n}");
            assert_eq!(nat::mul(&a, &bb), reference, "dispatch {n}x{m}");
        }
        let a = r.mag(n);
        let reference = mul::mul_schoolbook(&a, &a);
        assert_eq!(nat::sqr(&a), reference, "sqr {n}");
        assert_eq!(mul::sqr_recursive(&a), reference, "sqr rec {n}");
    }
}

#[test]
fn mul_ntt_agrees() {
    let mut r = Rng::new(12);
    for &(n, m) in &[(1600usize, 1600usize), (1700, 1601), (2048, 2048), (3000, 1700), (4096, 4000), (2500, 2500)] {
        let a = r.mag(n);
        let bb = r.mag(m);
        let reference = mul::mul_recursive(&a, &bb);
        assert_eq!(nat::mul(&a, &bb), reference, "ntt {n}x{m}");
        let sq = mul::sqr_recursive(&a);
        assert_eq!(nat::sqr(&a), sq, "ntt sqr {n}");
    }
    // all-ones extreme operands (max convolution values)
    let a = vec![rpy_bigint::MASK; 5000];
    assert_eq!(nat::mul(&a, &a), mul::mul_recursive(&a, &a));
}

fn check_divrem(a: &[u32], bb: &[u32]) {
    let (q, rem) = nat::divrem(a, bb);
    assert_eq!(nat::cmp(&rem, bb), core::cmp::Ordering::Less, "remainder too big");
    let mut back = nat::mul(&q, bb);
    nat::add_in_place(&mut back, &rem);
    assert_eq!(back, a, "a != q*b + r ({} / {})", a.len(), bb.len());
}

#[test]
fn division_identities() {
    let mut r = Rng::new(13);
    let sizes = [1usize, 2, 3, 5, 10, 31, 64, 100, 200, 399, 400, 401, 500, 800, 1300];
    for &n in &sizes {
        for &m in &sizes {
            if m > n {
                continue;
            }
            for _ in 0..2 {
                let a = r.mag(n);
                let bb = r.mag(m);
                check_divrem(&a, &bb);
                // knuth vs dispatch
                assert_eq!(nat::divrem(&a, &bb), div::divrem_knuth(&a, &bb));
            }
        }
    }
    // (a*b)/b == a with large operands, using the Newton path
    for &(n, m) in &[(500usize, 450usize), (1200, 800), (3000, 1000), (2000, 2000), (5000, 401)] {
        let a = r.mag(n);
        let bb = r.mag(m);
        let p = nat::mul(&a, &bb);
        let (q, rem) = nat::divrem(&p, &bb);
        assert!(rem.is_empty());
        assert_eq!(q, a);
        let mut p1 = p.clone();
        nat::add_in_place(&mut p1, &nat::sub(&bb, &[1]));
        let (q, rem) = nat::divrem(&p1, &bb);
        assert_eq!(q, a);
        assert_eq!(rem, nat::sub(&bb, &[1]));
        check_divrem(&p1, &bb);
        check_divrem(&p1, &a);
    }
    // Divisor reuse
    let d = r.mag(700);
    let dv = div::Divisor::new(&d);
    for n in [1usize, 300, 699, 700, 701, 1400, 2100, 5000] {
        let a = r.mag(n);
        assert_eq!(dv.divrem(&a), div::divrem_knuth(&a, &d), "Divisor {n}");
    }
    // Short-quotient path
    for &(n, m) in &[(1000usize, 900usize), (2000, 1900), (1500, 1450)] {
        let a = r.mag(n);
        let bb = r.mag(m);
        assert_eq!(nat::divrem(&a, &bb), div::divrem_knuth(&a, &bb));
    }
    // Edge: divisor with top digit 1 and many MASK digits
    let mut bb = vec![rpy_bigint::MASK; 600];
    bb.push(1);
    let a = r.mag(1500);
    assert_eq!(nat::divrem(&a, &bb), div::divrem_knuth(&a, &bb));
    let a = vec![rpy_bigint::MASK; 1500];
    assert_eq!(nat::divrem(&a, &bb), div::divrem_knuth(&a, &bb));
}

#[test]
fn signed_division_large() {
    let mut r = Rng::new(14);
    for _ in 0..200 {
        let n = 1 + r.below(60) as usize;
        let m = 1 + r.below(60) as usize;
        let x = r.big(n);
        let y = r.big(m);
        let (q, md) = x.divmod(&y).unwrap();
        assert_eq!(&(&q * &y) + &md, x);
        if y.is_negative() {
            assert!(md <= BigInt::zero() && md > y);
        } else {
            assert!(md >= BigInt::zero() && md < y);
        }
        assert_eq!(x.floordiv(&y).unwrap(), q);
        assert_eq!(x.modulo(&y).unwrap(), md);
        // check_division from test_long: multiplication commutes
        assert_eq!(&x * &y, &y * &x);
    }
}

#[test]
fn bit_identities_large() {
    // test_long.test_bitop_identities
    let mut r = Rng::new(15);
    for _ in 0..300 {
        let x = r.big_upto(10);
        let y = r.big_upto(10);
        let z = r.big_upto(10);
        assert_eq!(&x & &x, x);
        assert_eq!(&x | &x, x);
        assert_eq!(&x ^ &x, BigInt::zero());
        assert_eq!(&x & &(!&x), BigInt::zero());
        assert_eq!(&x | &(!&x), BigInt::from_i64(-1));
        assert_eq!(&x ^ &(!&x), BigInt::from_i64(-1));
        assert_eq!(-&x, &(!&x) + &BigInt::from_i64(1));
        assert_eq!(&x & &y, &y & &x);
        assert_eq!(&x | &y, &y | &x);
        assert_eq!(&x ^ &y, &y ^ &x);
        assert_eq!(&(&x ^ &y) ^ &x, y);
        assert_eq!(&x & &(&y | &z), &(&x & &y) | &(&x & &z));
        assert_eq!(&x | &(&y & &z), &(&x | &y) & &(&x | &z));
        assert_eq!(&x ^ &(&y ^ &z), &(&x ^ &y) ^ &z);
        assert_eq!(&x & &y, !&(&(!&x) | &(!&y)));
        assert_eq!(&x | &y, !&(&(!&x) & &(!&y)));
        assert_eq!(&(&x ^ &y), &(&(&x | &y) & &(!&(&x & &y))));
        for p in 0..(10 * 30) as u64 {
            let p2 = BigInt::from_i64(1).shl(p).unwrap();
            assert_eq!(&(&x << p) >> p, x);
            assert_eq!(x.floordiv(&p2).unwrap(), &x >> p);
            assert_eq!(&x * &p2, &x << p);
            assert_eq!(&x & &(&p2 - &BigInt::from_i64(1)), x.modulo(&p2).unwrap());
            if p > 280 {
                break;
            }
        }
    }
}

fn naive_to_decimal(m: &[u32]) -> String {
    if m.is_empty() {
        return "0".into();
    }
    let mut v = m.to_vec();
    let mut parts = Vec::new();
    while !v.is_empty() {
        let r = nat::div_u32_in_place(&mut v, 1_000_000_000);
        parts.push(r);
    }
    let mut s = parts.last().unwrap().to_string();
    for p in parts.iter().rev().skip(1) {
        s.push_str(&format!("{:09}", p));
    }
    s
}

#[test]
fn radix_roundtrip() {
    let mut r = Rng::new(16);
    for radix in 2..=36u32 {
        for n in [1usize, 2, 3, 10, 50, 121, 300, 1000] {
            let x = r.big(n);
            let s = x.to_str_radix(radix);
            let back = rpy_bigint::radix::parse_int(s.as_bytes(), radix, 0).unwrap();
            assert_eq!(back, x, "radix {radix} n {n}");
            if !x.is_negative() {
                assert_eq!(BigInt::from_str_radix(&s, radix).unwrap(), x);
            }
            // uppercase accepted
            let up = s.to_uppercase();
            assert_eq!(radix::parse_int(up.as_bytes(), radix, 0).unwrap(), x);
        }
    }
    // decimal: D&C vs naive
    for n in [100usize, 119, 120, 121, 200, 500, 1000, 2000, 5000, 12000] {
        let x = r.mag(n);
        let s = radix::to_string(rpy_bigint::IntRef::new(false, &x), 10);
        assert_eq!(s, naive_to_decimal(&x), "decimal n={n}");
        let back = radix::from_digits(s.as_bytes(), 10).unwrap();
        assert_eq!(back, x, "parse n={n}");
    }
    // other radices large
    for radix in [3u32, 7, 10, 12, 36] {
        let x = r.mag(3000);
        let s = radix::to_string(rpy_bigint::IntRef::new(false, &x), radix);
        assert_eq!(radix::from_digits(s.as_bytes(), radix).unwrap(), x);
    }
}

#[test]
fn known_decimal_values() {
    // test_int.PyLongModuleTests
    let one = BigInt::from_i64(1);
    let p = one.shl(100_000).unwrap();
    let s = p.to_string();
    assert!(s.ends_with("9883109376"));
    assert_eq!(s.len(), 30103);
    let pm1 = &p - &one;
    assert!(pm1.to_string().ends_with("9883109375"));
    let ten = BigInt::from_i64(10).pow_u64(30_000).unwrap();
    let ts = ten.to_string();
    assert_eq!(ts.len(), 30001);
    assert!(ts.starts_with("10000") && ts.ends_with("0000000000"));
    assert!((&ten - &one).to_string().ends_with("9999999999"));
    assert_eq!((&ten - &one).to_string(), "9".repeat(30000));
    assert!(BigInt::from_i64(3).pow_u64(60_000).unwrap().to_string().ends_with("9313200001"));
    assert_eq!((-&p).to_string(), format!("-{}", s));
    // roundtrip
    let v: BigInt = s.parse().unwrap();
    assert_eq!(v, p);
    // divmod(n*3 + 1, n)
    let (q, m) = (&(&p * &BigInt::from_i64(3)) + &one).divmod(&p).unwrap();
    assert_eq!(q, BigInt::from_i64(3));
    assert_eq!(m, one);
}

#[test]
fn parse_int_rules() {
    use rpy_bigint::radix::parse_int;
    let ok = |s: &str, base: u32| parse_int(s.as_bytes(), base, 0).map(|v| v.to_string());
    let bad = |s: &str, base: u32| {
        assert_eq!(parse_int(s.as_bytes(), base, 0), Err(IntError::InvalidLiteral { base }), "{s:?} base {base}")
    };
    assert_eq!(ok("-3", 10).unwrap(), "-3");
    assert_eq!(ok(" -3 ", 10).unwrap(), "-3");
    assert_eq!(ok("  \t\t  314  \t\t  ", 10).unwrap(), "314");
    assert_eq!(ok("10", 16).unwrap(), "16");
    assert_eq!(ok("0o123", 0).unwrap(), "83");
    assert_eq!(ok("0x123", 16).unwrap(), "291");
    assert_eq!(ok(" 0O123   ", 0).unwrap(), "83");
    assert_eq!(ok(" 0X123  ", 0).unwrap(), "291");
    assert_eq!(ok(" 0B100 ", 0).unwrap(), "4");
    assert_eq!(ok("000", 0).unwrap(), "0");
    assert_eq!(ok("0123", 10).unwrap(), "123");
    assert_eq!(ok("0_0_0", 0).unwrap(), "0");
    assert_eq!(ok("0b_0", 0).unwrap(), "0");
    assert_eq!(ok("0x_f", 0).unwrap(), "15");
    assert_eq!(ok("0o_5", 0).unwrap(), "5");
    assert_eq!(ok("1_00", 3).unwrap(), "9");
    assert_eq!(ok("0_100", 10).unwrap(), "100");
    assert_eq!(ok("1_0000_0000", 0).unwrap(), "100000000");
    assert_eq!(ok("0xffff_ffff", 0).unwrap(), "4294967295");
    assert_eq!(ok("1_2_3_4_5_6_7", 32).unwrap(), "1144132807");
    assert_eq!(ok("100000000000000000000000000000001", 2).unwrap(), "4294967297");
    assert_eq!(ok("1z141z5", 36).unwrap(), "4294967297");
    assert_eq!(ok("+0", 5).unwrap(), "0");
    for s in ["", " ", "  \t\t  ", "  1x", "  1\x02  ", "+", "-", "- 1", "+ 1", " + 1 ", "_100", "+_100", "1__00", "100_", "123\x00", "  123 456  ", "\u{bd}", "123\u{bd}"] {
        bad(s, 10);
    }
    for s in ["0x", "0o", "0b", "010", "0_7", "09_99", "0_", "42_", "0x_", "0b1_", "0xf_", "0o5_", "0_b0", "0_xf", "0_o5", "4_______2", "0b1001__0100", "0xffff__ffff", "0x___", "0o5__77"] {
        bad(s, 0);
    }
    for (s, b) in [("0x", 16), ("0o", 8), ("0b", 2), ("0b2", 2), ("0b02", 2), ("0B2", 2), ("0o8", 8), ("0o08", 8), ("0xg", 16), ("0x0g", 16), ("0X0g", 16), ("123\x00 245", 20), ("123\x00245", 16), ("0x1", 10)] {
        bad(s, b);
    }
    assert_eq!(parse_int(b"0", 1, 0), Err(IntError::InvalidBase));
    assert_eq!(parse_int(b"0", 37, 0), Err(IntError::InvalidBase));
    for base in 2..=36 {
        assert_eq!(ok("0", base).unwrap(), "0");
    }
    // max str digits
    let lim = 2048;
    let s = "1".repeat(lim);
    assert!(parse_int(s.as_bytes(), 10, lim).is_ok());
    assert!(parse_int(format!(" -{} ", s).as_bytes(), 10, lim).is_ok());
    let s1 = "1".repeat(lim + 1);
    assert_eq!(parse_int(s1.as_bytes(), 10, lim), Err(IntError::StrToIntDigitLimit { digits: lim + 1, limit: lim }));
    assert!(parse_int(s1.as_bytes(), 16, lim).is_ok());
    let triples = lim / 3;
    let su = "1_11".repeat(triples);
    assert!(parse_int(su.as_bytes(), 10, lim).is_ok());
    assert!(parse_int(format!("{}_111", su).as_bytes(), 10, lim).is_err());
    let big = BigInt::from_i64(10).pow_u64(lim as u64 - 1).unwrap();
    assert_eq!(radix::to_decimal_string_limited(big.as_ref(), lim).unwrap().len(), lim);
    let big = BigInt::from_i64(10).pow_u64(lim as u64).unwrap();
    assert_eq!(radix::to_decimal_string_limited(big.as_ref(), lim), Err(IntError::IntToStrDigitLimit { limit: lim }));
    assert_eq!(radix::to_decimal_string_limited((-&big).as_ref(), lim + 1).unwrap().len(), lim + 2);
}

#[test]
fn bytes_large() {
    use rpy_bigint::conv::{from_bytes, to_bytes};
    let mut r = Rng::new(17);
    for _ in 0..300 {
        let x = r.big_upto(20);
        let signed = x.is_negative() || r.bool();
        let little = r.bool();
        let len = rpy_bigint::conv::byte_length(x.as_ref(), signed);
        let by = to_bytes(x.as_ref(), len, little, signed).unwrap();
        assert_eq!(from_bytes(&by, little, signed), x);
        if len > 0 {
            assert_eq!(to_bytes(x.as_ref(), len - 1, little, signed), Err(IntError::IntTooBig));
        }
        let by2 = to_bytes(x.as_ref(), len + 3, little, signed).unwrap();
        assert_eq!(from_bytes(&by2, little, signed), x);
    }
    // test_long.test_to_bytes error cases
    let tb = |v: i128, l: usize, signed: bool| to_bytes(b(v).as_ref(), l, false, signed);
    assert_eq!(tb(256, 1, false), Err(IntError::IntTooBig));
    assert_eq!(tb(128, 1, true), Err(IntError::IntTooBig));
    assert_eq!(tb(-129, 1, true), Err(IntError::IntTooBig));
    assert_eq!(tb(-1, 2, false), Err(IntError::NegativeToUnsigned));
    assert_eq!(tb(1, 0, false), Err(IntError::IntTooBig));
    assert_eq!(tb(-1, 0, true), Err(IntError::IntTooBig));
    assert_eq!(tb(0, 0, true).unwrap(), Vec::<u8>::new());
    assert_eq!(tb(0, 0, false).unwrap(), Vec::<u8>::new());
    assert_eq!(tb(-128, 1, true).unwrap(), vec![0x80]);
    assert_eq!(tb(127, 1, true).unwrap(), vec![0x7f]);
    assert_eq!(tb(255, 1, false).unwrap(), vec![0xff]);
    assert_eq!(tb(-1, 1, true).unwrap(), vec![0xff]);
    assert_eq!(tb(-1, 3, true).unwrap(), vec![0xff, 0xff, 0xff]);
    assert_eq!(tb(1, 3, true).unwrap(), vec![0, 0, 1]);
    assert_eq!(tb(-65536, 3, true).unwrap(), vec![0xff, 0, 0]);
    assert_eq!(tb(-8388608, 3, true).unwrap(), vec![0x80, 0, 0]);
    assert_eq!(to_bytes(b(-129).as_ref(), 2, true, true).unwrap(), vec![0x7f, 0xff]);
    assert_eq!(from_bytes(&[0xff, 0xff], false, true), b(-1));
    assert_eq!(from_bytes(&[0x80, 0x00], false, true), b(-32768));
    assert_eq!(from_bytes(&[0x00, 0x80], true, true), b(-32768));
    assert_eq!(from_bytes(&[0x80, 0x00], false, false), b(32768));
    assert_eq!(from_bytes(&[], false, true), b(0));
    assert_eq!(from_bytes(&[0, 0, 0, 1], false, true), b(1));
    assert_eq!(from_bytes(&[0xff, 0, 0, 0], false, true), b(-16777216));
}

/// Check that f is the correctly rounded (half-even) value of a/b (a, b > 0).
fn check_rounding(a: &BigInt, bb: &BigInt, f: f64) {
    assert!(f.is_finite() && f >= 0.0);
    if f == 0.0 {
        // a/b <= 2^-1075
        let lhs = a.shl(1075).unwrap();
        assert!(lhs <= *bb, "rounded to zero incorrectly");
        return;
    }
    let bits = f.to_bits();
    let e = ((bits >> 52) & 0x7ff) as i64;
    let (m, ex) = if e == 0 { (bits & ((1 << 52) - 1), -1074) } else { ((bits & ((1 << 52) - 1)) | (1 << 52), e - 1075) };
    // compare 2*a*2^-ex with (2m-1)*b and (2m+1)*b
    let (lhs, scale_b) = if ex < 0 { (a.shl((-ex) as u64 + 1).unwrap(), bb.clone()) } else { (a.shl(1).unwrap(), bb.shl(ex as u64).unwrap()) };
    let lo = &scale_b * &BigInt::from_u64(2 * m - 1);
    let hi = &scale_b * &BigInt::from_u64(2 * m + 1);
    assert!(lhs >= lo && lhs <= hi, "not within half ulp");
    if lhs == lo || lhs == hi {
        assert!(m & 1 == 0, "tie not rounded to even");
    }
}

#[test]
fn true_division_rounding() {
    let mut r = Rng::new(18);
    for _ in 0..3000 {
        let a = BigInt::from_parts(false, r.mag_upto(40));
        let bb = BigInt::from_parts(false, r.mag_upto(40));
        match a.truediv(&bb) {
            Ok(f) => check_rounding(&a, &bb, f),
            Err(e) => {
                assert_eq!(e, IntError::TrueDivOverflow);
                // a/b >= DBL_MAX + half ulp
                assert!(a.bit_length() > bb.bit_length() + 1000);
            }
        }
    }
    // test_long cases
    let one = BigInt::from_i64(1);
    let huge = one.shl(40000).unwrap();
    let mhuge = -&huge;
    assert_eq!(huge.truediv(&huge).unwrap(), 1.0);
    assert_eq!(mhuge.truediv(&huge).unwrap(), -1.0);
    assert_eq!(one.truediv(&huge).unwrap(), 0.0);
    assert!(one.truediv(&mhuge).unwrap().is_sign_negative());
    let x = &(&BigInt::from_i64(666) * &huge) + &(&huge >> 1);
    assert_eq!(x.truediv(&huge).unwrap(), 666.5);
    assert_eq!(x.truediv(&mhuge).unwrap(), -666.5);
    assert_eq!(huge.truediv(&huge.shl(1).unwrap()).unwrap(), 0.5);
    assert_eq!((&BigInt::from_i64(1000000) * &huge).truediv(&huge).unwrap(), 1000000.0);
    assert_eq!(huge.truediv(&b(1)), Err(IntError::TrueDivOverflow));
    assert_eq!(mhuge.truediv(&b(200)), Err(IntError::TrueDivOverflow));
    // overflow corner: DBL_MIN_OVERFLOW = 2^1024 - 2^970
    let dmo = &one.shl(1024).unwrap() - &one.shl(970).unwrap();
    for m in [1i128, 2, 7, 17, 12345, -1, -2, -23] {
        for n in -10i128..10 {
            let num = &(&dmo * &b(m)) + &b(n);
            let got = num.truediv(&b(m));
            let expect_overflow = num.abs() >= (&dmo * &b(m)).abs();
            assert_eq!(got.is_err(), expect_overflow, "m={m} n={n}");
            if let Ok(f) = got {
                check_rounding(&num.abs(), &b(m).abs(), f.abs());
            }
        }
    }
    // inexactness detection in the shifting stage
    let m53 = one.shl(53).unwrap();
    for n in 0..250u64 {
        let a = &(&(&(&m53 + &one) * &b(12345)) * &one.shl(200).unwrap()) + &one.shl(n).unwrap();
        let bb = &m53 * &b(12345);
        check_rounding(&a, &bb, a.truediv(&bb).unwrap());
    }
    check_rounding(&b(1), &b(2731), b(1).truediv(&b(2731)).unwrap());
    let a = s("295147931372582273023");
    let bb = s("295147932265116303360");
    check_rounding(&a, &bb, a.truediv(&bb).unwrap());
    for i in 0..300u64 {
        let p = BigInt::from_i64(10).pow_u64(i).unwrap();
        let p1 = BigInt::from_i64(10).pow_u64(i + 1).unwrap();
        check_rounding(&p1, &p, p1.truediv(&p).unwrap());
        check_rounding(&p, &p1, p.truediv(&p1).unwrap());
    }
    // subnormal results
    let d = one.shl(1076).unwrap();
    for n in 1i128..20 {
        check_rounding(&b(n), &d, b(n).truediv(&d).unwrap());
    }
}

#[test]
fn float_conversion() {
    let one = BigInt::from_i64(1);
    let dbl_max = BigInt::from_f64(f64::MAX).unwrap();
    assert_eq!(dbl_max.to_f64().unwrap(), f64::MAX);
    assert_eq!((&dbl_max + &one).to_f64().unwrap(), f64::MAX);
    let top = one.shl(1024).unwrap();
    let halfway = (&dbl_max + &top) >> 1;
    assert_eq!((&halfway - &one).to_f64().unwrap(), f64::MAX);
    assert_eq!(halfway.to_f64(), Err(IntError::FloatOverflow));
    assert_eq!((&one - &halfway).to_f64().unwrap(), -f64::MAX);
    assert_eq!((-&halfway).to_f64(), Err(IntError::FloatOverflow));
    assert_eq!((&top - &one).to_f64(), Err(IntError::FloatOverflow));
    assert_eq!(top.to_f64(), Err(IntError::FloatOverflow));
    // round half even
    for (x, y) in [(1i128, 0i128), (2, 2), (3, 4), (4, 4), (5, 4), (6, 6), (7, 8)] {
        for p in 0..15u64 {
            let v = BigInt::from_i128((1i128 << 53) + x).shl(p).unwrap();
            let f = v.to_f64().unwrap();
            assert_eq!(BigInt::from_f64(f).unwrap(), BigInt::from_i128((1i128 << 53) + y).shl(p).unwrap());
        }
    }
    for p in 0..100u64 {
        let x = &BigInt::from_i128((1i128 << 53) + 1).shl(p).unwrap() + &one;
        let y = BigInt::from_i128((1i128 << 53) + 2).shl(p).unwrap();
        assert_eq!(BigInt::from_f64(x.to_f64().unwrap()).unwrap(), y);
        let x = BigInt::from_i128((1i128 << 53) + 1).shl(p).unwrap();
        let y = BigInt::from_i128(1i128 << 53).shl(p).unwrap();
        assert_eq!(BigInt::from_f64(x.to_f64().unwrap()).unwrap(), y);
    }
    let mut r = Rng::new(19);
    for _ in 0..2000 {
        let a = BigInt::from_parts(false, r.mag_upto(34));
        if let Ok(f) = a.to_f64() {
            check_rounding(&a, &one, f);
        } else {
            assert!(a.bit_length() >= 1024);
        }
    }
    assert_eq!(BigInt::from_f64(f64::INFINITY), Err(IntError::FloatInfinity));
    assert_eq!(BigInt::from_f64(f64::NAN), Err(IntError::FloatNaN));
    assert_eq!(BigInt::from_f64(-3.9).unwrap(), b(-3));
    assert_eq!(BigInt::from_f64(3.9).unwrap(), b(3));
    assert_eq!(BigInt::from_f64(-0.5).unwrap(), b(0));
    assert_eq!(BigInt::from_f64(1e100).unwrap().to_string(), "10000000000000000159028911097599180468360808563945281389781327557747838772170381060813469985856815104");
    // frexp / log of huge
    let h = one.shl(30000).unwrap();
    let (m, e) = rpy_bigint::conv::frexp(h.as_ref());
    assert_eq!((m, e), (0.5, 30001));
    let l = rpy_bigint::conv::ln(BigInt::from_i64(10).pow_u64(10000).unwrap().as_ref()).unwrap();
    assert!((l - 10000.0 * std::f64::consts::LN_10).abs() < 1e-9);
    // cmp with float
    use core::cmp::Ordering::*;
    let c = |v: &BigInt, f: f64| rpy_bigint::conv::cmp_f64(v.as_ref(), f);
    assert_eq!(c(&b(3), 3.5), Some(Less));
    assert_eq!(c(&b(4), 3.5), Some(Greater));
    assert_eq!(c(&b(-4), -3.5), Some(Less));
    assert_eq!(c(&b(3), 3.0), Some(Equal));
    assert_eq!(c(&b(0), -0.0), Some(Equal));
    assert_eq!(c(&b(0), 0.1), Some(Less));
    assert_eq!(c(&h, f64::INFINITY), Some(Less));
    assert_eq!(c(&h, f64::MAX), Some(Greater));
    assert_eq!(c(&dbl_max, f64::MAX), Some(Equal));
    assert_eq!(c(&(&dbl_max + &one), f64::MAX), Some(Greater));
    assert_eq!(c(&b(1), f64::NAN), None);
    let big53 = BigInt::from_i128((1 << 53) + 1);
    assert_eq!(c(&big53, ((1u64 << 53) + 1) as f64), Some(Greater));
}

#[test]
fn isqrt_gcd_factorial() {
    let mut r = Rng::new(20);
    for i in 0..2000u64 {
        let n = if i < 1000 { BigInt::from_u64(i * i + r.below(3)) } else { BigInt::from_parts(false, r.mag_upto(70)) };
        let s = n.isqrt().unwrap();
        assert!(&s * &s <= n);
        let s1 = &s + &b(1);
        assert!(&s1 * &s1 > n);
    }
    assert_eq!(b(-1).isqrt(), Err(IntError::IsqrtNegative));
    // test_math isqrt values
    for v in [0u64, 1, 2, 3, 4, 15, 16, 17, 99, 100, 101, u64::MAX, (1 << 32) - 1, 1 << 32] {
        let s = math::isqrt_u64(v) as u128;
        assert!(s * s <= v as u128 && (s + 1) * (s + 1) > v as u128);
    }
    // gcd
    for _ in 0..500 {
        let x = r.big_upto(30);
        let y = r.big_upto(30);
        let g = x.gcd(&y);
        assert!(x.modulo(&g).unwrap().is_zero());
        assert!(y.modulo(&g).unwrap().is_zero());
        let xg = x.abs().floordiv(&g).unwrap();
        let yg = y.abs().floordiv(&g).unwrap();
        assert_eq!(xg.gcd(&yg), b(1));
        // with a common factor
        let c = r.big_upto(10).abs();
        let g2 = (&x * &c).gcd(&(&y * &c));
        assert_eq!(g2, &g * &c);
        let l = math::lcm(x.as_ref(), y.as_ref());
        assert_eq!(&l * &g, (&x * &y).abs());
    }
    assert_eq!(b(0).gcd(&b(0)), b(0));
    assert_eq!(b(0).gcd(&b(-5)), b(5));
    assert_eq!(b(120).gcd(&b(84)), b(12));
    assert_eq!(b(-120).gcd(&b(84)), b(12));
    let a = s("1216451004088320000"); // 19!
    assert_eq!(a.gcd(&s("2432902008176640000")), a);
    // factorial
    let mut f = BigInt::from_i64(1);
    for n in 0..600u64 {
        if n > 0 {
            f = &f * &BigInt::from_u64(n);
        }
        assert_eq!(math::factorial(BigInt::from_u64(n).as_ref()).unwrap(), f, "{n}!");
    }
    assert_eq!(math::factorial(b(-1).as_ref()), Err(IntError::FactorialNegative));
    // comb/perm
    let comb = |n: i128, k: i128| math::comb(b(n).as_ref(), b(k).as_ref()).unwrap();
    let perm = |n: i128, k: i128| math::perm(b(n).as_ref(), b(k).as_ref()).unwrap();
    assert_eq!(comb(100, 50).to_string(), "100891344545564193334812497256");
    assert_eq!(comb(5, 7), b(0));
    assert_eq!(comb(0, 0), b(1));
    assert_eq!(perm(5, 2), b(20));
    assert_eq!(perm(5, 0), b(1));
    assert_eq!(perm(5, 6), b(0));
    for n in 0..60i128 {
        let mut row = b(1);
        for k in 0..=n {
            assert_eq!(comb(n, k), row, "comb({n},{k})");
            row = &(&row * &b(n - k)) / &b(k + 1);
        }
    }
    let big = s("100000000000000000000000000000");
    let c3 = math::comb(big.as_ref(), b(3).as_ref()).unwrap();
    let expect = &(&(&big * &(&big - &b(1))) * &(&big - &b(2))) / &b(6);
    assert_eq!(c3, expect);
    let p3 = math::perm(big.as_ref(), b(3).as_ref()).unwrap();
    assert_eq!(p3, &expect * &b(6));
    let c = math::comb(b(2000).as_ref(), b(1000).as_ref()).unwrap();
    let mut row = b(1);
    for k in 0..1000 {
        row = &(&row * &b(2000 - k)) / &b(k + 1);
    }
    assert_eq!(c, row);
    assert_eq!(math::comb(b(-1).as_ref(), b(1).as_ref()), Err(IntError::NNegative));
    assert_eq!(math::comb(b(1).as_ref(), b(-1).as_ref()), Err(IntError::KNegative));
}

#[test]
fn pow_large() {
    let mut r = Rng::new(21);
    for _ in 0..50 {
        let x = r.big_upto(5);
        let e = r.below(40);
        let mut expect = BigInt::from_i64(1);
        for _ in 0..e {
            expect = &expect * &x;
        }
        assert_eq!(x.pow_u64(e).unwrap(), expect);
        // modular, odd and even, multi-digit moduli
        for _ in 0..3 {
            let m = r.big_upto(8);
            let got = x.pow_mod(&BigInt::from_u64(e), &m).unwrap();
            assert_eq!(got, expect.modulo(&m).unwrap(), "pow_mod");
        }
    }
    // Fermat: a^(p-1) = 1 mod p for prime p = 2^127 - 1
    let p = &BigInt::from_i64(1).shl(127).unwrap() - &b(1);
    let pm1 = &p - &b(1);
    for a in [2i128, 3, 12345678901234567] {
        assert_eq!(b(a).pow_mod(&pm1, &p).unwrap(), b(1));
    }
    // big modulus exponentiation consistency: (a^e1)^e2 == a^(e1*e2) mod m
    let m = BigInt::from_parts(false, r.mag(500));
    let a = BigInt::from_parts(false, r.mag(400));
    let e1 = BigInt::from_parts(false, r.mag(3));
    let e2 = BigInt::from_parts(false, r.mag(2));
    let lhs = a.pow_mod(&e1, &m).unwrap().pow_mod(&e2, &m).unwrap();
    let rhs = a.pow_mod(&(&e1 * &e2), &m).unwrap();
    assert_eq!(lhs, rhs);
    let mo = &m + &b(if m.mag[0] & 1 == 0 { 1 } else { 0 });
    let lhs = a.pow_mod(&e1, &mo).unwrap().pow_mod(&e2, &mo).unwrap();
    let rhs = a.pow_mod(&(&e1 * &e2), &mo).unwrap();
    assert_eq!(lhs, rhs);
    // inverse with big modulus
    let inv = a.pow_mod(&b(-1), &mo);
    if let Ok(inv) = inv {
        assert_eq!((&inv * &a).modulo(&mo).unwrap(), b(1));
    }
    assert_eq!(ops::pow_u64(b(2).as_ref(), 1 << 50), Err(IntError::MemoryError));
}

#[test]
fn karatsuba_cases_from_test_long() {
    // test_long.test_karatsuba: (2^i - 1)(2^j - 1) for bit sizes around digit boundaries.
    let digits: Vec<u64> = (1..10).chain([24, 70, 71, 140, 141]).collect();
    let bits: Vec<u64> = digits.iter().map(|d| d * 30).collect();
    for &abits in &bits {
        let a = &BigInt::from_i64(1).shl(abits).unwrap() - &b(1);
        for &bbits in &bits {
            if bbits < abits {
                continue;
            }
            let bb = &BigInt::from_i64(1).shl(bbits).unwrap() - &b(1);
            let x = &a * &bb;
            let one = BigInt::from_i64(1);
            let y = &(&(&one.shl(abits + bbits).unwrap() - &one.shl(abits).unwrap()) - &one.shl(bbits).unwrap()) + &one;
            assert_eq!(x, y);
        }
    }
}
