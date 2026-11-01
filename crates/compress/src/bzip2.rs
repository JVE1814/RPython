//! bzip2 format: encoder (RLE1, BWT by prefix doubling, MTF/RUNA-RUNB, multi-table Huffman)
//! and an incremental decoder (resumable per symbol).

use crate::huffman::{canonical_codes, code_lengths};

const BLOCK_MAGIC: u64 = 0x3141_5926_5359;
const END_MAGIC: u64 = 0x1772_4538_5090;
const MAX_GROUPS: usize = 6;
const GROUP_SIZE: usize = 50;
const MAX_CODE_LEN: u32 = 20;

const fn make_crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u32) << 24;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x8000_0000 != 0 { (c << 1) ^ 0x04C1_1DB7 } else { c << 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}
static CRC: [u32; 256] = make_crc_table();

#[inline]
fn crc_update(crc: u32, data: &[u8]) -> u32 {
    let mut c = crc;
    for &b in data {
        c = (c << 8) ^ CRC[((c >> 24) as u8 ^ b) as usize];
    }
    c
}

// ====================================================================== encoder

struct BitWriter {
    out: Vec<u8>,
    buf: u64,
    n: u32,
}

impl BitWriter {
    fn put(&mut self, v: u32, bits: u32) {
        self.buf = (self.buf << bits) | (v as u64 & ((1u64 << bits) - 1));
        self.n += bits;
        while self.n >= 8 {
            self.n -= 8;
            self.out.push((self.buf >> self.n) as u8);
        }
    }
    fn put48(&mut self, v: u64) {
        self.put((v >> 24) as u32, 24);
        self.put((v & 0xff_ffff) as u32, 24);
    }
    fn flush(&mut self) {
        if self.n > 0 {
            self.out.push((self.buf << (8 - self.n)) as u8);
            self.n = 0;
        }
    }
}

/// Streaming bzip2 compressor.
#[derive(Clone)]
pub struct Encoder {
    level: u32,
    block: Vec<u8>,
    max_block: usize,
    block_crc: u32,
    combined_crc: u32,
    // pending RLE1 run
    run_byte: i32,
    run_len: u32,
    out: Vec<u8>,
    bitbuf: u64,
    bitn: u32,
    header_done: bool,
}

impl Encoder {
    pub fn new(level: u32) -> Encoder {
        let level = level.clamp(1, 9);
        Encoder {
            level,
            block: Vec::new(),
            max_block: 100_000 * level as usize - 19,
            block_crc: 0xffff_ffff,
            combined_crc: 0,
            run_byte: -1,
            run_len: 0,
            out: Vec::new(),
            bitbuf: 0,
            bitn: 0,
            header_done: false,
        }
    }

    fn writer(&mut self) -> BitWriter {
        BitWriter { out: std::mem::take(&mut self.out), buf: self.bitbuf, n: self.bitn }
    }
    fn restore(&mut self, w: BitWriter) {
        self.out = w.out;
        self.bitbuf = w.buf;
        self.bitn = w.n;
    }

    fn flush_run(&mut self) {
        if self.run_byte < 0 {
            return;
        }
        let b = self.run_byte as u8;
        let n = self.run_len;
        for _ in 0..n.min(4) {
            self.block.push(b);
        }
        if n >= 4 {
            self.block.push((n - 4) as u8);
        }
        self.run_byte = -1;
        self.run_len = 0;
    }

    pub fn compress(&mut self, data: &[u8]) {
        if !self.header_done {
            self.header_done = true;
            self.out.extend_from_slice(b"BZh");
            self.out.push(b'0' + self.level as u8);
        }
        for &b in data {
            if self.run_byte == b as i32 && self.run_len < 255 {
                self.run_len += 1;
            } else {
                // A run is written as at most 5 bytes; make sure it fits.
                if self.block.len() + 5 > self.max_block {
                    self.flush_run();
                    self.write_block();
                } else {
                    self.flush_run();
                }
                self.run_byte = b as i32;
                self.run_len = 1;
            }
            self.block_crc = (self.block_crc << 8) ^ CRC[((self.block_crc >> 24) as u8 ^ b) as usize];
        }
    }

