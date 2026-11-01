//! Non-LZ filters of the .xz format: Delta and the branch/call/jump (BCJ) converters that
//! turn relative branch targets of machine code into absolute ones (and back).

pub const FILTER_DELTA: u64 = 0x03;
pub const FILTER_X86: u64 = 0x04;
pub const FILTER_POWERPC: u64 = 0x05;
pub const FILTER_IA64: u64 = 0x06;
pub const FILTER_ARM: u64 = 0x07;
pub const FILTER_ARMTHUMB: u64 = 0x08;
pub const FILTER_SPARC: u64 = 0x09;
pub const FILTER_ARM64: u64 = 0x0A;
pub const FILTER_RISCV: u64 = 0x0B;
pub const FILTER_LZMA2: u64 = 0x21;
pub const FILTER_LZMA1: u64 = 0x4000_0000_0000_0001;

pub fn is_bcj(id: u64) -> bool {
    (FILTER_X86..=FILTER_SPARC).contains(&id) || id == FILTER_ARM64 || id == FILTER_RISCV
}

/// A filter converts a buffer in place.  It returns how many leading bytes are final; the
/// remaining tail needs more data (or is passed through unchanged at the end of input).
pub trait Filter: Send {
    fn convert(&mut self, buf: &mut [u8]) -> usize;
}

// ------------------------------------------------------------------ delta

pub struct Delta {
    dist: usize,
    hist: [u8; 256],
    idx: u8,
    encode: bool,
}

impl Delta {
    pub fn new(dist: usize, encode: bool) -> Delta {
        Delta { dist, hist: [0; 256], idx: 0, encode }
    }
}

impl Filter for Delta {
    fn convert(&mut self, buf: &mut [u8]) -> usize {
        // hist[idx - k] holds the (unfiltered) byte k positions back.
        for b in buf.iter_mut() {
            let back = self.hist[self.idx.wrapping_sub(self.dist as u8) as usize];
            let plain = if self.encode {
                let p = *b;
                *b = p.wrapping_sub(back);
                p
            } else {
                *b = b.wrapping_add(back);
                *b
            };
            self.hist[self.idx as usize] = plain;
            self.idx = self.idx.wrapping_add(1);
        }
        buf.len()
    }
}

// ------------------------------------------------------------------ BCJ

pub struct Bcj {
    id: u64,
    encode: bool,
    /// Stream position of `buf[0]` in the next call.
    pos: u32,
    /// x86 only: bitmask of recent 0xE8/0xE9 opcode bytes that were not converted.
    x86_mask: u32,
    /// x86 only: stream position just after the last examined opcode byte.
    x86_prev: u32,
}

impl Bcj {
    pub fn new(id: u64, start: u32, encode: bool) -> Bcj {
        Bcj { id, encode, pos: start, x86_mask: 0, x86_prev: start.wrapping_sub(5) }
    }

    #[inline]
    fn rel(&self, target: u32, at: u32) -> u32 {
        if self.encode { target.wrapping_add(at) } else { target.wrapping_sub(at) }
    }

    fn x86(&mut self, b: &mut [u8]) -> usize {
        // Which earlier-byte patterns still allow a conversion, and the byte of the
        // displacement that must then be checked.
        const ALLOWED: [bool; 8] = [true, true, true, false, true, false, false, false];
        const CHECK: [u32; 8] = [0, 1, 2, 2, 3, 3, 3, 3];
        let msb_ok = |v: u8| v == 0 || v == 0xFF;
        if b.len() < 5 {
            return 0;
        }
        let now = self.pos;
        if now.wrapping_sub(self.x86_prev) > 5 {
            self.x86_prev = now.wrapping_sub(5);
        }
        let mut mask = self.x86_mask;
        let mut i = 0usize;
        let last = b.len() - 5;
        while i <= last {
            if b[i] & 0xFE != 0xE8 {
                i += 1;
                continue;
            }
            let here = now.wrapping_add(i as u32);
            let gap = here.wrapping_sub(self.x86_prev);
            self.x86_prev = here;
            if gap > 5 {
                mask = 0;
            } else {
                for _ in 0..gap {
                    mask = (mask & 0x77) << 1;
                }
            }
            let hi = b[i + 4];
            if msb_ok(hi) && ALLOWED[((mask >> 1) & 7) as usize] && (mask >> 1) < 0x10 {
                let mut src = u32::from_le_bytes([b[i + 1], b[i + 2], b[i + 3], b[i + 4]]);
                let at = here.wrapping_add(5);
                let mut dest;
                loop {
                    dest = self.rel(src, at);
                    if mask == 0 {
                        break;
                    }
                    let k = CHECK[(mask >> 1) as usize];
                    let probe = (dest >> (24 - k * 8)) as u8;
                    if !msb_ok(probe) {
                        break;
                    }
                    src = dest ^ ((1u32 << (32 - k * 8)) - 1);
                }
                let sign = if (dest >> 24) & 1 != 0 { 0xFF } else { 0x00 };
                let d = dest.to_le_bytes();
                b[i + 1] = d[0];
                b[i + 2] = d[1];
                b[i + 3] = d[2];
                b[i + 4] = sign;
                i += 5;
                mask = 0;
            } else {
                i += 1;
                mask |= 1;
                if msb_ok(hi) {
                    mask |= 0x10;
                }
            }
        }
        self.x86_mask = mask;
        // The previous-position bookkeeping is relative to stream positions, so it carries
        // over to the next call unchanged.
        i
    }

