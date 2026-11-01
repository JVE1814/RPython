//! The printf-style engine behind PyUnicode_FromFormat(V), PyErr_Format(V) and friends.
//!
//! Supported: `%%`, `%c`, `%d %i %u %x %X %o` (length modifiers `l ll z t j`), `%p`,
//! `%s` (UTF-8, "replace" errors), `%U` (str), `%V` (str or UTF-8 fallback), `%S`
//! (str()), `%R` (repr()), `%A` (ascii()), `%T` (type of object, `#` uses ':'),
//! `%N` (type), `%ls`/`%lV` (wchar_t). Flags `-` and `0`, width and precision
//! (also `*`). Precision counts characters for strings, bytes for `%s`.

use super::*;
use crate::types::str::StrBuilder;
use crate::types::typeobject;
use std::ffi::VaList;

fn invalid(fmt: &[u8]) -> PyErr {
    system_error(&format!("invalid format string: {}", String::from_utf8_lossy(fmt)))
}

/// Decode at most `prec` bytes of a NUL-terminated UTF-8 string with the "replace"
/// error handler. An incomplete sequence cut off by the precision is dropped.
unsafe fn utf8_prefix(p: *const c_char, prec: Option<usize>) -> Vec<u32> {
    unsafe {
        let mut n = 0usize;
        let limit = prec.unwrap_or(usize::MAX);
        while n < limit && *p.add(n) != 0 {
            n += 1;
        }
        let truncated = n == limit && *p.add(n) != 0;
        let bytes = std::slice::from_raw_parts(p as *const u8, n);
        decode_utf8_replace(bytes, truncated)
    }
}

/// UTF-8 decode with U+FFFD replacement; if `partial`, an incomplete trailing
/// sequence is dropped instead of replaced.
pub fn decode_utf8_replace(bytes: &[u8], partial: bool) -> Vec<u32> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    loop {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                out.extend(s.chars().map(|c| c as u32));
                break;
            }
            Err(e) => {
                let (good, bad) = rest.split_at(e.valid_up_to());
                out.extend(unsafe { std::str::from_utf8_unchecked(good) }.chars().map(|c| c as u32));
                match e.error_len() {
                    Some(l) => {
                        out.push(0xfffd);
                        rest = &bad[l..];
                    }
                    None => {
                        if !partial {
                            out.push(0xfffd);
                        }
                        break;
                    }
                }
            }
        }
    }
    out
}

unsafe fn wchar_prefix(p: *const libc::wchar_t, prec: Option<usize>) -> Vec<u32> {
    unsafe {
        let mut v = Vec::new();
        let limit = prec.unwrap_or(usize::MAX);
        let mut i = 0;
        while v.len() < limit {
            let c = *p.add(i);
            if c == 0 {
                break;
            }
            v.push(c as u32);
            i += 1;
        }
        v
    }
}

fn push_padded(out: &mut StrBuilder, s: &[u32], width: Option<usize>, prec: Option<usize>, left: bool) {
    let s = match prec {
        Some(p) if p < s.len() => &s[..p],
        _ => s,
    };
    let pad = width.map_or(0, |w| w.saturating_sub(s.len()));
    if !left {
        for _ in 0..pad {
            out.push(b' ' as u32);
        }
    }
    for &c in s {
        out.push(c);
    }
    if left {
        for _ in 0..pad {
            out.push(b' ' as u32);
        }
    }
}

fn obj_chars(o: &PyObject) -> Vec<u32> {
    crate::types::str::str_data(o).to_vec()
}

