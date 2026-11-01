//! printf-style (`%`) formatting: parsing of a single conversion
//! specification and per-conversion formatting helpers.  The runtime handles
//! argument fetching (tuple / mapping, `*` arguments, type checks).

use crate::float_fmt::{double_to_string, DtoaFlags};
use crate::{try_reserve, NumFmtError};

/// Conversion flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PercentFlags {
    /// '-'
    pub left: bool,
    /// '+'
    pub plus: bool,
    /// ' '
    pub space: bool,
    /// '#'
    pub alt: bool,
    /// '0'
    pub zero: bool,
}

/// Width or precision field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PercentNum {
    None,
    /// '*': taken from the argument list.
    Star,
    Value(usize),
}

/// One parsed conversion specification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PercentSpec {
    /// Index of the '%'.
    pub start: usize,
    /// Index just after the conversion character.
    pub end: usize,
    /// Mapping key: code-unit range (excluding the parentheses).
    pub key: Option<(usize, usize)>,
    pub flags: PercentFlags,
    pub width: PercentNum,
    pub precision: PercentNum,
    /// Conversion character ('%' for a literal "%%").
    pub conv: char,
}

const PY_SSIZE_T_MAX: u64 = i64::MAX as u64;
const INT_MAX: u64 = i32::MAX as u64;

fn ascii_repr(c: u32) -> String {
    let ch = c as u8 as char;
    match ch {
        '\'' => "\"'\"".to_string(),
        '\\' => "'\\\\'".to_string(),
        _ => format!("'{}'", ch),
    }
}

/// Default printability approximation for non-ASCII characters (Unicode
/// categories Cc/Cf/Zs/Zl/Zp/Cs/Co/Cn are non-printable in Python).
pub fn default_is_printable(c: u32) -> bool {
    match c {
        0..=0x1f | 0x7f..=0xa0 | 0xad => false,
        0x2000..=0x200f | 0x2028..=0x202f | 0x205f..=0x206f | 0x3000 | 0xfeff => false,
        0xd800..=0xdfff | 0xe000..=0xf8ff | 0xfff9..=0xfffb => false,
        0xf0000.. => false,
        _ => char::from_u32(c).is_some(),
    }
}

fn unexpected_char(c: u32, is_bytes: bool, is_printable: &dyn Fn(u32) -> bool) -> String {
    if (0x20..0x7f).contains(&c) {
        return ascii_repr(c);
    }
    if is_bytes {
        return format!("with code 0x{:02x}", c);
    }
    if c >= 0x80 && is_printable(c) {
        if let Some(ch) = char::from_u32(c) {
            return format!("'{}' (U+{:04X})", ch, c);
        }
    }
    format!("U+{:04X}", c)
}

fn valid_conversion(c: u32, is_bytes: bool) -> bool {
    match char::from_u32(c) {
        Some('s' | 'r' | 'a' | 'c' | 'd' | 'i' | 'u' | 'o' | 'x' | 'X' | 'e' | 'E' | 'f' | 'F'
        | 'g' | 'G') => true,
        Some('b') => is_bytes,
        _ => false,
    }
}

