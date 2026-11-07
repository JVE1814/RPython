//! LZMA, LZMA2 and the .xz / .lzma containers (written from the published .xz file format
//! description and the LZMA algorithm as documented in the LZMA SDK specification).
//!
//! Formats: `.xz` (CRC32 / CRC64 / SHA-256 checks), legacy `.lzma` ("alone"), and raw filter
//! chains (up to three of Delta / BCJ followed by LZMA1 or LZMA2).

pub mod dec;
pub mod enc;
pub mod filters;
pub mod model;

use crate::checksum;
use crate::hash::{Digest, Sha256};
use dec::{Lzma1Dec, Lzma2Dec, Window};
pub use enc::{LzOptions, PRESET_EXTREME};
use filters::*;
use model::Props;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Corrupt input.
    Data,
    /// The input is not in the expected container format.
    Format,
    /// Unsupported or invalid options (also bad filter properties).
    Options,
    /// The memory limit is too low for the stream.
    MemLimit,
    /// Integrity check type not supported (encoder side).
    UnsupportedCheck,
    /// Internal: retry later.
    Retry,
}

impl Error {
    pub fn message(self) -> &'static str {
        match self {
            Error::Data | Error::Retry => "Corrupt input data",
            Error::Format => "Input format not supported by decoder",
            Error::Options => "Invalid or unsupported options",
            Error::MemLimit => "Memory usage limit exceeded",
            Error::UnsupportedCheck => "Unsupported integrity check",
        }
    }
}

pub const CHECK_NONE: u32 = 0;
pub const CHECK_CRC32: u32 = 1;
pub const CHECK_CRC64: u32 = 4;
pub const CHECK_SHA256: u32 = 10;
pub const CHECK_ID_MAX: u32 = 15;
pub const CHECK_UNKNOWN: u32 = 16;

pub fn check_supported(id: u32) -> bool {
    matches!(id, CHECK_NONE | CHECK_CRC32 | CHECK_CRC64 | CHECK_SHA256)
}

fn check_size(id: u32) -> usize {
    match id {
        0 => 0,
        1..=3 => 4,
        4..=6 => 8,
        7..=9 => 16,
        10..=12 => 32,
        _ => 64,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Auto,
    Xz,
    Alone,
    Raw,
}

/// One entry of a filter chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterSpec {
    pub id: u64,
    /// LZMA1 / LZMA2 options.
    pub lz: Option<LzOptions>,
    /// Delta distance (1..=256).
    pub dist: u32,
    /// BCJ start offset.
    pub start_offset: u32,
}

impl FilterSpec {
    pub fn lzma(id: u64, o: LzOptions) -> FilterSpec {
        FilterSpec { id, lz: Some(o), dist: 1, start_offset: 0 }
    }
    pub fn simple(id: u64) -> FilterSpec {
        FilterSpec { id, lz: None, dist: 1, start_offset: 0 }
    }

    pub fn known(id: u64) -> bool {
        id == FILTER_LZMA1 || id == FILTER_LZMA2 || id == FILTER_DELTA || is_bcj(id)
    }

    /// Encoded filter properties (as stored in .xz block headers and zip LZMA entries).
    pub fn encode_props(&self) -> Result<Vec<u8>, Error> {
        match self.id {
            FILTER_LZMA1 => {
                let o = self.lz.ok_or(Error::Options)?;
                if !o.valid(false) {
                    return Err(Error::Options);
                }
                let mut v = vec![o.props.to_byte()];
                v.extend_from_slice(&o.dict_size.to_le_bytes());
                Ok(v)
            }
            FILTER_LZMA2 => {
                let o = self.lz.ok_or(Error::Options)?;
                if !o.valid(true) {
                    return Err(Error::Options);
                }
                Ok(vec![enc::lzma2_dict_byte(o.dict_size)])
            }
            FILTER_DELTA => {
                if !(1..=256).contains(&self.dist) {
                    return Err(Error::Options);
                }
                Ok(vec![(self.dist - 1) as u8])
            }
            id if is_bcj(id) => {
                if self.start_offset == 0 {
                    Ok(Vec::new())
                } else {
                    Ok(self.start_offset.to_le_bytes().to_vec())
                }
            }
            _ => Err(Error::Options),
        }
    }

