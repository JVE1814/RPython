//! Built-in codec algorithms (used by `_codecs` and by `str.encode`/`bytes.decode`).

use crate::errors::*;
use crate::modules::_codecs::{DecCtx, EncCtx, Rep};
use crate::modules::text_util::{self as tu, StrBuf};
use crate::object::*;
use crate::types::exceptions as exc;
use crate::types::str::{self as pystr, StrData};
use crate::ffi::PyObject;

#[inline]
pub fn is_surrogate(c: u32) -> bool {
    (0xD800..0xE000).contains(&c)
}

// =====================================================================================
// Encoders (str -> bytes)
// =====================================================================================

/// Append a replacement produced by an error handler; `put(c, out)` writes a
/// replacement character or returns false if it cannot be encoded.
#[allow(clippy::too_many_arguments)]
pub fn put_rep(
    ctx: &mut EncCtx,
    rep: Rep,
    out: &mut Vec<u8>,
    start: usize,
    end: usize,
    reason: &str,
    unit: usize,
    mut put: impl FnMut(u32, &mut Vec<u8>) -> bool,
) -> PyResult<()> {
    match rep {
        Rep::Bytes(b) => {
            if unit > 1 && b.len() % unit != 0 {
                return Err(ctx.raise(start, end, reason));
            }
            out.extend_from_slice(&b);
        }
        Rep::Str(s) => {
            for c in s {
                if !put(c, out) {
                    return Err(ctx.raise(start, end, reason));
                }
            }
        }
    }
    Ok(())
}

#[inline]
fn push_utf8(c: u32, out: &mut Vec<u8>) {
    if c < 0x80 {
        out.push(c as u8);
    } else if c < 0x800 {
        out.push((0xC0 | (c >> 6)) as u8);
        out.push((0x80 | (c & 0x3F)) as u8);
    } else if c < 0x10000 {
        out.push((0xE0 | (c >> 12)) as u8);
        out.push((0x80 | ((c >> 6) & 0x3F)) as u8);
        out.push((0x80 | (c & 0x3F)) as u8);
    } else {
        out.push((0xF0 | (c >> 18)) as u8);
        out.push((0x80 | ((c >> 12) & 0x3F)) as u8);
        out.push((0x80 | ((c >> 6) & 0x3F)) as u8);
        out.push((0x80 | (c & 0x3F)) as u8);
    }
}

