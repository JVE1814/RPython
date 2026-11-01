//! String -> float / complex conversion with Python semantics.

use crate::NumFmtError;

/// Message used for `float(str)` failures.  CPython's message is
/// `could not convert string to float: <repr(arg)>`; the runtime must append
/// `": " + repr(original_argument)`.
pub const FLOAT_PARSE_ERROR: &str = "could not convert string to float";
/// Message used for `complex(str)` failures.
pub const COMPLEX_PARSE_ERROR: &str = "complex() arg is a malformed string";

#[inline]
fn py_isspace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r')
}

#[inline]
fn eq_ci(s: &[u8], at: usize, word: &[u8]) -> bool {
    s.len() >= at + word.len() && s[at..at + word.len()].eq_ignore_ascii_case(word)
}

/// Parse the longest prefix of `s` that is a valid float
/// (`[sign] (digits [. digits] | . digits) [e [sign] digits] | [sign] inf[inity] | [sign] nan`).
/// Returns the value and the number of bytes consumed, or None if no prefix
/// is valid.  Never skips whitespace.  Overflow gives +-inf, underflow 0.
pub fn parse_float_prefix(s: &[u8]) -> Option<(f64, usize)> {
    let mut i = 0;
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    if eq_ci(s, i, b"inf") {
        i += 3;
        if eq_ci(s, i, b"inity") {
            i += 5;
        }
        return Some((if neg { f64::NEG_INFINITY } else { f64::INFINITY }, i));
    }
    if eq_ci(s, i, b"nan") {
        let nan = if neg { -f64::NAN } else { f64::NAN };
        // make the sign explicit regardless of the platform NAN constant
        let nan = f64::from_bits((nan.to_bits() & !(1 << 63)) | ((neg as u64) << 63));
        return Some((nan, i + 3));
    }
    let num_start = i;
    let mut ndig = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        i += 1;
        ndig += 1;
    }
    if i < s.len() && s[i] == b'.' {
        let save = i;
        i += 1;
        let mut nfrac = 0;
        while i < s.len() && s[i].is_ascii_digit() {
            i += 1;
            nfrac += 1;
        }
        if ndig == 0 && nfrac == 0 {
            let _ = save;
            return None;
        }
        ndig += nfrac;
    }
    if ndig == 0 {
        return None;
    }
    if i < s.len() && (s[i] == b'e' || s[i] == b'E') {
        let mut j = i + 1;
        if j < s.len() && (s[j] == b'+' || s[j] == b'-') {
            j += 1;
        }
        let es = j;
        while j < s.len() && s[j].is_ascii_digit() {
            j += 1;
        }
        if j > es {
            i = j;
        }
    }
    let text = &s[num_start..i];
    // SAFETY: validated ASCII.
    let text = unsafe { core::str::from_utf8_unchecked(text) };
    let v: f64 = text.parse().ok()?;
    Some((if neg { -v } else { v }, i))
}

/// Remove underscores, checking that each one sits between two ASCII digits
/// (`_Py_string_to_number_with_underscores`).  Returns None if invalid.
fn strip_underscores(s: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len());
    let mut prev = 0u8;
    for &c in s {
        if c == b'_' {
            if !prev.is_ascii_digit() {
                return None;
            }
        } else {
            if prev == b'_' && !c.is_ascii_digit() {
                return None;
            }
            out.push(c);
        }
        prev = c;
    }
    if prev == b'_' {
        return None;
    }
    Some(out)
}

fn trim_ascii_space(s: &[u8]) -> &[u8] {
    let mut a = 0;
    let mut b = s.len();
    while a < b && py_isspace(s[a]) {
        a += 1;
    }
    while b > a && py_isspace(s[b - 1]) {
        b -= 1;
    }
    &s[a..b]
}

fn float_inner(s: &[u8]) -> Option<f64> {
    let t = trim_ascii_space(s);
    if t.is_empty() {
        return None;
    }
    match parse_float_prefix(t) {
        Some((v, n)) if n == t.len() => Some(v),
        _ => None,
    }
}

