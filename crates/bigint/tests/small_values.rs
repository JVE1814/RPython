//! Randomized cross-checks against i128 arithmetic for small values.
mod common;
use common::*;
use rpy_bigint::{ops, small, BigInt, IntError};

fn floordiv(a: i128, b: i128) -> i128 {
    let q = a / b;
    if (a % b != 0) && ((a < 0) != (b < 0)) { q - 1 } else { q }
}
fn floormod(a: i128, b: i128) -> i128 {
    let r = a % b;
    if r != 0 && ((r < 0) != (b < 0)) { r + b } else { r }
}

#[test]
fn add_sub_mul_cmp() {
    let mut r = Rng::new(1);
    for _ in 0..200_000 {
        let x = r.i64() as i128;
        let y = r.i64() as i128;
        let (bx, by) = (b(x), b(y));
        assert_eq!((&bx + &by).to_i128(), Some(x + y), "{x} + {y}");
        assert_eq!((&bx - &by).to_i128(), Some(x - y), "{x} - {y}");
        assert_eq!((&bx * &by).to_i128(), Some(x * y), "{x} * {y}");
        assert_eq!(bx.cmp(&by), x.cmp(&y));
        // add/sub with i128 range values
        let x2 = r.i128() / 2;
        let y2 = r.i128() / 2;
        assert_eq!((b(x2) + b(y2)).to_i128(), Some(x2 + y2));
        assert_eq!((b(x2) - b(y2)).to_i128(), Some(x2 - y2));
    }
}

#[test]
fn division() {
    let mut r = Rng::new(2);
    for _ in 0..200_000 {
        let x = r.i128();
        let mut y = if r.bool() { r.i64() as i128 } else { r.i128() };
        if y == 0 {
            y = 7;
        }
        let (bx, by) = (b(x), b(y));
        let (q, m) = bx.divmod(&by).unwrap();
        assert_eq!(q.to_i128(), Some(floordiv(x, y)), "{x} // {y}");
        assert_eq!(m.to_i128(), Some(floormod(x, y)), "{x} % {y}");
        assert_eq!(bx.floordiv(&by).unwrap(), q);
        assert_eq!(bx.modulo(&by).unwrap(), m);
    }
    let z = BigInt::zero();
    assert_eq!(b(5).divmod(&z), Err(IntError::DivisionByZero));
    assert_eq!(b(5).floordiv(&z), Err(IntError::DivisionByZero));
    assert_eq!(b(5).modulo(&z), Err(IntError::ModuloByZero));
    assert_eq!(b(5).truediv(&z), Err(IntError::TrueDivisionByZero));
    // test_long.test_mod_division / test_floordiv
    for (a, bb, m) in [(13, 10, 3), (-13, 10, 7), (13, -10, -7), (-13, -10, -3), (12, 4, 0), (-12, 4, 0), (12, -4, 0), (-12, -4, 0)] {
        assert_eq!(b(a).modulo(&b(bb)).unwrap(), b(m));
    }
    for (a, bb, q) in [(2, 3, 0), (2, -3, -1), (-2, 3, -1), (-2, -3, 0), (-11, -3, 3), (-11, 3, -4), (11, -3, -4), (11, 3, 3), (-12, -3, 4), (-12, 3, -4), (12, -3, -4), (12, 3, 4)] {
        assert_eq!(b(a).floordiv(&b(bb)).unwrap(), b(q));
    }
}

#[test]
fn bitops() {
    let mut r = Rng::new(3);
    for _ in 0..200_000 {
        let x = r.i128();
        let y = r.i128();
        let (bx, by) = (b(x), b(y));
        assert_eq!((&bx & &by).to_i128(), Some(x & y), "{x} & {y}");
        assert_eq!((&bx | &by).to_i128(), Some(x | y), "{x} | {y}");
        assert_eq!((&bx ^ &by).to_i128(), Some(x ^ y), "{x} ^ {y}");
        assert_eq!((!&bx).to_i128(), Some(!x));
        let s = r.below(70);
        let small = x >> 64;
        assert_eq!(bx.shr(s).to_i128(), Some(x >> s), "{x} >> {s}");
        let s = s.min(63);
        assert_eq!(b(small).shl(s).unwrap().to_i128(), Some(small << s), "{small} << {s}");
    }
}

