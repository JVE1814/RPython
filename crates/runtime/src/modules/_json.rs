//! `_json`: accelerators for the `json` package (data workstream).
//!
//! * `scanstring(s, end, strict=True)`
//! * `encode_basestring(s)`, `encode_basestring_ascii(s)`
//! * `make_scanner(context)` -> callable `scanner(string, idx)` (type `_json.Scanner`)
//! * `make_encoder(markers, default, encoder, indent, key_separator,
//!   item_separator, sort_keys, skipkeys, allow_nan)` -> callable
//!   `encoder(obj, _current_indent_level)` returning a 1-tuple with the text
//!   (type `_json.Encoder`).
//!
//! Error messages and positions follow the pure-Python implementation in
//! Lib/json/decoder.py exactly (the test suite runs both).

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::str::{StrBuilder, StrData};
use crate::types::typeobject::*;
use crate::types::{bool_, dict, float, int, list, none, str as pystr, tuple};
use crate::{mdefs, name, raise};

// ------------------------------------------------------------------ errors

fn decode_error(msg: &str, s: &PyObject, pos: usize) -> PyErr {
    let r = (|| -> PyResult<PyErr> {
        let m = crate::import::import_absolute("json.decoder")?;
        let cls = ab::getattr_str(&m, "JSONDecodeError")?;
        let e = ab::call(&cls, &[&pystr::new_str(msg), s, &int::new_int(pos as i64)])?;
        set_exc(e);
        Ok(PyErr)
    })();
    match r {
        Ok(e) => e,
        Err(e) => e,
    }
}

fn stop_iteration(idx: isize) -> PyErr {
    set_err_obj(crate::types::exceptions::StopIteration(), int::new_int(idx as i64))
}

/// Add a note to the pending exception (like `exc.add_note(...)`).
fn add_note(note: String) -> PyErr {
    if let Some(e) = take_exc() {
        let r = ab::call_method(&e, name!("add_note"), &[&pystr::new_str(&note)]);
        match r {
            Ok(_) => set_exc(e),
            Err(_) => {
                // adding the note failed: keep the original exception
                clear_exc();
                set_exc(e);
            }
        }
    }
    PyErr
}

fn ts() -> &'static crate::threads::ThreadState {
    crate::threads::current()
}

// ------------------------------------------------------------------ scanstring

fn is_hex(c: u32) -> bool {
    (c >= b'0' as u32 && c <= b'9' as u32) || (c >= b'a' as u32 && c <= b'f' as u32) || (c >= b'A' as u32 && c <= b'F' as u32)
}

fn hexval(c: u32) -> u32 {
    if c <= b'9' as u32 {
        c - b'0' as u32
    } else if c >= b'a' as u32 {
        c - b'a' as u32 + 10
    } else {
        c - b'A' as u32 + 10
    }
}

/// Decode the 4 hex digits after `\u` (pos = index of 'u'); None if invalid.
fn decode_u4(d: &StrData, pos: usize) -> Option<u32> {
    if pos + 5 > d.len() {
        return None;
    }
    let mut v = 0;
    for i in 1..5 {
        let c = d.get(pos + i);
        if !is_hex(c) {
            return None;
        }
        v = v * 16 + hexval(c);
    }
    Some(v)
}

/// Scan a JSON string body starting at `end` (just after the opening quote).
/// Returns the decoded string object and the index after the closing quote.
fn scan_string(s: &PyObject, d: &StrData, end: usize, strict: bool) -> PyResult<(PyRef, usize)> {
    let len = d.len();
    let begin = end.wrapping_sub(1);
    let mut i = end;
    // Fast path: no escapes.
    let mut j = i;
    while j < len {
        let c = d.get(j);
        if c == b'"' as u32 {
            let out = match d {
                StrData::L1(b) => pystr::from_latin1(&b[i..j]),
                StrData::U2(b) => pystr::from_ucs2(&b[i..j]),
                StrData::U4(b) => pystr::from_codepoints(&b[i..j]),
            };
            return Ok((out, j + 1));
        }
        if c == b'\\' as u32 || c < 0x20 {
            break;
        }
        j += 1;
    }
    let mut sb = StrBuilder::with_capacity(j - i + 16);
    loop {
        // copy unescaped run
        let mut j = i;
        let mut term = None;
        while j < len {
            let c = d.get(j);
            if c == b'"' as u32 || c == b'\\' as u32 {
                term = Some(c);
                break;
            }
            if c < 0x20 {
                if strict {
                    let ch = char::from_u32(c).unwrap();
                    let msg = format!("Invalid control character {} at", py_char_repr(ch));
                    return Err(decode_error(&msg, s, j));
                }
            }
            j += 1;
        }
        for k in i..j {
            sb.push(d.get(k));
        }
        let t = match term {
            None => return Err(decode_error("Unterminated string starting at", s, begin)),
            Some(t) => t,
        };
        if t == b'"' as u32 {
            return Ok((sb.finish(), j + 1));
        }
        // backslash
        let e = j + 1;
        if e >= len {
            return Err(decode_error("Unterminated string starting at", s, begin));
        }
        let esc = d.get(e);
        if esc != b'u' as u32 {
            let ch = match char::from_u32(esc).unwrap_or('\0') {
                '"' => '"',
                '\\' => '\\',
                '/' => '/',
                'b' => '\u{8}',
                'f' => '\u{c}',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                _ => {
                    let ch = char::from_u32(esc).unwrap_or('\u{fffd}');
                    let msg = format!("Invalid \\escape: {}", py_char_repr_any(esc, ch));
                    return Err(decode_error(&msg, s, e));
                }
            };
            sb.push(ch as u32);
            i = e + 1;
        } else {
            let mut uni = match decode_u4(d, e) {
                Some(v) => v,
                None => return Err(decode_error("Invalid \\uXXXX escape", s, e)),
            };
            let mut next = e + 5;
            if (0xd800..=0xdbff).contains(&uni) && next + 1 < len && d.get(next) == b'\\' as u32 && d.get(next + 1) == b'u' as u32 {
                let uni2 = match decode_u4(d, next + 1) {
                    Some(v) => v,
                    None => return Err(decode_error("Invalid \\uXXXX escape", s, next + 1)),
                };
                if (0xdc00..=0xdfff).contains(&uni2) {
                    uni = 0x10000 + (((uni - 0xd800) << 10) | (uni2 - 0xdc00));
                    next += 6;
                }
            }
            sb.push(uni);
            i = next;
        }
    }
}

