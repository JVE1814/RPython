#![allow(dead_code)]
use rpy_bigint::{BigInt, MASK};

pub struct Rng(pub u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1)
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    pub fn bool(&mut self) -> bool {
        self.next() & 1 == 1
    }
    /// Random i128 with random bit length (biased to interesting values).
    pub fn i128(&mut self) -> i128 {
        let bits = self.below(127) as u32;
        let v = ((self.next() as u128) << 64 | self.next() as u128) >> (127 - bits);
        let v = v as i128;
        if self.bool() { -v } else { v }
    }
    pub fn i64(&mut self) -> i64 {
        let bits = self.below(64) as u32;
        let v = (self.next() >> (63 - bits.min(63))) as i64;
        if self.bool() { v.wrapping_neg() } else { v }
    }
    /// Random magnitude with exactly `n` digits; sometimes with long runs of 0/1 bits.
    pub fn mag(&mut self, n: usize) -> Vec<u32> {
        if n == 0 {
            return vec![];
        }
        let mode = self.below(4);
        let mut v: Vec<u32> = (0..n)
            .map(|_| match mode {
                0 => MASK,
                1 => {
                    if self.below(4) == 0 {
                        0
                    } else {
                        MASK
                    }
                }
                _ => (self.next() as u32) & MASK,
            })
            .collect();
        if v[n - 1] == 0 {
            v[n - 1] = 1 + (self.next() as u32 & (MASK - 1));
        }
        v
    }
    pub fn big_upto(&mut self, max: usize) -> BigInt {
        let n = 1 + self.below(max as u64) as usize;
        self.big(n)
    }
    pub fn mag_upto(&mut self, max: usize) -> Vec<u32> {
        let n = 1 + self.below(max as u64) as usize;
        self.mag(n)
    }
    pub fn big(&mut self, n: usize) -> BigInt {
        let m = self.mag(n);
        let neg = self.bool();
        BigInt::from_parts(neg, m)
    }
}

pub fn b(v: i128) -> BigInt {
    BigInt::from_i128(v)
}

pub fn s(x: &str) -> BigInt {
    x.parse().unwrap()
}
