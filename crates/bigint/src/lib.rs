//! `rpy-bigint`: arbitrary-precision integers with exact Python `int` semantics.
//!
//! Magnitudes are little-endian slices of `u32` digits holding 30 bits each (base 2^30), which
//! is exactly CPython's `PyLongObject` digit layout, so the runtime can operate on object storage
//! directly. The sign is kept separately ([`IntRef`] / [`BigInt`]).
//!
//! Layers:
//! - [`nat`]: unsigned digit-slice kernels (add/sub/shift/single-digit mul and div, ...).
//! - [`mul`], [`div`]: multiplication (schoolbook, Karatsuba, Toom-3, NTT) and division
//!   (Knuth D, Newton/Barrett).
//! - [`ops`]: signed operations with Python semantics on [`IntRef`] views.
//! - [`conv`]: machine-int / float / bytes conversions, hashing.
//! - [`radix`]: string conversion (any radix 2..36) and Python's `int(str, base)` parser.
//! - [`math`]: gcd, lcm, isqrt, factorial, comb, perm.
//! - [`small`]: i64 fast paths with Python semantics.

pub mod conv;
pub mod div;
mod error;
pub mod math;
pub mod modpow;
pub mod mul;
pub mod nat;
mod kern;
mod ntt;
pub mod ops;
pub mod radix;
pub mod small;

pub use error::{ExcKind, IntError};
pub use nat::{BASE, MASK, SHIFT};

use core::cmp::Ordering;
use core::fmt;

/// Borrowed signed integer: sign flag plus a normalized magnitude (no high zero digits).
/// Zero must have `neg == false` and an empty magnitude.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IntRef<'a> {
    pub neg: bool,
    pub mag: &'a [u32],
}

impl<'a> IntRef<'a> {
    /// Build a view; `mag` must be normalized. Normalizes the sign of zero.
    #[inline]
    pub fn new(neg: bool, mag: &'a [u32]) -> Self {
        debug_assert!(mag.last() != Some(&0), "magnitude not normalized");
        IntRef { neg: neg && !mag.is_empty(), mag }
    }
    /// View that trims high zero digits.
    #[inline]
    pub fn new_trim(neg: bool, mag: &'a [u32]) -> Self {
        let mag = nat::trim(mag);
        IntRef { neg: neg && !mag.is_empty(), mag }
    }
    #[inline]
    pub fn is_zero(&self) -> bool {
        self.mag.is_empty()
    }
    #[inline]
    pub fn negated(self) -> Self {
        IntRef { neg: !self.neg && !self.mag.is_empty(), mag: self.mag }
    }
    /// Sign: -1, 0, 1.
    #[inline]
    pub fn signum(&self) -> i32 {
        if self.mag.is_empty() {
            0
        } else if self.neg {
            -1
        } else {
            1
        }
    }
    pub fn to_owned(&self) -> BigInt {
        BigInt { neg: self.neg, mag: self.mag.to_vec() }
    }
    /// Value as i64 if it fits.
    #[inline]
    pub fn to_i64(&self) -> Option<i64> {
        conv::to_i64(*self)
    }
}

/// Owned signed integer. Invariant: `mag` normalized, `neg == false` when zero.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct BigInt {
    pub neg: bool,
    pub mag: Vec<u32>,
}

