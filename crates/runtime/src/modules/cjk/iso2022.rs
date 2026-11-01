//! The ISO-2022 codecs of `_codecs_iso2022` (ISO-2022-JP and variants,
//! ISO-2022-KR).
//!
//! State layout (shared by encoder and decoder): bytes 0..3 hold the charset
//! designated to G0..G3 (the final byte of the designation, | 0x80 for
//! two-byte sets), byte 4 holds flags (`SHIFTED`: G1 is invoked by SO).

use super::dbcs::{self, is_2004_addition, jisx0213_1_rev, jisx0213_2_rev};
use super::table::push_value;
use super::{CodecDef, Fail, MbCodec, St, R};

const ESC: u8 = 0x1B;
const SO: u8 = 0x0E;
const SI: u8 = 0x0F;
const SHIFTED: u8 = 1;
const MAX_ESC_LEN: usize = 16;
const ASCII: u8 = b'B';

const NO_SHIFT: u8 = 1; // SO/SI are ordinary control characters
const USE_G2: u8 = 2; // ESC . designations and ESC N single shifts
const JISX0208_EXT: u8 = 4; // ESC & @ ESC $ B (JIS X 0208-1990)

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cs {
    Jisx0201Roman,
    Jisx0201Kana,
    Jisx0208,
    Jisx0212,
    Ksx1001,
    Gb2312,
    /// JIS X 0213 plane 1: (2000 edition?, combining pairs only?)
    X0213P1(bool, bool),
    X0213P2,
    /// Single-shifted G2 sets: decoded specially, never used for encoding.
    Iso8859,
}

#[derive(Clone, Copy)]
struct Dsg {
    mark: u8,
    plane: u8,
    width: u8,
    cs: Cs,
}

const fn dbcs(fin: u8, cs: Cs) -> Dsg {
    Dsg { mark: fin | 0x80, plane: 0, width: 2, cs }
}

const JISX0208: Dsg = dbcs(b'B', Cs::Jisx0208);
const JISX0208_1978: Dsg = dbcs(b'@', Cs::Jisx0208);
const JISX0212: Dsg = dbcs(b'D', Cs::Jisx0212);
const KSX1001_G0: Dsg = dbcs(b'C', Cs::Ksx1001);
const KSX1001_G1: Dsg = Dsg { mark: b'C' | 0x80, plane: 1, width: 2, cs: Cs::Ksx1001 };
const GB2312: Dsg = dbcs(b'A', Cs::Gb2312);
const JISX0201_R: Dsg = Dsg { mark: b'J', plane: 0, width: 1, cs: Cs::Jisx0201Roman };
const JISX0201_K: Dsg = Dsg { mark: b'I', plane: 0, width: 1, cs: Cs::Jisx0201Kana };
const X0213_2000_PAIRS: Dsg = dbcs(b'O', Cs::X0213P1(true, true));
const X0213_2000_1: Dsg = dbcs(b'O', Cs::X0213P1(true, false));
const X0213_2004_PAIRS: Dsg = dbcs(b'Q', Cs::X0213P1(false, true));
const X0213_2004_1: Dsg = dbcs(b'Q', Cs::X0213P1(false, false));
const X0213_2: Dsg = dbcs(b'P', Cs::X0213P2);
const ISO8859_1: Dsg = Dsg { mark: b'A', plane: 2, width: 1, cs: Cs::Iso8859 };
const ISO8859_7: Dsg = Dsg { mark: b'F', plane: 2, width: 1, cs: Cs::Iso8859 };

struct Iso2022 {
    flags: u8,
    dsgs: &'static [Dsg],
}

/// ISO 8859-7 upper half (0xA0-0xFF); 0 = unassigned.
const ISO8859_7_HIGH: [u16; 96] = [
    0x00A0, 0x2018, 0x2019, 0x00A3, 0x20AC, 0x20AF, 0x00A6, 0x00A7, 0x00A8, 0x00A9, 0x037A, 0x00AB, 0x00AC, 0x00AD, 0, 0x2015,
    0x00B0, 0x00B1, 0x00B2, 0x00B3, 0x0384, 0x0385, 0x0386, 0x00B7, 0x0388, 0x0389, 0x038A, 0x00BB, 0x038C, 0x00BD, 0x038E, 0x038F,
    0x0390, 0x0391, 0x0392, 0x0393, 0x0394, 0x0395, 0x0396, 0x0397, 0x0398, 0x0399, 0x039A, 0x039B, 0x039C, 0x039D, 0x039E, 0x039F,
    0x03A0, 0x03A1, 0, 0x03A3, 0x03A4, 0x03A5, 0x03A6, 0x03A7, 0x03A8, 0x03A9, 0x03AA, 0x03AB, 0x03AC, 0x03AD, 0x03AE, 0x03AF,
    0x03B0, 0x03B1, 0x03B2, 0x03B3, 0x03B4, 0x03B5, 0x03B6, 0x03B7, 0x03B8, 0x03B9, 0x03BA, 0x03BB, 0x03BC, 0x03BD, 0x03BE, 0x03BF,
    0x03C0, 0x03C1, 0x03C2, 0x03C3, 0x03C4, 0x03C5, 0x03C6, 0x03C7, 0x03C8, 0x03C9, 0x03CA, 0x03CB, 0x03CC, 0x03CD, 0x03CE, 0,
];

