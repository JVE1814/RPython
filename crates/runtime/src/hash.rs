//! Hashing: SipHash-1-3 for str/bytes (randomised per process unless
//! PYTHONHASHSEED is set), and the numeric hash (mod 2**61-1) shared by
//! int/float/complex/Fraction/Decimal.

use std::sync::atomic::{AtomicU64, Ordering::*};

pub static K0: AtomicU64 = AtomicU64::new(0);
pub static K1: AtomicU64 = AtomicU64::new(0);
pub static HASH_SEED_SET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub const MODULUS_BITS: u32 = 61;
pub const MODULUS: u64 = (1u64 << MODULUS_BITS) - 1;
pub const HASH_INF: isize = 314159;
pub const HASH_IMAG: isize = 1000003;

/// Initialise the hash key. `seed` = Some(0) disables randomisation (key all zero),
/// Some(n) derives the key deterministically, None uses OS randomness.
pub fn init_hash_secret(seed: Option<u32>) {
    let mut key = [0u8; 16];
    match seed {
        Some(0) => {}
        Some(s) => {
            // Linear congruential generator, as documented for PYTHONHASHSEED.
            let mut x: u32 = s;
            for b in key.iter_mut() {
                x = x.wrapping_mul(214013).wrapping_add(2531011);
                *b = ((x >> 16) & 0xff) as u8;
            }
        }
        None => unsafe {
            let n = libc::getrandom(key.as_mut_ptr() as *mut libc::c_void, 16, 0);
            if n != 16 {
                let t = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                key[..16].copy_from_slice(&(t ^ (libc::getpid() as u128) << 64).to_le_bytes());
            }
        },
    }
    K0.store(u64::from_le_bytes(key[0..8].try_into().unwrap()), Relaxed);
    K1.store(u64::from_le_bytes(key[8..16].try_into().unwrap()), Relaxed);
    HASH_SEED_SET.store(seed.is_some(), Relaxed);
}

#[inline(always)]
fn rotl(x: u64, b: u32) -> u64 {
    x.rotate_left(b)
}

#[inline(always)]
fn sipround(v0: &mut u64, v1: &mut u64, v2: &mut u64, v3: &mut u64) {
    *v0 = v0.wrapping_add(*v1);
    *v1 = rotl(*v1, 13);
    *v1 ^= *v0;
    *v0 = rotl(*v0, 32);
    *v2 = v2.wrapping_add(*v3);
    *v3 = rotl(*v3, 16);
    *v3 ^= *v2;
    *v0 = v0.wrapping_add(*v3);
    *v3 = rotl(*v3, 21);
    *v3 ^= *v0;
    *v2 = v2.wrapping_add(*v1);
    *v1 = rotl(*v1, 17);
    *v1 ^= *v2;
    *v2 = rotl(*v2, 32);
}

pub fn siphash13(k0: u64, k1: u64, data: &[u8]) -> u64 {
    let mut v0 = k0 ^ 0x736f6d6570736575;
    let mut v1 = k1 ^ 0x646f72616e646f6d;
    let mut v2 = k0 ^ 0x6c7967656e657261;
    let mut v3 = k1 ^ 0x7465646279746573;
    let len = data.len();
    let mut chunks = data.chunks_exact(8);
    for c in &mut chunks {
        let m = u64::from_le_bytes(c.try_into().unwrap());
        v3 ^= m;
        sipround(&mut v0, &mut v1, &mut v2, &mut v3);
        v0 ^= m;
    }
    let rem = chunks.remainder();
    let mut b: u64 = (len as u64) << 56;
    for (i, &byte) in rem.iter().enumerate() {
        b |= (byte as u64) << (8 * i);
    }
    v3 ^= b;
    sipround(&mut v0, &mut v1, &mut v2, &mut v3);
    v0 ^= b;
    v2 ^= 0xff;
    sipround(&mut v0, &mut v1, &mut v2, &mut v3);
    sipround(&mut v0, &mut v1, &mut v2, &mut v3);
    sipround(&mut v0, &mut v1, &mut v2, &mut v3);
    v0 ^ v1 ^ v2 ^ v3
}

