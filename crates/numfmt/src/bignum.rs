//! Tiny fixed-capacity unsigned big integer used for exact binary -> decimal
//! conversion.  Only the handful of operations needed by `dtoa` are provided.

/// 32-bit limbs, little endian.  128 limbs = 4096 bits, which comfortably
/// covers every intermediate value produced by the exact digit generator
/// (the worst case is about 2700 bits).
const CAP: usize = 128;

#[derive(Clone)]
pub(crate) struct Big {
    len: usize,
    d: [u32; CAP],
}

/// 5^13 is the largest power of five fitting in a u32.
const POW5_13: u32 = 1_220_703_125;
const SMALL_POW5: [u32; 14] = [
    1, 5, 25, 125, 625, 3125, 15625, 78125, 390625, 1953125, 9765625, 48828125, 244140625,
    1220703125,
];

impl Big {
    #[inline]
    pub(crate) fn from_u64(v: u64) -> Big {
        let mut b = Big { len: 0, d: [0; CAP] };
        b.d[0] = v as u32;
        b.d[1] = (v >> 32) as u32;
        b.len = 2;
        b.trim();
        b
    }

    #[inline]
    fn trim(&mut self) {
        while self.len > 0 && self.d[self.len - 1] == 0 {
            self.len -= 1;
        }
    }

