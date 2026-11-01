//! The LZMA probability model and the binary range coder that drives it.
//!
//! Every adaptive probability is an 11-bit estimate of "the next bit is 0"; a coded bit moves
//! the estimate 1/32 of the way towards the observed value.  The encoder and the decoder share
//! the model layout so both sides update it identically.

pub const STATES: usize = 12;
pub const POS_STATES_MAX: usize = 16;
pub const LEN_MIN: usize = 2;
pub const LEN_MAX: usize = 273;
pub const DIST_STATES: usize = 4;
/// Distance slots below this code their low bits with adaptive reverse bit trees.
pub const END_POS_MODEL: u32 = 14;
pub const FULL_DISTANCES: u32 = 1 << (END_POS_MODEL / 2);
pub const ALIGN_BITS: u32 = 4;
pub const PROB_BITS: u32 = 11;
pub const PROB_ONE: u32 = 1 << PROB_BITS;
pub const PROB_INIT: u16 = (PROB_ONE / 2) as u16;
pub const MOVE_BITS: u32 = 5;
pub const TOP: u32 = 1 << 24;

/// Coder state after a literal / match / rep / short-rep, per previous state.
#[inline]
pub fn after_literal(s: usize) -> usize {
    if s < 4 {
        0
    } else if s < 10 {
        s - 3
    } else {
        s - 6
    }
}
#[inline]
pub fn after_match(s: usize) -> usize {
    if s < 7 { 7 } else { 10 }
}
#[inline]
pub fn after_rep(s: usize) -> usize {
    if s < 7 { 8 } else { 11 }
}
#[inline]
pub fn after_shortrep(s: usize) -> usize {
    if s < 7 { 9 } else { 11 }
}
#[inline]
pub fn is_literal_state(s: usize) -> bool {
    s < 7
}

#[derive(Clone)]
pub struct LenModel {
    pub choice: u16,
    pub choice2: u16,
    pub low: [[u16; 8]; POS_STATES_MAX],
    pub mid: [[u16; 8]; POS_STATES_MAX],
    pub high: [u16; 256],
}

impl LenModel {
    pub fn new() -> LenModel {
        LenModel {
            choice: PROB_INIT,
            choice2: PROB_INIT,
            low: [[PROB_INIT; 8]; POS_STATES_MAX],
            mid: [[PROB_INIT; 8]; POS_STATES_MAX],
            high: [PROB_INIT; 256],
        }
    }
}

/// lc/lp/pb literal-context and position bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Props {
    pub lc: u32,
    pub lp: u32,
    pub pb: u32,
}

impl Props {
    pub fn from_byte(b: u8) -> Option<Props> {
        if b >= 9 * 5 * 5 {
            return None;
        }
        let b = b as u32;
        Some(Props { lc: b % 9, lp: (b / 9) % 5, pb: b / 45 })
    }
    pub fn to_byte(self) -> u8 {
        ((self.pb * 5 + self.lp) * 9 + self.lc) as u8
    }
}

#[derive(Clone)]
pub struct Model {
    pub props: Props,
    pub literal: Vec<u16>,
    pub is_match: [u16; STATES * POS_STATES_MAX],
    pub is_rep: [u16; STATES],
    pub is_rep_g0: [u16; STATES],
    pub is_rep_g1: [u16; STATES],
    pub is_rep_g2: [u16; STATES],
    pub is_rep0_long: [u16; STATES * POS_STATES_MAX],
    pub dist_slot: [[u16; 64]; DIST_STATES],
    /// Reverse bit trees for slots 4..13, addressed as `base - slot + m` (m starts at 1).
    pub dist_special: [u16; (FULL_DISTANCES - END_POS_MODEL + 1) as usize],
    pub align: [u16; 1 << ALIGN_BITS],
    pub len: LenModel,
    pub rep_len: LenModel,
    /// Coder state (0..12) and the four most recent distances.
    pub state: usize,
    pub reps: [u32; 4],
}

impl Model {
    pub fn new(props: Props) -> Model {
        Model {
            props,
            literal: vec![PROB_INIT; 0x300 << (props.lc + props.lp)],
            is_match: [PROB_INIT; STATES * POS_STATES_MAX],
            is_rep: [PROB_INIT; STATES],
            is_rep_g0: [PROB_INIT; STATES],
            is_rep_g1: [PROB_INIT; STATES],
            is_rep_g2: [PROB_INIT; STATES],
            is_rep0_long: [PROB_INIT; STATES * POS_STATES_MAX],
            dist_slot: [[PROB_INIT; 64]; DIST_STATES],
            dist_special: [PROB_INIT; (FULL_DISTANCES - END_POS_MODEL + 1) as usize],
            align: [PROB_INIT; 1 << ALIGN_BITS],
            len: LenModel::new(),
            rep_len: LenModel::new(),
            state: 0,
            reps: [0; 4],
        }
    }

