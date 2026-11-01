//! DEFLATE encoder (RFC 1951) with zlib/gzip wrappers.
//!
//! Hash-chain match finder with greedy (levels 1-3) or lazy (4-9) evaluation, dynamic /
//! fixed / stored block selection by exact bit cost.  The output depends only on the input
//! byte stream and the flush calls, never on how the input was split into `compress` calls.

use crate::checksum::{adler32, crc32};
use crate::huffman::{canonical_codes, code_lengths, reverse_bits};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wrap {
    Raw,
    Zlib,
    Gzip,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flush {
    None = 0,
    Partial = 1,
    Sync = 2,
    Full = 3,
    Finish = 4,
    Block = 5,
}

impl Flush {
    pub fn from_int(v: i64) -> Option<Flush> {
        Some(match v {
            0 => Flush::None,
            1 => Flush::Partial,
            2 => Flush::Sync,
            3 => Flush::Full,
            4 => Flush::Finish,
            5 => Flush::Block,
            _ => return None,
        })
    }
    fn rank(v: i32) -> i32 {
        v * 2 - if v > 4 { 9 } else { 0 }
    }
}

pub const STRATEGY_DEFAULT: u32 = 0;
pub const STRATEGY_FILTERED: u32 = 1;
pub const STRATEGY_HUFFMAN_ONLY: u32 = 2;
pub const STRATEGY_RLE: u32 = 3;
pub const STRATEGY_FIXED: u32 = 4;

pub(crate) static LEN_BASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
static LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
pub(crate) static DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
static DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
static CL_ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

struct Tables {
    len_code: [u8; 256],   // match length - 3 -> code index 0..28
    dist_code: [u8; 512],  // see dist_code()
    fixed_lit: [(u16, u8); 288],
    fixed_dist: [(u16, u8); 30],
}

static TABLES: std::sync::OnceLock<Tables> = std::sync::OnceLock::new();

fn tables() -> &'static Tables {
    TABLES.get_or_init(|| {
        let mut len_code = [0u8; 256];
        for c in 0..29 {
            let base = LEN_BASE[c] as usize - 3;
            let n = if c == 28 { 1 } else { 1usize << LEN_EXTRA[c] };
            for i in 0..n {
                if base + i < 256 {
                    len_code[base + i] = c as u8;
                }
            }
        }
        len_code[255] = 28;
        let mut dist_code = [0u8; 512];
        for c in 0..30 {
            let base = DIST_BASE[c] as usize - 1;
            let n = 1usize << DIST_EXTRA[c];
            for d in base..base + n {
                if d < 256 {
                    dist_code[d] = c as u8;
                } else {
                    dist_code[256 + (d >> 7)] = c as u8;
                }
            }
        }
        let mut fl = [0u8; 288];
        for (i, x) in fl.iter_mut().enumerate() {
            *x = if i < 144 {
                8
            } else if i < 256 {
                9
            } else if i < 280 {
                7
            } else {
                8
            };
        }
        let codes = canonical_codes(&fl);
        let mut fixed_lit = [(0u16, 0u8); 288];
        for i in 0..288 {
            fixed_lit[i] = (reverse_bits(codes[i], fl[i] as u32) as u16, fl[i]);
        }
        let mut fixed_dist = [(0u16, 0u8); 30];
        for i in 0..30 {
            fixed_dist[i] = (reverse_bits(i as u32, 5) as u16, 5);
        }
        Tables { len_code, dist_code, fixed_lit, fixed_dist }
    })
}

#[inline]
fn dist_code(t: &Tables, d: usize) -> usize {
    let d = d - 1;
    if d < 256 { t.dist_code[d] as usize } else { t.dist_code[256 + (d >> 7)] as usize }
}

const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const MIN_LOOKAHEAD: usize = MAX_MATCH + MIN_MATCH + 1;
const TOO_FAR: usize = 4096;

#[derive(Clone, Copy)]
struct Config {
    good: usize,
    lazy: usize,
    nice: usize,
    chain: usize,
    fast: bool,
}

const CONFIGS: [Config; 10] = [
    Config { good: 0, lazy: 0, nice: 0, chain: 0, fast: true },
    Config { good: 4, lazy: 4, nice: 8, chain: 4, fast: true },
    Config { good: 4, lazy: 5, nice: 16, chain: 8, fast: true },
    Config { good: 4, lazy: 6, nice: 32, chain: 32, fast: true },
    Config { good: 4, lazy: 4, nice: 16, chain: 16, fast: false },
    Config { good: 8, lazy: 16, nice: 32, chain: 32, fast: false },
    Config { good: 8, lazy: 16, nice: 128, chain: 128, fast: false },
    Config { good: 8, lazy: 32, nice: 128, chain: 256, fast: false },
    Config { good: 32, lazy: 128, nice: 258, chain: 1024, fast: false },
    Config { good: 32, lazy: 258, nice: 258, chain: 4096, fast: false },
];

