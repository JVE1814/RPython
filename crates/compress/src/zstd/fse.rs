//! Finite State Entropy (tANS) tables: the normalized-count table description, decoding
//! tables, encoding transforms and count normalisation.

use super::bits::{BitWriter, FwdReader, RevReader};
use super::Error;

#[derive(Clone, Copy, Default)]
pub struct Cell {
    pub sym: u8,
    pub nb: u8,
    pub base: u16,
}

/// Decoding table.
#[derive(Clone)]
pub struct DTable {
    pub log: u32,
    pub cells: Vec<Cell>,
}

/// Spread the symbols of a normalized distribution over the table (shared by both sides).
/// `-1` entries ("less than one") take single cells at the end of the table.
fn spread(norm: &[i16], log: u32) -> Vec<u8> {
    let size = 1usize << log;
    let mut tab = vec![0u8; size];
    let mut high = size - 1;
    for (s, &c) in norm.iter().enumerate() {
        if c == -1 {
            tab[high] = s as u8;
            high = high.wrapping_sub(1);
        }
    }
    let step = (size >> 1) + (size >> 3) + 3;
    let mask = size - 1;
    let mut pos = 0usize;
    for (s, &c) in norm.iter().enumerate() {
        for _ in 0..c.max(0) {
            tab[pos] = s as u8;
            loop {
                pos = (pos + step) & mask;
                if pos <= high || high == usize::MAX {
                    break;
                }
            }
        }
    }
    tab
}

impl DTable {
    pub fn build(norm: &[i16], log: u32) -> DTable {
        let size = 1u32 << log;
        let syms = spread(norm, log);
        let mut next: Vec<u32> = norm.iter().map(|&c| if c == -1 { 1 } else { c.max(0) as u32 }).collect();
        let cells = syms
            .iter()
            .map(|&s| {
                let n = next[s as usize];
                next[s as usize] += 1;
                let nb = log - (31 - n.leading_zeros());
                Cell { sym: s, nb: nb as u8, base: ((n << nb) - size) as u16 }
            })
            .collect();
        DTable { log, cells }
    }

    /// Single-symbol table (RLE mode).
    pub fn rle(sym: u8) -> DTable {
        DTable { log: 0, cells: vec![Cell { sym, nb: 0, base: 0 }] }
    }
}

/// Parse a table description; returns (normalized counts, accuracy log, bytes used).
pub fn read_ncount(data: &[u8], max_sym: usize, max_log: u32) -> Result<(Vec<i16>, u32, usize), Error> {
    let mut r = FwdReader::new(data);
    let log = r.read(4)? + 5;
    if log > max_log {
        return Err(Error::Corrupt);
    }
    let mut norm: Vec<i16> = Vec::with_capacity(max_sym + 1);
    let mut remaining: i32 = (1 << log) + 1;
    let mut threshold: i32 = 1 << log;
    let mut nbits = log + 1;
    while remaining > 1 {
        if norm.len() > max_sym {
            return Err(Error::Corrupt);
        }
        let max = (2 * threshold - 1) - remaining;
        let low = r.peek(nbits - 1) as i32;
        let v = if low < max {
            r.read(nbits - 1)?;
            low
        } else {
            let mut v = r.read(nbits)? as i32;
            if v >= threshold {
                v -= max;
            }
            v
        };
        let count = v - 1;
        remaining -= count.abs();
        norm.push(count as i16);
        if count == 0 {
            loop {
                let rep = r.read(2)?;
                for _ in 0..rep {
                    norm.push(0);
                }
                if rep != 3 {
                    break;
                }
            }
        }
        while remaining < threshold && nbits > 1 {
            nbits -= 1;
            threshold >>= 1;
        }
    }
    if remaining != 1 || norm.len() > max_sym + 1 {
        return Err(Error::Corrupt);
    }
    Ok((norm, log, r.bytes_used()))
}

/// Write a table description (inverse of `read_ncount`).
pub fn write_ncount(norm: &[i16], log: u32, w: &mut BitWriter) {
    w.put((log - 5) as u64, 4);
    let mut remaining: i32 = (1 << log) + 1;
    let mut threshold: i32 = 1 << log;
    let mut nbits = log + 1;
    let last = norm.iter().rposition(|&c| c != 0).unwrap_or(0);
    let mut s = 0usize;
    while remaining > 1 && s <= last {
        let count = norm[s] as i32;
        let value = count + 1;
        let max = (2 * threshold - 1) - remaining;
        if value < max {
            w.put(value as u64, nbits - 1);
        } else if value < threshold {
            w.put(value as u64, nbits);
        } else {
            w.put((value + max) as u64, nbits);
        }
        remaining -= count.abs();
        s += 1;
        if count == 0 {
            let mut z = 0;
            while s + z <= last && norm[s + z] == 0 {
                z += 1;
            }
            s += z;
            while z >= 3 {
                w.put(3, 2);
                z -= 3;
            }
            w.put(z as u64, 2);
        }
        while remaining < threshold && nbits > 1 {
            nbits -= 1;
            threshold >>= 1;
        }
    }
}

