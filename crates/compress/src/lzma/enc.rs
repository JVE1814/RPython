//! LZMA / LZMA2 encoders: a hash-chain match finder and a lazy parser that prefers repeated
//! distances when they are about as long as a fresh match.

use super::model::*;

/// Encoder tuning (dictionary size, literal/position bits, search effort).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LzOptions {
    pub dict_size: u32,
    pub props: Props,
    /// 1 = fast (greedy), 2 = normal (lazy).
    pub mode: u32,
    pub nice_len: u32,
    /// Match finder id (0x03 HC3, 0x04 HC4, 0x12 BT2, 0x13 BT3, 0x14 BT4); all are served by
    /// the hash-chain finder, only the minimum hashed length differs.
    pub mf: u32,
    /// Maximum chain search depth (0 = automatic).
    pub depth: u32,
}

pub const PRESET_EXTREME: u32 = 0x8000_0000;

impl LzOptions {
    /// Options for a preset level 0..=9, optionally with the extreme flag.
    pub fn preset(p: u32) -> Option<LzOptions> {
        let level = p & !PRESET_EXTREME;
        if level > 9 || (p & !PRESET_EXTREME & !0x1F) != 0 {
            return None;
        }
        let extreme = p & PRESET_EXTREME != 0;
        const DICT: [u32; 10] = [18, 20, 21, 22, 22, 23, 23, 24, 25, 26];
        const NICE: [u32; 10] = [64, 96, 128, 128, 128, 192, 273, 273, 273, 273];
        const DEPTH: [u32; 10] = [4, 8, 12, 16, 24, 32, 48, 64, 96, 128];
        let mut o = LzOptions {
            dict_size: 1 << DICT[level as usize],
            props: Props { lc: 3, lp: 0, pb: 2 },
            mode: if level <= 3 { 1 } else { 2 },
            nice_len: NICE[level as usize],
            mf: if level <= 3 { 0x04 } else { 0x14 },
            depth: DEPTH[level as usize],
        };
        if extreme {
            o.mode = 2;
            o.nice_len = 273;
            o.depth *= 4;
        }
        Some(o)
    }

    pub fn valid(&self, lzma2: bool) -> bool {
        let p = self.props;
        if p.lc > 8 || p.lp > 4 || p.pb > 4 {
            return false;
        }
        if lzma2 && p.lc + p.lp > 4 {
            return false;
        }
        if self.dict_size < 4096 || (lzma2 && self.dict_size > (3 << 29)) {
            return false;
        }
        if self.nice_len < 2 || self.nice_len > LEN_MAX as u32 {
            return false;
        }
        if !matches!(self.mode, 1 | 2) {
            return false;
        }
        matches!(self.mf, 0x03 | 0x04 | 0x12 | 0x13 | 0x14)
    }
}

// ------------------------------------------------------------------ match finder

struct MatchFinder {
    /// History + lookahead; `buf[0]` is stream position `base`.
    buf: Vec<u8>,
    base: u64,
    head3: Vec<u32>,
    head4: Vec<u32>,
    /// Previous position (+1) with the same 4-byte hash, per buffer index.
    chain: Vec<u32>,
    bits4: u32,
    inserted: usize,
    dict_size: usize,
    depth: u32,
    nice: usize,
}

const H3_BITS: u32 = 16;

