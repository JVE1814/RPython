//! `str.format` / `str.format_map` engine and the `_string` helpers
//! (`formatter_parser`, `formatter_field_name_split`).

use super::writer::{substring, Writer};
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::*;
use crate::raise;
use crate::types::str::{self as pystr, StrData};
use crate::types::*;

/// Output accumulator that avoids copying when the result is a single
/// exact str object.
pub struct Out {
    single: Option<PyRef>,
    w: Writer,
    used: bool,
}
impl Out {
    pub fn new() -> Self {
        Out { single: None, w: Writer::new(), used: false }
    }
    fn flush(&mut self) {
        if let Some(s) = self.single.take() {
            self.w.push_obj(&s);
        }
        self.used = true;
    }
    pub fn push_obj(&mut self, o: PyRef) {
        if pystr::str_len(&o) == 0 {
            return;
        }
        if !self.used && self.single.is_none() && pystr::is_str_exact(&o) {
            self.single = Some(o);
            return;
        }
        self.flush();
        self.w.push_obj(&o);
    }
    pub fn push_data(&mut self, d: StrData) {
        if d.len() == 0 {
            return;
        }
        self.flush();
        self.w.push_data(d);
    }
    pub fn push_str(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.flush();
        self.w.push_str(s);
    }
    pub fn push_repeat(&mut self, c: u32, n: usize) {
        if n == 0 {
            return;
        }
        self.flush();
        self.w.push_repeat(c, n);
    }
    pub fn finish(self) -> PyRef {
        if !self.used {
            if let Some(s) = self.single {
                return s;
            }
        }
        self.w.finish()
    }
}

/// Unparsed range [start, end) of the format string.
#[derive(Clone, Copy)]
struct Sub {
    start: usize,
    end: usize,
}

struct Field {
    name: Sub,
    spec: Sub,
    spec_needs_expanding: bool,
    conversion: Option<u32>,
}

enum Markup {
    /// literal text (may be empty) and an optional field
    Item(Sub, Option<Field>),
}

struct MarkupIter<'a> {
    s: StrData<'a>,
    pos: usize,
    end: usize,
}

impl<'a> MarkupIter<'a> {
    fn new(s: StrData<'a>, start: usize, end: usize) -> Self {
        MarkupIter { s, pos: start, end }
    }

    fn next(&mut self) -> PyResult<Option<Markup>> {
        if self.pos >= self.end {
            return Ok(None);
        }
        let s = self.s;
        let lit_start = self.pos;
        let mut c = 0u32;
        let mut markup_follows = false;
        while self.pos < self.end {
            c = s.get(self.pos);
            self.pos += 1;
            if c == '{' as u32 || c == '}' as u32 {
                markup_follows = true;
                break;
            }
        }
        let at_end = self.pos >= self.end;
        let mut lit_len = self.pos - lit_start;
        if markup_follows {
            if c == '}' as u32 && (at_end || s.get(self.pos) != '}' as u32) {
                raise!(ValueError, "Single '}}' encountered in format string");
            }
            if at_end && c == '{' as u32 {
                raise!(ValueError, "Single '{{' encountered in format string");
            }
            if !at_end && s.get(self.pos) == c {
                // escaped brace: literal includes one brace, skip the second
                self.pos += 1;
                markup_follows = false;
            } else {
                lit_len -= 1;
            }
        }
        let lit = Sub { start: lit_start, end: lit_start + lit_len };
        if !markup_follows {
            return Ok(Some(Markup::Item(lit, None)));
        }
        // parse the field up to the matching '}'
        let fstart = self.pos;
        let mut count = 1usize;
        let mut in_name = true;
        let mut name_end = None;
        let mut conv: Option<u32> = None;
        let mut conv_seen = false;
        let mut spec_start = None;
        let mut needs_expanding = false;
        loop {
            if self.pos >= self.end {
                raise!(ValueError, "expected '}}' before end of string");
            }
            let c = s.get(self.pos);
            self.pos += 1;
            if in_name {
                if c == '[' as u32 {
                    while self.pos < self.end && s.get(self.pos) != ']' as u32 {
                        self.pos += 1;
                    }
                    continue;
                }
                if c == '{' as u32 {
                    raise!(ValueError, "unexpected '{{' in field name");
                }
                if c == '}' as u32 {
                    name_end = Some(self.pos - 1);
                    break;
                }
                if c == ':' as u32 {
                    name_end = Some(self.pos - 1);
                    in_name = false;
                    spec_start = Some(self.pos);
                    continue;
                }
                if c == '!' as u32 {
                    name_end = Some(self.pos - 1);
                    in_name = false;
                    conv_seen = true;
                    // conversion char
                    if self.pos >= self.end {
                        raise!(ValueError, "end of string while looking for conversion specifier");
                    }
                    let cc = s.get(self.pos);
                    self.pos += 1;
                    if cc == '}' as u32 {
                        raise!(ValueError, "end of string while looking for conversion specifier");
                    }
                    conv = Some(cc);
                    if self.pos >= self.end {
                        raise!(ValueError, "expected '}}' before end of string");
                    }
                    let nx = s.get(self.pos);
                    self.pos += 1;
                    if nx == ':' as u32 {
                        spec_start = Some(self.pos);
                        continue;
                    }
                    if nx == '}' as u32 {
                        break;
                    }
                    raise!(ValueError, "expected ':' after conversion specifier");
                }
                continue;
            }
            // inside format spec
            if c == '{' as u32 {
                needs_expanding = true;
                count += 1;
            } else if c == '}' as u32 {
                count -= 1;
                if count == 0 {
                    break;
                }
            }
        }
        let fend = self.pos - 1; // index of closing '}'
        let name_end = name_end.unwrap_or(fend);
        let spec = match spec_start {
            Some(st) => Sub { start: st, end: fend },
            None => Sub { start: fend, end: fend },
        };
        let _ = conv_seen;
        Ok(Some(Markup::Item(
            lit,
            Some(Field { name: Sub { start: fstart, end: name_end }, spec, spec_needs_expanding: needs_expanding, conversion: conv }),
        )))
    }
}

