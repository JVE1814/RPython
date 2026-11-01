//! Streaming DEFLATE decoder (RFC 1951) with zlib (RFC 1950) and gzip (RFC 1952) wrappers.
//!
//! The decoder is resumable at "unit" granularity: every state handler either completes a
//! unit (a symbol, a block header, a header field) or rolls back to the checkpoint taken at
//! the start of the unit and reports that it needs more input.  Partial units are kept in an
//! internal `pending` buffer, so from the caller's point of view all input is consumed unless
//! the output limit is reached or the stream ends.

use crate::checksum::{adler32, crc32};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Raw,
    Zlib,
    Gzip,
    /// zlib or gzip, detected from the first two bytes.
    Auto,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    StreamEnd,
    NeedInput,
    OutputFull,
}

/// A zlib-style error: `code` is the zlib return code (-3 Z_DATA_ERROR, 2 Z_NEED_DICT, ...).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct InflateError {
    pub code: i32,
    pub msg: Option<&'static str>,
}

const fn data_err(m: &'static str) -> InflateError {
    InflateError { code: -3, msg: Some(m) }
}

pub const NEED_DICT: InflateError = InflateError { code: 2, msg: None };

// ---------------------------------------------------------------- decode tables
//
// Entry layout (u32): bits 0..4 total code length, bits 4..7 kind, bits 8..15 extra bits
// (or subtable index bits), bits 16..31 value (literal / base / subtable offset).

const K_LIT: u32 = 0;
const K_BASE: u32 = 1;
const K_EOB: u32 = 2;
const K_SUB: u32 = 3;
const K_INVALID: u32 = 4;

const INVALID_ENTRY: u32 = K_INVALID << 4;

#[inline(always)]
fn e_len(e: u32) -> u32 {
    e & 15
}
#[inline(always)]
fn e_kind(e: u32) -> u32 {
    (e >> 4) & 15
}
#[inline(always)]
fn e_extra(e: u32) -> u32 {
    (e >> 8) & 0xff
}
#[inline(always)]
fn e_val(e: u32) -> u32 {
    e >> 16
}

const LBITS: u32 = 10;
const DBITS: u32 = 8;
const CLBITS: u32 = 7;

static LEN_BASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
static LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
static DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
static DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
static CL_ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];


#[derive(PartialEq)]
enum TableKind {
    CodeLens,
    LitLen,
    Dist,
}

fn sym_entry(kind: &TableKind, sym: usize) -> u32 {
    match kind {
        TableKind::CodeLens => (K_LIT << 4) | ((sym as u32) << 16),
        TableKind::LitLen => {
            if sym < 256 {
                (K_LIT << 4) | ((sym as u32) << 16)
            } else if sym == 256 {
                K_EOB << 4
            } else if sym < 286 {
                let i = sym - 257;
                (K_BASE << 4) | ((LEN_EXTRA[i] as u32) << 8) | ((LEN_BASE[i] as u32) << 16)
            } else {
                INVALID_ENTRY
            }
        }
        TableKind::Dist => {
            if sym < 30 {
                (K_BASE << 4) | ((DIST_EXTRA[sym] as u32) << 8) | ((DIST_BASE[sym] as u32) << 16)
            } else {
                INVALID_ENTRY
            }
        }
    }
}

