//! Natural-number (magnitude) primitives on little-endian base-2^30 digit slices.
//!
//! Conventions: a "normalized" magnitude has no high zero digits (zero is the empty slice).
//! Functions returning `Vec<u32>` always return normalized vectors. Low-level kernels that take
//! output slices document their size requirements; they never require normalized inputs unless
//! stated.

use core::cmp::Ordering;

/// Bits per digit.
pub const SHIFT: u32 = 30;
/// Digit base (2^30).
pub const BASE: u32 = 1 << SHIFT;
/// Mask for one digit.
pub const MASK: u32 = BASE - 1;

/// Minimum length for the SIMD kernels.
pub const KERN_MIN: usize = 12;

/// Strip high zero digits.
#[inline]
pub fn trim(a: &[u32]) -> &[u32] {
    let mut n = a.len();
    while n > 0 && a[n - 1] == 0 {
        n -= 1;
    }
    &a[..n]
}

/// Length after stripping high zero digits.
#[inline]
pub fn trimmed_len(a: &[u32]) -> usize {
    let mut n = a.len();
    while n > 0 && a[n - 1] == 0 {
        n -= 1;
    }
    n
}

/// Remove high zero digits from a vector.
#[inline]
pub fn normalize(v: &mut Vec<u32>) {
    let n = trimmed_len(v);
    v.truncate(n);
}

/// Compare two normalized magnitudes.
#[inline]
pub fn cmp(a: &[u32], b: &[u32]) -> Ordering {
    if a.len() != b.len() {
        return a.len().cmp(&b.len());
    }
    cmp_same_len(a, b)
}

/// Compare two magnitudes of equal length (high zeros allowed).
#[inline]
pub fn cmp_same_len(a: &[u32], b: &[u32]) -> Ordering {
    debug_assert_eq!(a.len(), b.len());
    let mut i = a.len();
    while i > 0 {
        i -= 1;
        if a[i] != b[i] {
            return a[i].cmp(&b[i]);
        }
    }
    Ordering::Equal
}

/// Compare magnitudes that may have high zero digits.
pub fn cmp_unnorm(a: &[u32], b: &[u32]) -> Ordering {
    cmp(trim(a), trim(b))
}

/// Number of significant bits of a normalized magnitude (0 for zero).
#[inline]
pub fn bit_length(a: &[u32]) -> u64 {
    match a.last() {
        None => 0,
        Some(&top) => (a.len() as u64 - 1) * SHIFT as u64 + (32 - top.leading_zeros()) as u64,
    }
}

/// Number of one bits.
pub fn bit_count(a: &[u32]) -> u64 {
    a.iter().map(|d| d.count_ones() as u64).sum()
}

/// Number of trailing zero bits (None for zero).
pub fn trailing_zeros(a: &[u32]) -> Option<u64> {
    for (i, &d) in a.iter().enumerate() {
        if d != 0 {
            return Some(i as u64 * SHIFT as u64 + d.trailing_zeros() as u64);
        }
    }
    None
}

/// Value of bit `i`.
#[inline]
pub fn test_bit(a: &[u32], i: u64) -> bool {
    let di = (i / SHIFT as u64) as usize;
    di < a.len() && (a[di] >> (i % SHIFT as u64)) & 1 == 1
}

/// True if any bit below position `i` is set.
pub fn any_bits_below(a: &[u32], i: u64) -> bool {
    let di = (i / SHIFT as u64) as usize;
    let bi = (i % SHIFT as u64) as u32;
    let full = di.min(a.len());
    if a[..full].iter().any(|&d| d != 0) {
        return true;
    }
    di < a.len() && bi > 0 && a[di] & ((1u32 << bi) - 1) != 0
}

// ---------------------------------------------------------------------------------------------
// Conversions to/from machine integers
// ---------------------------------------------------------------------------------------------

/// Magnitude of a u64.
#[inline]
pub fn from_u64(mut x: u64) -> Vec<u32> {
    let mut v = Vec::with_capacity(3);
    while x != 0 {
        v.push((x as u32) & MASK);
        x >>= SHIFT;
    }
    v
}