/// Scale `counts` to a distribution summing to `1 << log`; every present symbol keeps a
/// nonzero probability.
pub fn normalize(counts: &[u32], log: u32) -> Vec<i16> {
    let total: u64 = counts.iter().map(|&c| c as u64).sum();
    let size = 1i64 << log;
    let mut norm: Vec<i16> = vec![0; counts.len()];
    if total == 0 {
        return norm;
    }
    let mut sum = 0i64;
    for (i, &c) in counts.iter().enumerate() {
        if c == 0 {
            continue;
        }
        let v = ((c as u64 * size as u64 + total / 2) / total) as i64;
        let v = v.max(1);
        norm[i] = v as i16;
        sum += v;
    }
    // Fix the total by adjusting the largest entries.
    while sum != size {
        let (imax, _) = norm.iter().enumerate().max_by_key(|&(_, &v)| v).unwrap();
        if sum > size {
            let excess = sum - size;
            let can = norm[imax] as i64 - 1;
            if can <= 0 {
                break;
            }
            let d = excess.min(can.max(1)).min((norm[imax] as i64 / 2).max(1));
            norm[imax] -= d as i16;
            sum -= d;
        } else {
            norm[imax] += (size - sum) as i16;
            sum = size;
        }
    }
    norm
}

/// Smallest sensible accuracy log for `n` samples with `nsym` distinct symbols.
pub fn pick_log(n: usize, nsym: usize, max_log: u32) -> u32 {
    let need = (usize::BITS - (nsym.max(2) - 1).leading_zeros()) + 1;
    let by_n = (usize::BITS - n.max(1).leading_zeros()).saturating_sub(1);
    by_n.max(need).max(5).min(max_log)
}

/// Encoding transform derived from a distribution.
pub struct CTable {
    pub log: u32,
    /// Per symbol: table cells (state indices) holding it, in table order.
    cells: Vec<Vec<u16>>,
    counts: Vec<u32>,
}

impl CTable {
    pub fn build(norm: &[i16], log: u32) -> CTable {
        let syms = spread(norm, log);
        let mut cells: Vec<Vec<u16>> = vec![Vec::new(); norm.len()];
        for (i, &s) in syms.iter().enumerate() {
            cells[s as usize].push(i as u16);
        }
        let counts = norm.iter().map(|&c| if c == -1 { 1 } else { c.max(0) as u32 }).collect();
        CTable { log, cells, counts }
    }

    pub fn has(&self, sym: usize) -> bool {
        sym < self.counts.len() && self.counts[sym] > 0
    }

    /// Initial state for the last symbol of a stream (no bits emitted).
    pub fn init_state(&self, sym: usize) -> u32 {
        self.cells[sym][0] as u32
    }

    /// Move from decoder state `next` back to a state holding `sym`: returns
    /// (new state, bits to emit, bit count).
    #[inline]
    pub fn encode(&self, next: u32, sym: usize) -> (u32, u32, u32) {
        let c = self.counts[sym];
        let z = next + (1 << self.log);
        let hz = 31 - z.leading_zeros();
        let hc = 31 - c.leading_zeros();
        let mut nb = hz - hc;
        let mut n = z >> nb;
        if n < c {
            nb -= 1;
            n = z >> nb;
        }
        let state = self.cells[sym][(n - c) as usize] as u32;
        (state, z & ((1 << nb) - 1), nb)
    }

    /// Approximate cost in 1/256 bits of coding `sym` (for mode decisions).
    pub fn cost(&self, sym: usize) -> u32 {
        let c = self.counts[sym].max(1) as f64;
        (((1u64 << self.log) as f64 / c).log2() * 256.0) as u32
    }
}

/// Decode `n`-ish symbols with two interleaved states (Huffman weight streams).
pub fn decode_two_states(t: &DTable, data: &[u8], max_out: usize) -> Result<Vec<u8>, Error> {
    let mut r = RevReader::new(data)?;
    let mut s1 = r.read(t.log) as usize;
    let mut s2 = r.read(t.log) as usize;
    let mut out = Vec::new();
    loop {
        let c = t.cells[s1];
        out.push(c.sym);
        s1 = c.base as usize + r.read(c.nb as u32) as usize;
        if r.overflowed() {
            out.push(t.cells[s2].sym);
            break;
        }
        let c = t.cells[s2];
        out.push(c.sym);
        s2 = c.base as usize + r.read(c.nb as u32) as usize;
        if r.overflowed() {
            out.push(t.cells[s1].sym);
            break;
        }
        if out.len() > max_out {
            return Err(Error::Corrupt);
        }
    }
    if out.len() > max_out {
        return Err(Error::Corrupt);
    }
    Ok(out)
}
