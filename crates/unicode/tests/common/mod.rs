//! Helpers shared by the integration tests: a SHA-1 implementation, CPython's
//! `%.12g` formatting and Python-level `str` method emulation on code point
//! vectors.
#![allow(dead_code)]

use rpy_unicode as u;
use std::path::PathBuf;

pub fn data_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

pub fn spec_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../spec").join(rel)
}

// ---------------------------------------------------------------- SHA-1
pub struct Sha1 {
    h: [u32; 5],
    buf: Vec<u8>,
    len: u64,
}

impl Sha1 {
    pub fn new() -> Sha1 {
        Sha1 { h: [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0], buf: Vec::new(), len: 0 }
    }
    fn block(&mut self, b: &[u8]) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b_, mut c, mut d, mut e] = self.h;
        for i in 0..80 {
            let (f, k) = match i {
                0..=19 => ((b_ & c) | (!b_ & d), 0x5A827999),
                20..=39 => (b_ ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b_ & c) | (b_ & d) | (c & d), 0x8F1BBCDC),
                _ => (b_ ^ c ^ d, 0xCA62C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(w[i]);
            e = d;
            d = c;
            c = b_.rotate_left(30);
            b_ = a;
            a = t;
        }
        self.h[0] = self.h[0].wrapping_add(a);
        self.h[1] = self.h[1].wrapping_add(b_);
        self.h[2] = self.h[2].wrapping_add(c);
        self.h[3] = self.h[3].wrapping_add(d);
        self.h[4] = self.h[4].wrapping_add(e);
    }
    pub fn update(&mut self, data: &[u8]) {
        self.len += data.len() as u64;
        self.buf.extend_from_slice(data);
        let n = self.buf.len() / 64 * 64;
        let buf = std::mem::take(&mut self.buf);
        for chunk in buf[..n].chunks(64) {
            self.block(chunk);
        }
        self.buf = buf[n..].to_vec();
    }
    pub fn hexdigest(mut self) -> String {
        let bits = self.len * 8;
        let mut pad = vec![0x80u8];
        while (self.buf.len() + pad.len()) % 64 != 56 {
            pad.push(0);
        }
        pad.extend_from_slice(&bits.to_be_bytes());
        let len = self.len;
        self.update(&pad);
        self.len = len;
        assert!(self.buf.is_empty());
        self.h.iter().map(|x| format!("{x:08x}")).collect()
    }
}

// ---------------------------------------------------------------- %.12g
pub fn fmt_g(v: f64, prec: usize) -> String {
    if v == 0.0 {
        return if v.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let e = format!("{:.*e}", prec - 1, v);
    let (mant, exp) = e.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let trim = |s: &str| -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s.to_string()
        }
    };
    if exp < -4 || exp >= prec as i32 {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{}{:02}", trim(mant), sign, exp.abs())
    } else {
        trim(&format!("{:.*}", (prec as i32 - 1 - exp) as usize, v))
    }
}

// ------------------------------------------------- UTF-8 (surrogatepass)
pub fn encode(s: &[u32], out: &mut Vec<u8>) {
    for &c in s {
        if c < 0x80 {
            out.push(c as u8);
        } else if c < 0x800 {
            out.push(0xC0 | (c >> 6) as u8);
            out.push(0x80 | (c & 0x3F) as u8);
        } else if c < 0x10000 {
            out.push(0xE0 | (c >> 12) as u8);
            out.push(0x80 | ((c >> 6) & 0x3F) as u8);
            out.push(0x80 | (c & 0x3F) as u8);
        } else {
            out.push(0xF0 | (c >> 18) as u8);
            out.push(0x80 | ((c >> 12) & 0x3F) as u8);
            out.push(0x80 | ((c >> 6) & 0x3F) as u8);
            out.push(0x80 | (c & 0x3F) as u8);
        }
    }
}

pub fn s(text: &str) -> Vec<u32> {
    text.chars().map(|c| c as u32).collect()
}