/// Build a decode table; returns false for an invalid (over-subscribed or illegally
/// incomplete) code.
fn build_table(lens: &[u8], pbits: u32, kind: TableKind, table: &mut Vec<u32>) -> bool {
    let mut count = [0i32; 16];
    for &l in lens {
        count[l as usize] += 1;
    }
    count[0] = 0;
    let mut max = 0;
    for l in 1..16 {
        if count[l] > 0 {
            max = l;
        }
    }
    table.clear();
    table.resize(1 << pbits, INVALID_ENTRY);
    if max == 0 {
        // No codes at all: legal for the distance code (zlib behaviour), every lookup is invalid.
        return kind != TableKind::CodeLens;
    }
    let mut left: i32 = 1;
    for l in 1..16 {
        left <<= 1;
        left -= count[l];
        if left < 0 {
            return false;
        }
    }
    if left > 0 && (kind == TableKind::CodeLens || max != 1) {
        return false;
    }
    let mut next = [0u32; 16];
    let mut code = 0u32;
    for l in 1..16 {
        code = (code + count[l - 1] as u32) << 1;
        next[l] = code;
    }
    let mut codes = vec![0u32; lens.len()];
    for (s, &l) in lens.iter().enumerate() {
        if l > 0 {
            codes[s] = crate::huffman::reverse_bits(next[l as usize], l as u32);
            next[l as usize] += 1;
        }
    }
    let psize = 1usize << pbits;
    // Subtable sizes per primary prefix.
    let mut subbits = vec![0u8; if max as u32 > pbits { psize } else { 0 }];
    for (s, &l) in lens.iter().enumerate() {
        if l as u32 > pbits {
            let pre = (codes[s] & (psize as u32 - 1)) as usize;
            let need = l as u32 - pbits;
            if need as u8 > subbits[pre] {
                subbits[pre] = need as u8;
            }
        }
    }
    if !subbits.is_empty() {
        for pre in 0..psize {
            if subbits[pre] > 0 {
                let off = table.len();
                table.resize(off + (1 << subbits[pre]), INVALID_ENTRY);
                table[pre] = (K_SUB << 4) | ((subbits[pre] as u32) << 8) | ((off as u32) << 16) | pbits;
            }
        }
    }
    for (s, &l) in lens.iter().enumerate() {
        if l == 0 {
            continue;
        }
        let l = l as u32;
        let e = sym_entry(&kind, s) | l;
        let c = codes[s];
        if l <= pbits {
            let mut i = c as usize;
            while i < psize {
                table[i] = e;
                i += 1 << l;
            }
        } else {
            let pre = (c & (psize as u32 - 1)) as usize;
            let link = table[pre];
            let off = e_val(link) as usize;
            let sb = e_extra(link);
            let mut i = (c >> pbits) as usize;
            while i < (1usize << sb) {
                table[off + i] = e;
                i += 1 << (l - pbits);
            }
        }
    }
    true
}

struct FixedTables {
    lit: Vec<u32>,
    dist: Vec<u32>,
}

static FIXED: std::sync::OnceLock<FixedTables> = std::sync::OnceLock::new();

fn fixed_tables() -> &'static FixedTables {
    FIXED.get_or_init(|| {
        let mut l = [0u8; 288];
        for (i, x) in l.iter_mut().enumerate() {
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
        let mut lit = Vec::new();
        build_table(&l, LBITS, TableKind::LitLen, &mut lit);
        let d = [5u8; 32];
        let mut dist = Vec::new();
        build_table(&d, DBITS, TableKind::Dist, &mut dist);
        FixedTables { lit, dist }
    })
}

// ---------------------------------------------------------------- state

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GzStep {
    Fixed,
    ExtraLen,
    Extra(u32),
    Name,
    Comment,
    Hcrc,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Header,
    DictId,
    Gz(GzStep),
    BlockHeader,
    StoredLen,
    Stored(u32),
    DynHeader,
    Codes,
    Match(u32, u32),
    Trailer,
    Done,
}

enum Flow {
    Next,
    /// Roll back to the unit checkpoint and wait for input.
    NeedInput,
    /// Progress was committed; wait for input without rolling back.
    Committed,
    OutputFull,
    End,
}

/// Window history kept between calls.
const HIST: usize = 32768;

#[derive(Clone)]
pub struct Inflater {
    format: Format,
    wbits: u32,
    state: State,
    last: bool,
    gzip: bool,
    gz_flags: u32,
    hcrc: u32,
    bitbuf: u64,
    bitcnt: u32,
    win: Vec<u8>,
    out_start: usize,
    check_pos: usize,
    check: u32,
    total_out: u64,
    dict: Option<Vec<u8>>,
    lit: Vec<u32>,
    dist: Vec<u32>,
    pending: Vec<u8>,
    unused: Vec<u8>,
}

impl Inflater {
    /// `wbits`: 8..=15 window bits, or 0 to take the window size from the zlib header.
    pub fn new(format: Format, wbits: u32) -> Inflater {
        Inflater {
            format,
            wbits,
            state: if format == Format::Raw { State::BlockHeader } else { State::Header },
            last: false,
            gzip: false,
            gz_flags: 0,
            hcrc: 0,
            bitbuf: 0,
            bitcnt: 0,
            win: Vec::new(),
            out_start: 0,
            check_pos: 0,
            check: if format == Format::Raw { 0 } else { 1 },
            total_out: 0,
            dict: None,
            lit: Vec::new(),
            dist: Vec::new(),
            pending: Vec::new(),
            unused: Vec::new(),
        }
    }

