//! Bit-level readers and writers used by the Zstandard entropy coders.

use super::Error;

/// Reads a little-endian bitstream forwards (FSE table descriptions).
pub struct FwdReader<'a> {
    data: &'a [u8],
    pub bit: usize,
}

impl<'a> FwdReader<'a> {
    pub fn new(data: &'a [u8]) -> FwdReader<'a> {
        FwdReader { data, bit: 0 }
    }

    pub fn read(&mut self, n: u32) -> Result<u32, Error> {
        if n == 0 {
            return Ok(0);
        }
        if self.bit + n as usize > self.data.len() * 8 {
            return Err(Error::Corrupt);
        }
        let v = self.peek(n);
        self.bit += n as usize;
        Ok(v)
    }

    pub fn peek(&self, n: u32) -> u32 {
        let byte = self.bit / 8;
        let mut w = 0u64;
        for i in 0..8 {
            if let Some(&b) = self.data.get(byte + i) {
                w |= (b as u64) << (8 * i);
            }
        }
        ((w >> (self.bit % 8)) & ((1u64 << n) - 1)) as u32
    }

    pub fn bytes_used(&self) -> usize {
        self.bit.div_ceil(8)
    }
}

/// Reads a bitstream backwards from its end, as written by `BitWriter` plus the final
/// marker bit.  Reading past the start yields zero bits and is reported by `overflowed`.
pub struct RevReader<'a> {
    data: &'a [u8],
    /// Number of bits not yet consumed (may go negative on overflow).
    pos: isize,
}

impl<'a> RevReader<'a> {
    pub fn new(data: &'a [u8]) -> Result<RevReader<'a>, Error> {
        let Some(&last) = data.last() else { return Err(Error::Corrupt) };
        if last == 0 {
            return Err(Error::Corrupt);
        }
        let pad = last.leading_zeros() as isize + 1;
        Ok(RevReader { data, pos: data.len() as isize * 8 - pad })
    }

    #[inline(always)]
    fn window(&self, at: isize) -> u64 {
        // 64 bits starting at bit `at` (at >= 0).
        let byte = (at >> 3) as usize;
        let sh = (at & 7) as u32;
        let w = if byte + 8 <= self.data.len() {
            u64::from_le_bytes(self.data[byte..byte + 8].try_into().unwrap())
        } else {
            let mut w = 0u64;
            for i in 0..8 {
                if let Some(&b) = self.data.get(byte + i) {
                    w |= (b as u64) << (8 * i);
                }
            }
            w
        };
        w >> sh
    }

    /// Value of the next `n` (<= 56) bits without consuming them.
    #[inline(always)]
    pub fn peek(&self, n: u32) -> u64 {
        if n == 0 {
            return 0;
        }
        let start = self.pos - n as isize;
        let mask = (1u64 << n) - 1;
        if start >= 0 {
            self.window(start) & mask
        } else {
            // Bits below the start of the stream read as zero.
            let avail = self.pos.max(0);
            if avail == 0 {
                return 0;
            }
            (self.window(0) & ((1u64 << avail) - 1)) << (-start)
        }
    }

    #[inline(always)]
    pub fn read(&mut self, n: u32) -> u64 {
        let v = self.peek(n);
        self.pos -= n as isize;
        v
    }

    #[inline(always)]
    pub fn skip(&mut self, n: u32) {
        self.pos -= n as isize;
    }

    pub fn overflowed(&self) -> bool {
        self.pos < 0
    }

    pub fn finished(&self) -> bool {
        self.pos == 0
    }

    pub fn remaining(&self) -> isize {
        self.pos
    }
}

/// Little-endian bit writer (LSB first).
pub struct BitWriter {
    pub out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    pub fn new() -> BitWriter {
        BitWriter { out: Vec::new(), acc: 0, n: 0 }
    }

    #[inline(always)]
    pub fn put(&mut self, v: u64, bits: u32) {
        if bits == 0 {
            return;
        }
        debug_assert!(bits <= 32);
        self.acc |= (v & ((1u64 << bits) - 1)) << self.n;
        self.n += bits;
        if self.n >= 32 {
            self.out.extend_from_slice(&(self.acc as u32).to_le_bytes());
            self.acc >>= 32;
            self.n -= 32;
        }
    }

    /// Append the end marker bit and pad to a byte boundary (backward streams).
    pub fn finish_marked(mut self) -> Vec<u8> {
        self.put(1, 1);
        self.flush_bytes();
        self.out
    }

    /// Pad to a byte boundary.
    pub fn finish(mut self) -> Vec<u8> {
        self.flush_bytes();
        self.out
    }

    fn flush_bytes(&mut self) {
        while self.n > 0 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n = self.n.saturating_sub(8);
        }
        self.acc = 0;
    }

    pub fn bit_len(&self) -> usize {
        self.out.len() * 8 + self.n as usize
    }
}