/// Decode one character of `cs` from GL bytes `b` (1 or 2 bytes, 7-bit).
fn decode_char(cs: Cs, b: &[u8]) -> Option<u32> {
    let two = || {
        if b[1] < 0x21 || b[1] > 0x7E {
            None
        } else {
            Some((b[0] | 0x80, b[1] | 0x80))
        }
    };
    match cs {
        Cs::Jisx0201Roman => Some(match b[0] {
            0x5C => 0xA5,
            0x7E => 0x203E,
            c => c as u32,
        }),
        Cs::Jisx0201Kana => {
            if (0x21..=0x5F).contains(&b[0]) { Some(0xFF61 + (b[0] - 0x21) as u32) } else { None }
        }
        Cs::Jisx0208 => two().and_then(|(x, y)| dbcs::jisx0208_decode(x, y)),
        Cs::Jisx0212 => two().and_then(|(x, y)| dbcs::jisx0212_decode(x, y)),
        Cs::Ksx1001 => two().and_then(|(x, y)| dbcs::ksx1001_decode(x, y)),
        Cs::Gb2312 => two().and_then(|(x, y)| dbcs::gb2312_decode(x, y)),
        Cs::X0213P1(y2000, _) => two().and_then(|(x, y)| dbcs::jisx0213_1_decode(x, y, y2000)),
        Cs::X0213P2 => two().and_then(|(x, y)| dbcs::jisx0213_2_get(x, y)),
        Cs::Iso8859 => None,
    }
}

enum Enc {
    Code(u16, usize),
    TooFew,
    None,
}

/// Encode `inp[pos]` with `cs`: the GL code and the characters consumed.
fn encode_char(cs: Cs, inp: &[u32], pos: usize, flush: bool) -> Enc {
    let c = inp[pos];
    let one = |v: Option<u16>| match v {
        Some(k) => Enc::Code(k & 0x7F7F, 1),
        None => Enc::None,
    };
    match cs {
        Cs::Jisx0201Roman => match c {
            0xA5 => Enc::Code(0x5C, 1),
            0x203E => Enc::Code(0x7E, 1),
            _ => Enc::None,
        },
        Cs::Jisx0201Kana => {
            if (0xFF61..=0xFF9F).contains(&c) { Enc::Code((c - 0xFF61 + 0x21) as u16, 1) } else { Enc::None }
        }
        Cs::Jisx0208 => one(dbcs::jisx0208_encode(c)),
        Cs::Jisx0212 => one(dbcs::jisx0212_encode(c)),
        Cs::Ksx1001 => one(dbcs::ksx1001_encode(c)),
        Cs::Gb2312 => one(dbcs::gb2312_encode(c)),
        Cs::X0213P1(y2000, pairs_only) => {
            let r1 = jisx0213_1_rev();
            if r1.is_pair_base(c) {
                if pos + 1 >= inp.len() {
                    if !flush {
                        return Enc::TooFew;
                    }
                } else if let Some(code) = r1.pair(c, inp[pos + 1]) {
                    return Enc::Code(code & 0x7F7F, 2);
                }
            }
            if pairs_only {
                return Enc::None;
            }
            match r1.get(c) {
                Some(code) if !(y2000 && is_2004_addition((code >> 8) as u8, code as u8)) => Enc::Code(code & 0x7F7F, 1),
                _ => Enc::None,
            }
        }
        Cs::X0213P2 => one(jisx0213_2_rev().get(c)),
        Cs::Iso8859 => Enc::None,
    }
}

impl Iso2022 {
    fn find(&self, mark: u8) -> Option<&Dsg> {
        self.dsgs.iter().find(|d| d.mark == mark)
    }

