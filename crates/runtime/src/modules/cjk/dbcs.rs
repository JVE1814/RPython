//! The double-byte (EUC / Shift_JIS / Big5 style) codecs of `_codecs_cn`,
//! `_codecs_jp`, `_codecs_kr`, `_codecs_tw` and `_codecs_hk`.

use super::data_cn::*;
use super::data_hk::*;
use super::data_jp::*;
use super::data_kr::*;
use super::data_tw::*;
use super::table::{push_value, Rev};
use super::{CodecDef, Fail, MbCodec, St, R};
use std::sync::OnceLock;

// ------------------------------------------------------------------ helpers

#[inline]
fn need(inp: &[u8], pos: usize, n: usize) -> R {
    if inp.len() - pos < n { Err(Fail::TooFew) } else { Ok(()) }
}

#[inline]
fn put2(out: &mut Vec<u8>, code: u16) {
    out.push((code >> 8) as u8);
    out.push(code as u8);
}

/// Write a code: one byte when below 0x100, else two.
#[inline]
fn put(out: &mut Vec<u8>, code: u16) {
    if code < 0x100 {
        out.push(code as u8);
    } else {
        put2(out, code);
    }
}

#[inline]
fn halfwidth_kana(c: u32) -> Option<u8> {
    if (0xFF61..=0xFF9F).contains(&c) { Some((c - 0xFEC0) as u8) } else { None }
}

/// Shift_JIS two-byte code -> (row, column) as EUC bytes (0xA1..), for
/// lead bytes 0x81-0x9F / 0xE0-0xEF (plane 1 / JIS X 0208 rows).
#[inline]
fn sjis_to_euc(c1: u8, c2: u8) -> Option<(u8, u8)> {
    let pair = match c1 {
        0x81..=0x9F => c1 - 0x81,
        0xE0..=0xEF => c1 - 0xC1,
        _ => return None,
    };
    let (row, col) = match c2 {
        0x40..=0x7E => (pair * 2, c2 - 0x40),
        0x80..=0x9E => (pair * 2, c2 - 0x41),
        0x9F..=0xFC => (pair * 2 + 1, c2 - 0x9F),
        _ => return None,
    };
    Some((row + 0xA1, col + 0xA1))
}

/// EUC (row, column) bytes -> Shift_JIS code (rows 1..94).
#[inline]
fn euc_to_sjis(r: u8, c: u8) -> u16 {
    let row = r - 0xA1; // 0-based
    let col = c - 0xA1;
    let lead = if row < 62 { 0x81 + row / 2 } else { 0xC1 + row / 2 };
    let trail = if row % 2 == 0 {
        let t = 0x40 + col;
        if t >= 0x7F { t + 1 } else { t }
    } else {
        0x9F + col
    };
    (lead as u16) << 8 | trail as u16
}

/// Shift_JIS lead bytes 0xF0-0xFC carry JIS X 0213 plane 2: the two rows of
/// each lead byte.
const SJIS_PLANE2_ROWS: [(u8, u8); 13] =
    [(1, 8), (3, 4), (5, 12), (13, 14), (15, 78), (79, 80), (81, 82), (83, 84), (85, 86), (87, 88), (89, 90), (91, 92), (93, 94)];

fn sjis_plane2_to_euc(c1: u8, c2: u8) -> Option<(u8, u8)> {
    let (r1, r2) = SJIS_PLANE2_ROWS[(c1 - 0xF0) as usize];
    let (row, col) = match c2 {
        0x40..=0x7E => (r1, c2 - 0x40),
        0x80..=0x9E => (r1, c2 - 0x41),
        0x9F..=0xFC => (r2, c2 - 0x9F),
        _ => return None,
    };
    Some((row + 0xA0, col + 0xA1))
}

fn euc_plane2_to_sjis(r: u8, c: u8) -> Option<u16> {
    let row = r - 0xA0;
    let col = c - 0xA1;
    for (i, (r1, r2)) in SJIS_PLANE2_ROWS.iter().enumerate() {
        let lead = 0xF0 + i as u16;
        if *r1 == row {
            let t = 0x40 + col;
            return Some(lead << 8 | if t >= 0x7F { t + 1 } else { t } as u16);
        }
        if *r2 == row {
            return Some(lead << 8 | (0x9F + col) as u16);
        }
    }
    None
}

/// Generic stateless encoder loop: ASCII passes through, everything else is
/// mapped by `f` to a one- or two-byte code.
#[inline]
fn simple_encode(inp: &[u32], pos: &mut usize, out: &mut Vec<u8>, f: impl Fn(u32) -> Option<u16>) -> R {
    while *pos < inp.len() {
        let c = inp[*pos];
        if c < 0x80 {
            out.push(c as u8);
        } else {
            match f(c) {
                Some(code) => put(out, code),
                None => return Err(Fail::Illegal(1)),
            }
        }
        *pos += 1;
    }
    Ok(())
}

/// Generic decoder loop for encodings whose multibyte sequences are exactly
/// two bytes: `lead(c)` tells whether `c` starts a two-byte code, `f` maps it.
#[inline]
fn simple_decode(inp: &[u8], pos: &mut usize, out: &mut Vec<u32>, lead: impl Fn(u8) -> bool, f: impl Fn(u8, u8) -> Option<u32>) -> R {
    while *pos < inp.len() {
        let c = inp[*pos];
        if c < 0x80 {
            out.push(c as u32);
            *pos += 1;
            continue;
        }
        if !lead(c) {
            return Err(Fail::Illegal(1));
        }
        need(inp, *pos, 2)?;
        match f(c, inp[*pos + 1]) {
            Some(v) => {
                push_value(out, v);
                *pos += 2;
            }
            None => return Err(Fail::Illegal(1)),
        }
    }
    Ok(())
}

macro_rules! lazy_rev {
    (pub $name:ident, $build:expr) => {
        pub fn $name() -> &'static Rev {
            static R: OnceLock<Rev> = OnceLock::new();
            R.get_or_init(|| $build)
        }
    };
    ($name:ident, $build:expr) => {
        fn $name() -> &'static Rev {
            static R: OnceLock<Rev> = OnceLock::new();
            R.get_or_init(|| $build)
        }
    };
}