#[test]
fn shifts_from_test_long() {
    const BASE: i128 = 1 << 30;
    assert_eq!(b(42).shr(1), b(21));
    assert_eq!(b(-42).shr(1), b(-21));
    assert_eq!(b(43).shr(1), b(21));
    assert_eq!(b(-43).shr(1), b(-22));
    assert_eq!(b(-1123).shr(1), b(-562));
    assert_eq!(b(i128::MIN).shr(1).to_i128(), Some(i128::MIN / 2));
    assert_eq!(b(-1122).shr(9), b(-3));
    assert_eq!(b(1 - BASE * BASE).shr(30), b(-BASE));
    assert_eq!(b(BASE - 1 - BASE * BASE).shr(30), b(-BASE));
    assert_eq!(b(-42).shr(32), b(-1));
    let huge = BigInt::from_i64(1).shl(1000).unwrap();
    assert_eq!(ops::rshift_big(b(42).as_ref(), huge.as_ref()).unwrap(), b(0));
    assert_eq!(ops::rshift_big(b(-42).as_ref(), huge.as_ref()).unwrap(), b(-1));
    assert_eq!(ops::lshift_big(b(0).as_ref(), huge.as_ref()).unwrap(), b(0));
    assert_eq!(ops::lshift_big(b(1).as_ref(), huge.as_ref()), Err(IntError::TooManyDigits));
    assert_eq!(ops::lshift_big(b(42).as_ref(), b(-3).as_ref()), Err(IntError::NegativeShiftCount));
    assert_eq!(ops::rshift_big(b(42).as_ref(), b(-3).as_ref()), Err(IntError::NegativeShiftCount));
    assert_eq!(ops::lshift_big(b(0).as_ref(), b(-1).as_ref()), Err(IntError::NegativeShiftCount));
    // huge digit count
    let sh = b(((i64::MAX as i128) - 5) * 30);
    assert_eq!(ops::lshift_big(b(1).as_ref(), sh.as_ref()), Err(IntError::TooManyDigits));
    assert_eq!(ops::lshift(b(1).as_ref(), u64::MAX), Err(IntError::MemoryError));
}

#[test]
fn conversions() {
    let mut r = Rng::new(4);
    for _ in 0..100_000 {
        let x = r.i128();
        let bx = b(x);
        assert_eq!(bx.to_i128(), Some(x));
        assert_eq!(bx.to_i64(), i64::try_from(x).ok());
        assert_eq!(bx.to_u64(), u64::try_from(x).ok());
        assert_eq!(bx.to_u128(), u128::try_from(x).ok());
        assert_eq!(bx.to_f64().unwrap(), x as f64, "float({x})");
        assert_eq!(bx.to_string(), x.to_string());
        assert_eq!(bx.bit_length(), 128 - x.unsigned_abs().leading_zeros() as u64);
        assert_eq!(bx.bit_count(), x.unsigned_abs().count_ones() as u64);
    }
    for v in [i64::MIN, i64::MAX, 0, -1, 1, 1 << 30, -(1 << 30), (1 << 60) - 1] {
        assert_eq!(BigInt::from_i64(v).to_i64(), Some(v));
    }
    assert_eq!(BigInt::from_i128(i128::MIN).to_i128(), Some(i128::MIN));
    assert_eq!(BigInt::from_u128(u128::MAX).to_u128(), Some(u128::MAX));
    assert_eq!((BigInt::from_u128(u128::MAX) + b(1)).to_u128(), None);
    assert_eq!((BigInt::from_i64(i64::MAX) + b(1)).to_i64(), None);
    assert_eq!((BigInt::from_i64(i64::MIN) - b(1)).to_i64(), None);
    assert_eq!(BigInt::from_u64(u64::MAX).to_u64(), Some(u64::MAX));
    assert_eq!(b(-1).to_u64(), None);
}

#[test]
fn i64_fast_paths() {
    let mut r = Rng::new(5);
    for _ in 0..200_000 {
        let x = r.i64();
        let y = r.i64();
        let (xi, yi) = (x as i128, y as i128);
        assert_eq!(small::add(x, y), i64::try_from(xi + yi).ok());
        assert_eq!(small::mul(x, y), i64::try_from(xi * yi).ok());
        if y != 0 {
            assert_eq!(small::floordiv(x, y), i64::try_from(floordiv(xi, yi)).ok());
            assert_eq!(small::modulo(x, y) as i128, floormod(xi, yi));
            let t = small::truediv(x, y);
            assert_eq!(t, b(xi).truediv(&b(yi)).unwrap());
        }
        let s = r.below(70);
        assert_eq!(small::rshift(x, s), (xi >> s.min(127)) as i64);
        let sm = x >> 40;
        let expect = (sm as i128) << s;
        assert_eq!(small::lshift(sm, s), i64::try_from(expect).ok(), "{sm} << {s}");
        assert_eq!(small::hash(x), b(xi).hash());
    }
    assert_eq!(small::floordiv(i64::MIN, -1), None);
    assert_eq!(small::modulo(i64::MIN, -1), 0);
    assert_eq!(small::pow(3, 39), Some(3i64.pow(39)));
    assert_eq!(small::pow(3, 40), None);
    assert_eq!(small::pow(-1, 1 << 40), Some(1));
    assert_eq!(small::pow_mod(2, 3, -5), -2);
    assert_eq!(small::pow_mod(-2, 3, 5), 2);
}

