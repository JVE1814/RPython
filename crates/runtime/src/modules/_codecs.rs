//! `_codecs`: the codec registry, the error handler registry and all built-in
//! codec functions (UTF-8/16/32/7, Latin-1, ASCII, charmap, unicode-escape,
//! raw-unicode-escape, escape_encode/decode, readbuffer_encode).
//!
//! The same Rust functions back `str.encode` / `bytes.decode` (see
//! `codecs_core.rs`).

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::text_util::{self as tu, ByteData, StrBuf};
use crate::object::*;
use crate::types::exceptions::{self as exc, UnicodeErrorExc};
use crate::types::str::{self as pystr, StrData};
use crate::types::typeobject::{is_subtype, tname};
use crate::types::{bool_, bytes, dict, int, list, none, tuple};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Mutex;

// =====================================================================================
// Registries
// =====================================================================================

struct Registry {
    search: Vec<PyRef>,
    cache: HashMap<String, PyRef>,
    errors: HashMap<String, PyRef>,
    initialized: bool,
}

static REGISTRY: std::sync::LazyLock<Mutex<Registry>> = std::sync::LazyLock::new(||
    Mutex::new(Registry { search: Vec::new(), cache: HashMap::new(), errors: HashMap::new(), initialized: false }));

const BUILTIN_HANDLERS: [&str; 8] =
    ["strict", "ignore", "replace", "xmlcharrefreplace", "backslashreplace", "namereplace", "surrogateescape", "surrogatepass"];

fn ensure_error_handlers() -> PyResult<()> {
    {
        let r = REGISTRY.lock().unwrap();
        if r.initialized {
            return Ok(());
        }
    }
    let table = crate::mdefs![
        ("strict_errors", O, py_strict_errors, "Implement the 'strict' error handling, which raises a UnicodeError on coding errors."),
        ("ignore_errors", O, py_ignore_errors, "Implement the 'ignore' error handling, which ignores malformed data and continues."),
        ("replace_errors", O, py_replace_errors, "Implement the 'replace' error handling, which replaces malformed data with a replacement marker."),
        ("xmlcharrefreplace_errors", O, py_xmlcharrefreplace_errors, "Implement the 'xmlcharrefreplace' error handling, which replaces an unencodable character with the appropriate XML character reference."),
        ("backslashreplace_errors", O, py_backslashreplace_errors, "Implement the 'backslashreplace' error handling, which replaces malformed data with a backslashed escape sequence."),
        ("namereplace_errors", O, py_namereplace_errors, "Implement the 'namereplace' error handling, which replaces an unencodable character with a \\N{...} escape sequence."),
        ("surrogateescape", O, py_surrogateescape_errors),
        ("surrogatepass", O, py_surrogatepass_errors),
    ];
    let module = crate::import::get_loaded_module("_codecs");
    let modname = pystr::new_str("_codecs");
    let mut fns = Vec::new();
    unsafe {
        let mut m = table;
        let mut i = 0;
        while !(*m).ml_name.is_null() {
            let f = crate::types::cfunction::new_cfunction(m, module.clone(), Some(modname.clone()));
            fns.push((BUILTIN_HANDLERS[i], f));
            m = m.add(1);
            i += 1;
        }
    }
    let mut r = REGISTRY.lock().unwrap();
    if !r.initialized {
        for (n, f) in fns {
            r.errors.insert(n.to_string(), f);
        }
        r.initialized = true;
    }
    Ok(())
}

/// `codecs.lookup_error(name)`.
pub fn lookup_error(name: &str) -> PyResult {
    ensure_error_handlers()?;
    let r = REGISTRY.lock().unwrap();
    match r.errors.get(name) {
        Some(h) => Ok(h.clone()),
        None => {
            drop(r);
            raise!(LookupError, "unknown error handler name '{}'", tu::trunc_chars(name, 400))
        }
    }
}

/// The C-level `_Py_normalize_encoding`: lower-case ASCII letters, digits and
/// dots are kept; every run of other characters becomes one underscore
/// (leading and trailing runs are dropped).
pub fn normalize_encoding(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut punct = false;
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b == b'.' {
            if punct && !out.is_empty() {
                out.push('_');
            }
            punct = false;
            out.push(b.to_ascii_lowercase() as char);
        } else {
            punct = true;
        }
    }
    out
}

/// Registry key of an encoding name: ASCII letters lower-cased and spaces
/// replaced by hyphens (non-ASCII names are deprecated).
pub fn registry_key(name: &str) -> PyResult<String> {
    if !name.is_ascii() {
        tu::warn(exc::DeprecationWarning(), "Support for non-ascii encoding names will be removed in 3.17", 1)?;
    }
    Ok(name.chars().map(|c| if c == ' ' { '-' } else { c.to_ascii_lowercase() }).collect())
}

fn ensure_encodings_imported() -> PyResult<()> {
    let empty = REGISTRY.lock().unwrap().search.is_empty();
    if empty && crate::import::get_loaded_module("encodings").is_none() {
        crate::import::import_absolute("encodings")?;
    }
    Ok(())
}

/// `codecs.lookup(encoding)`.
pub fn lookup(encoding: &str) -> PyResult {
    if encoding.contains('\0') {
        raise!(ValueError, "embedded null character");
    }
    ensure_encodings_imported()?;
    let key = registry_key(encoding)?;
    let (funcs, cached) = {
        let r = REGISTRY.lock().unwrap();
        (r.search.clone(), r.cache.get(&key).cloned())
    };
    if let Some(c) = cached {
        return Ok(c);
    }
    let k = pystr::new_str(&key);
    for f in funcs {
        let res = ab::call(&f, &[&k])?;
        if none::is_none(&res) {
            continue;
        }
        if !tuple::is_tuple(&res) || tuple::tuple_len(&res) != 4 {
            raise!(TypeError, "codec search functions must return 4-tuples");
        }
        REGISTRY.lock().unwrap().cache.insert(key, res.clone());
        return Ok(res);
    }
    raise!(LookupError, "unknown encoding: {}", encoding)
}

/// `codecs.lookup(name)` for a str object.  A name with lone surrogates has
/// no UTF-8 form, so no codec can be registered under it: after the
/// non-ASCII deprecation warning it is an unknown encoding.
pub fn lookup_obj(name: &PyObject) -> PyResult {
    if let Some(s) = pystr::to_string_strict(name) {
        return lookup(&s);
    }
    tu::warn(exc::DeprecationWarning(), "Support for non-ascii encoding names will be removed in 3.17", 1)?;
    raise!(LookupError, "unknown encoding: {}", pystr::to_string_lossy(name))
}

// =====================================================================================
// Error handler kinds and the built-in handler logic
// =====================================================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Eh {
    Strict,
    Ignore,
    Replace,
    BackslashReplace,
    XmlCharRefReplace,
    NameReplace,
    SurrogateEscape,
    SurrogatePass,
    Other,
}

pub fn eh_of(errors: &str) -> Eh {
    match errors {
        "strict" => Eh::Strict,
        "ignore" => Eh::Ignore,
        "replace" => Eh::Replace,
        "backslashreplace" => Eh::BackslashReplace,
        "xmlcharrefreplace" => Eh::XmlCharRefReplace,
        "namereplace" => Eh::NameReplace,
        "surrogateescape" => Eh::SurrogateEscape,
        "surrogatepass" => Eh::SurrogatePass,
        _ => Eh::Other,
    }
}

/// Replacement returned by an encoding error handler.
pub enum Rep {
    Str(Vec<u32>),
    Bytes(Vec<u8>),
}