pub fn encode_utf8(ctx: &mut EncCtx) -> PyResult<Vec<u8>> {
    let obj = ctx.obj;
    let d = pystr::str_data(obj);
    if let StrData::L1(b) = d {
        if pystr::str_is_ascii(obj) {
            return Ok(b.to_vec());
        }
        let mut out = Vec::with_capacity(b.len() * 2);
        for &c in b {
            push_utf8(c as u32, &mut out);
        }
        return Ok(out);
    }
    let n = d.len();
    let mut out = Vec::with_capacity(n * 3);
    let mut i = 0;
    while i < n {
        let c = d.get(i);
        if !is_surrogate(c) {
            push_utf8(c, &mut out);
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < n && is_surrogate(d.get(j)) {
            j += 1;
        }
        let (rep, np) = ctx.handle(i, j, "surrogates not allowed")?;
        put_rep(ctx, rep, &mut out, i, j, "surrogates not allowed", 1, |c, o| {
            if c < 0x80 {
                o.push(c as u8);
                true
            } else {
                false
            }
        })?;
        i = np;
    }
    Ok(out)
}

/// Latin-1 (limit 256) and ASCII (limit 128).
pub fn encode_charlimit(ctx: &mut EncCtx, limit: u32) -> PyResult<Vec<u8>> {
    let obj = ctx.obj;
    let d = pystr::str_data(obj);
    if let StrData::L1(b) = d {
        if limit == 256 || pystr::str_is_ascii(obj) {
            return Ok(b.to_vec());
        }
    }
    let reason = if limit == 128 { "ordinal not in range(128)" } else { "ordinal not in range(256)" };
    let n = d.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        let c = d.get(i);
        if c < limit {
            out.push(c as u8);
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < n && d.get(j) >= limit {
            j += 1;
        }
        let (rep, np) = ctx.handle(i, j, reason)?;
        put_rep(ctx, rep, &mut out, i, j, reason, 1, |c, o| {
            if c < limit {
                o.push(c as u8);
                true
            } else {
                false
            }
        })?;
        i = np;
    }
    Ok(out)
}

/// UTF-16; `bo`: 0 = native order with BOM, -1 little endian, 1 big endian.
pub fn encode_utf16(ctx: &mut EncCtx, bo: i32) -> PyResult<Vec<u8>> {
    let obj = ctx.obj;
    let d = pystr::str_data(obj);
    let n = d.len();
    let mut out = Vec::with_capacity(n * 2 + 2);
    let le = if bo == 0 { cfg!(target_endian = "little") } else { bo < 0 };
    let put16 = move |u: u16, o: &mut Vec<u8>| {
        if le {
            o.extend_from_slice(&u.to_le_bytes())
        } else {
            o.extend_from_slice(&u.to_be_bytes())
        }
    };
    if bo == 0 {
        put16(0xFEFF, &mut out);
    }
    let mut i = 0;
    while i < n {
        let c = d.get(i);
        if c < 0x10000 && !is_surrogate(c) {
            put16(c as u16, &mut out);
        } else if c >= 0x10000 {
            let v = c - 0x10000;
            put16((0xD800 | (v >> 10)) as u16, &mut out);
            put16((0xDC00 | (v & 0x3FF)) as u16, &mut out);
        } else {
            let mut j = i + 1;
            while j < n && is_surrogate(d.get(j)) {
                j += 1;
            }
            let (rep, np) = ctx.handle(i, j, "surrogates not allowed")?;
            put_rep(ctx, rep, &mut out, i, j, "surrogates not allowed", 2, |c, o| {
                if c < 0x80 {
                    put16(c as u16, o);
                    true
                } else {
                    false
                }
            })?;
            i = np;
            continue;
        }
        i += 1;
    }
    Ok(out)
}

pub fn encode_utf32(ctx: &mut EncCtx, bo: i32) -> PyResult<Vec<u8>> {
    let obj = ctx.obj;
    let d = pystr::str_data(obj);
    let n = d.len();
    let mut out = Vec::with_capacity(n * 4 + 4);
    let le = if bo == 0 { cfg!(target_endian = "little") } else { bo < 0 };
    let put32 = move |u: u32, o: &mut Vec<u8>| {
        if le {
            o.extend_from_slice(&u.to_le_bytes())
        } else {
            o.extend_from_slice(&u.to_be_bytes())
        }
    };
    if bo == 0 {
        put32(0xFEFF, &mut out);
    }
    let mut i = 0;
    while i < n {
        let c = d.get(i);
        if !is_surrogate(c) {
            put32(c, &mut out);
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < n && is_surrogate(d.get(j)) {
            j += 1;
        }
        let (rep, np) = ctx.handle(i, j, "surrogates not allowed")?;
        put_rep(ctx, rep, &mut out, i, j, "surrogates not allowed", 4, |c, o| {
            if c < 0x80 {
                put32(c, o);
                true
            } else {
                false
            }
        })?;
        i = np;
    }
    Ok(out)
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// 0: set D, 1: set O, 2: whitespace, 3: everything else.
fn utf7_category(c: u32) -> u8 {
    match c {
        0x41..=0x5A | 0x61..=0x7A | 0x30..=0x39 => 0,
        0x27 | 0x28 | 0x29 | 0x2C | 0x2D | 0x2E | 0x2F | 0x3A | 0x3F => 0,
        0x20 | 0x09 | 0x0A | 0x0D => 2,
        0x21..=0x26 | 0x2A | 0x3B..=0x3E | 0x40 | 0x5B | 0x5D..=0x60 | 0x7B..=0x7D => 1,
        _ => 3,
    }
}
fn utf7_encode_direct(c: u32) -> bool {
    c > 0 && c < 128 && utf7_category(c) != 3
}
fn is_b64(c: u32) -> bool {
    c < 128 && (c as u8 == b'+' || c as u8 == b'/' || (c as u8).is_ascii_alphanumeric())
}
fn from_b64(c: u8) -> u64 {
    match c {
        b'A'..=b'Z' => (c - b'A') as u64,
        b'a'..=b'z' => (c - b'a' + 26) as u64,
        b'0'..=b'9' => (c - b'0' + 52) as u64,
        b'+' => 62,
        _ => 63,
    }
}

pub fn encode_utf7(obj: &PyObject) -> Vec<u8> {
    let d = pystr::str_data(obj);
    let mut out = Vec::with_capacity(d.len());
    let mut in_shift = false;
    let mut bits: u32 = 0;
    let mut buf: u64 = 0;
    fn push_unit(u: u32, out: &mut Vec<u8>, bits: &mut u32, buf: &mut u64) {
        *bits += 16;
        *buf = (*buf << 16) | u as u64;
        while *bits >= 6 {
            out.push(B64[((*buf >> (*bits - 6)) & 0x3F) as usize]);
            *bits -= 6;
        }
        *buf &= (1u64 << *bits) - 1;
    }
    for i in 0..d.len() {
        let c = d.get(i);
        if in_shift {
            if utf7_encode_direct(c) {
                if bits > 0 {
                    out.push(B64[((buf << (6 - bits)) & 0x3F) as usize]);
                    buf = 0;
                    bits = 0;
                }
                in_shift = false;
                if is_b64(c) || c == b'-' as u32 {
                    out.push(b'-');
                }
                out.push(c as u8);
                continue;
            }
        } else if c == b'+' as u32 {
            out.extend_from_slice(b"+-");
            continue;
        } else if utf7_encode_direct(c) {
            out.push(c as u8);
            continue;
        } else {
            out.push(b'+');
            in_shift = true;
        }
        if c >= 0x10000 {
            let v = c - 0x10000;
            push_unit(0xD800 | (v >> 10), &mut out, &mut bits, &mut buf);
            push_unit(0xDC00 | (v & 0x3FF), &mut out, &mut bits, &mut buf);
        } else {
            push_unit(c, &mut out, &mut bits, &mut buf);
        }
    }
    if bits > 0 {
        out.push(B64[((buf << (6 - bits)) & 0x3F) as usize]);
    }
    if in_shift {
        out.push(b'-');
    }
    out
}

fn hexdig(c: u8) -> Option<u32> {
    (c as char).to_digit(16)
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn push_hex(out: &mut Vec<u8>, c: u32, digits: u32) {
    for k in (0..digits).rev() {
        out.push(HEX[((c >> (4 * k)) & 0xF) as usize]);
    }
}

pub fn encode_unicode_escape(obj: &PyObject) -> Vec<u8> {
    let d = pystr::str_data(obj);
    let mut out = Vec::with_capacity(d.len() + 2);
    for i in 0..d.len() {
        let c = d.get(i);
        if c >= 0x10000 {
            out.extend_from_slice(b"\\U");
            push_hex(&mut out, c, 8);
        } else if c >= 0x100 {
            out.extend_from_slice(b"\\u");
            push_hex(&mut out, c, 4);
        } else if c == b'\\' as u32 {
            out.extend_from_slice(b"\\\\");
        } else if c == 9 {
            out.extend_from_slice(b"\\t");
        } else if c == 10 {
            out.extend_from_slice(b"\\n");
        } else if c == 13 {
            out.extend_from_slice(b"\\r");
        } else if !(0x20..0x7F).contains(&c) {
            out.extend_from_slice(b"\\x");
            push_hex(&mut out, c, 2);
        } else {
            out.push(c as u8);
        }
    }
    out
}

pub fn encode_raw_unicode_escape(obj: &PyObject) -> Vec<u8> {
    let d = pystr::str_data(obj);
    if let StrData::L1(b) = d {
        return b.to_vec();
    }
    let mut out = Vec::with_capacity(d.len());
    for i in 0..d.len() {
        let c = d.get(i);
        if c >= 0x10000 {
            out.extend_from_slice(b"\\U");
            push_hex(&mut out, c, 8);
        } else if c >= 0x100 {
            out.extend_from_slice(b"\\u");
            push_hex(&mut out, c, 4);
        } else {
            out.push(c as u8);
        }
    }
    out
}

pub fn encode_escape(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for &c in data {
        match c {
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\'' => out.extend_from_slice(b"\\'"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            0x20..=0x7E => out.push(c),
            _ => {
                out.extend_from_slice(b"\\x");
                push_hex(&mut out, c as u32, 2);
            }
        }
    }
    out
}

// =====================================================================================
// Decoders (bytes -> str)
// =====================================================================================

/// Result of a decoding scan step.
pub enum Step {
    Done(usize),
    Error(usize, usize, &'static str),
}

fn utf8_scan(data: &[u8], mut i: usize, final_: bool, out: &mut StrBuf) -> Step {
    let n = data.len();
    let cont = |k: usize| k < n && (data[k] & 0xC0) == 0x80;
    while i < n {
        let c = data[i];
        if c < 0x80 {
            out.push(c as u32);
            i += 1;
            continue;
        }
        let eod = |i: usize| if final_ { Step::Error(i, n, "unexpected end of data") } else { Step::Done(i) };
        if (0xC2..=0xDF).contains(&c) {
            if i + 1 >= n {
                return eod(i);
            }
            if !cont(i + 1) {
                return Step::Error(i, i + 1, "invalid continuation byte");
            }
            out.push(((c as u32 & 0x1F) << 6) | (data[i + 1] as u32 & 0x3F));
            i += 2;
        } else if (0xE0..=0xEF).contains(&c) {
            let (lo, hi) = match c {
                0xE0 => (0xA0, 0xBF),
                0xED => (0x80, 0x9F),
                _ => (0x80, 0xBF),
            };
            if i + 1 >= n {
                return eod(i);
            }
            let b1 = data[i + 1];
            if !(lo..=hi).contains(&b1) {
                return Step::Error(i, i + 1, "invalid continuation byte");
            }
            if i + 2 >= n {
                return eod(i);
            }
            if !cont(i + 2) {
                return Step::Error(i, i + 2, "invalid continuation byte");
            }
            out.push(((c as u32 & 0x0F) << 12) | ((b1 as u32 & 0x3F) << 6) | (data[i + 2] as u32 & 0x3F));
            i += 3;
        } else if (0xF0..=0xF4).contains(&c) {
            let (lo, hi) = match c {
                0xF0 => (0x90, 0xBF),
                0xF4 => (0x80, 0x8F),
                _ => (0x80, 0xBF),
            };
            if i + 1 >= n {
                return eod(i);
            }
            let b1 = data[i + 1];
            if !(lo..=hi).contains(&b1) {
                return Step::Error(i, i + 1, "invalid continuation byte");
            }
            if i + 2 >= n {
                return eod(i);
            }
            if !cont(i + 2) {
                return Step::Error(i, i + 2, "invalid continuation byte");
            }
            if i + 3 >= n {
                return eod(i);
            }
            if !cont(i + 3) {
                return Step::Error(i, i + 3, "invalid continuation byte");
            }
            out.push(
                ((c as u32 & 0x07) << 18)
                    | ((b1 as u32 & 0x3F) << 12)
                    | ((data[i + 2] as u32 & 0x3F) << 6)
                    | (data[i + 3] as u32 & 0x3F),
            );
            i += 4;
        } else {
            return Step::Error(i, i + 1, "invalid start byte");
        }
    }
    Step::Done(i)
}

/// Generic driver: `scan(data, pos, out)` decodes until done or an error; errors
/// go through the context's handler and decoding resumes where it says.
pub fn drive(ctx: &mut DecCtx, out: &mut StrBuf, mut scan: impl FnMut(&[u8], usize, &mut StrBuf) -> Step) -> PyResult<usize> {
    let mut i = 0;
    loop {
        let st = scan(&ctx.data, i, out);
        match st {
            Step::Done(p) => return Ok(p),
            Step::Error(s, e, reason) => {
                i = ctx.handle(s, e, reason, out)?;
            }
        }
    }
}

pub fn decode_utf8(ctx: &mut DecCtx, final_: bool) -> PyResult<(PyRef, usize)> {
    if let Ok(s) = std::str::from_utf8(&ctx.data) {
        return Ok((pystr::new_str(s), ctx.data.len()));
    }
    let mut out = StrBuf::with_capacity(ctx.data.len());
    let n = drive(ctx, &mut out, |d, i, o| utf8_scan(d, i, final_, o))?;
    Ok((out.finish(), n))
}

pub fn decode_ascii(ctx: &mut DecCtx) -> PyResult<PyRef> {
    if ctx.data.is_ascii() {
        return Ok(pystr::from_latin1(&ctx.data));
    }
    let mut out = StrBuf::with_capacity(ctx.data.len());
    drive(ctx, &mut out, |d, mut i, o| {
        while i < d.len() {
            if d[i] >= 0x80 {
                return Step::Error(i, i + 1, "ordinal not in range(128)");
            }
            o.push(d[i] as u32);
            i += 1;
        }
        Step::Done(i)
    })?;
    Ok(out.finish())
}

/// UTF-16 decoding. `bo` is the byte order in/out (0 = look for a BOM).
pub fn decode_utf16(ctx: &mut DecCtx, bo: &mut i32, final_: bool) -> PyResult<(PyRef, usize)> {
    let mut start = 0;
    if *bo == 0 && ctx.data.len() >= 2 {
        let d = &ctx.data;
        if d[0] == 0xFF && d[1] == 0xFE {
            *bo = -1;
            start = 2;
        } else if d[0] == 0xFE && d[1] == 0xFF {
            *bo = 1;
            start = 2;
        }
    }
    let le = if *bo == 0 { cfg!(target_endian = "little") } else { *bo < 0 };
    let mut out = StrBuf::with_capacity(ctx.data.len() / 2);
    let mut first = true;
    let n = drive(ctx, &mut out, |d, i, o| {
        let mut i = if first {
            first = false;
            start.max(i)
        } else {
            i
        };
        let n = d.len();
        let rd = |k: usize| (if le { u16::from_le_bytes([d[k], d[k + 1]]) } else { u16::from_be_bytes([d[k], d[k + 1]]) }) as u32;
        while i + 1 < n {
            let u = rd(i);
            if !is_surrogate(u) {
                o.push(u);
                i += 2;
                continue;
            }
            if u >= 0xDC00 {
                return Step::Error(i, i + 2, "illegal encoding");
            }
            if i + 3 >= n {
                if !final_ {
                    return Step::Done(i);
                }
                return Step::Error(i, n, "unexpected end of data");
            }
            let u2 = rd(i + 2);
            if (0xDC00..0xE000).contains(&u2) {
                o.push(0x10000 + ((u - 0xD800) << 10) + (u2 - 0xDC00));
                i += 4;
            } else {
                return Step::Error(i, i + 2, "illegal UTF-16 surrogate");
            }
        }
        if i < n {
            if !final_ {
                return Step::Done(i);
            }
            return Step::Error(i, n, "truncated data");
        }
        Step::Done(i)
    })?;
    Ok((out.finish(), n))
}

pub fn decode_utf32(ctx: &mut DecCtx, bo: &mut i32, final_: bool) -> PyResult<(PyRef, usize)> {
    let mut start = 0;
    if *bo == 0 && ctx.data.len() >= 4 {
        let d = &ctx.data;
        if d[..4] == [0xFF, 0xFE, 0, 0] {
            *bo = -1;
            start = 4;
        } else if d[..4] == [0, 0, 0xFE, 0xFF] {
            *bo = 1;
            start = 4;
        }
    }
    let le = if *bo == 0 { cfg!(target_endian = "little") } else { *bo < 0 };
    let mut out = StrBuf::with_capacity(ctx.data.len() / 4);
    let mut first = true;
    let n = drive(ctx, &mut out, |d, i, o| {
        let mut i = if first {
            first = false;
            start.max(i)
        } else {
            i
        };
        let n = d.len();
        while i + 3 < n {
            let b = [d[i], d[i + 1], d[i + 2], d[i + 3]];
            let c = if le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) };
            if c >= 0x110000 {
                return Step::Error(i, i + 4, "code point not in range(0x110000)");
            }
            if is_surrogate(c) {
                return Step::Error(i, i + 4, "code point in surrogate code point range(0xd800, 0xe000)");
            }
            o.push(c);
            i += 4;
        }
        if i < n {
            if !final_ {
                return Step::Done(i);
            }
            return Step::Error(i, n, "truncated data");
        }
        Step::Done(i)
    })?;
    Ok((out.finish(), n))
}

pub fn decode_utf7(ctx: &mut DecCtx, final_: bool) -> PyResult<(PyRef, usize)> {
    let mut out = StrBuf::with_capacity(ctx.data.len());
    let mut in_shift = false;
    let mut bits: u32 = 0;
    let mut buf: u64 = 0;
    let mut surrogate: u32 = 0;
    let mut shift_start = 0usize; // input position of the '+'
    let mut shift_out_start = 0usize; // output length when the shift started
    let mut i = 0usize;
    loop {
        let mut err: Option<(usize, usize, &'static str)> = None;
        {
            let d: &[u8] = &ctx.data;
            let n = d.len();
            while i < n {
                let ch = d[i] as u32;
                if in_shift {
                    if is_b64(ch) {
                        buf = (buf << 6) | from_b64(ch as u8);
                        bits += 6;
                        i += 1;
                        if bits >= 16 {
                            let oc = ((buf >> (bits - 16)) & 0xFFFF) as u32;
                            bits -= 16;
                            buf &= (1u64 << bits) - 1;
                            if surrogate != 0 {
                                if (0xDC00..0xE000).contains(&oc) {
                                    out.push(0x10000 + ((surrogate - 0xD800) << 10) + (oc - 0xDC00));
                                    surrogate = 0;
                                    continue;
                                }
                                out.push(surrogate);
                                surrogate = 0;
                            }
                            if (0xD800..0xDC00).contains(&oc) {
                                surrogate = oc;
                            } else {
                                out.push(oc);
                            }
                        }
                    } else {
                        in_shift = false;
                        if bits > 0 {
                            if bits >= 6 {
                                i += 1;
                                err = Some((shift_start, i, "partial character in shift sequence"));
                                break;
                            } else if buf != 0 {
                                i += 1;
                                err = Some((shift_start, i, "non-zero padding bits in shift sequence"));
                                break;
                            }
                        }
                        if surrogate != 0 && ch < 128 && ch != b'+' as u32 {
                            out.push(surrogate);
                        }
                        surrogate = 0;
                        if ch == b'-' as u32 {
                            i += 1;
                        }
                    }
                } else if ch == b'+' as u32 {
                    shift_start = i;
                    i += 1;
                    if i < n && d[i] == b'-' {
                        i += 1;
                        out.push('+' as u32);
                    } else if i < n && !is_b64(d[i] as u32) {
                        i += 1;
                        err = Some((shift_start, i, "ill-formed sequence"));
                        break;
                    } else {
                        in_shift = true;
                        surrogate = 0;
                        shift_out_start = out.len();
                        bits = 0;
                        buf = 0;
                    }
                } else if ch < 128 {
                    out.push(ch);
                    i += 1;
                } else {
                    let s = i;
                    i += 1;
                    err = Some((s, i, "unexpected special character"));
                    break;
                }
            }
        }
        if let Some((s, e, reason)) = err {
            i = ctx.handle(s, e, reason, &mut out)?;
            continue;
        }
        if in_shift && final_ {
            in_shift = false;
            if surrogate != 0 || bits >= 6 || (bits > 0 && buf != 0) {
                let n = ctx.data.len();
                i = ctx.handle(shift_start, n, "unterminated shift sequence", &mut out)?;
                if i < ctx.data.len() {
                    continue;
                }
            }
        }
        break;
    }
    if !final_ && in_shift {
        out.v.truncate(shift_out_start);
        out.max = out.v.iter().copied().max().unwrap_or(0);
        return Ok((out.finish(), shift_start));
    }
    Ok((out.finish(), i))
}

/// unicode_escape (`raw = false`) / raw_unicode_escape (`raw = true`) decoding.
/// Returns (str, consumed, first invalid escape character).
pub fn decode_unicode_escape(ctx: &mut DecCtx, final_: bool, raw: bool) -> PyResult<(PyRef, usize, Option<u32>)> {
    let mut out = StrBuf::with_capacity(ctx.data.len());
    let mut first_invalid: Option<u32> = None;
    let mut i = 0usize;
    loop {
        let mut err: Option<(usize, usize, &'static str)> = None;
        let mut incomplete_at: Option<usize> = None;
        {
            let d: &[u8] = &ctx.data;
            let n = d.len();
            'scan: while i < n {
                let c = d[i];
                if c != b'\\' {
                    out.push(c as u32);
                    i += 1;
                    continue;
                }
                let start = i;
                i += 1;
                if raw {
                    if i >= n {
                        if !final_ {
                            incomplete_at = Some(start);
                            break 'scan;
                        }
                        out.push('\\' as u32);
                        continue;
                    }
                    let (count, msg) = match d[i] {
                        b'u' => (4, "truncated \\uXXXX escape"),
                        b'U' => (8, "truncated \\UXXXXXXXX escape"),
                        _ => {
                            out.push('\\' as u32);
                            continue;
                        }
                    };
                    i += 1;
                    let mut ch: u32 = 0;
                    for _ in 0..count {
                        if i >= n {
                            if !final_ {
                                incomplete_at = Some(start);
                                break 'scan;
                            }
                            err = Some((start, i, msg));
                            break 'scan;
                        }
                        match hexdig(d[i]) {
                            Some(v) => ch = (ch << 4) | v,
                            None => {
                                err = Some((start, i, msg));
                                break 'scan;
                            }
                        }
                        i += 1;
                    }
                    if ch > 0x10FFFF {
                        err = Some((start, i, "\\Uxxxxxxxx out of range"));
                        break 'scan;
                    }
                    out.push(ch);
                    continue;
                }
                if i >= n {
                    if !final_ {
                        incomplete_at = Some(start);
                        break 'scan;
                    }
                    err = Some((start, i, "\\ at end of string"));
                    break 'scan;
                }
                let c2 = d[i];
                i += 1;
                let simple: Option<Option<u8>> = match c2 {
                    b'\n' => Some(None),
                    b'\\' => Some(Some(b'\\')),
                    b'\'' => Some(Some(b'\'')),
                    b'"' => Some(Some(b'"')),
                    b'b' => Some(Some(8)),
                    b'f' => Some(Some(12)),
                    b't' => Some(Some(9)),
                    b'n' => Some(Some(10)),
                    b'r' => Some(Some(13)),
                    b'v' => Some(Some(11)),
                    b'a' => Some(Some(7)),
                    _ => None,
                };
                if let Some(s) = simple {
                    if let Some(ch) = s {
                        out.push(ch as u32);
                    }
                    continue;
                }
                match c2 {
                    b'0'..=b'7' => {
                        let mut ch = (c2 - b'0') as u32;
                        if i < n && (b'0'..=b'7').contains(&d[i]) {
                            ch = (ch << 3) + (d[i] - b'0') as u32;
                            i += 1;
                            if i < n && (b'0'..=b'7').contains(&d[i]) {
                                ch = (ch << 3) + (d[i] - b'0') as u32;
                                i += 1;
                            }
                        }
                        if ch > 0o377 && first_invalid.is_none() {
                            first_invalid = Some(ch);
                        }
                        out.push(ch);
                    }
                    b'x' | b'u' | b'U' => {
                        let (count, msg) = match c2 {
                            b'x' => (2, "truncated \\xXX escape"),
                            b'u' => (4, "truncated \\uXXXX escape"),
                            _ => (8, "truncated \\UXXXXXXXX escape"),
                        };
                        let mut ch: u32 = 0;
                        for _ in 0..count {
                            if i >= n {
                                if !final_ {
                                    incomplete_at = Some(start);
                                    break 'scan;
                                }
                                err = Some((start, i, msg));
                                break 'scan;
                            }
                            match hexdig(d[i]) {
                                Some(v) => ch = (ch << 4) | v,
                                None => {
                                    err = Some((start, i, msg));
                                    break 'scan;
                                }
                            }
                            i += 1;
                        }
                        if ch > 0x10FFFF {
                            err = Some((start, i, "illegal Unicode character"));
                            break 'scan;
                        }
                        out.push(ch);
                    }
                    b'N' => {
                        let msg = "malformed \\N character escape";
                        if i >= n {
                            if !final_ {
                                incomplete_at = Some(start);
                                break 'scan;
                            }
                            err = Some((start, i, msg));
                            break 'scan;
                        }
                        if d[i] == b'{' {
                            i += 1;
                            let ns = i;
                            while i < n && d[i] != b'}' {
                                i += 1;
                            }
                            if i >= n {
                                if !final_ {
                                    incomplete_at = Some(start);
                                    break 'scan;
                                }
                                err = Some((start, i, msg));
                                break 'scan;
                            }
                            if i > ns {
                                let name = &d[ns..i];
                                i += 1;
                                let found = std::str::from_utf8(name).ok().and_then(rpy_unicode::lookup_name);
                                if let Some(ch) = found {
                                    out.push(ch);
                                    continue;
                                }
                                err = Some((start, i, "unknown Unicode character name"));
                                break 'scan;
                            }
                        }
                        err = Some((start, i, msg));
                        break 'scan;
                    }
                    _ => {
                        if first_invalid.is_none() {
                            first_invalid = Some(c2 as u32);
                        }
                        out.push('\\' as u32);
                        out.push(c2 as u32);
                    }
                }
            }
        }
        if let Some(p) = incomplete_at {
            return Ok((out.finish(), p, first_invalid));
        }
        match err {
            Some((s, e, reason)) => i = ctx.handle(s, e, reason, &mut out)?,
            None => break,
        }
    }
    Ok((out.finish(), i, first_invalid))
}

/// `codecs.escape_decode` on bytes. Returns (bytes, first invalid escape).
pub fn decode_escape(data: &[u8], errors: &str) -> PyResult<(Vec<u8>, Option<u32>)> {
    let mut out = Vec::with_capacity(data.len());
    let mut first_invalid = None;
    let n = data.len();
    let mut i = 0;
    while i < n {
        if data[i] != b'\\' {
            out.push(data[i]);
            i += 1;
            continue;
        }
        i += 1;
        if i == n {
            raise!(ValueError, "Trailing \\ in string");
        }
        let c = data[i];
        i += 1;
        match c {
            b'\n' => {}
            b'\\' => out.push(b'\\'),
            b'\'' => out.push(b'\''),
            b'"' => out.push(b'"'),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b't' => out.push(9),
            b'n' => out.push(10),
            b'r' => out.push(13),
            b'v' => out.push(11),
            b'a' => out.push(7),
            b'0'..=b'7' => {
                let mut v = (c - b'0') as u32;
                if i < n && (b'0'..=b'7').contains(&data[i]) {
                    v = (v << 3) + (data[i] - b'0') as u32;
                    i += 1;
                    if i < n && (b'0'..=b'7').contains(&data[i]) {
                        v = (v << 3) + (data[i] - b'0') as u32;
                        i += 1;
                    }
                }
                if v > 0o377 && first_invalid.is_none() {
                    first_invalid = Some(v);
                }
                out.push(v as u8);
            }
            b'x' => {
                if i + 1 < n {
                    if let (Some(a), Some(b)) = (hexdig(data[i]), hexdig(data[i + 1])) {
                        out.push((a * 16 + b) as u8);
                        i += 2;
                        continue;
                    }
                }
                match errors {
                    "strict" => raise!(ValueError, "invalid \\x escape at position {}", i - 2),
                    "replace" => out.push(b'?'),
                    "ignore" => {}
                    _ => raise!(ValueError, "decoding error; unknown error handling code: {}", tu::trunc_chars(errors, 400)),
                }
                if i < n && hexdig(data[i]).is_some() {
                    i += 1;
                }
            }
            _ => {
                if first_invalid.is_none() {
                    first_invalid = Some(c as u32);
                }
                out.push(b'\\');
                i -= 1;
            }
        }
    }
    Ok((out, first_invalid))
}

/// DeprecationWarning for the first invalid escape of a decoded literal.
pub fn invalid_escape_warning(first: Option<u32>) -> PyResult<()> {
    if let Some(c) = first {
        let msg = if c > 0xFF {
            format!("\"\\{:o}\" is an invalid octal escape sequence. Such sequences will not work in the future. ", c)
        } else {
            format!(
                "\"\\{}\" is an invalid escape sequence. Such sequences will not work in the future. ",
                char::from_u32(c).unwrap_or('?')
            )
        };
        tu::warn(exc::DeprecationWarning(), &msg, 1)?;
    }
    Ok(())
}
