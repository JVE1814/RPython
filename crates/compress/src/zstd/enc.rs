//! Zstandard encoder: hash-chain match finding (greedy or lazy by level), Huffman literals,
//! FSE-coded sequences with per-block predefined / RLE / custom tables.

use super::bits::BitWriter;
use super::fse::{self, CTable};
use super::huf::HufCTable;
use super::xxh::Xxh64;
use super::*;
use std::sync::{Arc, OnceLock};

/// Compression parameters (names follow the zstd advanced API).
#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub level: i32,
    pub window_log: u32,
    pub hash_log: u32,
    pub chain_log: u32,
    pub search_log: u32,
    pub min_match: u32,
    pub target_length: u32,
    pub strategy: u32,
    pub checksum: bool,
    pub content_size: bool,
    pub dict_id: bool,
}

impl Params {
    pub fn from_level(level: i32) -> Params {
        let l = if level == 0 { CLEVEL_DEFAULT } else { level };
        let lv = l.clamp(1, 22) as u32;
        // (window_log, hash_log, search_log, min_match, strategy) by level
        const T: [(u32, u32, u32, u32, u32); 23] = [
            (19, 12, 0, 6, 1),
            (19, 13, 0, 6, 1),
            (20, 15, 1, 5, 2),
            (21, 16, 1, 5, 3),
            (21, 17, 2, 5, 3),
            (21, 17, 2, 5, 4),
            (21, 17, 3, 5, 4),
            (21, 18, 3, 5, 4),
            (21, 18, 4, 5, 5),
            (22, 18, 4, 5, 5),
            (22, 18, 5, 5, 5),
            (22, 18, 5, 5, 5),
            (22, 19, 5, 5, 5),
            (22, 19, 5, 5, 6),
            (22, 19, 6, 4, 6),
            (22, 19, 6, 4, 6),
            (22, 20, 6, 4, 7),
            (23, 20, 7, 4, 7),
            (23, 20, 7, 4, 8),
            (23, 20, 8, 4, 8),
            (25, 21, 8, 4, 9),
            (26, 21, 9, 4, 9),
            (27, 22, 9, 4, 9),
        ];
        let (w, h, s, m, st) = T[lv as usize];
        Params {
            level,
            window_log: w,
            hash_log: h,
            chain_log: w.min(24),
            search_log: s,
            min_match: m,
            target_length: if l < 0 { (-l) as u32 } else { 0 },
            strategy: st,
            checksum: false,
            content_size: true,
            dict_id: true,
        }
    }
}

/// A dictionary prepared for compression.
pub struct CDict {
    pub id: u32,
    pub content: Vec<u8>,
    pub reps: [u32; 3],
}

impl CDict {
    pub fn load(data: &[u8], raw: bool) -> Result<CDict, Error> {
        let d = dec::DDict::load(data, raw)?;
        let reps = d.entropy.as_ref().map(|e| e.reps).unwrap_or([1, 4, 8]);
        Ok(CDict { id: d.id, content: d.content, reps })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Continue,
    Flush,
    End,
}

struct Seq {
    ll: u32,
    ml: u32,
    ov: u32,
}

const MIN_MATCH: usize = 4;

pub struct Encoder {
    pub p: Params,
    dict: Option<Arc<CDict>>,
    /// The dictionary applies to the next frame only (prefix mode).
    dict_once: bool,
    in_frame: bool,
    pub pledged: Option<u64>,
    frame_in: u64,
    /// History (dictionary content + frame input) followed by pending input.
    buf: Vec<u8>,
    /// Next position to encode.
    pos: usize,
    head: Vec<u32>,
    chain: Vec<u32>,
    inserted: usize,
    reps: [u32; 3],
    xxh: Xxh64,
    dict_len: usize,
    /// Frame header not yet written (it goes out with the first block): its content size.
    header_due: Option<Option<u64>>,
}

impl Encoder {
    pub fn new(p: Params, dict: Option<(Arc<CDict>, bool)>) -> Encoder {
        let (dict, once) = match dict {
            Some((d, once)) => (Some(d), once),
            None => (None, false),
        };
        Encoder {
            p,
            dict,
            dict_once: once,
            in_frame: false,
            pledged: None,
            frame_in: 0,
            buf: Vec::new(),
            pos: 0,
            head: Vec::new(),
            chain: Vec::new(),
            inserted: 0,
            reps: [1, 4, 8],
            xxh: Xxh64::new(0),
            dict_len: 0,
            header_due: None,
        }
    }

