//! `__format__` implementations for float, int, str and complex.

use crate::float_fmt::{double_to_string, DtoaFlags};
use crate::spec::{unknown_presentation_type, Align, FormatSpec};
use crate::NumFmtError;

/// Numeric locale information used by the 'n' presentation type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locale {
    pub decimal_point: String,
    pub thousands_sep: String,
    /// C `localeconv()` grouping: group sizes from the right; a 0 entry (or
    /// the end of the list) repeats the previous size, 127 (CHAR_MAX) stops
    /// grouping.  Empty means no grouping.
    pub grouping: Vec<u8>,
}

impl Locale {
    /// The "C" locale: '.' decimal point and no grouping.
    pub fn c() -> Locale {
        Locale { decimal_point: ".".into(), thousands_sep: String::new(), grouping: Vec::new() }
    }
}

impl Default for Locale {
    fn default() -> Self {
        Locale::c()
    }
}

/// How an int should be formatted for a given spec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntPresentation {
    /// Format the magnitude digits in this radix (2, 8, 10 or 16) with
    /// [`format_int`].
    Radix(u32),
    /// 'c': format a code point with [`format_int_char`].
    Char,
    /// e/E/f/F/g/G/%: convert the int to float and use [`format_float_spec`].
    Float,
}

/// Decide how an int must be formatted (performs CPython's int-specific
/// validation: precision / 'z' / 'c' restrictions, unknown codes).
pub fn int_presentation(spec: &FormatSpec, type_name: &str) -> Result<IntPresentation, NumFmtError> {
    let radix = match spec.ty {
        'b' => 2,
        'o' => 8,
        'x' | 'X' => 16,
        'd' | 'n' => 10,
        'c' => 0,
        'e' | 'E' | 'f' | 'F' | 'g' | 'G' | '%' => return Ok(IntPresentation::Float),
        t => return Err(unknown_presentation_type(t, type_name)),
    };
    if spec.precision.is_some() {
        return Err(NumFmtError::value("Precision not allowed in integer format specifier"));
    }
    if spec.no_neg_0 {
        return Err(NumFmtError::value(
            "Negative zero coercion (z) not allowed in integer format specifier",
        ));
    }
    if radix == 0 {
        if spec.sign.is_some() {
            return Err(NumFmtError::value("Sign not allowed with integer format specifier 'c'"));
        }
        if spec.alternate {
            return Err(NumFmtError::value(
                "Alternate form (#) not allowed with integer format specifier 'c'",
            ));
        }
        return Ok(IntPresentation::Char);
    }
    Ok(IntPresentation::Radix(radix))
}

// ---------------------------------------------------------------------------
// number layout

struct Grouping<'a> {
    sep: &'a str,
    sizes: &'a [u8],
}

const NO_GROUPING: Grouping<'static> = Grouping { sep: "", sizes: &[] };
const GROUP3_COMMA: Grouping<'static> = Grouping { sep: ",", sizes: &[3] };
const GROUP3_UNDER: Grouping<'static> = Grouping { sep: "_", sizes: &[3] };
const GROUP4_UNDER: Grouping<'static> = Grouping { sep: "_", sizes: &[4] };

struct GroupGen<'a> {
    sizes: &'a [u8],
    idx: usize,
    previous: isize,
}

impl GroupGen<'_> {
    fn next(&mut self) -> isize {
        match self.sizes.get(self.idx) {
            None | Some(0) => self.previous,
            Some(127) => 0,
            Some(&c) => {
                self.previous = c as isize;
                self.idx += 1;
                c as isize
            }
        }
    }
}