    /// Supply a preset dictionary. For raw streams it is loaded into the window immediately;
    /// for zlib streams it is used when the header requests it.
    pub fn set_dictionary(&mut self, d: &[u8]) {
        if self.format == Format::Raw {
            self.load_dict(d);
        } else {
            self.dict = Some(d.to_vec());
        }
    }

    fn load_dict(&mut self, d: &[u8]) {
        let d = if d.len() > HIST { &d[d.len() - HIST..] } else { d };
        self.win.extend_from_slice(d);
        self.out_start = self.win.len();
        self.check_pos = self.win.len();
    }

    pub fn is_done(&self) -> bool {
        self.state == State::Done
    }

    /// Bytes following the end of the stream (only meaningful once done).
    pub fn take_unused(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.unused)
    }

    /// Number of input bytes held internally (part of an unfinished unit).
    pub fn pending_input(&self) -> usize {
        self.pending.len()
    }

    /// Output produced since the last `consume_output`.
    pub fn output(&self) -> &[u8] {
        &self.win[self.out_start..]
    }

    pub fn output_len(&self) -> usize {
        self.win.len() - self.out_start
    }

    /// Mark all output as taken; trims the history window.
    pub fn consume_output(&mut self) {
        self.update_check();
        let n = self.win.len();
        if n > 4 * HIST {
            if self.win.capacity() > (1 << 20) {
                let tail = self.win[n - HIST..].to_vec();
                let mut w = Vec::with_capacity(4 * HIST);
                w.extend_from_slice(&tail);
                self.win = w;
            } else {
                self.win.drain(..n - HIST);
            }
        }
        self.out_start = self.win.len();
        self.check_pos = self.win.len();
    }

    fn update_check(&mut self) {
        if self.check_pos < self.win.len() {
            let d = &self.win[self.check_pos..];
            self.total_out += d.len() as u64;
            match self.format {
                Format::Raw => {}
                _ => {
                    if self.gzip {
                        self.check = crc32(self.check, d);
                    } else {
                        self.check = adler32(self.check, d);
                    }
                }
            }
            self.check_pos = self.win.len();
        }
    }

    /// Decompress from `input`, producing at most `max_out` more bytes of output (in addition
    /// to output not yet consumed).  Returns the number of input bytes consumed.
    pub fn inflate(&mut self, input: &[u8], max_out: usize) -> Result<(usize, Status), InflateError> {
        let limit_end = self.win.len().saturating_add(max_out);
        if self.pending.is_empty() {
            let mut pos = 0;
            let st = self.run(input, &mut pos, limit_end)?;
            return Ok(self.finish(st, input, pos, input.len()));
        }
        let plen = self.pending.len();
        let take = input.len().min(4096);
        let mut buf = std::mem::take(&mut self.pending);
        buf.extend_from_slice(&input[..take]);
        let mut pos = 0;
        let st = self.run(&buf, &mut pos, limit_end)?;
        match st {
            Status::NeedInput => {
                if pos >= plen && take < input.len() {
                    let mut p2 = pos - plen;
                    let st2 = self.run(input, &mut p2, limit_end)?;
                    Ok(self.finish(st2, input, p2, input.len()))
                } else {
                    self.pending = buf[pos..].to_vec();
                    self.pending.extend_from_slice(&input[take..]);
                    Ok((input.len(), Status::NeedInput))
                }
            }
            Status::OutputFull => {
                if pos >= plen {
                    Ok((pos - plen, Status::OutputFull))
                } else {
                    self.pending = buf[pos..plen].to_vec();
                    Ok((0, Status::OutputFull))
                }
            }
            Status::StreamEnd => {
                self.unused.extend_from_slice(&buf[pos..]);
                self.unused.extend_from_slice(&input[take..]);
                Ok((input.len(), Status::StreamEnd))
            }
        }
    }

    fn finish(&mut self, st: Status, src: &[u8], pos: usize, len: usize) -> (usize, Status) {
        match st {
            Status::NeedInput => {
                self.pending.extend_from_slice(&src[pos..]);
                (len, st)
            }
            Status::OutputFull => (pos, st),
            Status::StreamEnd => {
                self.unused.extend_from_slice(&src[pos..]);
                (len, st)
            }
        }
    }

    // ------------------------------------------------------------ bit helpers

    #[inline]
    fn pull_byte(&mut self, buf: &[u8], pos: &mut usize) -> bool {
        if *pos < buf.len() {
            self.bitbuf |= (buf[*pos] as u64) << self.bitcnt;
            self.bitcnt += 8;
            *pos += 1;
            true
        } else {
            false
        }
    }

    #[inline]
    fn need(&mut self, n: u32, buf: &[u8], pos: &mut usize) -> bool {
        while self.bitcnt < n {
            if !self.pull_byte(buf, pos) {
                return false;
            }
        }
        true
    }

    #[inline]
    fn bits(&self, n: u32) -> u32 {
        (self.bitbuf & ((1u64 << n) - 1)) as u32
    }

    #[inline]
    fn drop_bits(&mut self, n: u32) {
        self.bitbuf >>= n;
        self.bitcnt -= n;
    }

    fn align(&mut self) {
        let r = self.bitcnt & 7;
        self.drop_bits(r);
    }

    /// Read one byte at a byte boundary (call `align` first).
    fn byte(&mut self, buf: &[u8], pos: &mut usize) -> Option<u8> {
        if !self.need(8, buf, pos) {
            return None;
        }
        let b = self.bits(8) as u8;
        self.drop_bits(8);
        Some(b)
    }

    // ------------------------------------------------------------ main loop

    fn run(&mut self, buf: &[u8], pos: &mut usize, limit_end: usize) -> Result<Status, InflateError> {
        loop {
            if self.state == State::Done {
                return Ok(Status::StreamEnd);
            }
            let save = (*pos, self.bitbuf, self.bitcnt, self.state);
            let f = self.step(buf, pos, limit_end);
            let f = match f {
                Ok(f) => f,
                Err(e) => return Err(e),
            };
            match f {
                Flow::Next => {}
                Flow::NeedInput => {
                    *pos = save.0;
                    self.bitbuf = save.1;
                    self.bitcnt = save.2;
                    self.state = save.3;
                    return Ok(Status::NeedInput);
                }
                Flow::Committed => return Ok(Status::NeedInput),
                Flow::OutputFull => return Ok(Status::OutputFull),
                Flow::End => {
                    self.state = State::Done;
                    // Give back whole bytes still in the bit buffer.
                    self.align();
                    while self.bitcnt >= 8 {
                        self.unused.push(self.bits(8) as u8);
                        self.drop_bits(8);
                    }
                    self.bitbuf = 0;
                    self.bitcnt = 0;
                    return Ok(Status::StreamEnd);
                }
            }
        }
    }

    fn step(&mut self, buf: &[u8], pos: &mut usize, limit_end: usize) -> Result<Flow, InflateError> {
        macro_rules! need {
            ($n:expr) => {
                if !self.need($n, buf, pos) {
                    return Ok(Flow::NeedInput);
                }
            };
        }
        macro_rules! byte {
            () => {
                match self.byte(buf, pos) {
                    Some(b) => b,
                    None => return Ok(Flow::NeedInput),
                }
            };
        }
        match self.state {
            State::Header => {
                need!(16);
                let b0 = self.bits(8);
                let b1 = (self.bits(16) >> 8) & 0xff;
                if (self.format == Format::Gzip || self.format == Format::Auto) && b0 == 0x1f && b1 == 0x8b {
                    self.gzip = true;
                    self.check = 0;
                    self.state = State::Gz(GzStep::Fixed);
                    return Ok(Flow::Next);
                }
                if self.format == Format::Gzip || ((b0 << 8) + b1) % 31 != 0 {
                    return Err(data_err("incorrect header check"));
                }
                if b0 & 15 != 8 {
                    return Err(data_err("unknown compression method"));
                }
                let len = (b0 >> 4) + 8;
                if self.wbits == 0 {
                    self.wbits = len;
                }
                if len > 15 || len > self.wbits {
                    return Err(data_err("invalid window size"));
                }
                self.drop_bits(16);
                self.state = if b1 & 0x20 != 0 { State::DictId } else { State::BlockHeader };
                Ok(Flow::Next)
            }
            State::DictId => {
                need!(32);
                let v = self.bits(32);
                self.drop_bits(32);
                let id = v.swap_bytes();
                match self.dict.take() {
                    None => {
                        // Leave the stream positioned after the dictionary id so a later
                        // dictionary could be supplied; report Z_NEED_DICT.
                        self.state = State::BlockHeader;
                        Err(NEED_DICT)
                    }
                    Some(d) => {
                        if adler32(1, &d) != id {
                            return Err(data_err("incorrect data check"));
                        }
                        self.load_dict(&d);
                        self.state = State::BlockHeader;
                        Ok(Flow::Next)
                    }
                }
            }
            State::Gz(step) => self.gz_header(step, buf, pos),
            State::BlockHeader => {
                need!(3);
                let v = self.bits(3);
                self.drop_bits(3);
                self.last = v & 1 != 0;
                match v >> 1 {
                    0 => self.state = State::StoredLen,
                    1 => {
                        let f = fixed_tables();
                        self.lit.clear();
                        self.lit.extend_from_slice(&f.lit);
                        self.dist.clear();
                        self.dist.extend_from_slice(&f.dist);
                        self.state = State::Codes;
                    }
                    2 => self.state = State::DynHeader,
                    _ => return Err(data_err("invalid block type")),
                }
                Ok(Flow::Next)
            }
            State::StoredLen => {
                self.align();
                need!(32);
                let v = self.bits(32);
                self.drop_bits(32);
                let len = v & 0xffff;
                if len != (!v >> 16) & 0xffff {
                    return Err(data_err("invalid stored block lengths"));
                }
                self.state = State::Stored(len);
                Ok(Flow::Next)
            }
            State::Stored(mut rem) => {
                // Copy bytes buffered in the bit buffer first, then straight from the input.
                while rem > 0 && self.bitcnt >= 8 {
                    if self.win.len() >= limit_end {
                        self.state = State::Stored(rem);
                        return Ok(Flow::OutputFull);
                    }
                    self.win.push(self.bits(8) as u8);
                    self.drop_bits(8);
                    rem -= 1;
                }
                if rem > 0 {
                    // The bit buffer is empty now; drop look-ahead bits (they may describe
                    // bytes we are about to copy directly).
                    self.bitbuf = 0;
                    let space = limit_end - self.win.len().min(limit_end);
                    let avail = buf.len() - *pos;
                    let n = (rem as usize).min(avail).min(space);
                    self.win.extend_from_slice(&buf[*pos..*pos + n]);
                    *pos += n;
                    rem -= n as u32;
                    if rem > 0 {
                        self.state = State::Stored(rem);
                        // The copied part is committed: report the reason we stopped.
                        if n == space {
                            return Ok(Flow::OutputFull);
                        }
                        return Ok(Flow::Committed);
                    }
                }
                self.state = self.after_block();
                Ok(Flow::Next)
            }
            State::DynHeader => self.dyn_header(buf, pos),
            State::Codes => self.codes(buf, pos, limit_end),
            State::Match(len, dist) => {
                let space = limit_end.saturating_sub(self.win.len());
                if space == 0 {
                    return Ok(Flow::OutputFull);
                }
                let n = (len as usize).min(space);
                copy_match(&mut self.win, dist as usize, n);
                if (n as u32) < len {
                    self.state = State::Match(len - n as u32, dist);
                    return Ok(Flow::OutputFull);
                }
                self.state = State::Codes;
                Ok(Flow::Next)
            }
            State::Trailer => {
                self.align();
                if self.format == Format::Raw {
                    return Ok(Flow::End);
                }
                self.update_check();
                if self.gzip {
                    need!(64);
                    let crc = self.bits(32);
                    self.drop_bits(32);
                    let isize = self.bits(32);
                    self.drop_bits(32);
                    if crc != self.check {
                        return Err(data_err("incorrect data check"));
                    }
                    if isize != self.total_out as u32 {
                        return Err(data_err("incorrect length check"));
                    }
                    Ok(Flow::End)
                } else {
                    need!(32);
                    let v = self.bits(32).swap_bytes();
                    if v != self.check {
                        return Err(data_err("incorrect data check"));
                    }
                    self.drop_bits(32);
                    Ok(Flow::End)
                }
            }
            State::Done => Ok(Flow::End),
        }
    }

    fn after_block(&self) -> State {
        if self.last { State::Trailer } else { State::BlockHeader }
    }
}