    pub fn in_frame(&self) -> bool {
        self.in_frame
    }

    fn start_frame(&mut self) {
        self.in_frame = true;
        self.frame_in = 0;
        self.buf.clear();
        self.pos = 0;
        self.inserted = 0;
        self.chain.clear();
        let hl = self.p.hash_log.clamp(10, 24);
        if self.head.len() != 1 << hl {
            self.head = vec![0; 1 << hl];
        } else {
            self.head.iter_mut().for_each(|v| *v = 0);
        }
        self.reps = [1, 4, 8];
        self.xxh = Xxh64::new(0);
        self.dict_len = 0;
        if let Some(d) = &self.dict {
            self.buf.extend_from_slice(&d.content);
            self.pos = d.content.len();
            self.dict_len = d.content.len();
            self.reps = d.reps;
        }
        let pos = self.pos;
        self.insert_upto(pos.saturating_sub(0));
    }

    /// Feed data; produce output according to `mode`.
    pub fn compress(&mut self, data: &[u8], mode: Mode, out: &mut Vec<u8>) -> Result<(), Error> {
        let fresh = !self.in_frame;
        if fresh {
            if mode == Mode::Continue && data.is_empty() {
                return Ok(());
            }
            if mode == Mode::Flush && data.is_empty() {
                return Ok(());
            }
            self.start_frame();
        }
        if let Some(pl) = self.pledged {
            if self.frame_in + data.len() as u64 > pl {
                return Err(Error::SrcSizeWrong);
            }
        }
        self.buf.extend_from_slice(data);
        self.frame_in += data.len() as u64;
        if self.p.checksum {
            self.xxh.update(data);
        }
        if fresh {
            let known = match (mode, self.pledged) {
                (_, Some(pl)) => Some(pl),
                (Mode::End, None) => Some(self.frame_in),
                _ => None,
            };
            self.header_due = Some(known);
        }
        match mode {
            Mode::Continue => {
                while self.buf.len() - self.pos > BLOCK_MAX {
                    self.block(BLOCK_MAX, false, out);
                }
            }
            Mode::Flush => {
                while self.buf.len() > self.pos {
                    let n = (self.buf.len() - self.pos).min(BLOCK_MAX);
                    self.block(n, false, out);
                }
            }
            Mode::End => {
                if let Some(pl) = self.pledged {
                    if pl != self.frame_in {
                        return Err(Error::SrcSizeWrong);
                    }
                }
                loop {
                    let n = (self.buf.len() - self.pos).min(BLOCK_MAX);
                    let last = self.buf.len() - self.pos == n;
                    self.block(n, last, out);
                    if last {
                        break;
                    }
                }
                if self.p.checksum {
                    out.extend_from_slice(&(self.xxh.digest() as u32).to_le_bytes());
                }
                self.in_frame = false;
                self.pledged = None;
                if self.dict_once {
                    self.dict = None;
                    self.dict_once = false;
                }
                self.buf = Vec::new();
                self.chain = Vec::new();
            }
        }
        self.slide();
        Ok(())
    }