/// Magnitude of a u128.
pub fn from_u128(mut x: u128) -> Vec<u32> {
    let mut v = Vec::with_capacity(5);
    while x != 0 {
        v.push((x as u32) & MASK);
        x >>= SHIFT;
    }
    v
}

/// Value as u64 if it fits.
#[inline]
pub fn to_u64(a: &[u32]) -> Option<u64> {
    match a.len() {
        0 => Some(0),
        1 => Some(a[0] as u64),
        2 => Some(a[0] as u64 | (a[1] as u64) << SHIFT),
        3 => {
            if a[2] >> 4 != 0 {
                None
            } else {
                Some(a[0] as u64 | (a[1] as u64) << SHIFT | (a[2] as u64) << (2 * SHIFT))
            }
        }
        _ => None,
    }
}

/// Value as u128 if it fits.
pub fn to_u128(a: &[u32]) -> Option<u128> {
    if bit_length(a) > 128 {
        return None;
    }
    let mut r: u128 = 0;
    for &d in a.iter().rev() {
        r = (r << SHIFT) | d as u128;
    }
    Some(r)
}

/// Low 64 bits of the magnitude (wrapping).
pub fn low_u64(a: &[u32]) -> u64 {
    let mut r: u64 = 0;
    for (i, &d) in a.iter().take(3).enumerate() {
        r |= (d as u64).wrapping_shl(SHIFT * i as u32);
    }
    r
}

// ---------------------------------------------------------------------------------------------
// Addition / subtraction kernels
// ---------------------------------------------------------------------------------------------

/// `out[..a.len()] = a + b` where `a.len() >= b.len()` and `out.len() >= a.len()`.
/// Returns the carry out of the top digit (0 or 1).
#[inline]
pub fn add_to(out: &mut [u32], a: &[u32], b: &[u32]) -> u32 {
    debug_assert!(a.len() >= b.len() && out.len() >= a.len());
    let n = b.len();
    if n >= KERN_MIN {
        let mut carry = crate::kern::add_n(out, &a[..n], b);
        let mut i = n;
        while carry != 0 && i < a.len() {
            let s = a[i] + carry;
            out[i] = s & MASK;
            carry = s >> SHIFT;
            i += 1;
        }
        out[i..a.len()].copy_from_slice(&a[i..]);
        return carry;
    }
    let mut carry = 0u32;
    for i in 0..n {
        let s = a[i] + b[i] + carry;
        out[i] = s & MASK;
        carry = s >> SHIFT;
    }
    for i in n..a.len() {
        let s = a[i] + carry;
        out[i] = s & MASK;
        carry = s >> SHIFT;
    }
    carry
}

/// `acc += b` (acc.len() >= b.len()); carry propagates through all of `acc`. Returns final carry.
#[inline]
pub fn add_assign(acc: &mut [u32], b: &[u32]) -> u32 {
    debug_assert!(acc.len() >= b.len());
    let mut carry = 0u32;
    let n = b.len();
    for i in 0..n {
        let s = acc[i] + b[i] + carry;
        acc[i] = s & MASK;
        carry = s >> SHIFT;
    }
    let mut i = n;
    while carry != 0 && i < acc.len() {
        let s = acc[i] + carry;
        acc[i] = s & MASK;
        carry = s >> SHIFT;
        i += 1;
    }
    carry
}

/// `acc += d` for a single digit value `d < 2^31`. Returns final carry.
#[inline]
pub fn add_digit_assign(acc: &mut [u32], d: u32) -> u32 {
    let mut carry = d;
    let mut i = 0;
    while carry != 0 && i < acc.len() {
        let s = acc[i] + carry;
        acc[i] = s & MASK;
        carry = s >> SHIFT;
        i += 1;
    }
    carry
}