/// CPython's `_PyUnicode_InsertThousandsGrouping`: insert separators into
/// `digits` (ASCII), zero padding on the left up to `min_width` characters.
/// Returns the number of characters written.
fn insert_grouping(out: &mut String, digits: &str, mut min_width: isize, g: &Grouping) -> usize {
    let sep_len = g.sep.chars().count() as isize;
    let mut segs: [(isize, isize); 128] = [(0, 0); 128];
    let mut segs_vec: Vec<(isize, isize)> = Vec::new();
    let mut nseg = 0usize;
    let mut push = |seg: (isize, isize), nseg: &mut usize| {
        if *nseg < segs.len() {
            segs[*nseg] = seg;
        } else {
            if segs_vec.is_empty() {
                segs_vec.extend_from_slice(&segs);
            }
            segs_vec.push(seg);
        }
        *nseg += 1;
    };
    let mut remaining = digits.len() as isize;
    let mut groups = GroupGen { sizes: g.sizes, idx: 0, previous: 0 };
    let mut broken = false;
    loop {
        let l0 = groups.next();
        if l0 <= 0 {
            break;
        }
        let l = l0.min(remaining.max(min_width).max(1));
        let n_zeros = (l - remaining).max(0);
        let n_chars = remaining.min(l).max(0);
        push((n_zeros, n_chars), &mut nseg);
        remaining -= n_chars;
        min_width -= l;
        if remaining <= 0 && min_width <= 0 {
            broken = true;
            break;
        }
        min_width -= sep_len;
    }
    if !broken {
        let l = remaining.max(min_width).max(1);
        let n_zeros = (l - remaining).max(0);
        let n_chars = remaining.min(l).max(0);
        push((n_zeros, n_chars), &mut nseg);
    }
    let segs: &[(isize, isize)] = if nseg <= 128 { &segs[..nseg] } else { &segs_vec };
    let mut written = 0usize;
    let mut idx = 0usize;
    for (k, &(z, c)) in segs.iter().rev().enumerate() {
        if k > 0 {
            out.push_str(g.sep);
            written += sep_len as usize;
        }
        for _ in 0..z {
            out.push('0');
        }
        out.push_str(&digits[idx..idx + c as usize]);
        idx += c as usize;
        written += (z + c) as usize;
    }
    written
}

fn frac_grouped_len(n: usize) -> usize {
    if n == 0 {
        0
    } else {
        n + (n - 1) / 3
    }
}

fn push_frac_grouped(out: &mut String, frac: &str, sep: Option<char>) {
    match sep {
        None => out.push_str(frac),
        Some(c) => {
            for (i, ch) in frac.chars().enumerate() {
                if i > 0 && i % 3 == 0 {
                    out.push(c);
                }
                out.push(ch);
            }
        }
    }
}

struct NumParts<'a> {
    sign: Option<char>,
    prefix: &'a str,
    digits: &'a str,
    decimal: &'a str,
    frac: &'a str,
    rest: &'a str,
}

#[inline]
fn push_fill(out: &mut String, c: char, n: usize) {
    for _ in 0..n {
        out.push(c);
    }
}

fn resolve_sign(spec_sign: Option<char>, negative: bool) -> Option<char> {
    match spec_sign {
        Some('+') => Some(if negative { '-' } else { '+' }),
        Some(' ') => Some(if negative { '-' } else { ' ' }),
        _ => {
            if negative {
                Some('-')
            } else {
                None
            }
        }
    }
}

/// Lay out a number: `<lpad><sign><prefix><spad><grouped digits><decimal><frac><rest><rpad>`.
/// `width`: None for no padding.  Returns number of characters written.
fn render_number(
    out: &mut String,
    p: &NumParts,
    fill: char,
    align: Align,
    width: Option<usize>,
    grouping: &Grouping,
    frac_sep: Option<char>,
) -> usize {
    let n_sign = p.sign.is_some() as usize;
    let n_prefix = p.prefix.len();
    let n_decimal = p.decimal.chars().count();
    let n_frac = if frac_sep.is_some() { frac_grouped_len(p.frac.len()) } else { p.frac.len() };
    let n_rest = p.rest.chars().count();
    let non_digit = n_sign + n_prefix + n_decimal + n_frac + n_rest;
    let min_width: isize = match width {
        Some(w) if fill == '0' && align == Align::AfterSign => w as isize - non_digit as isize,
        _ => 0,
    };
    let mut grouped = String::new();
    let n_grouped = if p.digits.is_empty() {
        0
    } else if grouping.sizes.is_empty() && min_width <= p.digits.len() as isize {
        grouped.push_str(p.digits);
        p.digits.len()
    } else {
        insert_grouping(&mut grouped, p.digits, min_width, grouping)
    };
    let total = non_digit + n_grouped;
    let n_pad = match width {
        Some(w) if w > total => w - total,
        _ => 0,
    };
    let (lpad, spad, rpad) = match align {
        Align::Left => (0, 0, n_pad),
        Align::Right => (n_pad, 0, 0),
        Align::Center => (n_pad / 2, 0, n_pad - n_pad / 2),
        Align::AfterSign => (0, n_pad, 0),
    };
    out.reserve(total + n_pad);
    push_fill(out, fill, lpad);
    if let Some(s) = p.sign {
        out.push(s);
    }
    out.push_str(p.prefix);
    push_fill(out, fill, spad);
    out.push_str(&grouped);
    out.push_str(p.decimal);
    push_frac_grouped(out, p.frac, frac_sep);
    out.push_str(p.rest);
    push_fill(out, fill, rpad);
    total + n_pad
}