    fn write_header(&mut self, content: Option<u64>, out: &mut Vec<u8>) {
        out.extend_from_slice(&FRAME_MAGIC.to_le_bytes());
        let window = 1u64 << self.p.window_log;
        let content = if self.p.content_size { content } else { None };
        let single = content.is_some_and(|c| c <= window);
        let did = match &self.dict {
            Some(d) if d.id != 0 && self.p.dict_id => d.id,
            _ => 0,
        };
        let did_flag = if did == 0 {
            0
        } else if did < 256 {
            1
        } else if did < 65536 {
            2
        } else {
            3
        };
        let (fcs_flag, fcs_bytes): (u8, usize) = match content {
            None => (0, 0),
            Some(c) if single && c < 256 => (0, 1),
            Some(c) if (256..65536 + 256).contains(&c) => (1, 2),
            Some(c) if c <= u32::MAX as u64 => (2, 4),
            Some(_) => (3, 8),
        };
        let fhd = (fcs_flag << 6) | ((single as u8) << 5) | ((self.p.checksum as u8) << 2) | did_flag;
        out.push(fhd);
        if !single {
            out.push(((self.p.window_log - 10) << 3) as u8);
        }
        let nd = [0usize, 1, 2, 4][did_flag as usize];
        out.extend_from_slice(&did.to_le_bytes()[..nd]);
        if let Some(c) = content {
            let v = if fcs_bytes == 2 { c - 256 } else { c };
            out.extend_from_slice(&v.to_le_bytes()[..fcs_bytes]);
        }
    }

    // ------------------------------------------------------------- match finding

    #[inline]
    fn hash(&self, i: usize) -> usize {
        let v = u32::from_le_bytes(self.buf[i..i + 4].try_into().unwrap());
        (v.wrapping_mul(0x9E37_79B1) >> (32 - self.p.hash_log.clamp(10, 24))) as usize
    }

    fn insert_upto(&mut self, upto: usize) {
        let lim = upto.min(self.buf.len().saturating_sub(3));
        if self.chain.len() < self.buf.len() {
            self.chain.resize(self.buf.len(), 0);
        }
        while self.inserted < lim {
            let i = self.inserted;
            let h = self.hash(i);
            self.chain[i] = self.head[h];
            self.head[h] = i as u32 + 1;
            self.inserted += 1;
        }
    }

    #[inline]
    fn mlen(&self, a: usize, b: usize, max: usize) -> usize {
        let buf = &self.buf;
        let mut n = 0;
        while n + 8 <= max {
            let x = u64::from_le_bytes(buf[a + n..a + n + 8].try_into().unwrap());
            let y = u64::from_le_bytes(buf[b + n..b + n + 8].try_into().unwrap());
            let d = x ^ y;
            if d != 0 {
                return n + (d.trailing_zeros() / 8) as usize;
            }
            n += 8;
        }
        while n < max && buf[a + n] == buf[b + n] {
            n += 1;
        }
        n
    }

    fn max_offset(&self, p: usize) -> usize {
        (1usize << self.p.window_log).min(p)
    }

    /// Longest match at `p` with at most `avail` bytes: (len, offset).
    fn find(&mut self, p: usize, avail: usize) -> (usize, usize) {
        if avail < MIN_MATCH || p + 4 > self.buf.len() {
            return (0, 0);
        }
        self.insert_upto(p);
        let maxoff = self.max_offset(p);
        let mut best = (0usize, 0usize);
        let mut v = self.head[self.hash(p)] as usize;
        let mut depth = 1u32 << self.p.search_log.min(10);
        let target = if self.p.target_length > 0 && self.p.level > 0 { self.p.target_length as usize } else { 256 };
        while v > 0 && depth > 0 {
            let c = v - 1;
            let off = p - c;
            if off > maxoff {
                break;
            }
            if self.buf[c + best.0.min(avail - 1)] == self.buf[p + best.0.min(avail - 1)] {
                let l = self.mlen(c, p, avail);
                if l > best.0 {
                    best = (l, off);
                    if l >= target || l == avail {
                        break;
                    }
                }
            }
            v = self.chain[c] as usize;
            depth -= 1;
        }
        if best.0 < (self.p.min_match as usize).clamp(MIN_MATCH, 7) { (0, 0) } else { best }
    }