    /// Parse filter properties.  LZMA options other than dictionary size and lc/lp/pb come
    /// from the default preset.
    pub fn decode_props(id: u64, p: &[u8]) -> Result<FilterSpec, Error> {
        match id {
            FILTER_LZMA1 => {
                if p.len() != 5 {
                    return Err(Error::Options);
                }
                let props = Props::from_byte(p[0]).ok_or(Error::Options)?;
                let mut o = LzOptions::preset(6).unwrap();
                o.props = props;
                o.dict_size = u32::from_le_bytes([p[1], p[2], p[3], p[4]]);
                Ok(FilterSpec::lzma(id, o))
            }
            FILTER_LZMA2 => {
                if p.len() != 1 {
                    return Err(Error::Options);
                }
                let mut o = LzOptions::preset(6).unwrap();
                o.dict_size = dec::lzma2_dict_size(p[0]).ok_or(Error::Options)?;
                Ok(FilterSpec::lzma(id, o))
            }
            FILTER_DELTA => {
                if p.len() != 1 {
                    return Err(Error::Options);
                }
                let mut f = FilterSpec::simple(id);
                f.dist = p[0] as u32 + 1;
                Ok(f)
            }
            id if is_bcj(id) => {
                let mut f = FilterSpec::simple(id);
                match p.len() {
                    0 => {}
                    4 => f.start_offset = u32::from_le_bytes([p[0], p[1], p[2], p[3]]),
                    _ => return Err(Error::Options),
                }
                Ok(f)
            }
            _ => Err(Error::Options),
        }
    }
}

/// Validate a chain: 1..=4 filters, LZMA1/LZMA2 last and only there.
fn validate_chain(chain: &[FilterSpec], allow_lzma1: bool) -> Result<(), Error> {
    if chain.is_empty() || chain.len() > 4 {
        return Err(Error::Options);
    }
    for (i, f) in chain.iter().enumerate() {
        let last = i + 1 == chain.len();
        let is_lz = f.id == FILTER_LZMA1 || f.id == FILTER_LZMA2;
        if is_lz != last {
            return Err(Error::Options);
        }
        if f.id == FILTER_LZMA1 && !allow_lzma1 {
            return Err(Error::Options);
        }
        if f.id == FILTER_DELTA && !(1..=256).contains(&f.dist) {
            return Err(Error::Options);
        }
        if f.id == FILTER_RISCV {
            return Err(Error::Options);
        }
        if let Some(o) = &f.lz {
            if !o.valid(f.id == FILTER_LZMA2) {
                return Err(Error::Options);
            }
        }
    }
    Ok(())
}

fn make_filter(f: &FilterSpec, encode: bool) -> Box<dyn Filter> {
    if f.id == FILTER_DELTA {
        Box::new(Delta::new(f.dist as usize, encode))
    } else {
        Box::new(Bcj::new(f.id, f.start_offset, encode))
    }
}

// ------------------------------------------------------------------ integrity checks

enum Check {
    None,
    Crc32(u32),
    Crc64(u64),
    Sha256(Box<Sha256>),
    /// Unsupported type: the stored value is skipped.
    Skip(usize),
}

impl Check {
    fn new(id: u32) -> Check {
        match id {
            CHECK_NONE => Check::None,
            CHECK_CRC32 => Check::Crc32(0),
            CHECK_CRC64 => Check::Crc64(0),
            CHECK_SHA256 => Check::Sha256(Box::new(Sha256::new())),
            other => Check::Skip(check_size(other)),
        }
    }
    fn update(&mut self, d: &[u8]) {
        match self {
            Check::Crc32(c) => *c = checksum::crc32(*c, d),
            Check::Crc64(c) => *c = checksum::crc64(*c, d),
            Check::Sha256(h) => h.update(d),
            _ => {}
        }
    }
    fn finish(&self) -> Vec<u8> {
        match self {
            Check::None => Vec::new(),
            Check::Crc32(c) => c.to_le_bytes().to_vec(),
            Check::Crc64(c) => c.to_le_bytes().to_vec(),
            Check::Sha256(h) => h.digest(),
            Check::Skip(n) => vec![0; *n],
        }
    }
    fn verifies(&self) -> bool {
        !matches!(self, Check::Skip(_))
    }
}

