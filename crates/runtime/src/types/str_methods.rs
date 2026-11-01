//! The `str` type: slots and the complete method suite (CPython 3.15 semantics).
//!
//! Layout/creation helpers live in `str.rs`; this module provides
//! * slots: repr, str, hash, richcompare, sequence/mapping protocol,
//!   iteration, `%` (nb_remainder), constructor, dealloc;
//! * every `str` method (`search` = substring search, `case` = case mapping
//!   and predicates, `format` = `str.format` engine + `_string` helpers,
//!   `percent` = printf-style formatting, `writer` = string building).

mod case;
pub mod format;
mod percent;
pub mod search;
pub mod writer;

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::str::{self as pystr, StrData};
use crate::types::typeobject::tname;
use crate::types::*;
use search::{dispatch, AsUnits, Unit};
use std::ffi::c_int;
use writer::{copy_into, dst_mut, fill, new_from_data, slice_data, substring, Writer};

pub use format::{format_with, formatter_field_name_split, formatter_parser};
pub use percent::percent_format;

#[inline(always)]
fn sd(o: &PyObject) -> StrData<'_> {
    pystr::str_data(o)
}
#[inline(always)]
fn slen(o: &PyObject) -> usize {
    pystr::str_len(o)
}
fn empty() -> PyRef {
    pystr::new_str("")
}
fn ni() -> *mut PyObject {
    none::not_implemented().to_ref().into_raw()
}
fn int_obj(v: usize) -> PyRef {
    int::new_int_usize(v)
}
fn bool_obj(b: bool) -> PyResult {
    Ok(bool_::new_bool(b))
}

// ================================================================== arg helpers

/// `_PyEval_SliceIndex`: None -> None; __index__ (clamped).
fn slice_index(o: Option<&PyObject>) -> PyResult<Option<isize>> {
    match o {
        None => Ok(None),
        Some(o) if none::is_none(o) => Ok(None),
        Some(o) => {
            if !ab::has_index(o) {
                raise!(TypeError, "slice indices must be integers or None or have an __index__ method");
            }
            Ok(Some(ab::index_as_ssize(o, None)?))
        }
    }
}

/// ADJUST_INDICES: returns (start, end) clamped; start may exceed end.
fn adjust(start: Option<isize>, end: Option<isize>, len: usize) -> (usize, usize) {
    let len = len as isize;
    let mut s = start.unwrap_or(0);
    let mut e = end.unwrap_or(isize::MAX);
    if e > len {
        e = len;
    } else if e < 0 {
        e += len;
        if e < 0 {
            e = 0;
        }
    }
    if s < 0 {
        s += len;
        if s < 0 {
            s = 0;
        }
    }
    (s as usize, e as usize)
}

/// Parse `(sub[, start[, end]])` for find-like methods.
fn find_args<'a>(name: &str, args: &[&'a PyObject]) -> PyResult<(&'a PyObject, Option<isize>, Option<isize>)> {
    crate::args::check_pos(name, args, 1, 3)?;
    let start = slice_index(args.get(1).copied())?;
    let end = slice_index(args.get(2).copied())?;
    Ok((args[0], start, end))
}

fn ensure_str(o: &PyObject) -> PyResult<()> {
    if !pystr::is_str(o) {
        raise!(TypeError, "must be str, not {}", tname(o));
    }
    Ok(())
}

fn arg_must_be_str(fname: &str, argn: &str, o: &PyObject) -> PyResult<()> {
    if !pystr::is_str(o) {
        raise!(TypeError, "{}() argument {} must be str, not {}", fname, argn, tname(o));
    }
    Ok(())
}

/// Py_ssize_t from an index-like (clinic `Py_ssize_t` converter).
fn ssize_arg(o: &PyObject) -> PyResult<isize> {
    if crate::types::float::is_float(o) {
        raise!(TypeError, "'float' object cannot be interpreted as an integer");
    }
    ab::index_as_ssize(o, Some(exceptions::OverflowError()))
}

/// Single-character fill argument (clinic `Py_UCS4` converter).
fn fillchar_arg(fname: &str, o: Option<&PyObject>) -> PyResult<u32> {
    match o {
        None => Ok(' ' as u32),
        Some(o) => {
            if !pystr::is_str(o) {
                raise!(TypeError, "{}() argument 2 must be a unicode character, not {}", fname, tname(o));
            }
            if slen(o) != 1 {
                raise!(
                    TypeError,
                    "{}() argument 2 must be a unicode character, not a string of length {}",
                    fname,
                    slen(o)
                );
            }
            Ok(sd(o).get(0))
        }
    }
}

// ================================================================== slots

pub fn repr_obj(o: &PyObject) -> PyRef {
    let d = sd(o);
    // choose quote
    let mut has_sq = false;
    let mut has_dq = false;
    match d {
        StrData::L1(s) => {
            has_sq = s.contains(&b'\'');
            has_dq = s.contains(&b'"');
        }
        _ => {
            for c in d.iter() {
                if c == '\'' as u32 {
                    has_sq = true;
                } else if c == '"' as u32 {
                    has_dq = true;
                }
            }
        }
    }
    let q = if has_sq && !has_dq { b'"' } else { b'\'' };
    let n = d.len();
    // ASCII fast path
    if let (true, StrData::L1(s)) = (pystr::str_is_ascii(o), d) {
        let mut out: Vec<u8> = Vec::with_capacity(n + 2);
        out.push(q);
        for &c in s {
            match c {
                b'\\' => out.extend_from_slice(b"\\\\"),
                b'\t' => out.extend_from_slice(b"\\t"),
                b'\n' => out.extend_from_slice(b"\\n"),
                b'\r' => out.extend_from_slice(b"\\r"),
                _ if c == q => {
                    out.push(b'\\');
                    out.push(q);
                }
                _ if c < 0x20 || c == 0x7f => {
                    const HEX: &[u8; 16] = b"0123456789abcdef";
                    out.extend_from_slice(&[b'\\', b'x', HEX[(c >> 4) as usize], HEX[(c & 15) as usize]]);
                }
                _ => out.push(c),
            }
        }
        out.push(q);
        return pystr::from_latin1(&out);
    }
    let mut w = Writer::with_capacity(n + 2, writer::data_class(d));
    w.push(q as u32);
    for c in d.iter() {
        repr_char(&mut w, c, q as u32);
    }
    w.push(q as u32);
    w.finish()
}

#[inline]
fn push_hex(w: &mut Writer, prefix: char, c: u32, digits: usize) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    w.push('\\' as u32);
    w.push(prefix as u32);
    for i in (0..digits).rev() {
        w.push(HEX[((c >> (4 * i)) & 15) as usize] as u32);
    }
}

fn repr_char(w: &mut Writer, c: u32, q: u32) {
    match c {
        0x5c => w.push_ascii("\\\\"),
        0x09 => w.push_ascii("\\t"),
        0x0a => w.push_ascii("\\n"),
        0x0d => w.push_ascii("\\r"),
        _ if c == q => {
            w.push('\\' as u32);
            w.push(q);
        }
        _ if c < 0x20 || c == 0x7f => push_hex(w, 'x', c, 2),
        _ if c < 0x7f => w.push(c),
        _ => {
            if rpy_unicode::is_printable(c) {
                w.push(c);
            } else if c <= 0xff {
                push_hex(w, 'x', c, 2);
            } else if c <= 0xffff {
                push_hex(w, 'u', c, 4);
            } else {
                push_hex(w, 'U', c, 8);
            }
        }
    }
}

/// `ascii()`-style escaping of a str (used by %a and !a).
pub fn ascii_escape(r: PyRef) -> PyRef {
    if pystr::str_is_ascii(&r) {
        return r;
    }
    let d = sd(&r);
    let mut w = Writer::with_capacity(d.len() + 8, 0x7f);
    for c in d.iter() {
        if c < 0x80 {
            w.push(c);
        } else if c <= 0xff {
            push_hex(&mut w, 'x', c, 2);
        } else if c <= 0xffff {
            push_hex(&mut w, 'u', c, 4);
        } else {
            push_hex(&mut w, 'U', c, 8);
        }
    }
    w.finish()
}

unsafe extern "C" fn str_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe { repr_obj(&*o).into_raw() }
}
unsafe extern "C" fn str_str(o: *mut PyObject) -> *mut PyObject {
    unsafe { writer::exact(&*o).into_raw() }
}
unsafe extern "C" fn str_hash_slot(o: *mut PyObject) -> Py_hash_t {
    unsafe { pystr::str_hash(&*o) }
}

fn cmp_result(c: std::cmp::Ordering, op: c_int) -> bool {
    use std::cmp::Ordering::*;
    match op {
        Py_LT => c == Less,
        Py_LE => c != Greater,
        Py_EQ => c == Equal,
        Py_NE => c != Equal,
        Py_GT => c == Greater,
        _ => c != Less,
    }
}

pub fn compare(a: &PyObject, b: &PyObject) -> std::cmp::Ordering {
    match (sd(a), sd(b)) {
        (StrData::L1(x), StrData::L1(y)) => x.cmp(y),
        (StrData::U2(x), StrData::U2(y)) => x.cmp(y),
        (StrData::U4(x), StrData::U4(y)) => x.cmp(y),
        (da, db) => {
            let n = da.len().min(db.len());
            for i in 0..n {
                let (x, y) = (da.get(i), db.get(i));
                if x != y {
                    return x.cmp(&y);
                }
            }
            da.len().cmp(&db.len())
        }
    }
}