#[inline(always)]
fn lookup(table: &[u32], pbits: u32, bb: u64) -> u32 {
    let e = table[(bb & ((1u64 << pbits) - 1)) as usize];
    if e_kind(e) == K_SUB {
        table[e_val(e) as usize + ((bb >> pbits) & ((1u64 << e_extra(e)) - 1)) as usize]
    } else {
        e
    }
}

/// Decode one symbol with only the bits available; pulls bytes as needed.  Returns None if
/// the input ran out before a complete code was available.
fn decode_slow(table: &[u32], pbits: u32, bb: &mut u64, cnt: &mut u32, buf: &[u8], pos: &mut usize) -> Option<u32> {
    loop {
        let e = lookup(table, pbits, *bb);
        let l = e_len(e);
        if (l != 0 && l <= *cnt) || (l == 0 && *cnt >= 15) {
            return Some(e);
        }
        if *pos >= buf.len() {
            return None;
        }
        *bb |= (buf[*pos] as u64) << *cnt;
        *cnt += 8;
        *pos += 1;
    }
}

#[inline]
fn copy_match(win: &mut Vec<u8>, dist: usize, len: usize) {
    let n = win.len();
    win.reserve(len + 16);
    unsafe {
        let p = win.as_mut_ptr();
        let src = p.add(n - dist);
        let dst = p.add(n);
        if dist >= 16 {
            let mut i = 0;
            while i < len {
                std::ptr::copy_nonoverlapping(src.add(i), dst.add(i), 16);
                i += 16;
            }
        } else if dist == 1 {
            std::ptr::write_bytes(dst, *src, len);
        } else {
            for i in 0..len {
                *dst.add(i) = *src.add(i);
            }
        }
        win.set_len(n + len);
    }
}

