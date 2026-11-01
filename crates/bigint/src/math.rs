//! Number-theoretic helpers: gcd (Lehmer), lcm, isqrt, factorial, comb, perm.

use crate::nat::{self, MASK, SHIFT};
use crate::{BigInt, IntError, IntRef};
use core::cmp::Ordering;

// ---------------------------------------------------------------------------------------------
// GCD
// ---------------------------------------------------------------------------------------------

/// Binary gcd of two u64.
pub fn gcd_u64(mut a: u64, mut b: u64) -> u64 {
    if a == 0 {
        return b;
    }
    if b == 0 {
        return a;
    }
    let shift = (a | b).trailing_zeros();
    a >>= a.trailing_zeros();
    loop {
        b >>= b.trailing_zeros();
        if a > b {
            core::mem::swap(&mut a, &mut b);
        }
        b -= a;
        if b == 0 {
            return a << shift;
        }
    }
}

/// `(A*a + B*b, C*a + D*b)` for signed single-precision cofactors; results must be >= 0.
fn lin_comb(a: &[u32], b: &[u32], ca: i64, cb: i64, cc: i64, cd: i64) -> (Vec<u32>, Vec<u32>) {
    let n = a.len().max(b.len());
    let mut x = vec![0u32; n + 1];
    let mut y = vec![0u32; n + 1];
    // Cofactors are below 2^31 in magnitude and digits below 2^30: every term fits an i64.
    let mut c1: i64 = 0;
    let mut c2: i64 = 0;
    let bl = b.len();
    for i in 0..n {
        let ai = a[i] as i64;
        let bi = if i < bl { b[i] as i64 } else { 0 };
        c1 += ca * ai + cb * bi;
        c2 += cc * ai + cd * bi;
        x[i] = (c1 as u32) & MASK;
        y[i] = (c2 as u32) & MASK;
        c1 >>= SHIFT;
        c2 >>= SHIFT;
    }
    debug_assert!((0..(1i64 << SHIFT)).contains(&c1));
    debug_assert!((0..(1i64 << SHIFT)).contains(&c2));
    x[n] = c1 as u32;
    y[n] = c2 as u32;
    nat::normalize(&mut x);
    nat::normalize(&mut y);
    (x, y)
}

/// gcd of two magnitudes (Lehmer's algorithm).
pub fn gcd_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (mut a, mut b) = if nat::cmp(a, b) == Ordering::Less { (b.to_vec(), a.to_vec()) } else { (a.to_vec(), b.to_vec()) };
    loop {
        // invariant: a >= b
        if b.is_empty() {
            return a;
        }
        if let (Some(x), Some(y)) = (nat::to_u64(&a), nat::to_u64(&b)) {
            return nat::from_u64(gcd_u64(x, y));
        }
        if a.len() > b.len() + 1 {
            let r = nat::divrem(&a, &b).1;
            a = b;
            b = r;
            continue;
        }
        let nbits = nat::bit_length(&a);
        let shift = nbits.saturating_sub(62);
        let mut x = nat::extract_bits(&a, shift, 62) as i128;
        let mut y = nat::extract_bits(&b, shift, 62) as i128;
        let (mut ca, mut cb, mut cc, mut cd): (i128, i128, i128, i128) = (1, 0, 0, 1);
        loop {
            if y + cc == 0 || y + cd == 0 {
                break;
            }
            let q = (x + ca) / (y + cc);
            let q2 = (x + cb) / (y + cd);
            if q != q2 {
                break;
            }
            if (ca - q * cc).abs() >= (1 << 31) || (cb - q * cd).abs() >= (1 << 31) {
                break;
            }
            let t = ca - q * cc;
            ca = cc;
            cc = t;
            let t = cb - q * cd;
            cb = cd;
            cd = t;
            let t = x - q * y;
            x = y;
            y = t;
        }
        if cb == 0 {
            let r = nat::divrem(&a, &b).1;
            a = b;
            b = r;
        } else {
            let (na, nb) = lin_comb(&a, &b, ca as i64, cb as i64, cc as i64, cd as i64);
            a = na;
            b = nb;
            if nat::cmp(&a, &b) == Ordering::Less {
                core::mem::swap(&mut a, &mut b);
            }
        }
    }
}

/// `math.gcd(a, b)` (non-negative).
pub fn gcd(a: IntRef, b: IntRef) -> BigInt {
    BigInt::from_parts(false, gcd_mag(a.mag, b.mag))
}