enum HFail {
    Raise,
    Type,
}

#[inline]
fn is_surrogate(c: u32) -> bool {
    (0xD800..0xE000).contains(&c)
}

fn push_ascii(v: &mut Vec<u32>, s: &str) {
    v.extend(s.bytes().map(|b| b as u32));
}

fn backslash_escape_char(v: &mut Vec<u32>, c: u32) {
    if c < 0x100 {
        push_ascii(v, &format!("\\x{:02x}", c));
    } else if c < 0x10000 {
        push_ascii(v, &format!("\\u{:04x}", c));
    } else {
        push_ascii(v, &format!("\\U{:08x}", c));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StdEnc {
    Utf8,
    Utf16Le,
    Utf16Be,
    Utf32Le,
    Utf32Be,
}

/// Standard UTF encoding of an encoding name (for "surrogatepass").
fn standard_encoding(name: &str) -> Option<StdEnc> {
    let l = name.to_ascii_lowercase();
    let rest = l.strip_prefix("utf")?;
    let rest = rest.strip_prefix(['-', '_']).unwrap_or(rest);
    if rest == "8" {
        return Some(StdEnc::Utf8);
    }
    let (w, tail) = if let Some(t) = rest.strip_prefix("16") {
        (16, t)
    } else if let Some(t) = rest.strip_prefix("32") {
        (32, t)
    } else {
        return None;
    };
    let le = match tail {
        "" => cfg!(target_endian = "little"),
        "le" | "-le" | "_le" => true,
        "be" | "-be" | "_be" => false,
        _ => return None,
    };
    Some(match (w, le) {
        (16, true) => StdEnc::Utf16Le,
        (16, false) => StdEnc::Utf16Be,
        (_, true) => StdEnc::Utf32Le,
        (_, false) => StdEnc::Utf32Be,
    })
}

fn std_enc_len(e: StdEnc) -> usize {
    match e {
        StdEnc::Utf8 => 3,
        StdEnc::Utf16Le | StdEnc::Utf16Be => 2,
        _ => 4,
    }
}

fn h_encode(kind: Eh, encoding: &str, d: StrData, start: usize, end: usize) -> Result<(Rep, usize), HFail> {
    let end = end.min(d.len());
    let start = start.min(end);
    match kind {
        Eh::Strict | Eh::Other => Err(HFail::Raise),
        Eh::Ignore => Ok((Rep::Str(Vec::new()), end)),
        Eh::Replace => Ok((Rep::Str(vec!['?' as u32; end - start]), end)),
        Eh::BackslashReplace => {
            let mut v = Vec::new();
            for i in start..end {
                backslash_escape_char(&mut v, d.get(i));
            }
            Ok((Rep::Str(v), end))
        }
        Eh::XmlCharRefReplace => {
            let mut v = Vec::new();
            for i in start..end {
                push_ascii(&mut v, &format!("&#{};", d.get(i)));
            }
            Ok((Rep::Str(v), end))
        }
        Eh::NameReplace => {
            tu::ensure_unicodedata();
            let mut v = Vec::new();
            for i in start..end {
                let c = d.get(i);
                match rpy_unicode::name(c) {
                    Some(n) => push_ascii(&mut v, &format!("\\N{{{}}}", n)),
                    None => backslash_escape_char(&mut v, c),
                }
            }
            Ok((Rep::Str(v), end))
        }
        Eh::SurrogateEscape => {
            let mut b = Vec::new();
            for i in start..end {
                let c = d.get(i);
                if !(0xDC80..=0xDCFF).contains(&c) {
                    return Err(HFail::Raise);
                }
                b.push((c - 0xDC00) as u8);
            }
            Ok((Rep::Bytes(b), end))
        }
        Eh::SurrogatePass => {
            let enc = standard_encoding(encoding).ok_or(HFail::Raise)?;
            let mut b = Vec::new();
            for i in start..end {
                let c = d.get(i);
                if !is_surrogate(c) {
                    return Err(HFail::Raise);
                }
                match enc {
                    StdEnc::Utf8 => {
                        b.push((0xE0 | (c >> 12)) as u8);
                        b.push((0x80 | ((c >> 6) & 0x3F)) as u8);
                        b.push((0x80 | (c & 0x3F)) as u8);
                    }
                    StdEnc::Utf16Le => b.extend_from_slice(&(c as u16).to_le_bytes()),
                    StdEnc::Utf16Be => b.extend_from_slice(&(c as u16).to_be_bytes()),
                    StdEnc::Utf32Le => b.extend_from_slice(&c.to_le_bytes()),
                    StdEnc::Utf32Be => b.extend_from_slice(&c.to_be_bytes()),
                }
            }
            Ok((Rep::Bytes(b), end))
        }
    }
}

fn h_decode(kind: Eh, encoding: &str, data: &[u8], start: usize, end: usize) -> Result<(Vec<u32>, usize), HFail> {
    let end = end.min(data.len());
    let start = start.min(end);
    match kind {
        Eh::Strict | Eh::Other => Err(HFail::Raise),
        Eh::Ignore => Ok((Vec::new(), end)),
        Eh::Replace => Ok((vec![0xFFFD], end)),
        Eh::BackslashReplace => {
            let mut v = Vec::new();
            for &b in &data[start..end] {
                push_ascii(&mut v, &format!("\\x{:02x}", b));
            }
            Ok((v, end))
        }
        Eh::XmlCharRefReplace | Eh::NameReplace => Err(HFail::Type),
        Eh::SurrogateEscape => {
            let mut v = Vec::new();
            let mut k = 0;
            while k < 4 && start + k < end {
                let b = data[start + k];
                if b < 128 {
                    break;
                }
                v.push(0xDC00 + b as u32);
                k += 1;
            }
            if k == 0 {
                return Err(HFail::Raise);
            }
            Ok((v, start + k))
        }
        Eh::SurrogatePass => {
            let enc = standard_encoding(encoding).ok_or(HFail::Raise)?;
            let n = std_enc_len(enc);
            if start + n > data.len() {
                return Err(HFail::Raise);
            }
            let p = &data[start..start + n];
            let ch = match enc {
                StdEnc::Utf8 => {
                    if (p[0] & 0xF0) == 0xE0 && (p[1] & 0xC0) == 0x80 && (p[2] & 0xC0) == 0x80 {
                        ((p[0] as u32 & 0x0F) << 12) | ((p[1] as u32 & 0x3F) << 6) | (p[2] as u32 & 0x3F)
                    } else {
                        0
                    }
                }
                StdEnc::Utf16Le => u16::from_le_bytes([p[0], p[1]]) as u32,
                StdEnc::Utf16Be => u16::from_be_bytes([p[0], p[1]]) as u32,
                StdEnc::Utf32Le => u32::from_le_bytes([p[0], p[1], p[2], p[3]]),
                StdEnc::Utf32Be => u32::from_be_bytes([p[0], p[1], p[2], p[3]]),
            };
            if !is_surrogate(ch) {
                return Err(HFail::Raise);
            }
            Ok((vec![ch], start + n))
        }
    }
}

fn h_translate(kind: Eh, d: StrData, start: usize, end: usize) -> Result<(Vec<u32>, usize), HFail> {
    let end = end.min(d.len());
    let start = start.min(end);
    match kind {
        Eh::Strict | Eh::Other => Err(HFail::Raise),
        Eh::Ignore => Ok((Vec::new(), end)),
        Eh::Replace => Ok((vec![0xFFFD; end - start], end)),
        Eh::BackslashReplace => {
            let mut v = Vec::new();
            for i in start..end {
                backslash_escape_char(&mut v, d.get(i));
            }
            Ok((v, end))
        }
        _ => Err(HFail::Type),
    }
}

// ------------------------------------------------------------- Python-visible handlers

#[derive(Clone, Copy, PartialEq, Eq)]
enum ErrKind {
    Encode,
    Decode,
    Translate,
}

fn err_kind(e: &PyObject) -> Option<ErrKind> {
    let t = e.type_ptr();
    if is_subtype(t, exc::UnicodeEncodeError()) {
        Some(ErrKind::Encode)
    } else if is_subtype(t, exc::UnicodeDecodeError()) {
        Some(ErrKind::Decode)
    } else if is_subtype(t, exc::UnicodeTranslateError()) {
        Some(ErrKind::Translate)
    } else {
        None
    }
}

fn wrong_exc(e: &PyObject) -> PyErr {
    err!(TypeError, "don't know how to handle {} in error callback", tu::trunc_chars(&tname(e), 200))
}

/// `exc.start` / `exc.end` clamped to the object like the C API getters do:
/// start into [0, len-1], end into [min(1, len), len].
fn exc_range(e: &PyObject, len: usize) -> PyResult<(usize, usize)> {
    let len = len as isize;
    let s = ab::index_as_ssize(&*ab::getattr_str(e, "start")?, None)?;
    let en = ab::index_as_ssize(&*ab::getattr_str(e, "end")?, None)?;
    let s = if s < 0 { 0 } else if s >= len { (len - 1).max(0) } else { s };
    let en = en.max(1.min(len)).min(len);
    Ok((s as usize, en as usize))
}

fn exc_str_object(e: &PyObject) -> PyResult {
    let o = ab::getattr_str(e, "object")?;
    if !pystr::is_str(&o) {
        raise!(TypeError, "object attribute must be unicode");
    }
    Ok(o)
}

fn exc_bytes_object(e: &PyObject) -> PyResult {
    let o = ab::getattr_str(e, "object")?;
    if !bytes::is_bytes(&o) {
        raise!(TypeError, "object attribute must be bytes");
    }
    Ok(o)
}

fn exc_encoding(e: &PyObject) -> PyResult<String> {
    let o = ab::getattr_str(e, "encoding")?;
    if !pystr::is_str(&o) {
        raise!(TypeError, "encoding attribute must be unicode");
    }
    Ok(pystr::to_string_lossy(&o))
}

fn reraise(e: &PyObject) -> PyErr {
    set_exc(e.to_ref());
    PyErr
}

fn result_pair(rep: PyRef, pos: usize) -> PyRef {
    tuple::new_tuple(vec![rep, int::new_int(pos as i64)])
}

fn rep_obj(r: Rep) -> PyRef {
    match r {
        Rep::Str(v) => pystr::from_codepoints(&v),
        Rep::Bytes(b) => bytes::new_bytes(&b),
    }
}

fn run_builtin_handler(kind: Eh, e: &PyObject) -> PyResult {
    let k = match err_kind(e) {
        Some(k) => k,
        None => return Err(wrong_exc(e)),
    };
    match k {
        ErrKind::Encode => {
            if matches!(kind, Eh::Strict) {
                return Err(reraise(e));
            }
            let obj = exc_str_object(e)?;
            let (s, en) = exc_range(e, pystr::str_len(&obj))?;
            let enc = if kind == Eh::SurrogatePass { exc_encoding(e)? } else { String::new() };
            match h_encode(kind, &enc, pystr::str_data(&obj), s, en) {
                Ok((r, p)) => Ok(result_pair(rep_obj(r), p)),
                Err(HFail::Raise) => Err(reraise(e)),
                Err(HFail::Type) => Err(wrong_exc(e)),
            }
        }
        ErrKind::Decode => {
            let obj = exc_bytes_object(e)?;
            let (s, en) = exc_range(e, bytes::bytes_data(&obj).len())?;
            let enc = if kind == Eh::SurrogatePass { exc_encoding(e)? } else { String::new() };
            match h_decode(kind, &enc, bytes::bytes_data(&obj), s, en) {
                Ok((v, p)) => Ok(result_pair(pystr::from_codepoints(&v), p)),
                Err(HFail::Raise) => Err(reraise(e)),
                Err(HFail::Type) => Err(wrong_exc(e)),
            }
        }
        ErrKind::Translate => {
            let obj = exc_str_object(e)?;
            let (s, en) = exc_range(e, pystr::str_len(&obj))?;
            match h_translate(kind, pystr::str_data(&obj), s, en) {
                Ok((v, p)) => Ok(result_pair(pystr::from_codepoints(&v), p)),
                Err(HFail::Raise) => Err(reraise(e)),
                Err(HFail::Type) => Err(wrong_exc(e)),
            }
        }
    }
}

fn py_strict_errors(_m: &PyObject, e: &PyObject) -> PyResult {
    if exc::is_exception_instance(e) {
        return Err(reraise(e));
    }
    raise!(TypeError, "codec must pass exception instance")
}
fn py_ignore_errors(_m: &PyObject, e: &PyObject) -> PyResult {
    run_builtin_handler(Eh::Ignore, e)
}
fn py_replace_errors(_m: &PyObject, e: &PyObject) -> PyResult {
    run_builtin_handler(Eh::Replace, e)
}
fn py_xmlcharrefreplace_errors(_m: &PyObject, e: &PyObject) -> PyResult {
    run_builtin_handler(Eh::XmlCharRefReplace, e)
}
fn py_backslashreplace_errors(_m: &PyObject, e: &PyObject) -> PyResult {
    run_builtin_handler(Eh::BackslashReplace, e)
}
fn py_namereplace_errors(_m: &PyObject, e: &PyObject) -> PyResult {
    run_builtin_handler(Eh::NameReplace, e)
}
fn py_surrogateescape_errors(_m: &PyObject, e: &PyObject) -> PyResult {
    run_builtin_handler(Eh::SurrogateEscape, e)
}
fn py_surrogatepass_errors(_m: &PyObject, e: &PyObject) -> PyResult {
    run_builtin_handler(Eh::SurrogatePass, e)
}

// =====================================================================================
// Error contexts used by the codec loops
// =====================================================================================

fn uexc(e: &PyObject) -> &mut UnicodeErrorExc {
    unsafe { &mut *(e.as_ptr() as *mut UnicodeErrorExc) }
}

fn update_exc(e: &PyObject, start: usize, end: usize, reason: &str) {
    let u = uexc(e);
    u.start = start as isize;
    u.end = end as isize;
    let r = pystr::new_str(reason).into_raw();
    let old = std::mem::replace(&mut u.reason, r);
    unsafe { xdecref(old) };
}

fn bad_position(pos: isize) -> PyErr {
    err!(IndexError, "position {} from error handler out of bounds", pos)
}

/// Error context of an encoder working on the str `obj`.
pub struct EncCtx<'a> {
    pub encoding: &'a str,
    pub obj: &'a PyObject,
    pub errors: &'a str,
    pub eh: Eh,
    exc: Option<PyRef>,
    handler: Option<PyRef>,
}

impl<'a> EncCtx<'a> {
    pub fn new(encoding: &'a str, obj: &'a PyObject, errors: &'a str) -> Self {
        EncCtx { encoding, obj, errors, eh: eh_of(errors), exc: None, handler: None }
    }

    fn exc_obj(&mut self, start: usize, end: usize, reason: &str) -> PyResult<PyRef> {
        if let Some(e) = &self.exc {
            update_exc(e, start, end, reason);
            return Ok(e.clone());
        }
        let e = exc::make_exception(
            exc::UnicodeEncodeError(),
            &[
                pystr::new_str(self.encoding),
                self.obj.to_ref(),
                int::new_int(start as i64),
                int::new_int(end as i64),
                pystr::new_str(reason),
            ],
        )?;
        self.exc = Some(e.clone());
        Ok(e)
    }

    /// Raise UnicodeEncodeError for [start, end).
    pub fn raise(&mut self, start: usize, end: usize, reason: &str) -> PyErr {
        match self.exc_obj(start, end, reason) {
            Ok(e) => {
                set_exc(e);
                PyErr
            }
            Err(e) => e,
        }
    }

    /// Call the error handler for the characters [start, end).
    pub fn handle(&mut self, start: usize, end: usize, reason: &str) -> PyResult<(Rep, usize)> {
        if self.eh != Eh::Other {
            return match h_encode(self.eh, self.encoding, pystr::str_data(self.obj), start, end) {
                Ok(r) => Ok(r),
                Err(_) => Err(self.raise(start, end, reason)),
            };
        }
        if self.handler.is_none() {
            self.handler = Some(lookup_error(self.errors)?);
        }
        let e = self.exc_obj(start, end, reason)?;
        let h = self.handler.clone().unwrap();
        let r = ab::call(&h, &[&e])?;
        let bad = || err!(TypeError, "encoding error handler must return (str/bytes, int) tuple");
        if !tuple::is_tuple(&r) || tuple::tuple_len(&r) != 2 {
            return Err(bad());
        }
        let rep = tuple::tuple_get(&r, 0);
        let pos = tuple::tuple_get(&r, 1);
        if !int::is_int(pos) {
            return Err(bad());
        }
        let rep = if pystr::is_str(rep) {
            Rep::Str(pystr::str_data(rep).to_vec())
        } else if bytes::is_bytes(rep) {
            Rep::Bytes(bytes::bytes_data(rep).to_vec())
        } else {
            return Err(bad());
        };
        let len = pystr::str_len(self.obj) as isize;
        let mut p = ab::index_as_ssize(pos, None)?;
        if p < 0 {
            p += len;
        }
        if p < 0 || p > len {
            return Err(bad_position(p));
        }
        Ok((rep, p as usize))
    }
}

/// Error context of a decoder. The input may be replaced by the error handler
/// (through `exc.object`), so decoders re-read `ctx.data` after each error.
pub struct DecCtx<'a> {
    pub encoding: &'a str,
    pub errors: &'a str,
    pub eh: Eh,
    pub data: Cow<'a, [u8]>,
    obj: Option<PyRef>,
    exc: Option<PyRef>,
    handler: Option<PyRef>,
}