/// The formatting engine. `next` supplies the C arguments.
pub unsafe fn format_v(fmt: *const c_char, ap: &mut VaList) -> PyResult {
    unsafe {
        let f = CStr::from_ptr(fmt).to_bytes();
        let mut out = StrBuilder::with_capacity(f.len());
        let mut i = 0;
        while i < f.len() {
            let c = f[i];
            if c >= 0x80 {
                return Err(value_error(&format!(
                    "PyUnicode_FromFormatV() expects an ASCII-encoded format string, got a non-ASCII byte: {:#x}",
                    c
                )));
            }
            if c != b'%' {
                out.push(c as u32);
                i += 1;
                continue;
            }
            let start = i;
            i += 1;
            if i < f.len() && f[i] == b'%' {
                out.push(b'%' as u32);
                i += 1;
                continue;
            }
            // flags
            let (mut left, mut zero, mut alt) = (false, false, false);
            while i < f.len() {
                match f[i] {
                    b'-' => left = true,
                    b'0' => zero = true,
                    b'#' => alt = true,
                    _ => break,
                }
                i += 1;
            }
            // width
            let mut width: Option<usize> = None;
            if i < f.len() && f[i] == b'*' {
                let w: c_int = ap.next_arg::<c_int>();
                if w < 0 {
                    left = true;
                }
                width = Some(w.unsigned_abs() as usize);
                i += 1;
            } else {
                while i < f.len() && f[i].is_ascii_digit() {
                    width = Some(width.unwrap_or(0) * 10 + (f[i] - b'0') as usize);
                    i += 1;
                }
            }
            // precision
            let mut prec: Option<usize> = None;
            if i < f.len() && f[i] == b'.' {
                i += 1;
                if i < f.len() && f[i] == b'*' {
                    let p: c_int = ap.next_arg::<c_int>();
                    prec = if p >= 0 { Some(p as usize) } else { None };
                    i += 1;
                } else {
                    let mut p = 0usize;
                    while i < f.len() && f[i].is_ascii_digit() {
                        p = p * 10 + (f[i] - b'0') as usize;
                        i += 1;
                    }
                    prec = Some(p);
                }
            }
            // length modifier
            let mut lm = 0u8; // 0 none, b'l', b'L' (ll), b'z', b't', b'j'
            if i < f.len() {
                match f[i] {
                    b'l' => {
                        if i + 1 < f.len() && f[i + 1] == b'l' {
                            lm = b'L';
                            i += 2;
                        } else {
                            lm = b'l';
                            i += 1;
                        }
                    }
                    b'z' | b't' | b'j' => {
                        lm = f[i];
                        i += 1;
                    }
                    _ => {}
                }
            }
            if i >= f.len() {
                return Err(invalid(&f[start..]));
            }
            let conv = f[i];
            i += 1;
            match conv {
                b'd' | b'i' | b'u' | b'x' | b'X' | b'o' => {
                    let signed = conv == b'd' || conv == b'i';
                    let (neg, mag): (bool, u64) = if signed {
                        let v: i64 = match lm {
                            0 => ap.next_arg::<c_int>() as i64,
                            b'l' | b'L' | b'j' => ap.next_arg::<i64>(),
                            _ => ap.next_arg::<isize>() as i64,
                        };
                        (v < 0, v.unsigned_abs())
                    } else {
                        let v: u64 = match lm {
                            0 => ap.next_arg::<std::ffi::c_uint>() as u64,
                            b'l' | b'L' | b'j' => ap.next_arg::<u64>(),
                            _ => ap.next_arg::<usize>() as u64,
                        };
                        (false, v)
                    };
                    let mut digits = match conv {
                        b'x' => format!("{:x}", mag),
                        b'X' => format!("{:X}", mag),
                        b'o' => format!("{:o}", mag),
                        _ => format!("{}", mag),
                    };
                    if let Some(p) = prec {
                        while digits.len() < p {
                            digits.insert(0, '0');
                        }
                    }
                    let sign = if neg { "-" } else { "" };
                    let len = digits.len() + sign.len();
                    let w = width.unwrap_or(0);
                    let s = if left {
                        format!("{}{}{}", sign, digits, " ".repeat(w.saturating_sub(len)))
                    } else if zero {
                        format!("{}{}{}", sign, "0".repeat(w.saturating_sub(len)), digits)
                    } else {
                        format!("{}{}{}", " ".repeat(w.saturating_sub(len)), sign, digits)
                    };
                    out.push_str(&s);
                }
                b'c' => {
                    if lm != 0 || width.is_some() || prec.is_some() {
                        return Err(invalid(&f[start..i]));
                    }
                    let v: c_int = ap.next_arg::<c_int>();
                    if !(0..=0x10ffff).contains(&v) {
                        return Err(overflow_error("character argument not in range(0x110000)"));
                    }
                    push_padded(&mut out, &[v as u32], width, None, left);
                }
                b'p' => {
                    let p: *const std::ffi::c_void = ap.next_arg::<*const std::ffi::c_void>();
                    let s: Vec<u32> = format!("{:#x}", p as usize).chars().map(|c| c as u32).collect();
                    push_padded(&mut out, &s, width, None, left);
                }
                b's' => {
                    if lm == b'l' {
                        let p: *const libc::wchar_t = ap.next_arg::<*const libc::wchar_t>();
                        let s = wchar_prefix(p, prec);
                        push_padded(&mut out, &s, width, None, left);
                    } else if lm != 0 {
                        return Err(invalid(&f[start..i]));
                    } else {
                        let p: *const c_char = ap.next_arg::<*const c_char>();
                        let s = utf8_prefix(p, prec);
                        push_padded(&mut out, &s, width, None, left);
                    }
                }
                b'U' | b'S' | b'R' | b'A' => {
                    if lm != 0 {
                        return Err(invalid(&f[start..i]));
                    }
                    let o: *mut PyObject = ap.next_arg::<*mut PyObject>();
                    if o.is_null() {
                        return Err(bad_internal());
                    }
                    let s = match conv {
                        b'U' => (*o).to_ref(),
                        b'S' => crate::abstract_::str(&*o)?,
                        b'R' => crate::abstract_::repr(&*o)?,
                        _ => crate::abstract_::ascii(&*o)?,
                    };
                    push_padded(&mut out, &obj_chars(&s), width, prec, left);
                }
                b'V' => {
                    let o: *mut PyObject = ap.next_arg::<*mut PyObject>();
                    if lm == b'l' {
                        let p: *const libc::wchar_t = ap.next_arg::<*const libc::wchar_t>();
                        if !o.is_null() {
                            push_padded(&mut out, &obj_chars(&*o), width, prec, left);
                        } else {
                            let s = wchar_prefix(p, prec);
                            push_padded(&mut out, &s, width, None, left);
                        }
                    } else if lm != 0 {
                        return Err(invalid(&f[start..i]));
                    } else {
                        let p: *const c_char = ap.next_arg::<*const c_char>();
                        if !o.is_null() {
                            push_padded(&mut out, &obj_chars(&*o), width, prec, left);
                        } else {
                            let s = utf8_prefix(p, prec);
                            push_padded(&mut out, &s, width, None, left);
                        }
                    }
                }
                b'T' | b'N' => {
                    if lm != 0 {
                        return Err(invalid(&f[start..i]));
                    }
                    let o: *mut PyObject = ap.next_arg::<*mut PyObject>();
                    let tp = if conv == b'T' {
                        (*o).type_ptr()
                    } else {
                        if !typeobject::is_type(&*o) {
                            return Err(type_error(&format!("%N argument must be a type; got {}", tname(&*o))));
                        }
                        o as *mut PyTypeObject
                    };
                    let q = typeobject::type_qualname(&*tp);
                    let s = match typeobject::type_module(tp) {
                        Some(m) if m != "builtins" && m != "__main__" => {
                            format!("{}{}{}", m, if alt { ":" } else { "." }, q)
                        }
                        _ => q,
                    };
                    let v: Vec<u32> = s.chars().map(|c| c as u32).collect();
                    push_padded(&mut out, &v, width, prec, left);
                }
                _ => return Err(invalid(&f[start..i])),
            }
        }
        Ok(out.finish())
    }
}

/// Format into a Rust String (used by PyOS_snprintf-style helpers and errors).
pub unsafe fn format_v_string(fmt: *const c_char, ap: &mut VaList) -> PyResult<String> {
    unsafe {
        let s = format_v(fmt, ap)?;
        Ok(crate::types::str::to_string_lossy(&s))
    }
}