    #[inline]
    pub(crate) fn is_zero(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub(crate) fn is_odd(&self) -> bool {
        self.len > 0 && (self.d[0] & 1) == 1
    }

    /// If the value fits in a u64, return it.
    #[inline]
    pub(crate) fn to_u64(&self) -> Option<u64> {
        match self.len {
            0 => Some(0),
            1 => Some(self.d[0] as u64),
            2 => Some(self.d[0] as u64 | ((self.d[1] as u64) << 32)),
            _ => None,
        }
    }

    pub(crate) fn add_small(&mut self, v: u32) {
        let mut carry = v as u64;
        let mut i = 0;
        while carry != 0 {
            if i == self.len {
                self.d[i] = carry as u32;
                self.len += 1;
                break;
            }
            let s = self.d[i] as u64 + carry;
            self.d[i] = s as u32;
            carry = s >> 32;
            i += 1;
        }
    }

    pub(crate) fn mul_small(&mut self, m: u32) {
        let mut carry: u64 = 0;
        for i in 0..self.len {
            let p = self.d[i] as u64 * m as u64 + carry;
            self.d[i] = p as u32;
            carry = p >> 32;
        }
        if carry != 0 {
            self.d[self.len] = carry as u32;
            self.len += 1;
        }
    }

    pub(crate) fn mul_pow5(&mut self, mut n: u32) {
        while n >= 13 {
            self.mul_small(POW5_13);
            n -= 13;
        }
        if n > 0 {
            self.mul_small(SMALL_POW5[n as usize]);
        }
    }

    pub(crate) fn shl(&mut self, bits: u32) {
        if self.len == 0 || bits == 0 {
            return;
        }
        let limbs = (bits / 32) as usize;
        let b = bits % 32;
        if b == 0 {
            for i in (0..self.len).rev() {
                self.d[i + limbs] = self.d[i];
            }
        } else {
            self.d[self.len + limbs] = 0;
            for i in (0..self.len).rev() {
                let v = self.d[i];
                self.d[i + limbs + 1] |= v >> (32 - b);
                self.d[i + limbs] = v << b;
            }
        }
        for i in 0..limbs {
            self.d[i] = 0;
        }
        self.len += limbs + 1;
        self.trim();
    }

    /// Shift right; return true if any shifted-out bit was nonzero.
    pub(crate) fn shr_sticky(&mut self, bits: u32) -> bool {
        if self.len == 0 || bits == 0 {
            return false;
        }
        let limbs = (bits / 32) as usize;
        let b = bits % 32;
        if limbs >= self.len {
            self.len = 0;
            return true; // value was nonzero
        }
        let mut sticky = false;
        for i in 0..limbs {
            if self.d[i] != 0 {
                sticky = true;
            }
        }
        if b != 0 && (self.d[limbs] & ((1u32 << b) - 1)) != 0 {
            sticky = true;
        }
        let n = self.len - limbs;
        for i in 0..n {
            let lo = self.d[i + limbs];
            let hi = if i + limbs + 1 < self.len { self.d[i + limbs + 1] } else { 0 };
            self.d[i] = if b == 0 { lo } else { (lo >> b) | (hi << (32 - b)) };
        }
        for i in n..self.len {
            self.d[i] = 0;
        }
        self.len = n;
        self.trim();
        sticky
    }

    /// Divide in place by a small divisor, returning the remainder.
    pub(crate) fn divrem_small(&mut self, dv: u32) -> u32 {
        let mut rem: u64 = 0;
        for i in (0..self.len).rev() {
            let cur = (rem << 32) | self.d[i] as u64;
            self.d[i] = (cur / dv as u64) as u32;
            rem = cur % dv as u64;
        }
        self.trim();
        rem as u32
    }

    /// Floor-divide by 5^n; return true if the division was inexact.
    pub(crate) fn div_pow5_sticky(&mut self, mut n: u32) -> bool {
        let mut sticky = false;
        while n >= 13 {
            if self.divrem_small(POW5_13) != 0 {
                sticky = true;
            }
            n -= 13;
        }
        if n > 0 && self.divrem_small(SMALL_POW5[n as usize]) != 0 {
            sticky = true;
        }
        sticky
    }

    /// Number of significant bits.
    #[allow(dead_code)]
    pub(crate) fn bit_len(&self) -> u32 {
        if self.len == 0 {
            0
        } else {
            (self.len as u32 - 1) * 32 + (32 - self.d[self.len - 1].leading_zeros())
        }
    }

    /// Write decimal digits (no leading zeros; empty for zero) into `out`,
    /// returning the number of digits written.  Destroys `self`.
    pub(crate) fn write_decimal(&mut self, out: &mut [u8]) -> usize {
        if let Some(v) = self.to_u64() {
            return write_u64(v, out);
        }
        // Collect base-1e9 chunks, least significant first.
        let mut chunks = [0u32; CAP * 32 / 29 + 2];
        let mut nchunks = 0;
        while !self.is_zero() {
            chunks[nchunks] = self.divrem_small(1_000_000_000);
            nchunks += 1;
        }
        let mut pos = 0;
        let first = chunks[nchunks - 1];
        pos += write_u64(first as u64, &mut out[pos..]);
        for i in (0..nchunks - 1).rev() {
            let mut c = chunks[i];
            for k in (0..9).rev() {
                out[pos + k] = b'0' + (c % 10) as u8;
                c /= 10;
            }
            pos += 9;
        }
        pos
    }
}

/// Write the decimal digits of `v` (nothing for 0) and return the count.
#[inline]
pub(crate) fn write_u64(mut v: u64, out: &mut [u8]) -> usize {
    let mut tmp = [0u8; 20];
    let mut n = 0;
    while v != 0 {
        tmp[n] = b'0' + (v % 10) as u8;
        v /= 10;
        n += 1;
    }
    for i in 0..n {
        out[i] = tmp[n - 1 - i];
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(b: &Big) -> String {
        let mut b = b.clone();
        let mut buf = [0u8; 2000];
        let n = b.write_decimal(&mut buf);
        if n == 0 {
            "0".to_string()
        } else {
            String::from_utf8(buf[..n].to_vec()).unwrap()
        }
    }

    #[test]
    fn basic_ops() {
        let mut b = Big::from_u64(1);
        b.mul_pow5(30);
        assert_eq!(dec(&b), "931322574615478515625");
        b.shl(30);
        assert_eq!(dec(&b), "1000000000000000000000000000000");
        let mut c2 = b.clone();
        assert!(c2.shr_sticky(31));
        assert!(!b.shr_sticky(29));
        assert_eq!(dec(&b), "1862645149230957031250");
        assert!(!b.div_pow5_sticky(30));
        assert_eq!(dec(&b), "2");
        assert!(!b.shr_sticky(1));
        assert_eq!(dec(&b), "1");
        assert!(b.shr_sticky(1));
        assert!(b.is_zero());
        let mut c = Big::from_u64(u64::MAX);
        c.add_small(1);
        assert_eq!(dec(&c), "18446744073709551616");
        c.shl(64);
        assert_eq!(c.bit_len(), 129);
    }
}