unsafe extern "C" fn str_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !pystr::is_str(b) || !pystr::is_str(a) {
            return ni();
        }
        if op == Py_EQ || op == Py_NE {
            let eq = pystr::str_eq(a, b);
            return bool_::new_bool(if op == Py_EQ { eq } else { !eq }).into_raw();
        }
        if a.is(b) {
            return bool_::new_bool(matches!(op, Py_LE | Py_GE)).into_raw();
        }
        bool_::new_bool(cmp_result(compare(a, b), op)).into_raw()
    }
}
unsafe extern "C" fn str_len_slot(o: *mut PyObject) -> Py_ssize_t {
    unsafe { slen(&*o) as isize }
}
unsafe extern "C" fn str_concat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !pystr::is_str(b) {
            return into_c(Err(err!(TypeError, "can only concatenate str (not \"{}\") to str", tname(b))));
        }
        if !pystr::is_str(a) {
            return ni();
        }
        into_c(concat(a, b))
    }
}

pub fn concat(a: &PyObject, b: &PyObject) -> PyResult {
    let (la, lb) = (slen(a), slen(b));
    if la == 0 {
        return Ok(writer::exact(b));
    }
    if lb == 0 {
        return Ok(writer::exact(a));
    }
    if la > isize::MAX as usize - lb {
        raise!(OverflowError, "strings are too large to concat");
    }
    let maxc = pystr::max_char(a).max(pystr::max_char(b));
    let r = writer::try_alloc(la + lb, maxc)?;
    let mut d = dst_mut(&r);
    copy_into(&mut d, 0, sd(a));
    copy_into(&mut d, la, sd(b));
    Ok(r)
}

pub fn repeat(a: &PyObject, n: isize) -> PyResult {
    if n <= 0 {
        return Ok(empty());
    }
    let len = slen(a);
    if n == 1 {
        return Ok(writer::exact(a));
    }
    if len == 0 {
        return Ok(empty());
    }
    let total = match len.checked_mul(n as usize) {
        Some(t) if t <= isize::MAX as usize => t,
        _ => raise!(OverflowError, "repeated string is too long"),
    };
    if len == 1 && pystr::str_kind(a) == 1 && total == 1 {
        return Ok(writer::exact(a));
    }
    let r = writer::try_alloc(total, pystr::max_char(a))?;
    let k = pystr::str_kind(a) as usize;
    unsafe {
        let p = pystr::str_data_ptr(&r);
        let src = pystr::str_data_ptr(a);
        if len == 1 && k == 1 {
            std::ptr::write_bytes(p, *src, total);
        } else {
            let unit = len * k;
            std::ptr::copy_nonoverlapping(src, p, unit);
            let mut done = unit;
            let all = total * k;
            while done < all {
                let c = done.min(all - done);
                std::ptr::copy_nonoverlapping(p, p.add(done), c);
                done += c;
            }
        }
    }
    Ok(r)
}

unsafe extern "C" fn str_repeat(a: *mut PyObject, n: Py_ssize_t) -> *mut PyObject {
    unsafe { into_c(repeat(&*a, n)) }
}

pub fn contains(a: &PyObject, b: &PyObject) -> PyResult<bool> {
    if !pystr::is_str(b) {
        raise!(TypeError, "'in <string>' requires string as left operand, not {}", tname(b));
    }
    Ok(search::find(sd(a), sd(b), 0, slen(a)).is_some())
}
unsafe extern "C" fn str_contains(a: *mut PyObject, b: *mut PyObject) -> c_int {
    unsafe {
        match contains(&*a, &*b) {
            Ok(v) => v as c_int,
            Err(_) => -1,
        }
    }
}

fn char_at(a: &PyObject, i: usize) -> PyRef {
    let c = sd(a).get(i);
    if c < 256 {
        pystr::latin1_char(c as u8)
    } else {
        pystr::from_codepoints_max(&[c], c)
    }
}

unsafe extern "C" fn str_item(a: *mut PyObject, i: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let a = &*a;
        if i < 0 || i as usize >= slen(a) {
            return into_c(Err(err!(IndexError, "string index out of range")));
        }
        char_at(a, i as usize).into_raw()
    }
}

pub fn getitem(a: &PyObject, k: &PyObject) -> PyResult {
    if slice::is_slice(k) {
        let n = slen(a) as isize;
        let (start, stop, step, len) = slice::indices(k, n)?;
        if len <= 0 {
            return Ok(empty());
        }
        if step == 1 {
            return Ok(substring(a, start as usize, stop as usize));
        }
        if len == 1 {
            return Ok(char_at(a, start as usize));
        }
        let len = len as usize;
        let d = sd(a);
        return Ok(dispatch!(d, |s: T| {
            let mut v: Vec<T> = Vec::with_capacity(len);
            let mut i = start;
            for _ in 0..len {
                v.push(s[i as usize]);
                i += step;
            }
            writer::new_from_units(&v)
        }));
    }
    if ab::has_index(k) {
        let mut i = ab::index_as_ssize(k, Some(exceptions::IndexError()))?;
        let n = slen(a) as isize;
        if i < 0 {
            i += n;
        }
        if i < 0 || i >= n {
            raise!(IndexError, "string index out of range");
        }
        return Ok(char_at(a, i as usize));
    }
    raise!(TypeError, "string indices must be integers, not '{}'", tname(k))
}

unsafe extern "C" fn str_subscript(a: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(getitem(&*a, &*k)) }
}

unsafe extern "C" fn str_mod(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !pystr::is_str(a) {
            return ni();
        }
        into_c(percent_format(a, b))
    }
}

unsafe extern "C" fn str_dealloc(op: *mut PyObject) {
    unsafe {
        let a = &*(op as *const PyASCIIObject);
        let st = a.state;
        const COMPACT: u32 = 1 << 11;
        const ASCII: u32 = 1 << 12;
        if st & COMPACT != 0 {
            if st & ASCII == 0 {
                let c = &*(op as *const PyCompactUnicodeObject);
                if !c.utf8.is_null() {
                    crate::alloc::raw_free(c.utf8 as *mut u8);
                }
            }
        } else {
            let u = &*(op as *const PyUnicodeObject);
            let data = u.data as *mut u8;
            let utf8 = u._base.utf8 as *mut u8;
            if !utf8.is_null() && utf8 != data {
                crate::alloc::raw_free(utf8);
            }
            if !data.is_null() {
                crate::alloc::raw_free(data);
            }
        }
        let tp = (*op).type_ptr();
        crate::types::typeobject::generic_free_and_type_decref(op, tp);
    }
}

// ------------------------------------------------------------------ constructor

unsafe extern "C" fn str_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let (all, kwn) = crate::args::kwdict_to_vectorcall(&*args, kw);
            let refs: Vec<&PyObject> = all.iter().map(|r| &**r).collect();
            let nkw = kwn.as_ref().map_or(0, |k| tuple::tuple_len(k));
            let npos = refs.len() - nkw;
            if nkw == 0 && npos > 3 {
                raise!(TypeError, "str expected at most 3 arguments, got {}", npos);
            }
            if refs.len() > 3 {
                raise!(TypeError, "str() takes at most 3 arguments ({} given)", refs.len());
            }
            static P: crate::args::Params = crate::args::Params::new("str", &["object", "encoding", "errors"], 0, 0);
            let [o, enc, errs] = crate::args::parse::<3>(&P, &refs, kwn.as_deref())?;
            if let Some(e) = enc {
                if !pystr::is_str(e) {
                    raise!(TypeError, "str() argument 'encoding' must be str, not {}", tname(e));
                }
            }
            if let Some(e) = errs {
                if !pystr::is_str(e) {
                    raise!(TypeError, "str() argument 'errors' must be str, not {}", tname(e));
                }
            }
            let s = match (o, enc, errs) {
                (None, None, None) => empty(),
                (Some(o), None, None) => ab::str(o)?,
                (None, _, _) => empty(),
                (Some(o), enc, errs) => decode_to_str(o, enc, errs)?,
            };
            if tp == pystr::str_type() {
                return Ok(s);
            }
            crate::types::str_sub::new_str_subclass(tp, &s)
        })();
        into_c(r)
    }
}

/// `str(obj, encoding, errors)` (PyUnicode_FromEncodedObject).
fn decode_to_str(o: &PyObject, enc: Option<&PyObject>, errs: Option<&PyObject>) -> PyResult {
    if pystr::is_str(o) {
        raise!(TypeError, "decoding str is not supported");
    }
    if !(bytes::is_bytes(o) || bytearray::is_bytearray(o) || crate::buffer::buffer_bytes(o).is_some()) {
        raise!(TypeError, "decoding to str: need a bytes-like object, {} found", tname(o));
    }
    let r = crate::modules::codecs_core::decode_object(o, enc, errs)?;
    if !pystr::is_str(&r) {
        let e = enc.map(pystr::to_string_lossy).unwrap_or_else(|| "utf-8".into());
        raise!(
            TypeError,
            "'{}' decoder returned '{}' instead of 'str'; use codecs.decode() to decode to arbitrary types",
            e,
            tname(&r)
        );
    }
    Ok(r)
}

