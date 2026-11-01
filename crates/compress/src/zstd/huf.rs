//! Huffman coding of literals: tree descriptions (weights), decoding tables and stream
//! encoding.  Codes are assigned in order of increasing weight (longest codes first),
//! symbols ascending within a weight.

use super::bits::{BitWriter, RevReader};
use super::fse;
use super::Error;

pub const MAX_BITS: u32 = 11;

#[derive(Clone)]
pub struct HufDTable {
    pub max_bits: u32,
    /// (symbol, code length) indexed by the next `max_bits` bits.
    pub entries: Vec<(u8, u8)>,
}

fn table_from_weights(weights: &[u8]) -> Result<HufDTable, Error> {
    // weights: explicit weights of all but the last symbol.
    if weights.is_empty() || weights.len() > 255 {
        return Err(Error::Corrupt);
    }
    let mut total: u32 = 0;
    for &w in weights {
        if w as u32 > MAX_BITS {
            return Err(Error::Corrupt);
        }
        if w > 0 {
            total += 1 << (w - 1);
        }
    }
    if total == 0 {
        return Err(Error::Corrupt);
    }
    let max_bits = 32 - total.leading_zeros();
    if max_bits > MAX_BITS {
        return Err(Error::Corrupt);
    }
    let rest = (1u32 << max_bits) - total;
    if !rest.is_power_of_two() {
        return Err(Error::Corrupt);
    }
    let last_w = rest.trailing_zeros() + 1;
    let mut all: Vec<u8> = weights.to_vec();
    all.push(last_w as u8);
    let size = 1usize << max_bits;
    let mut entries = Vec::with_capacity(size);
    for w in 1..=max_bits as u8 {
        for (s, &sw) in all.iter().enumerate() {
            if sw == w {
                let len = (max_bits + 1 - w as u32) as u8;
                for _ in 0..(1usize << (w - 1)) {
                    entries.push((s as u8, len));
                }
            }
        }
    }
    if entries.len() != size {
        return Err(Error::Corrupt);
    }
    Ok(HufDTable { max_bits, entries })
}

/// Parse a tree description; returns the table and the bytes used.
pub fn read_table(data: &[u8]) -> Result<(HufDTable, usize), Error> {
    let Some(&h) = data.first() else { return Err(Error::Corrupt) };
    if h >= 128 {
        let n = (h - 127) as usize;
        let bytes = n.div_ceil(2);
        if data.len() < 1 + bytes {
            return Err(Error::Corrupt);
        }
        let mut w = Vec::with_capacity(n);
        for i in 0..n {
            let b = data[1 + i / 2];
            w.push(if i % 2 == 0 { b >> 4 } else { b & 15 });
        }
        Ok((table_from_weights(&w)?, 1 + bytes))
    } else {
        let size = h as usize;
        if data.len() < 1 + size || size == 0 {
            return Err(Error::Corrupt);
        }
        let blob = &data[1..1 + size];
        let (norm, log, used) = fse::read_ncount(blob, 255, 6)?;
        if used >= blob.len() {
            return Err(Error::Corrupt);
        }
        let t = fse::DTable::build(&norm, log);
        let w = fse::decode_two_states(&t, &blob[used..], 255)?;
        Ok((table_from_weights(&w)?, 1 + size))
    }
}

/// Decode one stream of `n` symbols into `out`.
pub fn decode_stream(t: &HufDTable, data: &[u8], n: usize, out: &mut Vec<u8>) -> Result<(), Error> {
    let mut r = RevReader::new(data)?;
    let mb = t.max_bits;
    out.reserve(n);
    for _ in 0..n {
        let (s, l) = t.entries[r.peek(mb) as usize];
        r.skip(l as u32);
        out.push(s);
    }
    if !r.finished() {
        return Err(Error::Corrupt);
    }
    Ok(())
}

/// Decode a literals payload (1 or 4 streams).
pub fn decode_literals(t: &HufDTable, data: &[u8], regen: usize, four: bool) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity(regen);
    if !four {
        decode_stream(t, data, regen, &mut out)?;
        return Ok(out);
    }
    if data.len() < 6 {
        return Err(Error::Corrupt);
    }
    let s1 = u16::from_le_bytes([data[0], data[1]]) as usize;
    let s2 = u16::from_le_bytes([data[2], data[3]]) as usize;
    let s3 = u16::from_le_bytes([data[4], data[5]]) as usize;
    let body = &data[6..];
    if s1 + s2 + s3 > body.len() {
        return Err(Error::Corrupt);
    }
    let seg = regen.div_ceil(4);
    if seg * 3 > regen {
        return Err(Error::Corrupt);
    }
    let parts = [&body[..s1], &body[s1..s1 + s2], &body[s1 + s2..s1 + s2 + s3], &body[s1 + s2 + s3..]];
    for (i, p) in parts.iter().enumerate() {
        let n = if i < 3 { seg } else { regen - 3 * seg };
        decode_stream(t, p, n, &mut out)?;
    }
    Ok(out)
}

// ------------------------------------------------------------------ encoder

pub struct HufCTable {
    /// (code, length) per symbol; length 0 = absent.
    pub codes: Vec<(u32, u8)>,
    pub max_bits: u32,
    pub weights: Vec<u8>,
}