/// Split a formatted (unsigned) float string into digits / decimal / frac / rest.
fn split_number(s: &str) -> (&str, bool, &str, &str) {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let digits = &s[..i];
    let has_dec = i < b.len() && b[i] == b'.';
    if has_dec {
        i += 1;
    }
    let fstart = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    (digits, has_dec, &s[fstart..i], &s[i..])
}

fn number_grouping<'a>(spec: &FormatSpec, locale: &'a Locale, is_n: bool, four: bool) -> Grouping<'a> {
    if is_n {
        return Grouping { sep: &locale.thousands_sep, sizes: &locale.grouping };
    }
    match spec.grouping {
        Some(',') => GROUP3_COMMA,
        Some('_') if four => GROUP4_UNDER,
        Some('_') => GROUP3_UNDER,
        _ => NO_GROUPING,
    }
}

// ---------------------------------------------------------------------------
// float

fn check_precision_int_max(spec: &FormatSpec) -> Result<(), NumFmtError> {
    if let Some(p) = spec.precision {
        if p > i32::MAX as usize {
            return Err(NumFmtError::value("precision too big"));
        }
    }
    Ok(())
}

/// Format one float component (no padding) and return its sign char and
/// the unsigned text in `buf`.
fn float_component(
    buf: &mut String,
    x: f64,
    code: char,
    precision: usize,
    flags: DtoaFlags,
) -> Result<bool, NumFmtError> {
    double_to_string(buf, x, code, precision, flags)?;
    if buf.starts_with('-') {
        buf.remove(0);
        Ok(true)
    } else {
        Ok(false)
    }
}

/// `float.__format__(spec)` with an already parsed spec, appending to `out`.
pub fn format_float_spec(
    out: &mut String,
    x: f64,
    spec: &FormatSpec,
    locale: &Locale,
    type_name: &str,
) -> Result<(), NumFmtError> {
    let mut code = match spec.ty {
        '\0' | 'e' | 'E' | 'f' | 'F' | 'g' | 'G' | 'n' | '%' => spec.ty,
        t => return Err(unknown_presentation_type(t, type_name)),
    };
    check_precision_int_max(spec)?;
    let mut flags = DtoaFlags { alt: spec.alternate, no_neg_0: spec.no_neg_0, ..Default::default() };
    let mut default_precision = 6;
    let is_n = code == 'n';
    if code == '\0' {
        flags.add_dot_0 = true;
        code = 'r';
        default_precision = 0;
    }
    if code == 'n' {
        code = 'g';
    }
    let mut val = x;
    let mut add_pct = false;
    if code == '%' {
        code = 'f';
        val *= 100.0;
        add_pct = true;
    }
    let precision = match spec.precision {
        None => default_precision,
        Some(p) => {
            if code == 'r' {
                code = 'g';
            }
            p
        }
    };
    let mut buf = String::with_capacity(32);
    let neg = float_component(&mut buf, val, code, precision, flags)?;
    if add_pct {
        buf.push('%');
    }
    let (digits, has_dec, frac, rest) = split_number(&buf);
    let decimal = if has_dec {
        if is_n {
            locale.decimal_point.as_str()
        } else {
            "."
        }
    } else {
        ""
    };
    let parts = NumParts {
        sign: resolve_sign(spec.sign, neg),
        prefix: "",
        digits,
        decimal,
        frac,
        rest,
    };
    let g = number_grouping(spec, locale, is_n, false);
    render_number(out, &parts, spec.fill, spec.align, spec.width, &g, spec.frac_grouping);
    Ok(())
}

/// `format(x, spec)` for a float.  `type_name` is the Python type name used
/// in error messages (normally "float").  Uses the C locale for 'n'.
pub fn format_float(x: f64, spec: &str, type_name: &str) -> Result<String, NumFmtError> {
    if spec.is_empty() {
        return Ok(crate::float_repr(x));
    }
    let s = FormatSpec::parse_float(spec, type_name)?;
    let mut out = String::new();
    format_float_spec(&mut out, x, &s, &Locale::c(), type_name)?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// int

/// Format an integer given its sign and the digits of its magnitude in the
/// radix returned by [`int_presentation`] (lowercase letters for hex, no
/// prefix, "0" for zero).  Appends to `out`.
pub fn format_int(
    out: &mut String,
    negative: bool,
    digits: &str,
    spec: &FormatSpec,
    locale: &Locale,
) -> Result<(), NumFmtError> {
    let prefix = if spec.alternate {
        match spec.ty {
            'b' => "0b",
            'o' => "0o",
            'x' => "0x",
            'X' => "0X",
            _ => "",
        }
    } else {
        ""
    };
    let upper;
    let digits = if spec.ty == 'X' && digits.bytes().any(|b| b.is_ascii_lowercase()) {
        upper = digits.to_ascii_uppercase();
        upper.as_str()
    } else {
        digits
    };
    let is_n = spec.ty == 'n';
    let four = matches!(spec.ty, 'b' | 'o' | 'x' | 'X');
    let parts = NumParts {
        sign: resolve_sign(spec.sign, negative),
        prefix,
        digits,
        decimal: "",
        frac: "",
        rest: "",
    };
    let g = number_grouping(spec, locale, is_n, four);
    render_number(out, &parts, spec.fill, spec.align, spec.width, &g, None);
    Ok(())
}

/// Layout of a padded string: take the first `take` code points of the
/// value, with `lpad`/`rpad` copies of `fill` around it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StrLayout {
    pub take: usize,
    pub lpad: usize,
    pub rpad: usize,
    pub fill: char,
}