    fn rep_match(&self, p: usize, avail: usize, ll: usize) -> (usize, usize) {
        let mut best = (0usize, 0usize);
        if avail < 3 {
            return best;
        }
        let cands = [self.reps[0] as usize, self.reps[1] as usize, self.reps[2] as usize, (self.reps[0] as usize).wrapping_sub(1)];
        for (i, &off) in cands.iter().enumerate() {
            if off == 0 || off > p || off > self.max_offset(p) {
                continue;
            }
            // reps[0] is only codable as a repeat after literals; reps[0]-1 only without.
            if (i == 0 && ll == 0) || (i == 3 && ll != 0) {
                continue;
            }
            let l = self.mlen(p - off, p, avail);
            if l > best.0 {
                best = (l, off);
            }
        }
        if best.0 < 3 { (0, 0) } else { best }
    }

    fn offset_value(&mut self, off: u32, ll: u32) -> u32 {
        let r = self.reps;
        if ll > 0 {
            if off == r[0] {
                return 1;
            }
            if off == r[1] {
                self.reps = [r[1], r[0], r[2]];
                return 2;
            }
            if off == r[2] {
                self.reps = [r[2], r[0], r[1]];
                return 3;
            }
        } else {
            if off == r[1] {
                self.reps = [r[1], r[0], r[2]];
                return 1;
            }
            if off == r[2] {
                self.reps = [r[2], r[0], r[1]];
                return 2;
            }
            if r[0] > 1 && off == r[0] - 1 {
                self.reps = [r[0] - 1, r[0], r[1]];
                return 3;
            }
        }
        self.reps = [off, r[0], r[1]];
        off + 3
    }

    /// Parse `[a, b)` into sequences and literals.
    fn parse(&mut self, a: usize, b: usize) -> (Vec<Seq>, Vec<u8>) {
        let mut seqs = Vec::new();
        let mut lits = Vec::new();
        let mut p = a;
        let mut anchor = a;
        let lazy = self.p.strategy >= 4;
        let fast_skip = self.p.level < 0;
        while p + MIN_MATCH <= b {
            let avail = b - p;
            let ll = p - anchor;
            let (rl, ro) = self.rep_match(p, avail, ll);
            let (mut ml, mut mo) = self.find(p, avail);
            if rl >= ml.max(1) || (rl + 1 >= ml && rl >= 4) {
                ml = rl;
                mo = ro;
            }
            if ml == 0 {
                p += if fast_skip { 1 + ((p - anchor) >> 5).min(16) } else { 1 };
                continue;
            }
            if lazy && p + 1 + MIN_MATCH <= b {
                let (nl, no) = self.find(p + 1, b - p - 1);
                let gain_now = ml * 4 - (32 - (mo as u32).leading_zeros()) as usize;
                let gain_next = nl * 4 - (32 - (no.max(1) as u32).leading_zeros()) as usize;
                if nl > ml && gain_next > gain_now + 4 {
                    p += 1;
                    continue;
                }
            }
            // Extend backwards over literals.
            while p > anchor && p - mo > 0 && self.buf[p - 1] == self.buf[p - mo - 1] {
                p -= 1;
                ml += 1;
            }
            let ll = (p - anchor) as u32;
            lits.extend_from_slice(&self.buf[anchor..p]);
            let ov = self.offset_value(mo as u32, ll);
            seqs.push(Seq { ll, ml: ml as u32, ov });
            p += ml;
            anchor = p;
        }
        lits.extend_from_slice(&self.buf[anchor..b]);
        self.insert_upto(b.min(self.buf.len()));
        (seqs, lits)
    }

    // ------------------------------------------------------------- block coding