    /// Finish the stream; returns everything produced since the last take.
    pub fn finish(&mut self) {
        if !self.header_done {
            self.compress(&[]);
        }
        self.flush_run();
        if !self.block.is_empty() {
            self.write_block();
        }
        let mut w = self.writer();
        w.put48(END_MAGIC);
        w.put(self.combined_crc, 32);
        w.flush();
        self.restore(w);
    }

    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    fn write_block(&mut self) {
        let block = std::mem::take(&mut self.block);
        let crc = !self.block_crc;
        self.block_crc = 0xffff_ffff;
        self.combined_crc = self.combined_crc.rotate_left(1) ^ crc;
        let mut w = self.writer();
        encode_block(&mut w, &block, crc);
        self.restore(w);
    }
}

/// BWT via prefix doubling over cyclic rotations; returns (last column, origPtr).
fn bwt(s: &[u8]) -> (Vec<u8>, u32) {
    let n = s.len();
    let mut sa: Vec<u32> = (0..n as u32).collect();
    let mut rank: Vec<u32> = s.iter().map(|&b| b as u32).collect();
    let mut tmp = vec![0u32; n];
    let mut k = 1usize;
    // counting sort by first byte to start
    sa.sort_unstable_by_key(|&i| rank[i as usize]);
    loop {
        let key = |i: u32| -> (u32, u32) { (rank[i as usize], rank[(i as usize + k) % n]) };
        sa.sort_unstable_by_key(|&i| key(i));
        tmp[sa[0] as usize] = 0;
        let mut classes = 1;
        for j in 1..n {
            if key(sa[j]) != key(sa[j - 1]) {
                classes += 1;
            }
            tmp[sa[j] as usize] = classes - 1;
        }
        std::mem::swap(&mut rank, &mut tmp);
        if classes as usize == n || k >= n {
            break;
        }
        k *= 2;
    }
    let mut last = Vec::with_capacity(n);
    let mut orig = 0;
    for (j, &i) in sa.iter().enumerate() {
        if i == 0 {
            orig = j as u32;
        }
        last.push(s[(i as usize + n - 1) % n]);
    }
    (last, orig)
}