/// `out[..a.len()] = a - b` with `a.len() >= b.len()`. Returns borrow (1 if a < b).
#[inline]
pub fn sub_to(out: &mut [u32], a: &[u32], b: &[u32]) -> u32 {
    debug_assert!(a.len() >= b.len() && out.len() >= a.len());
    let n = b.len();
    if n >= KERN_MIN {
        let mut borrow = crate::kern::sub_n(out, &a[..n], b);
        let mut i = n;
        while borrow != 0 && i < a.len() {
            let d = a[i].wrapping_sub(borrow);
            out[i] = d & MASK;
            borrow = d >> 31;
            i += 1;
        }
        out[i..a.len()].copy_from_slice(&a[i..]);
        return borrow;
    }
    let mut borrow = 0u32;
    for i in 0..n {
        let d = a[i].wrapping_sub(b[i]).wrapping_sub(borrow);
        out[i] = d & MASK;
        borrow = d >> 31;
    }
    for i in n..a.len() {
        let d = a[i].wrapping_sub(borrow);
        out[i] = d & MASK;
        borrow = d >> 31;
    }
    borrow
}

/// `acc -= b` (acc.len() >= b.len()); borrow propagates through `acc`. Returns final borrow.
#[inline]
pub fn sub_assign(acc: &mut [u32], b: &[u32]) -> u32 {
    debug_assert!(acc.len() >= b.len());
    let mut borrow = 0u32;
    let n = b.len();
    for i in 0..n {
        let d = acc[i].wrapping_sub(b[i]).wrapping_sub(borrow);
        acc[i] = d & MASK;
        borrow = d >> 31;
    }
    let mut i = n;
    while borrow != 0 && i < acc.len() {
        let d = acc[i].wrapping_sub(borrow);
        acc[i] = d & MASK;
        borrow = d >> 31;
        i += 1;
    }
    borrow
}

/// `acc = b - acc` for equal-length slices. Returns borrow.
#[inline]
pub fn rsub_assign(acc: &mut [u32], b: &[u32]) -> u32 {
    debug_assert!(acc.len() == b.len());
    let mut borrow = 0u32;
    for i in 0..acc.len() {
        let d = b[i].wrapping_sub(acc[i]).wrapping_sub(borrow);
        acc[i] = d & MASK;
        borrow = d >> 31;
    }
    borrow
}

/// `acc -= d` single digit. Returns final borrow.
#[inline]
pub fn sub_digit_assign(acc: &mut [u32], d: u32) -> u32 {
    let mut borrow = d;
    let mut i = 0;
    while borrow != 0 && i < acc.len() {
        let x = acc[i].wrapping_sub(borrow);
        acc[i] = x & MASK;
        borrow = x >> 31;
        i += 1;
    }
    borrow
}

/// `a + b` (normalized inputs) as a new normalized vector.
pub fn add(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (a, b) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let mut out = vec![0u32; a.len() + 1];
    let c = add_to(&mut out, a, b);
    out[a.len()] = c;
    normalize(&mut out);
    out
}

/// Writes `a + b` to `out` (len >= max(len a, len b) + 1); returns normalized length.
pub fn add_into(out: &mut [u32], a: &[u32], b: &[u32]) -> usize {
    let (a, b) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let c = add_to(out, a, b);
    out[a.len()] = c;
    trimmed_len(&out[..a.len() + 1])
}

/// `a - b` for normalized `a >= b`.
pub fn sub(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = vec![0u32; a.len()];
    let br = sub_to(&mut out, a, b);
    debug_assert_eq!(br, 0);
    normalize(&mut out);
    out
}

/// Writes `a - b` (a >= b) into `out` (len >= a.len()); returns normalized length.
pub fn sub_into(out: &mut [u32], a: &[u32], b: &[u32]) -> usize {
    let br = sub_to(out, a, b);
    debug_assert_eq!(br, 0);
    trimmed_len(&out[..a.len()])
}