// ------------------------------------------------------------------ field names

enum FirstKey {
    Index(usize),
    Name(Sub),
    Auto,
}

enum Accessor {
    Attr(Sub),
    Index(usize),
    Key(Sub),
}

/// Parse decimal digits; Ok(None) if not all digits; error on overflow.
fn parse_index(s: StrData, sub: Sub) -> PyResult<Option<usize>> {
    if sub.start >= sub.end {
        return Ok(None);
    }
    let mut v: usize = 0;
    for i in sub.start..sub.end {
        let c = s.get(i);
        let d = match rpy_unicode::to_decimal(c) {
            Some(d) => d as usize,
            None => return Ok(None),
        };
        v = match v.checked_mul(10).and_then(|x| x.checked_add(d)) {
            Some(x) if x <= isize::MAX as usize => x,
            _ => raise!(ValueError, "Too many decimal digits in format string"),
        };
    }
    Ok(Some(v))
}

struct FieldNameIter<'a> {
    s: StrData<'a>,
    pos: usize,
    end: usize,
}

impl<'a> FieldNameIter<'a> {
    /// Split off the first part.
    fn first(s: StrData<'a>, name: Sub) -> PyResult<(FirstKey, FieldNameIter<'a>)> {
        let mut i = name.start;
        while i < name.end {
            let c = s.get(i);
            if c == '.' as u32 || c == '[' as u32 {
                break;
            }
            i += 1;
        }
        let first = Sub { start: name.start, end: i };
        let key = if first.start == first.end {
            FirstKey::Auto
        } else {
            match parse_index(s, first)? {
                Some(v) => FirstKey::Index(v),
                None => FirstKey::Name(first),
            }
        };
        Ok((key, FieldNameIter { s, pos: i, end: name.end }))
    }