impl BigInt {
    /// Zero.
    #[inline]
    pub fn zero() -> Self {
        BigInt { neg: false, mag: Vec::new() }
    }
    /// Construct from sign and (possibly unnormalized) magnitude.
    #[inline]
    pub fn from_parts(neg: bool, mut mag: Vec<u32>) -> Self {
        nat::normalize(&mut mag);
        BigInt { neg: neg && !mag.is_empty(), mag }
    }
    #[inline]
    pub fn as_ref(&self) -> IntRef<'_> {
        IntRef { neg: self.neg, mag: &self.mag }
    }
    #[inline]
    pub fn is_zero(&self) -> bool {
        self.mag.is_empty()
    }
    #[inline]
    pub fn is_negative(&self) -> bool {
        self.neg
    }
    pub fn signum(&self) -> i32 {
        self.as_ref().signum()
    }

    // -- conversions ------------------------------------------------------------------------
    pub fn from_i64(v: i64) -> Self {
        BigInt { neg: v < 0, mag: nat::from_u64(v.unsigned_abs()) }
    }
    pub fn from_u64(v: u64) -> Self {
        BigInt { neg: false, mag: nat::from_u64(v) }
    }
    pub fn from_i128(v: i128) -> Self {
        BigInt { neg: v < 0, mag: nat::from_u128(v.unsigned_abs()) }
    }
    pub fn from_u128(v: u128) -> Self {
        BigInt { neg: false, mag: nat::from_u128(v) }
    }
    pub fn to_i64(&self) -> Option<i64> {
        conv::to_i64(self.as_ref())
    }
    pub fn to_u64(&self) -> Option<u64> {
        conv::to_u64(self.as_ref())
    }
    pub fn to_i128(&self) -> Option<i128> {
        conv::to_i128(self.as_ref())
    }
    pub fn to_u128(&self) -> Option<u128> {
        conv::to_u128(self.as_ref())
    }
    /// Truncating conversion from a float (`int(x)`).
    pub fn from_f64(x: f64) -> Result<Self, IntError> {
        conv::from_f64(x)
    }
    /// Correctly rounded conversion to float (`float(x)`).
    pub fn to_f64(&self) -> Result<f64, IntError> {
        conv::to_f64(self.as_ref())
    }
    /// Parse digits (no sign/prefix/underscores) in `radix` (2..=36).
    pub fn from_str_radix(s: &str, radix: u32) -> Option<Self> {
        radix::from_digits(s.as_bytes(), radix).map(|m| BigInt::from_parts(false, m))
    }
    /// Python `int(s, base)` (see [`radix::parse_int`]).
    pub fn parse(s: &[u8], base: u32) -> Result<Self, IntError> {
        radix::parse_int(s, base, 0)
    }
    /// Lowercase string in `radix` (2..=36), with '-' for negatives, no prefix.
    pub fn to_str_radix(&self, radix: u32) -> String {
        radix::to_string(self.as_ref(), radix)
    }

    // -- arithmetic -------------------------------------------------------------------------
    pub fn add(&self, o: &BigInt) -> BigInt {
        ops::add(self.as_ref(), o.as_ref())
    }
    pub fn sub(&self, o: &BigInt) -> BigInt {
        ops::sub(self.as_ref(), o.as_ref())
    }
    pub fn mul(&self, o: &BigInt) -> BigInt {
        ops::mul(self.as_ref(), o.as_ref())
    }
    pub fn floordiv(&self, o: &BigInt) -> Result<BigInt, IntError> {
        ops::floordiv(self.as_ref(), o.as_ref())
    }
    pub fn modulo(&self, o: &BigInt) -> Result<BigInt, IntError> {
        ops::modulo(self.as_ref(), o.as_ref())
    }
    pub fn divmod(&self, o: &BigInt) -> Result<(BigInt, BigInt), IntError> {
        ops::divmod(self.as_ref(), o.as_ref())
    }
    pub fn truediv(&self, o: &BigInt) -> Result<f64, IntError> {
        ops::truediv(self.as_ref(), o.as_ref())
    }
    pub fn pow(&self, e: &BigInt) -> Result<BigInt, IntError> {
        ops::pow(self.as_ref(), e.as_ref())
    }
    pub fn pow_u64(&self, e: u64) -> Result<BigInt, IntError> {
        ops::pow_u64(self.as_ref(), e)
    }
    pub fn pow_mod(&self, e: &BigInt, m: &BigInt) -> Result<BigInt, IntError> {
        ops::pow_mod(self.as_ref(), e.as_ref(), m.as_ref())
    }
    pub fn shl(&self, n: u64) -> Result<BigInt, IntError> {
        ops::lshift(self.as_ref(), n)
    }
    pub fn shr(&self, n: u64) -> BigInt {
        ops::rshift(self.as_ref(), n)
    }
    pub fn and(&self, o: &BigInt) -> BigInt {
        ops::and(self.as_ref(), o.as_ref())
    }
    pub fn or(&self, o: &BigInt) -> BigInt {
        ops::or(self.as_ref(), o.as_ref())
    }
    pub fn xor(&self, o: &BigInt) -> BigInt {
        ops::xor(self.as_ref(), o.as_ref())
    }
    pub fn invert(&self) -> BigInt {
        ops::invert(self.as_ref())
    }
    pub fn neg(&self) -> BigInt {
        ops::neg(self.as_ref())
    }
    pub fn abs(&self) -> BigInt {
        ops::abs(self.as_ref())
    }
    pub fn bit_length(&self) -> u64 {
        nat::bit_length(&self.mag)
    }
    pub fn bit_count(&self) -> u64 {
        nat::bit_count(&self.mag)
    }
    /// Python hash.
    pub fn hash(&self) -> i64 {
        conv::hash(self.as_ref())
    }
    pub fn isqrt(&self) -> Result<BigInt, IntError> {
        math::isqrt(self.as_ref())
    }
    pub fn gcd(&self, o: &BigInt) -> BigInt {
        math::gcd(self.as_ref(), o.as_ref())
    }
}

