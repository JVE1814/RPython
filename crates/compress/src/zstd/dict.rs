//! Dictionary construction: content selection from samples (frequent substrings, most
//! valuable placed last) and entropy-table finalisation from statistics gathered by
//! compressing the samples against the content.

use super::bits::BitWriter;
use super::enc::{self, Encoder, Params};
use super::fse;
use super::huf::HufCTable;
use super::*;
use std::collections::HashMap;
use std::sync::Arc;

const K: usize = 8;
const MIN_DICT: usize = 256;

fn gram(d: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(d[i..i + K].try_into().unwrap())
}

/// Pick dictionary content (at most `budget` bytes) from the samples.
fn select_content(samples: &[&[u8]], budget: usize) -> Vec<u8> {
    // Document frequency of every 8-byte string.
    let mut df: HashMap<u64, (u32, u32)> = HashMap::new();
    for (si, s) in samples.iter().enumerate() {
        if s.len() < K {
            continue;
        }
        for i in 0..=s.len() - K {
            let e = df.entry(gram(s, i)).or_insert((0, u32::MAX));
            if e.1 != si as u32 {
                e.0 += 1;
                e.1 = si as u32;
            }
        }
    }
    // Candidate segments: maximal runs of positions whose string recurs in other samples.
    let mut cands: Vec<(u64, usize, usize, usize)> = Vec::new(); // (score, sample, start, end)
    for (si, s) in samples.iter().enumerate() {
        if s.len() < K {
            continue;
        }
        let mut i = 0;
        while i + K <= s.len() {
            let f = df[&gram(s, i)].0;
            if f < 2 {
                i += 1;
                continue;
            }
            let start = i;
            let mut score = 0u64;
            while i + K <= s.len() && i - start < 256 {
                let f = df[&gram(s, i)].0;
                if f < 2 {
                    break;
                }
                score += f as u64;
                i += 1;
            }
            let end = (i + K - 1).min(s.len());
            cands.push((score, si, start, end));
        }
    }
    cands.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut chosen: Vec<&[u8]> = Vec::new();
    let mut have: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut size = 0usize;
    for &(_, si, a, b) in &cands {
        if size >= budget {
            break;
        }
        let seg = &samples[si][a..b];
        if seg.len() < K {
            continue;
        }
        let total = seg.len() - K + 1;
        let known = (0..total).filter(|&i| have.contains(&gram(seg, i))).count();
        if known * 4 >= total * 3 {
            continue;
        }
        let seg = if size + seg.len() > budget { &seg[..budget - size] } else { seg };
        if seg.len() >= K {
            for i in 0..=seg.len() - K {
                have.insert(gram(seg, i));
            }
        }
        size += seg.len();
        chosen.push(seg);
    }
    if chosen.is_empty() {
        // Fall back to the tail of the concatenated samples.
        let mut all: Vec<u8> = samples.iter().flat_map(|s| s.iter().copied()).collect();
        let keep = all.len().min(budget);
        return all.split_off(all.len() - keep);
    }
    // Best segments go last (closest to the data, cheapest offsets).
    let mut out = Vec::with_capacity(size);
    for seg in chosen.iter().rev() {
        out.extend_from_slice(seg);
    }
    out
}

/// Statistics of compressing every sample against `content`.
fn gather(content: &[u8], samples: &[&[u8]], level: i32) -> ([u32; 256], Vec<u32>, Vec<u32>, Vec<u32>) {
    let mut lit = [0u32; 256];
    let mut ll = vec![0u32; MAX_LL_CODE + 1];
    let mut ml = vec![0u32; MAX_ML_CODE + 1];
    let mut of = vec![0u32; MAX_OF_CODE + 1];
    let cd = Arc::new(enc::CDict { id: 0, content: content.to_vec(), reps: [1, 4, 8] });
    for s in samples {
        let mut p = Params::from_level(level);
        p.content_size = true;
        let mut e = Encoder::new(p, Some((cd.clone(), false)));
        e.stats(s, &mut lit, &mut ll, &mut ml, &mut of);
    }
    (lit, ll, ml, of)
}

fn ncount_bytes(counts: &[u32], max_log: u32) -> Vec<u8> {
    let nsym = counts.iter().filter(|&&c| c > 0).count();
    let total: usize = counts.iter().map(|&c| c as usize).sum();
    let log = fse::pick_log(total, nsym, max_log).max(6).min(max_log);
    let norm = fse::normalize(counts, log);
    let mut w = BitWriter::new();
    fse::write_ncount(&norm, log, &mut w);
    w.finish()
}