impl HufCTable {
    /// Build from symbol counts (at least two distinct symbols).
    pub fn build(counts: &[u32; 256]) -> Option<HufCTable> {
        let last = counts.iter().rposition(|&c| c > 0)?;
        if counts.iter().filter(|&&c| c > 0).count() < 2 {
            return None;
        }
        let lens = crate::huffman::code_lengths(&counts[..=last], MAX_BITS, 0);
        let max_bits = *lens.iter().max().unwrap() as u32;
        let weights: Vec<u8> = lens.iter().map(|&l| if l == 0 { 0 } else { (max_bits + 1 - l as u32) as u8 }).collect();
        // Kraft equality is required (the last weight is implied).
        let kraft: u64 = lens.iter().filter(|&&l| l > 0).map(|&l| 1u64 << (max_bits - l as u32)).sum();
        if kraft != 1u64 << max_bits {
            return None;
        }
        let mut codes = vec![(0u32, 0u8); last + 1];
        let mut start = 0u32;
        for w in 1..=max_bits as u8 {
            for s in 0..=last {
                if weights[s] == w {
                    codes[s] = (start >> (w - 1), (max_bits + 1 - w as u32) as u8);
                    start += 1 << (w - 1);
                }
            }
        }
        Some(HufCTable { codes, max_bits, weights })
    }

    /// Serialized tree description (None if it cannot be represented).
    pub fn describe(&self) -> Option<Vec<u8>> {
        let n = self.weights.len() - 1; // the last weight is implied
        if n == 0 {
            return None;
        }
        if n <= 128 {
            let mut v = vec![(127 + n) as u8];
            for pair in self.weights[..n].chunks(2) {
                let hi = pair[0];
                let lo = if pair.len() > 1 { pair[1] } else { 0 };
                v.push((hi << 4) | lo);
            }
            return Some(v);
        }
        self.describe_fse(n)
    }

    fn describe_fse(&self, n: usize) -> Option<Vec<u8>> {
        let ws = &self.weights[..n];
        let mut counts = [0u32; 13];
        for &w in ws {
            counts[w as usize] += 1;
        }
        let nsym = counts.iter().filter(|&&c| c > 0).count();
        if nsym < 2 {
            return None;
        }
        let log = fse::pick_log(n, nsym, 6);
        let last = counts.iter().rposition(|&c| c > 0).unwrap();
        let norm = fse::normalize(&counts[..=last], log);
        let ct = fse::CTable::build(&norm, log);
        let mut hdr = BitWriter::new();
        fse::write_ncount(&norm, log, &mut hdr);
        let mut blob = hdr.finish();
        // Two interleaved states: symbols at even positions use state 1.  Encode backwards.
        let mut w = BitWriter::new();
        let mut st = [0u32; 2];
        let k_last = n - 1;
        let k_prev = n - 2;
        st[k_last % 2] = ct.init_state(ws[k_last] as usize);
        st[k_prev % 2] = ct.init_state(ws[k_prev] as usize);
        // The decoder stops right after the state update following the second-to-last
        // symbol overflows; mirror zstd's layout: states are updated for every symbol
        // except the final two.
        for k in (0..k_prev).rev() {
            let which = k % 2;
            let (ns, bits, nb) = ct.encode(st[which], ws[k] as usize);
            w.put(bits as u64, nb);
            st[which] = ns;
        }
        w.put(st[1] as u64, log);
        w.put(st[0] as u64, log);
        let stream = w.finish_marked();
        blob.extend_from_slice(&stream);
        if blob.len() >= 128 {
            return None;
        }
        // Verify (the termination rule depends on exact bit counts).
        let t = fse::DTable::build(&norm, log);
        let dec = fse::decode_two_states(&t, &stream, 255).ok()?;
        if dec != ws {
            return None;
        }
        let mut v = vec![blob.len() as u8];
        v.extend_from_slice(&blob);
        Some(v)
    }

    fn encode_stream(&self, data: &[u8]) -> Vec<u8> {
        let mut w = BitWriter::new();
        for &b in data.iter().rev() {
            let (c, l) = self.codes[b as usize];
            w.put(c as u64, l as u32);
        }
        w.finish_marked()
    }

    /// Encoded payload size estimate in bytes.
    pub fn estimate(&self, counts: &[u32; 256]) -> usize {
        let bits: u64 = counts.iter().enumerate().filter(|&(s, _)| s < self.codes.len()).map(|(s, &c)| c as u64 * self.codes[s].1 as u64).sum();
        (bits / 8) as usize + 10
    }

    /// Encode literals as 1 or 4 streams.
    pub fn encode(&self, data: &[u8], four: bool) -> Vec<u8> {
        if !four {
            return self.encode_stream(data);
        }
        let seg = data.len().div_ceil(4);
        let parts = [&data[..seg], &data[seg..2 * seg], &data[2 * seg..3 * seg], &data[3 * seg..]];
        let enc: Vec<Vec<u8>> = parts.iter().map(|p| self.encode_stream(p)).collect();
        let mut out = Vec::new();
        for e in &enc[..3] {
            out.extend_from_slice(&(e.len() as u16).to_le_bytes());
        }
        for e in &enc {
            out.extend_from_slice(e);
        }
        out
    }
}
