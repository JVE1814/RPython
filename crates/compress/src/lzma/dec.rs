//! LZMA and LZMA2 decoders.
//!
//! Both write into a [`Window`] (history + not-yet-consumed output).  LZMA2 input is handled a
//! whole chunk at a time (a chunk is at most 64 KiB packed), so only LZMA1 streams need the
//! speculative "tail" mode in which a symbol that runs out of input is rolled back.

use super::model::*;
use super::Error;

/// Decoded history.  Bytes from `taken` on have not been handed to the consumer yet; bytes
/// before `start` belong to a previous dictionary (LZMA2 dictionary reset) and may not be
/// referenced.
pub struct Window {
    pub buf: Vec<u8>,
    pub start: usize,
    pub taken: usize,
    pub dict_size: usize,
    /// Total bytes ever written (for literal position contexts).
    pub total: u64,
    /// `total` at the last dictionary reset (literal positions count from there).
    pub reset_at: u64,
}

impl Window {
    pub fn new(dict_size: usize) -> Window {
        Window { buf: Vec::new(), start: 0, taken: 0, dict_size: dict_size.max(4096), total: 0, reset_at: 0 }
    }

    #[inline]
    pub fn avail_dist(&self) -> usize {
        (self.buf.len() - self.start).min(self.dict_size)
    }

    #[inline]
    pub fn back(&self, dist: usize) -> u8 {
        self.buf[self.buf.len() - dist - 1]
    }

    #[inline]
    pub fn put(&mut self, b: u8) {
        self.buf.push(b);
        self.total += 1;
    }

    #[inline]
    pub fn copy(&mut self, dist: usize, len: usize) {
        let from = self.buf.len() - dist - 1;
        if dist + 1 >= len {
            self.buf.extend_from_within(from..from + len);
        } else {
            self.buf.reserve(len);
            for i in 0..len {
                let b = self.buf[from + i];
                self.buf.push(b);
            }
        }
        self.total += len as u64;
    }

    pub fn append(&mut self, d: &[u8]) {
        self.buf.extend_from_slice(d);
        self.total += d.len() as u64;
    }

    pub fn reset_dict(&mut self) {
        self.start = self.buf.len();
        self.reset_at = self.total;
    }

    pub fn pending(&self) -> usize {
        self.buf.len() - self.taken
    }

    /// Hand out every pending byte.
    pub fn take_all(&mut self) -> &[u8] {
        let t = self.taken;
        self.taken = self.buf.len();
        &self.buf[t..]
    }

    /// Drop history that can no longer be referenced and has been consumed.
    pub fn compact(&mut self) {
        let keep_from = self.buf.len().saturating_sub(self.dict_size);
        let cut = keep_from.min(self.taken);
        if cut >= (1 << 20) && cut >= self.buf.len() / 2 {
            self.buf.drain(..cut);
            self.taken -= cut;
            self.start = self.start.saturating_sub(cut);
        }
    }
}

pub enum Step {
    /// Output target reached (more may follow).
    Full,
    /// The coded symbols ran out (LZMA1: input starved; LZMA2 chunk: size reached).
    NeedInput,
    /// End-of-payload marker decoded.
    End,
}