// ------------------------------------------------------------------ varints

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Parse a multibyte integer: Some((value, length)), None if incomplete, Err if invalid.
fn get_varint(b: &[u8]) -> Result<Option<(u64, usize)>, Error> {
    let mut v = 0u64;
    for i in 0..9 {
        let Some(&c) = b.get(i) else { return Ok(None) };
        v |= ((c & 0x7F) as u64) << (7 * i);
        if c & 0x80 == 0 {
            if c == 0 && i > 0 {
                return Err(Error::Data);
            }
            return Ok(Some((v, i + 1)));
        }
    }
    Err(Error::Data)
}

// ------------------------------------------------------------------ decoding pipeline

enum Lz {
    L1(Lzma1Dec),
    L2(Lzma2Dec),
}

/// LZ decoder + trailing filters, producing final output into `out`.
struct Pipeline {
    lz: Lz,
    stages: Vec<Stage>,
    done: bool,
}

impl Pipeline {
    /// `chain` in encoding order.  `size`: known uncompressed size (LZMA1 only).
    fn new(chain: &[FilterSpec], size: Option<u64>) -> Pipeline {
        let last = chain.last().unwrap();
        let o = last.lz.unwrap();
        let lz = if last.id == FILTER_LZMA1 {
            Lz::L1(Lzma1Dec::new(o.props, o.dict_size, size))
        } else {
            Lz::L2(Lzma2Dec::new(o.dict_size))
        };
        let stages = chain[..chain.len() - 1].iter().rev().map(|f| Stage::new(make_filter(f, false))).collect();
        Pipeline { lz, stages, done: false }
    }

    fn mem_usage(chain: &[FilterSpec]) -> u64 {
        let o = chain.last().unwrap().lz.unwrap();
        let lit = (0x300u64 << (o.props.lc + o.props.lp)) * 2;
        o.dict_size as u64 + lit + 32 * 1024
    }

    fn win(&mut self) -> &mut Window {
        match &mut self.lz {
            Lz::L1(d) => &mut d.win,
            Lz::L2(d) => &mut d.win,
        }
    }

    /// Decode from `input` until `out` has grown by `want` bytes or input is exhausted.
    /// Returns consumed input bytes.
    fn run(&mut self, input: &[u8], want: usize, out: &mut Vec<u8>, check: &mut Check) -> Result<usize, Error> {
        let start = out.len();
        let mut consumed = 0;
        while !self.done && out.len() - start < want {
            let target = (want - (out.len() - start)).max(1);
            let (n, lz_done) = match &mut self.lz {
                Lz::L1(d) => {
                    let n = d.decode(&input[consumed..], target)?;
                    (n, d.done)
                }
                Lz::L2(d) => {
                    let n = d.decode(&input[consumed..], target)?;
                    (n, d.done)
                }
            };
            consumed += n;
            let w = self.win();
            let produced = w.pending() > 0;
            let mut data = w.take_all().to_vec();
            w.compact();
            for st in self.stages.iter_mut() {
                let mut next = Vec::with_capacity(data.len() + 16);
                st.push(&data, lz_done, &mut next);
                data = next;
            }
            check.update(&data);
            out.extend_from_slice(&data);
            if lz_done {
                self.done = true;
            } else if !produced && n == 0 {
                break;
            }
        }
        Ok(consumed)
    }
}

// ------------------------------------------------------------------ decoder

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DState {
    Detect,
    AloneHeader,
    StreamHeader,
    BlockStart,
    Block,
    BlockTail,
    Index,
    Footer,
    Raw,
    Done,
}

/// Streaming decoder for a single .xz / .lzma / raw stream.
pub struct Decoder {
    format: Format,
    state: DState,
    memlimit: u64,
    input: Vec<u8>,
    ipos: usize,
    out: Vec<u8>,
    opos: usize,
    pipe: Option<Pipeline>,
    raw_chain: Vec<FilterSpec>,
    check_id: Option<u32>,
    stream_flags: [u8; 2],
    check: Check,
    // current block
    block_hdr_size: usize,
    block_csize: Option<u64>,
    block_usize: Option<u64>,
    block_cdone: u64,
    block_udone: u64,
    // index bookkeeping: (unpadded size, uncompressed size)
    records: Vec<(u64, u64)>,
    index_size: u64,
    done: bool,
}