/// repr() of a one-character string (for error messages).
fn py_char_repr(c: char) -> String {
    let s = pystr::new_str(&c.to_string());
    ab::repr_string(&s).unwrap_or_else(|_| {
        clear_exc();
        format!("'{}'", c)
    })
}

fn py_char_repr_any(cp: u32, _c: char) -> String {
    let s = pystr::from_codepoints(&[cp]);
    ab::repr_string(&s).unwrap_or_else(|_| {
        clear_exc();
        "'?'".to_string()
    })
}

fn index_arg(o: &PyObject) -> PyResult<isize> {
    ab::index_as_ssize(o, Some(crate::types::exceptions::OverflowError()))
}

fn m_scanstring(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let p = crate::args::Params::new("scanstring", &["string", "end", "strict"], 0, 2);
    let [s, end, strict] = crate::args::parse::<3>(&p, args, kw)?;
    let s = s.unwrap();
    if !pystr::is_str(s) {
        raise!(TypeError, "first argument must be a string, not {}", tname(s));
    }
    let end = index_arg(end.unwrap())?;
    let strict = match strict {
        Some(v) => ab::is_true(v)?,
        None => true,
    };
    let d = pystr::str_data(s);
    if end < 0 || end as usize > d.len() {
        raise!(ValueError, "end is out of bounds");
    }
    let (r, e) = scan_string(s, &d, end as usize, strict)?;
    Ok(tuple::tuple2(r, int::new_int(e as i64)))
}

// ------------------------------------------------------------------ basestring encoders

fn push_u_escape(out: &mut StrBuilder, c: u32) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push(b'\\' as u32);
    out.push(b'u' as u32);
    out.push(HEX[((c >> 12) & 0xf) as usize] as u32);
    out.push(HEX[((c >> 8) & 0xf) as usize] as u32);
    out.push(HEX[((c >> 4) & 0xf) as usize] as u32);
    out.push(HEX[(c & 0xf) as usize] as u32);
}

/// Characters that are copied unchanged into JSON text by both escapers.
#[inline]
fn json_plain(c: u8) -> bool {
    c >= 0x20 && c != b'"' && c != b'\\' && c < 0x7f
}

fn escape_into(out: &mut StrBuilder, s: &PyObject, ascii: bool) {
    let d = pystr::str_data(s);
    if let StrData::L1(b) = d {
        // ASCII/Latin-1: copy runs of plain characters in one go
        out.push_ascii(b"\"");
        let mut start = 0;
        let mut i = 0;
        while i < b.len() {
            let c = b[i];
            if json_plain(c) {
                i += 1;
                continue;
            }
            if c >= 0x80 && !ascii {
                // non-ASCII Latin-1 is copied unescaped
                i += 1;
                continue;
            }
            if start < i {
                out.push_data(StrData::L1(&b[start..i]));
            }
            escape_one(out, c as u32, ascii);
            i += 1;
            start = i;
        }
        if start < b.len() {
            out.push_data(StrData::L1(&b[start..]));
        }
        out.push_ascii(b"\"");
        return;
    }
    out.push(b'"' as u32);
    for i in 0..d.len() {
        escape_one(out, d.get(i), ascii);
    }
    out.push(b'"' as u32);
}

/// Append the JSON form of one character.
#[inline]
fn escape_one(out: &mut StrBuilder, c: u32, ascii: bool) {
    {
        match c {
            0x22 => {
                out.push(b'\\' as u32);
                out.push(b'"' as u32);
            }
            0x5c => {
                out.push(b'\\' as u32);
                out.push(b'\\' as u32);
            }
            0x08 => {
                out.push(b'\\' as u32);
                out.push(b'b' as u32);
            }
            0x0c => {
                out.push(b'\\' as u32);
                out.push(b'f' as u32);
            }
            0x0a => {
                out.push(b'\\' as u32);
                out.push(b'n' as u32);
            }
            0x0d => {
                out.push(b'\\' as u32);
                out.push(b'r' as u32);
            }
            0x09 => {
                out.push(b'\\' as u32);
                out.push(b't' as u32);
            }
            _ if c < 0x20 => push_u_escape(out, c),
            _ if ascii && c >= 0x7f => {
                if c >= 0x10000 {
                    let v = c - 0x10000;
                    push_u_escape(out, 0xd800 | ((v >> 10) & 0x3ff));
                    push_u_escape(out, 0xdc00 | (v & 0x3ff));
                } else {
                    push_u_escape(out, c);
                }
            }
            _ => out.push(c),
        }
    }
}

fn escape_str(s: &PyObject, ascii: bool) -> PyRef {
    let mut out = StrBuilder::with_capacity(pystr::str_len(s) + 2);
    escape_into(&mut out, s, ascii);
    out.finish()
}

fn m_encode_basestring_ascii(_m: &PyObject, s: &PyObject) -> PyResult {
    if !pystr::is_str(s) {
        raise!(TypeError, "first argument must be a string, not {}", tname(s));
    }
    Ok(escape_str(s, true))
}

fn m_encode_basestring(_m: &PyObject, s: &PyObject) -> PyResult {
    if !pystr::is_str(s) {
        raise!(TypeError, "first argument must be a string, not {}", tname(s));
    }
    Ok(escape_str(s, false))
}

// ------------------------------------------------------------------ scanner type