fn high(c1: u8, c2: u8) -> bool {
    c1 >= 0xA1 && c2 >= 0xA1
}

// =================================================================== Chinese

fn gb2312_get(c1: u8, c2: u8) -> Option<u32> {
    GB2312.get(c1, c2)
}

fn gbk_get(c1: u8, c2: u8) -> Option<u32> {
    GBKEXT.get(c1, c2).or_else(|| if high(c1, c2) { GB2312.get(c1, c2) } else { None })
}

fn gb18030_get(c1: u8, c2: u8) -> Option<u32> {
    gbk_get(c1, c2).or_else(|| GB18030EXT.get(c1, c2))
}

lazy_rev!(gb2312_rev, {
    let mut r = Rev::new();
    r.add_all(0xA1..=0xF7, |c| gb2312_get((c >> 8) as u8, c as u8));
    r
});
lazy_rev!(gbk_rev, {
    let mut r = Rev::new();
    r.add_all(0x81..=0xFE, |c| gbk_get((c >> 8) as u8, c as u8));
    r
});
lazy_rev!(gb18030_rev, {
    let mut r = Rev::new();
    r.add_all(0x81..=0xFE, |c| gb18030_get((c >> 8) as u8, c as u8));
    r
});

struct Gb2312;
impl MbCodec for Gb2312 {
    fn encode(&self, _st: &mut St, inp: &[u32], pos: &mut usize, out: &mut Vec<u8>, _flush: bool) -> R {
        let rev = gb2312_rev();
        simple_encode(inp, pos, out, |c| rev.get(c))
    }
    fn decode(&self, _st: &mut St, inp: &[u8], pos: &mut usize, out: &mut Vec<u32>) -> R {
        simple_decode(inp, pos, out, |c| c >= 0xA1 && c != 0xFF, gb2312_get)
    }
}

struct Gbk;
impl MbCodec for Gbk {
    fn encode(&self, _st: &mut St, inp: &[u32], pos: &mut usize, out: &mut Vec<u8>, _flush: bool) -> R {
        let rev = gbk_rev();
        simple_encode(inp, pos, out, |c| rev.get(c))
    }
    fn decode(&self, _st: &mut St, inp: &[u8], pos: &mut usize, out: &mut Vec<u32>) -> R {
        simple_decode(inp, pos, out, |c| (0x81..=0xFE).contains(&c), gbk_get)
    }
}

/// Linear index of a four-byte GB 18030 sequence.
fn gb4_index(b: &[u8]) -> u32 {
    (((b[0] as u32 - 0x81) * 10 + (b[1] as u32 - 0x30)) * 126 + (b[2] as u32 - 0x81)) * 10 + (b[3] as u32 - 0x30)
}

fn gb4_bytes(mut l: u32, out: &mut Vec<u8>) {
    let b4 = l % 10;
    l /= 10;
    let b3 = l % 126;
    l /= 126;
    let b2 = l % 10;
    let b1 = l / 10;
    out.extend_from_slice(&[(b1 + 0x81) as u8, (b2 + 0x30) as u8, (b3 + 0x81) as u8, (b4 + 0x30) as u8]);
}

/// Four-byte BMP index -> code point.
fn gb4_bmp_decode(l: u32) -> Option<u32> {
    if l > GB18030_LAST_LINEAR {
        return None;
    }
    let i = GB18030_RUNS.partition_point(|r| r.0 <= l) - 1;
    let (rl, ru) = GB18030_RUNS[i];
    Some(ru + (l - rl))
}

/// Code point (not two-byte encodable, BMP) -> four-byte index.
fn gb4_bmp_encode(c: u32) -> Option<u32> {
    let i = GB18030_RUNS.partition_point(|r| r.1 <= c);
    if i == 0 {
        return None;
    }
    let (rl, ru) = GB18030_RUNS[i - 1];
    let l = rl + (c - ru);
    let end = if i < GB18030_RUNS.len() { GB18030_RUNS[i].0 } else { GB18030_LAST_LINEAR + 1 };
    if l < end { Some(l) } else { None }
}

const GB18030_SUPP_BASE: u32 = 189000; // linear index of 0x90308130 (U+10000)

struct Gb18030;
impl MbCodec for Gb18030 {
    fn encode(&self, _st: &mut St, inp: &[u32], pos: &mut usize, out: &mut Vec<u8>, _flush: bool) -> R {
        let rev = gb18030_rev();
        while *pos < inp.len() {
            let c = inp[*pos];
            if c < 0x80 {
                out.push(c as u8);
            } else if let Some(code) = rev.get(c) {
                put2(out, code);
            } else if c >= 0x10000 {
                gb4_bytes(GB18030_SUPP_BASE + (c - 0x10000), out);
            } else if (0xD800..0xE000).contains(&c) {
                return Err(Fail::Illegal(1));
            } else {
                match gb4_bmp_encode(c) {
                    Some(l) => gb4_bytes(l, out),
                    None => return Err(Fail::Illegal(1)),
                }
            }
            *pos += 1;
        }
        Ok(())
    }
    fn decode(&self, _st: &mut St, inp: &[u8], pos: &mut usize, out: &mut Vec<u32>) -> R {
        while *pos < inp.len() {
            let c = inp[*pos];
            if c < 0x80 {
                out.push(c as u32);
                *pos += 1;
                continue;
            }
            if c == 0x80 || c == 0xFF {
                return Err(Fail::Illegal(1));
            }
            need(inp, *pos, 2)?;
            let c2 = inp[*pos + 1];
            if (0x30..=0x39).contains(&c2) {
                need(inp, *pos, 4)?;
                let b = &inp[*pos..*pos + 4];
                if !(0x81..=0xFE).contains(&b[2]) || !(0x30..=0x39).contains(&b[3]) {
                    return Err(Fail::Illegal(1));
                }
                let l = gb4_index(b);
                let v = match c {
                    0x81..=0x84 => gb4_bmp_decode(l),
                    0x90..=0xE3 => {
                        let u = 0x10000 + (l - GB18030_SUPP_BASE);
                        if u <= 0x10FFFF { Some(u) } else { None }
                    }
                    _ => None,
                };
                match v {
                    Some(u) => {
                        out.push(u);
                        *pos += 4;
                    }
                    None => return Err(Fail::Illegal(1)),
                }
                continue;
            }
            match gb18030_get(c, c2) {
                Some(u) => {
                    out.push(u);
                    *pos += 2;
                }
                None => return Err(Fail::Illegal(1)),
            }
        }
        Ok(())
    }
}