impl Inflater {
    fn gz_next(&self, after: GzStep) -> State {
        let f = self.gz_flags;
        let order = [(GzStep::ExtraLen, 4u32), (GzStep::Name, 8), (GzStep::Comment, 16), (GzStep::Hcrc, 2)];
        let start = match after {
            GzStep::Fixed => 0,
            GzStep::ExtraLen | GzStep::Extra(_) => 1,
            GzStep::Name => 2,
            GzStep::Comment => 3,
            GzStep::Hcrc => 4,
        };
        for &(s, bit) in &order[start..] {
            if f & bit != 0 {
                return State::Gz(s);
            }
        }
        State::BlockHeader
    }

    fn gz_header(&mut self, step: GzStep, buf: &[u8], pos: &mut usize) -> Result<Flow, InflateError> {
        macro_rules! byte {
            () => {
                match self.byte(buf, pos) {
                    Some(b) => b,
                    None => return Ok(Flow::NeedInput),
                }
            };
        }
        match step {
            GzStep::Fixed => {
                let mut h = [0u8; 10];
                for b in h.iter_mut() {
                    *b = byte!();
                }
                if h[2] != 8 {
                    return Err(data_err("unknown compression method"));
                }
                if h[3] & 0xe0 != 0 {
                    return Err(data_err("unknown header flags set"));
                }
                self.gz_flags = h[3] as u32;
                self.hcrc = crc32(0, &h);
                self.state = self.gz_next(GzStep::Fixed);
            }
            GzStep::ExtraLen => {
                let a = byte!();
                let b = byte!();
                self.hcrc = crc32(self.hcrc, &[a, b]);
                let n = a as u32 | ((b as u32) << 8);
                self.state = if n > 0 { State::Gz(GzStep::Extra(n)) } else { self.gz_next(GzStep::ExtraLen) };
            }
            GzStep::Extra(mut rem) => {
                while rem > 0 {
                    match self.byte(buf, pos) {
                        Some(b) => {
                            self.hcrc = crc32(self.hcrc, &[b]);
                            rem -= 1;
                            self.state = State::Gz(GzStep::Extra(rem));
                        }
                        None => return Ok(Flow::Committed),
                    }
                }
                self.state = self.gz_next(GzStep::ExtraLen);
            }
            GzStep::Name | GzStep::Comment => loop {
                match self.byte(buf, pos) {
                    Some(b) => {
                        self.hcrc = crc32(self.hcrc, &[b]);
                        if b == 0 {
                            self.state = self.gz_next(step);
                            break;
                        }
                    }
                    None => return Ok(Flow::Committed),
                }
            },
            GzStep::Hcrc => {
                let a = byte!();
                let b = byte!();
                let v = a as u32 | ((b as u32) << 8);
                if v != self.hcrc & 0xffff {
                    return Err(data_err("header crc mismatch"));
                }
                self.state = State::BlockHeader;
            }
        }
        Ok(Flow::Next)
    }

