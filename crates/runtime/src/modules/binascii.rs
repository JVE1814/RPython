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

/// a2b input: ASCII str or bytes-like.
fn ascii_data(o: &PyObject) -> PyResult<Vec<u8>> {
    if pystr::is_str(o) {
        let d = pystr::str_data(o);
        let mut v = Vec::with_capacity(d.len());
        for c in d.iter() {
            if c >= 128 {
                raise!(ValueError, "string argument should contain only ASCII characters");
            }
            v.push(c as u8);
        }
        return Ok(v);
    }
    let b = Buf::get(o, false)?;
    Ok(b.data().to_vec())
}

/// b2a input: bytes-like only.
fn bin_data(o: &PyObject) -> PyResult<Vec<u8>> {
    let b = Buf::get(o, false)?;
    Ok(b.data().to_vec())
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
                bin_data(o)?
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
    let alpha = alphabet_arg(alphabet, BASE64, 64, false)?;
    let mut out = Vec::with_capacity(d.len() / 3 * 4 + 6);
    for ch in d.chunks(3) {
        let b0 = ch[0] as u32;
        let b1 = *ch.get(1).unwrap_or(&0) as u32;
        let b2 = *ch.get(2).unwrap_or(&0) as u32;
        let v = (b0 << 16) | (b1 << 8) | b2;
        out.push(alpha[(v >> 18) as usize & 63]);
        out.push(alpha[(v >> 12) as usize & 63]);
        if ch.len() > 1 {
            out.push(alpha[(v >> 6) as usize & 63]);
        } else if padded {
            out.push(b'=');
        }
        if ch.len() > 2 {
            out.push(alpha[v as usize & 63]);
        } else if padded {
            out.push(b'=');
        }
    }
    let width = if wrapcol == 0 { 0 } else { (wrapcol / 4 * 4).max(4) };
    let mut out = wrap_lines(out, width);
    if newline {
        out.push(b'\n');
    }
    bytes_ret(out)
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
    let alpha = alphabet_arg(alphabet, BASE64, 64, true)?;
    let table = decode_table(&alpha);
    let ign = ign.unwrap_or([false; 256]);
    let eq_ignorable = ign[b'=' as usize];

    let mut out = Vec::with_capacity(d.len() / 4 * 3 + 3);
    let mut st = PadState { quad_pos: 0, pads: 0, padding_started: false, complete: false };
    let mut leftchar: u32 = 0;
    let mut ndata: usize = 0;
    let mut pads_after_last: usize = 0;
    for &c in &d {
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
    bytes_ret(out)
}

// ------------------------------------------------------------------ base32

fn b2a_base32(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("b2a_base32", &["data", "padded", "wrapcol", "alphabet"], 1, 1).kwonly(3);
    let [data, padded, wrapcol, alphabet] = args::parse::<4>(&P, a, kw)?;
    let d = bin_data(data.unwrap())?;
    let padded = bool_arg(padded, true)?;
    let wrapcol = wrapcol_arg(wrapcol)?;
    let alpha = alphabet_arg(alphabet, BASE32, 32, false)?;
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
    let width = if wrapcol == 0 { 0 } else { (wrapcol / 8 * 8).max(8) };
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
    let mut out = Vec::with_capacity(d.len() / 8 * 5 + 5);
    let mut pos: usize = 0; // chars in current group
    let mut acc: u64 = 0;
    let mut ndata: usize = 0;
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
    for &c in &d {
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
    bytes_ret(out)
}

// ------------------------------------------------------------------ ascii85 / base85

fn enc85_group(v: u32, alpha: &[u8]) -> [u8; 5] {
    let mut r = [0u8; 5];
    let mut x = v;
    for k in (0..5).rev() {
        r[k] = alpha[(x % 85) as usize];
        x /= 85;
    }
    r
}

fn b2a_ascii85(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("b2a_ascii85", &["data", "foldspaces", "wrapcol", "pad", "adobe"], 1, 1).kwonly(4);
    let [data, foldspaces, wrapcol, pad, adobe] = args::parse::<5>(&P, a, kw)?;
    let d = bin_data(data.unwrap())?;
    let foldspaces = bool_arg(foldspaces, false)?;
    let wrapcol = wrapcol_arg(wrapcol)?;
    let pad = bool_arg(pad, false)?;
    let adobe = bool_arg(adobe, false)?;
    let alpha: Vec<u8> = (33u8..118).collect();
    let mut chars = Vec::with_capacity(d.len() / 4 * 5 + 5);
    for ch in d.chunks(4) {
        let mut buf = [0u8; 4];
        buf[..ch.len()].copy_from_slice(ch);
        let v = u32::from_be_bytes(buf);
        if ch.len() == 4 || pad {
            if v == 0 {
                chars.push(b'z');
                continue;
            }
            if foldspaces && v == 0x20202020 {
                chars.push(b'y');
                continue;
            }
            chars.extend_from_slice(&enc85_group(v, &alpha));
        } else {
            let g = enc85_group(v, &alpha);
            chars.extend_from_slice(&g[..ch.len() + 1]);
        }
    }
    // tokens: "<~" + chars + "~>" with unsplittable delimiters
    let mut out = Vec::with_capacity(chars.len() + 8);
    if wrapcol == 0 {
        if adobe {
            out.extend_from_slice(b"<~");
        }
        out.extend_from_slice(&chars);
        if adobe {
            out.extend_from_slice(b"~>");
        }
        return bytes_ret(out);
    }
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
    let mut d = ascii_data(data.unwrap())?;
    let foldspaces = bool_arg(foldspaces, false)?;
    let adobe = bool_arg(adobe, false)?;
    let ign = ignore_set(ignorechars)?.unwrap_or([false; 256]);
    let canonical = bool_arg(canonical, false)?;
    if adobe {
        if !d.ends_with(b"~>") {
            berr!("Ascii85 encoded byte sequences must end with b'~>'");
        }
        d.truncate(d.len() - 2);
        if d.starts_with(b"<~") {
            d.drain(..2);
        }
    }
    let mut out = Vec::with_capacity(d.len() / 5 * 4 + 4);
    let mut group: Vec<u8> = Vec::with_capacity(5);
    let mut grp_start_chars: Vec<u8> = Vec::new();
    let flush = |group: &[u8], out: &mut Vec<u8>, raw: &[u8], last: bool| -> PyResult<()> {
        let n = group.len();
        let mut v: u64 = 0;
        for k in 0..5 {
            let digit = if k < n { group[k] as u64 } else { 84 };
            v = v * 85 + digit;
        }
        if v > 0xffff_ffff {
            berr!("Ascii85 overflow");
        }
        if n == 1 {
            berr!("Incomplete Ascii85 group: 1 character groups are not allowed");
        }
        let bytes4 = (v as u32).to_be_bytes();
        let nb = if n == 5 { 4 } else { n - 1 };
        if canonical {
            if n == 5 && v == 0 {
                berr!("Non-canonical Ascii85 encoding: zero group must be encoded as b'z'");
            }
            if n < 5 {
                let mut b = [0u8; 4];
                b[..nb].copy_from_slice(&bytes4[..nb]);
                let alpha: Vec<u8> = (33u8..118).collect();
                let g = enc85_group(u32::from_be_bytes(b), &alpha);
                if &g[..n] != raw {
                    berr!("Non-canonical Ascii85 encoding");
                }
            }
        }
        let _ = last;
        out.extend_from_slice(&bytes4[..nb]);
        Ok(())
    };
    for &c in &d {
        if (b'!'..=b'u').contains(&c) {
            group.push(c - 33);
            grp_start_chars.push(c);
            if group.len() == 5 {
                flush(&group, &mut out, &grp_start_chars, false)?;
                group.clear();
                grp_start_chars.clear();
            }
        } else if c == b'z' {
            if !group.is_empty() {
                berr!("'z' inside Ascii85 5-tuple");
            }
            out.extend_from_slice(&[0, 0, 0, 0]);
        } else if c == b'y' && foldspaces {
            if !group.is_empty() {
                berr!("'y' inside Ascii85 5-tuple");
            }
            out.extend_from_slice(b"    ");
        } else if ign[c as usize] {
            continue;
        } else {
            berr!("Non-Ascii85 digit found: {}", c as char);
        }
    }
    if !group.is_empty() {
        flush(&group, &mut out, &grp_start_chars, true)?;
    }
    bytes_ret(out)
}

fn b2a_base85(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("b2a_base85", &["data", "pad", "wrapcol", "alphabet"], 1, 1).kwonly(3);
    let [data, pad, wrapcol, alphabet] = args::parse::<4>(&P, a, kw)?;
    let d = bin_data(data.unwrap())?;
    let pad = bool_arg(pad, false)?;
    let wrapcol = wrapcol_arg(wrapcol)?;
    let alpha = alphabet_arg(alphabet, BASE85, 85, false)?;
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
    let width = if wrapcol == 0 { 0 } else { (wrapcol / 5 * 5).max(5) };
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
    let mut out = Vec::with_capacity(d.len() / 5 * 4 + 4);
    let mut group: Vec<u8> = Vec::with_capacity(5);
    let mut raw: Vec<u8> = Vec::with_capacity(5);
    let mut nbytes_out = 0usize;
    let mut flush = |group: &[u8], raw: &[u8], out: &mut Vec<u8>| -> PyResult<()> {
        let n = group.len();
        let mut v: u64 = 0;
        for k in 0..5 {
            v = v * 85 + if k < n { group[k] as u64 } else { 84 };
        }
        if v > 0xffff_ffff {
            berr!("base85 overflow in hunk starting at byte {}", nbytes_out);
        }
        if n == 1 {
            berr!("Incomplete base85 group: 1 character groups are not allowed");
        }
        let b4 = (v as u32).to_be_bytes();
        let nb = if n == 5 { 4 } else { n - 1 };
        if canonical && n < 5 {
            let mut b = [0u8; 4];
            b[..nb].copy_from_slice(&b4[..nb]);
            let g = enc85_group(u32::from_be_bytes(b), &alpha);
            if &g[..n] != raw {
                berr!("Non-canonical base85 encoding");
            }
        }
        out.extend_from_slice(&b4[..nb]);
        nbytes_out += 5;
        Ok(())
    };
    for (i, &c) in d.iter().enumerate() {
        let v = table[c as usize];
        if v < 0 {
            if ign[c as usize] {
                continue;
            }
            berr!("bad base85 character at position {}", i);
        }
        group.push(v as u8);
        raw.push(c);
        if group.len() == 5 {
            flush(&group, &raw, &mut out)?;
            group.clear();
            raw.clear();
        }
    }
    if !group.is_empty() {
        flush(&group, &raw, &mut out)?;
    }
    bytes_ret(out)
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
    let mut digits = Vec::with_capacity(d.len());
    for &c in &d {
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
                b
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
    for &b in &d {
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