    /// Reset all probabilities and the coder state, possibly with new lc/lp/pb.
    pub fn reset(&mut self, props: Props) {
        if props != self.props {
            *self = Model::new(props);
            return;
        }
        self.literal.iter_mut().for_each(|p| *p = PROB_INIT);
        let fresh = LenModel::new();
        self.is_match = [PROB_INIT; STATES * POS_STATES_MAX];
        self.is_rep = [PROB_INIT; STATES];
        self.is_rep_g0 = [PROB_INIT; STATES];
        self.is_rep_g1 = [PROB_INIT; STATES];
        self.is_rep_g2 = [PROB_INIT; STATES];
        self.is_rep0_long = [PROB_INIT; STATES * POS_STATES_MAX];
        self.dist_slot = [[PROB_INIT; 64]; DIST_STATES];
        self.dist_special = [PROB_INIT; (FULL_DISTANCES - END_POS_MODEL + 1) as usize];
        self.align = [PROB_INIT; 1 << ALIGN_BITS];
        self.len = fresh.clone();
        self.rep_len = fresh;
        self.state = 0;
        self.reps = [0; 4];
    }

    #[inline]
    pub fn literal_base(&self, pos: u64, prev: u8) -> usize {
        let p = self.props;
        let lp_mask = (1u64 << p.lp) - 1;
        let ctx = (((pos & lp_mask) as usize) << p.lc) + ((prev as usize) >> (8 - p.lc));
        ctx * 0x300
    }

    #[inline]
    pub fn pos_mask(&self) -> u64 {
        (1u64 << self.props.pb) - 1
    }
}

/// Distance slot of a (zero-based) match distance.
#[inline]
pub fn dist_slot(dist: u32) -> u32 {
    if dist < 4 {
        return dist;
    }
    let top = 31 - dist.leading_zeros();
    (top << 1) | ((dist >> (top - 1)) & 1)
}

#[inline]
pub fn len_to_dist_state(len: usize) -> usize {
    if len - LEN_MIN < DIST_STATES { len - LEN_MIN } else { DIST_STATES - 1 }
}

// ------------------------------------------------------------------ range decoder

/// Range decoder over one contiguous input slice.  Running out of input sets `starved`
/// (zero bytes are substituted) so a caller decoding speculatively can roll back.
pub struct RcDec<'a> {
    pub range: u32,
    pub code: u32,
    pub buf: &'a [u8],
    pub pos: usize,
    pub starved: bool,
    /// When set, every probability update is recorded so it can be undone.
    pub undo: Option<&'a mut Vec<(*mut u16, u16)>>,
}

impl<'a> RcDec<'a> {
    /// Start decoding at `buf[pos..]`; the five initialisation bytes are read here.
    /// Returns None if the first byte is not zero (corrupt) .
    pub fn init(buf: &'a [u8], pos: usize) -> Option<RcDec<'a>> {
        if buf.len() < pos + 5 || buf[pos] != 0 {
            return None;
        }
        let code = u32::from_be_bytes([buf[pos + 1], buf[pos + 2], buf[pos + 3], buf[pos + 4]]);
        Some(RcDec { range: 0xFFFF_FFFF, code, buf, pos: pos + 5, starved: false, undo: None })
    }

    #[inline(always)]
    fn next_byte(&mut self) -> u8 {
        match self.buf.get(self.pos) {
            Some(&b) => {
                self.pos += 1;
                b
            }
            None => {
                self.starved = true;
                0
            }
        }
    }

    #[inline(always)]
    pub fn normalize(&mut self) {
        if self.range < TOP {
            self.range <<= 8;
            self.code = (self.code << 8) | self.next_byte() as u32;
        }
    }

    #[inline(always)]
    pub fn bit(&mut self, p: &mut u16) -> u32 {
        self.normalize();
        let prob = *p as u32;
        if let Some(log) = self.undo.as_deref_mut() {
            log.push((p as *mut u16, *p));
        }
        let bound = (self.range >> PROB_BITS) * prob;
        if self.code < bound {
            self.range = bound;
            *p = (prob + ((PROB_ONE - prob) >> MOVE_BITS)) as u16;
            0
        } else {
            self.range -= bound;
            self.code -= bound;
            *p = (prob - (prob >> MOVE_BITS)) as u16;
            1
        }
    }

    /// `n`-bit symbol from a bit tree (most significant bit first).
    #[inline]
    pub fn tree(&mut self, probs: &mut [u16], n: u32) -> u32 {
        let mut m = 1usize;
        for _ in 0..n {
            m = (m << 1) | self.bit(&mut probs[m]) as usize;
        }
        (m - (1 << n)) as u32
    }

    /// `n`-bit symbol from a reverse bit tree (least significant bit first).
    #[inline]
    pub fn rev_tree(&mut self, probs: &mut [u16], n: u32) -> u32 {
        let mut m = 1usize;
        let mut sym = 0u32;
        for i in 0..n {
            let b = self.bit(&mut probs[m]);
            m = (m << 1) | b as usize;
            sym |= b << i;
        }
        sym
    }

    #[inline]
    pub fn direct(&mut self, n: u32) -> u32 {
        let mut r = 0u32;
        for _ in 0..n {
            self.normalize();
            self.range >>= 1;
            let b = if self.code >= self.range {
                self.code -= self.range;
                1
            } else {
                0
            };
            r = (r << 1) | b;
        }
        r
    }

    pub fn len(&mut self, lm: &mut LenModel, pos_state: usize) -> usize {
        if self.bit(&mut lm.choice) == 0 {
            return LEN_MIN + self.tree(&mut lm.low[pos_state], 3) as usize;
        }
        if self.bit(&mut lm.choice2) == 0 {
            return LEN_MIN + 8 + self.tree(&mut lm.mid[pos_state], 3) as usize;
        }
        LEN_MIN + 16 + self.tree(&mut lm.high, 8) as usize
    }
}

// ------------------------------------------------------------------ range encoder

#[derive(Clone)]
pub struct RcEnc {
    low: u64,
    range: u32,
    cache: u8,
    cache_size: u64,
    pub out: Vec<u8>,
}

impl RcEnc {
    pub fn new() -> RcEnc {
        RcEnc { low: 0, range: 0xFFFF_FFFF, cache: 0, cache_size: 1, out: Vec::new() }
    }