// ------------------------------------------------- Python str emulation
pub fn lower(s: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut buf = [0; 3];
    for i in 0..s.len() {
        let n = u::lower_full_at(s, i, &mut buf);
        out.extend_from_slice(&buf[..n]);
    }
    out
}

pub fn upper(s: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut buf = [0; 3];
    for &c in s {
        let n = u::to_upper_full(c, &mut buf);
        out.extend_from_slice(&buf[..n]);
    }
    out
}

pub fn casefold(s: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut buf = [0; 3];
    for &c in s {
        let n = u::casefold_full(c, &mut buf);
        out.extend_from_slice(&buf[..n]);
    }
    out
}

pub fn title(s: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut buf = [0; 3];
    let mut prev_cased = false;
    for i in 0..s.len() {
        let c = s[i];
        let n = if prev_cased { u::lower_full_at(s, i, &mut buf) } else { u::to_title_full(c, &mut buf) };
        out.extend_from_slice(&buf[..n]);
        prev_cased = u::is_cased(c);
    }
    out
}

pub fn capitalize(s: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut buf = [0; 3];
    for i in 0..s.len() {
        let n = if i == 0 { u::to_title_full(s[0], &mut buf) } else { u::lower_full_at(s, i, &mut buf) };
        out.extend_from_slice(&buf[..n]);
    }
    out
}

pub fn swapcase(s: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut buf = [0; 3];
    for i in 0..s.len() {
        let c = s[i];
        let n = if u::is_uppercase(c) {
            u::lower_full_at(s, i, &mut buf)
        } else if u::is_lowercase(c) {
            u::to_upper_full(c, &mut buf)
        } else {
            buf[0] = c;
            1
        };
        out.extend_from_slice(&buf[..n]);
    }
    out
}

fn all(s: &[u32], f: fn(u32) -> bool) -> bool {
    !s.is_empty() && s.iter().all(|&c| f(c))
}
pub fn isalnum(s: &[u32]) -> bool { all(s, u::is_alnum) }
pub fn isalpha(s: &[u32]) -> bool { all(s, u::is_alpha) }
pub fn isdecimal(s: &[u32]) -> bool { all(s, u::is_decimal) }
pub fn isdigit(s: &[u32]) -> bool { all(s, u::is_digit) }
pub fn isnumeric(s: &[u32]) -> bool { all(s, u::is_numeric) }
pub fn isspace(s: &[u32]) -> bool { all(s, u::is_space) }
pub fn isprintable(s: &[u32]) -> bool { s.iter().all(|&c| u::is_printable(c)) }

pub fn islower(s: &[u32]) -> bool {
    if s.len() == 1 {
        return u::is_lowercase(s[0]);
    }
    let mut cased = false;
    for &c in s {
        if u::is_uppercase(c) || u::is_titlecase(c) {
            return false;
        } else if !cased && u::is_lowercase(c) {
            cased = true;
        }
    }
    cased
}

pub fn isupper(s: &[u32]) -> bool {
    if s.len() == 1 {
        return u::is_uppercase(s[0]);
    }
    let mut cased = false;
    for &c in s {
        if u::is_lowercase(c) || u::is_titlecase(c) {
            return false;
        } else if !cased && u::is_uppercase(c) {
            cased = true;
        }
    }
    cased
}

pub fn istitle(s: &[u32]) -> bool {
    if s.len() == 1 {
        return u::is_titlecase(s[0]) || u::is_uppercase(s[0]);
    }
    let mut cased = false;
    let mut prev = false;
    for &c in s {
        if u::is_uppercase(c) || u::is_titlecase(c) {
            if prev {
                return false;
            }
            prev = true;
            cased = true;
        } else if u::is_lowercase(c) {
            if !prev {
                return false;
            }
            prev = true;
            cased = true;
        } else {
            prev = false;
        }
    }
    cased
}