impl Decoder {
    pub fn new(format: Format, memlimit: Option<u64>, chain: Option<Vec<FilterSpec>>) -> Result<Decoder, Error> {
        let mut d = Decoder {
            format,
            state: match format {
                Format::Auto => DState::Detect,
                Format::Xz => DState::StreamHeader,
                Format::Alone => DState::AloneHeader,
                Format::Raw => DState::Raw,
            },
            memlimit: memlimit.unwrap_or(u64::MAX),
            input: Vec::new(),
            ipos: 0,
            out: Vec::new(),
            opos: 0,
            pipe: None,
            raw_chain: Vec::new(),
            check_id: if matches!(format, Format::Alone | Format::Raw) { Some(CHECK_NONE) } else { None },
            stream_flags: [0; 2],
            check: Check::None,
            block_hdr_size: 0,
            block_csize: None,
            block_usize: None,
            block_cdone: 0,
            block_udone: 0,
            records: Vec::new(),
            index_size: 0,
            done: false,
        };
        if format == Format::Raw {
            let chain = chain.ok_or(Error::Options)?;
            validate_chain(&chain, true)?;
            d.pipe = Some(Pipeline::new(&chain, None));
            d.raw_chain = chain;
        }
        Ok(d)
    }

    /// Check id of the stream once known (`CHECK_UNKNOWN` before).
    pub fn check(&self) -> u32 {
        self.check_id.unwrap_or(CHECK_UNKNOWN)
    }

    /// True once the end of the stream was reached and all output handed out.
    pub fn is_done(&self) -> bool {
        self.done && self.opos == self.out.len()
    }

    pub fn has_output(&self) -> bool {
        self.opos < self.out.len()
    }

    /// Input after the end of the stream (valid once done).
    pub fn unused(&self) -> &[u8] {
        if self.done { &self.input[self.ipos..] } else { &[] }
    }

    pub fn feed(&mut self, d: &[u8]) {
        if self.ipos > 0 && (self.ipos == self.input.len() || self.ipos >= (1 << 16)) {
            self.input.drain(..self.ipos);
            self.ipos = 0;
        }
        self.input.extend_from_slice(d);
    }

    /// Produce up to `max` bytes of output.
    pub fn read(&mut self, max: usize) -> Result<Vec<u8>, Error> {
        while !self.done && self.out.len() - self.opos < max {
            if !self.step(max - (self.out.len() - self.opos))? {
                break;
            }
        }
        let n = (self.out.len() - self.opos).min(max);
        let r = self.out[self.opos..self.opos + n].to_vec();
        self.opos += n;
        if self.opos == self.out.len() {
            self.out.clear();
            self.opos = 0;
            // (wheels2) the output limit was reached with nothing left over: go on
            // through what needs no output space (end markers, block check, index,
            // footer) like liblzma, so the call that returns the last bytes already
            // reports the end of the stream; stops at the first new output byte
            if n == max && max > 0 {
                while !self.done && self.out.is_empty() {
                    if !self.step(1)? {
                        break;
                    }
                }
            }
        }
        Ok(r)
    }

    fn avail(&self) -> &[u8] {
        &self.input[self.ipos..]
    }

    fn check_mem(&self, chain: &[FilterSpec]) -> Result<(), Error> {
        if Pipeline::mem_usage(chain) > self.memlimit {
            return Err(Error::MemLimit);
        }
        Ok(())
    }