    fn dyn_header(&mut self, buf: &[u8], pos: &mut usize) -> Result<Flow, InflateError> {
        macro_rules! need {
            ($n:expr) => {
                if !self.need($n, buf, pos) {
                    return Ok(Flow::NeedInput);
                }
            };
        }
        need!(14);
        let nlen = self.bits(5) as usize + 257;
        self.drop_bits(5);
        let ndist = self.bits(5) as usize + 1;
        self.drop_bits(5);
        let ncode = self.bits(4) as usize + 4;
        self.drop_bits(4);
        if nlen > 286 || ndist > 30 {
            return Err(data_err("too many length or distance symbols"));
        }
        let mut cl = [0u8; 19];
        for i in 0..ncode {
            need!(3);
            cl[CL_ORDER[i]] = self.bits(3) as u8;
            self.drop_bits(3);
        }
        let mut cltab = Vec::new();
        if !build_table(&cl, CLBITS, TableKind::CodeLens, &mut cltab) {
            return Err(data_err("invalid code lengths set"));
        }
        let total = nlen + ndist;
        let mut lens = [0u8; 320];
        let mut i = 0;
        while i < total {
            let e = match decode_slow(&cltab, CLBITS, &mut self.bitbuf, &mut self.bitcnt, buf, pos) {
                Some(e) => e,
                None => return Ok(Flow::NeedInput),
            };
            if e_kind(e) == K_INVALID {
                return Err(data_err("invalid code lengths set"));
            }
            self.drop_bits(e_len(e));
            let sym = e_val(e);
            if sym < 16 {
                lens[i] = sym as u8;
                i += 1;
                continue;
            }
            let (val, rep) = match sym {
                16 => {
                    need!(2);
                    if i == 0 {
                        return Err(data_err("invalid bit length repeat"));
                    }
                    let r = 3 + self.bits(2) as usize;
                    self.drop_bits(2);
                    (lens[i - 1], r)
                }
                17 => {
                    need!(3);
                    let r = 3 + self.bits(3) as usize;
                    self.drop_bits(3);
                    (0, r)
                }
                _ => {
                    need!(7);
                    let r = 11 + self.bits(7) as usize;
                    self.drop_bits(7);
                    (0, r)
                }
            };
            if i + rep > total {
                return Err(data_err("invalid bit length repeat"));
            }
            for _ in 0..rep {
                lens[i] = val;
                i += 1;
            }
        }
        if lens[256] == 0 {
            return Err(data_err("invalid code -- missing end-of-block"));
        }
        let mut lit = std::mem::take(&mut self.lit);
        let ok = build_table(&lens[..nlen], LBITS, TableKind::LitLen, &mut lit);
        self.lit = lit;
        if !ok {
            return Err(data_err("invalid literal/lengths set"));
        }
        let mut dist = std::mem::take(&mut self.dist);
        let ok = build_table(&lens[nlen..total], DBITS, TableKind::Dist, &mut dist);
        self.dist = dist;
        if !ok {
            return Err(data_err("invalid distances set"));
        }
        self.state = State::Codes;
        Ok(Flow::Next)
    }