// ------------------------------------------------------------------ iterators

#[repr(C)]
struct StrIter {
    ob_base: PyObject,
    idx: usize,
    s: *mut PyObject,
}
static mut STR_ASCII_ITER_TYPE: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(export_name = "PyUnicodeIter_Type")]
static mut STR_ITER_TYPE: PyTypeObject = PyTypeObject::ZERO;

unsafe extern "C" fn str_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let tp = if pystr::str_is_ascii(&*o) { &raw mut STR_ASCII_ITER_TYPE } else { &raw mut STR_ITER_TYPE };
        let op = crate::alloc::alloc_object(tp, std::mem::size_of::<StrIter>());
        if op.is_null() {
            return into_c(Err(memory_error()));
        }
        (*(op as *mut StrIter)).s = (*o).to_ref().into_raw();
        (*(op as *mut StrIter)).idx = 0;
        crate::gc::track(op);
        op
    }
}
unsafe extern "C" fn str_iter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let it = &mut *(o as *mut StrIter);
        if it.s.is_null() {
            return std::ptr::null_mut();
        }
        let s = &*it.s;
        if it.idx >= slen(s) {
            let p = std::mem::replace(&mut it.s, std::ptr::null_mut());
            decref(p);
            return std::ptr::null_mut();
        }
        let r = char_at(s, it.idx);
        it.idx += 1;
        r.into_raw()
    }
}
unsafe extern "C" fn str_iter_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let it = &mut *(o as *mut StrIter);
        xdecref(it.s);
        crate::alloc::free_object(o);
    }
}
unsafe extern "C" fn str_iter_traverse(o: *mut PyObject, visit: visitproc, arg: *mut std::ffi::c_void) -> c_int {
    unsafe {
        let it = &*(o as *const StrIter);
        if !it.s.is_null() {
            let r = visit.unwrap()(it.s, arg);
            if r != 0 {
                return r;
            }
        }
        0
    }
}
unsafe extern "C" fn self_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}
fn iter_obj(o: &PyObject) -> &mut StrIter {
    unsafe { &mut *(o.as_ptr() as *mut StrIter) }
}
fn str_iter_length_hint(o: &PyObject) -> PyResult {
    let it = iter_obj(o);
    if it.s.is_null() {
        return Ok(int_obj(0));
    }
    Ok(int_obj(slen(unsafe { &*it.s }).saturating_sub(it.idx)))
}
fn builtin_iter() -> PyResult {
    match dict::get_item_rs(&crate::builtins::builtins_dict(), "iter") {
        Some(f) => Ok(f),
        None => raise!(RuntimeError, "lost builtins.iter"),
    }
}
fn str_iter_reduce(o: &PyObject) -> PyResult {
    let it = iter_obj(o);
    let f = builtin_iter()?;
    if it.s.is_null() {
        return Ok(tuple::new_tuple(vec![f, tuple::new_tuple(vec![empty()])]));
    }
    let s = unsafe { (*it.s).to_ref() };
    Ok(tuple::new_tuple(vec![f, tuple::new_tuple(vec![s]), int_obj(it.idx)]))
}
fn str_iter_setstate(o: &PyObject, st: &PyObject) -> PyResult {
    let it = iter_obj(o);
    let mut i = ab::index_as_ssize(st, None)?;
    if !it.s.is_null() {
        let n = slen(unsafe { &*it.s }) as isize;
        if i < 0 {
            i = 0;
        } else if i > n {
            i = n;
        }
        it.idx = i as usize;
    }
    Ok(none::none_ref())
}

// ================================================================== methods: searching

fn find_impl(o: &PyObject, args: &[&PyObject], name: &str, rev: bool) -> PyResult<Option<usize>> {
    let (sub, s, e) = find_args(name, args)?;
    ensure_str(sub)?;
    let n = slen(o);
    let (start, end) = adjust(s, e, n);
    if start > end || end - start < slen(sub) {
        return Ok(None);
    }
    Ok(if rev { search::rfind(sd(o), sd(sub), start, end) } else { search::find(sd(o), sd(sub), start, end) })
}

fn idx_result(r: Option<usize>) -> PyResult {
    Ok(match r {
        Some(i) => int_obj(i),
        None => int::new_int(-1),
    })
}

fn m_find(o: &PyObject, args: &[&PyObject]) -> PyResult {
    idx_result(find_impl(o, args, "find", false)?)
}
fn m_rfind(o: &PyObject, args: &[&PyObject]) -> PyResult {
    idx_result(find_impl(o, args, "rfind", true)?)
}
fn m_index(o: &PyObject, args: &[&PyObject]) -> PyResult {
    match find_impl(o, args, "index", false)? {
        Some(i) => Ok(int_obj(i)),
        None => raise!(ValueError, "substring not found"),
    }
}
fn m_rindex(o: &PyObject, args: &[&PyObject]) -> PyResult {
    match find_impl(o, args, "rindex", true)? {
        Some(i) => Ok(int_obj(i)),
        None => raise!(ValueError, "substring not found"),
    }
}
fn m_count(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (sub, s, e) = find_args("count", args)?;
    ensure_str(sub)?;
    let (start, end) = adjust(s, e, slen(o));
    if start > end {
        return Ok(int_obj(0));
    }
    Ok(int_obj(search::count(sd(o), sd(sub), start, end, usize::MAX)))
}

fn tailmatch(o: &PyObject, sub: &PyObject, start: Option<isize>, end: Option<isize>, at_end: bool) -> bool {
    let (start, end) = adjust(start, end, slen(o));
    let m = slen(sub);
    if end < start || end - start < m {
        return false;
    }
    if m == 0 {
        return true;
    }
    let pos = if at_end { end - m } else { start };
    let h = sd(o);
    let n = sd(sub);
    match (h, n) {
        (StrData::L1(a), StrData::L1(b)) => &a[pos..pos + m] == b,
        (StrData::U2(a), StrData::U2(b)) => &a[pos..pos + m] == b,
        (StrData::U4(a), StrData::U4(b)) => &a[pos..pos + m] == b,
        _ => {
            if n.kind() > h.kind() {
                return false;
            }
            (0..m).all(|i| h.get(pos + i) == n.get(i))
        }
    }
}

fn startsends(o: &PyObject, args: &[&PyObject], name: &str, at_end: bool) -> PyResult {
    crate::args::check_pos(name, args, 1, 3)?;
    let s = slice_index(args.get(1).copied())?;
    let e = slice_index(args.get(2).copied())?;
    let x = args[0];
    if tuple::is_tuple(x) {
        for item in tuple::tuple_items(x) {
            if !pystr::is_str(item) {
                raise!(TypeError, "tuple for {} must only contain str, not {}", name, tname(item));
            }
            if tailmatch(o, item, s, e, at_end) {
                return bool_obj(true);
            }
        }
        return bool_obj(false);
    }
    if !pystr::is_str(x) {
        raise!(TypeError, "{} first arg must be str or a tuple of str, not {}", name, tname(x));
    }
    bool_obj(tailmatch(o, x, s, e, at_end))
}
fn m_startswith(o: &PyObject, args: &[&PyObject]) -> PyResult {
    startsends(o, args, "startswith", false)
}
fn m_endswith(o: &PyObject, args: &[&PyObject]) -> PyResult {
    startsends(o, args, "endswith", true)
}

// ================================================================== methods: case / predicates

fn m_lower(o: &PyObject) -> PyResult {
    Ok(case::case_map(o, case::CaseOp::Lower))
}
fn m_upper(o: &PyObject) -> PyResult {
    Ok(case::case_map(o, case::CaseOp::Upper))
}
fn m_title(o: &PyObject) -> PyResult {
    Ok(case::case_map(o, case::CaseOp::Title))
}
fn m_capitalize(o: &PyObject) -> PyResult {
    Ok(case::case_map(o, case::CaseOp::Capitalize))
}
fn m_swapcase(o: &PyObject) -> PyResult {
    Ok(case::case_map(o, case::CaseOp::Swapcase))
}
fn m_casefold(o: &PyObject) -> PyResult {
    Ok(case::case_map(o, case::CaseOp::Casefold))
}
fn m_isalnum(o: &PyObject) -> PyResult {
    bool_obj(case::predicate(o, case::Pred::Alnum))
}
fn m_isalpha(o: &PyObject) -> PyResult {
    bool_obj(case::predicate(o, case::Pred::Alpha))
}
fn m_isdecimal(o: &PyObject) -> PyResult {
    bool_obj(case::predicate(o, case::Pred::Decimal))
}
fn m_isdigit(o: &PyObject) -> PyResult {
    bool_obj(case::predicate(o, case::Pred::Digit))
}
fn m_isnumeric(o: &PyObject) -> PyResult {
    bool_obj(case::predicate(o, case::Pred::Numeric))
}
fn m_isspace(o: &PyObject) -> PyResult {
    bool_obj(case::predicate(o, case::Pred::Space))
}
fn m_isprintable(o: &PyObject) -> PyResult {
    bool_obj(case::predicate(o, case::Pred::Printable))
}
fn m_isascii(o: &PyObject) -> PyResult {
    bool_obj(pystr::str_is_ascii(o))
}
fn m_islower(o: &PyObject) -> PyResult {
    bool_obj(case::islower(o))
}
fn m_isupper(o: &PyObject) -> PyResult {
    bool_obj(case::isupper(o))
}
fn m_istitle(o: &PyObject) -> PyResult {
    bool_obj(case::istitle(o))
}
fn m_isidentifier(o: &PyObject) -> PyResult {
    bool_obj(case::isidentifier(o))
}