    /// One state-machine step; false when more input is needed.
    fn step(&mut self, want: usize) -> Result<bool, Error> {
        match self.state {
            DState::Detect => {
                let Some(&b) = self.avail().first() else { return Ok(false) };
                self.state = if b == 0xFD { DState::StreamHeader } else { DState::AloneHeader };
                Ok(true)
            }
            DState::AloneHeader => {
                let a = self.avail();
                if a.len() < 13 {
                    // Reject obviously bad property bytes early.
                    if let Some(&b) = a.first() {
                        if b >= 225 {
                            return Err(if self.format == Format::Auto { Error::Format } else { Error::Format });
                        }
                    }
                    return Ok(false);
                }
                let picky = self.format == Format::Auto;
                let props = Props::from_byte(a[0]).ok_or(Error::Format)?;
                let dict = u32::from_le_bytes([a[1], a[2], a[3], a[4]]);
                let size = u64::from_le_bytes(a[5..13].try_into().unwrap());
                if picky {
                    let ok_dict = dict == u32::MAX || {
                        let hb = 31 - dict.max(1).leading_zeros();
                        let p2 = 1u32 << hb;
                        dict == p2 || (hb > 0 && dict == p2 + (p2 >> 1))
                    };
                    if !ok_dict || (size != u64::MAX && size >= (1u64 << 38)) {
                        return Err(Error::Format);
                    }
                }
                let mut o = LzOptions::preset(6).unwrap();
                o.props = props;
                o.dict_size = dict.max(4096);
                let chain = [FilterSpec::lzma(FILTER_LZMA1, o)];
                self.check_mem(&chain)?;
                self.ipos += 13;
                self.check_id = Some(CHECK_NONE);
                self.pipe = Some(Pipeline::new(&chain, if size == u64::MAX { None } else { Some(size) }));
                self.raw_chain = chain.to_vec();
                self.state = DState::Raw;
                Ok(true)
            }
            DState::Raw => {
                let pipe = self.pipe.as_mut().unwrap();
                let before = self.out.len();
                let n = pipe.run(&self.input[self.ipos..], want, &mut self.out, &mut self.check)?;
                self.ipos += n;
                if pipe.done {
                    self.state = DState::Done;
                    self.done = true;
                    return Ok(true);
                }
                Ok(n > 0 || self.out.len() > before)
            }
            DState::StreamHeader => {
                let a = self.avail();
                const MAGIC: [u8; 6] = [0xFD, b'7', b'z', b'X', b'Z', 0];
                let k = a.len().min(6);
                if a[..k] != MAGIC[..k] {
                    return Err(Error::Format);
                }
                if a.len() < 12 {
                    return Ok(false);
                }
                let crc = u32::from_le_bytes([a[8], a[9], a[10], a[11]]);
                if checksum::crc32(0, &a[6..8]) != crc {
                    return Err(Error::Data);
                }
                if a[6] != 0 || a[7] & 0xF0 != 0 {
                    return Err(Error::Options);
                }
                let fl = [a[6], a[7]];
                self.stream_flags = fl;
                self.check_id = Some((fl[1] & 0x0F) as u32);

                self.ipos += 12;
                self.state = DState::BlockStart;
                Ok(true)
            }
            DState::BlockStart => {
                let a = self.avail();
                let Some(&b0) = a.first() else { return Ok(false) };
                if b0 == 0 {
                    self.state = DState::Index;
                    return Ok(true);
                }
                let hsize = (b0 as usize + 1) * 4;
                if a.len() < hsize {
                    return Ok(false);
                }
                let h = &a[..hsize];
                let crc = u32::from_le_bytes(h[hsize - 4..].try_into().unwrap());
                if checksum::crc32(0, &h[..hsize - 4]) != crc {
                    return Err(Error::Data);
                }
                let flags = h[1];
                if flags & 0x3C != 0 {
                    return Err(Error::Options);
                }
                let nfilt = (flags & 3) as usize + 1;
                let mut p = 2;
                let body = &h[..hsize - 4];
                let field = |p: &mut usize| -> Result<u64, Error> {
                    match get_varint(&body[*p..])? {
                        Some((v, n)) => {
                            *p += n;
                            Ok(v)
                        }
                        None => Err(Error::Data),
                    }
                };
                let csize = if flags & 0x40 != 0 {
                    let v = field(&mut p)?;
                    if v == 0 {
                        return Err(Error::Data);
                    }
                    Some(v)
                } else {
                    None
                };
                let usize_ = if flags & 0x80 != 0 { Some(field(&mut p)?) } else { None };
                let mut chain = Vec::new();
                for _ in 0..nfilt {
                    let id = field(&mut p)?;
                    let psz = field(&mut p)? as usize;
                    if p + psz > body.len() {
                        return Err(Error::Data);
                    }
                    if !FilterSpec::known(id) || id == FILTER_LZMA1 {
                        return Err(Error::Options);
                    }
                    chain.push(FilterSpec::decode_props(id, &body[p..p + psz])?);
                    p += psz;
                }
                if body[p..].iter().any(|&x| x != 0) {
                    return Err(Error::Options);
                }
                validate_chain(&chain, false)?;
                self.check_mem(&chain)?;
                self.pipe = Some(Pipeline::new(&chain, None));
                self.check = Check::new(self.check_id.unwrap());
                self.block_hdr_size = hsize;
                self.block_csize = csize;
                self.block_usize = usize_;
                self.block_cdone = 0;
                self.block_udone = 0;
                self.ipos += hsize;
                self.state = DState::Block;
                Ok(true)
            }
            DState::Block => {
                let pipe = self.pipe.as_mut().unwrap();
                let before = self.out.len();
                let mut inp = &self.input[self.ipos..];
                if let Some(c) = self.block_csize {
                    let left = (c - self.block_cdone).min(inp.len() as u64) as usize;
                    inp = &inp[..left];
                }
                let n = pipe.run(inp, want, &mut self.out, &mut self.check)?;
                self.ipos += n;
                self.block_cdone += n as u64;
                self.block_udone += (self.out.len() - before) as u64;
                if self.block_csize.is_some_and(|c| self.block_cdone > c)
                    || self.block_usize.is_some_and(|u| self.block_udone > u)
                {
                    return Err(Error::Data);
                }
                if pipe.done {
                    if self.block_csize.is_some_and(|c| c != self.block_cdone)
                        || self.block_usize.is_some_and(|u| u != self.block_udone)
                    {
                        return Err(Error::Data);
                    }
                    self.state = DState::BlockTail;
                    return Ok(true);
                }
                Ok(n > 0 || self.out.len() > before)
            }
            DState::BlockTail => {
                let pad = ((4 - (self.block_cdone % 4)) % 4) as usize;
                let cs = check_size(self.check_id.unwrap());
                let a = self.avail();
                if a.len() < pad + cs {
                    return Ok(false);
                }
                if a[..pad].iter().any(|&x| x != 0) {
                    return Err(Error::Data);
                }
                if self.check.verifies() && a[pad..pad + cs] != self.check.finish()[..] {
                    return Err(Error::Data);
                }
                let unpadded = self.block_hdr_size as u64 + self.block_cdone + cs as u64;
                self.records.push((unpadded, self.block_udone));
                self.ipos += pad + cs;
                self.pipe = None;
                self.state = DState::BlockStart;
                Ok(true)
            }
            DState::Index => {
                // Parse the whole index once it is buffered (its size is not known up front,
                // so parse incrementally from the start each time).
                let a = self.avail();
                let mut p = 1;
                let Some((count, n)) = get_varint(&a[p..])? else { return Ok(false) };
                p += n;
                if count != self.records.len() as u64 {
                    return Err(Error::Data);
                }
                for i in 0..count as usize {
                    let Some((unp, n)) = get_varint(&a[p..])? else { return Ok(false) };
                    p += n;
                    let Some((unc, n)) = get_varint(&a[p..])? else { return Ok(false) };
                    p += n;
                    if (unp, unc) != self.records[i] {
                        return Err(Error::Data);
                    }
                }
                let pad = (4 - p % 4) % 4;
                if a.len() < p + pad + 4 {
                    return Ok(false);
                }
                if a[p..p + pad].iter().any(|&x| x != 0) {
                    return Err(Error::Data);
                }
                let crc = u32::from_le_bytes(a[p + pad..p + pad + 4].try_into().unwrap());
                if checksum::crc32(0, &a[..p + pad]) != crc {
                    return Err(Error::Data);
                }
                self.index_size = (p + pad + 4) as u64;
                self.ipos += p + pad + 4;
                self.state = DState::Footer;
                Ok(true)
            }
            DState::Footer => {
                let a = self.avail();
                if a.len() < 12 {
                    return Ok(false);
                }
                if &a[10..12] != b"YZ" {
                    return Err(Error::Data);
                }
                let crc = u32::from_le_bytes(a[0..4].try_into().unwrap());
                if checksum::crc32(0, &a[4..10]) != crc {
                    return Err(Error::Data);
                }
                let back = (u32::from_le_bytes(a[4..8].try_into().unwrap()) as u64 + 1) * 4;
                if back != self.index_size || a[8..10] != self.stream_flags {
                    return Err(Error::Data);
                }
                self.ipos += 12;
                self.state = DState::Done;
                self.done = true;
                Ok(true)
            }
            DState::Done => Ok(false),
        }
    }
}

