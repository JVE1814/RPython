//! The `binascii` module: base64/base32/base85/ascii85/uu/qp/hex codecs and CRCs.

use crate::abstract_ as ab;
use crate::args::{self, Params};
use crate::errors::*;
use crate::ffi::PyObject;
use crate::modules::_struct::Buf;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::*;
use crate::types::{bytes, dict, int, int_methods, none, str as pystr, tuple};
use std::sync::atomic::Ordering;

// (interp1) per interpreter: a subinterpreter gets its own exception classes
static ERROR: crate::subinterp::InterpPtr = crate::subinterp::InterpPtr::new();
static INCOMPLETE: crate::subinterp::InterpPtr = crate::subinterp::InterpPtr::new();

fn error(msg: &str) -> PyErr {
    new_err(ERROR.load(Ordering::Acquire) as *mut crate::ffi::PyTypeObject, msg)
}
macro_rules! berr {
    ($($arg:tt)*) => {
        return Err(error(&format!($($arg)*)))
    };
}

const BASE64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URLSAFE64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
const CRYPT: &[u8] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const BINHEX: &[u8] = b"!\"#$%&'()*+,-012345689@ABCDEFGHIJKLMNPQRSTUVXYZ[`abcdefhijklmpqr";
const BASE85: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz!#$%&()*+-;<=>?@^_`{|}~";
const Z85: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ.-:+=^!/*?&<>()[]{}@%$#";
const BASE32: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
const BASE32HEX: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUV";

// ------------------------------------------------------------------ argument helpers

/// Input data borrowed from its owner (a buffer export or an ASCII str) for the
/// duration of a call; dereferences to the bytes (no copy).
enum Input {
    Buf(Buf),
    Str(PyRef),
}

impl std::ops::Deref for Input {
    type Target = [u8];
    #[inline]
    fn deref(&self) -> &[u8] {
        match self {
            Input::Buf(b) => b.data(),
            Input::Str(s) => pystr::str_data(s).as_bytes(),
        }
    }
}

/// a2b input: ASCII str or bytes-like.
fn ascii_data(o: &PyObject) -> PyResult<Input> {
    if pystr::is_str(o) {
        if !pystr::str_is_ascii(o) {
            raise!(ValueError, "string argument should contain only ASCII characters");
        }
        return Ok(Input::Str(o.to_ref()));
    }
    Ok(Input::Buf(Buf::get(o, false)?))
}

/// b2a input: bytes-like only.
fn bin_data(o: &PyObject) -> PyResult<Input> {
    Ok(Input::Buf(Buf::get(o, false)?))
}

/// Build a bytes result in place: `f` gets a buffer of `cap` bytes and returns how
/// many it wrote (at most `cap`). Avoids an intermediate vector and its copy.
fn emit(cap: usize, f: impl FnOnce(&mut [u8]) -> PyResult<usize>) -> PyResult {
    if cap <= 1 {
        let mut tmp = [0u8; 1];
        let n = f(&mut tmp[..cap])?;
        return Ok(bytes::new_bytes(&tmp[..n]));
    }
    let o = bytes::try_alloc_bytes_uninit(cap)?;
    let p = bytes::bytes_ptr(&o);
    let n = f(unsafe { std::slice::from_raw_parts_mut(p, cap) })?;
    if n == cap {
        return Ok(o);
    }
    if n <= 1 {
        return Ok(bytes::new_bytes(unsafe { std::slice::from_raw_parts(p, n) }));
    }
    unsafe { bytes::resize_unique(o, n) }.map_err(|_| PyErr)
}

/// Table of the 64 output characters (bounds checks vanish with `& 63`).
fn table64(alpha: &[u8]) -> [u8; 64] {
    let mut t = [0u8; 64];
    t.copy_from_slice(&alpha[..64]);
    t
}

fn ignore_set(o: Option<&PyObject>) -> PyResult<Option<[bool; 256]>> {
    match o {
        None => Ok(None),
        Some(o) => {
            if pystr::is_str(o) || o.is(none::none()) {
                raise!(TypeError, "a bytes-like object is required, not '{}'", tname(o));
            }
            let d = Buf::get(o, false)?;
            let mut s = [false; 256];
            for &c in d.data() {
                s[c as usize] = true;
            }
            Ok(Some(s))
        }
    }
}

fn alphabet_arg(o: Option<&PyObject>, default: &[u8], n: usize, decode: bool) -> PyResult<Vec<u8>> {
    match o {
        None => Ok(default.to_vec()),
        Some(o) => {
            let v = if decode {
                if !bytes::is_bytes(o) {
                    raise!(TypeError, "alphabet must be bytes, not {}", tname(o));
                }
                bytes::bytes_data(o).to_vec()
            } else {
                if pystr::is_str(o) || o.is(none::none()) {
                    raise!(TypeError, "a bytes-like object is required, not '{}'", tname(o));
                }
                bin_data(o)?.to_vec()
            };
            if v.len() != n {
                raise!(ValueError, "alphabet must have length {}", n);
            }
            let mut seen = [false; 256];
            for &c in &v {
                if seen[c as usize] {
                    raise!(ValueError, "alphabet must not contain duplicate characters");
                }
                seen[c as usize] = true;
            }
            Ok(v)
        }
    }
}

/// Decoding table of the standard base64 alphabet.
static BASE64_DECODE: [i16; 256] = {
    let mut t = [-1i16; 256];
    let mut i = 0;
    while i < 64 {
        t[BASE64[i] as usize] = i as i16;
        i += 1;
    }
    t
};

fn decode_table(alpha: &[u8]) -> [i16; 256] {
    let mut t = [-1i16; 256];
    for (i, &c) in alpha.iter().enumerate() {
        t[c as usize] = i as i16;
    }
    t
}

fn bool_arg(o: Option<&PyObject>, default: bool) -> PyResult<bool> {
    match o {
        None => Ok(default),
        Some(o) => ab::is_true(o),
    }
}

/// wrapcol: non-negative size_t.
fn wrapcol_arg(o: Option<&PyObject>) -> PyResult<usize> {
    match o {
        None => Ok(0),
        Some(o) => {
            if !int::is_int(o) && !ab::has_index(o) {
                raise!(TypeError, "'{}' object cannot be interpreted as an integer", tname(o));
            }
            let i = ab::index(o)?;
            if int::sign(&i) < 0 {
                raise!(ValueError, "wrapcol must be non-negative");
            }
            match rpy_bigint::conv::to_u64(int_methods::iref(&i)) {
                Some(v) => Ok(v.min(usize::MAX as u64) as usize),
                None => raise!(OverflowError, "Python int too large to convert to C size_t"),
            }
        }
    }
}

fn wrap_lines(s: Vec<u8>, width: usize) -> Vec<u8> {
    if width == 0 || s.len() <= width {
        return s;
    }
    let mut out = Vec::with_capacity(s.len() + s.len() / width + 1);
    for (i, ch) in s.chunks(width).enumerate() {
        if i > 0 {
            out.push(b'\n');
        }
        out.extend_from_slice(ch);
    }
    out
}

fn bytes_ret(v: Vec<u8>) -> PyResult {
    Ok(bytes::new_bytes(&v))
}

// ------------------------------------------------------------------ base64

fn b2a_base64(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("b2a_base64", &["data", "wrapcol", "newline", "padded", "alphabet"], 1, 1).kwonly(4);
    let [data, wrapcol, newline, padded, alphabet] = args::parse::<5>(&P, a, kw)?;
    let d = bin_data(data.unwrap())?;
    let wrapcol = wrapcol_arg(wrapcol)?;
    let newline = bool_arg(newline, true)?;
    let padded = bool_arg(padded, true)?;
    let alpha = match alphabet {
        None => table64(BASE64),
        Some(_) => table64(&alphabet_arg(alphabet, BASE64, 64, false)?),
    };
    let n = d.len();
    let rem = n % 3;
    let tail = match (rem, padded) {
        (0, _) => 0,
        (_, true) => 4,
        (r, false) => r + 1,
    };
    let enc_len = n / 3 * 4 + tail;
    let width = if wrapcol == 0 { 0 } else { (wrapcol / 4 * 4).max(4) };
    if width != 0 && enc_len > width {
        let mut out = vec![0u8; enc_len];
        encode64(&d, &alpha, padded, &mut out);
        let mut out = wrap_lines(out, width);
        if newline {
            out.push(b'\n');
        }
        return bytes_ret(out);
    }
    emit(enc_len + newline as usize, |out| {
        encode64(&d, &alpha, padded, out);
        if newline {
            out[enc_len] = b'\n';
        }
        Ok(enc_len + newline as usize)
    })
}

/// Base64-encode `d` into `out` (which has room for the exact encoded length).
fn encode64(d: &[u8], alpha: &[u8; 64], padded: bool, out: &mut [u8]) {
    let mut o = 0;
    let mut chunks = d.chunks_exact(3);
    for ch in &mut chunks {
        let v = (ch[0] as u32) << 16 | (ch[1] as u32) << 8 | ch[2] as u32;
        let dst = &mut out[o..o + 4];
        dst[0] = alpha[(v >> 18) as usize & 63];
        dst[1] = alpha[(v >> 12) as usize & 63];
        dst[2] = alpha[(v >> 6) as usize & 63];
        dst[3] = alpha[v as usize & 63];
        o += 4;
    }
    let r = chunks.remainder();
    if !r.is_empty() {
        let b1 = if r.len() > 1 { r[1] as u32 } else { 0 };
        let v = (r[0] as u32) << 16 | b1 << 8;
        out[o] = alpha[(v >> 18) as usize & 63];
        out[o + 1] = alpha[(v >> 12) as usize & 63];
        o += 2;
        if r.len() > 1 {
            out[o] = alpha[(v >> 6) as usize & 63];
            o += 1;
        } else if padded {
            out[o] = b'=';
            o += 1;
        }
        if padded {
            out[o] = b'=';
        }
    }
}

struct PadState {
    quad_pos: usize,
    pads: usize,
    padding_started: bool,
    complete: bool,
}

fn a2b_base64(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params =
        Params::new("a2b_base64", &["data", "strict_mode", "padded", "alphabet", "ignorechars", "canonical"], 1, 1).kwonly(5);
    let [data, strict, padded, alphabet, ignorechars, canonical] = args::parse::<6>(&P, a, kw)?;
    let d = ascii_data(data.unwrap())?;
    let ign = ignore_set(ignorechars)?;
    let strict = bool_arg(strict, ign.is_some())?;
    let padded = bool_arg(padded, true)?;
    let canonical = bool_arg(canonical, false)?;
    // perf3: the default alphabet and empty ignore set use prebuilt tables
    let custom;
    let table: &[i16; 256] = match alphabet {
        None => &BASE64_DECODE,
        Some(_) => {
            custom = decode_table(&alphabet_arg(alphabet, BASE64, 64, true)?);
            &custom
        }
    };
    const NO_IGNORE: [bool; 256] = [false; 256];
    let ign: &[bool; 256] = ign.as_ref().unwrap_or(&NO_IGNORE);
    let eq_ignorable = ign[b'=' as usize];

    emit(d.len() / 4 * 3 + 3, |outbuf| decode64(&d, table, outbuf, strict, padded, canonical, eq_ignorable, ign))
}

/// Base64 decoding proper; returns the number of bytes written to `outbuf` (which
/// has room for len/4*3+3 bytes).
#[allow(clippy::too_many_arguments)]
fn decode64(
    d: &[u8],
    table: &[i16; 256],
    outbuf: &mut [u8],
    strict: bool,
    padded: bool,
    canonical: bool,
    eq_ignorable: bool,
    ign: &[bool; 256],
) -> PyResult<usize> {
    // Fast prefix: whole quads of alphabet characters decode the same way in every
    // mode (no padding seen yet, nothing to skip).
    let mut o = 0usize;
    let mut i = 0usize;
    while i + 4 <= d.len() {
        let a = table[d[i] as usize];
        let b = table[d[i + 1] as usize];
        let c = table[d[i + 2] as usize];
        let e = table[d[i + 3] as usize];
        if (a | b | c | e) < 0 {
            break;
        }
        let v = (a as u32) << 18 | (b as u32) << 12 | (c as u32) << 6 | e as u32;
        let dst = &mut outbuf[o..o + 3];
        dst[0] = (v >> 16) as u8;
        dst[1] = (v >> 8) as u8;
        dst[2] = v as u8;
        o += 3;
        i += 4;
    }
    let mut out = OutCursor { buf: outbuf, len: o };
    let mut st = PadState { quad_pos: 0, pads: 0, padding_started: false, complete: false };
    let mut leftchar: u32 = 0;
    let mut ndata: usize = i;
    let mut pads_after_last: usize = 0;
    for &c in &d[i..] {
        if c == b'=' && table[b'=' as usize] < 0 {
            if !padded {
                if strict && !eq_ignorable {
                    berr!("Padding not allowed");
                }
                continue;
            }
            if !strict {
                pads_after_last += 1;
                continue;
            }
            // strict padding rules
            let err: Option<String> = if st.complete {
                Some("Excess padding not allowed".into())
            } else if st.quad_pos == 0 {
                if ndata == 0 {
                    Some("Leading padding not allowed".into())
                } else {
                    Some("Excess padding not allowed".into())
                }
            } else if st.quad_pos == 1 {
                Some(format!(
                    "Invalid base64-encoded string: number of data characters ({}) cannot be 1 more than a multiple of 4",
                    ndata
                ))
            } else {
                None
            };
            if let Some(e) = err {
                if eq_ignorable {
                    continue;
                }
                return Err(error(&e));
            }
            st.padding_started = true;
            st.pads += 1;
            if st.quad_pos + st.pads >= 4 {
                st.complete = true;
            }
            continue;
        }
        let v = table[c as usize];
        if v < 0 {
            if strict && !ign[c as usize] {
                berr!("Only base64 data is allowed");
            }
            continue;
        }
        if strict && (st.padding_started || st.complete) {
            if !eq_ignorable {
                if st.complete {
                    berr!("Excess data after padding");
                }
                berr!("Discontinuous padding not allowed");
            }
            // treat the previous '=' as ignored
            st.padding_started = false;
            st.complete = false;
            st.pads = 0;
        }
        pads_after_last = 0;
        ndata += 1;
        let v = v as u32;
        match st.quad_pos {
            0 => {
                leftchar = v;
                st.quad_pos = 1;
            }
            1 => {
                out.push(((leftchar << 2) | (v >> 4)) as u8);
                leftchar = v & 0x0f;
                st.quad_pos = 2;
            }
            2 => {
                out.push(((leftchar << 4) | (v >> 2)) as u8);
                leftchar = v & 0x03;
                st.quad_pos = 3;
            }
            _ => {
                out.push(((leftchar << 6) | v) as u8);
                leftchar = 0;
                st.quad_pos = 0;
            }
        }
    }
    if st.quad_pos == 1 {
        berr!(
            "Invalid base64-encoded string: number of data characters ({}) cannot be 1 more than a multiple of 4",
            ndata
        );
    }
    if st.quad_pos != 0 && padded {
        let have = if strict { st.pads } else { pads_after_last };
        if st.quad_pos + have < 4 {
            berr!("Incorrect padding");
        }
    }
    if canonical && st.quad_pos != 0 && leftchar != 0 {
        berr!("Non-zero padding bits are not allowed");
    }
    Ok(out.len)
}

/// Appends into a preallocated output buffer.
struct OutCursor<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl OutCursor<'_> {
    #[inline]
    fn push(&mut self, b: u8) {
        self.buf[self.len] = b;
        self.len += 1;
    }
    #[inline]
    fn extend_from_slice(&mut self, s: &[u8]) {
        self.buf[self.len..self.len + s.len()].copy_from_slice(s);
        self.len += s.len();
    }
}

// ------------------------------------------------------------------ base32

fn b2a_base32(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("b2a_base32", &["data", "padded", "wrapcol", "alphabet"], 1, 1).kwonly(3);
    let [data, padded, wrapcol, alphabet] = args::parse::<4>(&P, a, kw)?;
    let d = bin_data(data.unwrap())?;
    let padded = bool_arg(padded, true)?;
    let wrapcol = wrapcol_arg(wrapcol)?;
    let alpha = alphabet_arg(alphabet, BASE32, 32, false)?;
    let width = if wrapcol == 0 { 0 } else { (wrapcol / 8 * 8).max(8) };
    let full = d.len() / 5;
    let rem = d.len() % 5;
    let tail = match (rem, padded) {
        (0, _) => 0,
        (_, true) => 8,
        (1, false) => 2,
        (2, false) => 4,
        (3, false) => 5,
        (_, false) => 7,
    };
    let enc_len = full * 8 + tail;
    if width == 0 || enc_len <= width {
        let mut a32 = [0u8; 32];
        a32.copy_from_slice(&alpha[..32]);
        return emit(enc_len, |out| {
            let mut o = 0;
            for ch in d.chunks_exact(5) {
                let v = (ch[0] as u64) << 32 | (ch[1] as u64) << 24 | (ch[2] as u64) << 16 | (ch[3] as u64) << 8 | ch[4] as u64;
                let dst = &mut out[o..o + 8];
                for (k, slot) in dst.iter_mut().enumerate() {
                    *slot = a32[((v >> (35 - 5 * k)) & 31) as usize];
                }
                o += 8;
            }
            let r = &d[full * 5..];
            if !r.is_empty() {
                let mut buf = [0u8; 5];
                buf[..r.len()].copy_from_slice(r);
                let v = buf.iter().fold(0u64, |acc, &b| (acc << 8) | b as u64);
                let nchars = [0, 2, 4, 5, 7][r.len()];
                for k in 0..8 {
                    if k < nchars {
                        out[o] = a32[((v >> (35 - 5 * k)) & 31) as usize];
                        o += 1;
                    } else if padded {
                        out[o] = b'=';
                        o += 1;
                    }
                }
            }
            Ok(o)
        });
    }
    let mut out = Vec::with_capacity((d.len() + 4) / 5 * 8);
    for ch in d.chunks(5) {
        let mut buf = [0u8; 5];
        buf[..ch.len()].copy_from_slice(ch);
        let v = buf.iter().fold(0u64, |acc, &b| (acc << 8) | b as u64);
        let nchars = match ch.len() {
            1 => 2,
            2 => 4,
            3 => 5,
            4 => 7,
            _ => 8,
        };
        for k in 0..8 {
            if k < nchars {
                out.push(alpha[((v >> (35 - 5 * k)) & 31) as usize]);
            } else if padded {
                out.push(b'=');
            }
        }
    }
    bytes_ret(wrap_lines(out, width))
}

fn a2b_base32(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("a2b_base32", &["data", "padded", "alphabet", "ignorechars", "canonical"], 1, 1).kwonly(4);
    let [data, padded, alphabet, ignorechars, canonical] = args::parse::<5>(&P, a, kw)?;
    let d = ascii_data(data.unwrap())?;
    let ign = ignore_set(ignorechars)?.unwrap_or([false; 256]);
    let padded = bool_arg(padded, true)?;
    let canonical = bool_arg(canonical, false)?;
    let alpha = alphabet_arg(alphabet, BASE32, 32, true)?;
    let table = decode_table(&alpha);
    let eq_ignorable = ign[b'=' as usize];
    emit(d.len() / 8 * 5 + 5, |outbuf| {
    // fast prefix: whole groups of 8 alphabet characters
    let mut o = 0usize;
    let mut i = 0usize;
    while i + 8 <= d.len() {
        let mut v: u64 = 0;
        let mut bad = 0i16;
        for &c in &d[i..i + 8] {
            let t = table[c as usize];
            bad |= t;
            v = (v << 5) | (t as u64 & 31);
        }
        if bad < 0 {
            break;
        }
        let dst = &mut outbuf[o..o + 5];
        for (k, slot) in dst.iter_mut().enumerate() {
            *slot = (v >> (32 - 8 * k)) as u8;
        }
        o += 5;
        i += 8;
    }
    let mut out = OutCursor { buf: outbuf, len: o };
    let mut pos: usize = 0; // chars in current group
    let mut acc: u64 = 0;
    let mut ndata: usize = i;
    let mut pads: usize = 0;
    let mut padding_started = false;
    let mut complete = false;
    let need_pad = |p: usize| -> usize {
        match p {
            2 => 6,
            4 => 4,
            5 => 3,
            7 => 1,
            _ => 0,
        }
    };
    let invalid_len = |n: usize| format!("Invalid base32-encoded string: number of data characters ({}) cannot be 1, 3, or 6 more than a multiple of 8", n);
    for &c in &d[i..] {
        if c == b'=' && table[b'=' as usize] < 0 {
            if !padded {
                if eq_ignorable {
                    continue;
                }
                berr!("Padding not allowed");
            }
            let err: Option<String> = if complete {
                Some("Excess padding not allowed".into())
            } else if pos == 0 {
                if ndata == 0 { Some("Leading padding not allowed".into()) } else { Some("Excess padding not allowed".into()) }
            } else if matches!(pos, 1 | 3 | 6) {
                Some(invalid_len(ndata))
            } else {
                None
            };
            if let Some(e) = err {
                if eq_ignorable {
                    continue;
                }
                return Err(error(&e));
            }
            padding_started = true;
            pads += 1;
            if pads >= need_pad(pos) {
                complete = true;
            }
            continue;
        }
        let v = table[c as usize];
        if v < 0 {
            if ign[c as usize] {
                continue;
            }
            berr!("Only base32 data is allowed");
        }
        if padding_started || complete {
            if !eq_ignorable {
                if complete {
                    berr!("Excess data after padding");
                }
                berr!("Discontinuous padding not allowed");
            }
            padding_started = false;
            complete = false;
            pads = 0;
        }
        ndata += 1;
        acc = (acc << 5) | v as u64;
        pos += 1;
        if pos == 8 {
            for k in (0..5).rev() {
                out.push((acc >> (8 * k)) as u8);
            }
            acc = 0;
            pos = 0;
        }
    }
    if matches!(pos, 1 | 3 | 6) {
        return Err(error(&invalid_len(ndata)));
    }
    if pos != 0 {
        if padded && pads < need_pad(pos) {
            berr!("Incorrect padding");
        }
        let nbytes = match pos {
            2 => 1,
            4 => 2,
            5 => 3,
            _ => 4,
        };
        let bits = pos * 5;
        let extra = bits - nbytes * 8;
        if canonical && acc & ((1u64 << extra) - 1) != 0 {
            berr!("Non-zero padding bits are not allowed");
        }
        let v = acc >> extra;
        for k in (0..nbytes).rev() {
            out.push((v >> (8 * k)) as u8);
        }
    }
    Ok(out.len)
    })
}

// ------------------------------------------------------------------ ascii85 / base85

/// The Ascii85 digit characters ('!' + value).
const ASCII85: [u8; 85] = {
    let mut t = [0u8; 85];
    let mut i = 0;
    while i < 85 {
        t[i] = b'!' + i as u8;
        i += 1;
    }
    t
};

#[inline]
fn enc85_group(v: u32, alpha: &[u8]) -> [u8; 5] {
    let alpha: &[u8; 85] = alpha[..85].try_into().unwrap();
    let mut r = [0u8; 5];
    let mut x = v;
    for k in (0..5).rev() {
        r[k] = alpha[(x % 85) as usize];
        x /= 85;
    }
    r
}

/// Value of a five-digit base-85 group (digits already decoded); None on overflow.
#[inline]
fn dec85_group(g: [u8; 5]) -> Option<u32> {
    let v = g.iter().fold(0u64, |acc, &x| acc * 85 + x as u64);
    if v > 0xffff_ffff { None } else { Some(v as u32) }
}

fn b2a_ascii85(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("b2a_ascii85", &["data", "foldspaces", "wrapcol", "pad", "adobe"], 1, 1).kwonly(4);
    let [data, foldspaces, wrapcol, pad, adobe] = args::parse::<5>(&P, a, kw)?;
    let d = bin_data(data.unwrap())?;
    let foldspaces = bool_arg(foldspaces, false)?;
    let wrapcol = wrapcol_arg(wrapcol)?;
    let pad = bool_arg(pad, false)?;
    let adobe = bool_arg(adobe, false)?;
    let cap = (d.len() / 4 + 1) * 5;
    let encode = |out: &mut OutCursor| {
        let mut chunks = d.chunks_exact(4);
        for ch in &mut chunks {
            let v = u32::from_be_bytes([ch[0], ch[1], ch[2], ch[3]]);
            if v == 0 {
                out.push(b'z');
            } else if foldspaces && v == 0x20202020 {
                out.push(b'y');
            } else {
                out.extend_from_slice(&enc85_group(v, &ASCII85));
            }
        }
        let r = chunks.remainder();
        if !r.is_empty() {
            let mut buf = [0u8; 4];
            buf[..r.len()].copy_from_slice(r);
            let v = u32::from_be_bytes(buf);
            if pad {
                if v == 0 {
                    out.push(b'z');
                } else if foldspaces && v == 0x20202020 {
                    out.push(b'y');
                } else {
                    out.extend_from_slice(&enc85_group(v, &ASCII85));
                }
            } else {
                out.extend_from_slice(&enc85_group(v, &ASCII85)[..r.len() + 1]);
            }
        }
    };
    if wrapcol == 0 {
        // tokens: "<~" + chars + "~>"
        return emit(cap + 4, |buf| {
            let mut out = OutCursor { buf, len: 0 };
            if adobe {
                out.extend_from_slice(b"<~");
            }
            encode(&mut out);
            if adobe {
                out.extend_from_slice(b"~>");
            }
            Ok(out.len)
        });
    }
    let mut cbuf = vec![0u8; cap];
    let mut cur = OutCursor { buf: &mut cbuf, len: 0 };
    encode(&mut cur);
    let n = cur.len;
    cbuf.truncate(n);
    let chars = cbuf;
    if !adobe {
        // every token is one character: plain fixed-width lines
        return bytes_ret(wrap_lines(chars, wrapcol));
    }
    // the "<~" and "~>" delimiters are not split across lines
    let mut out = Vec::with_capacity(chars.len() + chars.len() / wrapcol.max(1) + 8);
    let width = if adobe { wrapcol.max(2) } else { wrapcol };
    let mut linelen = 0usize;
    let mut put = |tok: &[u8], out: &mut Vec<u8>, linelen: &mut usize| {
        if *linelen > 0 && *linelen + tok.len() > width {
            out.push(b'\n');
            *linelen = 0;
        }
        out.extend_from_slice(tok);
        *linelen += tok.len();
    };
    if adobe {
        put(b"<~", &mut out, &mut linelen);
    }
    for &c in &chars {
        put(&[c], &mut out, &mut linelen);
    }
    if adobe {
        put(b"~>", &mut out, &mut linelen);
    }
    bytes_ret(out)
}

fn a2b_ascii85(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("a2b_ascii85", &["data", "foldspaces", "adobe", "ignorechars", "canonical"], 1, 1).kwonly(4);
    let [data, foldspaces, adobe, ignorechars, canonical] = args::parse::<5>(&P, a, kw)?;
    let input = ascii_data(data.unwrap())?;
    let mut d: &[u8] = &input;
    let foldspaces = bool_arg(foldspaces, false)?;
    let adobe = bool_arg(adobe, false)?;
    let ign = ignore_set(ignorechars)?.unwrap_or([false; 256]);
    let canonical = bool_arg(canonical, false)?;
    if adobe {
        if !d.ends_with(b"~>") {
            berr!("Ascii85 encoded byte sequences must end with b'~>'");
        }
        d = &d[..d.len() - 2];
        if d.starts_with(b"<~") {
            d = &d[2..];
        }
    }
    // 'z' and 'y' expand one character to four bytes
    let short = d.iter().filter(|&&c| c == b'z' || c == b'y').count();
    let cap = d.len() / 5 * 4 + 4 + short * 4;
    // a (possibly partial) group of n digits; raw are its characters
    let flush = |group: &[u8; 5], n: usize, out: &mut OutCursor| -> PyResult<()> {
        let mut full = [84u8; 5];
        full[..n].copy_from_slice(&group[..n]);
        let v = match dec85_group(full) {
            Some(v) => v,
            None => berr!("Ascii85 overflow"),
        };
        if n == 1 {
            berr!("Incomplete Ascii85 group: 1 character groups are not allowed");
        }
        let bytes4 = v.to_be_bytes();
        let nb = if n == 5 { 4 } else { n - 1 };
        if canonical {
            if n == 5 && v == 0 {
                berr!("Non-canonical Ascii85 encoding: zero group must be encoded as b'z'");
            }
            if n < 5 {
                let mut b = [0u8; 4];
                b[..nb].copy_from_slice(&bytes4[..nb]);
                let g = enc85_group(u32::from_be_bytes(b), &ASCII85);
                if g[..n].iter().zip(&group[..n]).any(|(&e, &x)| e != x + 33) {
                    berr!("Non-canonical Ascii85 encoding");
                }
            }
        }
        out.extend_from_slice(&bytes4[..nb]);
        Ok(())
    };
    emit(cap, |buf| {
        let mut out = OutCursor { buf, len: 0 };
        let mut group = [0u8; 5];
        let mut n = 0usize;
        let mut i = 0usize;
        while i < d.len() {
            if n == 0 && i + 5 <= d.len() {
                // fast path: a whole group of digits
                let g: [u8; 5] = d[i..i + 5].try_into().unwrap();
                if g.iter().all(|c| (b'!'..=b'u').contains(c)) {
                    let digits = g.map(|c| c - 33);
                    if !(canonical && digits == [0; 5]) {
                        if let Some(v) = dec85_group(digits) {
                            out.extend_from_slice(&v.to_be_bytes());
                            i += 5;
                            continue;
                        }
                    }
                }
            }
            let c = d[i];
            i += 1;
            if (b'!'..=b'u').contains(&c) {
                group[n] = c - 33;
                n += 1;
                if n == 5 {
                    flush(&group, 5, &mut out)?;
                    n = 0;
                }
            } else if c == b'z' {
                if n != 0 {
                    berr!("'z' inside Ascii85 5-tuple");
                }
                out.extend_from_slice(&[0, 0, 0, 0]);
            } else if c == b'y' && foldspaces {
                if n != 0 {
                    berr!("'y' inside Ascii85 5-tuple");
                }
                out.extend_from_slice(b"    ");
            } else if ign[c as usize] {
                continue;
            } else {
                berr!("Non-Ascii85 digit found: {}", c as char);
            }
        }
        if n != 0 {
            flush(&group, n, &mut out)?;
        }
        Ok(out.len)
    })
}

fn b2a_base85(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("b2a_base85", &["data", "pad", "wrapcol", "alphabet"], 1, 1).kwonly(3);
    let [data, pad, wrapcol, alphabet] = args::parse::<4>(&P, a, kw)?;
    let d = bin_data(data.unwrap())?;
    let pad = bool_arg(pad, false)?;
    let wrapcol = wrapcol_arg(wrapcol)?;
    let alpha = alphabet_arg(alphabet, BASE85, 85, false)?;
    let width = if wrapcol == 0 { 0 } else { (wrapcol / 5 * 5).max(5) };
    let rem = d.len() % 4;
    let enc_len = d.len() / 4 * 5 + if rem == 0 { 0 } else if pad { 5 } else { rem + 1 };
    if width == 0 || enc_len <= width {
        let alpha: [u8; 85] = alpha[..85].try_into().unwrap();
        return emit(enc_len, |buf| {
            let mut out = OutCursor { buf, len: 0 };
            let mut chunks = d.chunks_exact(4);
            for ch in &mut chunks {
                out.extend_from_slice(&enc85_group(u32::from_be_bytes([ch[0], ch[1], ch[2], ch[3]]), &alpha));
            }
            let r = chunks.remainder();
            if !r.is_empty() {
                let mut b = [0u8; 4];
                b[..r.len()].copy_from_slice(r);
                let g = enc85_group(u32::from_be_bytes(b), &alpha);
                out.extend_from_slice(if pad { &g } else { &g[..r.len() + 1] });
            }
            Ok(out.len)
        });
    }
    let mut out = Vec::with_capacity(d.len() / 4 * 5 + 5);
    for ch in d.chunks(4) {
        let mut buf = [0u8; 4];
        buf[..ch.len()].copy_from_slice(ch);
        let g = enc85_group(u32::from_be_bytes(buf), &alpha);
        if ch.len() == 4 || pad {
            out.extend_from_slice(&g);
        } else {
            out.extend_from_slice(&g[..ch.len() + 1]);
        }
    }
    bytes_ret(wrap_lines(out, width))
}

fn a2b_base85(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("a2b_base85", &["data", "alphabet", "ignorechars", "canonical"], 1, 1).kwonly(3);
    let [data, alphabet, ignorechars, canonical] = args::parse::<4>(&P, a, kw)?;
    let d = ascii_data(data.unwrap())?;
    let alpha = alphabet_arg(alphabet, BASE85, 85, true)?;
    let ign = ignore_set(ignorechars)?.unwrap_or([false; 256]);
    let canonical = bool_arg(canonical, false)?;
    let table = decode_table(&alpha);
    // `groups`: groups decoded so far (the error message reports the hunk offset)
    let flush = |group: &[u8; 5], raw: &[u8; 5], n: usize, groups: usize, out: &mut OutCursor| -> PyResult<()> {
        let mut full = [84u8; 5];
        full[..n].copy_from_slice(&group[..n]);
        let v = match dec85_group(full) {
            Some(v) => v,
            None => berr!("base85 overflow in hunk starting at byte {}", groups * 5),
        };
        if n == 1 {
            berr!("Incomplete base85 group: 1 character groups are not allowed");
        }
        let b4 = v.to_be_bytes();
        let nb = if n == 5 { 4 } else { n - 1 };
        if canonical && n < 5 {
            let mut b = [0u8; 4];
            b[..nb].copy_from_slice(&b4[..nb]);
            let g = enc85_group(u32::from_be_bytes(b), &alpha);
            if g[..n] != raw[..n] {
                berr!("Non-canonical base85 encoding");
            }
        }
        out.extend_from_slice(&b4[..nb]);
        Ok(())
    };
    emit(d.len() / 5 * 4 + 4, |buf| {
        let mut out = OutCursor { buf, len: 0 };
        let mut group = [0u8; 5];
        let mut raw = [0u8; 5];
        let mut n = 0usize;
        let mut groups = 0usize;
        let mut i = 0usize;
        while i < d.len() {
            if n == 0 && i + 5 <= d.len() {
                // fast path: five alphabet characters in a row
                let t = [
                    table[d[i] as usize],
                    table[d[i + 1] as usize],
                    table[d[i + 2] as usize],
                    table[d[i + 3] as usize],
                    table[d[i + 4] as usize],
                ];
                if t.iter().all(|&x| x >= 0) {
                    if let Some(v) = dec85_group(t.map(|x| x as u8)) {
                        out.extend_from_slice(&v.to_be_bytes());
                        groups += 1;
                        i += 5;
                        continue;
                    }
                }
            }
            let c = d[i];
            let v = table[c as usize];
            if v < 0 {
                if !ign[c as usize] {
                    berr!("bad base85 character at position {}", i);
                }
                i += 1;
                continue;
            }
            i += 1;
            group[n] = v as u8;
            raw[n] = c;
            n += 1;
            if n == 5 {
                flush(&group, &raw, 5, groups, &mut out)?;
                groups += 1;
                n = 0;
            }
        }
        if n != 0 {
            flush(&group, &raw, n, groups, &mut out)?;
        }
        Ok(out.len)
    })
}

// ------------------------------------------------------------------ uu

fn a2b_uu(_m: &PyObject, data: &PyObject) -> PyResult {
    let d = ascii_data(data)?;
    if d.is_empty() {
        berr!("Empty input");
    }
    let mut bin_len = (d[0].wrapping_sub(b' ') & 0o77) as usize;
    let mut out = Vec::with_capacity(bin_len);
    let mut leftchar: u32 = 0;
    let mut leftbits = 0;
    let mut i = 1usize;
    while bin_len > 0 {
        let mut ch = if i < d.len() { d[i] } else { 0 };
        if i >= d.len() || ch == b'\n' || ch == b'\r' {
            ch = 0;
        } else {
            if ch < b' ' || ch > b' ' + 64 {
                berr!("Illegal char");
            }
            ch = (ch - b' ') & 0o77;
        }
        leftchar = (leftchar << 6) | ch as u32;
        leftbits += 6;
        if leftbits >= 8 {
            leftbits -= 8;
            out.push((leftchar >> leftbits) as u8);
            leftchar &= (1 << leftbits) - 1;
            bin_len -= 1;
        }
        i += 1;
    }
    while i < d.len() {
        let ch = d[i];
        if ch != b' ' && ch != b' ' + 64 && ch != b'\n' && ch != b'\r' {
            berr!("Trailing garbage");
        }
        i += 1;
    }
    bytes_ret(out)
}

fn b2a_uu(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("b2a_uu", &["data", "backtick"], 1, 1).kwonly(1);
    let [data, backtick] = args::parse::<2>(&P, a, kw)?;
    let d = bin_data(data.unwrap())?;
    let backtick = bool_arg(backtick, false)?;
    if d.len() > 45 {
        berr!("At most 45 bytes at once");
    }
    let enc = |v: u8| -> u8 {
        if backtick && v == 0 { b'`' } else { b' ' + v }
    };
    let mut out = Vec::with_capacity(2 + (d.len() + 2) / 3 * 4);
    out.push(enc(d.len() as u8 & 0o77));
    for ch in d.chunks(3) {
        let b0 = ch[0] as u32;
        let b1 = *ch.get(1).unwrap_or(&0) as u32;
        let b2 = *ch.get(2).unwrap_or(&0) as u32;
        let v = (b0 << 16) | (b1 << 8) | b2;
        for k in (0..4).rev() {
            out.push(enc(((v >> (6 * k)) & 63) as u8));
        }
    }
    out.push(b'\n');
    bytes_ret(out)
}

// ------------------------------------------------------------------ hex

/// Hex digit values; 0xff for non-digits.
const HEXVAL: [u8; 256] = {
    let mut t = [0xffu8; 256];
    let mut i = 0;
    while i < 256 {
        let c = i as u8;
        t[i] = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => 0xff,
        };
        i += 1;
    }
    t
};

fn hexval(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn a2b_hex(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("a2b_hex", &["hexstr", "ignorechars"], 1, 1).kwonly(1);
    let [data, ignorechars] = args::parse::<2>(&P, a, kw)?;
    let d = ascii_data(data.unwrap())?;
    let ign = ignore_set(ignorechars)?;
    if ign.is_none() {
        // without ignorable characters an odd length is always the error reported
        if d.len() % 2 == 1 {
            berr!("Odd-length string");
        }
        return emit(d.len() / 2, |out| {
            for (dst, p) in out.iter_mut().zip(d.chunks_exact(2)) {
                let hi = HEXVAL[p[0] as usize];
                let lo = HEXVAL[p[1] as usize];
                if (hi | lo) & 0x80 != 0 {
                    berr!("Non-hexadecimal digit found");
                }
                *dst = (hi << 4) | lo;
            }
            Ok(d.len() / 2)
        });
    }
    let mut digits = Vec::with_capacity(d.len());
    for &c in d.iter() {
        match hexval(c) {
            Some(v) => digits.push(v),
            None => {
                if let Some(s) = &ign {
                    if s[c as usize] {
                        continue;
                    }
                }
                if ign.is_none() && d.len() % 2 == 1 {
                    berr!("Odd-length string");
                }
                berr!("Non-hexadecimal digit found");
            }
        }
    }
    if digits.len() % 2 == 1 {
        berr!("Odd-length string");
    }
    let out: Vec<u8> = digits.chunks(2).map(|p| (p[0] << 4) | p[1]).collect();
    bytes_ret(out)
}

/// bytes.hex()-style formatting with optional separator.
pub fn hex_with_sep(d: &[u8], sep: Option<u8>, bytes_per_sep: i64) -> Vec<u8> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = Vec::with_capacity(d.len() * 3);
    let group = bytes_per_sep.unsigned_abs() as usize;
    let use_sep = sep.is_some() && group > 0 && d.len() > 0;
    for (i, &b) in d.iter().enumerate() {
        if use_sep && i > 0 {
            let boundary = if bytes_per_sep > 0 { (d.len() - i) % group == 0 } else { i % group == 0 };
            if boundary {
                out.push(sep.unwrap());
            }
        }
        out.push(HEX[(b >> 4) as usize]);
        out.push(HEX[(b & 15) as usize]);
    }
    out
}

fn b2a_hex(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("b2a_hex", &["data", "sep", "bytes_per_sep"], 0, 1);
    let [data, sep, bps] = args::parse::<3>(&P, a, kw)?;
    let d = bin_data(data.unwrap())?;
    if sep.is_none() {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        return emit(d.len() * 2, |out| {
            for (dst, &b) in out.chunks_exact_mut(2).zip(d.iter()) {
                dst[0] = HEX[(b >> 4) as usize];
                dst[1] = HEX[(b & 15) as usize];
            }
            Ok(d.len() * 2)
        });
    }
    let sep = match sep {
        None => None,
        Some(s) => {
            let v: Vec<u8> = if pystr::is_str(s) {
                let sd = pystr::str_data(s);
                if sd.len() != 1 {
                    raise!(ValueError, "sep must be length 1.");
                }
                let c = sd.get(0);
                if c >= 128 {
                    raise!(ValueError, "sep must be ASCII.");
                }
                vec![c as u8]
            } else {
                let b = bin_data(s)?;
                if b.len() != 1 {
                    raise!(ValueError, "sep must be length 1.");
                }
                if b[0] >= 128 {
                    raise!(ValueError, "sep must be ASCII.");
                }
                b.to_vec()
            };
            Some(v[0])
        }
    };
    let bps = match bps {
        None => 1,
        Some(b) => ab::as_i64(b)?,
    };
    bytes_ret(hex_with_sep(&d, sep, bps))
}

// ------------------------------------------------------------------ quoted-printable

fn a2b_qp(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("a2b_qp", &["data", "header"], 0, 1);
    let [data, header] = args::parse::<2>(&P, a, kw)?;
    let d = ascii_data(data.unwrap())?;
    let header = bool_arg(header, false)?;
    let mut out = Vec::with_capacity(d.len());
    let n = d.len();
    let mut i = 0;
    while i < n {
        if d[i] == b'=' {
            i += 1;
            if i >= n {
                break;
            }
            if d[i] == b'\n' || d[i] == b'\r' {
                if d[i] != b'\n' {
                    while i < n && d[i] != b'\n' {
                        i += 1;
                    }
                }
                if i < n {
                    i += 1;
                }
            } else if d[i] == b'=' {
                out.push(b'=');
                i += 1;
            } else if i + 1 < n && hexval(d[i]).is_some() && hexval(d[i + 1]).is_some() {
                out.push((hexval(d[i]).unwrap() << 4) | hexval(d[i + 1]).unwrap());
                i += 2;
            } else {
                out.push(b'=');
            }
        } else if header && d[i] == b'_' {
            out.push(b' ');
            i += 1;
        } else {
            out.push(d[i]);
            i += 1;
        }
    }
    bytes_ret(out)
}

fn b2a_qp(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("b2a_qp", &["data", "quotetabs", "istext", "header"], 0, 1);
    let [data, quotetabs, istext, header] = args::parse::<4>(&P, a, kw)?;
    let d = bin_data(data.unwrap())?;
    let quotetabs = bool_arg(quotetabs, false)?;
    let istext = bool_arg(istext, true)?;
    let header = bool_arg(header, false)?;
    const MAXLINESIZE: usize = 76;
    const HEXU: &[u8; 16] = b"0123456789ABCDEF";
    let crlf = match d.iter().position(|&c| c == b'\n') {
        Some(p) => p > 0 && d[p - 1] == b'\r',
        None => false,
    };
    let n = d.len();
    let mut out: Vec<u8> = Vec::with_capacity(n * 3 / 2 + 8);
    let mut linelen = 0usize;
    let mut i = 0;
    let soft_break = |out: &mut Vec<u8>| {
        out.push(b'=');
        if crlf {
            out.push(b'\r');
        }
        out.push(b'\n');
    };
    while i < n {
        let c = d[i];
        let needs_quote = c > 126
            || c == b'='
            || (header && c == b'_')
            || (c == b'.' && linelen == 0 && (i + 1 == n || d[i + 1] == b'\n' || d[i + 1] == b'\r' || d[i + 1] == 0))
            || (!istext && (c == b'\r' || c == b'\n'))
            || ((c == b'\t' || c == b' ') && i + 1 == n)
            || (c < 33 && c != b'\r' && c != b'\n' && (quotetabs || (c != b'\t' && c != b' ')));
        if needs_quote {
            if linelen + 3 >= MAXLINESIZE {
                soft_break(&mut out);
                linelen = 0;
            }
            out.push(b'=');
            out.push(HEXU[(c >> 4) as usize]);
            out.push(HEXU[(c & 15) as usize]);
            i += 1;
            linelen += 3;
        } else if istext && (c == b'\n' || (i + 1 < n && c == b'\r' && d[i + 1] == b'\n')) {
            linelen = 0;
            if let Some(&last) = out.last() {
                if last == b' ' || last == b'\t' {
                    out.pop();
                    out.push(b'=');
                    out.push(HEXU[(last >> 4) as usize]);
                    out.push(HEXU[(last & 15) as usize]);
                }
            }
            if crlf {
                out.push(b'\r');
            }
            out.push(b'\n');
            i += if c == b'\r' { 2 } else { 1 };
        } else {
            if i + 1 != n && d[i + 1] != b'\n' && linelen + 1 >= MAXLINESIZE {
                soft_break(&mut out);
                linelen = 0;
            }
            linelen += 1;
            if header && c == b' ' {
                out.push(b'_');
            } else {
                out.push(c);
            }
            i += 1;
        }
    }
    bytes_ret(out)
}

// ------------------------------------------------------------------ CRCs

static CRC32_TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();

pub fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    let t = CRC32_TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for i in 0..256u32 {
            let mut c = i;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb88320 ^ (c >> 1) } else { c >> 1 };
            }
            t[i as usize] = c;
        }
        t
    });
    crc = !crc;
    for &b in data {
        crc = t[((crc ^ b as u32) & 0xff) as usize] ^ (crc >> 8);
    }
    !crc
}

fn crc_arg(o: Option<&PyObject>) -> PyResult<u64> {
    match o {
        None => Ok(0),
        Some(o) => {
            let i = ab::index(o)?;
            Ok(rpy_bigint::conv::to_u64_wrapping(int_methods::iref(&i)))
        }
    }
}

fn crc32(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("crc32", a, 1, 2)?;
    let d = bin_data(a[0])?;
    let crc = crc_arg(a.get(1).copied())? as u32;
    Ok(int::new_int(crc32_update(crc, &d) as i64))
}

fn crc_hqx(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("crc_hqx", a, 2, 2)?;
    let d = bin_data(a[0])?;
    let mut crc = (crc_arg(Some(a[1]))? & 0xffff) as u16;
    for &b in d.iter() {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    Ok(int::new_int(crc as i64))
}

// ------------------------------------------------------------------ module

fn new_exc(name: &str, base: *mut crate::ffi::PyTypeObject) -> PyResult {
    let ns = dict::new_dict();
    dict::set_item_str(&ns, "__module__", &pystr::new_str("binascii"))?;
    let bases = tuple::new_tuple(vec![type_ref(base)]);
    ab::call(type_obj(type_type()), &[&pystr::new_str(name), &bases, &ns])
}

pub fn init_module() -> PyResult {
    let m = crate::types::module::new_module("binascii")?;
    let d = crate::types::module::module_dict(&m);
    dict::set_item_str(&d, "__doc__", &pystr::new_str("Conversion between binary data and ASCII"))?;
    if ERROR.load(Ordering::Acquire).is_null() {
        let e = new_exc("Error", crate::types::exceptions::ValueError())?;
        ERROR.store(e.into_raw(), Ordering::Release);
        let i = new_exc("Incomplete", crate::types::exceptions::Exception())?;
        INCOMPLETE.store(i.into_raw(), Ordering::Release);
    }
    unsafe {
        dict::set_item_str(&d, "Error", &PyRef::from_borrowed(ERROR.load(Ordering::Acquire)))?;
        dict::set_item_str(&d, "Incomplete", &PyRef::from_borrowed(INCOMPLETE.load(Ordering::Acquire)))?;
    }
    for (k, v) in [
        ("BASE64_ALPHABET", BASE64),
        ("URLSAFE_BASE64_ALPHABET", URLSAFE64),
        ("CRYPT_ALPHABET", CRYPT),
        ("BINHEX_ALPHABET", BINHEX),
        ("BASE85_ALPHABET", BASE85),
        ("Z85_ALPHABET", Z85),
        ("BASE32_ALPHABET", BASE32),
        ("BASE32HEX_ALPHABET", BASE32HEX),
    ] {
        dict::set_item_str(&d, k, &bytes::new_bytes(v))?;
    }
    let uu: Vec<u8> = (32u8..96).collect();
    dict::set_item_str(&d, "UU_ALPHABET", &bytes::new_bytes(&uu))?;
    let a85: Vec<u8> = (33u8..118).collect();
    dict::set_item_str(&d, "ASCII85_ALPHABET", &bytes::new_bytes(&a85))?;
    let methods = crate::mdefs![
        ("a2b_uu", O, a2b_uu, "a2b_uu($module, data, /)\n--\n\nDecode a line of uuencoded data."),
        ("b2a_uu", FASTKW, b2a_uu, "b2a_uu($module, data, /, *, backtick=False)\n--\n\nUuencode line of data."),
        ("a2b_base64", FASTKW, a2b_base64, "a2b_base64($module, data, /, *, strict_mode=<unrepresentable>, padded=True, alphabet=BASE64_ALPHABET, ignorechars=<unrepresentable>, canonical=False)\n--\n\nDecode a line of base64 data."),
        ("b2a_base64", FASTKW, b2a_base64, "b2a_base64($module, data, /, *, wrapcol=0, newline=True, padded=True, alphabet=BASE64_ALPHABET)\n--\n\nBase64-code line of data."),
        ("a2b_base32", FASTKW, a2b_base32, "a2b_base32($module, data, /, *, padded=True, alphabet=BASE32_ALPHABET, ignorechars=b'', canonical=False)\n--\n\nDecode a line of base32 data."),
        ("b2a_base32", FASTKW, b2a_base32, "b2a_base32($module, data, /, *, padded=True, wrapcol=0, alphabet=BASE32_ALPHABET)\n--\n\nBase32-code line of data."),
        ("a2b_ascii85", FASTKW, a2b_ascii85, "a2b_ascii85($module, data, /, *, foldspaces=False, adobe=False, ignorechars=b'', canonical=False)\n--\n\nDecode Ascii85 data."),
        ("b2a_ascii85", FASTKW, b2a_ascii85, "b2a_ascii85($module, data, /, *, foldspaces=False, wrapcol=0, pad=False, adobe=False)\n--\n\nAscii85-encode data."),
        ("a2b_base85", FASTKW, a2b_base85, "a2b_base85($module, data, /, *, alphabet=BASE85_ALPHABET, ignorechars=b'', canonical=False)\n--\n\nDecode a line of base85 data."),
        ("b2a_base85", FASTKW, b2a_base85, "b2a_base85($module, data, /, *, pad=False, wrapcol=0, alphabet=BASE85_ALPHABET)\n--\n\nBase85-code line of data."),
        ("a2b_hex", FASTKW, a2b_hex, "a2b_hex($module, hexstr, /, *, ignorechars=<unrepresentable>)\n--\n\nBinary data of hexadecimal representation."),
        ("unhexlify", FASTKW, a2b_hex, "unhexlify($module, hexstr, /, *, ignorechars=<unrepresentable>)\n--\n\nBinary data of hexadecimal representation."),
        ("b2a_hex", FASTKW, b2a_hex, "b2a_hex($module, /, data, sep=<unrepresentable>, bytes_per_sep=1)\n--\n\nHexadecimal representation of binary data."),
        ("hexlify", FASTKW, b2a_hex, "hexlify($module, /, data, sep=<unrepresentable>, bytes_per_sep=1)\n--\n\nHexadecimal representation of binary data."),
        ("a2b_qp", FASTKW, a2b_qp, "a2b_qp($module, /, data, header=False)\n--\n\nDecode a string of qp-encoded data."),
        ("b2a_qp", FASTKW, b2a_qp, "b2a_qp($module, /, data, quotetabs=False, istext=True, header=False)\n--\n\nEncode a string using quoted-printable encoding."),
        ("crc32", FAST, crc32, "crc32($module, data, crc=0, /)\n--\n\nCompute CRC-32 incrementally."),
        ("crc_hqx", FAST, crc_hqx, "crc_hqx($module, data, crc, /)\n--\n\nCompute CRC-CCITT incrementally."),
    ];
    crate::builtins::add_methods_to_dict(&d, methods, Some(&m))?;
    Ok(m)
}