    fn next(&mut self) -> PyResult<Option<Accessor>> {
        if self.pos >= self.end {
            return Ok(None);
        }
        let s = self.s;
        let c = s.get(self.pos);
        self.pos += 1;
        if c == '.' as u32 {
            let st = self.pos;
            while self.pos < self.end {
                let c = s.get(self.pos);
                if c == '.' as u32 || c == '[' as u32 {
                    break;
                }
                self.pos += 1;
            }
            if st == self.pos {
                raise!(ValueError, "Empty attribute in format string");
            }
            return Ok(Some(Accessor::Attr(Sub { start: st, end: self.pos })));
        }
        if c == '[' as u32 {
            let st = self.pos;
            let mut found = false;
            while self.pos < self.end {
                if s.get(self.pos) == ']' as u32 {
                    found = true;
                    break;
                }
                self.pos += 1;
            }
            if !found {
                raise!(ValueError, "Missing ']' in format string");
            }
            let sub = Sub { start: st, end: self.pos };
            self.pos += 1; // skip ']'
            if st == sub.end {
                raise!(ValueError, "Empty attribute in format string");
            }
            if self.pos < self.end {
                let nc = s.get(self.pos);
                if nc != '.' as u32 && nc != '[' as u32 {
                    raise!(ValueError, "Only '.' or '[' may follow ']' in format field specifier");
                }
            }
            return Ok(Some(match parse_index(s, sub)? {
                Some(v) => Accessor::Index(v),
                None => Accessor::Key(sub),
            }));
        }
        raise!(ValueError, "Only '.' or '[' may follow ']' in format field specifier")
    }
}

// ------------------------------------------------------------------ engine

struct Ctx<'a> {
    fmt: &'a PyObject,
    args: &'a [&'a PyObject],
    kwargs: Option<&'a PyObject>,
    is_map: bool,
    /// 0 = undetermined, 1 = auto numbering, 2 = manual
    numbering: u8,
    auto_idx: usize,
}

fn sub_str(s: &PyObject, sub: Sub) -> PyRef {
    substring(s, sub.start, sub.end)
}

impl<'a> Ctx<'a> {
    fn get_field(&mut self, name: Sub) -> PyResult {
        let s = pystr::str_data(self.fmt);
        let (first, mut rest) = FieldNameIter::first(s, name)?;
        let mut obj = match first {
            FirstKey::Name(k) => {
                let key = sub_str(self.fmt, k);
                match self.kwargs {
                    None => return Err(set_err_obj(exceptions::KeyError(), key)),
                    Some(kw) => {
                        if !self.is_map && dict::is_dict_exact(kw) {
                            match dict::get_item(kw, &key)? {
                                Some(v) => v,
                                None => return Err(set_err_obj(exceptions::KeyError(), key)),
                            }
                        } else {
                            ab::getitem(kw, &key)?
                        }
                    }
                }
            }
            FirstKey::Auto | FirstKey::Index(_) => {
                if self.is_map {
                    raise!(ValueError, "Format string contains positional fields");
                }
                let idx = match first {
                    FirstKey::Auto => {
                        if self.numbering == 2 {
                            raise!(ValueError, "cannot switch from manual field specification to automatic field numbering");
                        }
                        self.numbering = 1;
                        let i = self.auto_idx;
                        self.auto_idx += 1;
                        i
                    }
                    FirstKey::Index(i) => {
                        if self.numbering == 1 {
                            raise!(ValueError, "cannot switch from automatic field numbering to manual field specification");
                        }
                        self.numbering = 2;
                        i
                    }
                    _ => unreachable!(),
                };
                match self.args.get(idx) {
                    Some(v) => v.to_ref(),
                    None => raise!(IndexError, "Replacement index {} out of range for positional args tuple", idx),
                }
            }
        };
        while let Some(acc) = rest.next()? {
            obj = match acc {
                Accessor::Attr(a) => {
                    let nm = sub_str(self.fmt, a);
                    ab::getattr(&obj, &nm)?
                }
                Accessor::Index(i) => ab::getitem(&obj, &int::new_int_usize(i))?,
                Accessor::Key(k) => {
                    let key = sub_str(self.fmt, k);
                    ab::getitem(&obj, &key)?
                }
            };
        }
        Ok(obj)
    }

    fn build(&mut self, start: usize, end: usize, depth: i32, out: &mut Out) -> PyResult<()> {
        if depth <= 0 {
            raise!(ValueError, "Max string recursion exceeded");
        }
        let s = pystr::str_data(self.fmt);
        let mut it = MarkupIter::new(s, start, end);
        while let Some(Markup::Item(lit, field)) = it.next()? {
            if lit.end > lit.start {
                if lit.start == 0 && lit.end == pystr::str_len(self.fmt) && pystr::is_str_exact(self.fmt) {
                    out.push_obj(self.fmt.to_ref());
                } else {
                    out.push_data(super::writer::slice_data(s, lit.start, lit.end));
                }
            }
            let f = match field {
                Some(f) => f,
                None => continue,
            };
            let obj = self.get_field(f.name)?;
            let obj = match f.conversion {
                None => obj,
                Some(c) => convert(&obj, c)?,
            };
            let spec = if f.spec_needs_expanding {
                let mut sub = Out::new();
                self.build(f.spec.start, f.spec.end, depth - 1, &mut sub)?;
                sub.finish()
            } else {
                sub_str(self.fmt, f.spec)
            };
            out.push_obj(format_obj(&obj, &spec)?);
        }
        Ok(())
    }
}