// ------------------------------------------------------------------ encoder

enum LzEnc {
    L1(enc::Lzma1Encoder),
    L2(enc::Lzma2Encoder),
}

/// Forward filters followed by the LZ encoder.
struct EncPipeline {
    stages: Vec<Stage>,
    lz: LzEnc,
}

impl EncPipeline {
    fn new(chain: &[FilterSpec]) -> EncPipeline {
        let last = chain.last().unwrap();
        let o = last.lz.unwrap();
        let lz = if last.id == FILTER_LZMA1 { LzEnc::L1(enc::Lzma1Encoder::new(o)) } else { LzEnc::L2(enc::Lzma2Encoder::new(o)) };
        let stages = chain[..chain.len() - 1].iter().map(|f| Stage::new(make_filter(f, true))).collect();
        EncPipeline { stages, lz }
    }

    fn push(&mut self, d: &[u8], end: bool, out: &mut Vec<u8>, marker: bool) {
        let filtered;
        let data: &[u8] = if self.stages.is_empty() {
            d
        } else {
            let mut cur = d.to_vec();
            for st in self.stages.iter_mut() {
                let mut next = Vec::with_capacity(cur.len() + 16);
                st.push(&cur, end, &mut next);
                cur = next;
            }
            filtered = cur;
            &filtered
        };
        match &mut self.lz {
            LzEnc::L1(e) => e.compress(data, out),
            LzEnc::L2(e) => e.compress(data, out),
        }
        if end {
            match &mut self.lz {
                LzEnc::L1(e) => e.finish(out, marker),
                LzEnc::L2(e) => e.finish(out),
            }
        }
    }
}