impl<'a> DecCtx<'a> {
    pub fn new(encoding: &'a str, data: &'a [u8], errors: &'a str) -> Self {
        DecCtx { encoding, errors, eh: eh_of(errors), data: Cow::Borrowed(data), obj: None, exc: None, handler: None }
    }

    fn exc_obj(&mut self, start: usize, end: usize, reason: &str) -> PyResult<PyRef> {
        if let Some(e) = &self.exc {
            update_exc(e, start, end, reason);
            return Ok(e.clone());
        }
        let b = match &self.obj {
            Some(o) => o.clone(),
            None => bytes::new_bytes(&self.data),
        };
        let e = exc::make_exception(
            exc::UnicodeDecodeError(),
            &[pystr::new_str(self.encoding), b, int::new_int(start as i64), int::new_int(end as i64), pystr::new_str(reason)],
        )?;
        let u = uexc(&e);
        if !u.object.is_null() {
            self.obj = Some(unsafe { (*u.object).to_ref() });
        }
        self.exc = Some(e.clone());
        Ok(e)
    }

    pub fn raise(&mut self, start: usize, end: usize, reason: &str) -> PyErr {
        match self.exc_obj(start, end, reason) {
            Ok(e) => {
                set_exc(e);
                PyErr
            }
            Err(e) => e,
        }
    }