#[derive(Clone)]
pub struct Deflater {
    level: u32,
    strategy: u32,
    wrap: Wrap,
    wbits: u32,
    cfg: Config,
    w_size: usize,
    w_mask: usize,
    window: Vec<u8>,
    strstart: usize,
    lookahead: usize,
    block_start: isize,
    head: Vec<u16>,
    prev: Vec<u16>,
    hash_shift: u32,
    match_available: bool,
    match_length: usize,
    match_start: usize,
    prev_length: usize,
    prev_match: usize,
    syms: Vec<u32>,
    lit_bufsize: usize,
    lfreq: [u32; 288],
    dfreq: [u32; 30],
    stored_buf: Vec<u8>,
    out: Vec<u8>,
    bitbuf: u64,
    bitcnt: u32,
    check: u32,
    total_in: u64,
    started: bool,
    finished: bool,
    last_flush: i32,
    dict_id: Option<u32>,
}

impl Deflater {
    /// `level` 0..=9 (callers map -1 to 6), `wbits` 9..=15, `mem_level` 1..=9.
    pub fn new(level: u32, wrap: Wrap, wbits: u32, mem_level: u32, strategy: u32) -> Deflater {
        let wbits = wbits.clamp(9, 15);
        let w_size = 1usize << wbits;
        let hash_bits = (mem_level + 7).clamp(8, 16);
        let lit_bufsize = 1usize << (mem_level + 6);
        Deflater {
            level,
            strategy,
            wrap,
            wbits,
            cfg: CONFIGS[level.min(9) as usize],
            w_size,
            w_mask: w_size - 1,
            window: vec![0u8; 2 * w_size + 2 * MAX_MATCH + 16],
            strstart: 0,
            lookahead: 0,
            block_start: 0,
            head: vec![0u16; 1 << hash_bits],
            prev: vec![0u16; w_size],
            hash_shift: 32 - hash_bits,
            match_available: false,
            match_length: MIN_MATCH - 1,
            match_start: 0,
            prev_length: MIN_MATCH - 1,
            prev_match: 0,
            syms: Vec::with_capacity(lit_bufsize),
            lit_bufsize,
            lfreq: [0; 288],
            dfreq: [0; 30],
            stored_buf: Vec::new(),
            out: Vec::new(),
            bitbuf: 0,
            bitcnt: 0,
            check: if wrap == Wrap::Gzip { 0 } else { 1 },
            total_in: 0,
            started: false,
            finished: false,
            last_flush: -2,
            dict_id: None,
        }
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Preset dictionary (call before any input).
    pub fn set_dictionary(&mut self, d: &[u8]) {
        if self.wrap == Wrap::Zlib {
            self.dict_id = Some(adler32(1, d));
        }
        if self.level == 0 {
            return;
        }
        let d = if d.len() > self.w_size { &d[d.len() - self.w_size..] } else { d };
        // Place the dictionary at offset 1 so that no dictionary position equals NIL (0).
        let start = 1;
        self.window[start..start + d.len()].copy_from_slice(d);
        self.strstart = start + d.len();
        self.block_start = self.strstart as isize;
        if d.len() >= MIN_MATCH {
            for p in start..start + d.len() - (MIN_MATCH - 1) {
                self.insert(p);
            }
        }
    }

    /// Output produced so far and not yet taken.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    pub fn output_mut(&mut self) -> &mut Vec<u8> {
        &mut self.out
    }

    fn write_header(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        match self.wrap {
            Wrap::Raw => {}
            Wrap::Zlib => {
                let cinfo = self.wbits - 8;
                let mut h: u32 = ((cinfo << 4) | 8) << 8;
                let lflags = if self.strategy >= STRATEGY_HUFFMAN_ONLY || self.level < 2 {
                    0
                } else if self.level < 6 {
                    1
                } else if self.level == 6 {
                    2
                } else {
                    3
                };
                h |= lflags << 6;
                if self.dict_id.is_some() {
                    h |= 0x20;
                }
                h += 31 - (h % 31);
                self.out.push((h >> 8) as u8);
                self.out.push(h as u8);
                if let Some(id) = self.dict_id {
                    self.out.extend_from_slice(&id.to_be_bytes());
                }
            }
            Wrap::Gzip => {
                let xfl = if self.level == 9 {
                    2
                } else if self.strategy >= STRATEGY_HUFFMAN_ONLY || self.level < 2 {
                    4
                } else {
                    0
                };
                self.out.extend_from_slice(&[0x1f, 0x8b, 8, 0, 0, 0, 0, 0, xfl, 3]);
            }
        }
    }