#[inline]
fn rd32(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

impl MatchFinder {
    fn new(o: &LzOptions) -> MatchFinder {
        let lg = 32 - (o.dict_size.max(1 << 16) - 1).leading_zeros();
        let bits4 = (lg.saturating_sub(2)).clamp(14, 20);
        MatchFinder {
            buf: Vec::new(),
            base: 0,
            head3: vec![0; 1 << H3_BITS],
            head4: vec![0; 1 << bits4],
            chain: Vec::new(),
            bits4,
            inserted: 0,
            dict_size: o.dict_size as usize,
            depth: if o.depth == 0 { 48 } else { o.depth },
            nice: o.nice_len as usize,
        }
    }

    #[inline]
    fn h3(&self, i: usize) -> usize {
        let v = rd32(&self.buf, i) & 0x00FF_FFFF;
        (v.wrapping_mul(0x9E37_79B1) >> (32 - H3_BITS)) as usize
    }
    #[inline]
    fn h4(&self, i: usize) -> usize {
        (rd32(&self.buf, i).wrapping_mul(0x9E37_79B1) >> (32 - self.bits4)) as usize
    }

    /// Insert every position before `upto` that has four bytes of context.
    fn insert_upto(&mut self, upto: usize) {
        let lim = upto.min(self.buf.len().saturating_sub(3));
        if self.chain.len() < self.buf.len() {
            self.chain.resize(self.buf.len(), 0);
        }
        while self.inserted < lim {
            let i = self.inserted;
            let a = self.h3(i);
            let b = self.h4(i);
            self.chain[i] = self.head4[b];
            self.head4[b] = i as u32 + 1;
            self.head3[a] = i as u32 + 1;
            self.inserted += 1;
        }
    }

    #[inline]
    fn match_len(&self, a: usize, b: usize, max: usize) -> usize {
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

    /// Longest match at `p` (not yet inserted) with at most `avail` bytes: (len, dist).
    fn find(&self, p: usize, avail: usize) -> (usize, u32) {
        if avail < 4 || p + 4 > self.buf.len() {
            return (0, 0);
        }
        let mut best = 0usize;
        let mut dist = 0u32;
        let c = self.head3[self.h3(p)] as usize;
        if c > 0 {
            let c = c - 1;
            let d = p - c - 1;
            if d < self.dict_size && d < (1 << 16) {
                let l = self.match_len(c, p, avail);
                if l >= 3 {
                    best = l;
                    dist = d as u32;
                }
            }
        }
        if best >= self.nice {
            return (best, dist);
        }
        let mut v = self.head4[self.h4(p)] as usize;
        let mut depth = self.depth;
        while v > 0 && depth > 0 {
            let c = v - 1;
            let d = p - c - 1;
            if d >= self.dict_size {
                break;
            }
            if best < avail && self.buf[c + best] == self.buf[p + best] {
                let l = self.match_len(c, p, avail);
                if l > best {
                    best = l;
                    dist = d as u32;
                    if l >= self.nice || l >= avail {
                        break;
                    }
                }
            }
            v = self.chain[c] as usize;
            depth -= 1;
        }
        if best < 3 { (0, 0) } else { (best, dist) }
    }

    /// Forget history older than the dictionary once enough has accumulated before `p`.
    /// Returns the number of bytes dropped from the front.
    fn slide(&mut self, p: usize) -> usize {
        let slack = (self.dict_size / 2).max(1 << 16);
        if p < self.dict_size + slack {
            return 0;
        }
        let cut = p - self.dict_size;
        let cut32 = cut as u32;
        let fix = |v: &mut u32| *v = if *v > cut32 { *v - cut32 } else { 0 };
        self.head3.iter_mut().for_each(fix);
        self.head4.iter_mut().for_each(fix);
        self.chain.drain(..cut.min(self.chain.len()));
        self.chain.iter_mut().for_each(fix);
        self.buf.drain(..cut);
        self.base += cut as u64;
        self.inserted -= cut;
        cut
    }
}

// ------------------------------------------------------------------ symbol coder

/// LZMA symbol encoder working on the match finder's buffer.
pub struct LzEncoder {
    mf: MatchFinder,
    pub model: Model,
    pub rc: RcEnc,
    pub opts: LzOptions,
    /// Next buffer index to encode.
    pos: usize,
    /// Lazy-evaluation cache: match found at `pos` (if computed).
    cached: Option<(usize, usize, u32)>,
}

pub enum Stop {
    /// Ran out of input (keeping lookahead unless finishing).
    Input,
    /// The caller's limit was reached.
    Limit,
}

impl LzEncoder {
    pub fn new(o: LzOptions) -> LzEncoder {
        LzEncoder { mf: MatchFinder::new(&o), model: Model::new(o.props), rc: RcEnc::new(), opts: o, pos: 0, cached: None }
    }

    pub fn feed(&mut self, d: &[u8]) {
        self.mf.buf.extend_from_slice(d);
    }

    /// Bytes encoded so far (stream position).
    pub fn position(&self) -> u64 {
        self.mf.base + self.pos as u64
    }

    pub fn unencoded(&self) -> usize {
        self.mf.buf.len() - self.pos
    }

    /// Raw bytes of the stream between absolute positions (must still be buffered).
    pub fn raw(&self, from: u64, to: u64) -> &[u8] {
        let a = (from - self.mf.base) as usize;
        let b = (to - self.mf.base) as usize;
        &self.mf.buf[a..b]
    }

    /// Keep history at or after `keep` (stream position) available to `raw`.
    fn can_slide(&self, keep: u64) -> usize {
        // largest buffer index that may become the new start
        (keep.saturating_sub(self.mf.base)) as usize
    }

    #[inline]
    fn lit(&mut self, p: usize) {
        let pos = self.mf.base + p as u64;
        let m = &mut self.model;
        let ps = (pos & m.pos_mask()) as usize;
        let s = m.state;
        self.rc.bit(&mut m.is_match[(s << 4) + ps], 0);
        let prev = if pos > 0 { self.mf.buf[p - 1] } else { 0 };
        let base = m.literal_base(pos, prev);
        let probs = &mut m.literal[base..base + 0x300];
        let byte = self.mf.buf[p] as u32;
        let mut sym = 1usize;
        if is_literal_state(s) {
            for i in (0..8).rev() {
                let b = (byte >> i) & 1;
                self.rc.bit(&mut probs[sym], b);
                sym = (sym << 1) | b as usize;
            }
        } else {
            let mb = self.mf.buf[p - m.reps[0] as usize - 1] as u32;
            let mut matched = true;
            for i in (0..8).rev() {
                let b = (byte >> i) & 1;
                if matched {
                    let mbit = (mb >> i) & 1;
                    self.rc.bit(&mut probs[0x100 + ((mbit as usize) << 8) + sym], b);
                    matched = b == mbit;
                } else {
                    self.rc.bit(&mut probs[sym], b);
                }
                sym = (sym << 1) | b as usize;
            }
        }
        m.state = after_literal(s);
    }

    #[inline]
    fn pos_state(&self, p: usize) -> usize {
        ((self.mf.base + p as u64) & self.model.pos_mask()) as usize
    }

    fn mtch(&mut self, p: usize, len: usize, dist: u32) {
        let ps = self.pos_state(p);
        let m = &mut self.model;
        let s = m.state;
        self.rc.bit(&mut m.is_match[(s << 4) + ps], 1);
        self.rc.bit(&mut m.is_rep[s], 0);
        self.rc.len(&mut m.len, len, ps);
        let slot = dist_slot(dist);
        self.rc.tree(&mut m.dist_slot[len_to_dist_state(len)], 6, slot);
        if slot >= 4 {
            let nb = (slot >> 1) - 1;
            let base = (2 | (slot & 1)) << nb;
            let red = dist - base;
            if slot < END_POS_MODEL {
                self.rc.rev_tree(&mut m.dist_special[(base - slot) as usize..], nb, red);
            } else {
                self.rc.direct(nb - ALIGN_BITS, red >> ALIGN_BITS);
                self.rc.rev_tree(&mut m.align, ALIGN_BITS, red & ((1 << ALIGN_BITS) - 1));
            }
        }
        m.reps = [dist, m.reps[0], m.reps[1], m.reps[2]];
        m.state = after_match(s);
    }

    fn rep(&mut self, p: usize, idx: usize, len: usize) {
        let ps = self.pos_state(p);
        let m = &mut self.model;
        let s = m.state;
        self.rc.bit(&mut m.is_match[(s << 4) + ps], 1);
        self.rc.bit(&mut m.is_rep[s], 1);
        if idx == 0 {
            self.rc.bit(&mut m.is_rep_g0[s], 0);
            self.rc.bit(&mut m.is_rep0_long[(s << 4) + ps], 1);
        } else {
            self.rc.bit(&mut m.is_rep_g0[s], 1);
            if idx == 1 {
                self.rc.bit(&mut m.is_rep_g1[s], 0);
            } else {
                self.rc.bit(&mut m.is_rep_g1[s], 1);
                self.rc.bit(&mut m.is_rep_g2[s], (idx - 2) as u32);
            }
            let d = m.reps[idx];
            for i in (1..=idx).rev() {
                m.reps[i] = m.reps[i - 1];
            }
            m.reps[0] = d;
        }
        self.rc.len(&mut m.rep_len, len, ps);
        m.state = after_rep(s);
    }

    fn short_rep(&mut self, p: usize) {
        let ps = self.pos_state(p);
        let m = &mut self.model;
        let s = m.state;
        self.rc.bit(&mut m.is_match[(s << 4) + ps], 1);
        self.rc.bit(&mut m.is_rep[s], 1);
        self.rc.bit(&mut m.is_rep_g0[s], 0);
        self.rc.bit(&mut m.is_rep0_long[(s << 4) + ps], 0);
        m.state = after_shortrep(s);
    }

    /// Encode the end-of-payload marker (LZMA1 streams of unknown size).
    pub fn end_marker(&mut self) {
        let p = self.pos;
        self.mtch(p, LEN_MIN, 0xFFFF_FFFF);
    }

    /// Best repeat match at `p`: (len, index).
    fn best_rep(&self, p: usize, avail: usize) -> (usize, usize) {
        let mut best = (0, 0);
        for i in 0..4 {
            let d = self.model.reps[i] as usize;
            if d + 1 > p || d >= self.mf.dict_size {
                continue;
            }
            let c = p - d - 1;
            if self.mf.buf[c] != self.mf.buf[p] || self.mf.buf[c + 1] != self.mf.buf[p + 1] {
                continue;
            }
            let l = self.mf.match_len(c, p, avail);
            if l > best.0 {
                best = (l, i);
            }
        }
        best
    }

    fn find_at(&mut self, p: usize, avail: usize) -> (usize, u32) {
        if let Some((cp, l, d)) = self.cached {
            if cp == p {
                return (l.min(avail), d);
            }
        }
        self.mf.insert_upto(p);
        let r = self.mf.find(p, avail);
        self.cached = Some((p, r.0, r.1));
        r
    }

    /// Encode symbols.  Without `finish`, `LEN_MAX + 1` bytes of lookahead are kept back.
    /// `limit(uncompressed_bytes_from_start_mark, rc_pending)` returning true stops before
    /// the next symbol.
    pub fn encode(&mut self, finish: bool, mut limit: impl FnMut(u64, usize) -> bool) -> Stop {
        let keep = if finish { 0 } else { LEN_MAX + 1 };
        loop {
            let end = self.mf.buf.len();
            if self.pos + keep >= end || self.pos >= end {
                return Stop::Input;
            }
            if limit(self.mf.base + self.pos as u64, self.rc.pending()) {
                return Stop::Limit;
            }
            let p = self.pos;
            let avail = (end - p).min(LEN_MAX);
            if avail < 2 {
                self.lit(p);
                self.pos += 1;
                continue;
            }
            let nice = self.mf.nice;
            let (rl, ri) = self.best_rep(p, avail);
            if rl >= nice {
                self.rep(p, ri, rl);
                self.advance(rl);
                continue;
            }
            let (ml, md) = self.find_at(p, avail);
            if ml >= nice {
                self.mtch(p, ml, md);
                self.advance(ml);
                continue;
            }
            if rl >= 2 && (rl + 1 >= ml || (rl + 2 >= ml && md >= (1 << 9)) || (rl + 3 >= ml && md >= (1 << 15))) {
                self.rep(p, ri, rl);
                self.advance(rl);
                continue;
            }
            if ml < 3 {
                let r0 = self.model.reps[0] as usize;
                if r0 < p && self.mf.buf[p - r0 - 1] == self.mf.buf[p] && !is_literal_state(self.model.state) {
                    self.short_rep(p);
                } else {
                    self.lit(p);
                }
                self.pos += 1;
                continue;
            }
            if self.opts.mode >= 2 && p + 1 < end && ml < nice {
                // Lazy step: a clearly better match one byte later wins over this one.
                let avail1 = (end - p - 1).min(LEN_MAX);
                let (nl, nd) = self.find_at(p + 1, avail1);
                let better = nl > ml + 1 || (nl == ml + 1 && (nd >> 7) <= md) || (nl == ml && nd < md / 128);
                let (nrl, _) = if avail1 >= 2 { self.best_rep(p + 1, avail1) } else { (0, 0) };
                if better || nrl > ml {
                    self.lit(p);
                    self.pos += 1;
                    continue;
                }
                self.cached = Some((p, ml, md));
            }
            self.mtch(p, ml, md);
            self.advance(ml);
        }
    }

    fn advance(&mut self, n: usize) {
        self.pos += n;
    }

    /// Drop history no longer needed; `keep` is the oldest stream position the caller still
    /// needs raw access to.
    pub fn trim(&mut self, keep: u64) {
        let lim = self.can_slide(keep).min(self.pos);
        let cut = self.mf.slide(lim);
        if cut > 0 {
            self.pos -= cut;
            self.cached = None;
        }
    }
}

// ------------------------------------------------------------------ LZMA1 stream

/// Raw LZMA1 encoder (end marker written at `finish`).
pub struct Lzma1Encoder {
    pub lz: LzEncoder,
    finished: bool,
}

impl Lzma1Encoder {
    pub fn new(o: LzOptions) -> Lzma1Encoder {
        Lzma1Encoder { lz: LzEncoder::new(o), finished: false }
    }

    /// Compress `d`; returns produced bytes.
    pub fn compress(&mut self, d: &[u8], out: &mut Vec<u8>) {
        self.lz.feed(d);
        self.lz.encode(false, |_, _| false);
        let keep = self.lz.position();
        self.lz.trim(keep);
        out.append(&mut self.lz.rc.out);
    }

    pub fn finish(&mut self, out: &mut Vec<u8>, marker: bool) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.lz.encode(true, |_, _| false);
        if marker {
            self.lz.end_marker();
        }
        self.lz.rc.flush();
        out.append(&mut self.lz.rc.out);
    }
}

// ------------------------------------------------------------------ LZMA2 stream

const CHUNK_UNCOMPRESSED_MAX: u64 = 1 << 21;
const CHUNK_COMPRESSED_MAX: usize = 1 << 16;

pub fn lzma2_dict_byte(dict_size: u32) -> u8 {
    for b in 0..40u8 {
        let s = (2 | (b as u32 & 1)) << (b / 2 + 11);
        if s >= dict_size {
            return b;
        }
    }
    40
}

pub struct Lzma2Encoder {
    pub lz: LzEncoder,
    chunk_open: bool,
    chunk_start: u64,
    need_dict_reset: bool,
    need_props: bool,
    need_state_reset: bool,
    finished: bool,
}

impl Lzma2Encoder {
    pub fn new(o: LzOptions) -> Lzma2Encoder {
        Lzma2Encoder {
            lz: LzEncoder::new(o),
            chunk_open: false,
            chunk_start: 0,
            need_dict_reset: true,
            need_props: true,
            need_state_reset: false,
            finished: false,
        }
    }

    pub fn compress(&mut self, d: &[u8], out: &mut Vec<u8>) {
        self.lz.feed(d);
        self.run(false, out);
    }

    pub fn finish(&mut self, out: &mut Vec<u8>) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.run(true, out);
        out.push(0);
    }

    fn run(&mut self, finish: bool, out: &mut Vec<u8>) {
        loop {
            if !self.chunk_open {
                if self.lz.unencoded() == 0 || (!finish && self.lz.unencoded() <= LEN_MAX + 1) {
                    break;
                }
                self.chunk_open = true;
                self.chunk_start = self.lz.position();
                self.lz.rc.reset();
                self.lz.rc.out.clear();
                if self.need_state_reset {
                    let p = self.lz.opts.props;
                    self.lz.model.reset(p);
                }
            }
            let start = self.chunk_start;
            let stop = self.lz.encode(finish, |pos, pending| {
                pos - start + LEN_MAX as u64 > CHUNK_UNCOMPRESSED_MAX || pending + 64 > CHUNK_COMPRESSED_MAX - 8
            });
            match stop {
                Stop::Limit => self.close_chunk(out),
                Stop::Input => {
                    if finish {
                        self.close_chunk(out);
                        break;
                    }
                    break;
                }
            }
        }
        let keep = if self.chunk_open { self.chunk_start } else { self.lz.position() };
        self.lz.trim(keep);
    }

    fn close_chunk(&mut self, out: &mut Vec<u8>) {
        if !self.chunk_open {
            return;
        }
        self.chunk_open = false;
        let end = self.lz.position();
        let unc = (end - self.chunk_start) as usize;
        if unc == 0 {
            return;
        }
        self.lz.rc.flush();
        let comp = std::mem::take(&mut self.lz.rc.out);
        if comp.len() >= unc || comp.len() > CHUNK_COMPRESSED_MAX {
            // Store the data instead; the coder state used for it is discarded.
            let data = self.lz.raw(self.chunk_start, end);
            for (i, piece) in data.chunks(1 << 16).enumerate() {
                out.push(if self.need_dict_reset && i == 0 { 1 } else { 2 });
                let n = piece.len() - 1;
                out.push((n >> 8) as u8);
                out.push(n as u8);
                out.extend_from_slice(piece);
            }
            self.need_dict_reset = false;
            self.need_state_reset = true;
            return;
        }
        let reset = if self.need_dict_reset {
            3
        } else if self.need_props {
            2
        } else if self.need_state_reset {
            1
        } else {
            0
        };
        let u = unc - 1;
        let c = comp.len() - 1;
        out.push(0x80 | (reset << 5) | ((u >> 16) as u8));
        out.push((u >> 8) as u8);
        out.push(u as u8);
        out.push((c >> 8) as u8);
        out.push(c as u8);
        if reset >= 2 {
            out.push(self.lz.opts.props.to_byte());
        }
        out.extend_from_slice(&comp);
        self.need_dict_reset = false;
        self.need_props = false;
        self.need_state_reset = false;
    }
}