/// Decode one symbol.  Returns Ok(true) at the end marker.
#[inline(always)]
fn symbol(m: &mut Model, rc: &mut RcDec, w: &mut Window) -> Result<Option<(usize, usize)>, Error> {
    let pos = w.total - w.reset_at;
    let pos_state = (pos & m.pos_mask()) as usize;
    let s = m.state;
    if rc.bit(&mut m.is_match[(s << 4) + pos_state]) == 0 {
        // literal
        let prev = if w.buf.len() > w.start { w.back(0) } else { 0 };
        let base = m.literal_base(pos, prev);
        let probs = &mut m.literal[base..base + 0x300];
        let mut sym = 1usize;
        if !is_literal_state(s) {
            let avail = w.avail_dist();
            let mut mb = if (m.reps[0] as usize) < avail { w.back(m.reps[0] as usize) as usize } else { 0 };
            loop {
                let mbit = (mb >> 7) & 1;
                mb <<= 1;
                let b = rc.bit(&mut probs[0x100 + (mbit << 8) + sym]) as usize;
                sym = (sym << 1) | b;
                if b != mbit {
                    break;
                }
                if sym >= 0x100 {
                    break;
                }
            }
        }
        while sym < 0x100 {
            sym = (sym << 1) | rc.bit(&mut probs[sym]) as usize;
        }
        m.state = after_literal(s);
        return Ok(Some((0, sym & 0xFF)));
    }
    let len;
    if rc.bit(&mut m.is_rep[s]) == 0 {
        // plain match
        len = rc.len(&mut m.len, pos_state);
        let ds = len_to_dist_state(len);
        let slot = rc.tree(&mut m.dist_slot[ds], 6);
        let dist = if slot < 4 {
            slot
        } else {
            let nb = (slot >> 1) - 1;
            let mut d = (2 | (slot & 1)) << nb;
            if slot < END_POS_MODEL {
                let off = (d - slot) as usize;
                d += rc.rev_tree(&mut m.dist_special[off..], nb);
            } else {
                d += rc.direct(nb - ALIGN_BITS) << ALIGN_BITS;
                d += rc.rev_tree(&mut m.align, ALIGN_BITS);
            }
            d
        };
        m.reps = [dist, m.reps[0], m.reps[1], m.reps[2]];
        m.state = after_match(s);
        if dist == 0xFFFF_FFFF {
            return Ok(None);
        }
    } else {
        if rc.bit(&mut m.is_rep_g0[s]) == 0 {
            if rc.bit(&mut m.is_rep0_long[(s << 4) + pos_state]) == 0 {
                m.state = after_shortrep(s);
                return Ok(Some((1, 0)));
            }
        } else {
            let d;
            if rc.bit(&mut m.is_rep_g1[s]) == 0 {
                d = m.reps[1];
            } else if rc.bit(&mut m.is_rep_g2[s]) == 0 {
                d = m.reps[2];
                m.reps[2] = m.reps[1];
            } else {
                d = m.reps[3];
                m.reps[3] = m.reps[2];
                m.reps[2] = m.reps[1];
            }
            m.reps[1] = m.reps[0];
            m.reps[0] = d;
        }
        len = rc.len(&mut m.rep_len, pos_state);
        m.state = after_rep(s);
    }
    Ok(Some((len, 0)))
}

/// Apply a decoded symbol to the window.
#[inline(always)]
fn apply(m: &Model, w: &mut Window, sym: (usize, usize)) -> Result<(), Error> {
    let (len, lit) = sym;
    if len == 0 {
        w.put(lit as u8);
        return Ok(());
    }
    let d = m.reps[0] as usize;
    if d >= w.avail_dist() {
        return Err(Error::Data);
    }
    w.copy(d, len);
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Plenty of input buffered: stop when fewer than 64 bytes remain.
    Fast,
    /// Near the end of the buffered input: decode each symbol speculatively.
    Tail,
    /// The input slice is complete (an LZMA2 chunk): running out of it is corruption.
    Exact,
}