#[repr(C)]
struct ScannerObj {
    ob_base: PyObject,
    strict: bool,
    object_hook: *mut PyObject,
    object_pairs_hook: *mut PyObject,
    array_hook: *mut PyObject,
    parse_float: *mut PyObject,
    parse_int: *mut PyObject,
    parse_constant: *mut PyObject,
    memo: *mut PyObject,
}

pub static mut SCANNER_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut ENCODER_TYPE: PyTypeObject = PyTypeObject::ZERO;

fn opt(p: *mut PyObject) -> Option<&'static PyObject> {
    if p.is_null() || unsafe { (*p).is(none::none()) } {
        None
    } else {
        Some(unsafe { &*p })
    }
}

struct Scanner<'a> {
    sc: &'a ScannerObj,
    s: &'a PyObject,
    d: StrData<'a>,
    /// Object keys seen during this call (equal keys share one str object). Kept
    /// here instead of in the scanner's `memo` dict, which the call would empty
    /// again anyway (perf3).
    memo: std::cell::RefCell<KeyMemo>,
}

/// Small open-addressing set of key strs keyed by their (cached) hash.
#[derive(Default)]
struct KeyMemo {
    slots: Vec<*mut PyObject>,
    len: usize,
}

impl KeyMemo {
    /// The memoized str equal to `key`, or `key` itself after remembering it.
    fn intern(&mut self, key: PyRef) -> PyRef {
        if self.slots.is_empty() {
            self.slots = vec![std::ptr::null_mut(); 32];
        }
        let h = pystr::str_hash(&key) as usize;
        let mask = self.slots.len() - 1;
        let mut i = (h ^ (h >> 17)) & mask;
        loop {
            let p = self.slots[i];
            if p.is_null() {
                break;
            }
            let o = unsafe { &*p };
            if pystr::str_hash(o) as usize == h && pystr::str_eq(o, &key) {
                return o.to_ref();
            }
            i = (i + 1) & mask;
        }
        self.slots[i] = key.to_ref().into_raw();
        self.len += 1;
        if self.len * 2 > self.slots.len() {
            let n2 = self.slots.len() * 2;
            let old = std::mem::replace(&mut self.slots, vec![std::ptr::null_mut(); n2]);
            let mask = self.slots.len() - 1;
            for p in old.into_iter().filter(|p| !p.is_null()) {
                let h = pystr::str_hash(unsafe { &*p }) as usize;
                let mut j = (h ^ (h >> 17)) & mask;
                while !self.slots[j].is_null() {
                    j = (j + 1) & mask;
                }
                self.slots[j] = p;
            }
        }
        key
    }
}

impl Drop for KeyMemo {
    fn drop(&mut self) {
        for &p in &self.slots {
            if !p.is_null() {
                unsafe { crate::object::decref(p) };
            }
        }
    }
}

const WS: [u32; 4] = [0x20, 0x09, 0x0a, 0x0d];

impl<'a> Scanner<'a> {
    #[inline]
    fn at(&self, i: usize) -> Option<u32> {
        if i < self.d.len() { Some(self.d.get(i)) } else { None }
    }
    #[inline]
    fn skip_ws(&self, mut i: usize) -> usize {
        while let Some(c) = self.at(i) {
            if WS.contains(&c) {
                i += 1;
            } else {
                break;
            }
        }
        i
    }
    fn matches(&self, i: usize, lit: &str) -> bool {
        let b = lit.as_bytes();
        if i + b.len() > self.d.len() {
            return false;
        }
        b.iter().enumerate().all(|(k, &c)| self.d.get(i + k) == c as u32)
    }

    /// Scan one value at idx. Ok(None) means "no value here" (StopIteration).
    fn scan_once(&self, idx: usize) -> PyResult<Option<(PyRef, usize)>> {
        let c = match self.at(idx) {
            None => return Ok(None),
            Some(c) => c,
        };
        match c {
            0x22 => {
                let (s, e) = scan_string(self.s, &self.d, idx + 1, self.sc.strict)?;
                return Ok(Some((s, e)));
            }
            0x7b => {
                let t = ts();
                crate::interp::enter_recursive_call(t, " while decoding a JSON object from a unicode string")?;
                let r = self.parse_object(idx + 1);
                crate::interp::leave_recursive_call(t);
                return r.map(Some);
            }
            0x5b => {
                let t = ts();
                crate::interp::enter_recursive_call(t, " while decoding a JSON array from a unicode string")?;
                let r = self.parse_array(idx + 1);
                crate::interp::leave_recursive_call(t);
                return r.map(Some);
            }
            0x6e if self.matches(idx, "null") => return Ok(Some((none::none_ref(), idx + 4))),
            0x74 if self.matches(idx, "true") => return Ok(Some((bool_::true_obj().to_ref(), idx + 4))),
            0x66 if self.matches(idx, "false") => return Ok(Some((bool_::false_obj().to_ref(), idx + 5))),
            0x4e if self.matches(idx, "NaN") => return Ok(Some((self.constant("NaN")?, idx + 3))),
            0x49 if self.matches(idx, "Infinity") => return Ok(Some((self.constant("Infinity")?, idx + 8))),
            0x2d if self.matches(idx, "-Infinity") => return Ok(Some((self.constant("-Infinity")?, idx + 9))),
            _ => {}
        }
        self.match_number(idx)
    }

    fn constant(&self, name: &str) -> PyResult {
        let pc = unsafe { &*self.sc.parse_constant };
        ab::call(pc, &[&pystr::new_str(name)])
    }

    fn is_digit(c: Option<u32>) -> bool {
        matches!(c, Some(c) if (0x30..=0x39).contains(&c))
    }

