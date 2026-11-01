//! `Decimal.__format__`: the format-specification mini-language for decimals.

use super::dec::*;
use super::nat;
use super::ops::rescale_raw;

pub const CHAR_MAX: u32 = 127;

#[derive(Clone, Debug)]
pub struct Spec {
    pub fill: char,
    pub align: char,
    pub explicit_fill_align: bool,
    pub sign: char,
    pub no_neg_0: bool,
    pub alt: bool,
    pub zeropad: bool,
    pub width: usize,
    pub thousands_sep: Option<char>,
    pub precision: Option<usize>,
    pub frac_sep: Option<char>,
    pub ty: Option<char>,
}

#[derive(Clone, Debug)]
pub struct LocaleInfo {
    pub decimal_point: String,
    pub thousands_sep: String,
    /// grouping as in localeconv(): values; CHAR_MAX (or any value >= CHAR_MAX) stops grouping,
    /// 0 repeats the previous group.
    pub grouping: Vec<u32>,
}

const LIMIT: u128 = MAX_PREC as u128;

pub fn parse_spec(spec: &str, allow_upper_n: bool) -> Result<Spec, String> {
    let cs: Vec<char> = spec.chars().collect();
    let mut i = 0;
    let mut s = Spec {
        fill: ' ',
        align: '>',
        explicit_fill_align: false,
        sign: '-',
        no_neg_0: false,
        alt: false,
        zeropad: false,
        width: 0,
        thousands_sep: None,
        precision: None,
        frac_sep: None,
        ty: None,
    };
    let is_align = |c: char| matches!(c, '<' | '>' | '=' | '^');
    let mut have_fill = false;
    let mut have_align = false;
    if cs.len() >= 2 && is_align(cs[1]) {
        s.fill = cs[0];
        s.align = cs[1];
        have_fill = true;
        have_align = true;
        i = 2;
    } else if !cs.is_empty() && is_align(cs[0]) {
        s.align = cs[0];
        have_align = true;
        i = 1;
    }
    if i < cs.len() && matches!(cs[i], '+' | '-' | ' ') {
        s.sign = cs[i];
        i += 1;
    }
    if i < cs.len() && cs[i] == 'z' {
        s.no_neg_0 = true;
        i += 1;
    }
    if i < cs.len() && cs[i] == '#' {
        s.alt = true;
        i += 1;
    }
    if i < cs.len() && cs[i] == '0' {
        s.zeropad = true;
        i += 1;
    }
    let mut w: u128 = 0;
    let mut wd = false;
    while i < cs.len() && cs[i].is_ascii_digit() {
        w = w * 10 + cs[i] as u128 - '0' as u128;
        if w > LIMIT {
            return Err("invalid format string".into());
        }
        wd = true;
        i += 1;
    }
    if wd {
        s.width = w as usize;
    }
    if i < cs.len() && (cs[i] == ',' || cs[i] == '_') {
        s.thousands_sep = Some(cs[i]);
        i += 1;
    }
    if i < cs.len() && cs[i] == '.' {
        i += 1;
        let mut p: u128 = 0;
        let mut pd = false;
        while i < cs.len() && cs[i].is_ascii_digit() {
            p = p * 10 + cs[i] as u128 - '0' as u128;
            if p > LIMIT {
                return Err("invalid format string".into());
            }
            pd = true;
            i += 1;
        }
        if pd {
            s.precision = Some(p as usize);
        }
        if i < cs.len() && (cs[i] == ',' || cs[i] == '_') {
            s.frac_sep = Some(cs[i]);
            i += 1;
        }
        if !pd && s.frac_sep.is_none() {
            return Err(format!("Invalid format specifier '{}' for object of type 'Decimal'", spec));
        }
    }
    if i < cs.len() && (matches!(cs[i], 'e' | 'E' | 'f' | 'F' | 'g' | 'G' | 'n' | '%') || (allow_upper_n && cs[i] == 'N')) {
        s.ty = Some(cs[i]);
        i += 1;
    }
    if i != cs.len() {
        return Err(format!("Invalid format specifier '{}' for object of type 'Decimal'", spec));
    }
    if s.zeropad {
        if !have_fill {
            s.fill = '0';
        }
        if !have_align {
            s.align = '=';
        }
    }
    s.explicit_fill_align = have_fill || have_align;
    if matches!(s.ty, None | Some('g') | Some('G') | Some('n') | Some('N')) && s.precision == Some(0) {
        s.precision = Some(1);
    }
    if matches!(s.ty, Some('n') | Some('N')) && s.thousands_sep.is_some() {
        return Err("Cannot specify ',' with 'n'.".into());
    }
    Ok(s)
}