    /// Feed input and apply a flush mode.  Returns false for a stream error (use after
    /// finish, or a repeated flush with nothing to do — the caller treats the latter as
    /// "no output").
    pub fn compress(&mut self, input: &[u8], flush: Flush) -> bool {
        if self.finished {
            return false;
        }
        let old_flush = self.last_flush;
        self.last_flush = flush as i32;
        if input.is_empty() && Flush::rank(flush as i32) <= Flush::rank(old_flush) && flush != Flush::Finish {
            // zlib returns Z_BUF_ERROR here; nothing is produced.
            self.write_header();
            return true;
        }
        self.write_header();
        match self.wrap {
            Wrap::Zlib => self.check = adler32(self.check, input),
            Wrap::Gzip => self.check = crc32(self.check, input),
            Wrap::Raw => {}
        }
        self.total_in += input.len() as u64;
        if self.level == 0 {
            self.stored_buf.extend_from_slice(input);
            while self.stored_buf.len() >= 65535 {
                let chunk: Vec<u8> = self.stored_buf.drain(..65535).collect();
                self.stored_block(&chunk, false);
            }
        } else {
            self.feed(input);
        }
        if flush != Flush::None {
            self.do_flush(flush);
        }
        self.flush_bytes();
        true
    }

    fn do_flush(&mut self, flush: Flush) {
        if self.level == 0 {
            let data = std::mem::take(&mut self.stored_buf);
            if flush == Flush::Finish {
                self.stored_block(&data, true);
            } else if !data.is_empty() {
                self.stored_block(&data, false);
            }
        } else {
            self.process(true);
            if self.match_available {
                let c = self.window[self.strstart - 1];
                self.tally_lit(c);
                self.match_available = false;
            }
            if flush == Flush::Finish {
                self.flush_block(true);
            } else if !self.syms.is_empty() {
                self.flush_block(false);
            }
        }
        match flush {
            Flush::Finish => {
                self.align_bits();
                match self.wrap {
                    Wrap::Raw => {}
                    Wrap::Zlib => self.out.extend_from_slice(&self.check.to_be_bytes()),
                    Wrap::Gzip => {
                        let c = self.check;
                        self.out.extend_from_slice(&c.to_le_bytes());
                        let t = self.total_in as u32;
                        self.out.extend_from_slice(&t.to_le_bytes());
                    }
                }
                self.finished = true;
            }
            Flush::Partial => {
                // Empty fixed block (EOB only) so the decoder can see everything so far.
                self.put_bits(2, 3);
                self.put_bits(0, 7);
            }
            Flush::Sync | Flush::Full => {
                self.put_bits(0, 3);
                self.align_bits();
                self.out.extend_from_slice(&[0, 0, 0xff, 0xff]);
                if flush == Flush::Full && self.level > 0 {
                    self.head.iter_mut().for_each(|h| *h = 0);
                    self.prev.iter_mut().for_each(|h| *h = 0);
                }
            }
            Flush::Block | Flush::None => {}
        }
    }

    // ------------------------------------------------------------ bit output

    #[inline]
    fn put_bits(&mut self, v: u32, n: u32) {
        self.bitbuf |= (v as u64) << self.bitcnt;
        self.bitcnt += n;
        if self.bitcnt >= 32 {
            self.out.extend_from_slice(&(self.bitbuf as u32).to_le_bytes());
            self.bitbuf >>= 32;
            self.bitcnt -= 32;
        }
    }

    fn flush_bytes(&mut self) {
        while self.bitcnt >= 8 {
            self.out.push(self.bitbuf as u8);
            self.bitbuf >>= 8;
            self.bitcnt -= 8;
        }
    }

    fn align_bits(&mut self) {
        self.flush_bytes();
        if self.bitcnt > 0 {
            self.out.push(self.bitbuf as u8);
            self.bitbuf = 0;
            self.bitcnt = 0;
        }
    }

    fn stored_block(&mut self, data: &[u8], last: bool) {
        let mut chunks: Vec<&[u8]> = data.chunks(65535).collect();
        if chunks.is_empty() {
            chunks.push(&[]);
        }
        let n = chunks.len();
        for (i, ch) in chunks.into_iter().enumerate() {
            let fin = last && i + 1 == n;
            self.put_bits(fin as u32, 3);
            self.align_bits();
            let l = ch.len() as u16;
            self.out.extend_from_slice(&l.to_le_bytes());
            self.out.extend_from_slice(&(!l).to_le_bytes());
            self.out.extend_from_slice(ch);
        }
    }

    // ------------------------------------------------------------ window / matching

    #[inline]
    fn hash(&self, p: usize) -> usize {
        let w = &self.window;
        let v = (w[p] as u32) | ((w[p + 1] as u32) << 8) | ((w[p + 2] as u32) << 16);
        (v.wrapping_mul(0x9E37_79B1) >> self.hash_shift) as usize
    }