/// `float(b)` for a bytes-like argument (or an ASCII-transformed str):
/// surrounding ASCII whitespace, underscores between digits, inf/nan.
/// On failure returns a ValueError whose message must be completed by the
/// caller (see [`FLOAT_PARSE_ERROR`]).
pub fn parse_float_bytes(s: &[u8]) -> Result<f64, NumFmtError> {
    let r = if s.contains(&b'_') {
        strip_underscores(s).and_then(|v| float_inner(&v))
    } else {
        float_inner(s)
    };
    r.ok_or_else(|| NumFmtError::value(FLOAT_PARSE_ERROR))
}

/// CPython's `_PyUnicode_TransformDecimalAndSpaceToASCII`: map non-ASCII
/// whitespace to ' ', non-ASCII decimal digits to ASCII digits, and
/// anything else non-ASCII to '?' (which then fails to parse).
pub fn transform_decimal_and_space(
    s: &str,
    is_space: &dyn Fn(char) -> bool,
    decimal: &dyn Fn(char) -> Option<u32>,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for c in s.chars() {
        if (c as u32) < 127 {
            out.push(c as u8);
        } else if is_space(c) {
            out.push(b' ');
        } else if let Some(d) = decimal(c) {
            out.push(b'0' + d as u8);
        } else {
            out.push(b'?');
            break;
        }
    }
    out
}

/// Zero code points of the Unicode decimal digit (Nd) blocks; used by the
/// default digit mapping.  The runtime should pass the Unicode database's
/// `decimal()` instead where available.
const ND_ZEROS: &[u32] = &[
    0x0660, 0x06F0, 0x07C0, 0x0966, 0x09E6, 0x0A66, 0x0AE6, 0x0B66, 0x0BE6, 0x0C66, 0x0CE6,
    0x0D66, 0x0DE6, 0x0E50, 0x0ED0, 0x0F20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0,
    0x1A80, 0x1A90, 0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0,
    0xAA50, 0xABF0, 0xFF10, 0x104A0, 0x10D30, 0x11066, 0x110F0, 0x11136, 0x111D0, 0x112F0,
    0x11450, 0x114D0, 0x11650, 0x116C0, 0x11730, 0x118E0, 0x11950, 0x11C50, 0x11D50, 0x11DA0,
    0x16A60, 0x16AC0, 0x16B50, 0x1D7CE, 0x1D7D8, 0x1D7E2, 0x1D7EC, 0x1D7F6, 0x1E140, 0x1E2F0,
    0x1E950, 0x1FBF0,
];

/// Default non-ASCII decimal digit mapping (Unicode Nd blocks).
pub fn default_decimal(c: char) -> Option<u32> {
    let cp = c as u32;
    for &z in ND_ZEROS {
        if cp >= z && cp < z + 10 {
            return Some(cp - z);
        }
    }
    None
}

/// Default whitespace predicate (Unicode White_Space; matches
/// `str.isspace()` for all non-ASCII characters).
pub fn default_is_space(c: char) -> bool {
    c.is_whitespace() || matches!(c as u32, 0x1c..=0x1f)
}

/// `float(s)` for a str, with explicit Unicode predicates.
pub fn parse_float_with(
    s: &str,
    is_space: &dyn Fn(char) -> bool,
    decimal: &dyn Fn(char) -> Option<u32>,
) -> Result<f64, NumFmtError> {
    if s.is_ascii() {
        return parse_float_bytes(s.as_bytes());
    }
    parse_float_bytes(&transform_decimal_and_space(s, is_space, decimal))
}

/// `float(s)` for a str using the built-in Unicode approximations.
/// On failure the message is [`FLOAT_PARSE_ERROR`] and the caller appends
/// `": " + repr(s)`.
pub fn parse_float(s: &str) -> Result<f64, NumFmtError> {
    parse_float_with(s, &default_is_space, &default_decimal)
}