pub fn round_sig(d: &Dec, n: usize, round: Round) -> Dec {
    if !d.is_finite() || d.is_zero() {
        return d.clone();
    }
    let target = d.adjusted() as i128 - n as i128 + 1;
    let (mut r, _) = rescale_raw(d, target as i64, round);
    // rounding up 99..9 yields 10..0 with one digit too many; that digit is a zero
    if nat::digits(&r.coef) as usize > n {
        r.coef = nat::shr_trunc(&r.coef, 1).into();
        r.exp += 1;
    }
    r
}

fn rescale(d: &Dec, exp: i64, round: Round) -> Dec {
    if d.is_zero() {
        return Dec::finite(d.neg, vec![], exp);
    }
    rescale_raw(d, exp, round).0
}

/// Group sizes (least significant first) for `n` integer digits under a
/// localeconv()-style grouping: an entry >= CHAR_MAX ends grouping, 0 or the end
/// of the list repeats the previous size; an empty list means no grouping.
fn group_sizes(n: usize, grouping: &[u32]) -> Vec<usize> {
    let mut sizes = Vec::new();
    let mut remaining = n;
    let mut repeat: Option<usize> = None;
    let mut pos = 0;
    let mut frozen = false;
    while remaining > 0 {
        let size = if frozen || pos >= grouping.len() {
            repeat.unwrap_or(remaining)
        } else {
            let g = grouping[pos];
            pos += 1;
            if g >= CHAR_MAX {
                repeat = None;
                frozen = true;
                remaining
            } else if g == 0 {
                frozen = true;
                repeat.unwrap_or(remaining)
            } else {
                repeat = Some(g as usize);
                g as usize
            }
        };
        let take = size.min(remaining).max(1);
        sizes.push(take);
        remaining -= take;
    }
    if sizes.is_empty() {
        sizes.push(0);
    }
    sizes
}

fn grouped_len(n: usize, grouping: &[u32], seplen: usize) -> usize {
    n + seplen * (group_sizes(n, grouping).len() - 1)
}

/// Integer digits with separators, left-padded with zeros so that the result
/// is at least `min_width` characters long.
fn group_int(digits: &str, sep: &str, grouping: &[u32], min_width: usize) -> String {
    let seplen = sep.chars().count();
    let n = digits.len();
    let mut pad = 0usize;
    if grouped_len(n, grouping, seplen) < min_width {
        // smallest pad reaching the width (the length grows monotonically with pad)
        let (mut lo, mut hi) = (1usize, min_width);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if grouped_len(n + mid, grouping, seplen) >= min_width {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        pad = lo;
    }
    let all: Vec<u8> = std::iter::repeat_n(b'0', pad).chain(digits.bytes()).collect();
    if sep.is_empty() {
        return String::from_utf8(all).unwrap();
    }
    let sizes = group_sizes(all.len(), grouping);
    let mut parts: Vec<&str> = Vec::with_capacity(sizes.len());
    let mut end = all.len();
    for s in sizes {
        parts.push(std::str::from_utf8(&all[end - s..end]).unwrap());
        end -= s;
    }
    parts.reverse();
    parts.join(sep)
}

/// Fraction digits with a separator after every third digit.
fn group_frac(frac: &str, sep: char) -> String {
    let mut out = String::with_capacity(frac.len() + frac.len() / 3);
    for (i, c) in frac.chars().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(sep);
        }
        out.push(c);
    }
    out
}

fn pad_to_width(sign: &str, body: &str, s: &Spec) -> String {
    let used = sign.chars().count() + body.chars().count();
    let missing = s.width.saturating_sub(used);
    let fill = |k: usize| -> String { std::iter::repeat_n(s.fill, k).collect() };
    match s.align {
        '<' => format!("{sign}{body}{}", fill(missing)),
        '=' => format!("{sign}{}{body}", fill(missing)),
        '^' => format!("{}{sign}{body}{}", fill(missing / 2), fill(missing - missing / 2)),
        _ => format!("{}{sign}{body}", fill(missing)),
    }
}

fn sign_text(neg: bool, s: &Spec) -> &'static str {
    match (neg, s.sign) {
        (true, _) => "-",
        (false, '+') => "+",
        (false, ' ') => " ",
        _ => "",
    }
}