fn encode_block(w: &mut BitWriter, block: &[u8], crc: u32) {
    let (last, orig) = bwt(block);
    // symbols in use
    let mut used = [false; 256];
    for &b in &last {
        used[b as usize] = true;
    }
    let mut seq = [0u8; 256];
    let mut n_in_use = 0usize;
    let mut map = [0u8; 256];
    for i in 0..256 {
        if used[i] {
            map[i] = n_in_use as u8;
            seq[n_in_use] = i as u8;
            n_in_use += 1;
        }
    }
    let eob = n_in_use + 1;
    let alpha = n_in_use + 2;
    // MTF + zero-run coding
    let mut mtf: Vec<u8> = (0..n_in_use).map(|x| x as u8).collect();
    let mut syms: Vec<u16> = Vec::with_capacity(last.len() + 1);
    let mut zrun = 0usize;
    let emit_run = |syms: &mut Vec<u16>, mut r: usize| {
        // bijective base-2 with digits RUNA=1, RUNB=2
        while r > 0 {
            r -= 1;
            syms.push((r & 1) as u16);
            r >>= 1;
        }
    };
    for &b in &last {
        let v = map[b as usize];
        let pos = mtf.iter().position(|&x| x == v).unwrap();
        if pos == 0 {
            zrun += 1;
            continue;
        }
        if zrun > 0 {
            emit_run(&mut syms, zrun);
            zrun = 0;
        }
        mtf.copy_within(0..pos, 1);
        mtf[0] = v;
        syms.push(pos as u16 + 1);
    }
    if zrun > 0 {
        emit_run(&mut syms, zrun);
    }
    syms.push(eob as u16);

    // Huffman tables
    let n_groups = match syms.len() {
        0..200 => 2,
        200..600 => 3,
        600..1200 => 4,
        1200..2400 => 5,
        _ => 6,
    };
    let n_sel = syms.len().div_ceil(GROUP_SIZE);
    let mut freq_all = vec![0u32; alpha];
    for &s in &syms {
        freq_all[s as usize] += 1;
    }
    // initial tables: split the alphabet into ranges of roughly equal frequency
    let mut lens = vec![vec![15u8; alpha]; n_groups];
    {
        let total: u32 = freq_all.iter().sum();
        let mut lo = 0usize;
        let mut remaining = total;
        for g in 0..n_groups {
            let target = remaining / (n_groups - g) as u32;
            let mut acc = 0u32;
            let mut hi = lo;
            while hi < alpha && (acc < target || hi == lo) {
                acc += freq_all[hi];
                hi += 1;
            }
            for s in 0..alpha {
                lens[g][s] = if s >= lo && s < hi { 0 } else { 15 };
            }
            remaining = remaining.saturating_sub(acc);
            lo = hi;
        }
    }
    let mut selectors = vec![0u8; n_sel];
    for _iter in 0..4 {
        let mut freqs = vec![vec![0u32; alpha]; n_groups];
        for (gi, chunk) in syms.chunks(GROUP_SIZE).enumerate() {
            let mut best = 0;
            let mut best_cost = u32::MAX;
            for g in 0..n_groups {
                let c: u32 = chunk.iter().map(|&s| lens[g][s as usize] as u32).sum();
                if c < best_cost {
                    best_cost = c;
                    best = g;
                }
            }
            selectors[gi] = best as u8;
            for &s in chunk {
                freqs[best][s as usize] += 1;
            }
        }
        for g in 0..n_groups {
            let mut f = freqs[g].clone();
            for x in f.iter_mut() {
                *x = *x * 2 + 1; // every symbol must have a code
            }
            lens[g] = code_lengths(&f, 17, 2);
        }
    }

    w.put48(BLOCK_MAGIC);
    w.put(crc, 32);
    w.put(0, 1);
    w.put(orig, 24);
    let mut in16 = 0u32;
    for i in 0..16 {
        if used[i * 16..i * 16 + 16].iter().any(|&u| u) {
            in16 |= 1 << (15 - i);
        }
    }
    w.put(in16, 16);
    for i in 0..16 {
        if in16 & (1 << (15 - i)) != 0 {
            let mut bits = 0u32;
            for j in 0..16 {
                if used[i * 16 + j] {
                    bits |= 1 << (15 - j);
                }
            }
            w.put(bits, 16);
        }
    }
    w.put(n_groups as u32, 3);
    w.put(n_sel as u32, 15);
    let mut smtf: Vec<u8> = (0..n_groups as u8).collect();
    for &s in &selectors {
        let pos = smtf.iter().position(|&x| x == s).unwrap();
        smtf.copy_within(0..pos, 1);
        smtf[0] = s;
        for _ in 0..pos {
            w.put(1, 1);
        }
        w.put(0, 1);
    }
    for g in 0..n_groups {
        let mut cur = lens[g][0] as i32;
        w.put(cur as u32, 5);
        for s in 0..alpha {
            let l = lens[g][s] as i32;
            while cur < l {
                w.put(2, 2);
                cur += 1;
            }
            while cur > l {
                w.put(3, 2);
                cur -= 1;
            }
            w.put(0, 1);
        }
    }
    let codes: Vec<Vec<u32>> = lens.iter().map(|l| canonical_codes(l)).collect();
    for (gi, chunk) in syms.chunks(GROUP_SIZE).enumerate() {
        let g = selectors[gi] as usize;
        for &s in chunk {
            w.put(codes[g][s as usize], lens[g][s as usize] as u32);
        }
    }
    let _ = seq;
}