impl StrLayout {
    /// Apply the layout to `s`.
    pub fn apply(&self, out: &mut String, s: &str) {
        push_fill(out, self.fill, self.lpad);
        match s.char_indices().nth(self.take) {
            Some((i, _)) => out.push_str(&s[..i]),
            None => out.push_str(s),
        }
        push_fill(out, self.fill, self.rpad);
    }
}

fn calc_padding(nchars: usize, width: Option<usize>, align: Align) -> (usize, usize) {
    let total = match width {
        Some(w) if w > nchars => w,
        _ => nchars,
    };
    let lpad = match align {
        Align::Right => total - nchars,
        Align::Center => (total - nchars) / 2,
        _ => 0,
    };
    (lpad, total - nchars - lpad)
}

/// Validate the spec for int 'c' formatting and compute the layout of the
/// resulting single character.  `code` is the int value if it fits in an
/// i64 (None for larger magnitudes).
pub fn int_char_layout(code: Option<i64>, spec: &FormatSpec) -> Result<(u32, StrLayout), NumFmtError> {
    let cp = match code {
        Some(c) if (0..=0x10ffff).contains(&c) => c as u32,
        _ => return Err(NumFmtError::overflow("%c arg not in range(0x110000)")),
    };
    let (lpad, rpad) = calc_padding(1, spec.width, spec.align);
    // '=' alignment puts the padding between the (absent) sign and the char.
    let (lpad, rpad) = if spec.align == Align::AfterSign { (lpad + rpad, 0) } else { (lpad, rpad) };
    Ok((cp, StrLayout { take: 1, lpad, rpad, fill: spec.fill }))
}

/// Format an int with presentation type 'c' (after [`int_presentation`]
/// returned `Char`).  Lone surrogates cannot be represented in a Rust
/// string; use [`int_char_layout`] for those.
pub fn format_int_char(out: &mut String, code: Option<i64>, spec: &FormatSpec) -> Result<(), NumFmtError> {
    let (cp, layout) = int_char_layout(code, spec)?;
    let ch = char::from_u32(cp).unwrap_or('\u{FFFD}');
    let mut tmp = [0u8; 4];
    layout.apply(out, ch.encode_utf8(&mut tmp));
    Ok(())
}

// ---------------------------------------------------------------------------
// str

/// Validate a spec for `str.__format__` and compute the layout for a string
/// of `len` code points.
pub fn str_layout(len: usize, spec: &FormatSpec, type_name: &str) -> Result<StrLayout, NumFmtError> {
    if spec.ty != 's' {
        return Err(unknown_presentation_type(spec.ty, type_name));
    }
    if let Some(s) = spec.sign {
        return Err(NumFmtError::value(if s == ' ' {
            "Space not allowed in string format specifier"
        } else {
            "Sign not allowed in string format specifier"
        }));
    }
    if spec.no_neg_0 {
        return Err(NumFmtError::value(
            "Negative zero coercion (z) not allowed in string format specifier",
        ));
    }
    if spec.alternate {
        return Err(NumFmtError::value("Alternate form (#) not allowed in string format specifier"));
    }
    if spec.align == Align::AfterSign {
        return Err(NumFmtError::value("'=' alignment not allowed in string format specifier"));
    }
    let take = match spec.precision {
        Some(p) if p < len => p,
        _ => len,
    };
    let (lpad, rpad) = calc_padding(take, spec.width, spec.align);
    Ok(StrLayout { take, lpad, rpad, fill: spec.fill })
}

/// `format(s, spec)` for a str.  Widths and precisions count code points.
pub fn format_str(s: &str, spec: &str, type_name: &str) -> Result<String, NumFmtError> {
    if spec.is_empty() {
        return Ok(s.to_string());
    }
    let f = FormatSpec::parse_str(spec, type_name)?;
    let layout = str_layout(s.chars().count(), &f, type_name)?;
    let mut out = String::with_capacity(s.len() + layout.lpad + layout.rpad);
    layout.apply(&mut out, s);
    Ok(out)
}