/// `math.lcm(a, b)` (non-negative).
pub fn lcm(a: IntRef, b: IntRef) -> BigInt {
    if a.mag.is_empty() || b.mag.is_empty() {
        return BigInt::zero();
    }
    let g = gcd_mag(a.mag, b.mag);
    let q = nat::divrem(a.mag, &g).0;
    BigInt::from_parts(false, nat::mul(&q, b.mag))
}

// ---------------------------------------------------------------------------------------------
// Integer square root
// ---------------------------------------------------------------------------------------------

/// floor(sqrt(n)) for u64.
pub fn isqrt_u64(n: u64) -> u64 {
    if n == 0 {
        return 0;
    }
    let mut r = (n as f64).sqrt() as u64;
    while r.checked_mul(r).is_none_or(|s| s > n) {
        r -= 1;
    }
    while (r + 1).checked_mul(r + 1).is_some_and(|s| s <= n) {
        r += 1;
    }
    r
}

/// floor(sqrt(n)) of a magnitude.
pub fn isqrt_mag(n: &[u32]) -> Vec<u32> {
    if let Some(v) = nat::to_u64(n) {
        return nat::from_u64(isqrt_u64(v));
    }
    let c = (nat::bit_length(n) - 1) / 2;
    let cbits = 64 - c.leading_zeros();
    let mut a: Vec<u32> = vec![1];
    let mut d: u64 = 0;
    for s in (0..cbits).rev() {
        let e = d;
        d = c >> s;
        // a = (a << (d - e - 1)) + (n >> (2c - e - d + 1)) // a
        let t = nat::shr(n, 2 * c - e - d + 1);
        let q = nat::divrem(&t, &a).0;
        let mut na = nat::shl(&a, d - e - 1);
        nat::add_in_place(&mut na, &q);
        a = na;
    }
    // a - (a*a > n)
    let sq = nat::sqr(&a);
    if nat::cmp(&sq, n) == Ordering::Greater {
        nat::sub_in_place(&mut a, &[1]);
    }
    a
}

/// `math.isqrt(n)`.
pub fn isqrt(n: IntRef) -> Result<BigInt, IntError> {
    if n.neg {
        return Err(IntError::IsqrtNegative);
    }
    Ok(BigInt::from_parts(false, isqrt_mag(n.mag)))
}

// ---------------------------------------------------------------------------------------------
// Factorial, comb, perm
// ---------------------------------------------------------------------------------------------

/// Product of all odd integers k with lo < k <= hi (as a magnitude).
fn odd_product(lo: u64, hi: u64) -> Vec<u32> {
    // first odd > lo, last odd <= hi
    let first = if lo % 2 == 0 { lo + 1 } else { lo + 2 };
    let last = if hi % 2 == 0 { hi.wrapping_sub(1) } else { hi };
    if hi == 0 || first > last {
        return vec![1];
    }
    let count = (last - first) / 2 + 1;
    odd_product_rec(first, count)
}

fn odd_product_rec(first: u64, count: u64) -> Vec<u32> {
    if count <= 16 {
        let mut acc: Vec<u32> = vec![1];
        let mut cur: u64 = 1;
        let mut k = first;
        for _ in 0..count {
            match cur.checked_mul(k) {
                Some(v) if v < (1 << 60) => cur = v,
                _ => {
                    acc = mul_u64(&acc, cur);
                    cur = k;
                }
            }
            k += 2;
        }
        return mul_u64(&acc, cur);
    }
    let h = count / 2;
    let a = odd_product_rec(first, h);
    let b = odd_product_rec(first + 2 * h, count - h);
    nat::mul(&a, &b)
}

fn mul_u64(a: &[u32], v: u64) -> Vec<u32> {
    let vm = nat::from_u64(v);
    nat::mul(a, &vm)
}

const SMALL_FACTORIALS: [u64; 21] = {
    let mut t = [1u64; 21];
    let mut i = 1;
    while i < 21 {
        t[i] = t[i - 1] * i as u64;
        i += 1;
    }
    t
};

/// n! as a magnitude.
pub fn factorial_mag(n: u64) -> Vec<u32> {
    if n <= 20 {
        return nat::from_u64(SMALL_FACTORIALS[n as usize]);
    }
    let mut inner: Vec<u32> = vec![1];
    let mut outer: Vec<u32> = vec![1];
    let levels = 64 - n.leading_zeros();
    for i in (0..levels).rev() {
        let upper = n >> i;
        let lower = n >> (i + 1);
        let p = odd_product(lower, upper);
        if p != [1] {
            inner = nat::mul(&inner, &p);
        }
        if inner != [1] {
            outer = nat::mul(&outer, &inner);
        }
    }
    nat::shl(&outer, n - n.count_ones() as u64)
}