/// One-shot compression.
pub fn compress(data: &[u8], level: u32) -> Vec<u8> {
    let mut e = Encoder::new(level);
    e.compress(data);
    e.finish();
    e.take_output()
}

// ====================================================================== decoder

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Invalid data ("Invalid data stream").
    Data,
}

#[derive(Clone)]
struct Table {
    // canonical decoding: for each length, limit (max code) and base index into perm
    limit: [i32; 22],
    base: [i32; 22],
    perm: Vec<u16>,
    min_len: u32,
    max_len: u32,
}

impl Table {
    fn new(lens: &[u8]) -> Result<Table, Error> {
        let min_len = *lens.iter().min().unwrap() as u32;
        let max_len = *lens.iter().max().unwrap() as u32;
        let mut perm = Vec::with_capacity(lens.len());
        for l in min_len..=max_len {
            for (s, &x) in lens.iter().enumerate() {
                if x as u32 == l {
                    perm.push(s as u16);
                }
            }
        }
        let mut limit = [-1i32; 22];
        let mut base = [0i32; 22];
        let mut code = 0i32;
        let mut idx = 0i32;
        for l in min_len..=max_len {
            let cnt = lens.iter().filter(|&&x| x as u32 == l).count() as i32;
            base[l as usize] = idx - code;
            code += cnt;
            idx += cnt;
            limit[l as usize] = code - 1;
            code <<= 1;
        }
        if min_len == 0 || max_len > MAX_CODE_LEN {
            return Err(Error::Data);
        }
        Ok(Table { limit, base, perm, min_len, max_len })
    }
}

#[derive(Clone)]
struct BlockState {
    crc: u32,
    orig: u32,
    seq: Vec<u8>,
    tables: Vec<Table>,
    selectors: Vec<u8>,
    n_sym: usize,      // symbols decoded so far (for group switching)
    mtf: Vec<u8>,
    run: u32,
    run_weight: u32,
    tt: Vec<u8>,
    eob: u16,
}

#[derive(Clone)]
enum St {
    StreamHeader,
    BlockStart,
    Symbols(Box<BlockState>),
    Done,
}

/// Incremental bzip2 decoder for one stream.
#[derive(Clone)]
pub struct Decoder {
    input: Vec<u8>,
    bitpos: usize,
    st: St,
    max_block: usize,
    combined: u32,
    out: Vec<u8>,
    out_pos: usize,
}