/// HZ: ASCII with `~{` ... `~}` GB 2312 (7-bit) shifts; state byte 0 is the
/// shift (0 ASCII, 1 GB).
struct Hz;
impl MbCodec for Hz {
    fn encode(&self, st: &mut St, inp: &[u32], pos: &mut usize, out: &mut Vec<u8>, _flush: bool) -> R {
        let rev = gb2312_rev();
        while *pos < inp.len() {
            let c = inp[*pos];
            if c < 0x80 {
                if st[0] != 0 {
                    out.extend_from_slice(b"~}");
                    st[0] = 0;
                }
                if c == b'~' as u32 {
                    out.extend_from_slice(b"~~");
                } else {
                    out.push(c as u8);
                }
            } else {
                let code = rev.get(c).ok_or(Fail::Illegal(1))?;
                if st[0] == 0 {
                    out.extend_from_slice(b"~{");
                    st[0] = 1;
                }
                put2(out, code & 0x7F7F);
            }
            *pos += 1;
        }
        Ok(())
    }
    fn enc_reset(&self, st: &mut St, out: &mut Vec<u8>) {
        if st[0] != 0 {
            out.extend_from_slice(b"~}");
            st[0] = 0;
        }
    }
    fn decode(&self, st: &mut St, inp: &[u8], pos: &mut usize, out: &mut Vec<u32>) -> R {
        while *pos < inp.len() {
            let c = inp[*pos];
            if c == b'~' {
                need(inp, *pos, 2)?;
                let gb = st[0] != 0;
                match (inp[*pos + 1], gb) {
                    (b'~', false) => out.push(b'~' as u32),
                    (b'{', false) => st[0] = 1,
                    (b'\n', false) => {}
                    (b'}', true) => st[0] = 0,
                    _ => return Err(Fail::Illegal(1)),
                }
                *pos += 2;
                continue;
            }
            if c & 0x80 != 0 {
                return Err(Fail::Illegal(1));
            }
            if st[0] == 0 {
                out.push(c as u32);
                *pos += 1;
                continue;
            }
            need(inp, *pos, 2)?;
            let c2 = inp[*pos + 1];
            match if c2 & 0x80 == 0 { gb2312_get(c | 0x80, c2 | 0x80) } else { None } {
                Some(u) => {
                    out.push(u);
                    *pos += 2;
                }
                None => return Err(Fail::Illegal(1)),
            }
        }
        Ok(())
    }
}

pub static CN: &[CodecDef] = &[
    CodecDef { name: "gb2312", imp: &Gb2312 },
    CodecDef { name: "gbk", imp: &Gbk },
    CodecDef { name: "gb18030", imp: &Gb18030 },
    CodecDef { name: "hz", imp: &Hz },
];

// ================================================================== Japanese

/// JIS X 0208 by EUC bytes (0xA1C0 is the plain table's U+005C).
#[inline]
fn jisx0208_get(c1: u8, c2: u8) -> Option<u32> {
    JISX0208.get(c1, c2)
}

/// JIS X 0213 plane 1 by EUC bytes; `y2000` rejects the 2004 additions.
#[inline]
fn jisx0213_1_get(c1: u8, c2: u8, y2000: bool) -> Option<u32> {
    if y2000 && is_2004_addition(c1, c2) {
        return None;
    }
    JISX0208.get(c1, c2).or_else(|| JISX0213_1.get(c1, c2))
}

#[inline]
pub fn is_2004_addition(c1: u8, c2: u8) -> bool {
    let code = (c1 as u16) << 8 | c2 as u16;
    JISX0213_2004_ADDED.contains(&code)
}

#[inline]
pub fn jisx0213_2_get(c1: u8, c2: u8) -> Option<u32> {
    JISX0213_2.get(c1, c2)
}

lazy_rev!(jisx0208_rev, {
    let mut r = Rev::new();
    r.add_all(0xA1..=0xFE, |c| jisx0208_get((c >> 8) as u8, c as u8));
    r
});
lazy_rev!(jisx0212_rev, {
    let mut r = Rev::new();
    r.add_all(0xA1..=0xFE, |c| JISX0212.get16(c));
    r
});
lazy_rev!(pub jisx0213_1_rev, {
    let mut r = Rev::new();
    r.add_all(0xA1..=0xFE, |c| jisx0213_1_get((c >> 8) as u8, c as u8, false));
    r
});
lazy_rev!(pub jisx0213_2_rev, {
    let mut r = Rev::new();
    r.add_all(0xA1..=0xFE, |c| JISX0213_2.get16(c));
    r
});

/// JIS X 0213 encoder shared by the EUC, Shift_JIS and ISO-2022 forms:
/// returns (plane, EUC code, characters consumed).
pub enum X0213 {
    Found(u8, u16, usize),
    TooFew,
    None,
}

/// Look up `inp[pos]` (and a following combining character) in JIS X 0213.
/// `pairs_only`: only succeed for a two-character combination.
pub fn jisx0213_encode(inp: &[u32], pos: usize, flush: bool, y2000: bool, pairs_only: bool) -> X0213 {
    let c = inp[pos];
    let r1 = jisx0213_1_rev();
    if r1.is_pair_base(c) {
        if pos + 1 >= inp.len() {
            if !flush {
                return X0213::TooFew;
            }
        } else if let Some(code) = r1.pair(c, inp[pos + 1]) {
            return X0213::Found(1, code, 2);
        }
    }
    if pairs_only {
        return X0213::None;
    }
    if let Some(code) = r1.get(c) {
        if !(y2000 && is_2004_addition((code >> 8) as u8, code as u8)) {
            return X0213::Found(1, code, 1);
        }
        return X0213::None;
    }
    if let Some(code) = jisx0213_2_rev().get(c) {
        return X0213::Found(2, code, 1);
    }
    X0213::None
}