/// |a - b| and whether a < b.
pub fn abs_diff(a: &[u32], b: &[u32]) -> (Vec<u32>, bool) {
    match cmp(a, b) {
        Ordering::Less => (sub(b, a), true),
        Ordering::Equal => (Vec::new(), false),
        Ordering::Greater => (sub(a, b), false),
    }
}

/// In-place `v += b` growing `v` as needed (normalized result).
pub fn add_in_place(v: &mut Vec<u32>, b: &[u32]) {
    if v.len() < b.len() {
        v.resize(b.len(), 0);
    }
    let c = add_assign(v, b);
    if c != 0 {
        v.push(c);
    }
    normalize(v);
}

/// In-place `v -= b` (requires v >= b).
pub fn sub_in_place(v: &mut Vec<u32>, b: &[u32]) {
    let br = sub_assign(v, b);
    debug_assert_eq!(br, 0);
    normalize(v);
}

/// In-place `v = b - v` (requires b >= v).
pub fn rsub_in_place(v: &mut Vec<u32>, b: &[u32]) {
    v.resize(b.len(), 0);
    let br = rsub_assign(v, b);
    debug_assert_eq!(br, 0);
    normalize(v);
}

// ---------------------------------------------------------------------------------------------
// Single-digit multiplication / division
// ---------------------------------------------------------------------------------------------

/// `out[..a.len()] = a * d + carry_in`, returns carry digit(s) (< 2^31 when d < 2^30... in
/// general < 2^34). `d` may be up to 2^32-1.
#[inline]
pub fn mul_digit_to(out: &mut [u32], a: &[u32], d: u32, carry_in: u64) -> u64 {
    if carry_in == 0 && d <= MASK && a.len() >= KERN_MIN {
        return crate::kern::mul1(out, a, d);
    }
    let mut carry = carry_in;
    let d = d as u64;
    for i in 0..a.len() {
        let t = a[i] as u64 * d + carry;
        out[i] = (t as u32) & MASK;
        carry = t >> SHIFT;
    }
    carry
}

/// `acc[..a.len()] += a * d`; returns the carry to add at `acc[a.len()]` (< 2^31 for d < 2^30).
#[inline]
pub fn mul_digit_add(acc: &mut [u32], a: &[u32], d: u32) -> u32 {
    debug_assert!(d <= MASK);
    let d = d as u64;
    let mut carry: u32 = 0;
    let mut hi_prev: u32 = 0;
    for (x, &y) in acc.iter_mut().zip(a.iter()) {
        let p = y as u64 * d;
        let t = *x + ((p as u32) & MASK) + hi_prev + carry;
        *x = t & MASK;
        carry = t >> SHIFT;
        hi_prev = (p >> SHIFT) as u32;
    }
    hi_prev + carry
}

/// `acc[..a.len()] -= a * d`; returns the amount to subtract at `acc[a.len()]`.
#[inline]
pub fn mul_digit_sub(acc: &mut [u32], a: &[u32], d: u32) -> u32 {
    debug_assert!(d <= BASE);
    let d = d as u64;
    let mut borrow: i64 = 0;
    let mut hi_prev: i64 = 0;
    for (x, &y) in acc.iter_mut().zip(a.iter()) {
        let p = y as u64 * d;
        let t = *x as i64 - (p & MASK as u64) as i64 - hi_prev - borrow;
        *x = (t as u32) & MASK;
        borrow = -(t >> SHIFT);
        hi_prev = (p >> SHIFT) as i64;
    }
    (hi_prev + borrow) as u32
}

/// `a * d` for a u32 multiplier.
pub fn mul_u32(a: &[u32], d: u32) -> Vec<u32> {
    if a.is_empty() || d == 0 {
        return Vec::new();
    }
    let mut out = vec![0u32; a.len() + 2];
    let mut c = mul_digit_to(&mut out, a, d, 0);
    let mut i = a.len();
    while c != 0 {
        out[i] = (c as u32) & MASK;
        c >>= SHIFT;
        i += 1;
    }
    normalize(&mut out);
    out
}

