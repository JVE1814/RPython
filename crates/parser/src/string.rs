//! String literal decoding: escape sequences, raw strings, bytes, invalid-escape warnings.

use crate::ast::PyStr;

/// An invalid escape found while decoding (reported as a SyntaxWarning by the parser).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidEscape {
    /// Byte offset of the backslash within the decoded body.
    pub offset: usize,
    /// The escape text without the backslash (e.g. "z" or "407").
    pub text: String,
    pub octal: bool,
}

/// A decoding error (reported as SyntaxError "(unicode error) ..." / "(value error) ...").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeError {
    pub msg: String,
}

fn hexval(c: u8) -> Option<u32> {
    match c {
        b'0'..=b'9' => Some((c - b'0') as u32),
        b'a'..=b'f' => Some((c - b'a' + 10) as u32),
        b'A'..=b'F' => Some((c - b'A' + 10) as u32),
        _ => None,
    }
}

/// Decode the body of a non-raw str literal (between the quotes). `first_invalid` receives the
/// first invalid escape sequence.
pub fn decode_str_escapes(body: &str, lookup_name: fn(&str) -> Option<u32>, first_invalid: &mut Option<InvalidEscape>) -> Result<PyStr, DecodeError> {
    let b = body.as_bytes();
    if !b.contains(&b'\\') {
        return Ok(PyStr::Utf8(body.to_string()));
    }
    let mut out = PyStr::Utf8(String::with_capacity(body.len()));
    let mut i = 0;
    let mut last = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            i += 1;
            continue;
        }
        out.push_str(&body[last..i]);
        let start = i;
        i += 1;
        if i >= b.len() {
            return Err(DecodeError { msg: format!("(unicode error) 'unicodeescape' codec can't decode byte 0x5c in position {}: \\ at end of string", start) });
        }
        let c = b[i];
        i += 1;
        match c {
            b'\n' => {}
            b'\\' => out.push_char('\\'),
            b'\'' => out.push_char('\''),
            b'"' => out.push_char('"'),
            b'a' => out.push_char('\x07'),
            b'b' => out.push_char('\x08'),
            b'f' => out.push_char('\x0c'),
            b'n' => out.push_char('\n'),
            b'r' => out.push_char('\r'),
            b't' => out.push_char('\t'),
            b'v' => out.push_char('\x0b'),
            b'0'..=b'7' => {
                let mut v = (c - b'0') as u32;
                let mut n = 1;
                while n < 3 && i < b.len() && (b'0'..=b'7').contains(&b[i]) {
                    v = v * 8 + (b[i] - b'0') as u32;
                    i += 1;
                    n += 1;
                }
                if v > 0o377 && first_invalid.is_none() {
                    *first_invalid = Some(InvalidEscape { offset: start, text: body[start + 1..i].to_string(), octal: true });
                }
                out.push_code_point(v);
            }
            b'x' | b'u' | b'U' => {
                let n = match c {
                    b'x' => 2,
                    b'u' => 4,
                    _ => 8,
                };
                let mut v: u32 = 0;
                for k in 0..n {
                    match b.get(i + k).and_then(|&h| hexval(h)) {
                        Some(h) => v = v.wrapping_mul(16).wrapping_add(h),
                        None => {
                            let what = match c {
                                b'x' => "truncated \\xXX escape",
                                b'u' => "truncated \\uXXXX escape",
                                _ => "truncated \\UXXXXXXXX escape",
                            };
                            return Err(DecodeError {
                                msg: format!(
                                    "(unicode error) 'unicodeescape' codec can't decode bytes in position {}-{}: {}",
                                    start,
                                    i + k - 1,
                                    what
                                ),
                            });
                        }
                    }
                }
                i += n;
                if v > 0x10FFFF {
                    return Err(DecodeError {
                        msg: format!("(unicode error) 'unicodeescape' codec can't decode bytes in position {}-{}: illegal Unicode character", start, i - 1),
                    });
                }
                out.push_code_point(v);
            }
            b'N' => {
                // \N{name}
                if i < b.len() && b[i] == b'{' {
                    if let Some(rel) = body[i + 1..].find('}') {
                        let name = &body[i + 1..i + 1 + rel];
                        if !name.is_empty() {
                            if let Some(cp) = lookup_name(name) {
                                out.push_code_point(cp);
                                i = i + 1 + rel + 1;
                                last = i;
                                continue;
                            }
                        }
                        return Err(DecodeError {
                            msg: format!(
                                "(unicode error) 'unicodeescape' codec can't decode bytes in position {}-{}: unknown Unicode character name",
                                start,
                                i + rel + 1
                            ),
                        });
                    }
                }
                return Err(DecodeError {
                    msg: format!("(unicode error) 'unicodeescape' codec can't decode bytes in position {}-{}: malformed \\N character escape", start, (i).min(b.len()) - 1 + if i < b.len() { 1 } else { 0 }),
                });
            }
            _ => {
                // invalid escape: keep backslash
                if first_invalid.is_none() {
                    let ch_len = utf8_len(c);
                    *first_invalid = Some(InvalidEscape { offset: start, text: body[start + 1..start + 1 + ch_len].to_string(), octal: false });
                }
                out.push_char('\\');
                i -= 1; // re-process the char as literal
            }
        }
        last = i;
    }
    out.push_str(&body[last..]);
    Ok(out)
}