/// EUC-JP: JIS X 0208 + half-width katakana (0x8E) + JIS X 0212 (0x8F).
struct EucJp;
impl MbCodec for EucJp {
    fn encode(&self, _st: &mut St, inp: &[u32], pos: &mut usize, out: &mut Vec<u8>, _flush: bool) -> R {
        let r0208 = jisx0208_rev();
        while *pos < inp.len() {
            let c = inp[*pos];
            if c < 0x80 {
                out.push(c as u8);
            } else if c == 0xFF3C {
                put2(out, 0xA1C0);
            } else if let Some(code) = r0208.get(c) {
                put2(out, code);
            } else if let Some(b) = halfwidth_kana(c) {
                out.extend_from_slice(&[0x8E, b]);
            } else if let Some(code) = jisx0212_rev().get(c) {
                out.push(0x8F);
                put2(out, code);
            } else if c == 0xA5 {
                out.push(0x5C);
            } else if c == 0x203E {
                out.push(0x7E);
            } else {
                return Err(Fail::Illegal(1));
            }
            *pos += 1;
        }
        Ok(())
    }
    fn decode(&self, _st: &mut St, inp: &[u8], pos: &mut usize, out: &mut Vec<u32>) -> R {
        while *pos < inp.len() {
            let c = inp[*pos];
            if c < 0x80 {
                out.push(c as u32);
                *pos += 1;
                continue;
            }
            match c {
                0x8E => {
                    need(inp, *pos, 2)?;
                    let c2 = inp[*pos + 1];
                    if !(0xA1..=0xDF).contains(&c2) {
                        return Err(Fail::Illegal(1));
                    }
                    out.push(0xFEC0 + c2 as u32);
                    *pos += 2;
                }
                0x8F => {
                    need(inp, *pos, 3)?;
                    match JISX0212.get(inp[*pos + 1], inp[*pos + 2]) {
                        Some(u) => out.push(u),
                        None => return Err(Fail::Illegal(1)),
                    }
                    *pos += 3;
                }
                0xA1..=0xFE => {
                    need(inp, *pos, 2)?;
                    let c2 = inp[*pos + 1];
                    let u = if c == 0xA1 && c2 == 0xC0 { Some(0xFF3C) } else { jisx0208_get(c, c2) };
                    match u {
                        Some(u) => out.push(u),
                        None => return Err(Fail::Illegal(1)),
                    }
                    *pos += 2;
                }
                _ => return Err(Fail::Illegal(1)),
            }
        }
        Ok(())
    }
}

/// Shift_JIS: JIS X 0201 + JIS X 0208.
struct ShiftJis;
impl MbCodec for ShiftJis {
    fn encode(&self, _st: &mut St, inp: &[u32], pos: &mut usize, out: &mut Vec<u8>, _flush: bool) -> R {
        let r0208 = jisx0208_rev();
        while *pos < inp.len() {
            let c = inp[*pos];
            if c < 0x80 {
                out.push(c as u8);
            } else if c == 0xA5 {
                out.push(0x5C);
            } else if c == 0x203E {
                out.push(0x7E);
            } else if let Some(b) = halfwidth_kana(c) {
                out.push(b);
            } else if c == 0xFF3C {
                put2(out, 0x815F);
            } else if let Some(code) = r0208.get(c) {
                put2(out, euc_to_sjis((code >> 8) as u8, code as u8));
            } else {
                return Err(Fail::Illegal(1));
            }
            *pos += 1;
        }
        Ok(())
    }
    fn decode(&self, _st: &mut St, inp: &[u8], pos: &mut usize, out: &mut Vec<u32>) -> R {
        while *pos < inp.len() {
            let c = inp[*pos];
            if c < 0x80 {
                out.push(c as u32);
                *pos += 1;
                continue;
            }
            if (0xA1..=0xDF).contains(&c) {
                out.push(0xFEC0 + c as u32);
                *pos += 1;
                continue;
            }
            if !matches!(c, 0x81..=0x9F | 0xE0..=0xFC) {
                return Err(Fail::Illegal(1));
            }
            need(inp, *pos, 2)?;
            let c2 = inp[*pos + 1];
            let u = match sjis_to_euc(c, c2) {
                Some((0xA1, 0xC0)) => Some(0xFF3C),
                Some((r, k)) => jisx0208_get(r, k),
                None => None,
            };
            match u {
                Some(u) => out.push(u),
                None => return Err(Fail::Illegal(1)),
            }
            *pos += 2;
        }
        Ok(())
    }
}

fn cp932_get(c1: u8, c2: u8) -> Option<u32> {
    if let Some(u) = CP932EXT.get(c1, c2) {
        return Some(u);
    }
    if let Some((r, k)) = sjis_to_euc(c1, c2) {
        if let Some(u) = jisx0208_get(r, k) {
            return Some(u);
        }
    }
    if (0xF0..=0xF9).contains(&c1) && matches!(c2, 0x40..=0x7E | 0x80..=0xFC) {
        let col = (c2 - 0x40 - (c2 >= 0x80) as u8) as u32;
        return Some(0xE000 + (c1 as u32 - 0xF0) * 188 + col);
    }
    None
}

lazy_rev!(cp932_rev, {
    let mut r = Rev::new();
    r.add_all(0x81..=0xFC, |c| cp932_get((c >> 8) as u8, c as u8));
    r
});