/// In-place `v = v * d + add` (d < 2^32, add < 2^32).
pub fn mul_add_u32_in_place(v: &mut Vec<u32>, d: u32, add: u32) {
    let mut carry = add as u64;
    let dd = d as u64;
    for x in v.iter_mut() {
        let t = *x as u64 * dd + carry;
        *x = (t as u32) & MASK;
        carry = t >> SHIFT;
    }
    while carry != 0 {
        v.push((carry as u32) & MASK);
        carry >>= SHIFT;
    }
    normalize(v);
}

/// Divide `a` by single digit `d` (0 < d < 2^32) writing the quotient to `out`
/// (len >= a.len()). Returns remainder.
#[inline]
pub fn divrem_digit_to(out: &mut [u32], a: &[u32], d: u32) -> u32 {
    debug_assert!(d != 0);
    let d = d as u64;
    let mut rem = 0u64;
    let mut i = a.len();
    while i > 0 {
        i -= 1;
        let cur = (rem << SHIFT) | a[i] as u64;
        let q = cur / d;
        out[i] = q as u32;
        rem = cur - q * d;
    }
    rem as u32
}

/// Remainder of `a` modulo a single digit `d` (0 < d < 2^32).
pub fn rem_digit(a: &[u32], d: u32) -> u32 {
    let d = d as u64;
    let mut rem = 0u64;
    for &x in a.iter().rev() {
        rem = ((rem << SHIFT) | x as u64) % d;
    }
    rem as u32
}

/// Quotient and remainder by a single digit (0 < d < 2^32).
pub fn divrem_u32(a: &[u32], d: u32) -> (Vec<u32>, u32) {
    let mut q = vec![0u32; a.len()];
    let r = divrem_digit_to(&mut q, a, d);
    normalize(&mut q);
    (q, r)
}

/// In-place division by a single digit; returns remainder.
pub fn div_u32_in_place(v: &mut Vec<u32>, d: u32) -> u32 {
    let dd = d as u64;
    let mut rem = 0u64;
    for x in v.iter_mut().rev() {
        let cur = (rem << SHIFT) | *x as u64;
        let q = cur / dd;
        *x = q as u32;
        rem = cur - q * dd;
    }
    normalize(v);
    rem as u32
}

// ---------------------------------------------------------------------------------------------
// Shifts
// ---------------------------------------------------------------------------------------------

/// `out[..a.len()] = a << bits` for `bits < 30`; returns the bits shifted out of the top.
#[inline]
pub fn shl_bits_to(out: &mut [u32], a: &[u32], bits: u32) -> u32 {
    debug_assert!(bits < SHIFT);
    if bits == 0 {
        out[..a.len()].copy_from_slice(a);
        return 0;
    }
    let n = a.len();
    if n == 0 {
        return 0;
    }
    let out = &mut out[..n];
    out[0] = (a[0] << bits) & MASK;
    let rb = SHIFT - bits;
    for ((o, &x1), &x0) in out[1..].iter_mut().zip(&a[1..]).zip(&a[..n - 1]) {
        *o = ((x1 << bits) & MASK) | (x0 >> rb);
    }
    a[n - 1] >> rb
}

/// `out[..a.len()] = a >> bits` for `bits < 30`, with `hi` supplying bits from above.
/// Returns the bits shifted out of the bottom (in the low `bits` bits).
#[inline]
pub fn shr_bits_to(out: &mut [u32], a: &[u32], bits: u32) -> u32 {
    debug_assert!(bits < SHIFT);
    if bits == 0 {
        out[..a.len()].copy_from_slice(a);
        return 0;
    }
    let n = a.len();
    if n == 0 {
        return 0;
    }
    let out = &mut out[..n];
    let mask = (1u32 << bits) - 1;
    let lb = SHIFT - bits;
    for ((o, &x0), &x1) in out[..n - 1].iter_mut().zip(&a[..n - 1]).zip(&a[1..]) {
        *o = (x0 >> bits) | ((x1 & mask) << lb);
    }
    out[n - 1] = a[n - 1] >> bits;
    a[0] & mask
}