/// Parse a Python float *literal* as accepted by the tokenizer
/// (`digitpart | pointfloat | exponentfloat`, single underscores between
/// digits, no sign, no whitespace, no inf/nan).  Used for float literals
/// and the numeric part of imaginary literals.
pub fn parse_float_literal(s: &str) -> Result<f64, NumFmtError> {
    let err = || NumFmtError::value(format!("invalid float literal '{}'", s));
    let b = s.as_bytes();
    let mut clean: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    // digitpart: digit (["_"] digit)*
    let digitpart = |i: &mut usize, clean: &mut Vec<u8>| -> Result<usize, ()> {
        let start = *i;
        while *i < b.len() {
            let c = b[*i];
            if c.is_ascii_digit() {
                clean.push(c);
                *i += 1;
            } else if c == b'_' && *i > start && *i + 1 < b.len() && b[*i + 1].is_ascii_digit() {
                *i += 1;
            } else {
                break;
            }
        }
        if *i < b.len() && b[*i] == b'_' {
            return Err(());
        }
        Ok(*i - start)
    };
    let n_int = digitpart(&mut i, &mut clean).map_err(|_| err())?;
    let mut n_frac = 0;
    if i < b.len() && b[i] == b'.' {
        clean.push(b'.');
        i += 1;
        n_frac = digitpart(&mut i, &mut clean).map_err(|_| err())?;
    }
    if n_int + n_frac == 0 {
        return Err(err());
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        clean.push(b'e');
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            clean.push(b[i]);
            i += 1;
        }
        let n = digitpart(&mut i, &mut clean).map_err(|_| err())?;
        if n == 0 {
            return Err(err());
        }
    }
    if i != b.len() {
        return Err(err());
    }
    let text = unsafe { core::str::from_utf8_unchecked(&clean) };
    text.parse::<f64>().map_err(|_| err())
}

fn complex_inner(s: &[u8]) -> Option<(f64, f64)> {
    let n = s.len();
    let mut i = 0;
    let at = |i: usize| -> u8 { if i < n { s[i] } else { 0 } };
    while i < n && py_isspace(s[i]) {
        i += 1;
    }
    let mut got_bracket = false;
    if at(i) == b'(' {
        got_bracket = true;
        i += 1;
        while i < n && py_isspace(s[i]) {
            i += 1;
        }
    }
    let (mut x, mut y) = (0.0f64, 0.0f64);
    if let Some((z, len)) = parse_float_prefix(&s[i..]) {
        i += len;
        let c = at(i);
        if c == b'+' || c == b'-' {
            x = z;
            if let Some((v, l2)) = parse_float_prefix(&s[i..]) {
                y = v;
                i += l2;
            } else {
                y = if c == b'+' { 1.0 } else { -1.0 };
                i += 1;
            }
            if !(at(i) == b'j' || at(i) == b'J') {
                return None;
            }
            i += 1;
        } else if c == b'j' || c == b'J' {
            i += 1;
            y = z;
        } else {
            x = z;
        }
    } else {
        let c = at(i);
        if c == b'+' || c == b'-' {
            y = if c == b'+' { 1.0 } else { -1.0 };
            i += 1;
        } else {
            y = 1.0;
        }
        if !(at(i) == b'j' || at(i) == b'J') {
            return None;
        }
        i += 1;
    }
    while i < n && py_isspace(s[i]) {
        i += 1;
    }
    if got_bracket {
        if at(i) != b')' {
            return None;
        }
        i += 1;
        while i < n && py_isspace(s[i]) {
            i += 1;
        }
    }
    if i != n {
        return None;
    }
    Some((x, y))
}

/// `complex(s)` for an ASCII-transformed string (see
/// [`transform_decimal_and_space`]).  Returns (real, imag).
pub fn parse_complex_bytes(s: &[u8]) -> Result<(f64, f64), NumFmtError> {
    let r = if s.contains(&b'_') {
        strip_underscores(s).and_then(|v| complex_inner(&v))
    } else {
        complex_inner(s)
    };
    r.ok_or_else(|| NumFmtError::value(COMPLEX_PARSE_ERROR))
}

/// `complex(s)` for a str with explicit Unicode predicates.
pub fn parse_complex_with(
    s: &str,
    is_space: &dyn Fn(char) -> bool,
    decimal: &dyn Fn(char) -> Option<u32>,
) -> Result<(f64, f64), NumFmtError> {
    if s.is_ascii() {
        return parse_complex_bytes(s.as_bytes());
    }
    parse_complex_bytes(&transform_decimal_and_space(s, is_space, decimal))
}

/// `complex(s)` for a str using the built-in Unicode approximations.
pub fn parse_complex(s: &str) -> Result<(f64, f64), NumFmtError> {
    parse_complex_with(s, &default_is_space, &default_decimal)
}