/// `math.factorial(n)`.
pub fn factorial(n: IntRef) -> Result<BigInt, IntError> {
    if n.neg {
        return Err(IntError::FactorialNegative);
    }
    let v = match nat::to_u64(n.mag) {
        Some(v) if v <= (1u64 << 40) => v,
        Some(v) if v <= i64::MAX as u64 => return Err(IntError::MemoryError),
        _ => return Err(IntError::FactorialTooLarge),
    };
    Ok(BigInt::from_parts(false, factorial_mag(v)))
}

/// Product of the consecutive integers in (lo, hi] (u64 bounds).
fn range_product(lo: u64, hi: u64) -> Vec<u32> {
    if hi <= lo {
        return vec![1];
    }
    let count = hi - lo;
    if count <= 8 {
        let mut acc: Vec<u32> = vec![1];
        for k in lo + 1..=hi {
            acc = mul_u64(&acc, k);
        }
        return acc;
    }
    let mid = lo + count / 2;
    nat::mul(&range_product(lo, mid), &range_product(mid, hi))
}

/// Product of BigInt values n, n-1, ..., n-k+1 (n >= k).
fn falling_big(n: &BigInt, lo: u64, hi: u64) -> BigInt {
    // product of (n - i) for i in lo..hi
    if hi - lo <= 4 {
        let mut acc = BigInt::from_i64(1);
        for i in lo..hi {
            acc = &acc * &(n - &BigInt::from_u64(i));
        }
        return acc;
    }
    let mid = lo + (hi - lo) / 2;
    &falling_big(n, lo, mid) * &falling_big(n, mid, hi)
}

/// `math.perm(n, k)`.
pub fn perm(n: IntRef, k: IntRef) -> Result<BigInt, IntError> {
    if n.neg {
        return Err(IntError::NNegative);
    }
    if k.neg {
        return Err(IntError::KNegative);
    }
    if nat::cmp(k.mag, n.mag) == Ordering::Greater {
        return Ok(BigInt::zero());
    }
    let kv = match nat::to_u64(k.mag) {
        Some(v) if v <= (1u64 << 40) => v,
        Some(v) if v <= i64::MAX as u64 => return Err(IntError::MemoryError),
        _ => return Err(IntError::PermKTooLarge),
    };
    if let Some(nv) = nat::to_u64(n.mag) {
        return Ok(BigInt::from_parts(false, range_product(nv - kv, nv)));
    }
    Ok(falling_big(&n.to_owned(), 0, kv))
}

/// `math.comb(n, k)`.
pub fn comb(n: IntRef, k: IntRef) -> Result<BigInt, IntError> {
    if n.neg {
        return Err(IntError::NNegative);
    }
    if k.neg {
        return Err(IntError::KNegative);
    }
    if nat::cmp(k.mag, n.mag) == Ordering::Greater {
        return Ok(BigInt::zero());
    }
    // k = min(k, n - k)
    let nk = nat::sub(n.mag, k.mag);
    let kk: &[u32] = if nat::cmp(&nk, k.mag) == Ordering::Less { &nk } else { k.mag };
    let kv = match nat::to_u64(kk) {
        Some(v) if v <= (1u64 << 40) => v,
        Some(v) if v <= i64::MAX as u64 => return Err(IntError::MemoryError),
        _ => return Err(IntError::CombKTooLarge),
    };
    if kv == 0 {
        return Ok(BigInt::from_i64(1));
    }
    let num: Vec<u32> = if let Some(nv) = nat::to_u64(n.mag) {
        if kv <= 60 && nv <= 1 << 20 {
            // small: iterative exact u128
            let mut r: u128 = 1;
            let mut ok = true;
            for i in 0..kv {
                match r.checked_mul((nv - i) as u128) {
                    Some(v) => r = v / (i as u128 + 1),
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                return Ok(BigInt::from_u128(r));
            }
        }
        range_product(nv - kv, nv)
    } else {
        falling_big(&n.to_owned(), 0, kv).mag
    };
    let den = factorial_mag(kv);
    let (q, r) = nat::divrem(&num, &den);
    debug_assert!(r.is_empty());
    Ok(BigInt::from_parts(false, q))
}