/// Parse the conversion specification starting at `fmt[start] == '%'`.
///
/// `fmt` is the format string as code units (`u8` for bytes/latin-1,
/// `u16`, `u32` or `char`), `is_bytes` selects bytes error wording and the
/// extra `%b` conversion, `is_printable` decides how unexpected non-ASCII
/// characters are shown (pass [`default_is_printable`] or the Unicode DB).
pub fn parse_percent_spec<T: Copy + Into<u32>>(
    fmt: &[T],
    start: usize,
    is_bytes: bool,
    is_printable: &dyn Fn(u32) -> bool,
) -> Result<PercentSpec, NumFmtError> {
    let at = |i: usize| -> Option<u32> { fmt.get(i).map(|&c| c.into()) };
    let stray = || NumFmtError::value(format!("stray % at position {}", start));
    let mut pos = start + 1;
    let mut spec = PercentSpec {
        start,
        end: 0,
        key: None,
        flags: PercentFlags::default(),
        width: PercentNum::None,
        precision: PercentNum::None,
        conv: '%',
    };
    match at(pos) {
        None => return Err(stray()),
        Some(0x25) => {
            spec.end = pos + 1;
            return Ok(spec);
        }
        _ => {}
    }
    // mapping key
    if at(pos) == Some('(' as u32) {
        let kstart = pos + 1;
        let mut depth = 1;
        pos += 1;
        loop {
            match at(pos) {
                None => {
                    return Err(NumFmtError::value(format!(
                        "stray % or incomplete format key at position {}",
                        start
                    )))
                }
                Some(0x28) => depth += 1,
                Some(0x29) => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            pos += 1;
        }
        spec.key = Some((kstart, pos));
        pos += 1;
    }
    // flags
    loop {
        match at(pos).and_then(char::from_u32) {
            Some('-') => spec.flags.left = true,
            Some('+') => spec.flags.plus = true,
            Some(' ') => spec.flags.space = true,
            Some('#') => spec.flags.alt = true,
            Some('0') => spec.flags.zero = true,
            _ => break,
        }
        pos += 1;
    }
    // width
    if at(pos) == Some('*' as u32) {
        if spec.key.is_some() {
            return Err(NumFmtError::value(format!(
                "* cannot be used with a parenthesised mapping key at position {}",
                start
            )));
        }
        spec.width = PercentNum::Star;
        pos += 1;
    } else {
        let mut w: u64 = 0;
        let mut any = false;
        while let Some(c @ 0x30..=0x39) = at(pos) {
            let d = (c - 0x30) as u64;
            if w > (PY_SSIZE_T_MAX - d) / 10 {
                return Err(NumFmtError::value(format!("width too big at position {}", start)));
            }
            w = w * 10 + d;
            any = true;
            pos += 1;
        }
        if any {
            spec.width = PercentNum::Value(w as usize);
        }
    }
    // precision
    if at(pos) == Some('.' as u32) {
        pos += 1;
        if at(pos) == Some('*' as u32) {
            if spec.key.is_some() {
                return Err(NumFmtError::value(format!(
                    "* cannot be used with a parenthesised mapping key at position {}",
                    start
                )));
            }
            spec.precision = PercentNum::Star;
            pos += 1;
        } else {
            let mut p: u64 = 0;
            while let Some(c @ 0x30..=0x39) = at(pos) {
                let d = (c - 0x30) as u64;
                if p > (INT_MAX - d) / 10 {
                    return Err(NumFmtError::value(format!(
                        "precision too big at position {}",
                        start
                    )));
                }
                p = p * 10 + d;
                pos += 1;
            }
            spec.precision = PercentNum::Value(p as usize);
        }
    }
    // length modifier (ignored)
    if let Some(c) = at(pos) {
        if c == 'h' as u32 || c == 'l' as u32 || c == 'L' as u32 {
            pos += 1;
        }
    }
    let c = match at(pos) {
        None => return Err(stray()),
        Some(c) => c,
    };
    if !valid_conversion(c, is_bytes) {
        if c < 0x80 && (c as u8).is_ascii_alphabetic() {
            return Err(NumFmtError::value(format!(
                "unsupported format %{} at position {}",
                c as u8 as char, start
            )));
        }
        return Err(NumFmtError::value(format!(
            "stray % at position {} or unexpected format character {} at position {}",
            start,
            unexpected_char(c, is_bytes, is_printable),
            pos
        )));
    }
    spec.conv = char::from_u32(c).unwrap();
    spec.end = pos + 1;
    Ok(spec)
}

/// Resolved options for formatting one argument.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PercentOpts {
    pub flags: PercentFlags,
    /// Width (a negative `*` width must be turned into `flags.left` + abs).
    pub width: Option<usize>,
    pub precision: Option<usize>,
}

impl PercentOpts {
    /// Build from a parsed spec whose width/precision are not `*`.
    pub fn from_spec(spec: &PercentSpec) -> PercentOpts {
        let num = |n: PercentNum| match n {
            PercentNum::Value(v) => Some(v),
            _ => None,
        };
        PercentOpts { flags: spec.flags, width: num(spec.width), precision: num(spec.precision) }
    }
}

/// Write `sign + prefix + body` with width/flag handling for numeric
/// conversions (CPython's `unicode_format_arg_output`).
fn output_numeric(
    out: &mut String,
    sign: Option<char>,
    prefix: &str,
    body: &str,
    opts: &PercentOpts,
) -> Result<(), NumFmtError> {
    let len = sign.is_some() as usize + prefix.len() + body.chars().count();
    let pad = opts.width.map_or(0, |w| w.saturating_sub(len));
    try_reserve(out, pad.saturating_add(body.len() + 4))?;
    if opts.flags.left {
        if let Some(s) = sign {
            out.push(s);
        }
        out.push_str(prefix);
        out.push_str(body);
        out.extend(core::iter::repeat_n(' ', pad));
    } else if opts.flags.zero {
        if let Some(s) = sign {
            out.push(s);
        }
        out.push_str(prefix);
        out.extend(core::iter::repeat_n('0', pad));
        out.push_str(body);
    } else {
        out.extend(core::iter::repeat_n(' ', pad));
        if let Some(s) = sign {
            out.push(s);
        }
        out.push_str(prefix);
        out.push_str(body);
    }
    Ok(())
}