    #[inline]
    fn insert(&mut self, p: usize) -> usize {
        let h = self.hash(p);
        let old = self.head[h] as usize;
        self.prev[p & self.w_mask] = old as u16;
        self.head[h] = p as u16;
        old
    }

    fn max_dist(&self) -> usize {
        self.w_size - MIN_LOOKAHEAD
    }

    fn slide(&mut self) {
        let w = self.w_size;
        self.window.copy_within(w..2 * w, 0);
        self.strstart -= w;
        self.match_start = self.match_start.saturating_sub(w);
        self.block_start -= w as isize;
        for h in self.head.iter_mut() {
            *h = if *h as usize >= w { (*h as usize - w) as u16 } else { 0 };
        }
        for h in self.prev.iter_mut() {
            *h = if *h as usize >= w { (*h as usize - w) as u16 } else { 0 };
        }
    }

    fn feed(&mut self, input: &[u8]) {
        let mut ip = 0;
        let wsz = 2 * self.w_size;
        loop {
            let mut space = wsz - (self.strstart + self.lookahead);
            if space == 0 && ip < input.len() {
                self.slide();
                space = wsz - (self.strstart + self.lookahead);
            }
            let n = space.min(input.len() - ip);
            let at = self.strstart + self.lookahead;
            self.window[at..at + n].copy_from_slice(&input[ip..ip + n]);
            self.lookahead += n;
            ip += n;
            self.process(false);
            if ip == input.len() {
                break;
            }
        }
    }

    #[inline]
    fn match_len(&self, a: usize, b: usize, max: usize) -> usize {
        let w = &self.window;
        let mut n = 0;
        while n + 8 <= max {
            let x = u64::from_le_bytes(w[a + n..a + n + 8].try_into().unwrap());
            let y = u64::from_le_bytes(w[b + n..b + n + 8].try_into().unwrap());
            let d = x ^ y;
            if d != 0 {
                return n + (d.trailing_zeros() / 8) as usize;
            }
            n += 8;
        }
        while n < max && w[a + n] == w[b + n] {
            n += 1;
        }
        n
    }

    fn longest_match(&mut self, mut cur: usize) -> usize {
        let mut chain = self.cfg.chain;
        let s = self.strstart;
        let mut best = self.prev_length;
        let nice = self.cfg.nice.min(self.lookahead);
        let max = MAX_MATCH.min(self.lookahead);
        let limit = s.saturating_sub(self.max_dist());
        if self.prev_length >= self.cfg.good {
            chain >>= 2;
        }
        let w = &self.window;
        loop {
            // Quick reject on the byte that would extend the best match.
            if best < max && w[cur + best] == w[s + best] && w[cur] == w[s] && w[cur + 1] == w[s + 1] {
                let l = self.match_len(cur, s, max);
                if l > best {
                    self.match_start = cur;
                    best = l;
                    if l >= nice {
                        break;
                    }
                }
            }
            let nxt = self.prev[cur & self.w_mask] as usize;
            if nxt <= limit || nxt >= cur {
                break;
            }
            cur = nxt;
            chain -= 1;
            if chain == 0 {
                break;
            }
        }
        best.min(self.lookahead)
    }

    #[inline]
    fn tally_lit(&mut self, c: u8) {
        self.syms.push(c as u32);
        self.lfreq[c as usize] += 1;
    }

    #[inline]
    fn tally_match(&mut self, dist: usize, len: usize) {
        let t = tables();
        self.syms.push(((dist as u32) << 16) | (len - MIN_MATCH) as u32 | 0x8000);
        self.lfreq[257 + t.len_code[len - MIN_MATCH] as usize] += 1;
        self.dfreq[dist_code(t, dist)] += 1;
    }

    #[inline]
    fn full(&self) -> bool {
        self.syms.len() >= self.lit_bufsize - 1
    }

    fn process(&mut self, flushing: bool) {
        match self.strategy {
            STRATEGY_HUFFMAN_ONLY => self.process_huff(flushing),
            STRATEGY_RLE => self.process_rle(flushing),
            _ if self.cfg.fast => self.process_fast(flushing),
            _ => self.process_slow(flushing),
        }
    }

    fn enough(&self, flushing: bool) -> bool {
        if flushing { self.lookahead > 0 } else { self.lookahead >= MIN_LOOKAHEAD }
    }

    fn process_huff(&mut self, flushing: bool) {
        while self.enough(flushing) {
            let c = self.window[self.strstart];
            self.tally_lit(c);
            self.strstart += 1;
            self.lookahead -= 1;
            if self.full() {
                self.flush_block(false);
            }
        }
    }