    fn match_number(&self, start: usize) -> PyResult<Option<(PyRef, usize)>> {
        let mut i = start;
        if self.at(i) == Some(0x2d) {
            i += 1;
        }
        match self.at(i) {
            Some(0x30) => i += 1,
            Some(c) if (0x31..=0x39).contains(&c) => {
                i += 1;
                while Self::is_digit(self.at(i)) {
                    i += 1;
                }
            }
            _ => return Ok(None),
        }
        let mut is_float = false;
        if self.at(i) == Some(0x2e) && Self::is_digit(self.at(i + 1)) {
            is_float = true;
            i += 2;
            while Self::is_digit(self.at(i)) {
                i += 1;
            }
        }
        if matches!(self.at(i), Some(0x65) | Some(0x45)) {
            let mut j = i + 1;
            if matches!(self.at(j), Some(0x2b) | Some(0x2d)) {
                j += 1;
            }
            if Self::is_digit(self.at(j)) {
                while Self::is_digit(self.at(j)) {
                    j += 1;
                }
                is_float = true;
                i = j;
            }
        }
        if !is_float && i - start <= 18 {
            let pi = unsafe { &*self.sc.parse_int };
            if pi.is(type_obj(int::int_type())) {
                // machine-word integer: digits accumulated directly
                let neg = self.d.get(start) == 0x2d;
                let mut v: i64 = 0;
                for k in (start + neg as usize)..i {
                    v = v * 10 + (self.d.get(k) - 0x30) as i64;
                }
                return Ok(Some((int::new_int(if neg { -v } else { v }), i)));
            }
        }
        let text: String = (start..i).map(|k| self.d.get(k) as u8 as char).collect();
        let v = if is_float {
            let pf = unsafe { &*self.sc.parse_float };
            if pf.is(type_obj(float::float_type())) {
                match text.parse::<f64>() {
                    Ok(f) => float::new_float(f),
                    Err(_) => ab::call(pf, &[&pystr::new_str(&text)])?,
                }
            } else {
                ab::call(pf, &[&pystr::new_str(&text)])?
            }
        } else {
            let pi = unsafe { &*self.sc.parse_int };
            if pi.is(type_obj(int::int_type())) && text.len() <= 18 {
                int::new_int(text.parse::<i64>().unwrap())
            } else {
                ab::call(pi, &[&pystr::new_str(&text)])?
            }
        };
        Ok(Some((v, i)))
    }

    fn parse_object(&self, mut end: usize) -> PyResult<(PyRef, usize)> {
        let s = self.s;
        let pairs_hook = opt(self.sc.object_pairs_hook);
        let mut pairs: Vec<(PyRef, PyRef)> = Vec::new();
        // without a pairs hook the members go straight into the dict
        let direct = if pairs_hook.is_none() { Some(dict::new_dict()) } else { None };
        end = self.skip_ws(end);
        if self.at(end) != Some(0x7d) {
            loop {
                if self.at(end) != Some(0x22) {
                    return Err(decode_error("Expecting property name enclosed in double quotes", s, end));
                }
                let (key, e) = scan_string(s, &self.d, end + 1, self.sc.strict)?;
                // key memoization (identical keys share one object)
                let key = self.memo.borrow_mut().intern(key);
                end = self.skip_ws(e);
                if self.at(end) != Some(0x3a) {
                    return Err(decode_error("Expecting ':' delimiter", s, end));
                }
                end = self.skip_ws(end + 1);
                let (val, e) = match self.scan_once(end)? {
                    Some(v) => v,
                    None => return Err(decode_error("Expecting value", s, end)),
                };
                match &direct {
                    Some(d) => dict::set_item(d, &key, &val)?,
                    None => pairs.push((key, val)),
                }
                end = self.skip_ws(e);
                match self.at(end) {
                    Some(0x7d) => break,
                    Some(0x2c) => {
                        let comma = end;
                        end = self.skip_ws(end + 1);
                        if self.at(end) == Some(0x7d) {
                            return Err(decode_error("Illegal trailing comma before end of object", s, comma));
                        }
                    }
                    _ => return Err(decode_error("Expecting ',' delimiter", s, end)),
                }
            }
        }
        let end = end + 1;
        if let Some(h) = pairs_hook {
            let items: Vec<PyRef> = pairs.into_iter().map(|(k, v)| tuple::tuple2(k, v)).collect();
            let l = list::new_list(items);
            return Ok((ab::call(h, &[&l])?, end));
        }
        let d = match direct {
            Some(d) => d,
            None => dict::new_dict(),
        };
        if let Some(h) = opt(self.sc.object_hook) {
            return Ok((ab::call(h, &[&d])?, end));
        }
        Ok((d, end))
    }

    fn parse_array(&self, mut end: usize) -> PyResult<(PyRef, usize)> {
        let s = self.s;
        let mut items: Vec<PyRef> = Vec::new();
        end = self.skip_ws(end);
        if self.at(end) != Some(0x5d) {
            loop {
                let (val, e) = match self.scan_once(end)? {
                    Some(v) => v,
                    None => return Err(decode_error("Expecting value", s, end)),
                };
                items.push(val);
                end = self.skip_ws(e);
                match self.at(end) {
                    Some(0x5d) => break,
                    Some(0x2c) => {
                        let comma = end;
                        end = self.skip_ws(end + 1);
                        if self.at(end) == Some(0x5d) {
                            return Err(decode_error("Illegal trailing comma before end of array", s, comma));
                        }
                    }
                    _ => return Err(decode_error("Expecting ',' delimiter", s, end)),
                }
            }
        }
        let l = list::new_list(items);
        if let Some(h) = opt(self.sc.array_hook) {
            return Ok((ab::call(h, &[&l])?, end + 1));
        }
        Ok((l, end + 1))
    }
}

fn scanner_type() -> *mut PyTypeObject {
    &raw mut SCANNER_TYPE
}
fn encoder_type() -> *mut PyTypeObject {
    &raw mut ENCODER_TYPE
}