    fn powerpc(&mut self, b: &mut [u8]) -> usize {
        let mut i = 0;
        while i + 4 <= b.len() {
            if b[i] >> 2 == 0x12 && b[i + 3] & 3 == 1 {
                let src = (((b[i] & 3) as u32) << 24) | ((b[i + 1] as u32) << 16) | ((b[i + 2] as u32) << 8) | ((b[i + 3] & !3) as u32);
                let d = self.rel(src, self.pos.wrapping_add(i as u32));
                b[i] = 0x48 | ((d >> 24) as u8 & 3);
                b[i + 1] = (d >> 16) as u8;
                b[i + 2] = (d >> 8) as u8;
                b[i + 3] = (b[i + 3] & 3) | (d as u8 & !3);
            }
            i += 4;
        }
        i
    }

    fn ia64(&mut self, b: &mut [u8]) -> usize {
        // Bundle templates whose slots may hold IP-relative branches (bit per slot).
        const SLOTS: [u8; 32] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 4, 6, 6, 0, 0, 7, 7, 4, 4, 0, 0, 4, 4, 0, 0];
        let mut i = 0;
        while i + 16 <= b.len() {
            let slots = SLOTS[(b[i] & 0x1F) as usize];
            for slot in 0..3u32 {
                if (slots >> slot) & 1 == 0 {
                    continue;
                }
                let bit = 5 + 41 * slot;
                let byte = (bit / 8) as usize;
                let shift = bit % 8;
                let mut raw = 0u64;
                for j in 0..6 {
                    raw |= (b[i + byte + j] as u64) << (8 * j);
                }
                let mut ins = raw >> shift;
                if (ins >> 37) & 0xF != 0x5 || (ins >> 9) & 7 != 0 {
                    continue;
                }
                let mut src = ((ins >> 13) & 0xFFFFF) as u32;
                src |= (((ins >> 36) & 1) as u32) << 20;
                src <<= 4;
                let d = self.rel(src, self.pos.wrapping_add(i as u32)) >> 4;
                ins &= !(0x8FFFFFu64 << 13);
                ins |= ((d & 0xFFFFF) as u64) << 13;
                ins |= ((d & 0x100000) as u64) << (36 - 20);
                raw &= (1u64 << shift) - 1;
                raw |= ins << shift;
                for j in 0..6 {
                    b[i + byte + j] = (raw >> (8 * j)) as u8;
                }
            }
            i += 16;
        }
        i
    }

    fn arm(&mut self, b: &mut [u8]) -> usize {
        let mut i = 0;
        while i + 4 <= b.len() {
            if b[i + 3] == 0xEB {
                let src = (((b[i + 2] as u32) << 16) | ((b[i + 1] as u32) << 8) | b[i] as u32) << 2;
                let d = self.rel(src, self.pos.wrapping_add(i as u32 + 8)) >> 2;
                b[i + 2] = (d >> 16) as u8;
                b[i + 1] = (d >> 8) as u8;
                b[i] = d as u8;
            }
            i += 4;
        }
        i
    }

    fn armthumb(&mut self, b: &mut [u8]) -> usize {
        let mut i = 0;
        while i + 4 <= b.len() {
            if b[i + 1] & 0xF8 == 0xF0 && b[i + 3] & 0xF8 == 0xF8 {
                let src = ((((b[i + 1] & 7) as u32) << 19) | ((b[i] as u32) << 11) | (((b[i + 3] & 7) as u32) << 8) | b[i + 2] as u32) << 1;
                let d = self.rel(src, self.pos.wrapping_add(i as u32 + 4)) >> 1;
                b[i + 1] = 0xF0 | ((d >> 19) & 7) as u8;
                b[i] = (d >> 11) as u8;
                b[i + 3] = 0xF8 | ((d >> 8) & 7) as u8;
                b[i + 2] = d as u8;
                i += 2;
            }
            i += 2;
        }
        i
    }

    fn sparc(&mut self, b: &mut [u8]) -> usize {
        let mut i = 0;
        while i + 4 <= b.len() {
            if (b[i] == 0x40 && b[i + 1] & 0xC0 == 0) || (b[i] == 0x7F && b[i + 1] & 0xC0 == 0xC0) {
                let src = u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) << 2;
                let mut d = self.rel(src, self.pos.wrapping_add(i as u32)) >> 2;
                d = ((0u32.wrapping_sub((d >> 22) & 1) << 22) & 0x3FFF_FFFF) | (d & 0x3F_FFFF) | 0x4000_0000;
                b[i..i + 4].copy_from_slice(&d.to_be_bytes());
            }
            i += 4;
        }
        i
    }

    fn arm64(&mut self, b: &mut [u8]) -> usize {
        let mut i = 0;
        while i + 4 <= b.len() {
            let pc = self.pos.wrapping_add(i as u32);
            let mut ins = u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
            if ins >> 26 == 0x25 {
                // BL: 26-bit word offset
                let off = self.rel(ins, pc >> 2) & 0x03FF_FFFF;
                ins = 0x9400_0000 | off;
                b[i..i + 4].copy_from_slice(&ins.to_le_bytes());
            } else if ins & 0x9F00_0000 == 0x9000_0000 {
                // ADRP: only targets within +-512 MiB are converted
                let src = ((ins >> 29) & 3) | ((ins >> 3) & 0x001F_FFFC);
                if (src.wrapping_add(0x0002_0000)) & 0x001C_0000 == 0 {
                    let d = self.rel(src, pc >> 12);
                    ins &= 0x9000_001F;
                    ins |= (d & 3) << 29;
                    ins |= (d & 0x0003_FFFC) << 3;
                    ins |= (0u32.wrapping_sub(d & 0x0002_0000)) & 0x00E0_0000;
                    b[i..i + 4].copy_from_slice(&ins.to_le_bytes());
                }
            }
            i += 4;
        }
        i
    }
}