#[test]
fn hash_values() {
    let m = (1i128 << 61) - 1;
    assert_eq!(b(-1).hash(), -2);
    assert_eq!(b(-2).hash(), -2);
    assert_eq!(b(0).hash(), 0);
    assert_eq!(b(m).hash(), 0);
    assert_eq!(b(m + 1).hash(), 1);
    assert_eq!(b(-m - 1).hash(), -2);
    assert_eq!(b(-m - 2).hash(), -2);
    assert_eq!(b(m - 1).hash(), (m - 1) as i64);
    let mut r = Rng::new(6);
    for _ in 0..50_000 {
        let x = r.i128();
        let h = x.unsigned_abs() % (m as u128);
        let mut e = if x < 0 { -(h as i64) } else { h as i64 };
        if e == -1 {
            e = -2;
        }
        assert_eq!(b(x).hash(), e);
    }
    // 2**k hash cycles with period 61
    let p = BigInt::from_i64(1).shl(61 * 7 + 5).unwrap();
    assert_eq!(p.hash(), 32);
}

#[test]
fn pow_small() {
    let mut r = Rng::new(7);
    for _ in 0..20_000 {
        let x = (r.i64() % 1000) as i128;
        let e = r.below(12);
        let expect = x.pow(e as u32);
        assert_eq!(b(x).pow_u64(e).unwrap().to_i128(), Some(expect));
        let m = (r.i64() % 100000) as i128;
        if m != 0 {
            let got = b(x).pow_mod(&b(e as i128), &b(m)).unwrap();
            assert_eq!(got.to_i128(), Some(floormod(expect, m)), "pow({x},{e},{m})");
        }
    }
    // test_pow.py cases
    assert_eq!(b(2).pow_mod(&b(-1), &b(3)).unwrap(), b(2)); // inverse of 2 mod 3
    assert_eq!(b(-1).pow_mod(&b(-1), &b(5)).unwrap(), b(4));
    assert_eq!(b(4).pow_mod(&b(-1), &b(-7)).unwrap(), b(-5)); // 2 - 7
    assert_eq!(b(2).pow_mod(&b(-1), &b(4)), Err(IntError::NotInvertible));
    assert_eq!(b(0).pow_mod(&b(-1), &b(4)), Err(IntError::NotInvertible));
    assert_eq!(b(2).pow_mod(&b(3), &b(0)), Err(IntError::PowModulusZero));
    assert_eq!(b(5).pow_mod(&b(0), &b(1)).unwrap(), b(0));
    assert_eq!(b(5).pow_mod(&b(0), &b(-1)).unwrap(), b(0));
    assert_eq!(b(0).pow_mod(&b(0), &b(7)).unwrap(), b(1));
    assert_eq!(b(2).pow(&b(-1)), Err(IntError::NegativeExponent));
    assert_eq!(b(0).pow(&b(0)).unwrap(), b(1));
    assert_eq!(b(-1).pow(&BigInt::from_i64(1).shl(100).unwrap()).unwrap(), b(1));
    assert_eq!(b(-1).pow(&(BigInt::from_i64(1).shl(100).unwrap() + b(1))).unwrap(), b(-1));
    assert_eq!(b(-2).pow_u64(3).unwrap(), b(-8));
    assert_eq!(b(-2).pow_u64(64).unwrap().to_i128(), Some(1i128 << 64));
    assert_eq!(b(12).pow_u64(30).unwrap().to_i128(), Some(12i128.pow(30)));
    // test_pow negative exponent identities: pow(a, -1, m) * a == 1 mod m
    for a in -10i128..10 {
        for m in [7i128, 11, 13, 101, -7] {
            if a % m.abs() == 0 {
                continue;
            }
            let inv = b(a).pow_mod(&b(-1), &b(m)).unwrap().to_i128().unwrap();
            assert_eq!(floormod(inv * a, m.abs()), 1, "inverse of {a} mod {m}");
        }
    }
}

#[test]
fn bytes_roundtrip_small() {
    let mut r = Rng::new(8);
    for _ in 0..50_000 {
        let x = r.i128() >> 1;
        let little = r.bool();
        let bytes = rpy_bigint::conv::to_bytes(b(x).as_ref(), 16, little, true).unwrap();
        let mut expect = x.to_le_bytes().to_vec();
        if !little {
            expect.reverse();
        }
        assert_eq!(bytes, expect);
        assert_eq!(rpy_bigint::conv::from_bytes(&bytes, little, true), b(x));
    }
}

#[test]
fn true_division_small() {
    let mut r = Rng::new(9);
    for _ in 0..100_000 {
        let x = r.i64() >> 12;
        let mut y = r.i64() >> 12;
        if y == 0 {
            y = 3;
        }
        assert_eq!(b(x as i128).truediv(&b(y as i128)).unwrap(), x as f64 / y as f64);
    }
    assert!(b(0).truediv(&b(-3)).unwrap().is_sign_negative());
    assert!(b(0).truediv(&b(3)).unwrap().is_sign_positive());
}