// ================================================================== methods: padding

fn pad(o: &PyObject, left: usize, right: usize, fillc: u32) -> PyResult {
    if left == 0 && right == 0 {
        return Ok(writer::exact(o));
    }
    let n = slen(o);
    let total = n + left + right;
    let maxc = pystr::max_char(o).max(writer::class_of(fillc));
    let r = writer::try_alloc(total, maxc)?;
    let mut d = dst_mut(&r);
    fill(&mut d, 0, left, fillc);
    copy_into(&mut d, left, sd(o));
    fill(&mut d, left + n, right, fillc);
    Ok(r)
}

fn width_fill(name: &str, args: &[&PyObject]) -> PyResult<(isize, u32)> {
    crate::args::check_pos(name, args, 1, 2)?;
    let w = ssize_arg(args[0])?;
    let f = fillchar_arg(name, args.get(1).copied())?;
    Ok((w, f))
}

fn m_center(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (w, f) = width_fill("center", args)?;
    let n = slen(o) as isize;
    if w <= n {
        return Ok(writer::exact(o));
    }
    let marg = (w - n) as usize;
    let left = marg / 2 + (marg & (w as usize) & 1);
    pad(o, left, marg - left, f)
}
fn m_ljust(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (w, f) = width_fill("ljust", args)?;
    let n = slen(o) as isize;
    if w <= n {
        return Ok(writer::exact(o));
    }
    pad(o, 0, (w - n) as usize, f)
}
fn m_rjust(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (w, f) = width_fill("rjust", args)?;
    let n = slen(o) as isize;
    if w <= n {
        return Ok(writer::exact(o));
    }
    pad(o, (w - n) as usize, 0, f)
}
fn m_zfill(o: &PyObject, wo: &PyObject) -> PyResult {
    let w = ssize_arg(wo)?;
    let n = slen(o) as isize;
    if w <= n {
        return Ok(writer::exact(o));
    }
    let fillw = (w - n) as usize;
    let r = pad(o, fillw, 0, '0' as u32)?;
    // move a leading sign to the front
    let d = sd(&r);
    if d.get(fillw) == '+' as u32 || d.get(fillw) == '-' as u32 {
        let sign = d.get(fillw);
        let mut dm = dst_mut(&r);
        fill(&mut dm, 0, 1, sign);
        fill(&mut dm, fillw, 1, '0' as u32);
    }
    Ok(r)
}

fn m_expandtabs(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("expandtabs", &["tabsize"], 0, 0);
    let [ts] = crate::args::parse::<1>(&P, args, kw)?;
    let tabsize: isize = match ts {
        None => 8,
        Some(t) => {
            if crate::types::float::is_float(t) {
                raise!(TypeError, "'float' object cannot be interpreted as an integer");
            }
            let v = ab::index(t)?;
            match int::as_i64(&v) {
                Some(x) if x >= i32::MIN as i64 && x <= i32::MAX as i64 => x as isize,
                _ => raise!(OverflowError, "Python int too large to convert to C int"),
            }
        }
    };
    let d = sd(o);
    let n = d.len();
    // first pass: length
    let has_tab = match d {
        StrData::L1(s) => s.contains(&b'\t'),
        _ => d.iter().any(|c| c == 9),
    };
    if !has_tab {
        return Ok(writer::exact(o));
    }
    let mut total: usize = 0;
    let mut col: usize = 0;
    for c in d.iter() {
        if c == 9 {
            if tabsize > 0 {
                let inc = tabsize as usize - col % tabsize as usize;
                col = col.checked_add(inc).filter(|&x| x <= isize::MAX as usize).ok_or_else(|| err!(OverflowError, "new string is too long"))?;
                total = total.checked_add(inc).filter(|&x| x <= isize::MAX as usize).ok_or_else(|| err!(OverflowError, "new string is too long"))?;
            }
        } else {
            total = total.checked_add(1).filter(|&x| x <= isize::MAX as usize).ok_or_else(|| err!(OverflowError, "new string is too long"))?;
            col += 1;
            if c == 10 || c == 13 {
                col = 0;
            }
        }
    }
    let r = writer::try_alloc(total, pystr::max_char(o))?;
    let mut dm = dst_mut(&r);
    let mut j = 0;
    col = 0;
    for i in 0..n {
        let c = d.get(i);
        if c == 9 {
            if tabsize > 0 {
                let inc = tabsize as usize - col % tabsize as usize;
                fill(&mut dm, j, inc, ' ' as u32);
                j += inc;
                col += inc;
            }
        } else {
            fill(&mut dm, j, 1, c);
            j += 1;
            col += 1;
            if c == 10 || c == 13 {
                col = 0;
            }
        }
    }
    Ok(r)
}

// ================================================================== methods: strip

fn strip_impl(o: &PyObject, args: &[&PyObject], name: &str, left: bool, right: bool) -> PyResult {
    crate::args::check_pos(name, args, 0, 1)?;
    let d = sd(o);
    let n = d.len();
    let (mut i, mut j) = (0usize, n);
    match args.first() {
        None => strip_ws(d, left, right, &mut i, &mut j),
        Some(c) if none::is_none(c) => strip_ws(d, left, right, &mut i, &mut j),
        Some(c) => {
            if !pystr::is_str(c) {
                raise!(TypeError, "{} arg must be None or str", name);
            }
            let cs = sd(c);
            let set_has = |ch: u32| -> bool {
                match cs {
                    StrData::L1(s) => ch < 256 && s.contains(&(ch as u8)),
                    _ => cs.iter().any(|x| x == ch),
                }
            };
            if left {
                while i < j && set_has(d.get(i)) {
                    i += 1;
                }
            }
            if right {
                while j > i && set_has(d.get(j - 1)) {
                    j -= 1;
                }
            }
        }
    }
    Ok(substring(o, i, j))
}

fn strip_ws(d: StrData, left: bool, right: bool, i: &mut usize, j: &mut usize) {
    if left {
        while *i < *j && case::is_space(d.get(*i)) {
            *i += 1;
        }
    }
    if right {
        while *j > *i && case::is_space(d.get(*j - 1)) {
            *j -= 1;
        }
    }
}

fn m_strip(o: &PyObject, args: &[&PyObject]) -> PyResult {
    strip_impl(o, args, "strip", true, true)
}
fn m_lstrip(o: &PyObject, args: &[&PyObject]) -> PyResult {
    strip_impl(o, args, "lstrip", true, false)
}
fn m_rstrip(o: &PyObject, args: &[&PyObject]) -> PyResult {
    strip_impl(o, args, "rstrip", false, true)
}

fn m_removeprefix(o: &PyObject, p: &PyObject) -> PyResult {
    arg_must_be_str("removeprefix", "", p).map_err(|_| err!(TypeError, "removeprefix() argument must be str, not {}", tname(p)))?;
    if tailmatch(o, p, None, None, false) {
        return Ok(substring(o, slen(p), slen(o)));
    }
    Ok(writer::exact(o))
}
fn m_removesuffix(o: &PyObject, p: &PyObject) -> PyResult {
    arg_must_be_str("removesuffix", "", p).map_err(|_| err!(TypeError, "removesuffix() argument must be str, not {}", tname(p)))?;
    if slen(p) > 0 && tailmatch(o, p, None, None, true) {
        return Ok(substring(o, 0, slen(o) - slen(p)));
    }
    Ok(writer::exact(o))
}

// ================================================================== methods: split family