    fn process_rle(&mut self, flushing: bool) {
        while self.enough(flushing) {
            let s = self.strstart;
            let mut len = 0;
            if self.lookahead >= MIN_MATCH && s > 0 {
                let c = self.window[s - 1];
                let max = MAX_MATCH.min(self.lookahead);
                while len < max && self.window[s + len] == c {
                    len += 1;
                }
            }
            if len >= MIN_MATCH {
                self.tally_match(1, len);
                self.strstart += len;
                self.lookahead -= len;
            } else {
                let c = self.window[s];
                self.tally_lit(c);
                self.strstart += 1;
                self.lookahead -= 1;
            }
            if self.full() {
                self.flush_block(false);
            }
        }
    }

    fn process_fast(&mut self, flushing: bool) {
        while self.enough(flushing) {
            let mut head = 0;
            if self.lookahead >= MIN_MATCH {
                head = self.insert(self.strstart);
            }
            self.match_length = 0;
            if head != 0 && self.strstart - head <= self.max_dist() {
                self.prev_length = MIN_MATCH - 1;
                self.match_length = self.longest_match(head);
            }
            if self.match_length >= MIN_MATCH {
                let len = self.match_length;
                self.tally_match(self.strstart - self.match_start, len);
                self.lookahead -= len;
                if len <= self.cfg.lazy && self.lookahead >= MIN_MATCH {
                    for _ in 1..len {
                        self.strstart += 1;
                        self.insert(self.strstart);
                    }
                    self.strstart += 1;
                } else {
                    self.strstart += len;
                }
            } else {
                let c = self.window[self.strstart];
                self.tally_lit(c);
                self.lookahead -= 1;
                self.strstart += 1;
            }
            if self.full() {
                self.flush_block(false);
            }
        }
    }

    fn process_slow(&mut self, flushing: bool) {
        while self.enough(flushing) {
            let mut head = 0;
            if self.lookahead >= MIN_MATCH {
                head = self.insert(self.strstart);
            }
            self.prev_length = self.match_length;
            self.prev_match = self.match_start;
            self.match_length = MIN_MATCH - 1;
            if head != 0 && self.prev_length < self.cfg.lazy && self.strstart - head <= self.max_dist() {
                self.match_length = self.longest_match(head);
                if self.match_length <= 5
                    && (self.strategy == STRATEGY_FILTERED
                        || (self.match_length == MIN_MATCH && self.strstart - self.match_start > TOO_FAR))
                {
                    self.match_length = MIN_MATCH - 1;
                }
            }
            if self.prev_length >= MIN_MATCH && self.match_length <= self.prev_length {
                let max_insert = self.strstart + self.lookahead - MIN_MATCH;
                let plen = self.prev_length;
                self.tally_match(self.strstart - 1 - self.prev_match, plen);
                self.lookahead -= plen - 1;
                for _ in 0..plen - 2 {
                    self.strstart += 1;
                    if self.strstart <= max_insert {
                        self.insert(self.strstart);
                    }
                }
                self.match_available = false;
                self.match_length = MIN_MATCH - 1;
                self.strstart += 1;
                if self.full() {
                    self.flush_block(false);
                }
            } else if self.match_available {
                let c = self.window[self.strstart - 1];
                self.tally_lit(c);
                if self.full() {
                    self.flush_block(false);
                }
                self.strstart += 1;
                self.lookahead -= 1;
            } else {
                self.match_available = true;
                self.strstart += 1;
                self.lookahead -= 1;
            }
        }
    }

    // ------------------------------------------------------------ block output