    fn block(&mut self, n: usize, last: bool, out: &mut Vec<u8>) {
        let a = self.pos;
        let b = a + n;
        self.pos = b;
        if let Some(cs) = self.header_due.take() {
            self.write_header(cs, out);
        }
        let lastbit = last as u32;
        if n == 0 {
            out.extend_from_slice(&[lastbit as u8, 0, 0]);
            return;
        }
        let data_first = self.buf[a];
        if n > 16 && self.buf[a..b].iter().all(|&x| x == data_first) {
            let h = lastbit | (1 << 1) | ((n as u32) << 3);
            out.extend_from_slice(&h.to_le_bytes()[..3]);
            out.push(data_first);
            self.insert_upto(b);
            return;
        }
        let saved = self.reps;
        let (seqs, lits) = self.parse(a, b);
        let body = encode_block_body(&seqs, &lits);
        if body.len() >= n {
            self.reps = saved;
            let h = lastbit | ((n as u32) << 3);
            out.extend_from_slice(&h.to_le_bytes()[..3]);
            out.extend_from_slice(&self.buf[a..b]);
            return;
        }
        let h = lastbit | (2 << 1) | ((body.len() as u32) << 3);
        out.extend_from_slice(&h.to_le_bytes()[..3]);
        out.extend_from_slice(&body);
    }

    /// Forget history beyond the window.
    fn slide(&mut self) {
        let w = 1usize << self.p.window_log;
        if self.pos < 2 * w + (1 << 20) {
            return;
        }
        let cut = self.pos - w;
        let cut32 = cut as u32;
        let fix = |v: &mut u32| *v = if *v > cut32 { *v - cut32 } else { 0 };
        self.head.iter_mut().for_each(fix);
        self.chain.drain(..cut.min(self.chain.len()));
        self.chain.iter_mut().for_each(fix);
        self.buf.drain(..cut);
        self.pos -= cut;
        self.inserted -= cut.min(self.inserted);
        self.dict_len = self.dict_len.saturating_sub(cut);
    }
}

impl Encoder {
    /// Run the match finder over `data` as one frame, reporting each block's literals and
    /// (literal length, match length, offset value) triples instead of encoding them.
    pub fn compress_collect(&mut self, data: &[u8], _out: &mut Vec<u8>, cb: &mut dyn FnMut(&[u8], &[(u32, u32, u32)])) -> Result<(), Error> {
        self.start_frame();
        self.buf.extend_from_slice(data);
        while self.pos < self.buf.len() {
            let a = self.pos;
            let b = (a + BLOCK_MAX).min(self.buf.len());
            self.pos = b;
            let (seqs, lits) = self.parse(a, b);
            let t: Vec<(u32, u32, u32)> = seqs.iter().map(|s| (s.ll, s.ml, s.ov)).collect();
            cb(&lits, &t);
        }
        self.in_frame = false;
        Ok(())
    }
}

pub fn ll_code_pub(ll: u32) -> u32 {
    ll_code(ll)
}

pub fn ml_code_pub(ml: u32) -> u32 {
    ml_code(ml)
}

// ------------------------------------------------------------------ entropy coding

fn ll_code(ll: u32) -> u32 {
    if ll < 16 {
        return ll;
    }
    if ll >= 64 {
        return (31 - ll.leading_zeros()) + 19;
    }
    const T: [u8; 48] = [
        16, 16, 17, 17, 18, 18, 19, 19, 20, 20, 20, 20, 21, 21, 21, 21, 22, 22, 22, 22, 22, 22, 22, 22, 23, 23, 23, 23, 23, 23, 23, 23, 24, 24,
        24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24,
    ];
    T[(ll - 16) as usize] as u32
}

fn ml_code(ml: u32) -> u32 {
    let m = ml - 3;
    if m < 32 {
        return m;
    }
    if m >= 128 {
        return (31 - m.leading_zeros()) + 36;
    }
    let mut c = 32;
    while c + 1 < ML_BASE.len() && ML_BASE[c + 1] <= ml {
        c += 1;
    }
    c as u32
}

fn predefined_ct(kind: usize) -> &'static CTable {
    static T: OnceLock<[CTable; 3]> = OnceLock::new();
    &T.get_or_init(|| [CTable::build(&LL_DEFAULT, 6), CTable::build(&OF_DEFAULT, 5), CTable::build(&ML_DEFAULT, 6)])[kind]
}