    /// Handle undecodable input [start, end): appends the replacement to `out`
    /// and returns the position where decoding resumes.
    pub fn handle(&mut self, start: usize, end: usize, reason: &str, out: &mut StrBuf) -> PyResult<usize> {
        if self.eh != Eh::Other {
            return match h_decode(self.eh, self.encoding, &self.data, start, end) {
                Ok((v, p)) => {
                    out.extend_u32(&v);
                    Ok(p)
                }
                Err(HFail::Raise) => Err(self.raise(start, end, reason)),
                Err(HFail::Type) => {
                    let e = self.exc_obj(start, end, reason)?;
                    Err(wrong_exc(&e))
                }
            };
        }
        if self.handler.is_none() {
            self.handler = Some(lookup_error(self.errors)?);
        }
        let e = self.exc_obj(start, end, reason)?;
        let h = self.handler.clone().unwrap();
        let r = ab::call(&h, &[&e])?;
        let bad = || err!(TypeError, "decoding error handler must return (str, int) tuple");
        if !tuple::is_tuple(&r) || tuple::tuple_len(&r) != 2 {
            return Err(bad());
        }
        let rep = tuple::tuple_get(&r, 0);
        let pos = tuple::tuple_get(&r, 1);
        if !pystr::is_str(rep) || !int::is_int(pos) {
            return Err(bad());
        }
        // The handler may have replaced the input.
        let cur = uexc(&e).object;
        let same = match &self.obj {
            Some(o) => o.as_ptr() == cur,
            None => false,
        };
        if !same {
            if cur.is_null() || !bytes::is_bytes(unsafe { &*cur }) {
                raise!(TypeError, "exception attribute object must be bytes");
            }
            let o = unsafe { (*cur).to_ref() };
            self.data = Cow::Owned(bytes::bytes_data(&o).to_vec());
            self.obj = Some(o);
        }
        let len = self.data.len() as isize;
        let mut p = ab::index_as_ssize(pos, None)?;
        if p < 0 {
            p += len;
        }
        if p < 0 || p > len {
            return Err(bad_position(p));
        }
        out.extend_obj(rep);
        Ok(p as usize)
    }
}

// =====================================================================================
// Text model entry points (str.encode / bytes.decode / codecs.encode / codecs.decode)
// =====================================================================================

use crate::modules::codec_charmap as cmap;
use crate::modules::codec_impl as ci;

enum Fast {
    Utf8,
    Utf16,
    Utf32,
    Ascii,
    Latin1,
}

fn fast_codec(encoding: &str) -> Option<Fast> {
    let n = normalize_encoding(encoding);
    if let Some(rest) = n.strip_prefix("utf") {
        let rest = rest.strip_prefix('_').unwrap_or(rest);
        return match rest {
            "8" => Some(Fast::Utf8),
            "16" => Some(Fast::Utf16),
            "32" => Some(Fast::Utf32),
            _ => None,
        };
    }
    match n.as_str() {
        "ascii" | "us_ascii" => Some(Fast::Ascii),
        "latin1" | "latin_1" | "iso_8859_1" | "iso8859_1" => Some(Fast::Latin1),
        _ => None,
    }
}

fn add_note(op: &str, encoding: &str) {
    crate::modules::traceback_util::add_note_to_current(&format!("{} with '{}' codec failed", op, encoding));
}

fn lookup_text_codec(encoding: &str, alternate: &str) -> PyResult {
    let info = lookup(encoding)?;
    check_text_codec(info, encoding, alternate)
}