/// Build a complete dictionary (header, entropy tables, content) of at most `max_size`
/// bytes around `content`.
fn assemble(content: &[u8], samples: &[&[u8]], max_size: usize, level: i32, salt: u32) -> Result<Vec<u8>, Error> {
    let (mut lit, mut ll, mut ml, mut of) = gather(content, samples, level);
    // Every length/offset code must stay representable with the dictionary's tables.
    ll.iter_mut().for_each(|c| *c += 1);
    ml.iter_mut().for_each(|c| *c += 1);
    let max_off = content.len() + (1 << 17);
    let of_max = 32 - (max_off as u32).leading_zeros();
    for (i, c) in of.iter_mut().enumerate() {
        if i as u32 <= of_max.min(MAX_OF_CODE as u32) {
            *c += 1;
        }
    }
    if lit.iter().filter(|&&c| c > 0).count() < 2 {
        lit[0] += 1;
        lit[b' ' as usize] += 1;
    }
    let mut huf = HufCTable::build(&lit).and_then(|t| t.describe());
    if huf.is_none() {
        // Restrict to the low half so the weights can be stored directly.
        let mut low = [0u32; 256];
        low[..128].copy_from_slice(&lit[..128]);
        low[0] += 1;
        low[1] += 1;
        huf = HufCTable::build(&low).and_then(|t| t.describe());
    }
    let huf = huf.ok_or(Error::DictTraining)?;
    let mut hdr = Vec::new();
    hdr.extend_from_slice(&DICT_MAGIC.to_le_bytes());
    hdr.extend_from_slice(&[0, 0, 0, 0]);
    hdr.extend_from_slice(&huf);
    hdr.extend_from_slice(&ncount_bytes(&of, OF_MAX_LOG));
    hdr.extend_from_slice(&ncount_bytes(&ml, ML_MAX_LOG));
    hdr.extend_from_slice(&ncount_bytes(&ll, LL_MAX_LOG));
    for r in [1u32, 4, 8] {
        hdr.extend_from_slice(&r.to_le_bytes());
    }
    if hdr.len() + 8 > max_size {
        return Err(Error::DictTraining);
    }
    let room = max_size - hdr.len();
    let body = if content.len() > room { &content[content.len() - room..] } else { content };
    if body.len() < 8 {
        return Err(Error::DictTraining);
    }
    // Dictionary id: a hash of the content, kept within the range zstd reserves for
    // user dictionaries.
    let h = xxh::xxh64(body, salt as u64) as u32;
    let id = 32768 + h % ((1u32 << 31) - 32768);
    hdr[4..8].copy_from_slice(&id.to_le_bytes());
    hdr.extend_from_slice(body);
    // Validate by loading.
    dec::DDict::load(&hdr, false).map_err(|_| Error::DictTraining)?;
    Ok(hdr)
}

fn split<'a>(data: &'a [u8], sizes: &[usize]) -> Vec<&'a [u8]> {
    let mut v = Vec::with_capacity(sizes.len());
    let mut p = 0;
    for &n in sizes {
        v.push(&data[p..p + n]);
        p += n;
    }
    v
}

/// Train a dictionary of at most `dict_size` bytes; `sizes` splits `data` into samples.
pub fn train(data: &[u8], sizes: &[usize], dict_size: usize) -> Result<Vec<u8>, Error> {
    if sizes.is_empty() || data.is_empty() || dict_size < MIN_DICT {
        return Err(Error::DictTraining);
    }
    let samples = split(data, sizes);
    let content = select_content(&samples, dict_size.saturating_sub(dict_size / 8 + 128).max(64));
    let mut d = assemble(&content, &samples, dict_size, 3, 0)?;
    if d.len() > dict_size {
        d.truncate(dict_size);
    }
    Ok(d)
}

/// Build a dictionary from `content` with entropy tables fitted to the samples.
pub fn finalize(content: &[u8], data: &[u8], sizes: &[usize], dict_size: usize, level: i32) -> Result<Vec<u8>, Error> {
    if sizes.is_empty() || data.is_empty() || dict_size < MIN_DICT || content.len() < 8 {
        return Err(Error::DictTraining);
    }
    let samples = split(data, sizes);
    assemble(content, &samples, dict_size, level, 0x5EED)
}

impl Encoder {
    /// Compress `data` as one frame purely to collect coding statistics.
    pub fn stats(&mut self, data: &[u8], lit: &mut [u32; 256], ll: &mut [u32], ml: &mut [u32], of: &mut [u32]) {
        let mut out = Vec::new();
        let _ = self.compress_collect(data, &mut out, &mut |l: &[u8], seqs: &[(u32, u32, u32)]| {
            for &b in l {
                lit[b as usize] += 1;
            }
            for &(a, m, o) in seqs {
                ll[enc::ll_code_pub(a) as usize] += 1;
                ml[enc::ml_code_pub(m) as usize] += 1;
                of[(31 - o.leading_zeros()) as usize] += 1;
            }
        });
    }
}