/// Microsoft's Shift_JIS (code page 932).
struct Cp932;
impl MbCodec for Cp932 {
    fn encode(&self, _st: &mut St, inp: &[u32], pos: &mut usize, out: &mut Vec<u8>, _flush: bool) -> R {
        let rev = cp932_rev();
        while *pos < inp.len() {
            let c = inp[*pos];
            if c <= 0x80 {
                out.push(c as u8);
            } else if let Some(b) = halfwidth_kana(c) {
                out.push(b);
            } else if c == 0xF8F0 {
                out.push(0xA0);
            } else if (0xF8F1..=0xF8F3).contains(&c) {
                out.push((c - 0xF8F1 + 0xFD) as u8);
            } else if let Some(code) = rev.get(c) {
                put2(out, code);
            } else if let Some(code) = jisx0208_rev().get(c) {
                // JIS mappings that CP932 replaced by its own are still accepted.
                put2(out, euc_to_sjis((code >> 8) as u8, code as u8));
            } else {
                return Err(Fail::Illegal(1));
            }
            *pos += 1;
        }
        Ok(())
    }
    fn decode(&self, _st: &mut St, inp: &[u8], pos: &mut usize, out: &mut Vec<u32>) -> R {
        while *pos < inp.len() {
            let c = inp[*pos];
            let single = match c {
                0..=0x80 => Some(c as u32),
                0xA0 => Some(0xF8F0),
                0xA1..=0xDF => Some(0xFEC0 + c as u32),
                0xFD..=0xFF => Some(0xF8F1 + (c - 0xFD) as u32),
                _ => None,
            };
            if let Some(u) = single {
                out.push(u);
                *pos += 1;
                continue;
            }
            need(inp, *pos, 2)?;
            match cp932_get(c, inp[*pos + 1]) {
                Some(u) => out.push(u),
                None => return Err(Fail::Illegal(1)),
            }
            *pos += 2;
        }
        Ok(())
    }
}

/// EUC-JIS-2004 / EUC-JISX0213.
struct EucJis2004 {
    y2000: bool,
}
impl MbCodec for EucJis2004 {
    fn encode(&self, _st: &mut St, inp: &[u32], pos: &mut usize, out: &mut Vec<u8>, flush: bool) -> R {
        while *pos < inp.len() {
            let c = inp[*pos];
            if c < 0x80 {
                out.push(c as u8);
                *pos += 1;
                continue;
            }
            if let Some(b) = halfwidth_kana(c) {
                out.extend_from_slice(&[0x8E, b]);
                *pos += 1;
                continue;
            }
            if c == 0xFF3C {
                put2(out, 0xA1C0);
                *pos += 1;
                continue;
            }
            match jisx0213_encode(inp, *pos, flush, self.y2000, false) {
                X0213::Found(plane, code, n) => {
                    if plane == 2 {
                        out.push(0x8F);
                    }
                    put2(out, code);
                    *pos += n;
                }
                X0213::TooFew => return Err(Fail::TooFew),
                X0213::None => return Err(Fail::Illegal(1)),
            }
        }
        Ok(())
    }
    fn decode(&self, _st: &mut St, inp: &[u8], pos: &mut usize, out: &mut Vec<u32>) -> R {
        while *pos < inp.len() {
            let c = inp[*pos];
            if c < 0x80 {
                out.push(c as u32);
                *pos += 1;
                continue;
            }
            match c {
                0x8E => {
                    need(inp, *pos, 2)?;
                    let c2 = inp[*pos + 1];
                    if !(0xA1..=0xDF).contains(&c2) {
                        return Err(Fail::Illegal(1));
                    }
                    out.push(0xFEC0 + c2 as u32);
                    *pos += 2;
                }
                0x8F => {
                    need(inp, *pos, 3)?;
                    match jisx0213_2_get(inp[*pos + 1], inp[*pos + 2]) {
                        Some(u) => push_value(out, u),
                        None => return Err(Fail::Illegal(1)),
                    }
                    *pos += 3;
                }
                0xA1..=0xFE => {
                    need(inp, *pos, 2)?;
                    let c2 = inp[*pos + 1];
                    let u = if c == 0xA1 && c2 == 0xC0 { Some(0xFF3C) } else { jisx0213_1_get(c, c2, self.y2000) };
                    match u {
                        Some(u) => push_value(out, u),
                        None => return Err(Fail::Illegal(1)),
                    }
                    *pos += 2;
                }
                _ => return Err(Fail::Illegal(1)),
            }
        }
        Ok(())
    }
}

/// Shift_JIS-2004 / Shift_JISX0213 (JIS X 0201 Roman in the single bytes).
struct ShiftJis2004 {
    y2000: bool,
}
impl MbCodec for ShiftJis2004 {
    fn encode(&self, _st: &mut St, inp: &[u32], pos: &mut usize, out: &mut Vec<u8>, flush: bool) -> R {
        while *pos < inp.len() {
            let c = inp[*pos];
            let single = match c {
                0x5C | 0x7E => None,
                0..=0x7F => Some(c as u8),
                0xA5 => Some(0x5C),
                0x203E => Some(0x7E),
                _ => halfwidth_kana(c),
            };
            if let Some(b) = single {
                out.push(b);
                *pos += 1;
                continue;
            }
            match jisx0213_encode(inp, *pos, flush, self.y2000, false) {
                X0213::Found(plane, code, n) => {
                    let (r, k) = ((code >> 8) as u8, code as u8);
                    let sj = if plane == 1 { Some(euc_to_sjis(r, k)) } else { euc_plane2_to_sjis(r, k) };
                    match sj {
                        Some(sj) => put2(out, sj),
                        None => return Err(Fail::Illegal(1)),
                    }
                    *pos += n;
                }
                X0213::TooFew => return Err(Fail::TooFew),
                X0213::None => return Err(Fail::Illegal(1)),
            }
        }
        Ok(())
    }
    fn decode(&self, _st: &mut St, inp: &[u8], pos: &mut usize, out: &mut Vec<u32>) -> R {
        while *pos < inp.len() {
            let c = inp[*pos];
            let single = match c {
                0x5C => Some(0xA5),
                0x7E => Some(0x203E),
                0..=0x7F => Some(c as u32),
                0xA1..=0xDF => Some(0xFEC0 + c as u32),
                _ => None,
            };
            if let Some(u) = single {
                out.push(u);
                *pos += 1;
                continue;
            }
            if !matches!(c, 0x81..=0x9F | 0xE0..=0xFC) {
                return Err(Fail::Illegal(1));
            }
            need(inp, *pos, 2)?;
            let c2 = inp[*pos + 1];
            let u = if c >= 0xF0 {
                sjis_plane2_to_euc(c, c2).and_then(|(r, k)| jisx0213_2_get(r, k))
            } else {
                sjis_to_euc(c, c2).and_then(|(r, k)| jisx0213_1_get(r, k, self.y2000))
            };
            match u {
                Some(u) => push_value(out, u),
                None => return Err(Fail::Illegal(1)),
            }
            *pos += 2;
        }
        Ok(())
    }
}