unsafe extern "C" fn scanner_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let a = tuple::tuple_items(&*args);
            let ctx_ref: PyRef = if a.len() == 1 && (kw.is_null() || dict::dict_len(&*kw) == 0) {
                a[0].to_ref()
            } else if a.is_empty() && !kw.is_null() && dict::dict_len(&*kw) == 1 {
                match dict::get_item_rs(&*kw, "context") {
                    Some(c) => c,
                    None => raise!(TypeError, "make_scanner() missing required argument 'context' (pos 1)"),
                }
            } else {
                raise!(TypeError, "make_scanner() takes exactly 1 argument ({} given)", a.len());
            };
            let ctx: &PyObject = &ctx_ref;
            let strict_o = ab::getattr_str(ctx, "strict")?;
            let strict = ab::is_true(&strict_o)?;
            let object_hook = ab::getattr_str(ctx, "object_hook")?;
            let object_pairs_hook = ab::getattr_str(ctx, "object_pairs_hook")?;
            let array_hook = match ab::getattr_opt(ctx, &pystr::intern_str("array_hook"))? {
                Some(v) => v,
                None => none::none_ref(),
            };
            let parse_float = ab::getattr_str(ctx, "parse_float")?;
            let parse_int = ab::getattr_str(ctx, "parse_int")?;
            let parse_constant = ab::getattr_str(ctx, "parse_constant")?;
            let op = generic_alloc(tp, 0);
            if op.is_null() {
                return Err(PyErr);
            }
            let so = &mut *(op as *mut ScannerObj);
            so.strict = strict;
            so.object_hook = object_hook.into_raw();
            so.object_pairs_hook = object_pairs_hook.into_raw();
            so.array_hook = array_hook.into_raw();
            so.parse_float = parse_float.into_raw();
            so.parse_int = parse_int.into_raw();
            so.parse_constant = parse_constant.into_raw();
            so.memo = dict::new_dict().into_raw();
            Ok(PyRef::from_raw(op))
        })();
        into_c(r)
    }
}

unsafe fn xdecref(p: *mut PyObject) {
    if !p.is_null() {
        unsafe { crate::object::decref(p) };
    }
}

unsafe extern "C" fn scanner_dealloc(o: *mut PyObject) {
    unsafe {
        let tp = (*o).type_ptr();
        let so = &mut *(o as *mut ScannerObj);
        for p in [so.object_hook, so.object_pairs_hook, so.array_hook, so.parse_float, so.parse_int, so.parse_constant, so.memo] {
            xdecref(p);
        }
        generic_free_and_type_decref(o, tp);
    }
}

unsafe extern "C" fn scanner_call(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let (s, idx): (PyRef, PyRef) = if kw.is_null() && tuple::tuple_len(&*args) == 2 {
                (tuple::tuple_get(&*args, 0).to_ref(), tuple::tuple_get(&*args, 1).to_ref())
            } else {
                let (pos, kwn) = crate::args::kwdict_to_vectorcall(&*args, kw);
                let refs: Vec<&PyObject> = pos.iter().map(|x| &**x).collect();
                let p = crate::args::Params::new("scan_once", &["string", "idx"], 0, 2);
                let [s, idx] = crate::args::parse::<2>(&p, &refs, kwn.as_deref())?;
                (s.unwrap().to_ref(), idx.unwrap().to_ref())
            };
            let s: &PyObject = &s;
            if !pystr::is_str(s) {
                raise!(TypeError, "first argument must be a string, not {}", tname(s));
            }
            let idx = index_arg(&idx)?;
            if idx < 0 {
                raise!(ValueError, "idx cannot be negative");
            }
            let so = &*(o as *const ScannerObj);
            let sc = Scanner { sc: so, s, d: pystr::str_data(s), memo: Default::default() };
            let r = sc.scan_once(idx as usize);
            drop(sc);
            match r? {
                Some((v, e)) => Ok(tuple::tuple2(v, int::new_int(e as i64))),
                None => Err(stop_iteration(idx)),
            }
        })();
        into_c(r)
    }
}

// ------------------------------------------------------------------ encoder type

#[repr(C)]
struct EncoderObj {
    ob_base: PyObject,
    markers: *mut PyObject,
    default: *mut PyObject,
    encoder: *mut PyObject,
    indent: *mut PyObject,
    key_separator: *mut PyObject,
    item_separator: *mut PyObject,
    sort_keys: bool,
    skipkeys: bool,
    allow_nan: bool,
    /// 0: call `encoder`; 1: built-in ascii escaping; 2: built-in unicode escaping
    fast_encode: u8,
}

static mut ENCODE_ASCII_FN: *mut PyObject = std::ptr::null_mut();
static mut ENCODE_UNICODE_FN: *mut PyObject = std::ptr::null_mut();

unsafe extern "C" fn encoder_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let a: [Option<&PyObject>; 9] = if kw.is_null() && tuple::tuple_len(&*args) == 9 {
                std::array::from_fn(|i| Some(tuple::tuple_get(&*args, i)))
            } else {
                let (pos, kwn) = crate::args::kwdict_to_vectorcall(&*args, kw);
                let refs: Vec<&PyObject> = pos.iter().map(|x| &**x).collect();
                let p = crate::args::Params::new(
                    "make_encoder",
                    &["markers", "default", "encoder", "indent", "key_separator", "item_separator", "sort_keys", "skipkeys", "allow_nan"],
                    0,
                    9,
                );
                let a = crate::args::parse::<9>(&p, &refs, kwn.as_deref())?;
                // the parsed references point into `pos`, which lives until the end of
                // this call; the caller's tuple/dict keep the objects alive as well
                a.map(|x| x.map(|r| &*(r as *const PyObject)))
            };
            let markers = a[0].unwrap();
            if !markers.is(none::none()) && !dict::is_dict(markers) {
                raise!(TypeError, "make_encoder() argument 1 must be dict or None, not {}", tname(markers));
            }
            let indent = a[3].unwrap();
            if !indent.is(none::none()) && !pystr::is_str(indent) {
                raise!(TypeError, "make_encoder() argument 4 must be str or None, not {}", tname(indent));
            }
            let ksep = a[4].unwrap();
            if !pystr::is_str(ksep) {
                raise!(TypeError, "make_encoder() argument 5 must be str, not {}", tname(ksep));
            }
            let isep = a[5].unwrap();
            if !pystr::is_str(isep) {
                raise!(TypeError, "make_encoder() argument 6 must be str, not {}", tname(isep));
            }
            let sort_keys = ab::is_true(a[6].unwrap())?;
            let skipkeys = ab::is_true(a[7].unwrap())?;
            let allow_nan = ab::is_true(a[8].unwrap())?;
            let enc = a[2].unwrap();
            let fast = if !ENCODE_ASCII_FN.is_null() && enc.is(&*ENCODE_ASCII_FN) {
                1
            } else if !ENCODE_UNICODE_FN.is_null() && enc.is(&*ENCODE_UNICODE_FN) {
                2
            } else {
                0
            };
            let op = generic_alloc(tp, 0);
            if op.is_null() {
                return Err(PyErr);
            }
            let eo = &mut *(op as *mut EncoderObj);
            eo.markers = markers.to_ref().into_raw();
            eo.default = a[1].unwrap().to_ref().into_raw();
            eo.encoder = enc.to_ref().into_raw();
            eo.indent = indent.to_ref().into_raw();
            eo.key_separator = ksep.to_ref().into_raw();
            eo.item_separator = isep.to_ref().into_raw();
            eo.sort_keys = sort_keys;
            eo.skipkeys = skipkeys;
            eo.allow_nan = allow_nan;
            eo.fast_encode = fast;
            Ok(PyRef::from_raw(op))
        })();
        into_c(r)
    }
}