    /// Process the escape sequence at `inp[*pos]` (an ESC followed by an
    /// intermediate byte).
    fn escape(&self, st: &mut St, inp: &[u8], pos: &mut usize) -> R {
        let left = inp.len() - *pos;
        let b = &inp[*pos..];
        let mut esclen = 0;
        let mut i = 1;
        while i < MAX_ESC_LEN {
            if i >= left {
                return Err(Fail::TooFew);
            }
            if b[i].is_ascii_uppercase() || b[i] == b'@' {
                esclen = i + 1;
                break;
            }
            if self.flags & JISX0208_EXT != 0 && i + 1 < left && b[i] == b'&' && b[i + 1] == b'@' {
                i += 2;
            } else {
                i += 1;
            }
        }
        if esclen == 0 {
            return Err(Fail::Illegal(1));
        }
        let (charset, plane) = match esclen {
            3 => {
                if b[1] == b'$' {
                    (b[2] | 0x80, 0)
                } else {
                    let plane = match b[1] {
                        b'(' => 0,
                        b')' => 1,
                        b'.' if self.flags & USE_G2 != 0 => 2,
                        _ => return Err(Fail::Illegal(3)),
                    };
                    (b[2], plane)
                }
            }
            4 => {
                if b[1] != b'$' {
                    return Err(Fail::Illegal(4));
                }
                let plane = match b[2] {
                    b'(' => 0,
                    b')' => 1,
                    _ => return Err(Fail::Illegal(4)),
                };
                (b[3] | 0x80, plane)
            }
            6 => {
                if self.flags & JISX0208_EXT != 0 && b[3] == ESC && b[4] == b'$' && b[5] == b'B' {
                    (b'B' | 0x80, 0)
                } else {
                    return Err(Fail::Illegal(6));
                }
            }
            n => return Err(Fail::Illegal(n)),
        };
        if charset != ASCII && self.find(charset).is_none() {
            return Err(Fail::Illegal(esclen));
        }
        st[plane] = charset;
        *pos += esclen;
        Ok(())
    }

    /// `ESC N x`: one character from G2.
    fn single_shift(&self, st: &St, inp: &[u8], pos: &mut usize, out: &mut Vec<u32>) -> R {
        if inp.len() - *pos < 3 {
            return Err(Fail::TooFew);
        }
        let x = inp[*pos + 2];
        let u = match st[2] {
            b'A' => {
                if x < 0x80 { Some(x as u32 + 0x80) } else { None }
            }
            b'F' => {
                match x {
                    0..=0x1F => Some(x as u32 + 0x80),
                    0x20..=0x7F => Some(ISO8859_7_HIGH[x as usize - 0x20] as u32).filter(|v| *v != 0),
                    _ => None,
                }
            }
            ASCII => {
                if x < 0x80 { Some(x as u32) } else { None }
            }
            _ => return Err(Fail::Internal),
        };
        match u {
            Some(u) => {
                out.push(u);
                *pos += 3;
                Ok(())
            }
            None => Err(Fail::Illegal(3)),
        }
    }
}

impl MbCodec for Iso2022 {
    fn enc_init(&self, st: &mut St) {
        *st = [0; 8];
        st[0] = ASCII;
        st[1] = ASCII;
    }

    fn encode(&self, st: &mut St, inp: &[u32], pos: &mut usize, out: &mut Vec<u8>, flush: bool) -> R {
        while *pos < inp.len() {
            let c = inp[*pos];
            if c < 0x80 {
                if st[0] != ASCII {
                    out.extend_from_slice(&[ESC, b'(', b'B']);
                    st[0] = ASCII;
                }
                if st[4] & SHIFTED != 0 {
                    out.push(SI);
                    st[4] &= !SHIFTED;
                }
                out.push(c as u8);
                *pos += 1;
                continue;
            }
            let mut found = None;
            for d in self.dsgs {
                match encode_char(d.cs, inp, *pos, flush) {
                    Enc::Code(code, n) => {
                        found = Some((d, code, n));
                        break;
                    }
                    Enc::TooFew => return Err(Fail::TooFew),
                    Enc::None => {}
                }
            }
            let Some((d, code, n)) = found else { return Err(Fail::Illegal(1)) };
            match d.plane {
                0 => {
                    if st[4] & SHIFTED != 0 {
                        out.push(SI);
                        st[4] &= !SHIFTED;
                    }
                    if st[0] != d.mark {
                        let fin = d.mark & 0x7F;
                        if d.width == 1 {
                            out.extend_from_slice(&[ESC, b'(', fin]);
                        } else if matches!(fin, b'@' | b'A' | b'B') {
                            out.extend_from_slice(&[ESC, b'$', fin]);
                        } else {
                            out.extend_from_slice(&[ESC, b'$', b'(', fin]);
                        }
                        st[0] = d.mark;
                    }
                }
                1 => {
                    if st[1] != d.mark {
                        let fin = d.mark & 0x7F;
                        if d.width == 1 {
                            out.extend_from_slice(&[ESC, b')', fin]);
                        } else {
                            out.extend_from_slice(&[ESC, b'$', b')', fin]);
                        }
                        st[1] = d.mark;
                    }
                    if st[4] & SHIFTED == 0 {
                        out.push(SO);
                        st[4] |= SHIFTED;
                    }
                }
                _ => return Err(Fail::Internal),
            }
            if d.width == 1 {
                out.push(code as u8);
            } else {
                out.push((code >> 8) as u8);
                out.push(code as u8);
            }
            *pos += n;
        }
        Ok(())
    }

