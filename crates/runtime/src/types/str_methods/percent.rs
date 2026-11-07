//! printf-style formatting for str (`fmt % args`).
//!
//! Conversion specifications are parsed by `rpy_numfmt::percent`; numbers
//! are rendered by its `percent_format_*` helpers.  This module does the
//! argument plumbing (tuple / mapping / single argument, `*` fields) and the
//! CPython 3.15 error messages.

use super::format::Out;
use super::writer::slice_data;
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::*;
use crate::raise;
use crate::types::str::{self as pystr, StrData};
use crate::types::typeobject::{tname, type_fqname};
use crate::types::*;
use rpy_numfmt::percent::{self as pct, PercentNum, PercentOpts, PercentSpec};

struct Args<'a> {
    args: &'a PyObject,
    /// tuple length, or -1 for a single (non-tuple) argument
    arglen: isize,
    /// next index; -2 = single argument not yet used, -1 = used
    argidx: isize,
    dict: Option<&'a PyObject>,
}

/// How an argument is named in error messages.
#[derive(Clone)]
enum ArgName {
    Single,
    Pos(usize),
    Key(String),
}

impl ArgName {
    fn prefix(&self) -> String {
        match self {
            ArgName::Single => "format argument: ".into(),
            ArgName::Pos(i) => format!("format argument {}: ", i),
            ArgName::Key(k) => format!("format argument '{}': ", k),
        }
    }
}

impl<'a> Args<'a> {
    fn need(&self, n: isize) -> PyResult<()> {
        let remaining = if self.arglen < 0 { (self.argidx == -2) as isize } else { self.arglen - self.argidx };
        if remaining < n {
            raise!(TypeError, "not enough arguments for format string (got {})", if self.arglen < 0 { 1 } else { self.arglen });
        }
        Ok(())
    }
    fn next(&mut self) -> PyResult<(PyRef, ArgName)> {
        let got = |s: &Self| if s.arglen < 0 { 1 } else { s.arglen };
        if self.argidx < self.arglen {
            let i = self.argidx;
            if self.arglen < 0 {
                self.argidx = -1;
                return Ok((self.args.to_ref(), ArgName::Single));
            }
            self.argidx += 1;
            return Ok((tuple::tuple_get(self.args, i as usize).to_ref(), ArgName::Pos(i as usize + 1)));
        }
        raise!(TypeError, "not enough arguments for format string (got {})", got(self))
    }
}

fn is_mapping(o: &PyObject) -> bool {
    let mp = o.ty().tp_as_mapping;
    !mp.is_null() && unsafe { (*mp).mp_subscript.is_some() } && !tuple::is_tuple(o) && !pystr::is_str(o)
}

fn count_stars(f: StrData, start: usize) -> (bool, bool) {
    let mut p = start + 1;
    let at = |i: usize| if i < f.len() { f.get(i) } else { 0 };
    while matches!(char::from_u32(at(p)), Some('-' | '+' | ' ' | '#' | '0')) {
        p += 1;
    }
    let mut w = false;
    if at(p) == '*' as u32 {
        w = true;
        p += 1;
    } else {
        while (0x30..=0x39).contains(&at(p)) {
            p += 1;
        }
    }
    let mut pr = false;
    if at(p) == '.' as u32 {
        p += 1;
        if at(p) == '*' as u32 {
            pr = true;
        }
    }
    (w, pr)
}

fn star_value(a: &mut Args, is_width: bool) -> PyResult<(isize, ArgName)> {
    let (v, nm) = a.next()?;
    if !int::is_int(&v) {
        raise!(TypeError, "{}* requires int, not {}", nm.prefix(), type_fqname(v.type_ptr()));
    }
    let x = int::as_i64(&v);
    let what = if is_width { "width" } else { "precision" };
    match x {
        Some(x) if is_width && x != i64::MIN => Ok((x as isize, nm)),
        Some(x) if !is_width && x >= i32::MIN as i64 && x <= i32::MAX as i64 => Ok((x as isize, nm)),
        _ => raise!(OverflowError, "{}too big for {}", nm.prefix(), what),
    }
}