unsafe extern "C" fn encoder_dealloc(o: *mut PyObject) {
    unsafe {
        let tp = (*o).type_ptr();
        let eo = &mut *(o as *mut EncoderObj);
        for p in [eo.markers, eo.default, eo.encoder, eo.indent, eo.key_separator, eo.item_separator] {
            xdecref(p);
        }
        generic_free_and_type_decref(o, tp);
    }
}

struct Enc<'a> {
    e: &'a EncoderObj,
    out: StrBuilder,
    /// Circular-reference bookkeeping kept here instead of in the `markers` dict
    /// (used when that dict is empty at the start of the call: the dict then stays
    /// empty, exactly as it would be after a successful encoding).
    private_markers: bool,
    active: Vec<usize>,
    use_set: bool,
    active_set: std::collections::HashSet<usize>,
}

/// How a container was registered for the circular-reference check.
enum Mark {
    None,
    Private,
    Dict(PyRef),
}

fn is_frozendict(o: &PyObject) -> bool {
    match crate::builtins::builtin("frozendict") {
        Ok(fd) => {
            if crate::types::typeobject::is_type(&fd) {
                is_subtype(o.type_ptr(), as_type(&fd))
            } else {
                false
            }
        }
        Err(_) => {
            clear_exc();
            false
        }
    }
}

impl<'a> Enc<'a> {
    fn markers(&self) -> Option<&PyObject> {
        opt(self.e.markers)
    }

    fn encode_str(&mut self, s: &PyObject) -> PyResult<()> {
        match self.e.fast_encode {
            1 => escape_into(&mut self.out, s, true),
            2 => escape_into(&mut self.out, s, false),
            _ => {
                let r = ab::call(unsafe { &*self.e.encoder }, &[s])?;
                if !pystr::is_str(&r) {
                    raise!(TypeError, "encoder() must return a string, not {}", tname(&r));
                }
                self.out.push_obj(&r);
            }
        }
        Ok(())
    }

    fn float_str(&self, o: &PyObject) -> PyResult<PyRef> {
        let v = float::float_value(o);
        if v.is_finite() {
            return ab::repr(&float::new_float(v));
        }
        if !self.e.allow_nan {
            let r = ab::repr(o)?;
            raise!(ValueError, "Out of range float values are not JSON compliant: {}", pystr::to_string_lossy(&r));
        }
        Ok(pystr::new_str(if v.is_nan() {
            "NaN"
        } else if v > 0.0 {
            "Infinity"
        } else {
            "-Infinity"
        }))
    }

    fn int_str(&self, o: &PyObject) -> PyResult<PyRef> {
        ab::repr(&crate::types::int_methods::exact_copy(o))
    }

    /// Append the decimal text of an int (machine-word values without a str object).
    fn push_int(&mut self, o: &PyObject) -> PyResult<()> {
        if let Some(v) = int::as_i64(o) {
            let mut buf = [0u8; 20];
            let mut i = buf.len();
            let mut m = v.unsigned_abs();
            loop {
                i -= 1;
                buf[i] = b'0' + (m % 10) as u8;
                m /= 10;
                if m == 0 {
                    break;
                }
            }
            if v < 0 {
                self.out.push_ascii(b"-");
            }
            self.out.push_ascii(&buf[i..]);
            return Ok(());
        }
        let s = self.int_str(o)?;
        self.out.push_obj(&s);
        Ok(())
    }

    /// Append the text of a float (repr for finite values).
    fn push_float(&mut self, o: &PyObject) -> PyResult<()> {
        let v = float::float_value(o);
        if v.is_finite() {
            self.out.push_ascii(rpy_numfmt::float_repr(v).as_bytes());
            return Ok(());
        }
        let s = self.float_str(o)?;
        self.out.push_obj(&s);
        Ok(())
    }

    fn newline_indent(&mut self, level: usize) {
        self.out.push(b'\n' as u32);
        let ind = unsafe { &*self.e.indent };
        for _ in 0..level {
            self.out.push_obj(ind);
        }
    }

    fn has_indent(&self) -> bool {
        !unsafe { &*self.e.indent }.is(none::none())
    }