    pub fn reset(&mut self) {
        self.low = 0;
        self.range = 0xFFFF_FFFF;
        self.cache = 0;
        self.cache_size = 1;
    }

    /// Bytes produced or still pending inside the coder (an upper bound of the final size
    /// minus the flush).
    #[inline]
    pub fn pending(&self) -> usize {
        self.out.len() + self.cache_size as usize
    }

    fn shift_low(&mut self) {
        if (self.low as u32) < 0xFF00_0000 || (self.low >> 32) != 0 {
            let carry = (self.low >> 32) as u8;
            let mut c = self.cache;
            loop {
                self.out.push(c.wrapping_add(carry));
                c = 0xFF;
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }
            }
            self.cache = (self.low >> 24) as u8;
        }
        self.cache_size += 1;
        self.low = (self.low & 0x00FF_FFFF) << 8;
    }

    #[inline(always)]
    pub fn bit(&mut self, p: &mut u16, b: u32) {
        let prob = *p as u32;
        let bound = (self.range >> PROB_BITS) * prob;
        if b == 0 {
            self.range = bound;
            *p = (prob + ((PROB_ONE - prob) >> MOVE_BITS)) as u16;
        } else {
            self.low += bound as u64;
            self.range -= bound;
            *p = (prob - (prob >> MOVE_BITS)) as u16;
        }
        while self.range < TOP {
            self.range <<= 8;
            self.shift_low();
        }
    }

    pub fn tree(&mut self, probs: &mut [u16], n: u32, sym: u32) {
        let mut m = 1usize;
        for i in (0..n).rev() {
            let b = (sym >> i) & 1;
            self.bit(&mut probs[m], b);
            m = (m << 1) | b as usize;
        }
    }

    pub fn rev_tree(&mut self, probs: &mut [u16], n: u32, sym: u32) {
        let mut m = 1usize;
        for i in 0..n {
            let b = (sym >> i) & 1;
            self.bit(&mut probs[m], b);
            m = (m << 1) | b as usize;
        }
    }

    pub fn direct(&mut self, n: u32, v: u32) {
        for i in (0..n).rev() {
            self.range >>= 1;
            if (v >> i) & 1 != 0 {
                self.low += self.range as u64;
            }
            while self.range < TOP {
                self.range <<= 8;
                self.shift_low();
            }
        }
    }

    pub fn len(&mut self, lm: &mut LenModel, len: usize, pos_state: usize) {
        let l = (len - LEN_MIN) as u32;
        if l < 8 {
            self.bit(&mut lm.choice, 0);
            self.tree(&mut lm.low[pos_state], 3, l);
        } else if l < 16 {
            self.bit(&mut lm.choice, 1);
            self.bit(&mut lm.choice2, 0);
            self.tree(&mut lm.mid[pos_state], 3, l - 8);
        } else {
            self.bit(&mut lm.choice, 1);
            self.bit(&mut lm.choice2, 1);
            self.tree(&mut lm.high, 8, l - 16);
        }
    }

    /// Emit the remaining state; the coder must be `reset()` before reuse.
    pub fn flush(&mut self) {
        for _ in 0..5 {
            self.shift_low();
        }
    }
}