    fn flush_block(&mut self, last: bool) {
        let t = tables();
        // Symbols for the block are in self.syms; the raw bytes (if still in the window) are
        // window[block_start .. strstart - pending lazy literal].
        // Every byte before strstart has been tallied (a pending lazy literal sits at
        // strstart itself when this is called from the lazy-literal branch).
        let end = self.strstart;
        let stored_len = (end as isize - self.block_start).max(0) as usize;
        self.lfreq[256] = 1;
        // Extra bits common to dynamic and fixed coding.
        let mut extra_bits: u64 = 0;
        for c in 0..29 {
            extra_bits += self.lfreq[257 + c] as u64 * LEN_EXTRA[c] as u64;
        }
        for c in 0..30 {
            extra_bits += self.dfreq[c] as u64 * DIST_EXTRA[c] as u64;
        }
        // Fixed cost.
        let mut fixed_bits: u64 = 3 + extra_bits;
        for s in 0..286 {
            fixed_bits += self.lfreq[s] as u64 * t.fixed_lit[s].1 as u64;
        }
        for s in 0..30 {
            fixed_bits += self.dfreq[s] as u64 * 5;
        }
        // Dynamic trees.
        let llens = code_lengths(&self.lfreq[..286], 15, 2);
        let dlens = code_lengths(&self.dfreq, 15, 2);
        let mut hlit = 286;
        while hlit > 257 && llens[hlit - 1] == 0 {
            hlit -= 1;
        }
        let mut hdist = 30;
        while hdist > 1 && dlens[hdist - 1] == 0 {
            hdist -= 1;
        }
        let mut seq: Vec<u8> = Vec::with_capacity(hlit + hdist);
        seq.extend_from_slice(&llens[..hlit]);
        seq.extend_from_slice(&dlens[..hdist]);
        let rle = rle_lengths(&seq);
        let mut blfreq = [0u32; 19];
        for &(s, _) in &rle {
            blfreq[s as usize] += 1;
        }
        let bllens = code_lengths(&blfreq, 7, 2);
        let mut hclen = 19;
        while hclen > 4 && bllens[CL_ORDER[hclen - 1]] == 0 {
            hclen -= 1;
        }
        let mut dyn_bits: u64 = 3 + 14 + 3 * hclen as u64 + extra_bits;
        for s in 0..19 {
            dyn_bits += blfreq[s] as u64 * bllens[s] as u64;
        }
        dyn_bits += blfreq[16] as u64 * 2 + blfreq[17] as u64 * 3 + blfreq[18] as u64 * 7;
        for s in 0..286 {
            dyn_bits += self.lfreq[s] as u64 * llens[s] as u64;
        }
        for s in 0..30 {
            dyn_bits += self.dfreq[s] as u64 * dlens[s] as u64;
        }
        let mut opt_lenb = (dyn_bits + 7) >> 3;
        let static_lenb = (fixed_bits + 7) >> 3;
        if static_lenb <= opt_lenb || self.strategy == STRATEGY_FIXED {
            opt_lenb = static_lenb;
        }
        let have_raw = self.block_start >= 0;
        if have_raw && (stored_len as u64) + 4 <= opt_lenb {
            let bs = self.block_start as usize;
            let data = self.window[bs..bs + stored_len].to_vec();
            self.stored_block(&data, last);
        } else if static_lenb == opt_lenb {
            self.put_bits(2 | last as u32, 3);
            let fl = t.fixed_lit;
            let fd = t.fixed_dist;
            self.emit_syms(&fl, &fd);
        } else {
            self.put_bits(4 | last as u32, 3);
            self.put_bits((hlit - 257) as u32, 5);
            self.put_bits((hdist - 1) as u32, 5);
            self.put_bits((hclen - 4) as u32, 4);
            for &i in &CL_ORDER[..hclen] {
                self.put_bits(bllens[i] as u32, 3);
            }
            let blcodes = canonical_codes(&bllens);
            for &(s, x) in &rle {
                let s = s as usize;
                self.put_bits(reverse_bits(blcodes[s], bllens[s] as u32), bllens[s] as u32);
                match s {
                    16 => self.put_bits(x as u32, 2),
                    17 => self.put_bits(x as u32, 3),
                    18 => self.put_bits(x as u32, 7),
                    _ => {}
                }
            }
            let lc = canonical_codes(&llens);
            let dc = canonical_codes(&dlens);
            let mut lt = [(0u16, 0u8); 288];
            for s in 0..286 {
                lt[s] = (reverse_bits(lc[s], llens[s] as u32) as u16, llens[s]);
            }
            let mut dt = [(0u16, 0u8); 30];
            for s in 0..30 {
                dt[s] = (reverse_bits(dc[s], dlens[s] as u32) as u16, dlens[s]);
            }
            self.emit_syms(&lt, &dt);
        }
        self.syms.clear();
        self.lfreq = [0; 288];
        self.dfreq = [0; 30];
        self.block_start = end as isize;
    }

    fn emit_syms(&mut self, lt: &[(u16, u8); 288], dt: &[(u16, u8); 30]) {
        let t = tables();
        let syms = std::mem::take(&mut self.syms);
        let mut bb = self.bitbuf;
        let mut cnt = self.bitcnt;
        let out = &mut self.out;
        out.reserve(syms.len() * 2 + 16);
        macro_rules! put {
            ($v:expr, $n:expr) => {{
                bb |= ($v as u64) << cnt;
                cnt += $n as u32;
                if cnt >= 32 {
                    out.extend_from_slice(&(bb as u32).to_le_bytes());
                    bb >>= 32;
                    cnt -= 32;
                }
            }};
        }
        for &s in &syms {
            if s & 0x8000 == 0 {
                let (c, l) = lt[s as usize];
                put!(c, l);
            } else {
                let lm = (s & 0xff) as usize;
                let dist = (s >> 16) as usize;
                let lc = t.len_code[lm] as usize;
                let (c, l) = lt[257 + lc];
                put!(c, l);
                let x = LEN_EXTRA[lc];
                if x > 0 {
                    put!(lm + 3 - LEN_BASE[lc] as usize, x);
                }
                let dcode = dist_code(t, dist);
                let (c, l) = dt[dcode];
                put!(c, l);
                let x = DIST_EXTRA[dcode];
                if x > 0 {
                    put!(dist - DIST_BASE[dcode] as usize, x);
                }
            }
        }
        let (c, l) = lt[256];
        put!(c, l);
        self.bitbuf = bb;
        self.bitcnt = cnt;
        let mut syms = syms;
        syms.clear();
        self.syms = syms;
    }
}