fn sign_for(negative: bool, flags: &PercentFlags) -> Option<char> {
    if negative {
        Some('-')
    } else if flags.plus {
        Some('+')
    } else if flags.space {
        Some(' ')
    } else {
        None
    }
}

/// Format a float for `%e %E %f %F %g %G` and append to `out`.
pub fn percent_format_float(out: &mut String, x: f64, conv: char, opts: &PercentOpts) -> Result<(), NumFmtError> {
    let prec = opts.precision.unwrap_or(6);
    if opts.width.is_none() {
        let start = out.len();
        let space = opts.flags.space && !opts.flags.plus;
        if space {
            out.push(' ');
        }
        let f = DtoaFlags { alt: opts.flags.alt, sign: opts.flags.plus, ..Default::default() };
        double_to_string(out, x, conv, prec, f)?;
        if space && out.as_bytes().get(start + 1) == Some(&b'-') {
            out.remove(start);
        }
        return Ok(());
    }
    let mut buf = String::with_capacity(32);
    double_to_string(&mut buf, x, conv, prec, DtoaFlags { alt: opts.flags.alt, ..Default::default() })?;
    let (neg, body) = match buf.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, buf.as_str()),
    };
    output_numeric(out, sign_for(neg, &opts.flags), "", body, opts)
}

/// Format an integer for `%d %i %u %o %x %X` given its sign and the digits
/// of its magnitude in the conversion's radix (lowercase hex).
pub fn percent_format_int(
    out: &mut String,
    negative: bool,
    digits: &str,
    conv: char,
    opts: &PercentOpts,
) -> Result<(), NumFmtError> {
    let prefix = if opts.flags.alt {
        match conv {
            'o' => "0o",
            'x' => "0x",
            'X' => "0X",
            _ => "",
        }
    } else {
        ""
    };
    let ndig = digits.len();
    let zeros = opts.precision.map_or(0, |p| p.saturating_sub(ndig));
    let mut body = String::new();
    try_reserve(&mut body, zeros.saturating_add(ndig))?;
    body.extend(core::iter::repeat_n('0', zeros));
    if conv == 'X' {
        body.extend(digits.chars().map(|c| c.to_ascii_uppercase()));
    } else {
        body.push_str(digits);
    }
    output_numeric(out, sign_for(negative, &opts.flags), prefix, &body, opts)
}

/// Layout for `%s %r %a %c` (and `%b` for bytes): number of code points to
/// take and padding.  Returns (take, left_pad, right_pad); padding is spaces.
pub fn percent_str_layout(len: usize, conv: char, opts: &PercentOpts) -> (usize, usize, usize) {
    let take = match opts.precision {
        Some(p) if matches!(conv, 's' | 'r' | 'a' | 'b') && p < len => p,
        _ => len,
    };
    let pad = opts.width.map_or(0, |w| w.saturating_sub(take));
    if opts.flags.left {
        (take, 0, pad)
    } else {
        (take, pad, 0)
    }
}

/// Append a string argument for `%s %r %a %c`.
pub fn percent_format_str(
    out: &mut String,
    s: &str,
    conv: char,
    opts: &PercentOpts,
) -> Result<(), NumFmtError> {
    let (take, l, r) = percent_str_layout(s.chars().count(), conv, opts);
    try_reserve(out, l.saturating_add(r).saturating_add(s.len()))?;
    out.extend(core::iter::repeat_n(' ', l));
    match s.char_indices().nth(take) {
        Some((i, _)) => out.push_str(&s[..i]),
        None => out.push_str(s),
    }
    out.extend(core::iter::repeat_n(' ', r));
    Ok(())
}

/// Error for `%c` with an out-of-range int.
pub fn percent_char_range_error(is_bytes: bool) -> &'static str {
    if is_bytes {
        "%c argument not in range(256)"
    } else {
        "%c argument not in range(0x110000)"
    }
}
