//! unicodeobject.h: PEP 393 strings, codecs entry points, PyUnicodeWriter.

use super::*;
use crate::abstract_ as ab;
use crate::modules::codecs_core;
use crate::types::str::{self as pystr, StrBuilder};
use crate::types::{bytes, int, list, none, tuple};
use std::ffi::VaList;

pub type Py_UCS4 = u32;

// ---------------------------------------------------------------- helpers

pub(crate) fn decode_utf8(b: &[u8], errors: Option<&str>) -> PyResult {
    codecs_core::decode_utf8(b, errors.unwrap_or("strict"))
}

/// Decode a file system path (UTF-8 + surrogateescape).
pub(crate) fn decode_fs(b: &[u8]) -> PyResult {
    codecs_core::decode_utf8(b, "surrogateescape")
}

/// Encode str to file-system bytes (UTF-8 + surrogateescape).
pub(crate) fn encode_fs(s: &PyObject) -> PyResult {
    codecs_core::encode_str(s, None, Some(&pystr::new_str("surrogateescape")))
}

unsafe fn ustr<'a>(o: *mut PyObject) -> PyResult<&'a PyObject> {
    unsafe {
        if o.is_null() || !pystr::is_str(&*o) {
            let _ = type_error("bad argument type for built-in operation");
            return Err(PyErr);
        }
        Ok(&*o)
    }
}

unsafe fn opt_cstr(p: *const c_char) -> Option<String> {
    if p.is_null() { None } else { Some(unsafe { cs(p).into_owned() }) }
}

fn chars(o: &PyObject) -> Vec<u32> {
    pystr::str_data(o).to_vec()
}

fn substr(o: &PyObject, start: usize, end: usize) -> PyRef {
    let d = pystr::str_data(o);
    let v: Vec<u32> = (start..end).map(|i| d.get(i)).collect();
    pystr::from_codepoints(&v)
}

fn adjust_indices(start: isize, end: isize, len: isize) -> (usize, usize) {
    let mut s = start;
    let mut e = end;
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
    (s as usize, e.max(0) as usize)
}