fn numfmt_err(e: rpy_numfmt::NumFmtError) -> PyErr {
    crate::types::format_impl::numfmt_error(e)
}

fn key_text(f: &PyObject, k: (usize, usize)) -> PyRef {
    super::writer::substring(f, k.0, k.1)
}

/// PyNumber_Long-like conversion used by %d/%i/%u.
fn number_long(v: &PyObject) -> PyResult {
    if int::is_int(v) {
        return Ok(v.to_ref());
    }
    unsafe {
        let nb = v.ty().tp_as_number;
        if !nb.is_null() {
            if let Some(f) = (*nb).nb_int {
                let r = from_c(f(v.as_ptr()))?;
                if !int::is_int(&r) {
                    raise!(TypeError, "__int__ returned non-int (type {})", tname(&r));
                }
                return Ok(r);
            }
            if (*nb).nb_index.is_some() {
                return ab::index(v);
            }
        }
    }
    raise!(TypeError, "int() argument must be a string, a bytes-like object or a real number, not '{}'", tname(v))
}

fn has_nb(v: &PyObject, f: impl Fn(&crate::ffi::PyNumberMethods) -> bool) -> bool {
    let nb = v.ty().tp_as_number;
    !nb.is_null() && f(unsafe { &*nb })
}

fn is_real_number(v: &PyObject) -> bool {
    if complex::is_complex(v) {
        return false;
    }
    has_nb(v, |n| n.nb_index.is_some() || n.nb_int.is_some() || n.nb_float.is_some())
}