impl Default for Decoder {
    fn default() -> Self {
        Decoder::new()
    }
}

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    #[inline]
    fn get(&mut self, n: u32) -> Option<u32> {
        if self.pos + n as usize > self.d.len() * 8 {
            return None;
        }
        let mut v = 0u32;
        for _ in 0..n {
            let b = (self.d[self.pos >> 3] >> (7 - (self.pos & 7))) & 1;
            v = (v << 1) | b as u32;
            self.pos += 1;
        }
        Some(v)
    }
    fn get48(&mut self) -> Option<u64> {
        let a = self.get(24)? as u64;
        let b = self.get(24)? as u64;
        Some((a << 24) | b)
    }
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder { input: Vec::new(), bitpos: 0, st: St::StreamHeader, max_block: 0, combined: 0, out: Vec::new(), out_pos: 0 }
    }

    pub fn is_done(&self) -> bool {
        matches!(self.st, St::Done)
    }

    /// Bytes of input after the end of the stream (valid once done).
    pub fn unused(&self) -> &[u8] {
        let byte = self.bitpos.div_ceil(8);
        &self.input[byte.min(self.input.len())..]
    }

    /// Whether the decoder holds decoded output not yet returned.
    pub fn has_output(&self) -> bool {
        self.out_pos < self.out.len()
    }

    /// Whether unprocessed input is buffered.
    pub fn has_input(&self) -> bool {
        !self.is_done() && self.bitpos < self.input.len() * 8
    }

    pub fn feed(&mut self, data: &[u8]) {
        // Drop consumed whole bytes first.
        let drop = self.bitpos / 8;
        if drop > 0 && !self.is_done() {
            self.input.drain(..drop);
            self.bitpos -= drop * 8;
        }
        self.input.extend_from_slice(data);
    }

    /// Produce up to `max` bytes of output (decoding more input as needed).
    pub fn read(&mut self, max: usize) -> Result<Vec<u8>, Error> {
        let mut res = Vec::new();
        loop {
            if self.out_pos < self.out.len() {
                let n = (self.out.len() - self.out_pos).min(max - res.len());
                res.extend_from_slice(&self.out[self.out_pos..self.out_pos + n]);
                self.out_pos += n;
                if self.out_pos == self.out.len() {
                    self.out.clear();
                    self.out_pos = 0;
                }
            }
            if res.len() >= max || self.is_done() {
                return Ok(res);
            }
            if !self.step()? {
                return Ok(res);
            }
        }
    }

    /// Advance the state machine; Ok(false) when more input is needed.
    fn step(&mut self) -> Result<bool, Error> {
        let st = std::mem::replace(&mut self.st, St::Done);
        let input = std::mem::take(&mut self.input);
        let r = self.step_inner(st, &input);
        self.input = input;
        r
    }

    fn step_inner(&mut self, st: St, input: &[u8]) -> Result<bool, Error> {
        let mut b = Bits { d: input, pos: self.bitpos };
        match st {
            St::StreamHeader => {
                if input.len() * 8 < self.bitpos + 32 {
                    // Validate what we have early.
                    let have = &input[self.bitpos / 8..];
                    if !b"BZh".starts_with(&have[..have.len().min(3)]) {
                        return Err(Error::Data);
                    }
                    self.st = St::StreamHeader;
                    return Ok(false);
                }
                let h = &input[self.bitpos / 8..self.bitpos / 8 + 4];
                if &h[..3] != b"BZh" || !(b'1'..=b'9').contains(&h[3]) {
                    return Err(Error::Data);
                }
                self.max_block = (h[3] - b'0') as usize * 100_000;
                self.bitpos += 32;
                self.st = St::BlockStart;
                Ok(true)
            }
            St::BlockStart => match self.block_header(&mut b)? {
                None => {
                    self.st = St::BlockStart;
                    Ok(false)
                }
                Some(None) => {
                    self.bitpos = b.pos;
                    self.st = St::Done;
                    Ok(true)
                }
                Some(Some(bs)) => {
                    self.bitpos = b.pos;
                    self.st = St::Symbols(Box::new(bs));
                    Ok(true)
                }
            },
            St::Symbols(mut bs) => {
                let done = self.symbols(&mut b, &mut bs)?;
                if !done {
                    self.st = St::Symbols(bs);
                    return Ok(false);
                }
                self.finish_block(*bs)?;
                self.st = St::BlockStart;
                Ok(true)
            }
            St::Done => {
                self.st = St::Done;
                Ok(false)
            }
        }
    }

    /// Ok(None): need input.  Ok(Some(None)): end of stream.  Ok(Some(Some)): block.
    fn block_header(&mut self, b: &mut Bits) -> Result<Option<Option<BlockState>>, Error> {
        macro_rules! g {
            ($n:expr) => {
                match b.get($n) {
                    Some(v) => v,
                    None => return Ok(None),
                }
            };
        }
        let magic = match b.get48() {
            Some(m) => m,
            None => return Ok(None),
        };
        if magic == END_MAGIC {
            let crc = g!(32);
            if crc != self.combined {
                return Err(Error::Data);
            }
            return Ok(Some(None));
        }
        if magic != BLOCK_MAGIC {
            return Err(Error::Data);
        }
        let crc = g!(32);
        if g!(1) != 0 {
            return Err(Error::Data); // randomised blocks are obsolete
        }
        let orig = g!(24);
        let in16 = g!(16);
        let mut seq = Vec::new();
        for i in 0..16 {
            if in16 & (1 << (15 - i)) != 0 {
                let bits = g!(16);
                for j in 0..16 {
                    if bits & (1 << (15 - j)) != 0 {
                        seq.push((i * 16 + j) as u8);
                    }
                }
            }
        }
        if seq.is_empty() {
            return Err(Error::Data);
        }
        let alpha = seq.len() + 2;
        let n_groups = g!(3) as usize;
        if !(2..=MAX_GROUPS).contains(&n_groups) {
            return Err(Error::Data);
        }
        let n_sel = g!(15) as usize;
        if n_sel == 0 {
            return Err(Error::Data);
        }
        let mut smtf: Vec<u8> = (0..n_groups as u8).collect();
        let mut selectors = Vec::with_capacity(n_sel);
        for _ in 0..n_sel {
            let mut j = 0;
            while g!(1) == 1 {
                j += 1;
                if j >= n_groups {
                    return Err(Error::Data);
                }
            }
            let v = smtf[j];
            smtf.copy_within(0..j, 1);
            smtf[0] = v;
            selectors.push(v);
        }
        let mut tables = Vec::with_capacity(n_groups);
        for _ in 0..n_groups {
            let mut cur = g!(5) as i32;
            let mut lens = vec![0u8; alpha];
            for l in lens.iter_mut() {
                loop {
                    if !(1..=MAX_CODE_LEN as i32).contains(&cur) {
                        return Err(Error::Data);
                    }
                    if g!(1) == 0 {
                        break;
                    }
                    if g!(1) == 0 {
                        cur += 1;
                    } else {
                        cur -= 1;
                    }
                }
                *l = cur as u8;
            }
            tables.push(Table::new(&lens)?);
        }
        if orig as usize >= self.max_block.max(1) {
            return Err(Error::Data);
        }
        Ok(Some(Some(BlockState {
            crc,
            orig,
            mtf: (0..seq.len()).map(|x| x as u8).collect(),
            seq,
            tables,
            selectors,
            n_sym: 0,
            run: 0,
            run_weight: 1,
            tt: Vec::new(),
            eob: (alpha - 1) as u16,
        })))
    }

    fn symbols(&mut self, b: &mut Bits, bs: &mut BlockState) -> Result<bool, Error> {
        loop {
            let gi = bs.n_sym / GROUP_SIZE;
            if gi >= bs.selectors.len() {
                return Err(Error::Data);
            }
            let t = &bs.tables[bs.selectors[gi] as usize];
            let start = b.pos;
            let mut code = match b.get(t.min_len) {
                Some(v) => v as i32,
                None => {
                    self.bitpos = start;
                    return Ok(false);
                }
            };
            let mut l = t.min_len;
            let sym;
            loop {
                if code <= t.limit[l as usize] {
                    let idx = t.base[l as usize] + code;
                    if idx < 0 || idx as usize >= t.perm.len() {
                        return Err(Error::Data);
                    }
                    sym = t.perm[idx as usize];
                    break;
                }
                l += 1;
                if l > t.max_len {
                    return Err(Error::Data);
                }
                match b.get(1) {
                    Some(v) => code = (code << 1) | v as i32,
                    None => {
                        self.bitpos = start;
                        return Ok(false);
                    }
                }
            }
            self.bitpos = b.pos;
            bs.n_sym += 1;
            if sym <= 1 {
                bs.run += bs.run_weight << sym;
                bs.run_weight <<= 1;
                if bs.run > self.max_block as u32 {
                    return Err(Error::Data);
                }
                continue;
            }
            if bs.run > 0 {
                let v = bs.seq[bs.mtf[0] as usize];
                if bs.tt.len() + bs.run as usize > self.max_block {
                    return Err(Error::Data);
                }
                bs.tt.extend(std::iter::repeat_n(v, bs.run as usize));
                bs.run = 0;
                bs.run_weight = 1;
            }
            if sym == bs.eob {
                return Ok(true);
            }
            let pos = (sym - 1) as usize;
            if pos >= bs.mtf.len() {
                return Err(Error::Data);
            }
            let v = bs.mtf[pos];
            bs.mtf.copy_within(0..pos, 1);
            bs.mtf[0] = v;
            if bs.tt.len() >= self.max_block {
                return Err(Error::Data);
            }
            bs.tt.push(bs.seq[v as usize]);
        }
    }

    fn finish_block(&mut self, bs: BlockState) -> Result<(), Error> {
        let n = bs.tt.len();
        if bs.orig as usize >= n {
            return Err(Error::Data);
        }
        // inverse BWT
        let mut count = [0usize; 256];
        for &c in &bs.tt {
            count[c as usize] += 1;
        }
        let mut start = [0usize; 256];
        let mut sum = 0;
        for i in 0..256 {
            start[i] = sum;
            sum += count[i];
        }
        let mut next = vec![0u32; n];
        for (i, &c) in bs.tt.iter().enumerate() {
            next[start[c as usize]] = i as u32;
            start[c as usize] += 1;
        }
        let mut p = next[bs.orig as usize] as usize;
        let mut decoded = Vec::with_capacity(n);
        for _ in 0..n {
            decoded.push(bs.tt[p]);
            p = next[p] as usize;
        }
        // undo RLE1
        let mut out = Vec::with_capacity(n + n / 4);
        let mut i = 0;
        let mut run = 0;
        let mut last: i32 = -1;
        while i < decoded.len() {
            let c = decoded[i];
            i += 1;
            if run == 4 {
                out.extend(std::iter::repeat_n(last as u8, c as usize));
                run = 0;
                last = -1;
                continue;
            }
            if c as i32 == last {
                run += 1;
            } else {
                run = 1;
                last = c as i32;
            }
            out.push(c);
        }
        let crc = !crc_update(0xffff_ffff, &out);
        if crc != bs.crc {
            return Err(Error::Data);
        }
        self.combined = self.combined.rotate_left(1) ^ crc;
        if self.out_pos == self.out.len() {
            self.out = out;
            self.out_pos = 0;
        } else {
            self.out.extend_from_slice(&out);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_all(c: &[u8]) -> Vec<u8> {
        let mut d = Decoder::new();
        d.feed(c);
        let out = d.read(usize::MAX).unwrap();
        assert!(d.is_done());
        out
    }

    #[test]
    fn roundtrip_and_external() {
        let mut data = Vec::new();
        for i in 0..300_000u32 {
            data.push(if i % 1000 < 300 { b'a' } else { (i.wrapping_mul(2654435761) >> 24) as u8 % 7 + b'a' });
        }
        for n in [0usize, 1, 5, 1000, 300_000] {
            for lvl in [1u32, 9] {
                let c = compress(&data[..n], lvl);
                assert_eq!(decode_all(&c), &data[..n]);
                // streaming in small pieces
                let mut d = Decoder::new();
                let mut out = Vec::new();
                for ch in c.chunks(7) {
                    d.feed(ch);
                    out.extend(d.read(100_000).unwrap());
                }
                out.extend(d.read(usize::MAX).unwrap());
                assert!(out == data[..n]);
            }
        }
        if let Ok(o) = std::process::Command::new("bzip2").arg("--version").output() {
            let _ = o;
            use std::io::Write;
            let p = std::env::temp_dir().join("rpy_bz2_test.bin");
            std::fs::File::create(&p).unwrap().write_all(&data).unwrap();
            let out = std::process::Command::new("bzip2").arg("-c").arg(&p).output().unwrap();
            assert_eq!(decode_all(&out.stdout), data);
            let ours = compress(&data, 9);
            let q = std::env::temp_dir().join("rpy_bz2_test.bz2");
            std::fs::File::create(&q).unwrap().write_all(&ours).unwrap();
            let back = std::process::Command::new("bzip2").arg("-dc").arg(&q).output().unwrap();
            assert!(back.status.success());
            assert!(back.stdout == data);
        }
    }
}