// ---------------------------------------------------------------- creation

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_New(size: Py_ssize_t, maxchar: Py_UCS4) -> *mut PyObject {
    if size < 0 {
        let _ = system_error("Negative size passed to PyUnicode_New");
        return std::ptr::null_mut();
    }
    if maxchar > 0x10ffff {
        let _ = system_error("invalid maximum character passed to PyUnicode_New");
        return std::ptr::null_mut();
    }
    if size == 0 {
        return pystr::new_str("").into_raw();
    }
    match pystr::try_alloc_str(size as usize, maxchar) {
        Some(s) => s.into_raw(),
        None => {
            let _ = memory_error();
            std::ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromStringAndSize(u: *const c_char, size: Py_ssize_t) -> *mut PyObject {
    unsafe {
        if size < 0 {
            let _ = system_error("Negative size passed to PyUnicode_FromStringAndSize");
            return std::ptr::null_mut();
        }
        if u.is_null() {
            if size == 0 {
                return pystr::new_str("").into_raw();
            }
            let _ = system_error("NULL string with positive size with NULL passed to PyUnicode_FromStringAndSize");
            return std::ptr::null_mut();
        }
        if size as usize > (isize::MAX as usize) / 4 {
            // no str of that length can be allocated (and the data cannot be that long)
            let _ = memory_error();
            return std::ptr::null_mut();
        }
        ret(decode_utf8(std::slice::from_raw_parts(u as *const u8, size as usize), None))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromString(u: *const c_char) -> *mut PyObject {
    unsafe { ret(decode_utf8(CStr::from_ptr(u).to_bytes(), None)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromKindAndData(kind: c_int, buffer: *const std::ffi::c_void, size: Py_ssize_t) -> *mut PyObject {
    unsafe {
        if size < 0 {
            let _ = value_error("size must be positive");
            return std::ptr::null_mut();
        }
        let n = size as usize;
        match kind {
            1 => pystr::from_latin1(std::slice::from_raw_parts(buffer as *const u8, n)).into_raw(),
            2 => pystr::from_ucs2(std::slice::from_raw_parts(buffer as *const u16, n)).into_raw(),
            4 => {
                let s = std::slice::from_raw_parts(buffer as *const u32, n);
                if let Some(&bad) = s.iter().find(|&&c| c > 0x10ffff) {
                    let _ = value_error(&format!("character U+{:x} is not in range [U+0000; U+10ffff]", bad));
                    return std::ptr::null_mut();
                }
                pystr::from_codepoints(s).into_raw()
            }
            _ => {
                let _ = system_error("invalid kind");
                std::ptr::null_mut()
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromWideChar(w: *const libc::wchar_t, size: Py_ssize_t) -> *mut PyObject {
    unsafe {
        if w.is_null() && size != 0 {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        let n = if size == -1 { libc::wcslen(w) } else if size < 0 {
            let _ = bad_internal();
            return std::ptr::null_mut();
        } else {
            size as usize
        };
        if n == 0 {
            return pystr::new_str("").into_raw();
        }
        let s = std::slice::from_raw_parts(w as *const u32, n);
        if let Some(&bad) = s.iter().find(|&&c| c > 0x10ffff) {
            let _ = value_error(&format!("character U+{:x} is not in range [U+0000; U+10ffff]", bad));
            return std::ptr::null_mut();
        }
        pystr::from_codepoints(s).into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromOrdinal(c: c_int) -> *mut PyObject {
    if !(0..=0x10ffff).contains(&c) {
        let _ = value_error("chr() arg not in range(0x110000)");
        return std::ptr::null_mut();
    }
    pystr::from_codepoints(&[c as u32]).into_raw()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromObject(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        if pystr::is_str_exact(&*o) {
            return (*o).to_ref().into_raw();
        }
        if pystr::is_str(&*o) {
            return pystr::from_data(pystr::str_data(&*o)).into_raw();
        }
        let _ = type_error(&format!("Can't convert '{}' object to str implicitly", tname(&*o)));
        std::ptr::null_mut()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromEncodedObject(o: *mut PyObject, encoding: *const c_char, errors: *const c_char) -> *mut PyObject {
    unsafe {
        if o.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        if pystr::is_str(&*o) {
            let _ = type_error("decoding str is not supported");
            return std::ptr::null_mut();
        }
        let Some(data) = crate::buffer::buffer_bytes(&*o).or_else(|| bytes::bytes_like_data(&*o)) else {
            let _ = type_error(&format!("decoding to str: need a bytes-like object, {} found", tname(&*o)));
            return std::ptr::null_mut();
        };
        decode_impl(&data, encoding, errors)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromFormatV(fmt: *const c_char, mut ap: VaList) -> *mut PyObject {
    unsafe { ret(super::printf::format_v(fmt, &mut ap)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FromFormat(fmt: *const c_char, mut args: ...) -> *mut PyObject {
    unsafe { ret(super::printf::format_v(fmt, &mut args)) }
}

// ---------------------------------------------------------------- PEP 393 accessors

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DATA(o: *mut PyObject) -> *mut std::ffi::c_void {
    unsafe { pystr::str_data_ptr(&*o) as *mut std::ffi::c_void }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_KIND(o: *mut PyObject) -> c_int {
    unsafe { pystr::str_kind(&*o) as c_int }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_GetLength(o: *mut PyObject) -> Py_ssize_t {
    unsafe {
        match ustr(o) {
            Ok(s) => pystr::str_len(s) as Py_ssize_t,
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_ReadChar(o: *mut PyObject, i: Py_ssize_t) -> Py_UCS4 {
    unsafe {
        let Ok(s) = ustr(o) else { return u32::MAX };
        if i < 0 || i as usize >= pystr::str_len(s) {
            let _ = crate::err!(IndexError, "string index out of range");
            return u32::MAX;
        }
        pystr::str_data(s).get(i as usize)
    }
}

fn modifiable(o: &PyObject) -> bool {
    o.refcnt() == 1 && unsafe { (*(o.as_ptr() as *mut PyASCIIObject)).hash } == -1 && !pystr::is_interned(o) && pystr::is_str_exact(o)
}

unsafe fn write_raw(o: &PyObject, i: usize, c: u32) {
    unsafe {
        let p = pystr::str_data_ptr(o);
        match pystr::str_kind(o) {
            1 => *p.add(i) = c as u8,
            2 => *(p as *mut u16).add(i) = c as u16,
            _ => *(p as *mut u32).add(i) = c,
        }
    }
}

fn kind_max(o: &PyObject) -> u32 {
    match pystr::str_kind(o) {
        1 => {
            if pystr::str_is_ascii(o) { 0x7f } else { 0xff }
        }
        2 => 0xffff,
        _ => 0x10ffff,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_WriteChar(o: *mut PyObject, i: Py_ssize_t, c: Py_UCS4) -> c_int {
    unsafe {
        let Ok(s) = ustr(o) else { return -1 };
        if i < 0 || i as usize >= pystr::str_len(s) {
            let _ = crate::err!(IndexError, "string index out of range");
            return -1;
        }
        if !modifiable(s) {
            let _ = system_error("Cannot modify a string currently used");
            return -1;
        }
        if c > kind_max(s) {
            let _ = value_error("character out of range");
            return -1;
        }
        write_raw(s, i as usize, c);
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Fill(o: *mut PyObject, start: Py_ssize_t, length: Py_ssize_t, c: Py_UCS4) -> Py_ssize_t {
    unsafe {
        let Ok(s) = ustr(o) else { return -1 };
        if start < 0 {
            let _ = crate::err!(IndexError, "string index out of range");
            return -1;
        }
        if c > kind_max(s) {
            let _ = value_error("fill character is bigger than the string maximum character");
            return -1;
        }
        if !modifiable(s) {
            let _ = system_error("Cannot modify a string currently used");
            return -1;
        }
        let len = pystr::str_len(s) as isize;
        let n = length.min(len - start).max(0);
        for k in 0..n {
            write_raw(s, (start + k) as usize, c);
        }
        n
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_CopyCharacters(
    to: *mut PyObject,
    to_start: Py_ssize_t,
    from: *mut PyObject,
    from_start: Py_ssize_t,
    how_many: Py_ssize_t,
) -> Py_ssize_t {
    unsafe {
        let (Ok(t), Ok(f)) = (ustr(to), ustr(from)) else {
            return -1;
        };
        let (tl, fl) = (pystr::str_len(t) as isize, pystr::str_len(f) as isize);
        if from_start < 0 || from_start > fl || to_start < 0 || to_start > tl {
            let _ = crate::err!(IndexError, "string index out of range");
            return -1;
        }
        if how_many < 0 {
            let _ = system_error("how_many cannot be negative");
            return -1;
        }
        let n = how_many.min(fl - from_start).min(tl - to_start);
        if n == 0 {
            return 0;
        }
        if !modifiable(t) {
            let _ = system_error("Cannot modify a string currently used");
            return -1;
        }
        let fd = pystr::str_data(f);
        let maxc = (from_start..from_start + n).map(|i| fd.get(i as usize)).max().unwrap_or(0);
        if maxc > kind_max(t) {
            let _ = system_error(&format!(
                "Cannot write {} characters at {} in a string of {} characters",
                n, to_start, tl
            ));
            return -1;
        }
        let tmp: Vec<u32> = (from_start..from_start + n).map(|i| fd.get(i as usize)).collect();
        for (k, c) in tmp.into_iter().enumerate() {
            write_raw(t, to_start as usize + k, c);
        }
        n
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Resize(p: *mut *mut PyObject, length: Py_ssize_t) -> c_int {
    unsafe {
        if p.is_null() || (*p).is_null() || !pystr::is_str(&**p) || length < 0 {
            let _ = bad_internal();
            return -1;
        }
        let old = &**p;
        let d = pystr::str_data(old);
        let n = length as usize;
        let mut v: Vec<u32> = (0..n.min(d.len())).map(|i| d.get(i)).collect();
        v.resize(n, 0);
        // keep the kind (callers may write wider characters afterwards via WriteChar)
        let new = if n == 0 { pystr::new_str("") } else { pystr::alloc_str(n, kind_max(old)) };
        for (i, c) in v.into_iter().enumerate() {
            write_raw(&new, i, c);
        }
        let newp = new.into_raw();
        decref(*p);
        *p = newp;
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Substring(o: *mut PyObject, start: Py_ssize_t, end: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let Ok(s) = ustr(o) else { return std::ptr::null_mut() };
        if start < 0 || end < 0 {
            let _ = crate::err!(IndexError, "string index out of range");
            return std::ptr::null_mut();
        }
        let len = pystr::str_len(s);
        let e = (end as usize).min(len);
        let st = (start as usize).min(e);
        if st == 0 && e == len && pystr::is_str_exact(s) {
            return s.to_ref().into_raw();
        }
        substr(s, st, e).into_raw()
    }
}

// ---------------------------------------------------------------- conversions

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsUTF8AndSize(o: *mut PyObject, size: *mut Py_ssize_t) -> *const c_char {
    unsafe {
        let s = match ustr(o) {
            Ok(s) => s,
            Err(_) => {
                if !size.is_null() {
                    *size = -1;
                }
                return std::ptr::null();
            }
        };
        match pystr::as_utf8(s) {
            Ok(u) => {
                if !size.is_null() {
                    *size = u.len() as Py_ssize_t;
                }
                u.as_ptr() as *const c_char
            }
            Err(_) => {
                if !size.is_null() {
                    *size = -1;
                }
                std::ptr::null()
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsUTF8(o: *mut PyObject) -> *const c_char {
    unsafe {
        let mut n: Py_ssize_t = 0;
        let p = PyUnicode_AsUTF8AndSize(o, &mut n);
        if !p.is_null() && libc::strlen(p) != n as usize {
            let _ = value_error("embedded null character");
            return std::ptr::null();
        }
        p
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsUCS4(o: *mut PyObject, buf: *mut Py_UCS4, buflen: Py_ssize_t, copy_null: c_int) -> *mut Py_UCS4 {
    unsafe {
        let Ok(s) = ustr(o) else { return std::ptr::null_mut() };
        if buf.is_null() || buflen < 0 {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        let d = pystr::str_data(s);
        let need = d.len() + (copy_null != 0) as usize;
        if (buflen as usize) < need {
            let _ = system_error(&format!("string is longer than the buffer"));
            if copy_null != 0 && buflen > 0 {
                *buf = 0;
            }
            return std::ptr::null_mut();
        }
        for i in 0..d.len() {
            *buf.add(i) = d.get(i);
        }
        if copy_null != 0 {
            *buf.add(d.len()) = 0;
        }
        buf
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsUCS4Copy(o: *mut PyObject) -> *mut Py_UCS4 {
    unsafe {
        let Ok(s) = ustr(o) else { return std::ptr::null_mut() };
        let n = pystr::str_len(s) + 1;
        let buf = crate::capi::mem::PyMem_Malloc(n * 4) as *mut u32;
        if buf.is_null() {
            let _ = memory_error();
            return buf;
        }
        PyUnicode_AsUCS4(o, buf, n as isize, 1)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsWideChar(o: *mut PyObject, w: *mut libc::wchar_t, size: Py_ssize_t) -> Py_ssize_t {
    unsafe {
        let Ok(s) = ustr(o) else { return -1 };
        let d = pystr::str_data(s);
        if w.is_null() {
            return d.len() as Py_ssize_t + 1;
        }
        let n = (size.max(0) as usize).min(d.len() + 1);
        for i in 0..n {
            *w.add(i) = if i < d.len() { d.get(i) as libc::wchar_t } else { 0 };
        }
        if n > d.len() { d.len() as Py_ssize_t } else { n as Py_ssize_t }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsWideCharString(o: *mut PyObject, size: *mut Py_ssize_t) -> *mut libc::wchar_t {
    unsafe {
        let Ok(s) = ustr(o) else { return std::ptr::null_mut() };
        let d = pystr::str_data(s);
        if size.is_null() && d.iter().any(|c| c == 0) {
            let _ = value_error("embedded null character");
            return std::ptr::null_mut();
        }
        let buf = crate::capi::mem::PyMem_Malloc((d.len() + 1) * 4) as *mut libc::wchar_t;
        if buf.is_null() {
            let _ = memory_error();
            return buf;
        }
        for i in 0..d.len() {
            *buf.add(i) = d.get(i) as libc::wchar_t;
        }
        *buf.add(d.len()) = 0;
        if !size.is_null() {
            *size = d.len() as Py_ssize_t;
        }
        buf
    }
}

unsafe fn decode_impl(data: &[u8], encoding: *const c_char, errors: *const c_char) -> *mut PyObject {
    unsafe {
        let enc = opt_cstr(encoding).unwrap_or_else(|| "utf-8".into());
        let err = opt_cstr(errors).unwrap_or_else(|| "strict".into());
        ret(codecs_core::decode_bytes(data, &enc, &err))
    }
}
unsafe fn encode_impl(o: &PyObject, encoding: &str, errors: *const c_char) -> *mut PyObject {
    unsafe {
        let e = pystr::new_str(encoding);
        let er = opt_cstr(errors).map(|s| pystr::new_str(&s));
        ret(codecs_core::encode_str(o, Some(&e), er.as_deref()))
    }
}
unsafe fn raw<'a>(s: *const c_char, size: Py_ssize_t) -> &'a [u8] {
    unsafe { if size <= 0 { &[] } else { std::slice::from_raw_parts(s as *const u8, size as usize) } }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Decode(s: *const c_char, size: Py_ssize_t, encoding: *const c_char, errors: *const c_char) -> *mut PyObject {
    unsafe { decode_impl(raw(s, size), encoding, errors) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeUTF8(s: *const c_char, size: Py_ssize_t, errors: *const c_char) -> *mut PyObject {
    unsafe {
        let err = opt_cstr(errors);
        ret(decode_utf8(raw(s, size), err.as_deref()))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeUTF8Stateful(s: *const c_char, size: Py_ssize_t, errors: *const c_char, consumed: *mut Py_ssize_t) -> *mut PyObject {
    unsafe {
        let mut data = raw(s, size);
        if !consumed.is_null() {
            // leave an incomplete trailing sequence undecoded
            let cut = incomplete_utf8_tail(data);
            data = &data[..data.len() - cut];
        }
        let err = opt_cstr(errors);
        let r = decode_utf8(data, err.as_deref());
        // *consumed is written only when decoding succeeds
        if r.is_ok() && !consumed.is_null() {
            *consumed = data.len() as Py_ssize_t;
        }
        ret(r)
    }
}
/// Number of bytes at the end of `b` that form an incomplete (but so far valid) UTF-8 sequence.
pub(crate) fn incomplete_utf8_tail(b: &[u8]) -> usize {
    for back in 1..=3.min(b.len()) {
        let c = b[b.len() - back];
        if c & 0xc0 == 0x80 {
            continue;
        }
        let need = if c >= 0xf0 && c <= 0xf4 { 4 } else if c >= 0xe0 { 3 } else if c >= 0xc2 && c < 0xe0 { 2 } else { return 0 };
        if need > back && std::str::from_utf8(&b[b.len() - back..]).err().map_or(false, |e| e.error_len().is_none()) {
            return back;
        }
        return 0;
    }
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeLatin1(s: *const c_char, size: Py_ssize_t, _errors: *const c_char) -> *mut PyObject {
    unsafe { pystr::from_latin1(raw(s, size)).into_raw() }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeASCII(s: *const c_char, size: Py_ssize_t, errors: *const c_char) -> *mut PyObject {
    unsafe {
        let err = opt_cstr(errors).unwrap_or_else(|| "strict".into());
        ret(codecs_core::decode_bytes(raw(s, size), "ascii", &err))
    }
}
macro_rules! decode_named {
    ($fn:ident, $enc:expr) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $fn(s: *const c_char, size: Py_ssize_t, errors: *const c_char) -> *mut PyObject {
            unsafe {
                let err = opt_cstr(errors).unwrap_or_else(|| "strict".into());
                ret(codecs_core::decode_bytes(raw(s, size), $enc, &err))
            }
        }
    };
}
decode_named!(PyUnicode_DecodeUnicodeEscape, "unicode_escape");
decode_named!(PyUnicode_DecodeRawUnicodeEscape, "raw_unicode_escape");

unsafe fn decode_stateful_via_codecs(name: &str, data: &[u8], errors: *const c_char, extra: &[PyRef], consumed: *mut Py_ssize_t) -> *mut PyObject {
    unsafe {
        ret((|| -> PyResult {
            let codecs = crate::import::import_absolute("_codecs")?;
            let f = ab::getattr_str(&codecs, name)?;
            let b = bytes::new_bytes(data);
            let err = match opt_cstr(errors) {
                Some(e) => pystr::new_str(&e),
                None => pystr::new_str("strict"),
            };
            let fin = crate::types::bool_::new_bool(consumed.is_null());
            let mut args: Vec<&PyObject> = vec![&b, &err];
            for e in extra {
                args.push(e);
            }
            args.push(&fin);
            let r = ab::call(&f, &args)?;
            let items = tuple::tuple_items(&r);
            if !consumed.is_null() {
                *consumed = int::as_i64(items[1]).unwrap_or(0) as Py_ssize_t;
            }
            Ok(items[0].to_ref())
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeUTF7Stateful(s: *const c_char, size: Py_ssize_t, errors: *const c_char, consumed: *mut Py_ssize_t) -> *mut PyObject {
    unsafe { decode_stateful_via_codecs("utf_7_decode", raw(s, size), errors, &[], consumed) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeUTF7(s: *const c_char, size: Py_ssize_t, errors: *const c_char) -> *mut PyObject {
    unsafe { PyUnicode_DecodeUTF7Stateful(s, size, errors, std::ptr::null_mut()) }
}
unsafe fn decode_utf_16_32(name: &str, s: *const c_char, size: Py_ssize_t, errors: *const c_char, byteorder: *mut c_int, consumed: *mut Py_ssize_t) -> *mut PyObject {
    unsafe {
        let bo = if byteorder.is_null() { 0 } else { *byteorder };
        let r = decode_stateful_via_codecs(&format!("{}_ex_decode", name), raw(s, size), errors, &[int::new_int(bo as i64)], consumed);
        r
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeUTF16Stateful(s: *const c_char, size: Py_ssize_t, errors: *const c_char, bo: *mut c_int, consumed: *mut Py_ssize_t) -> *mut PyObject {
    unsafe { decode_utf_16_32("utf_16", s, size, errors, bo, consumed) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeUTF16(s: *const c_char, size: Py_ssize_t, errors: *const c_char, bo: *mut c_int) -> *mut PyObject {
    unsafe { decode_utf_16_32("utf_16", s, size, errors, bo, std::ptr::null_mut()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeUTF32Stateful(s: *const c_char, size: Py_ssize_t, errors: *const c_char, bo: *mut c_int, consumed: *mut Py_ssize_t) -> *mut PyObject {
    unsafe { decode_utf_16_32("utf_32", s, size, errors, bo, consumed) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeUTF32(s: *const c_char, size: Py_ssize_t, errors: *const c_char, bo: *mut c_int) -> *mut PyObject {
    unsafe { decode_utf_16_32("utf_32", s, size, errors, bo, std::ptr::null_mut()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeCharmap(s: *const c_char, size: Py_ssize_t, mapping: *mut PyObject, errors: *const c_char) -> *mut PyObject {
    unsafe {
        if mapping.is_null() {
            return PyUnicode_DecodeLatin1(s, size, errors);
        }
        ret((|| -> PyResult {
            let codecs = crate::import::import_absolute("_codecs")?;
            let f = ab::getattr_str(&codecs, "charmap_decode")?;
            let b = bytes::new_bytes(raw(s, size));
            let err = opt_cstr(errors).map(|e| pystr::new_str(&e)).unwrap_or_else(none::none_ref);
            let r = ab::call(&f, &[&b, &err, &*mapping])?;
            Ok(tuple::tuple_get(&r, 0).to_ref())
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsCharmapString(o: *mut PyObject, mapping: *mut PyObject) -> *mut PyObject {
    unsafe {
        let Ok(s) = ustr(o) else { return std::ptr::null_mut() };
        if mapping.is_null() {
            return encode_impl(s, "latin-1", std::ptr::null());
        }
        ret((|| -> PyResult {
            let codecs = crate::import::import_absolute("_codecs")?;
            let f = ab::getattr_str(&codecs, "charmap_encode")?;
            let r = ab::call(&f, &[s, none::none(), &*mapping])?;
            Ok(tuple::tuple_get(&r, 0).to_ref())
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_BuildEncodingMap(string: *mut PyObject) -> *mut PyObject {
    unsafe {
        ret((|| -> PyResult {
            let codecs = crate::import::import_absolute("_codecs")?;
            let f = ab::getattr_str(&codecs, "charmap_build")?;
            ab::call(&f, &[&*string])
        })())
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AsEncodedString(o: *mut PyObject, encoding: *const c_char, errors: *const c_char) -> *mut PyObject {
    unsafe {
        let Ok(s) = ustr(o) else { return std::ptr::null_mut() };
        let enc = opt_cstr(encoding).unwrap_or_else(|| "utf-8".into());
        encode_impl(s, &enc, errors)
    }
}
macro_rules! encode_named {
    ($fn:ident, $enc:expr) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $fn(o: *mut PyObject) -> *mut PyObject {
            unsafe {
                let Ok(s) = ustr(o) else { return std::ptr::null_mut() };
                encode_impl(s, $enc, std::ptr::null())
            }
        }
    };
}
encode_named!(PyUnicode_AsUTF8String, "utf-8");
encode_named!(PyUnicode_AsLatin1String, "latin-1");
encode_named!(PyUnicode_AsASCIIString, "ascii");
encode_named!(PyUnicode_AsUTF16String, "utf-16");
encode_named!(PyUnicode_AsUTF32String, "utf-32");
encode_named!(PyUnicode_AsUnicodeEscapeString, "unicode_escape");
encode_named!(PyUnicode_AsRawUnicodeEscapeString, "raw_unicode_escape");

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeFSDefaultAndSize(s: *const c_char, size: Py_ssize_t) -> *mut PyObject {
    unsafe { ret(decode_fs(raw(s, size))) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeFSDefault(s: *const c_char) -> *mut PyObject {
    unsafe { ret(decode_fs(CStr::from_ptr(s).to_bytes())) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_EncodeFSDefault(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let Ok(s) = ustr(o) else { return std::ptr::null_mut() };
        ret(encode_fs(s))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeLocaleAndSize(s: *const c_char, size: Py_ssize_t, errors: *const c_char) -> *mut PyObject {
    unsafe {
        let err = opt_cstr(errors).unwrap_or_else(|| "strict".into());
        let err = if err == "surrogateescape" || err == "strict" { err } else { "strict".into() };
        ret(codecs_core::decode_utf8(raw(s, size), &err))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_DecodeLocale(s: *const c_char, errors: *const c_char) -> *mut PyObject {
    unsafe { PyUnicode_DecodeLocaleAndSize(s, libc::strlen(s) as Py_ssize_t, errors) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_EncodeLocale(o: *mut PyObject, errors: *const c_char) -> *mut PyObject {
    unsafe {
        let Ok(s) = ustr(o) else { return std::ptr::null_mut() };
        encode_impl(s, "utf-8", errors)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_GetDefaultEncoding() -> *const c_char {
    c"utf-8".as_ptr()
}

/// PyUnicode_FSConverter: str/bytes/os.PathLike -> bytes (O& converter).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FSConverter(arg: *mut PyObject, addr: *mut std::ffi::c_void) -> c_int {
    unsafe {
        let out = addr as *mut *mut PyObject;
        if arg.is_null() {
            xdecref(*out);
            *out = std::ptr::null_mut();
            return 1;
        }
        let r = (|| -> PyResult {
            let p = crate::capi::misc::fspath(&*arg)?;
            let b = if pystr::is_str(&p) { encode_fs(&p)? } else { p };
            if !bytes::is_bytes(&b) {
                return Err(type_error("path should be string, bytes, or os.PathLike"));
            }
            if bytes::bytes_data(&b).contains(&0) {
                return Err(value_error("embedded null byte"));
            }
            Ok(b)
        })();
        match r {
            Ok(b) => {
                *out = b.into_raw();
                0x20000 // Py_CLEANUP_SUPPORTED
            }
            Err(_) => 0,
        }
    }
}
/// PyUnicode_FSDecoder: str/bytes/os.PathLike -> str (O& converter).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FSDecoder(arg: *mut PyObject, addr: *mut std::ffi::c_void) -> c_int {
    unsafe {
        let out = addr as *mut *mut PyObject;
        if arg.is_null() {
            xdecref(*out);
            *out = std::ptr::null_mut();
            return 1;
        }
        let r = (|| -> PyResult {
            let p = crate::capi::misc::fspath(&*arg)?;
            let s = if bytes::is_bytes(&p) { decode_fs(bytes::bytes_data(&p))? } else { p };
            if !pystr::is_str(&s) {
                return Err(type_error(&format!("path should be string, bytes, or os.PathLike, not {}", tname(&*arg))));
            }
            if pystr::str_data(&s).iter().any(|c| c == 0) {
                return Err(value_error("embedded null character"));
            }
            Ok(s)
        })();
        match r {
            Ok(s) => {
                *out = s.into_raw();
                0x20000
            }
            Err(_) => 0,
        }
    }
}

// ---------------------------------------------------------------- operations

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Concat(l: *mut PyObject, r: *mut PyObject) -> *mut PyObject {
    unsafe {
        if l.is_null() || r.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        if !pystr::is_str(&*l) || !pystr::is_str(&*r) {
            let bad = if !pystr::is_str(&*l) { l } else { r };
            let _ = type_error(&format!("can only concatenate str (not \"{}\") to str", tname(&*bad)));
            return std::ptr::null_mut();
        }
        pystr::concat(&*l, &*r).into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Append(p: *mut *mut PyObject, right: *mut PyObject) {
    unsafe {
        if p.is_null() {
            let _ = bad_internal();
            return;
        }
        let left = *p;
        if left.is_null() {
            return;
        }
        if right.is_null() || !pystr::is_str(&*left) || !pystr::is_str(&*right) {
            if !err_occurred() {
                let _ = bad_internal();
            }
            decref(left);
            *p = std::ptr::null_mut();
            return;
        }
        let r = pystr::concat(&*left, &*right).into_raw();
        decref(left);
        *p = r;
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_AppendAndDel(p: *mut *mut PyObject, right: *mut PyObject) {
    unsafe {
        PyUnicode_Append(p, right);
        xdecref(right);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Contains(container: *mut PyObject, element: *mut PyObject) -> c_int {
    unsafe {
        if !pystr::is_str(&*element) {
            let _ = type_error(&format!("'in <string>' requires string as left operand, not {}", tname(&*element)));
            return -1;
        }
        let Ok(c) = ustr(container) else { return -1 };
        ret_b(ab::contains(c, &*element))
    }
}
unsafe fn call_m(o: *mut PyObject, m: &str, args: &[&PyObject]) -> PyResult {
    unsafe {
        let s = ustr(o)?;
        ab::call_method_str(s, m, args)
    }
}
fn sep_or_none(p: *mut PyObject) -> &'static PyObject {
    unsafe { if p.is_null() { none::none() } else { &*p } }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Split(s: *mut PyObject, sep: *mut PyObject, maxsplit: Py_ssize_t) -> *mut PyObject {
    unsafe { ret(call_m(s, "split", &[sep_or_none(sep), &int::new_int(maxsplit as i64)])) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_RSplit(s: *mut PyObject, sep: *mut PyObject, maxsplit: Py_ssize_t) -> *mut PyObject {
    unsafe { ret(call_m(s, "rsplit", &[sep_or_none(sep), &int::new_int(maxsplit as i64)])) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Splitlines(s: *mut PyObject, keepends: c_int) -> *mut PyObject {
    unsafe { ret(call_m(s, "splitlines", &[&crate::types::bool_::new_bool(keepends != 0)])) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Partition(s: *mut PyObject, sep: *mut PyObject) -> *mut PyObject {
    unsafe { ret(call_m(s, "partition", &[sep_or_none(sep)])) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_RPartition(s: *mut PyObject, sep: *mut PyObject) -> *mut PyObject {
    unsafe { ret(call_m(s, "rpartition", &[sep_or_none(sep)])) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Join(sep: *mut PyObject, seq: *mut PyObject) -> *mut PyObject {
    unsafe {
        if seq.is_null() {
            return null_arg();
        }
        let sep_o = if sep.is_null() { pystr::new_str(" ") } else { (*sep).to_ref() };
        if !pystr::is_str(&sep_o) {
            let _ = type_error(&format!("separator: expected str instance, {} found", tname(&sep_o)));
            return std::ptr::null_mut();
        }
        ret(ab::call_method_str(&sep_o, "join", &[&*seq]))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Replace(s: *mut PyObject, old: *mut PyObject, new: *mut PyObject, maxcount: Py_ssize_t) -> *mut PyObject {
    unsafe {
        if ustr(old).is_err() || ustr(new).is_err() {
            return std::ptr::null_mut();
        }
        ret(call_m(s, "replace", &[&*old, &*new, &int::new_int(maxcount as i64)]))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Translate(s: *mut PyObject, table: *mut PyObject, errors: *const c_char) -> *mut PyObject {
    unsafe {
        let _ = errors;
        ret(call_m(s, "translate", &[&*table]))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Format(format: *mut PyObject, args: *mut PyObject) -> *mut PyObject {
    unsafe {
        let Ok(f) = ustr(format) else { return std::ptr::null_mut() };
        if args.is_null() {
            return null_arg();
        }
        ret(ab::binary_op(f, &*args, ab::NbSlot::Mod))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Count(s: *mut PyObject, sub: *mut PyObject, start: Py_ssize_t, end: Py_ssize_t) -> Py_ssize_t {
    unsafe {
        if ustr(sub).is_err() {
            return -1;
        }
        match call_m(s, "count", &[&*sub, &int::new_int(start as i64), &int::new_int(end as i64)]) {
            Ok(r) => int::as_i64(&r).unwrap_or(-1) as Py_ssize_t,
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Find(s: *mut PyObject, sub: *mut PyObject, start: Py_ssize_t, end: Py_ssize_t, direction: c_int) -> Py_ssize_t {
    unsafe {
        if ustr(sub).is_err() {
            return -2;
        }
        let m = if direction > 0 { "find" } else { "rfind" };
        match call_m(s, m, &[&*sub, &int::new_int(start as i64), &int::new_int(end as i64)]) {
            Ok(r) => int::as_i64(&r).unwrap_or(-2) as Py_ssize_t,
            Err(_) => -2,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_FindChar(s: *mut PyObject, ch: Py_UCS4, start: Py_ssize_t, end: Py_ssize_t, direction: c_int) -> Py_ssize_t {
    unsafe {
        let Ok(st) = ustr(s) else { return -2 };
        let d = pystr::str_data(st);
        let (a, b) = adjust_indices(start, end, d.len() as isize);
        if a >= b {
            return -1;
        }
        if direction > 0 {
            (a..b).find(|&i| d.get(i) == ch).map_or(-1, |i| i as isize)
        } else {
            (a..b).rev().find(|&i| d.get(i) == ch).map_or(-1, |i| i as isize)
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Tailmatch(s: *mut PyObject, sub: *mut PyObject, start: Py_ssize_t, end: Py_ssize_t, direction: c_int) -> Py_ssize_t {
    unsafe {
        let (Ok(st), Ok(su)) = (ustr(s), ustr(sub)) else { return -1 };
        let d = pystr::str_data(st);
        let sd = pystr::str_data(su);
        let (a, b) = adjust_indices(start, end, d.len() as isize);
        let n = sd.len();
        if b < a || b - a < n {
            return 0;
        }
        let off = if direction > 0 { b - n } else { a };
        (0..n).all(|i| d.get(off + i) == sd.get(i)) as Py_ssize_t
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Compare(l: *mut PyObject, r: *mut PyObject) -> c_int {
    unsafe {
        if pystr::is_str(&*l) && pystr::is_str(&*r) {
            return match pystr::str_cmp(&*l, &*r) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            };
        }
        let _ = type_error(&format!("Can't compare {} and {}", tname(&*l), tname(&*r)));
        -1
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_CompareWithASCIIString(u: *mut PyObject, s: *const c_char) -> c_int {
    unsafe {
        let d = pystr::str_data(&*u);
        let b = CStr::from_ptr(s).to_bytes();
        for i in 0..d.len().min(b.len()) {
            let (x, y) = (d.get(i), b[i] as u32);
            if x != y {
                return if x < y { -1 } else { 1 };
            }
        }
        match d.len().cmp(&b.len()) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_EqualToUTF8AndSize(u: *mut PyObject, s: *const c_char, size: Py_ssize_t) -> c_int {
    unsafe {
        if !pystr::is_str(&*u) {
            return 0;
        }
        let b = raw(s, size);
        let d = pystr::str_data(&*u);
        let Ok(st) = std::str::from_utf8(b) else { return 0 };
        let mut i = 0;
        for c in st.chars() {
            if i >= d.len() || d.get(i) != c as u32 {
                return 0;
            }
            i += 1;
        }
        (i == d.len()) as c_int
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_EqualToUTF8(u: *mut PyObject, s: *const c_char) -> c_int {
    unsafe { PyUnicode_EqualToUTF8AndSize(u, s, libc::strlen(s) as Py_ssize_t) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_Equal(a: *mut PyObject, b: *mut PyObject) -> c_int {
    unsafe {
        for o in [a, b] {
            if !pystr::is_str(&*o) {
                let _ = type_error(&format!("first argument must be str, not {}", tname(&*o)).replace(
                    "first",
                    if o == a { "first" } else { "second" },
                ));
                return -1;
            }
        }
        pystr::str_eq(&*a, &*b) as c_int
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_RichCompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        if !pystr::is_str(&*a) || !pystr::is_str(&*b) {
            return crate::types::none::not_implemented().to_ref().into_raw();
        }
        ret(ab::rich_compare(&*a, &*b, op))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_IsIdentifier(o: *mut PyObject) -> c_int {
    unsafe {
        let v = chars(&*o);
        rpy_unicode::is_identifier(&v) as c_int
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_InternInPlace(p: *mut *mut PyObject) {
    unsafe {
        if p.is_null() || (*p).is_null() || !pystr::is_str_exact(&**p) {
            return;
        }
        let s = PyRef::from_raw(*p);
        *p = pystr::intern(s).into_raw();
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicode_InternFromString(s: *const c_char) -> *mut PyObject {
    unsafe {
        match decode_utf8(CStr::from_ptr(s).to_bytes(), None) {
            Ok(o) => pystr::intern(o).into_raw(),
            Err(_) => std::ptr::null_mut(),
        }
    }
}

// ---------------------------------------------------------------- character database

#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_IsLowercase(c: Py_UCS4) -> c_int {
    rpy_unicode::is_lowercase(c) as c_int
}
#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_IsUppercase(c: Py_UCS4) -> c_int {
    rpy_unicode::is_uppercase(c) as c_int
}
#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_IsTitlecase(c: Py_UCS4) -> c_int {
    rpy_unicode::is_titlecase(c) as c_int
}
#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_IsWhitespace(c: Py_UCS4) -> c_int {
    rpy_unicode::is_space(c) as c_int
}
#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_IsLinebreak(c: Py_UCS4) -> c_int {
    rpy_unicode::is_linebreak(c) as c_int
}
#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_ToLowercase(c: Py_UCS4) -> Py_UCS4 {
    rpy_unicode::py_to_lowercase(c)
}
#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_ToUppercase(c: Py_UCS4) -> Py_UCS4 {
    rpy_unicode::py_to_uppercase(c)
}
#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_ToTitlecase(c: Py_UCS4) -> Py_UCS4 {
    rpy_unicode::py_to_titlecase(c)
}
#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_ToDecimalDigit(c: Py_UCS4) -> c_int {
    rpy_unicode::to_decimal(c).map_or(-1, |d| d as c_int)
}
#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_ToDigit(c: Py_UCS4) -> c_int {
    rpy_unicode::to_digit(c).map_or(-1, |d| d as c_int)
}
#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_ToNumeric(c: Py_UCS4) -> f64 {
    rpy_unicode::to_numeric(c).unwrap_or(-1.0)
}
#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_IsDecimalDigit(c: Py_UCS4) -> c_int {
    rpy_unicode::is_decimal(c) as c_int
}
#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_IsDigit(c: Py_UCS4) -> c_int {
    rpy_unicode::is_digit(c) as c_int
}
#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_IsNumeric(c: Py_UCS4) -> c_int {
    rpy_unicode::is_numeric(c) as c_int
}
#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_IsPrintable(c: Py_UCS4) -> c_int {
    rpy_unicode::is_printable(c) as c_int
}
#[unsafe(no_mangle)]
pub extern "C" fn _PyUnicode_IsAlpha(c: Py_UCS4) -> c_int {
    rpy_unicode::is_alpha(c) as c_int
}

// ---------------------------------------------------------------- PyUnicodeWriter (3.14)

/// The public PyUnicodeWriter. Its first word is a tag that tells the private
/// _PyUnicodeWriter_* functions (compat.rs) when C code hands them a public writer (CPython's
/// two writer APIs share one struct, and _testcapi relies on that).
#[repr(C)]
pub struct Writer {
    tag: usize,
    b: StrBuilder,
}
pub(crate) const PUBLIC_WRITER_TAG: usize = 0x7075_6277_7269_7465;
/// The public writer behind a private-writer pointer, if it is one.
pub(crate) unsafe fn as_public_writer(p: *mut std::ffi::c_void) -> Option<*mut Writer> {
    unsafe { if !p.is_null() && *(p as *const usize) == PUBLIC_WRITER_TAG { Some(p as *mut Writer) } else { None } }
}
/// Append chars to a public writer (the private API's byte-string writes).
pub(crate) unsafe fn public_writer_push(p: *mut Writer, chars: impl Iterator<Item = u32>) {
    unsafe {
        for c in chars {
            w(p).push(c);
        }
    }
}

unsafe fn w<'a>(p: *mut Writer) -> &'a mut StrBuilder {
    unsafe { &mut (*p).b }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicodeWriter_Create(length: Py_ssize_t) -> *mut Writer {
    if length < 0 {
        let _ = value_error("length must be positive");
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(Writer { tag: PUBLIC_WRITER_TAG, b: StrBuilder::with_capacity(length as usize) }))
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicodeWriter_Discard(wr: *mut Writer) {
    if !wr.is_null() {
        drop(unsafe { Box::from_raw(wr) });
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicodeWriter_Finish(wr: *mut Writer) -> *mut PyObject {
    unsafe {
        let b = Box::from_raw(wr);
        b.b.finish().into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicodeWriter_WriteChar(wr: *mut Writer, c: Py_UCS4) -> c_int {
    unsafe {
        if c > 0x10ffff {
            let _ = value_error("character must be in range(0x110000)");
            return -1;
        }
        w(wr).push(c);
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicodeWriter_WriteUTF8(wr: *mut Writer, s: *const c_char, size: Py_ssize_t) -> c_int {
    unsafe {
        let n = if size < 0 { libc::strlen(s) as Py_ssize_t } else { size };
        match decode_utf8(raw(s, n), None) {
            Ok(o) => {
                w(wr).push_obj(&o);
                0
            }
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicodeWriter_WriteASCII(wr: *mut Writer, s: *const c_char, size: Py_ssize_t) -> c_int {
    unsafe {
        let n = if size < 0 { libc::strlen(s) as Py_ssize_t } else { size };
        for &b in raw(s, n) {
            w(wr).push(b as u32);
        }
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicodeWriter_WriteWideChar(wr: *mut Writer, s: *const libc::wchar_t, size: Py_ssize_t) -> c_int {
    unsafe {
        let o = PyUnicode_FromWideChar(s, size);
        if o.is_null() {
            return -1;
        }
        let o = PyRef::from_raw(o);
        w(wr).push_obj(&o);
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicodeWriter_WriteUCS4(wr: *mut Writer, s: *const Py_UCS4, size: Py_ssize_t) -> c_int {
    unsafe {
        if size < 0 {
            let _ = value_error("size must be positive");
            return -1;
        }
        let sl = std::slice::from_raw_parts(s, size as usize);
        if let Some(&bad) = sl.iter().find(|&&c| c > 0x10ffff) {
            let _ = value_error(&format!("character U+{:x} is not in range [U+0000; U+10ffff]", bad));
            return -1;
        }
        for &c in sl {
            w(wr).push(c);
        }
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicodeWriter_WriteStr(wr: *mut Writer, o: *mut PyObject) -> c_int {
    unsafe {
        match ab::str(&*o) {
            Ok(s) => {
                w(wr).push_obj(&s);
                0
            }
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicodeWriter_WriteRepr(wr: *mut Writer, o: *mut PyObject) -> c_int {
    unsafe {
        let r = if o.is_null() { Ok(pystr::new_str("<NULL>")) } else { ab::repr(&*o) };
        match r {
            Ok(s) => {
                w(wr).push_obj(&s);
                0
            }
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicodeWriter_WriteSubstring(wr: *mut Writer, o: *mut PyObject, start: Py_ssize_t, end: Py_ssize_t) -> c_int {
    unsafe {
        let Ok(s) = ustr(o) else { return -1 };
        let len = pystr::str_len(s) as isize;
        if start < 0 || start > len || end < start || end > len {
            let _ = value_error("invalid start/end");
            return -1;
        }
        let d = pystr::str_data(s);
        for i in start..end {
            w(wr).push(d.get(i as usize));
        }
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicodeWriter_Format(wr: *mut Writer, fmt: *const c_char, mut args: ...) -> c_int {
    unsafe {
        match super::printf::format_v(fmt, &mut args) {
            Ok(s) => {
                w(wr).push_obj(&s);
                0
            }
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnicodeWriter_DecodeUTF8Stateful(
    wr: *mut Writer,
    s: *const c_char,
    size: Py_ssize_t,
    errors: *const c_char,
    consumed: *mut Py_ssize_t,
) -> c_int {
    unsafe {
        let o = PyUnicode_DecodeUTF8Stateful(s, size, errors, consumed);
        if o.is_null() {
            return -1;
        }
        let o = PyRef::from_raw(o);
        w(wr).push_obj(&o);
        0
    }
}

#[allow(dead_code)]
fn _unused(_: &PyObject) -> PyRef {
    list::new_list_empty()
}
