//! Zstandard frame decoder (RFC 8878): streaming over whole blocks, skippable frames,
//! dictionaries (raw content or formatted with entropy tables), content checksums.

use super::bits::RevReader;
use super::fse::{read_ncount, DTable};
use super::huf::{self, HufDTable};
use super::xxh::Xxh64;
use super::*;
use std::sync::Arc;

/// Entropy state carried from block to block (and loaded from dictionaries).
#[derive(Clone)]
pub struct Entropy {
    pub huf: Option<HufDTable>,
    pub ll: Option<DTable>,
    pub of: Option<DTable>,
    pub ml: Option<DTable>,
    pub reps: [u32; 3],
}

impl Entropy {
    pub fn fresh() -> Entropy {
        Entropy { huf: None, ll: None, of: None, ml: None, reps: [1, 4, 8] }
    }
}

/// A dictionary prepared for decoding.
pub struct DDict {
    pub id: u32,
    pub content: Vec<u8>,
    pub entropy: Option<Entropy>,
}

impl DDict {
    /// Load `data`: a formatted dictionary when it starts with the magic number (unless
    /// `raw` forces raw content), raw content otherwise.
    pub fn load(data: &[u8], raw: bool) -> Result<DDict, Error> {
        if raw || data.len() < 8 || u32::from_le_bytes(data[..4].try_into().unwrap()) != DICT_MAGIC {
            return Ok(DDict { id: 0, content: data.to_vec(), entropy: None });
        }
        let id = u32::from_le_bytes(data[4..8].try_into().unwrap());
        let mut p = 8;
        let (h, n) = huf::read_table(&data[p..]).map_err(|_| Error::DictCorrupt)?;
        p += n;
        let (of, log, n) = read_ncount(&data[p..], MAX_OF_CODE, OF_MAX_LOG).map_err(|_| Error::DictCorrupt)?;
        let of = DTable::build(&of, log);
        p += n;
        let (ml, log, n) = read_ncount(&data[p..], MAX_ML_CODE, ML_MAX_LOG).map_err(|_| Error::DictCorrupt)?;
        let ml = DTable::build(&ml, log);
        p += n;
        let (ll, log, n) = read_ncount(&data[p..], MAX_LL_CODE, LL_MAX_LOG).map_err(|_| Error::DictCorrupt)?;
        let ll = DTable::build(&ll, log);
        p += n;
        if data.len() < p + 12 {
            return Err(Error::DictCorrupt);
        }
        let mut reps = [0u32; 3];
        for (i, r) in reps.iter_mut().enumerate() {
            *r = u32::from_le_bytes(data[p + 4 * i..p + 4 * i + 4].try_into().unwrap());
        }
        p += 12;
        let content = data[p..].to_vec();
        for &r in &reps {
            if r == 0 || r as usize > content.len().max(1) + (1 << 17) {
                return Err(Error::DictCorrupt);
            }
        }
        Ok(DDict { id, content, entropy: Some(Entropy { huf: Some(h), ll: Some(ll), of: Some(of), ml: Some(ml), reps }) })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FrameHeader {
    pub content_size: Option<u64>,
    pub window_size: u64,
    pub dict_id: u32,
    pub checksum: bool,
    pub header_len: usize,
}

/// Parse a frame header at the start of `d` (after which `header_len` bytes are used).
/// Ok(None): not enough data.  Skippable frames are reported with `content_size` = Some(0)
/// and `header_len` = 8 (and window_size = the skippable payload size).
pub fn parse_frame_header(d: &[u8]) -> Result<Option<(FrameHeader, bool)>, Error> {
    if d.len() < 4 {
        return Ok(None);
    }
    let magic = u32::from_le_bytes(d[..4].try_into().unwrap());
    if magic & 0xFFFF_FFF0 == SKIPPABLE_MAGIC {
        if d.len() < 8 {
            return Ok(None);
        }
        let size = u32::from_le_bytes(d[4..8].try_into().unwrap()) as u64;
        return Ok(Some((FrameHeader { content_size: Some(0), window_size: size, dict_id: 0, checksum: false, header_len: 8 }, true)));
    }
    if magic != FRAME_MAGIC {
        return Err(Error::UnknownFrame);
    }
    if d.len() < 5 {
        return Ok(None);
    }
    let fhd = d[4];
    let fcs_flag = fhd >> 6;
    let single = fhd & 0x20 != 0;
    if fhd & 0x08 != 0 {
        return Err(Error::Corrupt);
    }
    let checksum = fhd & 0x04 != 0;
    let did_size = [0usize, 1, 2, 4][(fhd & 3) as usize];
    let fcs_size = match fcs_flag {
        0 => {
            if single {
                1
            } else {
                0
            }
        }
        1 => 2,
        2 => 4,
        _ => 8,
    };
    let wd_size = if single { 0 } else { 1 };
    let len = 5 + wd_size + did_size + fcs_size;
    if d.len() < len {
        return Ok(None);
    }
    let mut p = 5;
    let mut window_size = 0u64;
    if !single {
        let wd = d[p];
        p += 1;
        let exp = (wd >> 3) as u32;
        let wlog = 10 + exp;
        let base = 1u64 << wlog;
        window_size = base + (base / 8) * (wd & 7) as u64;
    }
    let mut dict_id = 0u32;
    for i in 0..did_size {
        dict_id |= (d[p + i] as u32) << (8 * i);
    }
    p += did_size;
    let mut fcs = 0u64;
    for i in 0..fcs_size {
        fcs |= (d[p + i] as u64) << (8 * i);
    }
    if fcs_size == 2 {
        fcs += 256;
    }
    let content_size = if fcs_size > 0 { Some(fcs) } else { None };
    if single {
        window_size = fcs;
    }
    Ok(Some((FrameHeader { content_size, window_size, dict_id, checksum, header_len: len }, false)))
}

/// Total size of the first frame in `d` (None if incomplete).
pub fn frame_size(d: &[u8]) -> Result<Option<usize>, Error> {
    let Some((h, skippable)) = parse_frame_header(d)? else { return Ok(None) };
    if skippable {
        let n = 8 + h.window_size as usize;
        return Ok(if d.len() >= n { Some(n) } else { None });
    }
    let mut p = h.header_len;
    loop {
        if d.len() < p + 3 {
            return Ok(None);
        }
        let bh = d[p] as u32 | (d[p + 1] as u32) << 8 | (d[p + 2] as u32) << 16;
        let last = bh & 1 != 0;
        let btype = (bh >> 1) & 3;
        let size = (bh >> 3) as usize;
        p += 3 + match btype {
            0 | 2 => size,
            1 => 1,
            _ => return Err(Error::Corrupt),
        };
        if last {
            break;
        }
    }
    if h.checksum {
        p += 4;
    }
    Ok(if d.len() >= p { Some(p) } else { None })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum St {
    Magic,
    Header,
    BlockHeader,
    Block { last: bool, btype: u32, size: usize },
    Checksum,
    Skip(u64),
    Done,
}

pub struct Decoder {
    st: St,
    input: Vec<u8>,
    ipos: usize,
    /// History: dictionary content followed by the frame's output.
    hist: Vec<u8>,
    taken: usize,
    frame_out: u64,
    window_max: u64,
    dict: Option<Arc<DDict>>,
    header: Option<FrameHeader>,
    xxh: Option<Xxh64>,
    ent: Entropy,
    err: Option<Error>,
    lits: Vec<u8>,
}

impl Decoder {
    pub fn new(dict: Option<Arc<DDict>>, window_log_max: u32) -> Decoder {
        Decoder {
            st: St::Magic,
            input: Vec::new(),
            ipos: 0,
            hist: Vec::new(),
            taken: 0,
            frame_out: 0,
            window_max: 1u64 << window_log_max,
            dict,
            header: None,
            xxh: None,
            ent: Entropy::fresh(),
            err: None,
            lits: Vec::new(),
        }
    }

    pub fn feed(&mut self, d: &[u8]) {
        if self.ipos > 0 && (self.ipos == self.input.len() || self.ipos >= (1 << 16)) {
            self.input.drain(..self.ipos);
            self.ipos = 0;
        }
        self.input.extend_from_slice(d);
    }

    /// End of the frame reached and all output handed out.
    pub fn is_done(&self) -> bool {
        self.st == St::Done && self.taken == self.hist.len()
    }

    /// True while a frame has been started but not finished.
    pub fn in_frame(&self) -> bool {
        !matches!(self.st, St::Magic | St::Done)
    }

    pub fn unused(&self) -> &[u8] {
        &self.input[self.ipos..]
    }

    pub fn pending(&self) -> usize {
        self.hist.len() - self.taken
    }

    /// Decode and return up to `max` bytes.
    pub fn read(&mut self, max: usize) -> Result<Vec<u8>, Error> {
        if let Some(e) = self.err {
            return Err(e);
        }
        loop {
            if self.st == St::Done {
                break;
            }
            let produces = matches!(self.st, St::Block { .. });
            if produces && self.pending() >= max {
                break;
            }
            match self.step() {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) => {
                    self.err = Some(e);
                    return Err(e);
                }
            }
        }
        let n = self.pending().min(max);
        let out = self.hist[self.taken..self.taken + n].to_vec();
        self.taken += n;
        self.compact();
        Ok(out)
    }

    fn compact(&mut self) {
        let w = self.header.map(|h| h.window_size).unwrap_or(0).max(1 << 17) as usize;
        let keep_from = self.hist.len().saturating_sub(w + self.dict.as_ref().map_or(0, |d| d.content.len()));
        let cut = keep_from.min(self.taken);
        if cut >= (1 << 20) && cut >= self.hist.len() / 2 {
            self.hist.drain(..cut);
            self.taken -= cut;
        }
    }

    fn avail(&self) -> &[u8] {
        &self.input[self.ipos..]
    }

    fn step(&mut self) -> Result<bool, Error> {
        match self.st {
            St::Magic | St::Header => {
                let Some((h, skippable)) = parse_frame_header(self.avail())? else { return Ok(false) };
                if skippable {
                    self.ipos += 8;
                    self.st = St::Skip(h.window_size);
                    return Ok(true);
                }
                if h.window_size > self.window_max {
                    return Err(Error::WindowTooLarge);
                }
                self.ipos += h.header_len;
                self.begin_frame(h)?;
                self.st = St::BlockHeader;
                Ok(true)
            }
            St::Skip(n) => {
                let a = self.avail().len() as u64;
                let k = a.min(n);
                self.ipos += k as usize;
                if k == n {
                    self.st = St::Done;
                    return Ok(true);
                }
                self.st = St::Skip(n - k);
                Ok(false)
            }
            St::BlockHeader => {
                let a = self.avail();
                if a.len() < 3 {
                    return Ok(false);
                }
                let bh = a[0] as u32 | (a[1] as u32) << 8 | (a[2] as u32) << 16;
                let btype = (bh >> 1) & 3;
                let size = (bh >> 3) as usize;
                if btype == 3 {
                    return Err(Error::Corrupt);
                }
                let h = self.header.unwrap();
                let block_max = (h.window_size.max(1) as usize).min(BLOCK_MAX);
                if (btype != 2 && size > block_max.max(1)) || (btype == 2 && size > BLOCK_MAX) {
                    return Err(Error::Corrupt);
                }
                self.ipos += 3;
                self.st = St::Block { last: bh & 1 != 0, btype, size };
                Ok(true)
            }
            St::Block { last, btype, size } => {
                let need = if btype == 1 { 1 } else { size };
                if self.avail().len() < need {
                    return Ok(false);
                }
                let start = self.hist.len();
                let data_start = self.ipos;
                self.ipos += need;
                match btype {
                    0 => {
                        let (a, b) = (data_start, data_start + size);
                        let chunk = self.input[a..b].to_vec();
                        self.hist.extend_from_slice(&chunk);
                    }
                    1 => {
                        let b = self.input[data_start];
                        self.hist.resize(start + size, b);
                    }
                    _ => {
                        let blk = std::mem::take(&mut self.input);
                        let r = self.compressed_block(&blk[data_start..data_start + size]);
                        self.input = blk;
                        r?;
                    }
                }
                let produced = self.hist.len() - start;
                if produced > BLOCK_MAX {
                    return Err(Error::Corrupt);
                }
                if let Some(x) = &mut self.xxh {
                    x.update(&self.hist[start..]);
                }
                self.frame_out += produced as u64;
                let h = self.header.unwrap();
                if let Some(cs) = h.content_size {
                    if self.frame_out > cs {
                        return Err(Error::Corrupt);
                    }
                }
                if last {
                    if let Some(cs) = h.content_size {
                        if self.frame_out != cs {
                            return Err(Error::Corrupt);
                        }
                    }
                    self.st = if h.checksum { St::Checksum } else { St::Done };
                } else {
                    self.st = St::BlockHeader;
                }
                Ok(true)
            }
            St::Checksum => {
                let a = self.avail();
                if a.len() < 4 {
                    return Ok(false);
                }
                let want = u32::from_le_bytes(a[..4].try_into().unwrap());
                let got = self.xxh.as_ref().unwrap().digest() as u32;
                if want != got {
                    return Err(Error::Checksum);
                }
                self.ipos += 4;
                self.st = St::Done;
                Ok(true)
            }
            St::Done => Ok(false),
        }
    }

    fn begin_frame(&mut self, h: FrameHeader) -> Result<(), Error> {
        self.ent = Entropy::fresh();
        // Drop previous output (already handed out) and install the dictionary prefix.
        self.hist.drain(..self.taken);
        self.taken = 0;
        if let Some(d) = &self.dict {
            if h.dict_id != 0 && d.id != h.dict_id {
                return Err(Error::DictMismatch);
            }
            if !self.hist.is_empty() {
                return Err(Error::Corrupt);
            }
            self.hist.extend_from_slice(&d.content);
            self.taken = self.hist.len();
            if let Some(e) = &d.entropy {
                self.ent = e.clone();
            }
        } else if h.dict_id != 0 {
            return Err(Error::DictMismatch);
        }
        self.frame_out = 0;
        self.xxh = if h.checksum { Some(Xxh64::new(0)) } else { None };
        self.header = Some(h);
        Ok(())
    }

    fn literals(&mut self, d: &[u8]) -> Result<usize, Error> {
        let Some(&b0) = d.first() else { return Err(Error::Corrupt) };
        let ltype = b0 & 3;
        let sf = (b0 >> 2) & 3;
        match ltype {
            0 | 1 => {
                let (regen, hl) = match sf {
                    0 | 2 => ((b0 >> 3) as usize, 1),
                    1 => {
                        if d.len() < 2 {
                            return Err(Error::Corrupt);
                        }
                        (((b0 >> 4) as usize) | (d[1] as usize) << 4, 2)
                    }
                    _ => {
                        if d.len() < 3 {
                            return Err(Error::Corrupt);
                        }
                        (((b0 >> 4) as usize) | (d[1] as usize) << 4 | (d[2] as usize) << 12, 3)
                    }
                };
                if regen > BLOCK_MAX {
                    return Err(Error::Corrupt);
                }
                self.lits.clear();
                if ltype == 0 {
                    if d.len() < hl + regen {
                        return Err(Error::Corrupt);
                    }
                    self.lits.extend_from_slice(&d[hl..hl + regen]);
                    Ok(hl + regen)
                } else {
                    if d.len() < hl + 1 {
                        return Err(Error::Corrupt);
                    }
                    self.lits.resize(regen, d[hl]);
                    Ok(hl + 1)
                }
            }
            _ => {
                let (hl, bits, four) = match sf {
                    0 => (3, 10, false),
                    1 => (3, 10, true),
                    2 => (4, 14, true),
                    _ => (5, 18, true),
                };
                if d.len() < hl {
                    return Err(Error::Corrupt);
                }
                let mut v = 0u64;
                for i in 0..hl {
                    v |= (d[i] as u64) << (8 * i);
                }
                v >>= 4;
                let mask = (1u64 << bits) - 1;
                let regen = (v & mask) as usize;
                let csize = ((v >> bits) & mask) as usize;
                if regen > BLOCK_MAX || d.len() < hl + csize {
                    return Err(Error::Corrupt);
                }
                let mut body = &d[hl..hl + csize];
                if ltype == 2 {
                    let (t, n) = huf::read_table(body)?;
                    body = &body[n..];
                    self.ent.huf = Some(t);
                }
                let t = self.ent.huf.as_ref().ok_or(Error::Corrupt)?;
                self.lits = huf::decode_literals(t, body, regen, four)?;
                Ok(hl + csize)
            }
        }
    }

    fn table(&self, mode: u8, d: &[u8], kind: usize) -> Result<(Option<DTable>, usize), Error> {
        let (max_sym, max_log) = match kind {
            0 => (MAX_LL_CODE, LL_MAX_LOG),
            1 => (MAX_OF_CODE, OF_MAX_LOG),
            _ => (MAX_ML_CODE, ML_MAX_LOG),
        };
        match mode {
            0 => Ok((Some(predefined(kind).clone()), 0)),
            1 => {
                let Some(&s) = d.first() else { return Err(Error::Corrupt) };
                if s as usize > max_sym {
                    return Err(Error::Corrupt);
                }
                Ok((Some(DTable::rle(s)), 1))
            }
            2 => {
                let (norm, log, n) = read_ncount(d, max_sym, max_log)?;
                Ok((Some(DTable::build(&norm, log)), n))
            }
            _ => Ok((None, 0)),
        }
    }

    fn compressed_block(&mut self, d: &[u8]) -> Result<(), Error> {
        let mut p = self.literals(d)?;
        if p > d.len() {
            return Err(Error::Corrupt);
        }
        let s = &d[p..];
        let Some(&b0) = s.first() else { return Err(Error::Corrupt) };
        let (nseq, hl) = if b0 == 0 {
            (0usize, 1)
        } else if b0 < 128 {
            (b0 as usize, 1)
        } else if b0 < 255 {
            if s.len() < 2 {
                return Err(Error::Corrupt);
            }
            ((((b0 - 128) as usize) << 8) + s[1] as usize, 2)
        } else {
            if s.len() < 3 {
                return Err(Error::Corrupt);
            }
            (s[1] as usize + ((s[2] as usize) << 8) + 0x7F00, 3)
        };
        p += hl;
        if nseq == 0 {
            if p != d.len() {
                return Err(Error::Corrupt);
            }
            let lits = std::mem::take(&mut self.lits);
            self.hist.extend_from_slice(&lits);
            self.lits = lits;
            return Ok(());
        }
        let Some(&modes) = d.get(p) else { return Err(Error::Corrupt) };
        p += 1;
        if modes & 3 != 0 {
            return Err(Error::Corrupt);
        }
        for (kind, shift) in [(0usize, 6u8), (1, 4), (2, 2)] {
            let m = (modes >> shift) & 3;
            let (t, n) = self.table(m, &d[p..], kind)?;
            p += n;
            if let Some(t) = t {
                match kind {
                    0 => self.ent.ll = Some(t),
                    1 => self.ent.of = Some(t),
                    _ => self.ent.ml = Some(t),
                }
            } else {
                let have = match kind {
                    0 => self.ent.ll.is_some(),
                    1 => self.ent.of.is_some(),
                    _ => self.ent.ml.is_some(),
                };
                if !have {
                    return Err(Error::Corrupt);
                }
            }
        }
        if p > d.len() {
            return Err(Error::Corrupt);
        }
        self.sequences(&d[p..], nseq)
    }

    fn sequences(&mut self, d: &[u8], nseq: usize) -> Result<(), Error> {
        let ll_t = self.ent.ll.take().unwrap();
        let of_t = self.ent.of.take().unwrap();
        let ml_t = self.ent.ml.take().unwrap();
        let r = self.run_sequences(d, nseq, &ll_t, &of_t, &ml_t);
        self.ent.ll = Some(ll_t);
        self.ent.of = Some(of_t);
        self.ent.ml = Some(ml_t);
        r
    }

    fn run_sequences(&mut self, d: &[u8], nseq: usize, ll_t: &DTable, of_t: &DTable, ml_t: &DTable) -> Result<(), Error> {
        let mut r = RevReader::new(d)?;
        let mut lls = r.read(ll_t.log) as usize;
        let mut ofs = r.read(of_t.log) as usize;
        let mut mls = r.read(ml_t.log) as usize;
        let lits = std::mem::take(&mut self.lits);
        let mut lp = 0usize;
        let mut reps = self.ent.reps;
        let block_start = self.hist.len();
        let res = (|| {
            for i in 0..nseq {
                let llc = ll_t.cells[lls];
                let ofc = of_t.cells[ofs];
                let mlc = ml_t.cells[mls];
                let of_code = ofc.sym as u32;
                if of_code > 31 {
                    return Err(Error::Corrupt);
                }
                let ov = (1u64 << of_code) + r.read(of_code);
                let ml = ML_BASE[mlc.sym as usize] as usize + r.read(ML_BITS[mlc.sym as usize] as u32) as usize;
                let ll = LL_BASE[llc.sym as usize] as usize + r.read(LL_BITS[llc.sym as usize] as u32) as usize;
                let off: u64 = if ov > 3 {
                    let o = ov - 3;
                    reps = [o as u32, reps[0], reps[1]];
                    o
                } else {
                    let idx = (ov - 1) as usize + (ll == 0) as usize;
                    if idx == 0 {
                        reps[0] as u64
                    } else {
                        let o = if idx == 3 { reps[0].wrapping_sub(1) } else { reps[idx] };
                        if idx == 1 {
                            reps = [o, reps[0], reps[2]];
                        } else {
                            reps = [o, reps[0], reps[1]];
                        }
                        o as u64
                    }
                };
                if i + 1 < nseq {
                    lls = llc.base as usize + r.read(llc.nb as u32) as usize;
                    mls = mlc.base as usize + r.read(mlc.nb as u32) as usize;
                    ofs = ofc.base as usize + r.read(ofc.nb as u32) as usize;
                }
                if lp + ll > lits.len() {
                    return Err(Error::Corrupt);
                }
                self.hist.extend_from_slice(&lits[lp..lp + ll]);
                lp += ll;
                let off = off as usize;
                if off == 0 || off > self.hist.len() {
                    return Err(Error::Corrupt);
                }
                if self.hist.len() + ml - block_start > BLOCK_MAX {
                    return Err(Error::Corrupt);
                }
                let from = self.hist.len() - off;
                if off >= ml {
                    self.hist.extend_from_within(from..from + ml);
                } else {
                    self.hist.reserve(ml);
                    for k in 0..ml {
                        let b = self.hist[from + k];
                        self.hist.push(b);
                    }
                }
            }
            if !r.finished() {
                return Err(Error::Corrupt);
            }
            self.hist.extend_from_slice(&lits[lp..]);
            Ok(())
        })();
        self.lits = lits;
        self.ent.reps = reps;
        res
    }
}

// ------------------------------------------------------------------ predefined tables

pub fn predefined(kind: usize) -> &'static DTable {
    use std::sync::OnceLock;
    static T: OnceLock<[DTable; 3]> = OnceLock::new();
    &T.get_or_init(|| [DTable::build(&LL_DEFAULT, 6), DTable::build(&OF_DEFAULT, 5), DTable::build(&ML_DEFAULT, 6)])[kind]
}