fn split_args(name: &str, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult<(Option<PyRef>, usize)> {
    static NAMES: [&str; 2] = ["sep", "maxsplit"];
    let p = crate::args::Params::new(name, &NAMES, 0, 0);
    let [sep, ms] = crate::args::parse::<2>(&p, args, kw)?;
    let sep = match sep {
        None => None,
        Some(s) if none::is_none(s) => None,
        Some(s) => {
            if !pystr::is_str(s) {
                raise!(TypeError, "must be str or None, not {}", tname(s));
            }
            if slen(s) == 0 {
                raise!(ValueError, "empty separator");
            }
            Some(s.to_ref())
        }
    };
    let maxsplit = match ms {
        None => -1,
        Some(m) => ssize_arg(m)?,
    };
    Ok((sep, if maxsplit < 0 { usize::MAX } else { maxsplit as usize }))
}

fn list_of(v: Vec<PyRef>) -> PyResult {
    Ok(list::new_list(v))
}

fn m_split(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let (sep, maxsplit) = split_args("split", args, kw)?;
    let d = sd(o);
    let n = d.len();
    let mut out = Vec::new();
    match sep {
        None => {
            let mut i = 0;
            let mut splits = 0;
            while i < n {
                while i < n && case::is_space(d.get(i)) {
                    i += 1;
                }
                if i == n {
                    break;
                }
                if splits == maxsplit {
                    // rest (keeps trailing whitespace)
                    if i == 0 && pystr::is_str_exact(o) {
                        out.push(o.to_ref());
                    } else {
                        out.push(substring(o, i, n));
                    }
                    break;
                }
                let j = i;
                while i < n && !case::is_space(d.get(i)) {
                    i += 1;
                }
                if j == 0 && i == n && pystr::is_str_exact(o) {
                    out.push(o.to_ref());
                } else {
                    out.push(substring(o, j, i));
                }
                splits += 1;
            }
        }
        Some(sep) => {
            let m = slen(&sep);
            let pos = search::find_all(d, sd(&sep), maxsplit);
            if pos.is_empty() {
                out.push(writer::exact(o));
            } else {
                let mut last = 0;
                for p in pos {
                    out.push(substring(o, last, p));
                    last = p + m;
                }
                out.push(substring(o, last, n));
            }
        }
    }
    list_of(out)
}

fn m_rsplit(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let (sep, maxsplit) = split_args("rsplit", args, kw)?;
    let d = sd(o);
    let n = d.len();
    let mut out = Vec::new();
    match sep {
        None => {
            let mut i = n;
            let mut splits = 0;
            while i > 0 {
                while i > 0 && case::is_space(d.get(i - 1)) {
                    i -= 1;
                }
                if i == 0 {
                    break;
                }
                if splits == maxsplit {
                    out.push(substring(o, 0, i));
                    break;
                }
                let j = i;
                while i > 0 && !case::is_space(d.get(i - 1)) {
                    i -= 1;
                }
                out.push(substring(o, i, j));
                splits += 1;
            }
        }
        Some(sep) => {
            let m = slen(&sep);
            let pos = search::rfind_all(d, sd(&sep), maxsplit);
            if pos.is_empty() {
                out.push(writer::exact(o));
            } else {
                let mut last = n;
                for p in pos {
                    out.push(substring(o, p + m, last));
                    last = p;
                }
                out.push(substring(o, 0, last));
            }
        }
    }
    out.reverse();
    list_of(out)
}

fn m_splitlines(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("splitlines", &["keepends"], 0, 0);
    let [ke] = crate::args::parse::<1>(&P, args, kw)?;
    let keep = match ke {
        None => false,
        Some(k) => {
            if crate::types::float::is_float(k) {
                raise!(TypeError, "'float' object cannot be interpreted as an integer");
            }
            ab::is_true(k)?
        }
    };
    let d = sd(o);
    let n = d.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let j = i;
        while i < n && !case::is_linebreak(d.get(i)) {
            i += 1;
        }
        let mut eol = i;
        if i < n {
            if d.get(i) == 13 && i + 1 < n && d.get(i + 1) == 10 {
                i += 2;
            } else {
                i += 1;
            }
            if keep {
                eol = i;
            }
        }
        if j == 0 && eol == n && pystr::is_str_exact(o) {
            out.push(o.to_ref());
        } else {
            out.push(substring(o, j, eol));
        }
    }
    list_of(out)
}

fn partition_impl(o: &PyObject, sep: &PyObject, rev: bool) -> PyResult {
    if !pystr::is_str(sep) {
        raise!(TypeError, "must be str, not {}", tname(sep));
    }
    let m = slen(sep);
    if m == 0 {
        raise!(ValueError, "empty separator");
    }
    let n = slen(o);
    let pos = if rev { search::rfind(sd(o), sd(sep), 0, n) } else { search::find(sd(o), sd(sep), 0, n) };
    let v = match pos {
        Some(p) => vec![substring(o, 0, p), writer::exact(sep), substring(o, p + m, n)],
        None => {
            if rev {
                vec![empty(), empty(), writer::exact(o)]
            } else {
                vec![writer::exact(o), empty(), empty()]
            }
        }
    };
    Ok(tuple::new_tuple(v))
}
fn m_partition(o: &PyObject, sep: &PyObject) -> PyResult {
    partition_impl(o, sep, false)
}
fn m_rpartition(o: &PyObject, sep: &PyObject) -> PyResult {
    partition_impl(o, sep, true)
}

// ================================================================== join / replace

pub fn join(sep: &PyObject, it: &PyObject) -> PyResult {
    let owned;
    let items: &[&PyObject] = if list::is_list_exact(it) {
        // snapshot (the list may be mutated by nothing here, but keep refs alive)
        owned = list::list_to_vec(it);
        unsafe { std::mem::transmute::<&[PyRef], &[&PyObject]>(&owned[..]) }
    } else if tuple::is_tuple(it) {
        tuple::tuple_items(it)
    } else {
        let tp = it.ty();
        if tp.tp_iter.is_none() && (tp.tp_as_sequence.is_null() || unsafe { (*tp.tp_as_sequence).sq_item.is_none() }) {
            raise!(TypeError, "can only join an iterable");
        }
        owned = ab::collect_iterable(it)?;
        unsafe { std::mem::transmute::<&[PyRef], &[&PyObject]>(&owned[..]) }
    };
    join_items(sep, items)
}

pub fn join_items(sep: &PyObject, items: &[&PyObject]) -> PyResult {
    match items.len() {
        0 => return Ok(empty()),
        1 if pystr::is_str_exact(items[0]) => return Ok(items[0].to_ref()),
        _ => {}
    }
    let sl = slen(sep);
    let mut total: usize = 0;
    let mut maxc = if items.len() > 1 { pystr::max_char(sep) } else { 0 };
    for (i, x) in items.iter().enumerate() {
        if !pystr::is_str(x) {
            raise!(TypeError, "sequence item {}: expected str instance, {} found", i, tname(x));
        }
        let add = slen(x) + if i > 0 { sl } else { 0 };
        total = match total.checked_add(add) {
            Some(t) if t <= isize::MAX as usize => t,
            _ => raise!(OverflowError, "join() result is too long for a Python string"),
        };
        if slen(x) > 0 {
            maxc = maxc.max(pystr::max_char(x));
        }
    }
    if total == 0 {
        return Ok(empty());
    }
    let r = writer::try_alloc(total, maxc)?;
    let mut d = dst_mut(&r);
    let mut at = 0;
    let sdat = sd(sep);
    for (i, x) in items.iter().enumerate() {
        if i > 0 && sl > 0 {
            copy_into(&mut d, at, sdat);
            at += sl;
        }
        let xl = slen(x);
        copy_into(&mut d, at, sd(x));
        at += xl;
    }
    if total == 1 {
        let c = sd(&r).get(0);
        if c < 256 {
            return Ok(pystr::latin1_char(c as u8));
        }
    }
    Ok(r)
}

fn m_join(sep: &PyObject, it: &PyObject) -> PyResult {
    join(sep, it)
}

pub fn replace(o: &PyObject, old: &PyObject, new: &PyObject, count: usize) -> PyResult {
    let n = slen(o);
    let m = slen(old);
    let nl = slen(new);
    if count == 0 || (m == 0 && false) {
        return Ok(writer::exact(o));
    }
    if m == 0 {
        // insert `new` between every character (count-limited)
        let inserts = (n + 1).min(count);
        if nl == 0 {
            return Ok(writer::exact(o));
        }
        let total = inserts
            .checked_mul(nl)
            .and_then(|x| x.checked_add(n))
            .filter(|&x| x <= isize::MAX as usize)
            .ok_or_else(|| err!(OverflowError, "replace string is too long"))?;
        let maxc = pystr::max_char(o).max(pystr::max_char(new));
        let r = writer::try_alloc(total, maxc)?;
        let mut d = dst_mut(&r);
        let src = sd(o);
        let nd = sd(new);
        let mut at = 0;
        for i in 0..n {
            if i < inserts {
                copy_into(&mut d, at, nd);
                at += nl;
            }
            copy_into(&mut d, at, slice_data(src, i, i + 1));
            at += 1;
        }
        if n < inserts {
            copy_into(&mut d, at, nd);
        }
        return Ok(fix_width(r, pystr::max_char(o) >= pystr::max_char(new)));
    }
    if m > n {
        return Ok(writer::exact(o));
    }
    let pos = search::find_all(sd(o), sd(old), count);
    if pos.is_empty() {
        return Ok(writer::exact(o));
    }
    let k = pos.len();
    let total = if nl >= m {
        (nl - m).checked_mul(k).and_then(|x| x.checked_add(n))
    } else {
        Some(n - (m - nl) * k)
    }
    .filter(|&x| x <= isize::MAX as usize)
    .ok_or_else(|| err!(OverflowError, "replace string is too long"))?;
    if total == 0 {
        return Ok(empty());
    }
    let maxc = pystr::max_char(o).max(if nl > 0 { pystr::max_char(new) } else { 0 });
    let r = writer::try_alloc(total, maxc)?;
    let mut d = dst_mut(&r);
    let src = sd(o);
    let nd = sd(new);
    let mut at = 0;
    let mut last = 0;
    for p in pos {
        let seg = p - last;
        copy_into(&mut d, at, slice_data(src, last, p));
        at += seg;
        if nl > 0 {
            copy_into(&mut d, at, nd);
            at += nl;
        }
        last = p + m;
    }
    copy_into(&mut d, at, slice_data(src, last, n));
    // the source's widest characters may all have been replaced
    Ok(fix_width(r, pystr::max_char(o) > pystr::max_char(new) || nl == 0))
}