enum Table<'a> {
    Rle,
    Ct(&'a CTable),
}

/// Choose the coding mode for one symbol stream: returns (mode, table description bytes,
/// owned table if custom, rle symbol).
fn choose(codes: &[u8], kind: usize, max_log: u32) -> (u8, Vec<u8>, Option<CTable>) {
    let nmax = codes.iter().copied().max().unwrap_or(0) as usize;
    let mut counts = vec![0u32; nmax + 1];
    for &c in codes {
        counts[c as usize] += 1;
    }
    let nsym = counts.iter().filter(|&&c| c > 0).count();
    if nsym == 1 && codes.len() > 2 {
        return (1, vec![codes[0]], None);
    }
    let pre = predefined_ct(kind);
    let pre_ok = codes.iter().all(|&c| pre.has(c as usize));
    let pre_cost: u64 = if pre_ok { counts.iter().enumerate().filter(|&(_, &c)| c > 0).map(|(s, &c)| c as u64 * pre.cost(s) as u64).sum() } else { u64::MAX };
    if codes.len() < 8 && pre_ok {
        return (0, Vec::new(), None);
    }
    let log = fse::pick_log(codes.len(), nsym, max_log);
    let norm = fse::normalize(&counts, log);
    let ct = CTable::build(&norm, log);
    let mut w = BitWriter::new();
    fse::write_ncount(&norm, log, &mut w);
    let desc = w.finish();
    let cost: u64 = counts.iter().enumerate().filter(|&(_, &c)| c > 0).map(|(s, &c)| c as u64 * ct.cost(s) as u64).sum::<u64>() + desc.len() as u64 * 8 * 256;
    if pre_ok && pre_cost <= cost {
        return (0, Vec::new(), None);
    }
    (2, desc, Some(ct))
}

fn encode_literals(lits: &[u8], out: &mut Vec<u8>) {
    let n = lits.len();
    let raw = |out: &mut Vec<u8>, ltype: u8| {
        if n < 32 {
            out.push(ltype | ((n as u8) << 3));
        } else if n < 4096 {
            out.push(ltype | (1 << 2) | (((n & 15) as u8) << 4));
            out.push((n >> 4) as u8);
        } else {
            out.push(ltype | (3 << 2) | (((n & 15) as u8) << 4));
            out.push((n >> 4) as u8);
            out.push((n >> 12) as u8);
        }
    };
    if n > 1 && lits.iter().all(|&x| x == lits[0]) {
        raw(out, 1);
        out.push(lits[0]);
        return;
    }
    if n >= 64 {
        let mut counts = [0u32; 256];
        for &b in lits {
            counts[b as usize] += 1;
        }
        if let Some(t) = HufCTable::build(&counts) {
            if t.estimate(&counts) < n - n / 16 {
                if let Some(desc) = t.describe() {
                    let four = n > 1023;
                    let mut payload = desc;
                    payload.extend_from_slice(&t.encode(lits, four));
                    let c = payload.len();
                    let (sf, hl, bits) = if !four {
                        (0u64, 3usize, 10u32)
                    } else if n < 1024 && c < 1024 {
                        (1, 3, 10)
                    } else if n < (1 << 14) && c < (1 << 14) {
                        (2, 4, 14)
                    } else {
                        (3, 5, 18)
                    };
                    if c < (1 << bits) && c + hl < n + if n < 32 { 1 } else if n < 4096 { 2 } else { 3 } {
                        let v: u64 = 2 | (sf << 2) | ((n as u64) << 4) | ((c as u64) << (4 + bits));
                        out.extend_from_slice(&v.to_le_bytes()[..hl]);
                        out.extend_from_slice(&payload);
                        return;
                    }
                }
            }
        }
    }
    raw(out, 0);
    out.extend_from_slice(lits);
}