fn utf8_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first >= 0xF0 {
        4
    } else if first >= 0xE0 {
        3
    } else {
        2
    }
}

/// Decode the body of a non-raw bytes literal. The body must be ASCII (checked by the caller).
pub fn decode_bytes_escapes(body: &str, first_invalid: &mut Option<InvalidEscape>) -> Result<Vec<u8>, DecodeError> {
    let b = body.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c != b'\\' {
            out.push(c);
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        if i >= b.len() {
            return Err(DecodeError { msg: "(value error) Trailing \\ in string".into() });
        }
        let c = b[i];
        i += 1;
        match c {
            b'\n' => {}
            b'\\' => out.push(b'\\'),
            b'\'' => out.push(b'\''),
            b'"' => out.push(b'"'),
            b'a' => out.push(7),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'v' => out.push(11),
            b'0'..=b'7' => {
                let mut v = (c - b'0') as u32;
                let mut n = 1;
                while n < 3 && i < b.len() && (b'0'..=b'7').contains(&b[i]) {
                    v = v * 8 + (b[i] - b'0') as u32;
                    i += 1;
                    n += 1;
                }
                if v > 0o377 && first_invalid.is_none() {
                    *first_invalid = Some(InvalidEscape { offset: start, text: body[start + 1..i].to_string(), octal: true });
                }
                out.push((v & 0xFF) as u8);
            }
            b'x' => {
                let h1 = b.get(i).and_then(|&h| hexval(h));
                let h2 = b.get(i + 1).and_then(|&h| hexval(h));
                match (h1, h2) {
                    (Some(a), Some(bb)) => {
                        out.push((a * 16 + bb) as u8);
                        i += 2;
                    }
                    _ => return Err(DecodeError { msg: format!("(value error) invalid \\x escape at position {}", start) }),
                }
            }
            _ => {
                if first_invalid.is_none() {
                    *first_invalid = Some(InvalidEscape { offset: start, text: body[start + 1..start + 1 + utf8_len(c)].to_string(), octal: false });
                }
                out.push(b'\\');
                i -= 1;
            }
        }
    }
    Ok(out)
}

/// Warning messages for an invalid escape: (warning message, message when turned into an error).
pub fn invalid_escape_messages(e: &InvalidEscape) -> (String, String) {
    let kind = if e.octal { "invalid octal escape sequence" } else { "invalid escape sequence" };
    (
        format!(
            "\"\\{}\" is an {}. Such sequences will not work in the future. Did you mean \"\\\\{}\"? A raw string is also an option.",
            e.text, kind, e.text
        ),
        format!("\"\\{}\" is an {}. Did you mean \"\\\\{}\"? A raw string is also an option.", e.text, kind, e.text),
    )
}

/// Parsed string prefix flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Prefix {
    pub bytes: bool,
    pub raw: bool,
    pub unicode: bool,
    pub fstring: bool,
    pub tstring: bool,
    /// length of the prefix in bytes
    pub len: usize,
}

pub fn parse_prefix(tok: &str) -> Prefix {
    let mut p = Prefix::default();
    for (i, c) in tok.bytes().enumerate() {
        match c.to_ascii_lowercase() {
            b'b' => p.bytes = true,
            b'r' => p.raw = true,
            b'u' => p.unicode = true,
            b'f' => p.fstring = true,
            b't' => p.tstring = true,
            _ => {
                p.len = i;
                return p;
            }
        }
    }
    p.len = tok.len();
    p
}