/// Re-canonicalise a result whose width came from a bound that may no longer hold.
fn fix_width(r: PyRef, may_shrink: bool) -> PyRef {
    if !may_shrink {
        if slen(&r) == 1 {
            return new_from_data(sd(&r));
        }
        return r;
    }
    let d = sd(&r);
    let cls = writer::data_class(d);
    if cls != pystr::max_char(&r) || slen(&r) == 1 {
        return new_from_data(d);
    }
    r
}

fn m_replace(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("replace", &["old", "new", "count"], 2, 2);
    let [old, new, cnt] = crate::args::parse::<3>(&P, args, kw)?;
    let (old, new) = (old.unwrap(), new.unwrap());
    arg_must_be_str("replace", "1", old)?;
    arg_must_be_str("replace", "2", new)?;
    let count = match cnt {
        None => -1,
        Some(c) => ssize_arg(c)?,
    };
    replace(o, old, new, if count < 0 { usize::MAX } else { count as usize })
}

// ================================================================== translate / maketrans

unsafe extern "C" fn maketrans_c(_s: *mut PyObject, a: *const *mut PyObject, n: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let args: &[&PyObject] = if n == 0 { &[] } else { std::slice::from_raw_parts(a as *const &PyObject, n as usize) };
        into_c(m_maketrans(none::none(), args))
    }
}

fn m_maketrans(_cls: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("maketrans", args, 1, 3)?;
    let x = args[0];
    let out = dict::new_dict();
    if args.len() == 1 {
        let items: Vec<(PyRef, PyRef)> = if dict::is_dict(x) {
            dict::items_vec(x)
        } else if tname(x) == "frozendict" {
            let it = ab::call_method_str(x, "items", &[])?;
            let mut v = Vec::new();
            for kv in ab::collect_iterable(&it)? {
                let t = ab::sequence_tuple(&kv)?;
                v.push((tuple::tuple_get(&t, 0).to_ref(), tuple::tuple_get(&t, 1).to_ref()));
            }
            v
        } else {
            raise!(TypeError, "if you give only one argument to maketrans it must be a dict");
        };
        for (k, v) in items {
            if pystr::is_str(&k) {
                if slen(&k) != 1 {
                    raise!(ValueError, "string keys in translate table must be of length 1");
                }
                let c = int::new_int(sd(&k).get(0) as i64);
                dict::set_item(&out, &c, &v)?;
            } else if int::is_int(&k) {
                dict::set_item(&out, &k, &v)?;
            } else {
                raise!(TypeError, "keys in translate table must be strings or integers");
            }
        }
        return Ok(out);
    }
    let y = args[1];
    if !pystr::is_str(x) {
        raise!(TypeError, "maketrans() argument 1 must be str, not {}", tname(x));
    }
    if !pystr::is_str(y) {
        raise!(TypeError, "maketrans() argument 2 must be str, not {}", tname(y));
    }
    if slen(x) != slen(y) {
        raise!(ValueError, "the first two maketrans arguments must have equal length");
    }
    let (dx, dy) = (sd(x), sd(y));
    for i in 0..dx.len() {
        let k = int::new_int(dx.get(i) as i64);
        let v = int::new_int(dy.get(i) as i64);
        dict::set_item(&out, &k, &v)?;
    }
    if let Some(z) = args.get(2) {
        if !pystr::is_str(z) {
            raise!(TypeError, "maketrans() argument 3 must be str, not {}", tname(z));
        }
        for c in sd(z).iter() {
            let k = int::new_int(c as i64);
            dict::set_item(&out, &k, none::none())?;
        }
    }
    Ok(out)
}

enum Tr {
    Keep,
    Delete,
    Char(u32),
    Str(PyRef),
}

fn translate_lookup(table: &PyObject, c: u32) -> PyResult<Tr> {
    let key = int::new_int(c as i64);
    let r = match ab::getitem(table, &key) {
        Ok(r) => r,
        Err(e) => {
            if exc_matches(exceptions::LookupError()) {
                clear_exc();
                return Ok(Tr::Keep);
            }
            return Err(e);
        }
    };
    if none::is_none(&r) {
        return Ok(Tr::Delete);
    }
    if int::is_int(&r) {
        let v = int::as_i64(&r);
        match v {
            Some(v) if (0..=0x10ffff).contains(&v) => return Ok(Tr::Char(v as u32)),
            _ => raise!(ValueError, "character mapping must be in range(0x110000)"),
        }
    }
    if pystr::is_str(&r) {
        if slen(&r) == 1 {
            return Ok(Tr::Char(sd(&r).get(0)));
        }
        return Ok(Tr::Str(r));
    }
    raise!(TypeError, "character mapping must return integer, None or str")
}

fn m_translate(o: &PyObject, table: &PyObject) -> PyResult {
    let d = sd(o);
    let n = d.len();
    let mut w = Writer::with_capacity(n, 0);
    // cache for Latin-1 characters
    let mut cache: Vec<Option<Tr>> = Vec::new();
    let mut changed = false;
    for i in 0..n {
        let c = d.get(i);
        let tr_owned;
        let tr: &Tr = if c < 256 {
            if cache.is_empty() {
                cache.resize_with(256, || None);
            }
            if cache[c as usize].is_none() {
                cache[c as usize] = Some(translate_lookup(table, c)?);
            }
            cache[c as usize].as_ref().unwrap()
        } else {
            tr_owned = translate_lookup(table, c)?;
            &tr_owned
        };
        match tr {
            Tr::Keep => w.push(c),
            Tr::Delete => changed = true,
            Tr::Char(x) => {
                changed |= *x != c;
                w.push(*x)
            }
            Tr::Str(s) => {
                changed = true;
                w.push_obj(s)
            }
        }
    }
    if !changed && pystr::is_str_exact(o) {
        return Ok(o.to_ref());
    }
    Ok(w.finish())
}

// ================================================================== misc methods

fn m_encode(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("encode", &["encoding", "errors"], 0, 0);
    let [enc, errs] = crate::args::parse::<2>(&P, args, kw)?;
    if let Some(e) = enc {
        if !pystr::is_str(e) {
            raise!(TypeError, "encode() argument 'encoding' must be str, not {}", tname(e));
        }
    }
    if let Some(e) = errs {
        if !pystr::is_str(e) {
            raise!(TypeError, "encode() argument 'errors' must be str, not {}", tname(e));
        }
    }
    let r = crate::modules::codecs_core::encode_str(o, enc, errs)?;
    if !bytes::is_bytes(&r) {
        if bytearray::is_bytearray(&r) {
            return Ok(bytes::new_bytes(bytearray::bytearray_data(&r)));
        }
        let e = enc.map(pystr::to_string_lossy).unwrap_or_else(|| "utf-8".into());
        raise!(
            TypeError,
            "'{}' encoder returned '{}' instead of 'bytes'; use codecs.encode() to encode to arbitrary types",
            e,
            tname(&r)
        );
    }
    Ok(r)
}

fn m_format(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let (pos, kws) = crate::args::split(args, kw);
    let kwargs = if kws.is_empty() {
        None
    } else {
        let d = dict::new_dict();
        for (k, v) in kws {
            dict::set_item(&d, k, v)?;
        }
        Some(d)
    };
    format_with(o, pos, kwargs.as_deref(), false)
}

fn m_format_map(o: &PyObject, m: &PyObject) -> PyResult {
    format_with(o, &[], Some(m), true)
}

fn m_dunder_format(o: &PyObject, spec: &PyObject) -> PyResult {
    if !pystr::is_str(spec) {
        raise!(TypeError, "__format__() argument must be str, not {}", tname(spec));
    }
    format_str_spec(o, spec)
}

/// `str.__format__` (format-spec mini language for strings).
pub fn format_str_spec(o: &PyObject, spec: &PyObject) -> PyResult {
    if slen(spec) == 0 {
        if pystr::is_str_exact(o) {
            return Ok(o.to_ref());
        }
        return ab::str(o);
    }
    let tn = tname(o);
    let spec_s = pystr::to_string_lossy(spec);
    let fs = rpy_numfmt::FormatSpec::parse_str(&spec_s, &tn).map_err(format_impl::numfmt_error)?;
    let n = slen(o);
    let lay = rpy_numfmt::format::str_layout(n, &fs, &tn).map_err(format_impl::numfmt_error)?;
    let take = lay.take.min(n);
    if lay.lpad == 0 && lay.rpad == 0 && take == n {
        return Ok(writer::exact(o));
    }
    let src = writer::substring(o, 0, take);
    if lay.lpad == 0 && lay.rpad == 0 {
        return Ok(src);
    }
    pad(&src, lay.lpad, lay.rpad, lay.fill as u32)
}

fn m_getnewargs(o: &PyObject) -> PyResult {
    let copy = new_from_data(sd(o));
    // must not be the same object
    let copy = if copy.is(o) {
        let r = pystr::alloc_str(slen(o), pystr::max_char(o));
        copy_into(&mut dst_mut(&r), 0, sd(o));
        r
    } else {
        copy
    };
    Ok(tuple::new_tuple(vec![copy]))
}