pub static JP: &[CodecDef] = &[
    CodecDef { name: "shift_jis", imp: &ShiftJis },
    CodecDef { name: "cp932", imp: &Cp932 },
    CodecDef { name: "euc_jp", imp: &EucJp },
    CodecDef { name: "shift_jis_2004", imp: &ShiftJis2004 { y2000: false } },
    CodecDef { name: "shift_jisx0213", imp: &ShiftJis2004 { y2000: true } },
    CodecDef { name: "euc_jis_2004", imp: &EucJis2004 { y2000: false } },
    CodecDef { name: "euc_jisx0213", imp: &EucJis2004 { y2000: true } },
];

// =================================================================== Korean

fn ksx1001_get(c1: u8, c2: u8) -> Option<u32> {
    KSX1001.get(c1, c2)
}

lazy_rev!(ksx1001_rev, {
    let mut r = Rev::new();
    r.add_all(0xA1..=0xFE, |c| ksx1001_get((c >> 8) as u8, c as u8));
    r
});

fn cp949_get(c1: u8, c2: u8) -> Option<u32> {
    if high(c1, c2) { KSX1001.get(c1, c2) } else { None }.or_else(|| CP949EXT.get(c1, c2))
}

lazy_rev!(cp949_rev, {
    let mut r = Rev::new();
    r.add_all(0x81..=0xFE, |c| cp949_get((c >> 8) as u8, c as u8));
    r
});

/// Compatibility-jamo offset (from U+3131 / 0xA4A1) -> leading consonant index.
const JAMO_INITIAL: [i8; 30] = [0, 1, -1, 2, -1, -1, 3, 4, 5, -1, -1, -1, -1, -1, -1, -1, 6, 7, 8, -1, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18];
/// Compatibility-jamo offset -> trailing consonant index (1-based; 0 = none).
const JAMO_FINAL: [i8; 30] = [1, 2, 3, 4, 5, 6, 7, -1, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, -1, 18, 19, 20, 21, 22, -1, 23, 24, 25, 26, 27];

fn jamo_offset(table: &[i8; 30], idx: i8) -> u8 {
    table.iter().position(|&v| v == idx).unwrap() as u8
}

const FILLER: u16 = 0xA4D4;

/// EUC-KR (KS X 1001, with the KS X 1001:1998 Annex 3 eight-byte make-up
/// sequences for the syllables outside the 2350-syllable set).
struct EucKr;
impl MbCodec for EucKr {
    fn encode(&self, _st: &mut St, inp: &[u32], pos: &mut usize, out: &mut Vec<u8>, _flush: bool) -> R {
        let rev = ksx1001_rev();
        while *pos < inp.len() {
            let c = inp[*pos];
            if c < 0x80 {
                out.push(c as u8);
            } else if let Some(code) = rev.get(c) {
                put2(out, code);
            } else if (0xAC00..=0xD7A3).contains(&c) {
                let s = c - 0xAC00;
                let (i, m, f) = ((s / 588) as i8, (s / 28 % 21) as u8, (s % 28) as i8);
                put2(out, FILLER);
                put2(out, 0xA4A1 + jamo_offset(&JAMO_INITIAL, i) as u16);
                put2(out, 0xA4BF + m as u16);
                put2(out, if f == 0 { FILLER } else { 0xA4A1 + jamo_offset(&JAMO_FINAL, f) as u16 });
            } else {
                return Err(Fail::Illegal(1));
            }
            *pos += 1;
        }
        Ok(())
    }
    fn decode(&self, _st: &mut St, inp: &[u8], pos: &mut usize, out: &mut Vec<u32>) -> R {
        while *pos < inp.len() {
            let c = inp[*pos];
            if c < 0x80 {
                out.push(c as u32);
                *pos += 1;
                continue;
            }
            if !(0xA1..=0xFE).contains(&c) {
                return Err(Fail::Illegal(1));
            }
            need(inp, *pos, 2)?;
            let c2 = inp[*pos + 1];
            if (c as u16) << 8 | c2 as u16 == FILLER {
                need(inp, *pos, 8)?;
                let b = &inp[*pos..*pos + 8];
                if b[2] != 0xA4 || b[4] != 0xA4 || b[6] != 0xA4 {
                    return Err(Fail::Illegal(1));
                }
                let jamo = |x: u8, lo: u8, hi: u8| if (lo..=hi).contains(&x) { Some((x - lo) as usize) } else { None };
                let i = jamo(b[3], 0xA1, 0xBE).map(|k| JAMO_INITIAL[k]).filter(|v| *v >= 0);
                let m = jamo(b[5], 0xBF, 0xD3);
                let f = if b[7] == 0xD4 { Some(0) } else { jamo(b[7], 0xA1, 0xBE).map(|k| JAMO_FINAL[k]).filter(|v| *v >= 0) };
                match (i, m, f) {
                    (Some(i), Some(m), Some(f)) => {
                        out.push(0xAC00 + (i as u32 * 21 + m as u32) * 28 + f as u32);
                        *pos += 8;
                    }
                    _ => return Err(Fail::Illegal(1)),
                }
                continue;
            }
            match ksx1001_get(c, c2) {
                Some(u) => out.push(u),
                None => return Err(Fail::Illegal(1)),
            }
            *pos += 2;
        }
        Ok(())
    }
}