/// Format `d` according to the parsed spec. `locale` is required for type 'n'.
pub fn format_dec(d: &Dec, spec: &Spec, capitals: bool, round: Round, locale: Option<&LocaleInfo>) -> String {
    let mut s = spec.clone();
    let upper = s.ty == Some('N');
    if upper {
        s.ty = Some('n');
    }
    let out = format_inner(d, &s, capitals, round, locale);
    if upper { out.to_uppercase() } else { out }
}

/// Digits laid out as integer part, fraction part and optional exponent.
struct Layout {
    int: String,
    frac: String,
    exp: Option<i128>,
}

fn layout_fixed(digits: &str, exp: i128) -> Layout {
    let n = digits.len() as i128;
    if exp >= 0 {
        let mut int = digits.to_string();
        int.extend(std::iter::repeat_n('0', exp as usize));
        return Layout { int, frac: String::new(), exp: None };
    }
    let k = n + exp;
    if k > 0 {
        Layout { int: digits[..k as usize].to_string(), frac: digits[k as usize..].to_string(), exp: None }
    } else {
        let mut frac: String = std::iter::repeat_n('0', (-k) as usize).collect();
        frac.push_str(digits);
        Layout { int: "0".into(), frac, exp: None }
    }
}

fn layout_sci(digits: &str, adj: i128) -> Layout {
    Layout { int: digits[..1].to_string(), frac: digits[1..].to_string(), exp: Some(adj) }
}