/// Streaming encoder producing .xz (one block), .lzma or raw output.
pub struct Encoder {
    format: Format,
    chain: Vec<FilterSpec>,
    check_id: u32,
    pipe: Option<EncPipeline>,
    header_done: bool,
    block_open: bool,
    block_hdr_len: u64,
    block_csize: u64,
    block_usize: u64,
    check: Check,
    finished: bool,
}

impl Encoder {
    pub fn new(format: Format, check: u32, chain: Vec<FilterSpec>) -> Result<Encoder, Error> {
        match format {
            Format::Xz => {
                validate_chain(&chain, false)?;
                if check > CHECK_ID_MAX {
                    return Err(Error::Options);
                }
                if !check_supported(check) {
                    return Err(Error::UnsupportedCheck);
                }
            }
            Format::Alone => {
                if chain.len() != 1 || chain[0].id != FILTER_LZMA1 {
                    return Err(Error::Options);
                }
                validate_chain(&chain, true)?;
            }
            Format::Raw => validate_chain(&chain, true)?,
            Format::Auto => return Err(Error::Options),
        }
        let pipe = if format == Format::Xz { None } else { Some(EncPipeline::new(&chain)) };
        Ok(Encoder {
            format,
            chain,
            check_id: check,
            pipe,
            header_done: false,
            block_open: false,
            block_hdr_len: 0,
            block_csize: 0,
            block_usize: 0,
            check: Check::new(check),
            finished: false,
        })
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    fn header(&mut self, out: &mut Vec<u8>) {
        if self.header_done {
            return;
        }
        self.header_done = true;
        match self.format {
            Format::Xz => {
                out.extend_from_slice(&[0xFD, b'7', b'z', b'X', b'Z', 0]);
                let flags = [0u8, self.check_id as u8];
                out.extend_from_slice(&flags);
                out.extend_from_slice(&checksum::crc32(0, &flags).to_le_bytes());
            }
            Format::Alone => {
                let o = self.chain[0].lz.unwrap();
                out.push(o.props.to_byte());
                // The header stores the dictionary size rounded up to 2^n or 2^n + 2^(n-1).
                let d = o.dict_size as u64;
                let mut r = u32::MAX as u64;
                for n in 12..32u32 {
                    let a = 1u64 << n;
                    if a >= d {
                        r = a;
                        break;
                    }
                    if a + a / 2 >= d {
                        r = a + a / 2;
                        break;
                    }
                }
                out.extend_from_slice(&(r as u32).to_le_bytes());
                out.extend_from_slice(&u64::MAX.to_le_bytes());
            }
            _ => {}
        }
    }

    fn open_block(&mut self, out: &mut Vec<u8>) {
        if self.block_open {
            return;
        }
        self.block_open = true;
        let mut h = vec![0u8, (self.chain.len() - 1) as u8];
        for f in &self.chain {
            put_varint(&mut h, f.id);
            let p = f.encode_props().unwrap_or_default();
            put_varint(&mut h, p.len() as u64);
            h.extend_from_slice(&p);
        }
        while h.len() % 4 != 0 {
            h.push(0);
        }
        h[0] = (h.len() / 4) as u8; // (size including the CRC) / 4 - 1
        let crc = checksum::crc32(0, &h);
        h.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&h);
        self.block_hdr_len = h.len() as u64;
        self.pipe = Some(EncPipeline::new(&self.chain));
    }