/// `a << n` (normalized).
pub fn shl(a: &[u32], n: u64) -> Vec<u32> {
    if a.is_empty() {
        return Vec::new();
    }
    let dshift = (n / SHIFT as u64) as usize;
    let bshift = (n % SHIFT as u64) as u32;
    let mut out = vec![0u32; dshift + a.len() + 1];
    let c = shl_bits_to(&mut out[dshift..], a, bshift);
    out[dshift + a.len()] = c;
    normalize(&mut out);
    out
}

/// Writes `a << n` into out (len >= a.len() + n/30 + 1); returns normalized length.
pub fn shl_into(out: &mut [u32], a: &[u32], n: u64) -> usize {
    if a.is_empty() {
        return 0;
    }
    let dshift = (n / SHIFT as u64) as usize;
    let bshift = (n % SHIFT as u64) as u32;
    out[..dshift].fill(0);
    let c = shl_bits_to(&mut out[dshift..], a, bshift);
    out[dshift + a.len()] = c;
    trimmed_len(&out[..dshift + a.len() + 1])
}

/// `a >> n` (normalized).
pub fn shr(a: &[u32], n: u64) -> Vec<u32> {
    let dshift = n / SHIFT as u64;
    if dshift >= a.len() as u64 {
        return Vec::new();
    }
    let dshift = dshift as usize;
    let bshift = (n % SHIFT as u64) as u32;
    let src = &a[dshift..];
    let mut out = vec![0u32; src.len()];
    shr_bits_to(&mut out, src, bshift);
    normalize(&mut out);
    out
}

/// Low `n` bits of `a` (a mod 2^n).
pub fn low_bits(a: &[u32], n: u64) -> Vec<u32> {
    let full = (n / SHIFT as u64) as usize;
    let rem = (n % SHIFT as u64) as u32;
    if full >= a.len() {
        return a.to_vec();
    }
    let mut out = a[..full + if rem > 0 { 1 } else { 0 }].to_vec();
    if rem > 0 {
        out[full] &= (1u32 << rem) - 1;
    }
    normalize(&mut out);
    out
}

/// Multiplication dispatcher: `a * b` for normalized inputs.
pub fn mul(a: &[u32], b: &[u32]) -> Vec<u32> {
    crate::mul::mul(a, b)
}

/// Squaring.
pub fn sqr(a: &[u32]) -> Vec<u32> {
    crate::mul::sqr(a)
}

/// Quotient and remainder for normalized inputs, `b` nonzero.
pub fn divrem(a: &[u32], b: &[u32]) -> (Vec<u32>, Vec<u32>) {
    crate::div::divrem(a, b)
}

/// One digit of value `x < 2^60` split.
#[inline]
pub fn from_two(x: u64) -> [u32; 2] {
    [(x as u32) & MASK, (x >> SHIFT) as u32]
}

/// Power of two magnitude 2^n.
pub fn pow2(n: u64) -> Vec<u32> {
    let d = (n / SHIFT as u64) as usize;
    let mut v = vec![0u32; d + 1];
    v[d] = 1 << (n % SHIFT as u64);
    v
}

/// Is the magnitude zero.
#[inline]
pub fn is_zero(a: &[u32]) -> bool {
    a.is_empty()
}

/// Extract `bits` (<= 64) bits starting at bit position `pos`.
pub fn extract_bits(a: &[u32], pos: u64, bits: u32) -> u64 {
    debug_assert!(bits <= 64);
    let mut r: u128 = 0;
    let first = (pos / SHIFT as u64) as usize;
    let off = (pos % SHIFT as u64) as u32;
    let mut i = first;
    let mut got: u32 = 0;
    while got < bits + off && i < a.len() {
        r |= (a[i] as u128) << got;
        got += SHIFT;
        i += 1;
    }
    let r = r >> off;
    if bits == 64 { r as u64 } else { (r as u64) & ((1u64 << bits) - 1) }
}