/// `fmt % args` for str.
pub fn percent_format(fmt: &PyObject, args: &PyObject) -> PyResult {
    let f = pystr::str_data(fmt);
    let n = f.len();
    let (arglen, argidx) = if tuple::is_tuple(args) { (tuple::tuple_len(args) as isize, 0) } else { (-1, -2) };
    let dict = if is_mapping(args) { Some(args) } else { None };
    let mut a = Args { args, arglen, argidx, dict };
    let mut out = Out::new();
    let mut i = 0;
    let mut used_key = false;
    let printable = |c: u32| rpy_unicode::is_printable(c);
    while i < n {
        // literal run
        let st = i;
        match f {
            StrData::L1(s) => {
                i += super::search::find_unit(&s[i..], b'%').unwrap_or(n - i);
            }
            _ => {
                while i < n && f.get(i) != '%' as u32 {
                    i += 1;
                }
            }
        }
        if i > st {
            if st == 0 && i == n && pystr::is_str_exact(fmt) {
                out.push_obj(fmt.to_ref());
            } else {
                out.push_data(slice_data(f, st, i));
            }
        }
        if i >= n {
            break;
        }
        // (perf6) bare %s / %d / %i (no key, flags, width or precision): no spec parse,
        // exact str and machine-size int values written directly
        if i + 1 < n && !(used_key && a.dict.is_some()) {
            let c = f.get(i + 1);
            if c == 's' as u32 || c == 'd' as u32 || c == 'i' as u32 {
                let (v, nm) = a.next()?;
                i += 2;
                if c == 's' as u32 && pystr::is_str_exact(&v) {
                    out.push_obj(v);
                } else if c != 's' as u32 && int::is_int_exact(&v) && int::as_i64(&v).is_some() {
                    let x = int::as_i64(&v).unwrap();
                    let mut buf = [0u8; 20];
                    let mut k = buf.len();
                    let mut m = x.unsigned_abs();
                    loop {
                        k -= 1;
                        buf[k] = b'0' + (m % 10) as u8;
                        m /= 10;
                        if m == 0 {
                            break;
                        }
                    }
                    if x < 0 {
                        out.push_str("-");
                    }
                    out.push_str(unsafe { std::str::from_utf8_unchecked(&buf[k..]) });
                } else {
                    format_one(&mut out, &v, &nm, char::from_u32(c).unwrap_or('s'), &PercentOpts::default())?;
                }
                continue;
            }
            // %(key)s with a key without parentheses (logging's record formats)
            if c == '(' as u32 && a.dict.is_some() {
                let mut j = i + 2;
                while j < n && f.get(j) != ')' as u32 && f.get(j) != '(' as u32 {
                    j += 1;
                }
                if j + 1 < n && f.get(j) == ')' as u32 && f.get(j + 1) == 's' as u32 {
                    let key = key_text(fmt, (i + 2, j));
                    let v = ab::getitem(a.dict.unwrap(), &key)?;
                    used_key = true;
                    a.arglen = -1;
                    a.argidx = -2;
                    i = j + 2;
                    if pystr::is_str_exact(&v) {
                        out.push_obj(v);
                    } else {
                        format_one(&mut out, &v, &ArgName::Key(pystr::to_string_lossy(&key)), 's', &PercentOpts::default())?;
                    }
                    continue;
                }
            }
        }
        let parsed = dispatch_parse(f, i, &printable);
        let spec: PercentSpec = match parsed {
            Ok(s) => s,
            Err(e) => {
                let msg = e.msg.clone();
                let conv_problem = msg.starts_with("unsupported format") || msg.contains("unexpected format character");
                if conv_problem {
                    // CPython fetches the argument before validating the
                    // conversion character.
                    if !key_present(f, i) {
                        let (w, p) = count_stars(f, i);
                        a.need(w as isize + p as isize + 1)?;
                        if w {
                            star_value(&mut a, true)?;
                        }
                        if p {
                            star_value(&mut a, false)?;
                        }
                        if used_key && dict.is_some() {
                            raise!(ValueError, "format requires a parenthesised mapping key at position {}", i);
                        }
                        a.next()?;
                    }
                }
                return Err(numfmt_err(e));
            }
        };
        let start = i;
        i = spec.end;
        if spec.conv == '%' && spec.key.is_none() {
            out.push_str("%");
            continue;
        }
        // fetch the value
        let (value, name): (PyRef, ArgName) = match spec.key {
            Some(k) => {
                let d = match a.dict {
                    Some(d) => d,
                    None => raise!(TypeError, "format requires a mapping, not {}", tname(args)),
                };
                let key = key_text(fmt, k);
                let v = ab::getitem(d, &key)?;
                used_key = true;
                a.arglen = -1;
                a.argidx = -2;
                (v, ArgName::Key(pystr::to_string_lossy(&key)))
            }
            None => {
                if used_key && a.dict.is_some() {
                    raise!(ValueError, "format requires a parenthesised mapping key at position {}", start);
                }
                let mut opts_w = None;
                let mut opts_p = None;
                let nstars = (spec.width == PercentNum::Star) as isize + (spec.precision == PercentNum::Star) as isize;
                if nstars > 0 {
                    a.need(nstars + 1)?;
                }
                if spec.width == PercentNum::Star {
                    opts_w = Some(star_value(&mut a, true)?.0);
                }
                if spec.precision == PercentNum::Star {
                    opts_p = Some(star_value(&mut a, false)?.0);
                }
                let (v, nm) = a.next()?;
                let mut opts = PercentOpts::from_spec(&spec);
                if let Some(w) = opts_w {
                    if w < 0 {
                        opts.flags.left = true;
                        opts.width = Some((-w) as usize);
                    } else {
                        opts.width = Some(w as usize);
                    }
                }
                if let Some(p) = opts_p {
                    opts.precision = if p < 0 { None } else { Some(p as usize) };
                }
                format_one(&mut out, &v, &nm, spec.conv, &opts)?;
                continue;
            }
        };
        if spec.conv == '%' {
            out.push_str("%");
            continue;
        }
        let opts = PercentOpts::from_spec(&spec);
        format_one(&mut out, &value, &name, spec.conv, &opts)?;
    }
    if a.argidx < a.arglen && a.dict.is_none() {
        let (req, got) = if a.arglen < 0 { (0, 1) } else { (a.argidx, a.arglen) };
        let req = if a.arglen < 0 { if a.argidx == -1 { 1 } else { req } } else { req };
        raise!(TypeError, "not all arguments converted during string formatting (required {}, got {})", req, got);
    }
    Ok(out.finish())
}

fn key_present(f: StrData, i: usize) -> bool {
    i + 1 < f.len() && f.get(i + 1) == '(' as u32
}