// ---------------------------------------------------------------------------
// complex

/// Append `repr(complex(re, im))`.
pub fn write_complex_repr(out: &mut String, re: f64, im: f64) {
    if re == 0.0 && re.is_sign_positive() {
        crate::float_fmt::write_float_repr_no_dot0(out, im, false);
        out.push('j');
    } else {
        out.push('(');
        crate::float_fmt::write_float_repr_no_dot0(out, re, false);
        crate::float_fmt::write_float_repr_no_dot0(out, im, true);
        out.push_str("j)");
    }
}

/// `repr(complex(re, im))`.
pub fn complex_repr(re: f64, im: f64) -> String {
    let mut s = String::with_capacity(32);
    write_complex_repr(&mut s, re, im);
    s
}

/// `complex.__format__(spec)` with a parsed spec (parse with
/// `FormatSpec::parse_float`).
pub fn format_complex_spec(
    out: &mut String,
    re: f64,
    im: f64,
    spec: &FormatSpec,
    locale: &Locale,
    type_name: &str,
) -> Result<(), NumFmtError> {
    let mut code = match spec.ty {
        '\0' | 'e' | 'E' | 'f' | 'F' | 'g' | 'G' | 'n' => spec.ty,
        t => return Err(unknown_presentation_type(t, type_name)),
    };
    check_precision_int_max(spec)?;
    if spec.fill == '0' {
        return Err(NumFmtError::value("Zero padding is not allowed in complex format specifier"));
    }
    if spec.align == Align::AfterSign {
        return Err(NumFmtError::value(
            "'=' alignment flag is not allowed in complex format specifier",
        ));
    }
    let flags = DtoaFlags { alt: spec.alternate, no_neg_0: spec.no_neg_0, ..Default::default() };
    let mut default_precision = 6;
    let mut skip_re = false;
    let mut add_parens = false;
    let is_n = code == 'n';
    if code == '\0' {
        code = 'r';
        default_precision = 0;
        if re == 0.0 && re.is_sign_positive() {
            skip_re = true;
        } else {
            add_parens = true;
        }
    }
    if code == 'n' {
        code = 'g';
    }
    let precision = match spec.precision {
        None => default_precision,
        Some(p) => {
            if code == 'r' {
                code = 'g';
            }
            p
        }
    };
    let mut re_buf = String::with_capacity(32);
    let mut im_buf = String::with_capacity(32);
    let re_neg = float_component(&mut re_buf, re, code, precision, flags)?;
    let im_neg = float_component(&mut im_buf, im, code, precision, flags)?;
    let g = number_grouping(spec, locale, is_n, false);
    let dec_of = |has: bool| if !has { "" } else if is_n { locale.decimal_point.as_str() } else { "." };

    let mut body = String::with_capacity(re_buf.len() + im_buf.len() + 8);
    if add_parens {
        body.push('(');
    }
    if !skip_re {
        let (d, h, f, r) = split_number(&re_buf);
        let parts = NumParts { sign: resolve_sign(spec.sign, re_neg), prefix: "", digits: d, decimal: dec_of(h), frac: f, rest: r };
        render_number(&mut body, &parts, ' ', Align::Left, None, &g, spec.frac_grouping);
    }
    {
        let (d, h, f, r) = split_number(&im_buf);
        let im_sign = if skip_re { spec.sign } else { Some('+') };
        let parts = NumParts { sign: resolve_sign(im_sign, im_neg), prefix: "", digits: d, decimal: dec_of(h), frac: f, rest: r };
        render_number(&mut body, &parts, ' ', Align::Left, None, &g, spec.frac_grouping);
    }
    body.push('j');
    if add_parens {
        body.push(')');
    }
    let n = body.chars().count();
    let (lpad, rpad) = calc_padding(n, spec.width, spec.align);
    push_fill(out, spec.fill, lpad);
    out.push_str(&body);
    push_fill(out, spec.fill, rpad);
    Ok(())
}

/// `format(complex(re, im), spec)`; `type_name` normally "complex".
pub fn format_complex(re: f64, im: f64, spec: &str, type_name: &str) -> Result<String, NumFmtError> {
    if spec.is_empty() {
        return Ok(complex_repr(re, im));
    }
    let s = FormatSpec::parse_float(spec, type_name)?;
    let mut out = String::new();
    format_complex_spec(&mut out, re, im, &s, &Locale::c(), type_name)?;
    Ok(out)
}