impl From<i64> for BigInt {
    fn from(v: i64) -> Self {
        BigInt::from_i64(v)
    }
}
impl From<i32> for BigInt {
    fn from(v: i32) -> Self {
        BigInt::from_i64(v as i64)
    }
}
impl From<u64> for BigInt {
    fn from(v: u64) -> Self {
        BigInt::from_u64(v)
    }
}
impl From<i128> for BigInt {
    fn from(v: i128) -> Self {
        BigInt::from_i128(v)
    }
}
impl From<u128> for BigInt {
    fn from(v: u128) -> Self {
        BigInt::from_u128(v)
    }
}

impl Ord for BigInt {
    fn cmp(&self, o: &Self) -> Ordering {
        ops::cmp(self.as_ref(), o.as_ref())
    }
}
impl PartialOrd for BigInt {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl fmt::Display for BigInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&radix::to_string(self.as_ref(), 10))
    }
}
impl fmt::Debug for BigInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mag.len() > 64 {
            write!(f, "BigInt({}0x{}...{} digits)", if self.neg { "-" } else { "" }, &radix::to_string(self.abs().as_ref(), 16)[..16], self.mag.len())
        } else {
            write!(f, "BigInt({})", self)
        }
    }
}

impl core::str::FromStr for BigInt {
    type Err = IntError;
    fn from_str(s: &str) -> Result<Self, IntError> {
        radix::parse_int(s.as_bytes(), 10, 0)
    }
}

macro_rules! binop {
    ($tr:ident, $m:ident, $f:path) => {
        impl core::ops::$tr<&BigInt> for &BigInt {
            type Output = BigInt;
            fn $m(self, o: &BigInt) -> BigInt {
                $f(self.as_ref(), o.as_ref())
            }
        }
        impl core::ops::$tr<BigInt> for BigInt {
            type Output = BigInt;
            fn $m(self, o: BigInt) -> BigInt {
                $f(self.as_ref(), o.as_ref())
            }
        }
        impl core::ops::$tr<&BigInt> for BigInt {
            type Output = BigInt;
            fn $m(self, o: &BigInt) -> BigInt {
                $f(self.as_ref(), o.as_ref())
            }
        }
    };
}
binop!(Add, add, ops::add);
binop!(Sub, sub, ops::sub);
binop!(Mul, mul, ops::mul);
binop!(BitAnd, bitand, ops::and);
binop!(BitOr, bitor, ops::or);
binop!(BitXor, bitxor, ops::xor);

fn div_panic(a: IntRef, b: IntRef) -> BigInt {
    ops::floordiv(a, b).expect("division by zero")
}
fn rem_panic(a: IntRef, b: IntRef) -> BigInt {
    ops::modulo(a, b).expect("modulo by zero")
}
// `/` and `%` on BigInt use Python floor semantics and panic on zero divisors.
binop!(Div, div, div_panic);
binop!(Rem, rem, rem_panic);

impl core::ops::Neg for BigInt {
    type Output = BigInt;
    fn neg(self) -> BigInt {
        BigInt::from_parts(!self.neg, self.mag)
    }
}
impl core::ops::Neg for &BigInt {
    type Output = BigInt;
    fn neg(self) -> BigInt {
        ops::neg(self.as_ref())
    }
}
impl core::ops::Not for &BigInt {
    type Output = BigInt;
    fn not(self) -> BigInt {
        ops::invert(self.as_ref())
    }
}
impl core::ops::Shl<u64> for &BigInt {
    type Output = BigInt;
    fn shl(self, n: u64) -> BigInt {
        ops::lshift(self.as_ref(), n).expect("shift too large")
    }
}
impl core::ops::Shr<u64> for &BigInt {
    type Output = BigInt;
    fn shr(self, n: u64) -> BigInt {
        ops::rshift(self.as_ref(), n)
    }
}
impl core::ops::Shr<u64> for BigInt {
    type Output = BigInt;
    fn shr(self, n: u64) -> BigInt {
        ops::rshift(self.as_ref(), n)
    }
}
impl core::ops::Shl<u64> for BigInt {
    type Output = BigInt;
    fn shl(self, n: u64) -> BigInt {
        ops::lshift(self.as_ref(), n).expect("shift too large")
    }
}
impl core::ops::AddAssign<&BigInt> for BigInt {
    fn add_assign(&mut self, o: &BigInt) {
        if self.neg == o.neg {
            nat::add_in_place(&mut self.mag, &o.mag);
        } else {
            *self = ops::add(self.as_ref(), o.as_ref());
        }
    }
}
impl core::ops::SubAssign<&BigInt> for BigInt {
    fn sub_assign(&mut self, o: &BigInt) {
        *self = ops::sub(self.as_ref(), o.as_ref());
    }
}
impl core::ops::MulAssign<&BigInt> for BigInt {
    fn mul_assign(&mut self, o: &BigInt) {
        *self = ops::mul(self.as_ref(), o.as_ref());
    }
}