fn dispatch_parse(f: StrData, i: usize, printable: &dyn Fn(u32) -> bool) -> Result<PercentSpec, rpy_numfmt::NumFmtError> {
    match f {
        StrData::L1(s) => pct::parse_percent_spec(s, i, false, printable),
        StrData::U2(s) => pct::parse_percent_spec(s, i, false, printable),
        StrData::U4(s) => pct::parse_percent_spec(s, i, false, printable),
    }
}

fn push_str_layout(out: &mut Out, s: PyRef, conv: char, opts: &PercentOpts) {
    let len = pystr::str_len(&s);
    let (take, l, r) = pct::percent_str_layout(len, conv, opts);
    out.push_repeat(' ' as u32, l);
    if take == len {
        out.push_obj(s);
    } else {
        out.push_data(slice_data(pystr::str_data(&s), 0, take));
    }
    out.push_repeat(' ' as u32, r);
}

fn format_one(out: &mut Out, v: &PyObject, nm: &ArgName, conv: char, opts: &PercentOpts) -> PyResult<()> {
    match conv {
        's' | 'r' | 'a' => {
            let s = match conv {
                's' => {
                    if pystr::is_str_exact(v) {
                        v.to_ref()
                    } else {
                        ab::str(v)?
                    }
                }
                'r' => ab::repr(v)?,
                _ => ab::ascii(v)?,
            };
            push_str_layout(out, s, conv, opts);
            Ok(())
        }
        'c' => {
            let c: u32 = if pystr::is_str(v) {
                if pystr::str_len(v) != 1 {
                    raise!(
                        TypeError,
                        "{}%c requires an integer or a unicode character, not a string of length {}",
                        nm.prefix(),
                        pystr::str_len(v)
                    );
                }
                pystr::str_data(v).get(0)
            } else {
                if !ab::has_index(v) {
                    raise!(
                        TypeError,
                        "{}%c requires an integer or a unicode character, not {}",
                        nm.prefix(),
                        type_fqname(v.type_ptr())
                    );
                }
                let iv = ab::index(v)?;
                match int::as_i64(&iv) {
                    Some(x) if (0..0x110000).contains(&x) => x as u32,
                    _ => raise!(OverflowError, "{}{}", nm.prefix(), pct::percent_char_range_error(false)),
                }
            };
            let s = pystr::from_codepoints(&[c]);
            push_str_layout(out, s, 'c', opts);
            Ok(())
        }
        'd' | 'i' | 'u' | 'x' | 'X' | 'o' => {
            let iv = if int::is_int(v) {
                v.to_ref()
            } else if matches!(conv, 'x' | 'X' | 'o') {
                if !ab::has_index(v) {
                    raise!(TypeError, "{}%{} requires an integer, not {}", nm.prefix(), conv, type_fqname(v.type_ptr()));
                }
                ab::index(v)?
            } else {
                if !is_real_number(v) {
                    raise!(TypeError, "{}%{} requires a real number, not {}", nm.prefix(), conv, type_fqname(v.type_ptr()));
                }
                number_long(v)?
            };
            let radix = match conv {
                'x' | 'X' => 16,
                'o' => 8,
                _ => 10,
            };
            let digits = int::int_methods_mag_string(&iv, radix);
            let mut s = String::new();
            pct::percent_format_int(&mut s, int::sign(&iv) < 0, &digits, conv, opts).map_err(numfmt_err)?;
            out.push_str(&s);
            Ok(())
        }
        'e' | 'E' | 'f' | 'F' | 'g' | 'G' => {
            let x = if float::is_float(v) {
                float::float_value(v)
            } else {
                if !is_real_number(v) {
                    raise!(TypeError, "{}%{} requires a real number, not {}", nm.prefix(), conv, type_fqname(v.type_ptr()));
                }
                ab::float_value(v)?
            };
            let mut s = String::new();
            pct::percent_format_float(&mut s, x, conv, opts).map_err(numfmt_err)?;
            out.push_str(&s);
            Ok(())
        }
        _ => raise!(ValueError, "unsupported format character '{}'", conv),
    }
}