fn m_sizeof(o: &PyObject) -> PyResult {
    let a = unsafe { &*(o.as_ptr() as *const PyASCIIObject) };
    let k = pystr::str_kind(o) as usize;
    let n = slen(o);
    let compact = a.state & (1 << 11) != 0;
    let ascii = pystr::str_is_ascii(o);
    let mut size = if compact {
        if ascii {
            std::mem::size_of::<PyASCIIObject>()
        } else {
            std::mem::size_of::<PyCompactUnicodeObject>()
        }
    } else {
        std::mem::size_of::<PyUnicodeObject>()
    };
    size += (n + 1) * k;
    if !ascii {
        let c = unsafe { &*(o.as_ptr() as *const PyCompactUnicodeObject) };
        if !c.utf8.is_null() {
            size += c.utf8_length as usize + 1;
        }
    }
    if !compact {
        // basicsize of the subclass beyond PyUnicodeObject is accounted by object.__sizeof__
        let extra = o.ty().tp_basicsize as usize;
        size = size - std::mem::size_of::<PyUnicodeObject>() + extra.max(std::mem::size_of::<PyUnicodeObject>());
    }
    Ok(int_obj(size))
}

// ================================================================== registration

static mut STR_SEQ: PySequenceMethods = PySequenceMethods::ZERO;
static mut STR_MAP: PyMappingMethods = PyMappingMethods::ZERO;
static mut STR_NUM: PyNumberMethods = PyNumberMethods::ZERO;

const STR_DOC: &std::ffi::CStr = c"str(object='') -> str\nstr(bytes_or_buffer[, encoding[, errors]]) -> str\n\nCreate a new string object from the given object. If encoding or\nerrors is specified, then the object must expose a data buffer\nthat will be decoded using the given encoding and error handler.\nOtherwise, returns the result of object.__str__() (if defined)\nor repr(object).\nencoding defaults to 'utf-8'.\nerrors defaults to 'strict'.";