    fn encode_obj(&mut self, o: &PyObject, level: usize) -> PyResult<()> {
        if o.is(none::none()) {
            self.out.push_ascii(b"null");
        } else if o.is(bool_::true_obj()) {
            self.out.push_ascii(b"true");
        } else if o.is(bool_::false_obj()) {
            self.out.push_ascii(b"false");
        } else if pystr::is_str(o) {
            self.encode_str(o)?;
        } else if int::is_int_exact(o) {
            self.push_int(o)?;
        } else if int::is_int(o) {
            let s = self.int_str(o)?;
            self.out.push_obj(&s);
        } else if float::is_float(o) {
            self.push_float(o)?;
        } else if list::is_list(o) || tuple::is_tuple(o) {
            let t = ts();
            crate::interp::enter_recursive_call(t, " while encoding a JSON object")?;
            let r = self.encode_list(o, level);
            crate::interp::leave_recursive_call(t);
            r?;
        } else if dict::is_dict(o) || is_frozendict(o) {
            let t = ts();
            crate::interp::enter_recursive_call(t, " while encoding a JSON object")?;
            let r = self.encode_dict(o, level);
            crate::interp::leave_recursive_call(t);
            r?;
        } else {
            let ident = self.mark(o)?;
            let newobj = ab::call(unsafe { &*self.e.default }, &[o])?;
            let t = ts();
            crate::interp::enter_recursive_call(t, " while encoding a JSON object")?;
            let r = self.encode_obj(&newobj, level);
            crate::interp::leave_recursive_call(t);
            if r.is_err() {
                return Err(add_note(format!("when serializing {} object", tname(o))));
            }
            self.unmark(ident)?;
        }
        Ok(())
    }

    fn mark(&mut self, o: &PyObject) -> PyResult<Mark> {
        if self.private_markers {
            // containers being encoded, innermost last; a linear scan beats hashing
            // for the usual shallow nesting, a set takes over for deep documents
            let p = o.as_ptr() as usize;
            if !self.use_set && self.active.len() >= 32 {
                self.use_set = true;
                self.active_set.extend(self.active.iter().copied());
            }
            let seen = if self.use_set { !self.active_set.insert(p) } else { self.active.contains(&p) };
            if seen {
                raise!(ValueError, "Circular reference detected");
            }
            self.active.push(p);
            return Ok(Mark::Private);
        }
        if let Some(m) = self.markers() {
            let ident = int::new_int_usize(o.as_ptr() as usize);
            if dict::contains(m, &ident)? {
                raise!(ValueError, "Circular reference detected");
            }
            dict::set_item(m, &ident, o)?;
            return Ok(Mark::Dict(ident));
        }
        Ok(Mark::None)
    }

    fn unmark(&mut self, m: Mark) -> PyResult<()> {
        match m {
            Mark::None => {}
            Mark::Private => {
                if let Some(p) = self.active.pop() {
                    if self.use_set {
                        self.active_set.remove(&p);
                    }
                }
            }
            Mark::Dict(i) => {
                if let Some(m) = self.markers() {
                    dict::del_item(m, &i)?;
                }
            }
        }
        Ok(())
    }

    fn encode_list(&mut self, seq: &PyObject, level: usize) -> PyResult<()> {
        let is_list = list::is_list(seq);
        let n = if is_list { list::list_len(seq) } else { tuple::tuple_len(seq) };
        if n == 0 {
            self.out.push_str("[]");
            return Ok(());
        }
        let ident = self.mark(seq)?;
        self.out.push(b'[' as u32);
        let indent = self.has_indent();
        let level = level + 1;
        if indent {
            self.newline_indent(level);
        }
        let mut i = 0;
        loop {
            let item = if is_list {
                match list::get(seq, i) {
                    Some(v) => v,
                    None => break,
                }
            } else {
                if i >= n {
                    break;
                }
                tuple::tuple_get(seq, i).to_ref()
            };
            if i > 0 {
                self.out.push_obj(unsafe { &*self.e.item_separator });
                if indent {
                    self.newline_indent(level);
                }
            }
            if self.encode_obj(&item, level).is_err() {
                return Err(add_note(format!("when serializing {} item {}", tname(seq), i)));
            }
            i += 1;
        }
        if indent {
            self.newline_indent(level - 1);
        }
        self.out.push(b']' as u32);
        self.unmark(ident)
    }

    fn encode_dict(&mut self, d: &PyObject, level: usize) -> PyResult<()> {
        if ab::length(d)? == 0 {
            self.out.push_ascii(b"{}");
            return Ok(());
        }
        let ident = self.mark(d)?;
        self.out.push_ascii(b"{");
        let indent = self.has_indent();
        let level = level + 1;
        let mut first = true;
        if dict::is_dict_exact(d) && !self.e.sort_keys {
            // a snapshot of the entries: no item tuples or list needed
            for (key, value) in dict::items_vec(d) {
                self.encode_member(d, &key, &value, &mut first, indent, level)?;
            }
        } else {
            let items: PyRef = if dict::is_dict_exact(d) {
                let v: Vec<PyRef> = dict::items_vec(d).into_iter().map(|(k, v)| tuple::tuple2(k, v)).collect();
                list::new_list(v)
            } else {
                let it = ab::call_method(d, name!("items"), &[])?;
                if list::is_list_exact(&it) { it } else { list::list_from_iter(&it)? }
            };
            if self.e.sort_keys {
                ab::call_method(&items, name!("sort"), &[])?;
            }
            let mut i = 0;
            while let Some(item) = list::get(&items, i) {
                i += 1;
                if !tuple::is_tuple(&item) || tuple::tuple_len(&item) != 2 {
                    raise!(ValueError, "items must return 2-tuples");
                }
                let key = tuple::tuple_get(&item, 0).to_ref();
                let value = tuple::tuple_get(&item, 1).to_ref();
                self.encode_member(d, &key, &value, &mut first, indent, level)?;
            }
        }
        if !first && indent {
            self.newline_indent(level - 1);
        }
        self.out.push_ascii(b"}");
        self.unmark(ident)
    }