/// Decode symbols until the window holds `target` pending bytes, `limit` total bytes have
/// been produced (a match crossing it is an error), the end marker (after which the coder's
/// final normalisation has been done), or the input runs short.
pub fn run(
    m: &mut Model,
    rc: &mut RcDec,
    w: &mut Window,
    target: usize,
    limit: Option<u64>,
    mode: Mode,
    undo: &mut Vec<(*mut u16, u16)>,
) -> Result<Step, Error> {
    loop {
        if w.pending() >= target {
            return Ok(Step::Full);
        }
        if let Some(l) = limit {
            if w.total >= l {
                return Ok(Step::NeedInput);
            }
        }
        let r = match mode {
            Mode::Fast => {
                if rc.buf.len() - rc.pos < 64 {
                    return Ok(Step::NeedInput);
                }
                let r = symbol(m, rc, w)?;
                if r.is_none() {
                    rc.normalize();
                }
                r
            }
            Mode::Exact => {
                let r = symbol(m, rc, w)?;
                if r.is_none() {
                    rc.normalize();
                }
                if rc.starved {
                    return Err(Error::Data);
                }
                r
            }
            Mode::Tail => {
                undo.clear();
                let (state, reps) = (m.state, m.reps);
                let mut rc2 = RcDec { range: rc.range, code: rc.code, buf: rc.buf, pos: rc.pos, starved: false, undo: Some(&mut *undo) };
                let r = symbol(m, &mut rc2, w);
                if matches!(r, Ok(None)) {
                    rc2.normalize();
                }
                let (range, code, pos, starved) = (rc2.range, rc2.code, rc2.pos, rc2.starved);
                drop(rc2);
                if starved {
                    // Out of input mid-symbol: restore the model and wait for more.
                    for &(p, v) in undo.iter().rev() {
                        // SAFETY: the pointers refer into `m`, which has not moved.
                        unsafe { *p = v };
                    }
                    m.state = state;
                    m.reps = reps;
                    return Ok(Step::NeedInput);
                }
                rc.range = range;
                rc.code = code;
                rc.pos = pos;
                r?
            }
        };
        match r {
            None => return Ok(Step::End),
            Some(sym) => {
                if let Some(l) = limit {
                    let n = if sym.0 == 0 { 1 } else { sym.0 as u64 };
                    if w.total + n > l {
                        return Err(Error::Data);
                    }
                }
                apply(m, w, sym)?
            }
        }
    }
}

// ------------------------------------------------------------------ LZMA1 stream

/// Incremental raw LZMA1 decoder.  Input is passed per call; the number of bytes consumed
/// is returned (a partial trailing symbol is left unconsumed).
pub struct Lzma1Dec {
    pub model: Model,
    pub win: Window,
    rc_range: u32,
    rc_code: u32,
    rc_started: bool,
    /// Uncompressed size from the .lzma header, if known.
    pub size: Option<u64>,
    pub done: bool,
    undo: Vec<(*mut u16, u16)>,
}

// SAFETY: the raw pointers in `undo` are only used during a single decode call.
unsafe impl Send for Lzma1Dec {}

impl Lzma1Dec {
    pub fn new(props: Props, dict_size: u32, size: Option<u64>) -> Lzma1Dec {
        Lzma1Dec {
            model: Model::new(props),
            win: Window::new(dict_size as usize),
            rc_range: 0,
            rc_code: 0,
            rc_started: false,
            size,
            done: false,
            undo: Vec::new(),
        }
    }

    /// Decode until `target` bytes are pending in the window, the input runs out, or the end.
    pub fn decode(&mut self, input: &[u8], target: usize) -> Result<usize, Error> {
        if self.done {
            return Ok(0);
        }
        let mut start = 0;
        if !self.rc_started {
            if self.size == Some(0) && input.len() < 5 {
                self.done = true;
                return Ok(0);
            }
            if input.len() < 5 {
                return Ok(0);
            }
            let rc = RcDec::init(input, 0).ok_or(Error::Data)?;
            self.rc_range = rc.range;
            self.rc_code = rc.code;
            self.rc_started = true;
            start = 5;
        }
        let mut rc = RcDec { range: self.rc_range, code: self.rc_code, buf: input, pos: start, starved: false, undo: None };
        let res = loop {
            let mode = if rc.buf.len() - rc.pos < 64 { Mode::Tail } else { Mode::Fast };
            match run(&mut self.model, &mut rc, &mut self.win, target, self.size, mode, &mut self.undo) {
                Err(e) => break Err(e),
                Ok(Step::Full) => break Ok(()),
                Ok(Step::End) => {
                    if self.size.is_some_and(|s| s != self.win.total) {
                        break Err(Error::Data);
                    }
                    self.done = true;
                    break Ok(());
                }
                Ok(Step::NeedInput) => {
                    if let Some(s) = self.size {
                        if self.win.total >= s {
                            // Known size reached (an end marker is not required).
                            if rc.range < TOP && rc.pos < rc.buf.len() {
                                rc.normalize();
                            }
                            self.done = true;
                            break Ok(());
                        }
                    }
                    if mode == Mode::Tail {
                        break Ok(());
                    }
                }
            }
        };
        self.rc_range = rc.range;
        self.rc_code = rc.code;
        res.map(|_| rc.pos)
    }
}