/// Hash of raw bytes as used for str (PEP 393 data) and bytes.
#[inline]
pub fn hash_bytes(data: &[u8]) -> isize {
    if data.is_empty() {
        return 0;
    }
    let h = siphash13(K0.load(Relaxed), K1.load(Relaxed), data) as isize;
    if h == -1 { -2 } else { h }
}

/// Hash of a double, compatible with int hashing (mod 2**61-1).
pub fn hash_double(v: f64, identity: isize) -> isize {
    if !v.is_finite() {
        if v.is_infinite() {
            return if v > 0.0 { HASH_INF } else { -HASH_INF };
        }
        // NaN: identity-based hash
        return identity;
    }
    let (mut m, mut e) = frexp(v);
    let mut sign = 1i64;
    if m < 0.0 {
        sign = -1;
        m = -m;
    }
    let mut x: u64 = 0;
    while m != 0.0 {
        x = ((x << 28) & MODULUS) | (x >> (MODULUS_BITS - 28));
        m *= 268435456.0; // 2**28
        e -= 28;
        let y = m as u64;
        m -= y as f64;
        x += y;
        if x >= MODULUS {
            x -= MODULUS;
        }
    }
    let e = if e >= 0 {
        (e as u32) % MODULUS_BITS
    } else {
        MODULUS_BITS - 1 - ((-1 - e) as u32) % MODULUS_BITS
    };
    x = ((x << e) & MODULUS) | (x >> (MODULUS_BITS - e));
    let mut h = (x as i64) * sign;
    if h == -1 {
        h = -2;
    }
    h as isize
}

pub fn frexp(v: f64) -> (f64, i32) {
    if v == 0.0 || !v.is_finite() {
        return (v, 0);
    }
    let bits = v.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    if exp == 0 {
        // subnormal
        let (m, e) = frexp(v * 2f64.powi(64));
        return (m, e - 64);
    }
    let e = exp - 1022;
    let m = f64::from_bits((bits & !(0x7ffu64 << 52)) | (1022u64 << 52));
    (m, e)
}

/// Hash for a machine integer (matches hash(int)).
#[inline]
pub fn hash_i64(v: i64) -> isize {
    let neg = v < 0;
    let a = v.unsigned_abs();
    let mut h = (a % MODULUS) as i64;
    if neg {
        h = -h;
    }
    if h == -1 {
        h = -2;
    }
    h as isize
}

/// Tuple hash (xxHash-based, as specified for CPython 3.8+).
pub struct TupleHasher {
    acc: u64,
    len: usize,
}
const XXPRIME_1: u64 = 11400714785074694791;
const XXPRIME_2: u64 = 14029467366897019727;
const XXPRIME_5: u64 = 2870177450012600261;
impl TupleHasher {
    pub fn new(len: usize) -> Self {
        TupleHasher { acc: XXPRIME_5, len }
    }
    #[inline]
    pub fn add(&mut self, h: isize) {
        let lane = h as u64;
        self.acc = self.acc.wrapping_add(lane.wrapping_mul(XXPRIME_2));
        self.acc = self.acc.rotate_left(31);
        self.acc = self.acc.wrapping_mul(XXPRIME_1);
    }
    pub fn finish(self) -> isize {
        let acc = self.acc.wrapping_add((self.len as u64) ^ (XXPRIME_5 ^ 3527539));
        if acc == u64::MAX {
            return 1546275796;
        }
        acc as isize
    }
}

/// Pointer hash (object identity), rotated like CPython's _Py_HashPointer.
#[inline]
pub fn hash_pointer(p: usize) -> isize {
    let y = p.rotate_right(4) as isize;
    if y == -1 { -2 } else { y }
}