struct Cp949;
impl MbCodec for Cp949 {
    fn encode(&self, _st: &mut St, inp: &[u32], pos: &mut usize, out: &mut Vec<u8>, _flush: bool) -> R {
        let rev = cp949_rev();
        simple_encode(inp, pos, out, |c| rev.get(c))
    }
    fn decode(&self, _st: &mut St, inp: &[u8], pos: &mut usize, out: &mut Vec<u32>) -> R {
        simple_decode(inp, pos, out, |c| (0x81..=0xFE).contains(&c), cp949_get)
    }
}

/// Johab medial field value -> vowel index (-1 = invalid).
const JOHAB_MEDIAL: [i8; 32] = [
    -1, -1, -1, 0, 1, 2, 3, 4, -1, -1, 5, 6, 7, 8, 9, 10, -1, -1, 11, 12, 13, 14, 15, 16, -1, -1, 17, 18, 19, 20, -1, -1,
];

fn johab_hangul(code: u16) -> Option<u32> {
    let i = (code >> 10 & 31) as u32;
    let m = JOHAB_MEDIAL[(code >> 5 & 31) as usize];
    let f = (code & 31) as u32;
    if (2..=20).contains(&i) && m >= 0 {
        let fi = match f {
            1..=17 => f - 1,
            19..=29 => f - 2,
            _ => return None,
        };
        return Some(0xAC00 + ((i - 2) * 21 + m as u32) * 28 + fi);
    }
    JOHAB_JAMO.binary_search_by_key(&code, |e| e.0).ok().map(|k| JOHAB_JAMO[k].1 as u32)
}

/// Johab symbol / hanja code -> KS X 1001 EUC bytes.
fn johab_to_ksx(c1: u8, c2: u8) -> Option<(u8, u8)> {
    let first_row = match c1 {
        0xD9..=0xDE => (c1 - 0xD9) * 2 + 1,
        0xE0..=0xF9 => (c1 - 0xE0) * 2 + 42,
        _ => return None,
    };
    let (row, col) = match c2 {
        0x31..=0x7E => (first_row, c2 - 0x30),
        0x91..=0xA0 => (first_row, c2 - 0x90 + 78),
        0xA1..=0xFE => (first_row + 1, c2 - 0xA0),
        _ => return None,
    };
    if row == 4 && col <= 0x33 {
        // modern jamo are only encoded in the hangul area
        return None;
    }
    Some((row + 0xA0, col + 0xA0))
}

fn ksx_to_johab(r: u8, c: u8) -> Option<u16> {
    let row = r - 0xA0;
    let col = c - 0xA0;
    let (lead, first) = match row {
        1..=12 => (0xD9 + (row - 1) / 2, row % 2 == 1),
        42..=93 => (0xE0 + (row - 42) / 2, row % 2 == 0),
        _ => return None,
    };
    let trail = if first {
        if col <= 78 { 0x30 + col } else { 0x90 + (col - 78) }
    } else {
        0xA0 + col
    };
    Some((lead as u16) << 8 | trail as u16)
}

lazy_rev!(johab_jamo_rev, {
    let mut r = Rev::new();
    for (code, u) in JOHAB_JAMO {
        r.add(*u as u32, *code);
    }
    r
});

struct Johab;
impl MbCodec for Johab {
    fn encode(&self, _st: &mut St, inp: &[u32], pos: &mut usize, out: &mut Vec<u8>, _flush: bool) -> R {
        while *pos < inp.len() {
            let c = inp[*pos];
            if c < 0x80 {
                out.push(c as u8);
            } else if (0xAC00..=0xD7A3).contains(&c) {
                let s = c - 0xAC00;
                let i = s / 588 + 2;
                let m = JOHAB_MEDIAL.iter().position(|&v| v as u32 == s / 28 % 21).unwrap() as u32;
                let f = s % 28;
                let f = if f == 0 { 1 } else if f <= 16 { f + 1 } else { f + 2 };
                put2(out, (0x8000 | i << 10 | m << 5 | f) as u16);
            } else if let Some(code) = johab_jamo_rev().get(c) {
                put2(out, code);
            } else if let Some(j) = ksx1001_rev().get(c).and_then(|k| ksx_to_johab((k >> 8) as u8, k as u8)) {
                put2(out, j);
            } else {
                return Err(Fail::Illegal(1));
            }
            *pos += 1;
        }
        Ok(())
    }
    fn decode(&self, _st: &mut St, inp: &[u8], pos: &mut usize, out: &mut Vec<u32>) -> R {
        while *pos < inp.len() {
            let c = inp[*pos];
            if c < 0x80 {
                out.push(c as u32);
                *pos += 1;
                continue;
            }
            need(inp, *pos, 2)?;
            let c2 = inp[*pos + 1];
            let u = match c {
                0x84..=0xD3 => johab_hangul((c as u16) << 8 | c2 as u16),
                0xD8..=0xF9 => johab_to_ksx(c, c2).and_then(|(r, k)| ksx1001_get(r, k)),
                _ => None,
            };
            match u {
                Some(u) => out.push(u),
                None => return Err(Fail::Illegal(1)),
            }
            *pos += 2;
        }
        Ok(())
    }
}

pub static KR: &[CodecDef] = &[
    CodecDef { name: "euc_kr", imp: &EucKr },
    CodecDef { name: "cp949", imp: &Cp949 },
    CodecDef { name: "johab", imp: &Johab },
];

// ====================================================== Chinese (traditional)

/// Big5 codes whose reference table entry is undefined but which decode
/// like CP950 (decoding only).
const BIG5_DECODE_ONLY: [(u16, u32); 7] =
    [(0xA15A, 0x2574), (0xA1C3, 0xFFE3), (0xA1C5, 0x02CD), (0xA1FE, 0xFF0F), (0xA240, 0xFF3C), (0xA2CC, 0x5341), (0xA2CE, 0x5345)];

