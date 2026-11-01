//! Lookup structures for the generated two-byte tables (`data_*.rs`) and the
//! lazily built reverse (encoding) maps.

use std::collections::HashMap;

/// A two-byte character set: for each lead byte from `lo` on, the range of
/// trail bytes that has a slot in `data` (`(trail_lo, trail_hi, offset)`;
/// an empty row has `trail_lo > trail_hi`).  A slot holds a BMP code point,
/// `0xFFFF` (unassigned) or `0xFFFE` (look the code up in `wide`, which holds
/// supplementary code points and `PAIR | first << 16 | second` pairs).
pub struct Dbcs {
    pub lo: u8,
    pub idx: &'static [(u8, u8, u32)],
    pub data: &'static [u16],
    pub wide: &'static [(u16, u32)],
}

/// Marks a decoded value that stands for two BMP code points.
pub const PAIR: u32 = 0x8000_0000;

impl Dbcs {
    #[inline]
    pub fn get(&self, c1: u8, c2: u8) -> Option<u32> {
        let i = c1.wrapping_sub(self.lo) as usize;
        if c1 < self.lo || i >= self.idx.len() {
            return None;
        }
        let (tlo, thi, off) = self.idx[i];
        if c2 < tlo || c2 > thi {
            return None;
        }
        match self.data[off as usize + (c2 - tlo) as usize] {
            0xFFFF => None,
            0xFFFE => {
                let code = (c1 as u16) << 8 | c2 as u16;
                self.wide.binary_search_by_key(&code, |e| e.0).ok().map(|k| self.wide[k].1)
            }
            v => Some(v as u32),
        }
    }

    #[inline]
    pub fn get16(&self, code: u16) -> Option<u32> {
        self.get((code >> 8) as u8, code as u8)
    }
}

/// Append a decoded table value (one code point or a pair) to `out`.
#[inline]
pub fn push_value(out: &mut Vec<u32>, v: u32) {
    if v & PAIR != 0 {
        out.push((v >> 16) & 0x7FFF);
        out.push(v & 0xFFFF);
    } else {
        out.push(v);
    }
}

/// Reverse map code point (or pair) -> code; the first code added for a
/// code point wins.
pub struct Rev {
    bmp: Vec<u16>,
    wide: HashMap<u32, u16>,
    pairs: HashMap<(u32, u32), u16>,
    bases: Vec<u32>,
}

impl Rev {
    pub fn new() -> Rev {
        Rev { bmp: vec![0; 0x10000], wide: HashMap::new(), pairs: HashMap::new(), bases: Vec::new() }
    }

    pub fn add(&mut self, v: u32, code: u16) {
        if v & PAIR != 0 {
            let base = (v >> 16) & 0x7FFF;
            self.pairs.entry((base, v & 0xFFFF)).or_insert(code);
            if let Err(i) = self.bases.binary_search(&base) {
                self.bases.insert(i, base);
            }
        } else if v < 0x10000 {
            if self.bmp[v as usize] == 0 {
                self.bmp[v as usize] = code;
            }
        } else {
            self.wide.entry(v).or_insert(code);
        }
    }

    /// Add every code `lead << 8 | trail` (in ascending order) for which
    /// `lookup` yields a value.
    pub fn add_all(&mut self, leads: std::ops::RangeInclusive<u8>, lookup: impl Fn(u16) -> Option<u32>) -> &mut Self {
        for c1 in leads {
            for c2 in 0x21..=0xFEu8 {
                let code = (c1 as u16) << 8 | c2 as u16;
                if let Some(v) = lookup(code) {
                    self.add(v, code);
                }
            }
        }
        self
    }

    /// Forget the mapping of `v` if it currently points at `code` (for
    /// decode-only entries).
    pub fn drop_if(&mut self, v: u32, code: u16) -> &mut Self {
        if v < 0x10000 && self.bmp[v as usize] == code {
            self.bmp[v as usize] = 0;
        }
        self
    }

    #[inline]
    pub fn get(&self, c: u32) -> Option<u16> {
        if c < 0x10000 {
            match self.bmp[c as usize] {
                0 => None,
                v => Some(v),
            }
        } else {
            self.wide.get(&c).copied()
        }
    }

    #[inline]
    pub fn pair(&self, a: u32, b: u32) -> Option<u16> {
        self.pairs.get(&(a, b)).copied()
    }

    /// Code points that start a pair.
    pub fn is_pair_base(&self, c: u32) -> bool {
        !self.bases.is_empty() && self.bases.binary_search(&c).is_ok()
    }
}

impl Default for Rev {
    fn default() -> Self {
        Rev::new()
    }
}