    fn codes(&mut self, buf: &[u8], pos: &mut usize, limit_end: usize) -> Result<Flow, InflateError> {
        loop {
            if buf.len() - *pos >= 8 && self.win.len().saturating_add(258) <= limit_end {
                if let Some(f) = self.codes_fast(buf, pos, limit_end)? {
                    return Ok(f);
                }
            }
            // Slow path: one symbol at a time with explicit availability checks.
            if self.win.len() >= limit_end {
                return Ok(Flow::OutputFull);
            }
            let save = (*pos, self.bitbuf, self.bitcnt);
            macro_rules! restore {
                () => {{
                    *pos = save.0;
                    self.bitbuf = save.1;
                    self.bitcnt = save.2;
                    return Ok(Flow::Committed);
                }};
            }
            let e = match decode_slow(&self.lit, LBITS, &mut self.bitbuf, &mut self.bitcnt, buf, pos) {
                Some(e) => e,
                None => restore!(),
            };
            match e_kind(e) {
                K_LIT => {
                    self.drop_bits(e_len(e));
                    self.win.push(e_val(e) as u8);
                }
                K_EOB => {
                    self.drop_bits(e_len(e));
                    self.state = self.after_block();
                    return Ok(Flow::Next);
                }
                K_BASE => {
                    let l = e_len(e);
                    let x = e_extra(e);
                    if !self.need(l + x, buf, pos) {
                        restore!();
                    }
                    self.drop_bits(l);
                    let len = e_val(e) + self.bits(x);
                    self.drop_bits(x);
                    let de = match decode_slow(&self.dist, DBITS, &mut self.bitbuf, &mut self.bitcnt, buf, pos) {
                        Some(e) => e,
                        None => restore!(),
                    };
                    if e_kind(de) != K_BASE {
                        return Err(data_err("invalid distance code"));
                    }
                    let l = e_len(de);
                    let x = e_extra(de);
                    if !self.need(l + x, buf, pos) {
                        restore!();
                    }
                    self.drop_bits(l);
                    let dist = e_val(de) + self.bits(x);
                    self.drop_bits(x);
                    if dist as usize > self.win.len() {
                        return Err(data_err("invalid distance too far back"));
                    }
                    let space = limit_end - self.win.len();
                    let n = (len as usize).min(space);
                    copy_match(&mut self.win, dist as usize, n);
                    if (n as u32) < len {
                        self.state = State::Match(len - n as u32, dist);
                        return Ok(Flow::OutputFull);
                    }
                }
                _ => return Err(data_err("invalid literal/length code")),
            }
        }
    }