pub fn init_types() {
    unsafe {
        let t = &mut *pystr::str_type();
        t.tp_name = c"str".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyUnicodeObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_UNICODE_SUBCLASS | (1 << 22);
        t.tp_dealloc = Some(str_dealloc);
        t.tp_repr = Some(str_repr);
        t.tp_str = Some(str_str);
        t.tp_hash = Some(str_hash_slot);
        t.tp_richcompare = Some(str_richcompare);
        t.tp_iter = Some(str_iter);
        t.tp_new = Some(str_new);
        t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
        t.tp_doc = STR_DOC.as_ptr();
        let s = &mut *(&raw mut STR_SEQ);
        s.sq_length = Some(str_len_slot);
        s.sq_concat = Some(str_concat);
        s.sq_repeat = Some(str_repeat);
        s.sq_item = Some(str_item);
        s.sq_contains = Some(str_contains);
        t.tp_as_sequence = s;
        let m = &mut *(&raw mut STR_MAP);
        m.mp_length = Some(str_len_slot);
        m.mp_subscript = Some(str_subscript);
        t.tp_as_mapping = m;
        let nb = &mut *(&raw mut STR_NUM);
        nb.nb_remainder = Some(str_mod);
        t.tp_as_number = nb;
        t.tp_methods = crate::mdefs![
            ("encode", FASTKW, m_encode, "encode($self, /, encoding='utf-8', errors='strict')\n--\n\nEncode the string using the codec registered for encoding.\n\n  encoding\n    The encoding in which to encode the string.\n  errors\n    The error handling scheme to use for encoding errors.\n    The default is 'strict' meaning that encoding errors raise a\n    UnicodeEncodeError.  Other possible values are 'ignore', 'replace' and\n    'xmlcharrefreplace' as well as any other name registered with\n    codecs.register_error that can handle UnicodeEncodeErrors."),
            ("replace", FASTKW, m_replace, "replace($self, old, new, /, count=-1)\n--\n\nReturn a copy with all occurrences of substring old replaced by new.\n\n  count\n    Maximum number of occurrences to replace.\n    -1 (the default value) means replace all occurrences.\n\nIf the optional argument count is given, only the first count occurrences are\nreplaced."),
            ("split", FASTKW, m_split, "split($self, /, sep=None, maxsplit=-1)\n--\n\nReturn a list of the substrings in the string, using sep as the separator string.\n\n  sep\n    The separator used to split the string.\n\n    When set to None (the default value), will split on any whitespace\n    character (including \\n \\r \\t \\f and spaces) and will discard\n    empty strings from the result.\n  maxsplit\n    Maximum number of splits.\n    -1 (the default value) means no limit.\n\nSplitting starts at the front of the string and works to the end.\n\nNote, str.split() is mainly useful for data that has been intentionally\ndelimited.  With natural text that includes punctuation, consider using\nthe regular expression module."),
            ("rsplit", FASTKW, m_rsplit, "rsplit($self, /, sep=None, maxsplit=-1)\n--\n\nReturn a list of the substrings in the string, using sep as the separator string.\n\n  sep\n    The separator used to split the string.\n\n    When set to None (the default value), will split on any whitespace\n    character (including \\n \\r \\t \\f and spaces) and will discard\n    empty strings from the result.\n  maxsplit\n    Maximum number of splits.\n    -1 (the default value) means no limit.\n\nSplitting starts at the end of the string and works to the front."),
            ("join", O, m_join, "join($self, iterable, /)\n--\n\nConcatenate any number of strings.\n\nThe string whose method is called is inserted in between each given string.\nThe result is returned as a new string.\n\nExample: '.'.join(['ab', 'pq', 'rs']) -> 'ab.pq.rs'"),
            ("capitalize", NOARGS, m_capitalize, "capitalize($self, /)\n--\n\nReturn a capitalized version of the string.\n\nMore specifically, make the first character have upper case and the rest lower\ncase."),
            ("casefold", NOARGS, m_casefold, "casefold($self, /)\n--\n\nReturn a version of the string suitable for caseless comparisons."),
            ("title", NOARGS, m_title, "title($self, /)\n--\n\nReturn a version of the string where each word is titlecased.\n\nMore specifically, words start with uppercased characters and all remaining\ncased characters have lower case."),
            ("center", FAST, m_center, "center($self, width, fillchar=' ', /)\n--\n\nReturn a centered string of length width.\n\nPadding is done using the specified fill character (default is a space)."),
            ("count", FAST, m_count, "count($self, sub[, start[, end]], /)\n--\n\nReturn the number of non-overlapping occurrences of substring sub in string S[start:end].\n\nOptional arguments start and end are interpreted as in slice notation."),
            ("expandtabs", FASTKW, m_expandtabs, "expandtabs($self, /, tabsize=8)\n--\n\nReturn a copy where all tab characters are expanded using spaces.\n\nIf tabsize is not given, a tab size of 8 characters is assumed."),
            ("find", FAST, m_find, "find($self, sub[, start[, end]], /)\n--\n\nReturn the lowest index in S where substring sub is found, such that sub is contained within S[start:end].\n\nOptional arguments start and end are interpreted as in slice notation.\nReturn -1 on failure."),
            ("partition", O, m_partition, "partition($self, sep, /)\n--\n\nPartition the string into three parts using the given separator.\n\nThis will search for the separator in the string.  If the separator is found,\nreturns a 3-tuple containing the part before the separator, the separator\nitself, and the part after it.\n\nIf the separator is not found, returns a 3-tuple containing the original string\nand two empty strings."),
            ("index", FAST, m_index, "index($self, sub[, start[, end]], /)\n--\n\nReturn the lowest index in S where substring sub is found, such that sub is contained within S[start:end].\n\nOptional arguments start and end are interpreted as in slice notation.\nRaises ValueError when the substring is not found."),
            ("ljust", FAST, m_ljust, "ljust($self, width, fillchar=' ', /)\n--\n\nReturn a left-justified string of length width.\n\nPadding is done using the specified fill character (default is a space)."),
            ("lower", NOARGS, m_lower, "lower($self, /)\n--\n\nReturn a copy of the string converted to lowercase."),
            ("lstrip", FAST, m_lstrip, "lstrip($self, chars=None, /)\n--\n\nReturn a copy of the string with leading whitespace removed.\n\nIf chars is given and not None, remove characters in chars instead."),
            ("rfind", FAST, m_rfind, "rfind($self, sub[, start[, end]], /)\n--\n\nReturn the highest index in S where substring sub is found, such that sub is contained within S[start:end].\n\nOptional arguments start and end are interpreted as in slice notation.\nReturn -1 on failure."),
            ("rindex", FAST, m_rindex, "rindex($self, sub[, start[, end]], /)\n--\n\nReturn the highest index in S where substring sub is found, such that sub is contained within S[start:end].\n\nOptional arguments start and end are interpreted as in slice notation.\nRaises ValueError when the substring is not found."),
            ("rjust", FAST, m_rjust, "rjust($self, width, fillchar=' ', /)\n--\n\nReturn a right-justified string of length width.\n\nPadding is done using the specified fill character (default is a space)."),
            ("rstrip", FAST, m_rstrip, "rstrip($self, chars=None, /)\n--\n\nReturn a copy of the string with trailing whitespace removed.\n\nIf chars is given and not None, remove characters in chars instead."),
            ("rpartition", O, m_rpartition, "rpartition($self, sep, /)\n--\n\nPartition the string into three parts using the given separator.\n\nThis will search for the separator in the string, starting at the end. If\nthe separator is found, returns a 3-tuple containing the part before the\nseparator, the separator itself, and the part after it.\n\nIf the separator is not found, returns a 3-tuple containing two empty strings\nand the original string."),
            ("splitlines", FASTKW, m_splitlines, "splitlines($self, /, keepends=False)\n--\n\nReturn a list of the lines in the string, breaking at line boundaries.\n\nLine breaks are not included in the resulting list unless keepends is given and\ntrue."),
            ("strip", FAST, m_strip, "strip($self, chars=None, /)\n--\n\nReturn a copy of the string with leading and trailing whitespace removed.\n\nIf chars is given and not None, remove characters in chars instead."),
            ("swapcase", NOARGS, m_swapcase, "swapcase($self, /)\n--\n\nConvert uppercase characters to lowercase and lowercase characters to uppercase."),
            ("translate", O, m_translate, "translate($self, table, /)\n--\n\nReplace each character in the string using the given translation table.\n\n  table\n    Translation table, which must be a mapping of Unicode ordinals to\n    Unicode ordinals, strings, or None.\n\nThe table must implement lookup/indexing via __getitem__, for instance a\ndictionary or list.  If this operation raises LookupError, the character is\nleft untouched.  Characters mapped to None are deleted."),
            ("upper", NOARGS, m_upper, "upper($self, /)\n--\n\nReturn a copy of the string converted to uppercase."),
            ("startswith", FAST, m_startswith, "startswith($self, prefix[, start[, end]], /)\n--\n\nReturn True if the string starts with the specified prefix, False otherwise.\n\n  prefix\n    A string or a tuple of strings to try.\n  start\n    Optional start position. Default: start of the string.\n  end\n    Optional stop position. Default: end of the string."),
            ("endswith", FAST, m_endswith, "endswith($self, suffix[, start[, end]], /)\n--\n\nReturn True if the string ends with the specified suffix, False otherwise.\n\n  suffix\n    A string or a tuple of strings to try.\n  start\n    Optional start position. Default: start of the string.\n  end\n    Optional stop position. Default: end of the string."),
            ("removeprefix", O, m_removeprefix, "removeprefix($self, prefix, /)\n--\n\nReturn a str with the given prefix string removed if present.\n\nIf the string starts with the prefix string, return string[len(prefix):].\nOtherwise, return a copy of the original string."),
            ("removesuffix", O, m_removesuffix, "removesuffix($self, suffix, /)\n--\n\nReturn a str with the given suffix string removed if present.\n\nIf the string ends with the suffix string and that suffix is not empty,\nreturn string[:-len(suffix)]. Otherwise, return a copy of the original\nstring."),
            ("isascii", NOARGS, m_isascii, "isascii($self, /)\n--\n\nReturn True if all characters in the string are ASCII, False otherwise.\n\nASCII characters have code points in the range U+0000-U+007F.\nEmpty string is ASCII too."),
            ("islower", NOARGS, m_islower, "islower($self, /)\n--\n\nReturn True if the string is a lowercase string, False otherwise.\n\nA string is lowercase if all cased characters in the string are lowercase and\nthere is at least one cased character in the string."),
            ("isupper", NOARGS, m_isupper, "isupper($self, /)\n--\n\nReturn True if the string is an uppercase string, False otherwise.\n\nA string is uppercase if all cased characters in the string are uppercase and\nthere is at least one cased character in the string."),
            ("istitle", NOARGS, m_istitle, "istitle($self, /)\n--\n\nReturn True if the string is a title-cased string, False otherwise.\n\nIn a title-cased string, upper- and title-case characters may only\nfollow uncased characters and lowercase characters only cased ones."),
            ("isspace", NOARGS, m_isspace, "isspace($self, /)\n--\n\nReturn True if the string is a whitespace string, False otherwise.\n\nA string is whitespace if all characters in the string are whitespace and there\nis at least one character in the string."),
            ("isdecimal", NOARGS, m_isdecimal, "isdecimal($self, /)\n--\n\nReturn True if the string is a decimal string, False otherwise.\n\nA string is a decimal string if all characters in the string are decimal and\nthere is at least one character in the string."),
            ("isdigit", NOARGS, m_isdigit, "isdigit($self, /)\n--\n\nReturn True if the string is a digit string, False otherwise.\n\nA string is a digit string if all characters in the string are digits and there\nis at least one character in the string."),
            ("isnumeric", NOARGS, m_isnumeric, "isnumeric($self, /)\n--\n\nReturn True if the string is a numeric string, False otherwise.\n\nA string is numeric if all characters in the string are numeric and there is at\nleast one character in the string."),
            ("isalpha", NOARGS, m_isalpha, "isalpha($self, /)\n--\n\nReturn True if the string is an alphabetic string, False otherwise.\n\nA string is alphabetic if all characters in the string are alphabetic and there\nis at least one character in the string."),
            ("isalnum", NOARGS, m_isalnum, "isalnum($self, /)\n--\n\nReturn True if the string is an alpha-numeric string, False otherwise.\n\nA string is alpha-numeric if all characters in the string are alpha-numeric and\nthere is at least one character in the string."),
            ("isidentifier", NOARGS, m_isidentifier, "isidentifier($self, /)\n--\n\nReturn True if the string is a valid Python identifier, False otherwise.\n\nCall keyword.iskeyword(s) to test whether string s is a reserved identifier,\nsuch as \"def\" or \"class\"."),
            ("isprintable", NOARGS, m_isprintable, "isprintable($self, /)\n--\n\nReturn True if all characters in the string are printable, False otherwise.\n\nA character is printable if repr() may use it in its output."),
            ("zfill", O, m_zfill, "zfill($self, width, /)\n--\n\nPad a numeric string with zeros on the left, to fill a field of the given width.\n\nThe string is never truncated."),
            ("format", FASTKW, m_format, "format($self, /, *args, **kwargs)\n--\n\nReturn a formatted version of the string, using substitutions from args and kwargs.\nThe substitutions are identified by braces ('{' and '}')."),
            ("format_map", O, m_format_map, "format_map($self, mapping, /)\n--\n\nReturn a formatted version of the string, using substitutions from mapping.\nThe substitutions are identified by braces ('{' and '}')."),
            ("__format__", O, m_dunder_format, "__format__($self, format_spec, /)\n--\n\nReturn a formatted version of the string as described by format_spec."),
            ("maketrans", FAST, m_maketrans, "maketrans(x, y=<unrepresentable>, z=<unrepresentable>, /)\n--\n\nReturn a translation table usable for str.translate().\n\nIf there is only one argument, it must be a dictionary mapping Unicode\nordinals (integers) or characters to Unicode ordinals, strings or None.\nCharacter keys will be then converted to ordinals.\nIf there are two arguments, they must be strings of equal length, and\nin the resulting dictionary, each character in x will be mapped to the\ncharacter at the same position in y. If there is a third argument, it\nmust be a string, whose characters will be mapped to None in the result."; METH_STATIC),
            ("__sizeof__", NOARGS, m_sizeof, "__sizeof__($self, /)\n--\n\nReturn the size of the string in memory, in bytes."),
            ("__getnewargs__", NOARGS, m_getnewargs),
        ];
        // maketrans is a static method: self is NULL, so it needs its own C shim.
        let mut p = t.tp_methods;
        while !(*p).ml_name.is_null() {
            if std::ffi::CStr::from_ptr((*p).ml_name).to_bytes() == b"maketrans" {
                (*p).ml_meth = maketrans_c as *const std::ffi::c_void;
                (*p).ml_flags = METH_FASTCALL | METH_STATIC;
            }
            p = p.add(1);
        }

        for (tp, nm) in [(&raw mut STR_ASCII_ITER_TYPE, c"str_ascii_iterator"), (&raw mut STR_ITER_TYPE, c"str_iterator")] {
            let it = &mut *tp;
            it.tp_name = nm.as_ptr();
            it.tp_basicsize = std::mem::size_of::<StrIter>() as isize;
            it.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
            it.tp_dealloc = Some(str_iter_dealloc);
            it.tp_traverse = Some(str_iter_traverse);
            it.tp_iter = Some(self_iter);
            it.tp_iternext = Some(str_iter_next);
            it.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
            it.tp_methods = crate::mdefs![
                ("__length_hint__", NOARGS, str_iter_length_hint, "Private method returning an estimate of len(list(it))."),
                ("__reduce__", NOARGS, str_iter_reduce, "Return state information for pickling."),
                ("__setstate__", O, str_iter_setstate, "Set state information for unpickling."),
            ];
        }
    }
}

pub fn ready_types() -> PyResult<()> {
    crate::types::typeobject::type_ready(&raw mut STR_ASCII_ITER_TYPE)?;
    crate::types::typeobject::type_ready(&raw mut STR_ITER_TYPE)?;
    Ok(())
}

#[allow(dead_code)]
fn _unit_check<T: Unit>() {}
#[allow(dead_code)]
fn _asunits_check(d: StrData) -> Option<usize> {
    d.units::<u8>().map(|x| x.len())
}