fn encode_block_body(seqs: &[Seq], lits: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    encode_literals(lits, &mut out);
    let n = seqs.len();
    if n < 128 {
        out.push(n as u8);
    } else if n < 0x7F00 {
        out.push(((n >> 8) + 128) as u8);
        out.push(n as u8);
    } else {
        out.push(255);
        out.extend_from_slice(&((n - 0x7F00) as u16).to_le_bytes());
    }
    if n == 0 {
        return out;
    }
    let llc: Vec<u8> = seqs.iter().map(|s| ll_code(s.ll) as u8).collect();
    let mlc: Vec<u8> = seqs.iter().map(|s| ml_code(s.ml) as u8).collect();
    let ofc: Vec<u8> = seqs.iter().map(|s| (31 - s.ov.leading_zeros()) as u8).collect();
    let (llm, lld, llt) = choose(&llc, 0, LL_MAX_LOG);
    let (ofm, ofd, oft) = choose(&ofc, 1, OF_MAX_LOG);
    let (mlm, mld, mlt) = choose(&mlc, 2, ML_MAX_LOG);
    out.push((llm << 6) | (ofm << 4) | (mlm << 2));
    out.extend_from_slice(&lld);
    out.extend_from_slice(&ofd);
    out.extend_from_slice(&mld);
    fn pick<'a>(m: u8, t: &'static CTable, own: &'a Option<CTable>) -> Table<'a> {
        match m {
            1 => Table::Rle,
            0 => Table::Ct(t),
            _ => Table::Ct(own.as_ref().unwrap()),
        }
    }
    let tl = pick(llm, predefined_ct(0), &llt);
    let to = pick(ofm, predefined_ct(1), &oft);
    let tm = pick(mlm, predefined_ct(2), &mlt);
    let init = |t: &Table, s: u8| -> u32 {
        match t {
            Table::Rle => 0,
            Table::Ct(c) => c.init_state(s as usize),
        }
    };
    let step = |t: &Table, st: u32, s: u8, w: &mut BitWriter| -> u32 {
        match t {
            Table::Rle => 0,
            Table::Ct(c) => {
                let (ns, bits, nb) = c.encode(st, s as usize);
                w.put(bits as u64, nb);
                ns
            }
        }
    };
    let log = |t: &Table| -> u32 {
        match t {
            Table::Rle => 0,
            Table::Ct(c) => c.log,
        }
    };
    let mut w = BitWriter::new();
    let last = n - 1;
    let extras = |w: &mut BitWriter, k: usize| {
        let s = &seqs[k];
        let l = llc[k] as usize;
        w.put((s.ll - LL_BASE[l]) as u64, LL_BITS[l] as u32);
        let m = mlc[k] as usize;
        w.put((s.ml - ML_BASE[m]) as u64, ML_BITS[m] as u32);
        let o = ofc[k] as u32;
        w.put((s.ov - (1 << o)) as u64, o);
    };
    let mut sl = init(&tl, llc[last]);
    let mut so = init(&to, ofc[last]);
    let mut sm = init(&tm, mlc[last]);
    extras(&mut w, last);
    for k in (0..last).rev() {
        so = step(&to, so, ofc[k], &mut w);
        sm = step(&tm, sm, mlc[k], &mut w);
        sl = step(&tl, sl, llc[k], &mut w);
        extras(&mut w, k);
    }
    w.put(sm as u64, log(&tm));
    w.put(so as u64, log(&to));
    w.put(sl as u64, log(&tl));
    out.extend_from_slice(&w.finish_marked());
    out
}