fn check_text_codec(info: PyRef, encoding: &str, alternate: &str) -> PyResult {
    if let Some(v) = ab::getattr_opt(&info, crate::name!("_is_text_encoding"))? {
        if !ab::is_true(&v)? {
            raise!(LookupError, "'{}' is not a text encoding; use {} to handle arbitrary codecs", tu::trunc_chars(encoding, 400), alternate);
        }
    }
    Ok(info)
}

fn call_codec(func: &PyObject, obj: &PyObject, errors: Option<&str>, op: &str, encoding: &str) -> PyResult {
    let r = match errors {
        Some(e) => ab::call(func, &[obj, &pystr::new_str(e)]),
        None => ab::call(func, &[obj]),
    };
    let r = match r {
        Ok(r) => r,
        Err(e) => {
            add_note(op, encoding);
            return Err(e);
        }
    };
    if !tuple::is_tuple(&r) || tuple::tuple_len(&r) != 2 {
        if op == "encoding" {
            raise!(TypeError, "encoder must return a tuple (object, integer)");
        }
        raise!(TypeError, "decoder must return a tuple (object, integer)");
    }
    Ok(tuple::tuple_get(&r, 0).to_ref())
}

/// `str.encode(encoding, errors)`.
pub fn encode_text(s: &PyObject, encoding: &str, errors: &str) -> PyResult {
    if let Some(f) = if encoding.is_ascii() { fast_codec(encoding) } else { None } {
        let mut ctx = EncCtx::new(
            match f {
                Fast::Utf8 => "utf-8",
                Fast::Utf16 => "utf-16",
                Fast::Utf32 => "utf-32",
                Fast::Ascii => "ascii",
                Fast::Latin1 => "latin-1",
            },
            s,
            errors,
        );
        let v = match f {
            Fast::Utf8 => ci::encode_utf8(&mut ctx)?,
            Fast::Utf16 => ci::encode_utf16(&mut ctx, 0)?,
            Fast::Utf32 => ci::encode_utf32(&mut ctx, 0)?,
            Fast::Ascii => ci::encode_charlimit(&mut ctx, 128)?,
            Fast::Latin1 => ci::encode_charlimit(&mut ctx, 256)?,
        };
        return Ok(bytes::new_bytes(&v));
    }
    let info = lookup_text_codec(encoding, "codecs.encode()")?;
    encode_with(s, &info, encoding, errors)
}

/// `str.encode` with an encoding name that contains lone surrogates.
pub fn encode_text_obj(s: &PyObject, name: &PyObject, errors: &str) -> PyResult {
    let encoding = pystr::to_string_lossy(name);
    let info = check_text_codec(lookup_obj(name)?, &encoding, "codecs.encode()")?;
    encode_with(s, &info, &encoding, errors)
}

fn encode_with(s: &PyObject, info: &PyObject, encoding: &str, errors: &str) -> PyResult {
    let enc = tuple::tuple_get(&info, 0).to_ref();
    let v = call_codec(&enc, s, Some(errors), "encoding", encoding)?;
    if bytes::is_bytes(&v) {
        return Ok(v);
    }
    if crate::types::bytearray::is_bytearray(&v) {
        return Ok(bytes::new_bytes(crate::types::bytearray::bytearray_data(&v)));
    }
    raise!(
        TypeError,
        "'{}' encoder returned '{}' instead of 'bytes'; use codecs.encode() to encode to arbitrary types",
        tu::trunc_chars(encoding, 400),
        tu::trunc_chars(&tname(&v), 400)
    )
}

/// `bytes.decode(encoding, errors)`; `obj` is the original object (for the
/// registry path), `data` its contents.
pub fn decode_text(data: &[u8], obj: Option<&PyObject>, encoding: &str, errors: &str) -> PyResult {
    if let Some(f) = if encoding.is_ascii() { fast_codec(encoding) } else { None } {
        let name = match f {
            Fast::Utf8 => "utf-8",
            Fast::Utf16 => "utf-16",
            Fast::Utf32 => "utf-32",
            Fast::Ascii => "ascii",
            Fast::Latin1 => "latin-1",
        };
        let mut ctx = DecCtx::new(name, data, errors);
        return match f {
            Fast::Utf8 => Ok(ci::decode_utf8(&mut ctx, true)?.0),
            Fast::Utf16 => Ok(ci::decode_utf16(&mut ctx, &mut 0, true)?.0),
            Fast::Utf32 => Ok(ci::decode_utf32(&mut ctx, &mut 0, true)?.0),
            Fast::Ascii => ci::decode_ascii(&mut ctx),
            Fast::Latin1 => Ok(pystr::from_latin1(data)),
        };
    }
    let info = lookup_text_codec(encoding, "codecs.decode()")?;
    decode_with(data, obj, &info, encoding, errors)
}

/// Decoding of source code declared with a PEP 263 cookie (text encodings only).
pub fn decode_text_for_source(data: &[u8], encoding: &str) -> PyResult {
    let info = lookup_text_codec(encoding, "codecs.open()")?;
    decode_with(data, None, &info, encoding, "strict")
}

/// `bytes.decode` with an encoding name that contains lone surrogates.
pub fn decode_text_obj(data: &[u8], obj: Option<&PyObject>, name: &PyObject, errors: &str) -> PyResult {
    let encoding = pystr::to_string_lossy(name);
    let info = check_text_codec(lookup_obj(name)?, &encoding, "codecs.decode()")?;
    decode_with(data, obj, &info, &encoding, errors)
}

fn decode_with(data: &[u8], obj: Option<&PyObject>, info: &PyObject, encoding: &str, errors: &str) -> PyResult {
    let dec = tuple::tuple_get(&info, 1).to_ref();
    let tmp;
    let o = match obj {
        Some(o) => o,
        None => {
            tmp = bytes::new_bytes(data);
            &tmp
        }
    };
    let v = call_codec(&dec, o, Some(errors), "decoding", encoding)?;
    if pystr::is_str(&v) {
        return Ok(v);
    }
    raise!(
        TypeError,
        "'{}' decoder returned '{}' instead of 'str'; use codecs.decode() to decode to arbitrary types",
        tu::trunc_chars(encoding, 400),
        tu::trunc_chars(&tname(&v), 400)
    )
}

// =====================================================================================
// Module functions
// =====================================================================================

fn pos_args<'b, const N: usize>(fname: &str, args: &[&'b PyObject], min: usize) -> PyResult<[Option<&'b PyObject>; N]> {
    if args.len() < min || args.len() > N {
        if N == min {
            raise!(TypeError, "{}() takes exactly {} argument{} ({} given)", fname, N, if N == 1 { "" } else { "s" }, args.len());
        }
        if args.len() < min {
            raise!(TypeError, "{}() takes at least {} argument{} ({} given)", fname, min, if min == 1 { "" } else { "s" }, args.len());
        }
        raise!(TypeError, "{}() takes at most {} argument{} ({} given)", fname, N, if N == 1 { "" } else { "s" }, args.len());
    }
    let mut out = [None; N];
    for (i, a) in args.iter().enumerate() {
        out[i] = Some(*a);
    }
    Ok(out)
}

fn errors_arg(o: Option<&PyObject>, fname: &str, pos: usize) -> PyResult<String> {
    match o {
        None => Ok("strict".to_string()),
        Some(v) if none::is_none(v) => Ok("strict".to_string()),
        Some(v) => {
            if !pystr::is_str(v) {
                raise!(TypeError, "{}() argument {} must be str or None, not {}", fname, pos, tname(v));
            }
            Ok(pystr::as_utf8(v)?.to_string())
        }
    }
}