fn big5_get(c1: u8, c2: u8) -> Option<u32> {
    BIG5.get(c1, c2).or_else(|| {
        let code = (c1 as u16) << 8 | c2 as u16;
        BIG5_DECODE_ONLY.iter().find(|e| e.0 == code).map(|e| e.1)
    })
}

fn cp950_get(c1: u8, c2: u8) -> Option<u32> {
    CP950EXT.get(c1, c2).or_else(|| BIG5.get(c1, c2))
}

lazy_rev!(big5_rev, {
    let mut r = Rev::new();
    r.add_all(0xA1..=0xFE, |c| BIG5.get16(c));
    r
});
lazy_rev!(cp950_rev, {
    let mut r = Rev::new();
    r.add_all(0x81..=0xFE, |c| cp950_get((c >> 8) as u8, c as u8));
    r.drop_if(0x5341, 0xA2CC).drop_if(0x5345, 0xA2CE);
    r.add(0x5341, 0xA451);
    r.add(0x5345, 0xA4CA);
    r
});

struct Big5;
impl MbCodec for Big5 {
    fn encode(&self, _st: &mut St, inp: &[u32], pos: &mut usize, out: &mut Vec<u8>, _flush: bool) -> R {
        let rev = big5_rev();
        simple_encode(inp, pos, out, |c| rev.get(c))
    }
    fn decode(&self, _st: &mut St, inp: &[u8], pos: &mut usize, out: &mut Vec<u32>) -> R {
        simple_decode(inp, pos, out, |c| (0x81..=0xFE).contains(&c), big5_get)
    }
}

struct Cp950;
impl MbCodec for Cp950 {
    fn encode(&self, _st: &mut St, inp: &[u32], pos: &mut usize, out: &mut Vec<u8>, _flush: bool) -> R {
        let rev = cp950_rev();
        simple_encode(inp, pos, out, |c| rev.get(c))
    }
    fn decode(&self, _st: &mut St, inp: &[u8], pos: &mut usize, out: &mut Vec<u32>) -> R {
        simple_decode(inp, pos, out, |c| (0x81..=0xFE).contains(&c), cp950_get)
    }
}

pub static TW: &[CodecDef] = &[CodecDef { name: "big5", imp: &Big5 }, CodecDef { name: "cp950", imp: &Cp950 }];

// ================================================================ Hong Kong

/// The HKSCS block C6A1-C8FE replaces Big5's own assignments there.
fn in_hkscs_block(c1: u8, c2: u8) -> bool {
    (0xC6..=0xC8).contains(&c1) && (c1 != 0xC6 || c2 >= 0xA1)
}

fn big5hkscs_get(c1: u8, c2: u8) -> Option<u32> {
    if !in_hkscs_block(c1, c2) {
        if let Some(u) = big5_get(c1, c2) {
            return Some(u);
        }
    }
    HKSCS.get(c1, c2)
}

lazy_rev!(hkscs_rev, {
    let mut r = Rev::new();
    r.add_all(0x87..=0xFE, |c| HKSCS.get16(c));
    r
});

struct Big5Hkscs;
impl MbCodec for Big5Hkscs {
    fn encode(&self, _st: &mut St, inp: &[u32], pos: &mut usize, out: &mut Vec<u8>, flush: bool) -> R {
        let hk = hkscs_rev();
        let b5 = big5_rev();
        while *pos < inp.len() {
            let c = inp[*pos];
            if c < 0x80 {
                out.push(c as u8);
                *pos += 1;
                continue;
            }
            if hk.is_pair_base(c) {
                if *pos + 1 >= inp.len() {
                    if !flush {
                        return Err(Fail::TooFew);
                    }
                } else if let Some(code) = hk.pair(c, inp[*pos + 1]) {
                    put2(out, code);
                    *pos += 2;
                    continue;
                }
            }
            let code = hk.get(c).or_else(|| b5.get(c).filter(|k| !in_hkscs_block((k >> 8) as u8, *k as u8)));
            match code {
                Some(code) => put2(out, code),
                None => return Err(Fail::Illegal(1)),
            }
            *pos += 1;
        }
        Ok(())
    }
    fn decode(&self, _st: &mut St, inp: &[u8], pos: &mut usize, out: &mut Vec<u32>) -> R {
        simple_decode(inp, pos, out, |c| (0x81..=0xFE).contains(&c), big5hkscs_get)
    }
}

pub static HK: &[CodecDef] = &[CodecDef { name: "big5hkscs", imp: &Big5Hkscs }];

// Shared with the ISO-2022 codecs.
pub fn gb2312_encode(c: u32) -> Option<u16> {
    gb2312_rev().get(c)
}
pub fn ksx1001_encode(c: u32) -> Option<u16> {
    ksx1001_rev().get(c)
}
pub fn ksx1001_decode(c1: u8, c2: u8) -> Option<u32> {
    ksx1001_get(c1, c2)
}
pub fn gb2312_decode(c1: u8, c2: u8) -> Option<u32> {
    gb2312_get(c1, c2)
}
pub fn jisx0208_encode(c: u32) -> Option<u16> {
    if c == 0xFF3C { Some(0xA1C0) } else { jisx0208_rev().get(c).filter(|&k| k != 0xA1C0) }
}
pub fn jisx0208_decode(c1: u8, c2: u8) -> Option<u32> {
    if c1 == 0xA1 && c2 == 0xC0 { Some(0xFF3C) } else { jisx0208_get(c1, c2) }
}
pub fn jisx0212_encode(c: u32) -> Option<u16> {
    jisx0212_rev().get(c)
}
pub fn jisx0212_decode(c1: u8, c2: u8) -> Option<u32> {
    JISX0212.get(c1, c2)
}
pub fn jisx0213_1_decode(c1: u8, c2: u8, y2000: bool) -> Option<u32> {
    if c1 == 0xA1 && c2 == 0xC0 { Some(0xFF3C) } else { jisx0213_1_get(c1, c2, y2000) }
}