    pub fn compress(&mut self, d: &[u8], out: &mut Vec<u8>) {
        self.header(out);
        if self.format != Format::Xz {
            self.pipe.as_mut().unwrap().push(d, false, out, false);
            return;
        }
        if d.is_empty() {
            return;
        }
        self.open_block(out);
        self.check.update(d);
        self.block_usize += d.len() as u64;
        let start = out.len();
        self.pipe.as_mut().unwrap().push(d, false, out, false);
        self.block_csize += (out.len() - start) as u64;
    }

    pub fn finish(&mut self, out: &mut Vec<u8>) {
        if self.finished {
            return;
        }
        self.header(out);
        self.finished = true;
        match self.format {
            Format::Xz => {
                let mut records = Vec::new();
                if self.block_open {
                    let start = out.len();
                    self.pipe.as_mut().unwrap().push(&[], true, out, false);
                    self.block_csize += (out.len() - start) as u64;
                    let c = self.check.finish();
                    records.push((self.block_hdr_len + self.block_csize + c.len() as u64, self.block_usize));
                    while self.block_csize % 4 != 0 {
                        out.push(0);
                        self.block_csize += 1;
                    }
                    out.extend_from_slice(&c);
                }
                let mut idx = vec![0u8];
                put_varint(&mut idx, records.len() as u64);
                for &(u, n) in &records {
                    put_varint(&mut idx, u);
                    put_varint(&mut idx, n);
                }
                while idx.len() % 4 != 0 {
                    idx.push(0);
                }
                let crc = checksum::crc32(0, &idx);
                idx.extend_from_slice(&crc.to_le_bytes());
                out.extend_from_slice(&idx);
                let mut f = Vec::with_capacity(6);
                f.extend_from_slice(&((idx.len() / 4 - 1) as u32).to_le_bytes());
                f.extend_from_slice(&[0u8, self.check_id as u8]);
                out.extend_from_slice(&checksum::crc32(0, &f).to_le_bytes());
                out.extend_from_slice(&f);
                out.extend_from_slice(b"YZ");
            }
            Format::Alone => self.pipe.as_mut().unwrap().push(&[], true, out, true),
            Format::Raw => {
                let marker = self.chain.last().unwrap().id == FILTER_LZMA1;
                self.pipe.as_mut().unwrap().push(&[], true, out, marker)
            }
            Format::Auto => {}
        }
    }
}

/// One-shot helpers (used by tests).
pub fn compress_xz(data: &[u8], preset: u32, check: u32) -> Vec<u8> {
    let o = LzOptions::preset(preset).unwrap();
    let mut e = Encoder::new(Format::Xz, check, vec![FilterSpec::lzma(FILTER_LZMA2, o)]).unwrap();
    let mut out = Vec::new();
    e.compress(data, &mut out);
    e.finish(&mut out);
    out
}

pub fn decompress(data: &[u8], format: Format, chain: Option<Vec<FilterSpec>>) -> Result<(Vec<u8>, usize), Error> {
    let mut d = Decoder::new(format, None, chain)?;
    d.feed(data);
    let out = d.read(usize::MAX)?;
    if !d.is_done() {
        return Err(Error::Data);
    }
    Ok((out, d.unused().len()))
}

#[cfg(test)]
mod tests;