// ------------------------------------------------------------------ LZMA2 stream

pub fn lzma2_dict_size(b: u8) -> Option<u32> {
    if b > 40 {
        return None;
    }
    if b == 40 {
        return Some(0xFFFF_FFFF);
    }
    Some((2 | (b as u32 & 1)) << (b / 2 + 11))
}

/// Incremental LZMA2 decoder: whole chunks are decoded once they are fully available.
pub struct Lzma2Dec {
    pub model: Option<Model>,
    pub win: Window,
    need_dict_reset: bool,
    pub done: bool,
    undo: Vec<(*mut u16, u16)>,
}

// SAFETY: see Lzma1Dec.
unsafe impl Send for Lzma2Dec {}

impl Lzma2Dec {
    pub fn new(dict_size: u32) -> Lzma2Dec {
        Lzma2Dec {
            model: None,
            win: Window::new((dict_size as usize).min(1 << 31)),
            need_dict_reset: true,
            done: false,
            undo: Vec::new(),
        }
    }

    /// Decode whole chunks from `input` while fewer than `target` bytes are pending.
    /// Returns the bytes consumed.
    pub fn decode(&mut self, input: &[u8], target: usize) -> Result<usize, Error> {
        let mut used = 0;
        while !self.done && self.win.pending() < target {
            let inp = &input[used..];
            if inp.is_empty() {
                break;
            }
            let ctl = inp[0];
            if ctl == 0 {
                used += 1;
                self.done = true;
                break;
            }
            if ctl == 1 || ctl == 2 {
                if inp.len() < 3 {
                    break;
                }
                let size = (((inp[1] as usize) << 8) | inp[2] as usize) + 1;
                if inp.len() < 3 + size {
                    break;
                }
                if ctl == 1 {
                    self.win.reset_dict();
                    self.need_dict_reset = false;
                } else if self.need_dict_reset {
                    return Err(Error::Data);
                }
                self.win.append(&inp[3..3 + size]);
                used += 3 + size;
                continue;
            }
            if ctl < 0x80 {
                return Err(Error::Data);
            }
            let reset = (ctl >> 5) & 3;
            let hdr = if reset >= 2 { 6 } else { 5 };
            if inp.len() < hdr {
                break;
            }
            let unpacked = ((((ctl & 0x1F) as usize) << 16) | ((inp[1] as usize) << 8) | inp[2] as usize) + 1;
            let packed = (((inp[3] as usize) << 8) | inp[4] as usize) + 1;
            if inp.len() < hdr + packed {
                break;
            }
            if reset == 3 {
                self.win.reset_dict();
                self.need_dict_reset = false;
            } else if self.need_dict_reset {
                return Err(Error::Data);
            }
            if reset >= 2 {
                let props = Props::from_byte(inp[5]).ok_or(Error::Data)?;
                if props.lc + props.lp > 4 {
                    return Err(Error::Data);
                }
                match &mut self.model {
                    Some(m) => m.reset(props),
                    None => self.model = Some(Model::new(props)),
                }
            } else {
                match &mut self.model {
                    Some(m) if reset == 1 => {
                        let p = m.props;
                        m.reset(p)
                    }
                    Some(_) => {}
                    None => return Err(Error::Data),
                }
            }
            let chunk = &inp[hdr..hdr + packed];
            let m = self.model.as_mut().unwrap();
            let mut rc = RcDec::init(chunk, 0).ok_or(Error::Data)?;
            let limit = self.win.total + unpacked as u64;
            match run(m, &mut rc, &mut self.win, usize::MAX, Some(limit), Mode::Exact, &mut self.undo)? {
                Step::NeedInput if self.win.total == limit => {}
                _ => return Err(Error::Data),
            }
            rc.normalize();
            if rc.starved || rc.pos != chunk.len() {
                return Err(Error::Data);
            }
            used += hdr + packed;
        }
        Ok(used)
    }
}