fn str_arg<'b>(o: &'b PyObject, fname: &str, pos: usize) -> PyResult<&'b PyObject> {
    if !pystr::is_str(o) {
        raise!(TypeError, "{}() argument {} must be str, not {}", fname, pos, tname(o));
    }
    Ok(o)
}

fn buffer_arg<'b>(o: &'b PyObject) -> PyResult<ByteData<'b>> {
    if pystr::is_str(o) {
        raise!(TypeError, "a bytes-like object is required, not 'str'");
    }
    match tu::byte_data(o) {
        Some(d) => Ok(d),
        None => raise!(TypeError, "a bytes-like object is required, not '{}'", tname(o)),
    }
}

/// Py_buffer(accept={str, buffer}): str is converted to UTF-8.
fn str_or_buffer_arg<'b>(o: &'b PyObject) -> PyResult<ByteData<'b>> {
    if pystr::is_str(o) {
        return Ok(ByteData::Owned(pystr::as_utf8(o)?.as_bytes().to_vec()));
    }
    buffer_arg(o)
}

fn bool_arg(o: Option<&PyObject>, default: bool) -> PyResult<bool> {
    match o {
        None => Ok(default),
        Some(v) => ab::is_true(v),
    }
}

fn int_arg(o: Option<&PyObject>, default: i32) -> PyResult<i32> {
    match o {
        None => Ok(default),
        Some(v) => {
            if !int::is_int(v) && !ab::has_index(v) {
                raise!(TypeError, "'{}' object cannot be interpreted as an integer", tname(v));
            }
            let x = ab::as_i64(v)?;
            Ok(x.clamp(i32::MIN as i64, i32::MAX as i64) as i32)
        }
    }
}

fn pair(a: PyRef, n: usize) -> PyRef {
    tuple::new_tuple(vec![a, int::new_int(n as i64)])
}

// ------------------------------------------------------------------ registry functions

fn m_register(_m: &PyObject, f: &PyObject) -> PyResult {
    if !ab::is_callable(f) {
        raise!(TypeError, "argument must be callable");
    }
    REGISTRY.lock().unwrap().search.push(f.to_ref());
    Ok(none::none_ref())
}

fn m_unregister(_m: &PyObject, f: &PyObject) -> PyResult {
    let mut r = REGISTRY.lock().unwrap();
    if let Some(i) = r.search.iter().position(|x| x.is(f)) {
        r.search.remove(i);
        r.cache.clear();
    }
    Ok(none::none_ref())
}

fn m_lookup(_m: &PyObject, e: &PyObject) -> PyResult {
    let e = str_arg(e, "lookup", 1)?;
    lookup_obj(e)
}

fn m_forget_codec(_m: &PyObject, e: &PyObject) -> PyResult {
    let e = str_arg(e, "_forget_codec", 1)?;
    let key = registry_key(pystr::as_utf8(e)?)?;
    REGISTRY.lock().unwrap().cache.remove(&key);
    Ok(none::none_ref())
}