pub fn convert(obj: &PyObject, c: u32) -> PyResult {
    match char::from_u32(c) {
        Some('r') => ab::repr(obj),
        Some('s') => ab::str(obj),
        Some('a') => ab::ascii(obj),
        _ => {
            if (32..127).contains(&c) {
                raise!(ValueError, "Unknown conversion specifier {}", c as u8 as char)
            } else {
                raise!(ValueError, "Unknown conversion specifier \\x{:x}", c)
            }
        }
    }
}

/// PyObject_Format with fast paths.
pub fn format_obj(obj: &PyObject, spec: &PyObject) -> PyResult {
    if pystr::str_len(spec) == 0 {
        if pystr::is_str_exact(obj) {
            return Ok(obj.to_ref());
        }
        if int::is_int_exact(obj) {
            return ab::str(obj);
        }
    }
    ab::format(obj, spec)
}

/// `fmt.format(*args, **kwargs)` (or format_map when `is_map`).
pub fn format_with(fmt: &PyObject, args: &[&PyObject], kwargs: Option<&PyObject>, is_map: bool) -> PyResult {
    let n = pystr::str_len(fmt);
    let mut ctx = Ctx { fmt, args, kwargs, is_map, numbering: 0, auto_idx: 0 };
    let mut out = Out::new();
    ctx.build(0, n, 2, &mut out)?;
    let r = out.finish();
    Ok(r)
}

// ------------------------------------------------------------------ _string helpers

fn opt_str(o: &PyObject, sub: Sub) -> PyRef {
    sub_str(o, sub)
}

/// `_string.formatter_parser(str)`: iterator of
/// (literal, field_name, format_spec, conversion) tuples.
pub fn formatter_parser(s: &PyObject) -> PyResult {
    if !pystr::is_str(s) {
        raise!(TypeError, "expected str, got {}", crate::types::typeobject::tname(s));
    }
    let d = pystr::str_data(s);
    let mut it = MarkupIter::new(d, 0, d.len());
    let mut items = Vec::new();
    while let Some(Markup::Item(lit, field)) = it.next()? {
        let litobj = opt_str(s, lit);
        let t = match field {
            None => tuple::new_tuple(vec![litobj, none::none_ref(), none::none_ref(), none::none_ref()]),
            Some(f) => {
                let conv = match f.conversion {
                    None => none::none_ref(),
                    Some(c) => pystr::from_codepoints(&[c]),
                };
                tuple::new_tuple(vec![litobj, opt_str(s, f.name), opt_str(s, f.spec), conv])
            }
        };
        items.push(t);
    }
    ab::get_iter(&list::new_list(items))
}

/// `_string.formatter_field_name_split(str)`: (first, iterator of (is_attr, key)).
pub fn formatter_field_name_split(s: &PyObject) -> PyResult {
    if !pystr::is_str(s) {
        raise!(TypeError, "expected str, got {}", crate::types::typeobject::tname(s));
    }
    let d = pystr::str_data(s);
    let (first, mut rest) = FieldNameIter::first(d, Sub { start: 0, end: d.len() })?;
    let first_obj = match first {
        FirstKey::Index(i) => int::new_int_usize(i),
        FirstKey::Name(sub) => sub_str(s, sub),
        FirstKey::Auto => pystr::new_str(""),
    };
    let mut items = Vec::new();
    while let Some(acc) = rest.next()? {
        let t = match acc {
            Accessor::Attr(a) => tuple::new_tuple(vec![bool_::new_bool(true), sub_str(s, a)]),
            Accessor::Index(i) => tuple::new_tuple(vec![bool_::new_bool(false), int::new_int_usize(i)]),
            Accessor::Key(k) => tuple::new_tuple(vec![bool_::new_bool(false), sub_str(s, k)]),
        };
        items.push(t);
    }
    let it = ab::get_iter(&list::new_list(items))?;
    Ok(tuple::new_tuple(vec![first_obj, it]))
}