    /// Fast decode loop: requires >= 8 input bytes and >= 258 bytes of output room per symbol.
    /// Returns None when those conditions no longer hold.
    #[inline(never)]
    fn codes_fast(&mut self, buf: &[u8], pos: &mut usize, limit_end: usize) -> Result<Option<Flow>, InflateError> {
        let Inflater { lit, dist: dtab, win, bitbuf, bitcnt, state, last, .. } = self;
        let mut bb = *bitbuf;
        let mut cnt = *bitcnt;
        let mut p = *pos;
        let end = buf.len();
        let lmask = (1u64 << LBITS) - 1;
        let mut res = Ok(None);
        while p + 8 <= end && win.len().saturating_add(258) <= limit_end {
            if win.capacity() - win.len() < 300 {
                let extra = win.len().max(1 << 16);
                win.reserve(extra);
            }
            let v = u64::from_le_bytes(buf[p..p + 8].try_into().unwrap());
            bb |= v << cnt;
            p += ((63 - cnt) >> 3) as usize;
            cnt |= 56;
            let mut e = lit[(bb & lmask) as usize];
            if e_kind(e) == K_SUB {
                e = lit[e_val(e) as usize + ((bb >> LBITS) & ((1u64 << e_extra(e)) - 1)) as usize];
            }
            let k = e_kind(e);
            if k == K_LIT {
                let l = e_len(e);
                bb >>= l;
                cnt -= l;
                win.push(e_val(e) as u8);
                // Second literal without refill (needs <= 15 bits; at least 41 remain).
                let mut e2 = lit[(bb & lmask) as usize];
                if e_kind(e2) == K_SUB {
                    e2 = lit[e_val(e2) as usize + ((bb >> LBITS) & ((1u64 << e_extra(e2)) - 1)) as usize];
                }
                if e_kind(e2) == K_LIT {
                    let l = e_len(e2);
                    bb >>= l;
                    cnt -= l;
                    win.push(e_val(e2) as u8);
                }
                continue;
            }
            if k == K_BASE {
                let l = e_len(e);
                let x = e_extra(e);
                bb >>= l;
                let len = e_val(e) + (bb & ((1u64 << x) - 1)) as u32;
                bb >>= x;
                cnt -= l + x;
                let mut de = dtab[(bb & ((1u64 << DBITS) - 1)) as usize];
                if e_kind(de) == K_SUB {
                    de = dtab[e_val(de) as usize + ((bb >> DBITS) & ((1u64 << e_extra(de)) - 1)) as usize];
                }
                if e_kind(de) != K_BASE {
                    res = Err(data_err("invalid distance code"));
                    break;
                }
                let l = e_len(de);
                let x = e_extra(de);
                bb >>= l;
                let d = e_val(de) + (bb & ((1u64 << x) - 1)) as u32;
                bb >>= x;
                cnt -= l + x;
                if d as usize > win.len() {
                    res = Err(data_err("invalid distance too far back"));
                    break;
                }
                copy_match(win, d as usize, len as usize);
                continue;
            }
            if k == K_EOB {
                let l = e_len(e);
                bb >>= l;
                cnt -= l;
                *state = if *last { State::Trailer } else { State::BlockHeader };
                res = Ok(Some(Flow::Next));
                break;
            }
            res = Err(data_err("invalid literal/length code"));
            break;
        }
        *bitbuf = bb;
        *bitcnt = cnt;
        *pos = p;
        res
    }
}

/// One-shot helper: decompress a complete stream.  Returns (output, bytes consumed).
pub fn decompress(data: &[u8], format: Format, wbits: u32) -> Result<(Vec<u8>, usize), InflateError> {
    let mut inf = Inflater::new(format, wbits);
    let (_, st) = inf.inflate(data, usize::MAX)?;
    if st != Status::StreamEnd {
        return Err(InflateError { code: -5, msg: Some("incomplete or truncated stream") });
    }
    let out = inf.output().to_vec();
    let unused = inf.unused.len();
    Ok((out, data.len() - unused))
}