fn enc_errors_kw(fname: &str, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult<(PyRef, String, String)> {
    let [o, e, er] = crate::args::parse::<3>(&crate::args::Params::new(fname, &["obj", "encoding", "errors"], 0, 1), args, kw)?;
    let encoding = match e {
        None => "utf-8".to_string(),
        Some(v) => pystr::as_utf8(str_arg(v, fname, 2)?)?.to_string(),
    };
    let errors = match er {
        None => "strict".to_string(),
        Some(v) => pystr::as_utf8(str_arg(v, fname, 3)?)?.to_string(),
    };
    Ok((o.unwrap().to_ref(), encoding, errors))
}

fn m_encode(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let (o, encoding, errors) = enc_errors_kw("encode", args, kw)?;
    let info = lookup(&encoding)?;
    let f = tuple::tuple_get(&info, 0).to_ref();
    call_codec(&f, &o, Some(&errors), "encoding", &encoding)
}

fn m_decode(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let (o, encoding, errors) = enc_errors_kw("decode", args, kw)?;
    let info = lookup(&encoding)?;
    let f = tuple::tuple_get(&info, 1).to_ref();
    call_codec(&f, &o, Some(&errors), "decoding", &encoding)
}

fn m_register_error(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let [n, h] = pos_args::<2>("register_error", args, 2)?;
    let n = str_arg(n.unwrap(), "register_error", 1)?;
    let h = h.unwrap();
    if !ab::is_callable(h) {
        raise!(TypeError, "handler must be callable");
    }
    ensure_error_handlers()?;
    REGISTRY.lock().unwrap().errors.insert(pystr::as_utf8(n)?.to_string(), h.to_ref());
    Ok(none::none_ref())
}

fn m_lookup_error(_m: &PyObject, n: &PyObject) -> PyResult {
    let n = str_arg(n, "lookup_error", 1)?;
    lookup_error(pystr::as_utf8(n)?)
}

fn m_unregister_error(_m: &PyObject, n: &PyObject) -> PyResult {
    let n = str_arg(n, "_unregister_error", 1)?;
    let name = pystr::as_utf8(n)?;
    if BUILTIN_HANDLERS.contains(&name) {
        raise!(ValueError, "cannot un-register built-in error handler '{}'", name);
    }
    ensure_error_handlers()?;
    let removed = REGISTRY.lock().unwrap().errors.remove(name).is_some();
    Ok(bool_::new_bool(removed))
}

// ------------------------------------------------------------------ encoders

fn simple_encode(fname: &str, args: &[&PyObject], encname: &str, f: impl FnOnce(&mut EncCtx) -> PyResult<Vec<u8>>) -> PyResult {
    let [s, e] = pos_args::<2>(fname, args, 1)?;
    let s = str_arg(s.unwrap(), fname, 1)?;
    let errors = errors_arg(e, fname, 2)?;
    let mut ctx = EncCtx::new(encname, s, &errors);
    let v = f(&mut ctx)?;
    Ok(pair(bytes::new_bytes(&v), pystr::str_len(s)))
}

fn m_utf_8_encode(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    simple_encode("utf_8_encode", a, "utf-8", ci::encode_utf8)
}
fn m_latin_1_encode(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    simple_encode("latin_1_encode", a, "latin-1", |c| ci::encode_charlimit(c, 256))
}
fn m_ascii_encode(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    simple_encode("ascii_encode", a, "ascii", |c| ci::encode_charlimit(c, 128))
}
fn m_utf_16_le_encode(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    simple_encode("utf_16_le_encode", a, "utf-16-le", |c| ci::encode_utf16(c, -1))
}
fn m_utf_16_be_encode(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    simple_encode("utf_16_be_encode", a, "utf-16-be", |c| ci::encode_utf16(c, 1))
}
fn m_utf_32_le_encode(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    simple_encode("utf_32_le_encode", a, "utf-32-le", |c| ci::encode_utf32(c, -1))
}
fn m_utf_32_be_encode(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    simple_encode("utf_32_be_encode", a, "utf-32-be", |c| ci::encode_utf32(c, 1))
}
fn m_utf_7_encode(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    simple_encode("utf_7_encode", a, "utf-7", |c| Ok(ci::encode_utf7(c.obj)))
}
fn m_unicode_escape_encode(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    simple_encode("unicode_escape_encode", a, "unicodeescape", |c| Ok(ci::encode_unicode_escape(c.obj)))
}
fn m_raw_unicode_escape_encode(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    simple_encode("raw_unicode_escape_encode", a, "rawunicodeescape", |c| Ok(ci::encode_raw_unicode_escape(c.obj)))
}

fn bo_encode(fname: &str, args: &[&PyObject], encname: &str, w32: bool) -> PyResult {
    let [s, e, b] = pos_args::<3>(fname, args, 1)?;
    let s = str_arg(s.unwrap(), fname, 1)?;
    let errors = errors_arg(e, fname, 2)?;
    let bo = int_arg(b, 0)?;
    let bo = bo.signum();
    let name = match (w32, bo) {
        (false, 0) => "utf-16",
        (false, -1) => "utf-16-le",
        (false, _) => "utf-16-be",
        (true, 0) => "utf-32",
        (true, -1) => "utf-32-le",
        (true, _) => "utf-32-be",
    };
    let _ = encname;
    let mut ctx = EncCtx::new(name, s, &errors);
    let v = if w32 { ci::encode_utf32(&mut ctx, bo)? } else { ci::encode_utf16(&mut ctx, bo)? };
    Ok(pair(bytes::new_bytes(&v), pystr::str_len(s)))
}
fn m_utf_16_encode(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    bo_encode("utf_16_encode", a, "utf-16", false)
}
fn m_utf_32_encode(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    bo_encode("utf_32_encode", a, "utf-32", true)
}

fn m_charmap_encode(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let [s, e, mp] = pos_args::<3>("charmap_encode", args, 1)?;
    let s = str_arg(s.unwrap(), "charmap_encode", 1)?;
    let errors = errors_arg(e, "charmap_encode", 2)?;
    let mapping = mp.filter(|m| !none::is_none(m));
    let mut ctx = EncCtx::new(if mapping.is_some() { "charmap" } else { "latin-1" }, s, &errors);
    let v = cmap::charmap_encode(&mut ctx, mapping)?;
    Ok(pair(bytes::new_bytes(&v), pystr::str_len(s)))
}

fn m_charmap_build(_m: &PyObject, t: &PyObject) -> PyResult {
    cmap::charmap_build(t)
}

fn m_escape_encode(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let [d, e] = pos_args::<2>("escape_encode", args, 1)?;
    let d = d.unwrap();
    if !bytes::is_bytes(d) {
        raise!(TypeError, "escape_encode() argument 1 must be bytes, not {}", tname(d));
    }
    errors_arg(e, "escape_encode", 2)?;
    let v = ci::encode_escape(bytes::bytes_data(d));
    Ok(pair(bytes::new_bytes(&v), bytes::bytes_data(d).len()))
}

fn m_readbuffer_encode(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let [d, e] = pos_args::<2>("readbuffer_encode", args, 1)?;
    errors_arg(e, "readbuffer_encode", 2)?;
    let d = str_or_buffer_arg(d.unwrap())?;
    let s = d.as_slice();
    Ok(pair(bytes::new_bytes(s), s.len()))
}

// ------------------------------------------------------------------ decoders

fn final_decode(
    fname: &str,
    args: &[&PyObject],
    encname: &str,
    f: impl FnOnce(&mut DecCtx, bool) -> PyResult<(PyRef, usize)>,
) -> PyResult {
    let [d, e, fi] = pos_args::<3>(fname, args, 1)?;
    let data = buffer_arg(d.unwrap())?;
    let errors = errors_arg(e, fname, 2)?;
    let final_ = bool_arg(fi, false)?;
    let mut ctx = DecCtx::new(encname, data.as_slice(), &errors);
    let (s, n) = f(&mut ctx, final_)?;
    // A final decode reports the whole (original) input as consumed, even if
    // an error handler replaced the input.
    Ok(pair(s, if final_ { data.as_slice().len() } else { n }))
}

fn m_utf_8_decode(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    final_decode("utf_8_decode", a, "utf-8", ci::decode_utf8)
}
fn m_utf_7_decode(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    final_decode("utf_7_decode", a, "utf7", ci::decode_utf7)
}
fn m_utf_16_decode(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    final_decode("utf_16_decode", a, "utf-16", |c, f| ci::decode_utf16(c, &mut 0, f))
}
fn m_utf_16_le_decode(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    final_decode("utf_16_le_decode", a, "utf-16-le", |c, f| ci::decode_utf16(c, &mut -1, f))
}
fn m_utf_16_be_decode(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    final_decode("utf_16_be_decode", a, "utf-16-be", |c, f| ci::decode_utf16(c, &mut 1, f))
}
fn m_utf_32_decode(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    final_decode("utf_32_decode", a, "utf-32", |c, f| ci::decode_utf32(c, &mut 0, f))
}
fn m_utf_32_le_decode(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    final_decode("utf_32_le_decode", a, "utf-32-le", |c, f| ci::decode_utf32(c, &mut -1, f))
}
fn m_utf_32_be_decode(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    final_decode("utf_32_be_decode", a, "utf-32-be", |c, f| ci::decode_utf32(c, &mut 1, f))
}

fn ex_decode(fname: &str, args: &[&PyObject], w32: bool) -> PyResult {
    let [d, e, b, fi] = pos_args::<4>(fname, args, 1)?;
    let data = buffer_arg(d.unwrap())?;
    let errors = errors_arg(e, fname, 2)?;
    let mut bo = int_arg(b, 0)?.signum();
    let final_ = bool_arg(fi, false)?;
    let mut ctx = DecCtx::new(if w32 { "utf-32" } else { "utf-16" }, data.as_slice(), &errors);
    let (s, n) = if w32 { ci::decode_utf32(&mut ctx, &mut bo, final_)? } else { ci::decode_utf16(&mut ctx, &mut bo, final_)? };
    Ok(tuple::new_tuple(vec![s, int::new_int(n as i64), int::new_int(bo as i64)]))
}
fn m_utf_16_ex_decode(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    ex_decode("utf_16_ex_decode", a, false)
}
fn m_utf_32_ex_decode(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    ex_decode("utf_32_ex_decode", a, true)
}

fn m_latin_1_decode(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let [d, e] = pos_args::<2>("latin_1_decode", args, 1)?;
    let data = buffer_arg(d.unwrap())?;
    errors_arg(e, "latin_1_decode", 2)?;
    let s = data.as_slice();
    Ok(pair(pystr::from_latin1(s), s.len()))
}

fn m_ascii_decode(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let [d, e] = pos_args::<2>("ascii_decode", args, 1)?;
    let data = buffer_arg(d.unwrap())?;
    let errors = errors_arg(e, "ascii_decode", 2)?;
    let mut ctx = DecCtx::new("ascii", data.as_slice(), &errors);
    let s = ci::decode_ascii(&mut ctx)?;
    Ok(pair(s, ctx.data.len()))
}

fn m_charmap_decode(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let [d, e, mp] = pos_args::<3>("charmap_decode", args, 1)?;
    let data = buffer_arg(d.unwrap())?;
    let errors = errors_arg(e, "charmap_decode", 2)?;
    let mapping = mp.filter(|m| !none::is_none(m));
    let mut ctx = DecCtx::new(if mapping.is_some() { "charmap" } else { "latin-1" }, data.as_slice(), &errors);
    let s = cmap::charmap_decode(&mut ctx, mapping)?;
    Ok(pair(s, ctx.data.len()))
}

fn escape_decode_common(fname: &str, args: &[&PyObject], raw: bool) -> PyResult {
    let [d, e, fi] = pos_args::<3>(fname, args, 1)?;
    let data = str_or_buffer_arg(d.unwrap())?;
    let errors = errors_arg(e, fname, 2)?;
    let final_ = bool_arg(fi, true)?;
    let mut ctx = DecCtx::new(if raw { "rawunicodeescape" } else { "unicodeescape" }, data.as_slice(), &errors);
    let (s, n, first) = ci::decode_unicode_escape(&mut ctx, final_, raw)?;
    ci::invalid_escape_warning(first)?;
    Ok(pair(s, if final_ { data.as_slice().len() } else { n }))
}
fn m_unicode_escape_decode(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    escape_decode_common("unicode_escape_decode", a, false)
}
fn m_raw_unicode_escape_decode(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    escape_decode_common("raw_unicode_escape_decode", a, true)
}

fn m_escape_decode(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let [d, e] = pos_args::<2>("escape_decode", args, 1)?;
    let data = str_or_buffer_arg(d.unwrap())?;
    let errors = errors_arg(e, "escape_decode", 2)?;
    let (v, first) = ci::decode_escape(data.as_slice(), &errors)?;
    ci::invalid_escape_warning(first)?;
    Ok(pair(bytes::new_bytes(&v), data.as_slice().len()))
}

pub fn init_module() -> PyResult {
    cmap::init_types()?;
    let m = crate::types::module::new_module("_codecs")?;
    let d = crate::types::module::module_dict(&m);
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![
            ("register", O, m_register, "register($module, search_function, /)\n--\n\nAdd a codec search function to the registry."),
            ("unregister", O, m_unregister, "unregister($module, search_function, /)\n--\n\nRemove a codec search function and clear the lookup cache."),
            ("lookup", O, m_lookup, "lookup($module, encoding, /)\n--\n\nReturn the CodecInfo registered for an encoding name."),
            ("_forget_codec", O, m_forget_codec, "_forget_codec($module, encoding, /)\n--\n\nDrop an encoding from the lookup cache."),
            ("encode", FASTKW, m_encode, "encode($module, /, obj, encoding='utf-8', errors='strict')\n--\n\nEncode obj with the codec registered for encoding."),
            ("decode", FASTKW, m_decode, "decode($module, /, obj, encoding='utf-8', errors='strict')\n--\n\nDecode obj with the codec registered for encoding."),
            ("register_error", FAST, m_register_error, "register_error($module, errors, handler, /)\n--\n\nMake handler the error handler named errors.\n\nThe handler receives a UnicodeError and returns (replacement, resume_position)."),
            ("lookup_error", O, m_lookup_error, "lookup_error($module, name, /)\n--\n\nReturn the error handler registered under name."),
            ("_unregister_error", O, m_unregister_error, "_unregister_error($module, errors, /)\n--\n\nRemove a custom error handler; return True if one was removed."),
            ("utf_8_encode", FAST, m_utf_8_encode, "utf_8_encode($module, str, errors=None, /)\n--\n\n"),
            ("utf_8_decode", FAST, m_utf_8_decode, "utf_8_decode($module, data, errors=None, final=False, /)\n--\n\n"),
            ("utf_7_encode", FAST, m_utf_7_encode, "utf_7_encode($module, str, errors=None, /)\n--\n\n"),
            ("utf_7_decode", FAST, m_utf_7_decode, "utf_7_decode($module, data, errors=None, final=False, /)\n--\n\n"),
            ("utf_16_encode", FAST, m_utf_16_encode, "utf_16_encode($module, str, errors=None, byteorder=0, /)\n--\n\n"),
            ("utf_16_decode", FAST, m_utf_16_decode, "utf_16_decode($module, data, errors=None, final=False, /)\n--\n\n"),
            ("utf_16_le_encode", FAST, m_utf_16_le_encode, "utf_16_le_encode($module, str, errors=None, /)\n--\n\n"),
            ("utf_16_le_decode", FAST, m_utf_16_le_decode, "utf_16_le_decode($module, data, errors=None, final=False, /)\n--\n\n"),
            ("utf_16_be_encode", FAST, m_utf_16_be_encode, "utf_16_be_encode($module, str, errors=None, /)\n--\n\n"),
            ("utf_16_be_decode", FAST, m_utf_16_be_decode, "utf_16_be_decode($module, data, errors=None, final=False, /)\n--\n\n"),
            ("utf_16_ex_decode", FAST, m_utf_16_ex_decode, "utf_16_ex_decode($module, data, errors=None, byteorder=0, final=False, /)\n--\n\n"),
            ("utf_32_encode", FAST, m_utf_32_encode, "utf_32_encode($module, str, errors=None, byteorder=0, /)\n--\n\n"),
            ("utf_32_decode", FAST, m_utf_32_decode, "utf_32_decode($module, data, errors=None, final=False, /)\n--\n\n"),
            ("utf_32_le_encode", FAST, m_utf_32_le_encode, "utf_32_le_encode($module, str, errors=None, /)\n--\n\n"),
            ("utf_32_le_decode", FAST, m_utf_32_le_decode, "utf_32_le_decode($module, data, errors=None, final=False, /)\n--\n\n"),
            ("utf_32_be_encode", FAST, m_utf_32_be_encode, "utf_32_be_encode($module, str, errors=None, /)\n--\n\n"),
            ("utf_32_be_decode", FAST, m_utf_32_be_decode, "utf_32_be_decode($module, data, errors=None, final=False, /)\n--\n\n"),
            ("utf_32_ex_decode", FAST, m_utf_32_ex_decode, "utf_32_ex_decode($module, data, errors=None, byteorder=0, final=False, /)\n--\n\n"),
            ("latin_1_encode", FAST, m_latin_1_encode, "latin_1_encode($module, str, errors=None, /)\n--\n\n"),
            ("latin_1_decode", FAST, m_latin_1_decode, "latin_1_decode($module, data, errors=None, /)\n--\n\n"),
            ("ascii_encode", FAST, m_ascii_encode, "ascii_encode($module, str, errors=None, /)\n--\n\n"),
            ("ascii_decode", FAST, m_ascii_decode, "ascii_decode($module, data, errors=None, /)\n--\n\n"),
            ("charmap_encode", FAST, m_charmap_encode, "charmap_encode($module, str, errors=None, mapping=None, /)\n--\n\n"),
            ("charmap_decode", FAST, m_charmap_decode, "charmap_decode($module, data, errors=None, mapping=None, /)\n--\n\n"),
            ("charmap_build", O, m_charmap_build, "charmap_build($module, map, /)\n--\n\n"),
            ("unicode_escape_encode", FAST, m_unicode_escape_encode, "unicode_escape_encode($module, str, errors=None, /)\n--\n\n"),
            ("unicode_escape_decode", FAST, m_unicode_escape_decode, "unicode_escape_decode($module, data, errors=None, final=True, /)\n--\n\n"),
            ("raw_unicode_escape_encode", FAST, m_raw_unicode_escape_encode, "raw_unicode_escape_encode($module, str, errors=None, /)\n--\n\n"),
            ("raw_unicode_escape_decode", FAST, m_raw_unicode_escape_decode, "raw_unicode_escape_decode($module, data, errors=None, final=True, /)\n--\n\n"),
            ("escape_encode", FAST, m_escape_encode, "escape_encode($module, data, errors=None, /)\n--\n\n"),
            ("escape_decode", FAST, m_escape_decode, "escape_decode($module, data, errors=None, /)\n--\n\n"),
            ("readbuffer_encode", FAST, m_readbuffer_encode, "readbuffer_encode($module, data, errors=None, /)\n--\n\n"),
        ],
        Some(&m),
    )?;
    let _ = (list::new_list_empty, dict::new_dict);
    Ok(m)
}