fn format_inner(d: &Dec, s: &Spec, capitals: bool, round: Round, locale: Option<&LocaleInfo>) -> String {
    if !d.is_finite() {
        let mut body = to_string(&d.abs(), false, true);
        if s.ty == Some('%') {
            body.push('%');
        }
        return pad_to_width(sign_text(d.neg, s), &body, s);
    }
    let ty = s.ty.unwrap_or(if capitals { 'G' } else { 'g' });
    let fixed_kind = matches!(ty, 'f' | 'F' | '%');
    let mut x = d.clone();
    if ty == '%' {
        x.exp += 2;
    }
    if let Some(p) = s.precision {
        x = match ty {
            'e' | 'E' => round_sig(&x, p + 1, round),
            'f' | 'F' | '%' => rescale(&x, -(p as i64), round),
            _ if nat::digits(&x.coef) as usize > p => round_sig(&x, p, round),
            _ => x,
        };
    }
    if fixed_kind && x.is_zero() && x.exp > 0 {
        x = rescale(&x, 0, round);
    }
    let neg = x.neg && !(s.no_neg_0 && x.coef.is_empty());
    let digits = nat::to_string(&x.coef);
    let exp = x.exp as i128;
    let adj = exp + digits.len() as i128 - 1;
    let lay = match ty {
        'e' | 'E' => match s.precision {
            Some(p) if x.is_zero() => {
                Layout { int: "0".into(), frac: "0".repeat(p), exp: Some(exp + p as i128) }
            }
            _ => layout_sci(&digits, adj),
        },
        'f' | 'F' | '%' => layout_fixed(&digits, exp),
        _ => {
            if exp <= 0 && adj >= -6 {
                layout_fixed(&digits, exp)
            } else {
                layout_sci(&digits, adj)
            }
        }
    };
    let (point, tsep, grouping): (String, String, Vec<u32>) = if ty == 'n' {
        let l = locale.cloned().unwrap_or(LocaleInfo { decimal_point: ".".into(), thousands_sep: "".into(), grouping: vec![] });
        (l.decimal_point, l.thousands_sep, l.grouping)
    } else if let Some(c) = s.thousands_sep {
        (".".into(), c.to_string(), vec![3])
    } else {
        (".".into(), String::new(), vec![])
    };
    let mut tail = String::new();
    if !lay.frac.is_empty() || s.alt {
        tail.push_str(&point);
        match s.frac_sep {
            Some(c) => tail.push_str(&group_frac(&lay.frac, c)),
            None => tail.push_str(&lay.frac),
        }
    }
    if let Some(e) = lay.exp {
        if e != 0 || matches!(ty, 'e' | 'E') {
            tail.push(if matches!(ty, 'E' | 'G') { 'E' } else { 'e' });
            tail.push(if e < 0 { '-' } else { '+' });
            tail.push_str(&e.abs().to_string());
        }
    }
    if ty == '%' {
        tail.push('%');
    }
    let sign = sign_text(neg, s);
    let zero_fill = s.zeropad && s.fill == '0' && s.align == '=';
    let min_int = if zero_fill { s.width.saturating_sub(tail.chars().count() + sign.len()) } else { 0 };
    let int = group_int(&lay.int, &tsep, &grouping, min_int);
    pad_to_width(sign, &format!("{int}{tail}"), s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(spec: &str, v: &str) -> String {
        let d = parse(v.as_bytes()).unwrap();
        let s = parse_spec(spec, true).unwrap();
        format_dec(&d, &s, true, Round::HalfEven, None)
    }

    #[test]
    fn formatting_table() {
        let cases: &[(&str, &str, &str)] = &[
            ("e", "0E-15", "0e-15"),
            ("e", "2.3E-15", "2.3e-15"),
            ("e", "2.30E+2", "2.30e+2"),
            ("e", "2.30000E-15", "2.30000e-15"),
            ("e", "1.23456789123456789e40", "1.23456789123456789e+40"),
            ("e", "1.5", "1.5e+0"),
            ("e", "0.15", "1.5e-1"),
            ("e", "0.015", "1.5e-2"),
            ("e", "0.0000000000015", "1.5e-12"),
            ("e", "15.0", "1.50e+1"),
            ("e", "-15", "-1.5e+1"),
            ("e", "0", "0e+0"),
            ("e", "0E1", "0e+1"),
            ("e", "0.0", "0e-1"),
            ("e", "0.00", "0e-2"),
            (".6e", "0E-15", "0.000000e-9"),
            (".6e", "0", "0.000000e+6"),
            (".6e", "9.999999", "9.999999e+0"),
            (".6e", "9.9999999", "1.000000e+1"),
            (".6e", "-1.23e5", "-1.230000e+5"),
            (".6e", "1.23456789e-3", "1.234568e-3"),
            ("f", "0", "0"),
            ("f", "0.0", "0.0"),
            ("f", "0E-2", "0.00"),
            ("f", "0.00E-8", "0.0000000000"),
            ("f", "0E1", "0"),
            ("f", "3.2E1", "32"),
            ("f", "3.2E2", "320"),
            ("f", "3.20E2", "320"),
            ("f", "3.200E2", "320.0"),
            ("f", "3.2E-6", "0.0000032"),
            (".6f", "0E-15", "0.000000"),
            (".6f", "0E1", "0.000000"),
            (".6f", "0", "0.000000"),
            (".0f", "0", "0"),
            (".0f", "0e-2", "0"),
            (".0f", "3.14159265", "3"),
            (".1f", "3.14159265", "3.1"),
            (".01f", "3.14159265", "3.1"),
            (".4f", "3.14159265", "3.1416"),
            (".6f", "3.14159265", "3.141593"),
            (".7f", "3.14159265", "3.1415926"),
            (".8f", "3.14159265", "3.14159265"),
            (".9f", "3.14159265", "3.141592650"),
            ("g", "0", "0"),
            ("g", "0.0", "0.0"),
            ("g", "0E1", "0e+1"),
            ("G", "0E1", "0E+1"),
            ("g", "0E-5", "0.00000"),
            ("g", "0E-6", "0.000000"),
            ("g", "0E-7", "0e-7"),
            ("g", "-0E2", "-0e+2"),
            (".0g", "3.14159265", "3"),
            (".0n", "3.14159265", "3"),
            (".1g", "3.14159265", "3"),
            (".2g", "3.14159265", "3.1"),
            (".5g", "3.14159265", "3.1416"),
            (".7g", "3.14159265", "3.141593"),
            (".8g", "3.14159265", "3.1415926"),
            (".9g", "3.14159265", "3.14159265"),
            (".10g", "3.14159265", "3.14159265"),
            ("%", "0E1", "0%"),
            ("%", "0E0", "0%"),
            ("%", "0E-1", "0%"),
            ("%", "0E-2", "0%"),
            ("%", "0E-3", "0.0%"),
            ("%", "0E-4", "0.00%"),
            (".3%", "0", "0.000%"),
            (".3%", "0E10", "0.000%"),
            (".3%", "0E-10", "0.000%"),
            (".3%", "2.34", "234.000%"),
            (".3%", "1.234567", "123.457%"),
            (".0%", "1.23", "123%"),
            ("e", "NaN", "NaN"),
            ("f", "-NaN123", "-NaN123"),
            ("+g", "NaN456", "+NaN456"),
            (".3e", "Inf", "Infinity"),
            (".16f", "-Inf", "-Infinity"),
            (".0g", "-sNaN", "-sNaN"),
            ("", "1.00", "1.00"),
            ("6", "123", "   123"),
            ("<6", "123", "123   "),
            (">6", "123", "   123"),
            ("^6", "123", " 123  "),
            ("=+6", "123", "+  123"),
            ("#<10", "NaN", "NaN#######"),
            ("#<10", "-4.3", "-4.3######"),
            ("#<+10", "0.0130", "+0.0130###"),
            ("#< 10", "0.0130", " 0.0130###"),
            ("@>10", "-Inf", "@-Infinity"),
            ("#>5", "-Inf", "-Infinity"),
            ("?^5", "123", "?123?"),
            ("%^6", "123", "%123%%"),
            (" ^6", "-45.6", "-45.6 "),
            ("/=10", "-45.6", "-/////45.6"),
            ("/=+10", "45.6", "+/////45.6"),
            ("/= 10", "45.6", " /////45.6"),
            ("\x00=10", "-inf", "-\x00Infinity"),
            ("\x00^16", "-inf", "\x00\x00\x00-Infinity\x00\x00\x00\x00"),
            ("\x00>10", "1.2345", "\x00\x00\x00\x001.2345"),
            ("\x00<10", "1.2345", "1.2345\x00\x00\x00\x00"),
            (",", "1234567", "1,234,567"),
            (",", "123456", "123,456"),
            (",", "12345", "12,345"),
            (",", "1234", "1,234"),
            (",", "123", "123"),
            (",", "12", "12"),
            (",", "1", "1"),
            (",", "0", "0"),
            (",", "-1234567", "-1,234,567"),
            (",", "-123456", "-123,456"),
            ("7,", "123456", "123,456"),
            ("8,", "123456", " 123,456"),
            ("08,", "123456", "0,123,456"),
            ("+08,", "123456", "+123,456"),
            ("008,", "123456", "0,123,456"),
            (" 08,", "123456", " 123,456"),
            ("08,", "-123456", "-123,456"),
            ("+09,", "123456", "+0,123,456"),
            ("07,", "1234.56", "1,234.56"),
            ("08,", "1234.56", "1,234.56"),
            ("09,", "1234.56", "01,234.56"),
            ("010,", "1234.56", "001,234.56"),
            ("011,", "1234.56", "0,001,234.56"),
            ("012,", "1234.56", "0,001,234.56"),
            ("08,.1f", "1234.5", "01,234.5"),
            (",", "1.23456789", "1.23456789"),
            (",%", "123.456789", "12,345.6789%"),
            (",e", "123456", "1.23456e+5"),
            (",E", "123456", "1.23456E+5"),
            ("_", "1234567", "1_234_567"),
            ("07_", "1234.56", "1_234.56"),
            ("_", "1.23456789", "1.23456789"),
            ("_%", "123.456789", "12_345.6789%"),
            (".,", "1.23456789", "1.234,567,89"),
            ("._", "1.23456789", "1.234_567_89"),
            (".6_f", "12345.23456789", "12345.234_568"),
            (",._%", "123.456789", "12,345.678_9%"),
            (",._e", "123456", "1.234_56e+5"),
            (",.4_e", "123456", "1.234_6e+5"),
            (",.3_e", "123456", "1.235e+5"),
            (",._E", "123456", "1.234_56E+5"),
            (".1f", "-0", "-0.0"),
            (".1f", "-.0", "-0.0"),
            (".1f", "-.01", "-0.0"),
            ("z.1f", "0.", "0.0"),
            ("z6.1f", "0.", "   0.0"),
            ("z6.1f", "-1.", "  -1.0"),
            ("z.1f", "-0.", "0.0"),
            ("z.1f", ".01", "0.0"),
            ("z.1f", "-.01", "0.0"),
            ("z.2f", "0.", "0.00"),
            ("z.2f", "-0.", "0.00"),
            ("z.2f", ".001", "0.00"),
            ("z.2f", "-.001", "0.00"),
            ("z.1e", "0.", "0.0e+1"),
            ("z.1e", "-0.", "0.0e+1"),
            ("z.1E", "0.", "0.0E+1"),
            ("z.1E", "-0.", "0.0E+1"),
            ("z.2e", "-0.001", "-1.00e-3"),
            ("z.2g", "-0.001", "-0.001"),
            ("z.2%", "-0.001", "-0.10%"),
            ("zf", "-0.0000", "0.0000"),
            ("z.1f", "-00000.000001", "0.0"),
            ("z.1f", "-00000.", "0.0"),
            ("z.1f", "-.0000000000", "0.0"),
            ("z.2f", "-00000.000001", "0.00"),
            ("z.2f", "-00000.", "0.00"),
            ("z.2f", "-.0000000000", "0.00"),
            ("z.1f", ".09", "0.1"),
            ("z.1f", "-.09", "-0.1"),
            (" z.0f", "-0.", " 0"),
            ("+z.0f", "-0.", "+0"),
            ("-z.0f", "-0.", "0"),
            (" z.0f", "-1.", "-1"),
            ("+z.0f", "-1.", "-1"),
            ("-z.0f", "-1.", "-1"),
            ("z>6.1f", "-0.", "zz-0.0"),
            ("z>z6.1f", "-0.", "zzz0.0"),
            ("x>z6.1f", "-0.", "xxx0.0"),
            ("🖤>z6.1f", "-0.", "🖤🖤🖤0.0"),
            ("\x00>z6.1f", "-0.", "\x00\x00\x000.0"),
            ("z3,.10F", "-6.24E-323", "0.0000000000"),
            ("#", "0", "0."),
            ("a=-7.0", "0.12345", "aaaa0.1"),
            ("<^+15.20%", "inf", "<<+Infinity%<<<"),
            ("\x07>,%", "sNaN1234567", "sNaN1234567%"),
            ("=10.10%", "NaN123", "   NaN123%"),
            (".0e", "1.0", "1e+0"),
            ("#.0e", "1.0", "1.e+0"),
            (".0f", "1.0", "1"),
            ("#.0f", "1.0", "1."),
            ("g", "1.1", "1.1"),
            ("#g", "1.1", "1.1"),
            (".0g", "1", "1"),
            ("#.0g", "1", "1."),
            (".0%", "1.0", "100%"),
            ("#.0%", "1.0", "100.%"),
            ("N>10.3", "6.62607015e-34", "NN6.63E-34"),
            ("N>10.3n", "6.62607015e-34", "NN6.63e-34"),
            ("N>10.3e", "6.62607015e-34", "N6.626e-34"),
            ("N>10.3f", "6.62607015e-34", "NNNNN0.000"),
        ];
        let mut bad = vec![];
        for (spec, v, want) in cases {
            let got = f(spec, v);
            if got != *want {
                bad.push(format!("{spec:?} {v} -> {got:?} want {want:?}"));
            }
        }
        assert!(bad.is_empty(), "{:#?}", bad);
        for bad_spec in [".f", "._6f", "fz", ">Nf", "10Nf", "Nx", "<>=10.10"] {
            assert!(parse_spec(bad_spec, false).is_err(), "{bad_spec}");
        }
    }

    #[test]
    fn n_format() {
        let en = LocaleInfo { decimal_point: ".".into(), thousands_sep: ",".into(), grouping: vec![3, 3, 0] };
        let fr = LocaleInfo { decimal_point: ",".into(), thousands_sep: "".into(), grouping: vec![CHAR_MAX] };
        let crazy = LocaleInfo { decimal_point: "&".into(), thousands_sep: "-".into(), grouping: vec![1, 4, 2, CHAR_MAX] };
        let g = |v: &str, l: &LocaleInfo, spec: &str| {
            format_dec(&parse(v.as_bytes()).unwrap(), &parse_spec(spec, false).unwrap(), true, Round::HalfEven, Some(l))
        };
        assert_eq!(g("12.7", &en, "n"), "12.7");
        assert_eq!(g("12.7", &fr, "n"), "12,7");
        assert_eq!(g("12.7", &crazy, "n"), "1-2&7");
        assert_eq!(g("123456789", &en, "n"), "123,456,789");
        assert_eq!(g("123456789", &fr, "n"), "123456789");
        assert_eq!(g("1234567890123", &crazy, "n"), "123456-78-9012-3");
        assert_eq!(g("123456789", &en, ".6n"), "1.23457e+8");
        assert_eq!(g("1234", &fr, "05n"), "01234");
        assert_eq!(g("12345", &en, "07n"), "012,345");
        assert_eq!(g("12345", &en, "08n"), "0,012,345");
        assert_eq!(g("12345", &en, "010n"), "00,012,345");
        assert_eq!(g("123456", &crazy, "09n"), "01-2345-6");
        assert_eq!(g("123456", &crazy, "010n"), "0-01-2345-6");
        assert_eq!(g("123456", &crazy, "013n"), "000-01-2345-6");
    }
}