/// Run-length encode a code-length sequence into (symbol, extra) pairs (RFC 1951 3.2.7).
fn rle_lengths(seq: &[u8]) -> Vec<(u8, u8)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < seq.len() {
        let v = seq[i];
        let mut run = 1;
        while i + run < seq.len() && seq[i + run] == v {
            run += 1;
        }
        let mut r = run;
        if v == 0 {
            while r >= 11 {
                let n = r.min(138);
                out.push((18, (n - 11) as u8));
                r -= n;
            }
            if r >= 3 {
                out.push((17, (r - 3) as u8));
                r = 0;
            }
            for _ in 0..r {
                out.push((0, 0));
            }
        } else {
            out.push((v, 0));
            r -= 1;
            while r >= 3 {
                let n = r.min(6);
                out.push((16, (n - 3) as u8));
                r -= n;
            }
            for _ in 0..r {
                out.push((v, 0));
            }
        }
        i += run;
    }
    out
}

/// One-shot helper.
pub fn compress(data: &[u8], level: u32, wrap: Wrap, wbits: u32) -> Vec<u8> {
    let mut d = Deflater::new(level, wrap, wbits, 8, STRATEGY_DEFAULT);
    d.compress(data, Flush::Finish);
    d.take_output()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inflate::{self, Format, Inflater, Status};

    fn sample(n: usize, seed: u32) -> Vec<u8> {
        let words: [&[u8]; 8] = [b"the ", b"quick ", b"brown ", b"fox ", b"jumps ", b"over ", b"lazy ", b"dog\n"];
        let mut x = seed;
        let mut v = Vec::new();
        while v.len() < n {
            x = x.wrapping_mul(1103515245).wrapping_add(12345);
            if (x >> 16) % 13 == 0 {
                v.push((x >> 8) as u8);
            } else {
                v.extend_from_slice(words[((x >> 16) % 8) as usize]);
            }
        }
        v.truncate(n);
        v
    }

    #[test]
    fn roundtrip_levels() {
        for &n in &[0usize, 1, 2, 3, 100, 5000, 70000, 300000] {
            let data = sample(n, n as u32 + 1);
            for level in 0..=9 {
                for (wrap, fmt) in [(Wrap::Zlib, Format::Zlib), (Wrap::Raw, Format::Raw), (Wrap::Gzip, Format::Gzip)] {
                    let c = compress(&data, level, wrap, 15);
                    let (d, used) = inflate::decompress(&c, fmt, 15).unwrap_or_else(|e| panic!("{e:?} level {level} n {n} {wrap:?}"));
                    assert_eq!(used, c.len());
                    assert!(d == data, "level {level} n {n} {:?}", wrap);
                }
            }
        }
    }

    #[test]
    fn strategies_and_windows() {
        let data = sample(100000, 7);
        for strat in 0..5 {
            for wbits in [9u32, 12, 15] {
                let mut d = Deflater::new(6, Wrap::Zlib, wbits, 8, strat);
                d.compress(&data, Flush::Finish);
                let c = d.take_output();
                let (o, _) = inflate::decompress(&c, Format::Zlib, wbits).unwrap();
                assert!(o == data);
            }
        }
    }

    #[test]
    fn chunking_invariant() {
        let data = sample(200000, 3);
        for level in [1u32, 6, 9] {
            let one = compress(&data, level, Wrap::Zlib, 15);
            let mut d = Deflater::new(level, Wrap::Zlib, 15, 8, 0);
            let mut out = Vec::new();
            for ch in data.chunks(256) {
                d.compress(ch, Flush::None);
                out.extend(d.take_output());
            }
            d.compress(&[], Flush::Finish);
            out.extend(d.take_output());
            assert!(one == out, "level {level}");
        }
    }

    #[test]
    fn flushes_and_streaming_inflate() {
        let data = sample(50000, 9);
        for mode in [Flush::Sync, Flush::Full, Flush::Partial, Flush::Block] {
            for level in 0..=9 {
                let mut d = Deflater::new(level, Wrap::Zlib, 15, 8, 0);
                let mut c = Vec::new();
                d.compress(&data[..3000], Flush::None);
                d.compress(&[], mode);
                c.extend(d.take_output());
                d.compress(&data[3000..], Flush::None);
                d.compress(&[], Flush::Finish);
                c.extend(d.take_output());
                let (o, _) = inflate::decompress(&c, Format::Zlib, 15).unwrap_or_else(|e| panic!("{e:?} {mode:?} {level}"));
                assert!(o == data, "{:?} {level}", mode);
                // Byte-at-a-time streaming with a small output limit.
                let mut inf = Inflater::new(Format::Auto, 15);
                let mut got = Vec::new();
                let mut rest: &[u8] = &c;
                let mut fed = 0;
                loop {
                    let take = if rest.is_empty() { 0 } else { 1 + fed % 7 };
                    let chunk = &rest[..take.min(rest.len())];
                    let (used, st) = inf.inflate(chunk, 37).unwrap();
                    got.extend_from_slice(inf.output());
                    inf.consume_output();
                    rest = &rest[used..];
                    fed += 1;
                    if st == Status::StreamEnd {
                        break;
                    }
                    assert!(fed < 10_000_000);
                }
                assert!(got == data);
            }
        }
    }

    #[test]
    fn dictionary() {
        let dict = sample(5000, 11);
        let data = sample(3000, 11);
        for (wrap, fmt) in [(Wrap::Zlib, Format::Zlib), (Wrap::Raw, Format::Raw)] {
            let mut d = Deflater::new(6, wrap, 15, 8, 0);
            d.set_dictionary(&dict);
            d.compress(&data, Flush::Finish);
            let c = d.take_output();
            let mut inf = Inflater::new(fmt, 15);
            inf.set_dictionary(&dict);
            let (_, st) = inf.inflate(&c, usize::MAX).unwrap();
            assert_eq!(st, Status::StreamEnd);
            assert!(inf.output() == &data[..]);
            assert!(c.len() < 200);
        }
    }

    fn run_tool(args: &[&str], input: &[u8]) -> Vec<u8> {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut c = Command::new(args[0]).args(&args[1..]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let mut stdin = c.stdin.take().unwrap();
        let inp = input.to_vec();
        let t = std::thread::spawn(move || {
            stdin.write_all(&inp).unwrap();
        });
        let out = c.wait_with_output().unwrap();
        t.join().unwrap();
        assert!(out.status.success());
        out.stdout
    }

    #[test]
    fn external_gzip() {
        if std::process::Command::new("gzip").arg("--version").output().is_err() {
            return;
        }
        for n in [0usize, 10, 1000, 100000, 400000] {
            let data = sample(n, 5);
            for lvl in ["-1", "-6", "-9"] {
                let gz = run_tool(&["gzip", lvl, "-c", "-n"], &data);
                let (o, _) = inflate::decompress(&gz, Format::Gzip, 15).unwrap();
                assert!(o == data, "decoding gzip {lvl} n={n}");
            }
            for level in [0u32, 1, 6, 9] {
                let c = compress(&data, level, Wrap::Gzip, 15);
                let o = run_tool(&["gzip", "-dc"], &c);
                assert!(o == data, "gzip decoding ours level {level} n={n}");
            }
        }
    }

    #[test]
    fn partial_level0() {
        let data = sample(50000, 9);
        let mut d = Deflater::new(0, Wrap::Zlib, 15, 8, 0);
        let mut c = Vec::new();
        d.compress(&data[..3000], Flush::None);
        d.compress(&[], Flush::Partial);
        c.extend(d.take_output());
        d.compress(&data[3000..], Flush::None);
        d.compress(&[], Flush::Finish);
        c.extend(d.take_output());
        let o = inflate::decompress(&c, Format::Zlib, 15).unwrap().0;
        assert_eq!(o.len(), data.len());
        let p = o.iter().zip(&data).position(|(a, b)| a != b);
        assert_eq!(p, None, "{:?}", &c[..20]);
    }

    #[test]
    fn known_streams() {
        let x = b"x\x9cK\xcb\xcf\x07\x00\x02\x82\x01E";
        assert_eq!(inflate::decompress(x, Format::Zlib, 15).unwrap().0, b"foo");
        let e = compress(b"", 6, Wrap::Zlib, 15);
        assert_eq!(e, b"x\x9c\x03\x00\x00\x00\x00\x01");
        // Truncation / unused data.
        let mut inf = Inflater::new(Format::Zlib, 15);
        let mut v = x.to_vec();
        v.extend_from_slice(b"0123");
        let (_, st) = inf.inflate(&v, usize::MAX).unwrap();
        assert_eq!(st, Status::StreamEnd);
        assert_eq!(inf.take_unused(), b"0123");
    }
}