impl Filter for Bcj {
    fn convert(&mut self, buf: &mut [u8]) -> usize {
        let n = match self.id {
            FILTER_X86 => self.x86(buf),
            FILTER_POWERPC => self.powerpc(buf),
            FILTER_IA64 => self.ia64(buf),
            FILTER_ARM => self.arm(buf),
            FILTER_ARMTHUMB => self.armthumb(buf),
            FILTER_SPARC => self.sparc(buf),
            FILTER_ARM64 => self.arm64(buf),
            _ => buf.len(),
        };
        self.pos = self.pos.wrapping_add(n as u32);
        n
    }
}

/// Buffering wrapper: feeds a filter, keeping back the unconverted tail.
pub struct Stage {
    pub f: Box<dyn Filter>,
    pending: Vec<u8>,
}

impl Stage {
    pub fn new(f: Box<dyn Filter>) -> Stage {
        Stage { f, pending: Vec::new() }
    }

    /// Convert `input`, appending finished bytes to `out`.  At `end`, the tail is released.
    pub fn push(&mut self, input: &[u8], end: bool, out: &mut Vec<u8>) {
        if self.pending.is_empty() {
            let start = out.len();
            out.extend_from_slice(input);
            let n = self.f.convert(&mut out[start..]);
            if !end {
                self.pending.extend_from_slice(&out[start + n..]);
                out.truncate(start + n);
            }
            return;
        }
        self.pending.extend_from_slice(input);
        let n = self.f.convert(&mut self.pending);
        let take = if end { self.pending.len() } else { n };
        out.extend_from_slice(&self.pending[..take]);
        self.pending.drain(..take);
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}