    /// One `key: value` member of an object (a skipped key writes nothing).
    fn encode_member(
        &mut self,
        d: &PyObject,
        key: &PyObject,
        value: &PyObject,
        first: &mut bool,
        indent: bool,
        level: usize,
    ) -> PyResult<()> {
        let kstr: PyRef = if pystr::is_str(key) {
            key.to_ref()
        } else if float::is_float(key) {
            self.float_str(key)?
        } else if key.is(bool_::true_obj()) {
            pystr::new_str("true")
        } else if key.is(bool_::false_obj()) {
            pystr::new_str("false")
        } else if key.is(none::none()) {
            pystr::new_str("null")
        } else if int::is_int(key) {
            self.int_str(key)?
        } else if self.e.skipkeys {
            return Ok(());
        } else {
            raise!(TypeError, "keys must be str, int, float, bool or None, not {}", tname(key));
        };
        if *first {
            *first = false;
        } else {
            self.out.push_obj(unsafe { &*self.e.item_separator });
        }
        if indent {
            self.newline_indent(level);
        }
        self.encode_str(&kstr)?;
        self.out.push_obj(unsafe { &*self.e.key_separator });
        if self.encode_obj(value, level).is_err() {
            let kr = ab::repr_string(&kstr).unwrap_or_default();
            return Err(add_note(format!("when serializing {} item {}", tname(d), kr)));
        }
        Ok(())
    }
}

unsafe extern "C" fn encoder_call(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let (obj, lvl): (PyRef, PyRef) = if kw.is_null() && tuple::tuple_len(&*args) == 2 {
                (tuple::tuple_get(&*args, 0).to_ref(), tuple::tuple_get(&*args, 1).to_ref())
            } else {
                let (pos, kwn) = crate::args::kwdict_to_vectorcall(&*args, kw);
                let refs: Vec<&PyObject> = pos.iter().map(|x| &**x).collect();
                let p = crate::args::Params::new("_iterencode", &["obj", "_current_indent_level"], 0, 2);
                let [obj, lvl] = crate::args::parse::<2>(&p, &refs, kwn.as_deref())?;
                (obj.unwrap().to_ref(), lvl.unwrap().to_ref())
            };
            if !int::is_int(&lvl) {
                raise!(TypeError, "'{}' object cannot be interpreted as an integer", tname(&lvl));
            }
            let lvl = index_arg(&lvl)?.max(0) as usize;
            let eo = &*(o as *const EncoderObj);
            let private_markers = opt(eo.markers).is_some_and(|m| dict::is_dict_exact(m) && dict::dict_len(m) == 0);
            let mut enc = Enc {
                e: eo,
                out: StrBuilder::with_capacity(64),
                private_markers,
                active: Vec::new(),
                use_set: false,
                active_set: Default::default(),
            };
            enc.encode_obj(&obj, lvl)?;
            Ok(tuple::new_tuple(vec![enc.out.finish()]))
        })();
        into_c(r)
    }
}

// ------------------------------------------------------------------ module

static INIT: std::sync::Once = std::sync::Once::new();

pub fn init_module() -> PyResult {
    let mut res = Ok(());
    INIT.call_once(|| unsafe {
        let t = &mut *scanner_type();
        t.tp_name = c"_json.Scanner".as_ptr();
        t.tp_basicsize = std::mem::size_of::<ScannerObj>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT;
        t.tp_new = Some(scanner_new);
        t.tp_dealloc = Some(scanner_dealloc);
        t.tp_call = Some(scanner_call);
        t.tp_alloc = Some(generic_alloc);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_doc = c"Callable decoding one JSON value at a given index, configured by a JSONDecoder.".as_ptr();
        if let Err(e) = type_ready(scanner_type()) {
            res = Err(e);
            return;
        }
        let t = &mut *encoder_type();
        t.tp_name = c"_json.Encoder".as_ptr();
        t.tp_basicsize = std::mem::size_of::<EncoderObj>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT;
        t.tp_new = Some(encoder_new);
        t.tp_dealloc = Some(encoder_dealloc);
        t.tp_call = Some(encoder_call);
        t.tp_alloc = Some(generic_alloc);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_doc = c"Encoder(markers, default, encoder, indent, key_separator, item_separator, sort_keys, skipkeys, allow_nan)".as_ptr();
        res = type_ready(encoder_type());
    });
    res?;
    let m = crate::types::module::new_module("_json")?;
    let d = crate::types::module::module_dict(&m);
    dict::set_item_str(&d, "__doc__", &pystr::new_str("Native accelerator behind the json package.\n"))?;
    crate::builtins::add_methods_to_dict(
        &d,
        mdefs![
            ("scanstring", FASTKW, m_scanstring, "scanstring(string, end, strict=True)\n--\n\nDecode the JSON string literal whose opening quote is at string[end - 1].\nReturns (text, index after the closing quote); strict=False allows raw\ncontrol characters inside the literal."),
            ("encode_basestring_ascii", O, m_encode_basestring_ascii, "encode_basestring_ascii(s, /)\n--\n\nJSON string literal for s using only ASCII characters."),
            ("encode_basestring", O, m_encode_basestring, "encode_basestring(s, /)\n--\n\nJSON string literal for s (non-ASCII characters kept)."),
        ],
        Some(&m),
    )?;
    unsafe {
        if ENCODE_ASCII_FN.is_null() {
            if let Some(f) = dict::get_item_rs(&d, "encode_basestring_ascii") {
                ENCODE_ASCII_FN = f.into_raw();
            }
            if let Some(f) = dict::get_item_rs(&d, "encode_basestring") {
                ENCODE_UNICODE_FN = f.into_raw();
            }
        } else {
            // re-import (e.g. import_fresh_module): keep the identity of the
            // fast-path functions stable.
            dict::set_item_str(&d, "encode_basestring_ascii", &*ENCODE_ASCII_FN)?;
            dict::set_item_str(&d, "encode_basestring", &*ENCODE_UNICODE_FN)?;
        }
    }
    dict::set_item_str(&d, "make_scanner", &type_ref(scanner_type()))?;
    dict::set_item_str(&d, "make_encoder", &type_ref(encoder_type()))?;
    Ok(m)
}