    fn enc_reset(&self, st: &mut St, out: &mut Vec<u8>) {
        if st[4] & SHIFTED != 0 {
            out.push(SI);
            st[4] &= !SHIFTED;
        }
        if st[0] != ASCII {
            out.extend_from_slice(&[ESC, b'(', b'B']);
            st[0] = ASCII;
        }
    }

    fn dec_init(&self, st: &mut St) {
        *st = [0; 8];
        st[0] = ASCII;
        st[1] = ASCII;
        st[2] = ASCII;
    }

    fn decode(&self, st: &mut St, inp: &[u8], pos: &mut usize, out: &mut Vec<u32>) -> R {
        while *pos < inp.len() {
            let c = inp[*pos];
            match c {
                ESC => {
                    if inp.len() - *pos < 2 {
                        return Err(Fail::TooFew);
                    }
                    let c2 = inp[*pos + 1];
                    if matches!(c2, b'(' | b')' | b'$' | b'.' | b'&') {
                        self.escape(st, inp, pos)?;
                    } else if self.flags & USE_G2 != 0 && c2 == b'N' {
                        self.single_shift(st, inp, pos, out)?;
                    } else {
                        out.push(ESC as u32);
                        *pos += 1;
                    }
                    continue;
                }
                SI if self.flags & NO_SHIFT == 0 => {
                    st[4] &= !SHIFTED;
                    *pos += 1;
                    continue;
                }
                SO if self.flags & NO_SHIFT == 0 => {
                    st[4] |= SHIFTED;
                    *pos += 1;
                    continue;
                }
                b'\n' => {
                    st[4] &= !SHIFTED;
                    out.push(c as u32);
                    *pos += 1;
                    continue;
                }
                0..=0x1F => {
                    out.push(c as u32);
                    *pos += 1;
                    continue;
                }
                0x80..=0xFF => return Err(Fail::Illegal(1)),
                _ => {}
            }
            let charset = if st[4] & SHIFTED != 0 { st[1] } else { st[0] };
            if charset == ASCII {
                out.push(c as u32);
                *pos += 1;
                continue;
            }
            let Some(d) = self.find(charset) else { return Err(Fail::Illegal(1)) };
            let w = d.width as usize;
            if inp.len() - *pos < w {
                return Err(Fail::TooFew);
            }
            match decode_char(d.cs, &inp[*pos..*pos + w]) {
                Some(u) => push_value(out, u),
                None => return Err(Fail::Illegal(w)),
            }
            *pos += w;
        }
        Ok(())
    }
}

const JP_FLAGS: u8 = NO_SHIFT | JISX0208_EXT;

pub static CODECS: &[CodecDef] = &[
    CodecDef { name: "iso2022_kr", imp: &Iso2022 { flags: 0, dsgs: &[KSX1001_G1] } },
    CodecDef { name: "iso2022_jp", imp: &Iso2022 { flags: JP_FLAGS, dsgs: &[JISX0208, JISX0201_R, JISX0208_1978] } },
    CodecDef { name: "iso2022_jp_1", imp: &Iso2022 { flags: JP_FLAGS, dsgs: &[JISX0208, JISX0212, JISX0201_R, JISX0208_1978] } },
    CodecDef {
        name: "iso2022_jp_2",
        imp: &Iso2022 {
            flags: JP_FLAGS | USE_G2,
            dsgs: &[JISX0208, JISX0212, KSX1001_G0, GB2312, JISX0201_R, JISX0208_1978, ISO8859_1, ISO8859_7],
        },
    },
    CodecDef {
        name: "iso2022_jp_2004",
        imp: &Iso2022 { flags: JP_FLAGS, dsgs: &[X0213_2004_PAIRS, JISX0208, X0213_2004_1, X0213_2, X0213_2000_1] },
    },
    CodecDef { name: "iso2022_jp_3", imp: &Iso2022 { flags: JP_FLAGS, dsgs: &[X0213_2000_PAIRS, JISX0208, X0213_2000_1, X0213_2] } },
    CodecDef { name: "iso2022_jp_ext", imp: &Iso2022 { flags: JP_FLAGS, dsgs: &[JISX0208, JISX0212, JISX0201_R, JISX0201_K, JISX0208_1978] } },
];
