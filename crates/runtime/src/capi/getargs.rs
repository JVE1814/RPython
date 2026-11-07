//! modsupport.h argument parsing: PyArg_Parse*, PyArg_ParseTupleAndKeywords,
//! PyArg_ParseArray(AndKeywords) (3.15), _PyArg_ParseTupleAndKeywordsFast,
//! PyArg_UnpackTuple, PyArg_ValidateKeywordArguments. All format units.

use super::*;
use super::buffer::{PYBUF_WRITABLE, PyBuffer_Release, PyObject_GetBuffer};
use crate::abstract_ as ab;
use crate::types::{bytearray, bytes, complex, dict, float, int, list, none, str as pystr, tuple};
use std::ffi::{VaList, c_long, c_longlong, c_short, c_uint, c_ulong, c_ulonglong, c_ushort, c_void};

const CLEANUP_SUPPORTED: c_int = 0x20000;

enum Cleanup {
    Buffer(*mut Py_buffer),
    Mem(*mut *mut c_char),
    Converter(unsafe extern "C" fn(*mut PyObject, *mut c_void) -> c_int, *mut c_void),
}

/// Error from one conversion: either an exception is already set (Raised) or a
/// "must be X, not Y" message is to be decorated with the argument position.
enum ConvErr {
    Raised,
    Msg(String),
}
type CResult = Result<(), ConvErr>;

fn must_be(expected: &str, arg: &PyObject) -> ConvErr {
    let got = if none::is_none(arg) { "None".to_string() } else { tname(arg) };
    ConvErr::Msg(format!("must be {}, not {}", expected, got))
}

struct Ctx<'a, 'b> {
    ap: &'a mut VaList<'b>,
    cleanups: Vec<Cleanup>,
}

impl<'a, 'b> Ctx<'a, 'b> {
    unsafe fn ptr<T>(&mut self) -> *mut T {
        unsafe { self.ap.next_arg::<*mut c_void>() as *mut T }
    }
    unsafe fn cleanup(&mut self) {
        unsafe {
            for c in self.cleanups.drain(..).rev() {
                match c {
                    Cleanup::Buffer(b) => PyBuffer_Release(b),
                    Cleanup::Mem(p) => {
                        super::mem::PyMem_Free(*p as *mut c_void);
                        *p = std::ptr::null_mut();
                    }
                    Cleanup::Converter(f, addr) => {
                        f(std::ptr::null_mut(), addr);
                    }
                }
            }
        }
    }
}

/// Length of one format unit starting at `f[i]` (including modifiers), or None if invalid.
fn unit_len(f: &[u8], i: usize) -> Option<usize> {
    let c = *f.get(i)?;
    if c == b'(' {
        let mut level = 0;
        let mut j = i;
        while j < f.len() {
            match f[j] {
                b'(' => level += 1,
                b')' => {
                    level -= 1;
                    if level == 0 {
                        return Some(j - i + 1);
                    }
                }
                _ => {}
            }
            j += 1;
        }
        return None;
    }
    let next = f.get(i + 1).copied();
    let next2 = f.get(i + 2).copied();
    Some(match c {
        b'e' => match next {
            Some(b's') | Some(b't') => {
                if next2 == Some(b'#') || next2 == Some(b'*') {
                    3
                } else {
                    2
                }
            }
            _ => 2,
        },
        b'O' => match next {
            Some(b'!') | Some(b'&') | Some(b'#') | Some(b'*') => 2,
            _ => 1,
        },
        b's' | b'z' | b'y' | b'w' | b'u' | b'Z' => match next {
            Some(b'#') | Some(b'*') => 2,
            _ => 1,
        },
        _ => match next {
            Some(b'#') | Some(b'*') => 2,
            _ => 1,
        },
    })
}

/// Bad unit/modifier combination -> SystemError("impossible<bad format char>").
fn bad_format() -> ConvErr {
    let _ = system_error("impossible<bad format char>");
    ConvErr::Raised
}

/// For the read-only buffer formats (s*, y#, ...) an argument without any buffer
/// interface fails with the TypeError of PyObject_GetBuffer itself ("a bytes-like
/// object is required, not 'NoneType'"), which CPython leaves in place of the
/// generic "must be ..." message (pandas' ujson tests check it).  (wheels2)
unsafe fn no_buffer_raised(arg: &PyObject) -> bool {
    unsafe {
        if super::buffer::PyObject_CheckBuffer(arg.as_ptr()) != 0 {
            return false;
        }
        let mut view: Py_buffer = std::mem::zeroed();
        if PyObject_GetBuffer(arg.as_ptr(), &mut view, 0) == 0 {
            PyBuffer_Release(&mut view);
            return false;
        }
        true
    }
}

/// Err(true): the object exports a buffer, but not a C-contiguous one.
unsafe fn get_buffer(arg: &PyObject, view: *mut Py_buffer, writable: bool) -> Result<(), bool> {
    unsafe {
        let flags = if writable { PYBUF_WRITABLE } else { 0 };
        if super::buffer::PyObject_CheckBuffer(arg.as_ptr()) == 0 {
            return Err(false);
        }
        if PyObject_GetBuffer(arg.as_ptr(), view, flags) < 0 {
            clear_exc();
            // a strided export means the buffer exists but is not contiguous
            const PYBUF_FULL_RO: c_int = 0x0100 | 0x0010 | 0x0008 | 0x0004;
            if PyObject_GetBuffer(arg.as_ptr(), view, PYBUF_FULL_RO | flags) == 0 {
                PyBuffer_Release(view);
                return Err(true);
            }
            clear_exc();
            return Err(false);
        }
        if super::buffer::PyBuffer_IsContiguous(view, b'C' as c_char) == 0 {
            PyBuffer_Release(view);
            return Err(true);
        }
        Ok(())
    }
}

/// The low `bits` bits of the int `i` for an unsigned format unit. Values from the
/// signed minimum to the unsigned maximum of that width convert silently; anything
/// else still wraps but is deprecated (3.15).
unsafe fn unsigned_wrap(i: &PyObject, bits: u32) -> Result<u64, ConvErr> {
    unsafe {
        let lo = -(1i128 << (bits - 1));
        let hi = (1i128 << bits) - 1;
        let in_range = int::as_i128(i).is_some_and(|v| v >= lo && v <= hi);
        if !in_range {
            let msg = format!("integer is out of range for an unsigned {}-bit C type; the value is truncated", bits);
            if crate::modules::warnings::warn(crate::types::exceptions::DeprecationWarning(), &msg, 1).is_err() {
                return Err(ConvErr::Raised);
            }
        }
        let v = super::long::PyLong_AsUnsignedLongLongMask(i.as_ptr());
        if v == u64::MAX && err_occurred() {
            return Err(ConvErr::Raised);
        }
        Ok(if bits == 64 { v } else { v & ((1u64 << bits) - 1) })
    }
}

/// Convert one (non-nested) unit `u` for `arg`.
unsafe fn convert_simple(ctx: &mut Ctx, u: &[u8], arg: &PyObject) -> CResult {
    unsafe {
        let c = u[0];
        let m = u.get(1).copied();
        // modifier validity
        let ok_mod = match (c, m) {
            (_, None) => true,
            (b'O', Some(b'!')) | (b'O', Some(b'&')) => true,
            (b's' | b'z' | b'y', Some(b'#' | b'*')) => true,
            (b'w', Some(b'*')) => true,
            (b'e', _) => true,
            _ => false,
        };
        if !ok_mod {
            return Err(bad_format());
        }
        macro_rules! int_conv {
            ($t:ty, $lo:expr, $hi:expr, $lomsg:expr, $himsg:expr) => {{
                let p: *mut $t = ctx.ptr();
                let v = super::long::PyLong_AsLong(arg.as_ptr());
                if v == -1 && err_occurred() {
                    return Err(ConvErr::Raised);
                }
                if v < $lo {
                    let _ = overflow_error($lomsg);
                    return Err(ConvErr::Raised);
                }
                if v > $hi {
                    let _ = overflow_error($himsg);
                    return Err(ConvErr::Raised);
                }
                *p = v as $t;
                Ok(())
            }};
        }
        match c {
            b'b' => int_conv!(u8, 0, 255, "unsigned byte integer is less than minimum", "unsigned byte integer is greater than maximum"),
            b'h' => int_conv!(c_short, i16::MIN as c_long, i16::MAX as c_long, "signed short integer is less than minimum", "signed short integer is greater than maximum"),
            b'i' => int_conv!(c_int, i32::MIN as c_long, i32::MAX as c_long, "signed integer is less than minimum", "signed integer is greater than maximum"),
            b'B' | b'H' | b'I' => {
                let p: *mut c_void = ctx.ptr();
                if float::is_float(arg) {
                    let _ = type_error("'float' object cannot be interpreted as an integer");
                    return Err(ConvErr::Raised);
                }
                let i = match ab::index(arg) {
                    Ok(i) => i,
                    Err(_) => return Err(ConvErr::Raised),
                };
                let bits = match c {
                    b'B' => 8,
                    b'H' => 16,
                    _ => 32,
                };
                let v = unsigned_wrap(&i, bits)?;
                match c {
                    b'B' => *(p as *mut u8) = v as u8,
                    b'H' => *(p as *mut c_ushort) = v as c_ushort,
                    _ => *(p as *mut c_uint) = v as c_uint,
                }
                Ok(())
            }
            b'l' | b'L' | b'n' => {
                let p: *mut c_void = ctx.ptr();
                if float::is_float(arg) {
                    let _ = type_error("'float' object cannot be interpreted as an integer");
                    return Err(ConvErr::Raised);
                }
                match c {
                    b'n' => {
                        let i = match ab::index(arg) {
                            Ok(i) => i,
                            Err(_) => return Err(ConvErr::Raised),
                        };
                        let v = super::long::PyLong_AsSsize_t(i.as_ptr());
                        if v == -1 && err_occurred() {
                            return Err(ConvErr::Raised);
                        }
                        *(p as *mut Py_ssize_t) = v;
                    }
                    _ => {
                        let v = super::long::PyLong_AsLongLong(arg.as_ptr());
                        if v == -1 && err_occurred() {
                            return Err(ConvErr::Raised);
                        }
                        if c == b'l' {
                            *(p as *mut c_long) = v as c_long;
                        } else {
                            *(p as *mut c_longlong) = v;
                        }
                    }
                }
                Ok(())
            }
            b'k' | b'K' => {
                let p: *mut c_void = ctx.ptr();
                if !int::is_int(arg) && !ab::has_index(arg) {
                    return Err(must_be("int", arg));
                }
                // ints, and (3.14+) objects with __index__ through the int they convert to
                let i = match ab::index(arg) {
                    Ok(i) => i,
                    Err(_) => return Err(ConvErr::Raised),
                };
                let v = unsigned_wrap(&i, 64)?;
                if c == b'k' {
                    *(p as *mut c_ulong) = v as c_ulong;
                } else {
                    *(p as *mut c_ulonglong) = v;
                }
                Ok(())
            }
            b'c' => {
                let p: *mut c_char = ctx.ptr();
                if bytes::is_bytes(arg) && crate::object::var_size(arg) == 1 {
                    *p = bytes::bytes_data(arg)[0] as c_char;
                    return Ok(());
                }
                if bytearray::is_bytearray(arg) && crate::object::var_size(arg) == 1 {
                    *p = bytearray::bytearray_data(arg)[0] as c_char;
                    return Ok(());
                }
                if bytes::is_bytes(arg) || bytearray::is_bytearray(arg) {
                    return Err(ConvErr::Msg(format!(
                        "must be a byte string of length 1, not {} of length {}",
                        tname(arg),
                        crate::object::var_size(arg)
                    )));
                }
                Err(must_be("a byte string of length 1", arg))
            }
            b'C' => {
                let p: *mut c_int = ctx.ptr();
                if pystr::is_str(arg) && pystr::str_len(arg) == 1 {
                    *p = pystr::str_data(arg).get(0) as c_int;
                    return Ok(());
                }
                if pystr::is_str(arg) {
                    return Err(ConvErr::Msg(format!("must be a unicode character, not a string of length {}", pystr::str_len(arg))));
                }
                Err(must_be("a unicode character", arg))
            }
            b'p' => {
                let p: *mut c_int = ctx.ptr();
                match ab::is_true(arg) {
                    Ok(b) => {
                        *p = b as c_int;
                        Ok(())
                    }
                    Err(_) => Err(ConvErr::Raised),
                }
            }
            b'f' | b'd' => {
                let p: *mut c_void = ctx.ptr();
                let v = super::long::PyFloat_AsDouble(arg.as_ptr());
                if v == -1.0 && err_occurred() {
                    return Err(ConvErr::Raised);
                }
                if c == b'f' {
                    *(p as *mut f32) = v as f32;
                } else {
                    *(p as *mut f64) = v;
                }
                Ok(())
            }
            b'D' => {
                let p: *mut Py_complex = ctx.ptr();
                let v = super::long::PyComplex_AsCComplex(arg.as_ptr());
                if v.real == -1.0 && err_occurred() {
                    return Err(ConvErr::Raised);
                }
                *p = v;
                Ok(())
            }
            b'O' => match m {
                Some(b'!') => {
                    let tp: *mut PyTypeObject = ctx.ptr();
                    let p: *mut *mut PyObject = ctx.ptr();
                    if crate::types::typeobject::is_subtype(arg.type_ptr(), tp) {
                        *p = arg.as_ptr();
                        Ok(())
                    } else {
                        Err(must_be(&cs((*tp).tp_name), arg))
                    }
                }
                Some(b'&') => {
                    let fp: *mut c_void = ctx.ptr();
                    let addr: *mut c_void = ctx.ptr();
                    let f: unsafe extern "C" fn(*mut PyObject, *mut c_void) -> c_int = std::mem::transmute(fp);
                    let r = f(arg.as_ptr(), addr);
                    if r == 0 {
                        return Err(ConvErr::Raised);
                    }
                    if r == CLEANUP_SUPPORTED {
                        ctx.cleanups.push(Cleanup::Converter(f, addr));
                    }
                    Ok(())
                }
                _ => {
                    let p: *mut *mut PyObject = ctx.ptr();
                    *p = arg.as_ptr();
                    Ok(())
                }
            },
            b'S' | b'Y' | b'U' => {
                let p: *mut *mut PyObject = ctx.ptr();
                let (ok, what) = match c {
                    b'S' => (bytes::is_bytes(arg), "bytes"),
                    b'Y' => (bytearray::is_bytearray(arg), "bytearray"),
                    _ => (pystr::is_str(arg), "str"),
                };
                if !ok {
                    return Err(must_be(what, arg));
                }
                *p = arg.as_ptr();
                Ok(())
            }
            b's' | b'z' | b'y' => {
                let allow_none = c == b'z';
                let allow_str = c != b'y';
                match m {
                    Some(b'*') => {
                        let view: *mut Py_buffer = ctx.ptr();
                        if allow_none && none::is_none(arg) {
                            super::buffer::PyBuffer_FillInfo(view, std::ptr::null_mut(), std::ptr::null_mut(), 0, 1, 0);
                            return Ok(());
                        }
                        if allow_str && pystr::is_str(arg) {
                            let mut n: Py_ssize_t = 0;
                            let s = super::unicode::PyUnicode_AsUTF8AndSize(arg.as_ptr(), &mut n);
                            if s.is_null() {
                                return Err(ConvErr::Raised);
                            }
                            super::buffer::PyBuffer_FillInfo(view, arg.as_ptr(), s as *mut c_void, n, 1, 0);
                            ctx.cleanups.push(Cleanup::Buffer(view));
                            return Ok(());
                        }
                        if no_buffer_raised(arg) {
                            return Err(ConvErr::Raised);
                        }
                        if let Err(noncontig) = get_buffer(arg, view, false) {
                            if noncontig {
                                let _ = crate::errors::new_err(crate::types::exceptions::BufferError(), "argument is not a C-contiguous buffer");
                                return Err(ConvErr::Raised);
                            }
                            return Err(must_be(
                                match c {
                                    b's' => "str or bytes-like object",
                                    b'z' => "str, bytes-like object or None",
                                    _ => "bytes-like object",
                                },
                                arg,
                            ));
                        }
                        ctx.cleanups.push(Cleanup::Buffer(view));
                        Ok(())
                    }
                    Some(b'#') => {
                        let p: *mut *const c_char = ctx.ptr();
                        let lp: *mut Py_ssize_t = ctx.ptr();
                        if allow_none && none::is_none(arg) {
                            *p = std::ptr::null();
                            *lp = 0;
                            return Ok(());
                        }
                        if allow_str && pystr::is_str(arg) {
                            let s = super::unicode::PyUnicode_AsUTF8AndSize(arg.as_ptr(), lp);
                            if s.is_null() {
                                return Err(ConvErr::Raised);
                            }
                            *p = s;
                            return Ok(());
                        }
                        // read-only bytes-like objects only (no release needed: bytes)
                        if no_buffer_raised(arg) {
                            return Err(ConvErr::Raised);
                        }
                        let bp = arg.ty().tp_as_buffer;
                        let releases = !bp.is_null() && (*bp).bf_releasebuffer.is_some();
                        let mut view: Py_buffer = std::mem::zeroed();
                        if releases || bytearray::is_bytearray(arg) || get_buffer(arg, &mut view, false).is_err() {
                            return Err(must_be(
                                match c {
                                    b's' => "str or read-only bytes-like object",
                                    b'z' => "str, read-only bytes-like object or None",
                                    _ => "read-only bytes-like object",
                                },
                                arg,
                            ));
                        }
                        *p = view.buf as *const c_char;
                        *lp = view.len;
                        PyBuffer_Release(&mut view);
                        Ok(())
                    }
                    _ => {
                        let p: *mut *const c_char = ctx.ptr();
                        if allow_none && none::is_none(arg) {
                            *p = std::ptr::null();
                            return Ok(());
                        }
                        if c == b'y' {
                            if !bytes::is_bytes(arg) {
                                return Err(must_be("bytes", arg));
                            }
                            if bytes::bytes_data(arg).contains(&0) {
                                let _ = value_error("embedded null byte");
                                return Err(ConvErr::Raised);
                            }
                            *p = bytes::bytes_ptr(arg) as *const c_char;
                            return Ok(());
                        }
                        if !pystr::is_str(arg) {
                            return Err(must_be(if allow_none { "str or None" } else { "str" }, arg));
                        }
                        let mut n: Py_ssize_t = 0;
                        let s = super::unicode::PyUnicode_AsUTF8AndSize(arg.as_ptr(), &mut n);
                        if s.is_null() {
                            return Err(ConvErr::Raised);
                        }
                        if libc::strlen(s) != n as usize {
                            let _ = value_error("embedded null character");
                            return Err(ConvErr::Raised);
                        }
                        *p = s;
                        Ok(())
                    }
                }
            }
            b'w' => {
                if m != Some(b'*') {
                    return Err(bad_format());
                }
                let view: *mut Py_buffer = ctx.ptr();
                if get_buffer(arg, view, true).is_err() {
                    return Err(must_be("read-write bytes-like object", arg));
                }
                ctx.cleanups.push(Cleanup::Buffer(view));
                Ok(())
            }
            b'e' => {
                let kind = u.get(1).copied();
                if kind != Some(b's') && kind != Some(b't') {
                    return Err(bad_format());
                }
                let with_len = u.get(2) == Some(&b'#');
                if u.get(2) == Some(&b'*') {
                    return Err(bad_format());
                }
                let enc: *const c_char = ctx.ap.next_arg::<*const c_char>();
                let buf: *mut *mut c_char = ctx.ptr();
                let lenp: *mut Py_ssize_t = if with_len { ctx.ptr() } else { std::ptr::null_mut() };
                let encoding = if enc.is_null() { "utf-8".to_string() } else { cs(enc).into_owned() };
                // obtain bytes
                let data: Vec<u8> = if kind == Some(b't') && (bytes::is_bytes(arg) || bytearray::is_bytearray(arg)) {
                    bytes::bytes_like_data(arg).unwrap_or_default()
                } else if pystr::is_str(arg) {
                    let e = pystr::new_str(&encoding);
                    match crate::modules::codecs_core::encode_str(arg, Some(&e), None) {
                        Ok(b) => {
                            if !bytes::is_bytes(&b) {
                                return Err(ConvErr::Msg("(encoder failed to return bytes)".into()));
                            }
                            bytes::bytes_data(&b).to_vec()
                        }
                        Err(_) => return Err(ConvErr::Raised),
                    }
                } else {
                    return Err(must_be(if kind == Some(b't') { "str, bytes or bytearray" } else { "str" }, arg));
                };
                if with_len {
                    if (*buf).is_null() {
                        let p = pymem_cstring(&data);
                        if p.is_null() {
                            let _ = memory_error();
                            return Err(ConvErr::Raised);
                        }
                        *buf = p;
                        ctx.cleanups.push(Cleanup::Mem(buf));
                    } else {
                        if data.len() + 1 > *lenp as usize {
                            let _ = value_error("encoded string too long");
                            return Err(ConvErr::Raised);
                        }
                        std::ptr::copy_nonoverlapping(data.as_ptr(), *buf as *mut u8, data.len());
                        *(*buf).add(data.len()) = 0;
                    }
                    *lenp = data.len() as Py_ssize_t;
                } else {
                    if data.contains(&0) {
                        return Err(ConvErr::Msg("encoded string without null bytes".into()));
                    }
                    let p = pymem_cstring(&data);
                    if p.is_null() {
                        let _ = memory_error();
                        return Err(ConvErr::Raised);
                    }
                    *buf = p;
                    ctx.cleanups.push(Cleanup::Mem(buf));
                }
                Ok(())
            }
            _ => Err(bad_format()),
        }
    }
}

/// Skip the C arguments of one unit (used for missing optional arguments).
unsafe fn skip_unit(ctx: &mut Ctx, u: &[u8]) -> Result<(), ()> {
    unsafe {
        if u[0] == b'(' {
            let inner = &u[1..u.len() - 1];
            let mut i = 0;
            while i < inner.len() {
                let n = unit_len(inner, i).ok_or(())?;
                skip_unit(ctx, &inner[i..i + n])?;
                i += n;
            }
            return Ok(());
        }
        let n = match (u[0], u.get(1).copied(), u.get(2).copied()) {
            (b'O', Some(b'!'), _) | (b'O', Some(b'&'), _) => 2,
            (b'e', _, Some(b'#')) => 3,
            (b'e', _, _) => 2,
            (_, Some(b'#'), _) => 2,
            _ => 1,
        };
        // validate modifiers like the real converter would
        let ok_mod = match (u[0], u.get(1).copied()) {
            (b'w', m) => m == Some(b'*'),
            (b'e', m) => matches!(m, Some(b's' | b't')) && u.get(2) != Some(&b'*'),
            (_, None) => true,
            (b'O', Some(b'!' | b'&')) => true,
            (b's' | b'z' | b'y', Some(b'#' | b'*')) => true,
            _ => false,
        };
        if !ok_mod || !b"bBhHiIlkLKncCfdDOpSYUszyweN".contains(&u[0]) && u[0] != b'O' {
            let _ = system_error("impossible<bad format char>");
            return Err(());
        }
        for _ in 0..n {
            let _: *mut c_void = ctx.ptr();
        }
        Ok(())
    }
}

/// Convert one unit, handling nested tuples. `path` describes the position for errors.
unsafe fn convert_item(ctx: &mut Ctx, u: &[u8], arg: &PyObject) -> CResult {
    unsafe {
        if u[0] != b'(' {
            return convert_simple(ctx, u, arg);
        }
        let inner = &u[1..u.len() - 1];
        let mut units = Vec::new();
        let mut i = 0;
        while i < inner.len() {
            let n = match unit_len(inner, i) {
                Some(n) => n,
                None => return Err(bad_format()),
            };
            units.push(&inner[i..i + n]);
            i += n;
        }
        let n = units.len();
        let is_seq = !pystr::is_str(arg) && !bytes::is_bytes(arg) && !bytearray::is_bytearray(arg) && !dict::is_dict(arg)
            && super::abstract_::PySequence_Check(arg.as_ptr()) != 0;
        if !is_seq {
            return Err(ConvErr::Msg(format!("must be {}-item tuple, not {}", n, if none::is_none(arg) { "None".into() } else { tname(arg) })));
        }
        // units that keep borrowed references need a tuple: another sequence is
        // (deprecatedly) copied into one, and reported as such
        let as_tuple = tuple::is_tuple(arg) || inner.iter().any(|c| b"OSUY".contains(c));
        if !tuple::is_tuple(arg) && as_tuple {
            if crate::modules::warnings::warn(
                crate::types::exceptions::DeprecationWarning(),
                &format!("argument must be {}-item tuple, not {}", n, tname(arg)),
                1,
            )
            .is_err()
            {
                return Err(ConvErr::Raised);
            }
        }
        let len = match ab::length(arg) {
            Ok(l) => l,
            Err(_) => return Err(ConvErr::Raised),
        };
        if len != n {
            return Err(ConvErr::Msg(format!(
                "must be {} of length {}, not {}",
                if as_tuple { "tuple" } else { "sequence" },
                n,
                len
            )));
        }
        for (k, unit) in units.iter().enumerate() {
            let item = match ab::getitem(arg, &int::new_int(k as i64)) {
                Ok(x) => x,
                Err(_) => return Err(ConvErr::Raised),
            };
            // nested items stay alive through the container (CPython semantics for 'O')
            let ip = item.as_ptr();
            drop(item);
            if let Err(e) = convert_item(ctx, unit, &*ip) {
                return Err(match e {
                    ConvErr::Msg(m) => ConvErr::Msg(m),
                    r => r,
                });
            }
        }
        Ok(())
    }
}

/// Parsed top-level format.
struct Format<'f> {
    units: Vec<&'f [u8]>,
    min: usize,        // index of '|' (or len)
    kwonly: usize,     // index of '$' (or len)
    fname: Option<String>,
    message: Option<String>,
}

fn parse_format(f: &[u8]) -> Result<Format<'_>, String> {
    let mut units = Vec::new();
    let mut min = usize::MAX;
    let mut kwonly = usize::MAX;
    let mut fname = None;
    let mut message = None;
    let mut i = 0;
    while i < f.len() {
        match f[i] {
            b'|' => {
                if min != usize::MAX {
                    return Err("Invalid format string (| specified twice)".into());
                }
                if kwonly != usize::MAX {
                    return Err("Invalid format string ($ before |)".into());
                }
                min = units.len();
                i += 1;
            }
            b'$' => {
                if kwonly != usize::MAX {
                    return Err("Invalid format string ($ specified twice)".into());
                }
                kwonly = units.len();
                i += 1;
            }
            b':' => {
                fname = Some(String::from_utf8_lossy(&f[i + 1..]).into_owned());
                break;
            }
            b';' => {
                message = Some(String::from_utf8_lossy(&f[i + 1..]).into_owned());
                break;
            }
            b' ' | b'\t' | b'\n' | b',' => i += 1,
            _ => {
                let n = unit_len(f, i).ok_or_else(|| "excess ')' in getargs format".to_string())?;
                units.push(&f[i..i + n]);
                i += n;
            }
        }
    }
    let len = units.len();
    Ok(Format { units, min: min.min(len), kwonly: kwonly.min(len), fname, message })
}

fn fn_prefix(fname: &Option<String>) -> (String, &'static str) {
    match fname {
        Some(f) => (f.clone(), "()"),
        None => ("function".into(), ""),
    }
}

fn set_conv_error(e: ConvErr, iarg: usize, f: &Format) {
    if let ConvErr::Msg(m) = e {
        if err_occurred() {
            return;
        }
        if let Some(msg) = &f.message {
            let _ = type_error(msg);
            return;
        }
        let mut s = String::new();
        if let Some(n) = &f.fname {
            s.push_str(n);
            s.push_str("() ");
        }
        if iarg != 0 {
            s.push_str(&format!("argument {}", iarg));
        } else {
            s.push_str("argument");
        }
        if !m.starts_with(", ") {
            s.push(' ');
        }
        s.push_str(&m);
        let _ = type_error(&s);
    }
}

/// Positional-only parsing (PyArg_ParseTuple semantics).
unsafe fn parse_positional(args: &[*mut PyObject], fmt: *const c_char, ap: &mut VaList, compat_single: Option<&PyObject>) -> c_int {
    unsafe {
        let fb = CStr::from_ptr(fmt).to_bytes();
        let f = match parse_format(fb) {
            Ok(f) => f,
            Err(m) => {
                let _ = system_error(&m);
                return 0;
            }
        };
        let mut ctx = Ctx { ap, cleanups: Vec::new() };
        // PyArg_Parse: a single object is matched against a single unit
        if let Some(o) = compat_single {
            if f.units.len() == 1 {
                return match convert_item(&mut ctx, f.units[0], o) {
                    Ok(()) => 1,
                    Err(e) => {
                        set_conv_error(e, 0, &f);
                        ctx.cleanup();
                        0
                    }
                };
            }
            if f.units.is_empty() {
                return 1;
            }
        }
        let len = args.len();
        let max = f.units.len();
        let min = f.min;
        if len < min || len > max {
            match &f.message {
                Some(m) => {
                    let _ = type_error(m);
                }
                None => {
                    let (n, p) = fn_prefix(&f.fname);
                    let which = if min == max { "exactly" } else if len < min { "at least" } else { "at most" };
                    let cnt = if len < min { min } else { max };
                    let _ = type_error(&format!(
                        "{}{} takes {} {} argument{} ({} given)",
                        n,
                        p,
                        which,
                        cnt,
                        if cnt == 1 { "" } else { "s" },
                        len
                    ));
                }
            }
            return 0;
        }
        for (i, u) in f.units.iter().enumerate() {
            if i < len {
                if let Err(e) = convert_item(&mut ctx, u, &*args[i]) {
                    set_conv_error(e, i + 1, &f);
                    ctx.cleanup();
                    return 0;
                }
            } else if skip_unit(&mut ctx, u).is_err() {
                ctx.cleanup();
                return 0;
            }
        }
        1
    }
}

enum Kw<'a> {
    None,
    Dict(&'a PyObject),
    Names(&'a PyObject, &'a [*mut PyObject]),
}
impl<'a> Kw<'a> {
    fn pairs(&self) -> Vec<(PyRef, PyRef)> {
        match self {
            Kw::None => Vec::new(),
            Kw::Dict(d) => dict::items_vec(d),
            Kw::Names(n, vals) => tuple::tuple_items(n).iter().zip(vals.iter()).map(|(k, v)| (k.to_ref(), unsafe { (**v).to_ref() })).collect(),
        }
    }
}

/// Keyword parsing (PyArg_ParseTupleAndKeywords semantics).
unsafe fn parse_keywords(args: &[*mut PyObject], kw: Kw, fmt: *const c_char, kwlist: *const *const c_char, ap: &mut VaList) -> c_int {
    unsafe {
        let fb = CStr::from_ptr(fmt).to_bytes();
        let f = match parse_format(fb) {
            Ok(f) => f,
            Err(m) => {
                let _ = system_error(&m);
                return 0;
            }
        };
        // keyword names
        let mut names: Vec<Option<Vec<u8>>> = Vec::new();
        let mut k = kwlist;
        while !(*k).is_null() {
            names.push(Some(CStr::from_ptr(*k).to_bytes().to_vec()));
            k = k.add(1);
        }
        let len = f.units.len();
        if names.len() > len {
            let _ = system_error(&format!("More keyword list entries ({}) than format specifiers ({})", names.len(), len));
            return 0;
        }
        if names.len() < len {
            let _ = system_error(&format!(
                "More argument specifiers than keyword list entries (remaining format:'{}')",
                String::from_utf8_lossy(&f.units[names.len()..].concat())
            ));
            return 0;
        }
        let pos = names.iter().take_while(|n| n.as_ref().map_or(false, |v| v.is_empty())).count();
        for (i, n) in names.iter().enumerate().skip(pos) {
            if n.as_ref().map_or(false, |v| v.is_empty()) {
                if i >= f.kwonly {
                    let _ = system_error("Empty parameter name after $");
                } else {
                    let _ = system_error("Empty keyword parameter name");
                }
                return 0;
            }
        }
        if pos > f.kwonly {
            let _ = system_error("Empty parameter name after $");
            return 0;
        }
        let (fnm, fsuf) = fn_prefix(&f.fname);
        let nargs = args.len();
        let pairs = kw.pairs();
        for (key, _) in &pairs {
            if !pystr::is_str(key) {
                let _ = type_error("keywords must be strings");
                return 0;
            }
        }
        let nkw = pairs.len();
        if nargs + nkw > len {
            let _ = type_error(&format!(
                "{}{} takes at most {} {}argument{} ({} given)",
                fnm,
                fsuf,
                len,
                if nargs == 0 { "keyword " } else { "" },
                if len == 1 { "" } else { "s" },
                nargs + nkw
            ));
            return 0;
        }
        let max_pos = f.kwonly;
        if nargs > max_pos {
            let _ = type_error(&format!(
                "{}{} takes {} {} positional argument{} ({} given)",
                fnm,
                fsuf,
                if f.min < max_pos { "at most" } else { "exactly" },
                max_pos,
                if max_pos == 1 { "" } else { "s" },
                nargs
            ));
            return 0;
        }
        if nargs < pos.min(f.min) {
            let m = pos.min(f.min);
            let _ = type_error(&format!(
                "{}{} takes {} {} positional argument{} ({} given)",
                fnm,
                fsuf,
                if m < max_pos { "at least" } else { "exactly" },
                m,
                if m == 1 { "" } else { "s" },
                nargs
            ));
            return 0;
        }
        // match keywords
        let mut kwvals: Vec<Option<PyRef>> = vec![None; len];
        let mut used = vec![false; nkw];
        // a keyword naming a parameter already filled by position is an error only if no
        // later parameter takes it (a keyword-only parameter may repeat a positional name,
        // as msgspec's StructMeta.__new__(name, bases, dict, *, ..., dict=...) does)
        let mut conflict: Vec<Option<(usize, String)>> = vec![None; nkw];
        for i in pos..len {
            let Some(name) = names[i].as_ref() else { continue };
            let namestr = match std::str::from_utf8(name) {
                Ok(s) => s,
                Err(e) => {
                    // a name that must be looked up among the keywords has to decode
                    if i >= nargs && nkw > 0 {
                        let start = e.valid_up_to();
                        let end = start + e.error_len().unwrap_or(name.len() - start);
                        let _ = crate::types::exceptions::unicode_decode_error("utf-8", name, start, end, "invalid continuation byte");
                        return 0;
                    }
                    continue;
                }
            };
            for (j, (key, val)) in pairs.iter().enumerate() {
                // keywords match like dict lookups: an exact str by content, a str subclass
                // by hash and its own __eq__
                let hit = if pystr::is_str_exact(key) {
                    pystr::str_eq_ascii(key, namestr) || pystr::to_string_strict(key).as_deref() == Some(namestr)
                } else {
                    let n = pystr::new_str(namestr);
                    let same_hash = matches!((ab::hash(key), ab::hash(&n)), (Ok(a), Ok(b)) if a == b);
                    let r = same_hash && ab::rich_eq(key, &n).unwrap_or(false);
                    clear_exc();
                    r
                };
                if hit {
                    if used[j] {
                        continue;
                    }
                    if i < nargs {
                        if conflict[j].is_none() {
                            conflict[j] = Some((i, namestr.to_string()));
                        }
                        continue;
                    }
                    used[j] = true;
                    kwvals[i] = Some(val.clone());
                    break;
                }
            }
        }
        for j in 0..nkw {
            if let (false, Some((i, n))) = (used[j], &conflict[j]) {
                let _ = type_error(&format!("argument for {}{} given by name ('{}') and position ({})", fnm, fsuf, n, i + 1));
                return 0;
            }
        }
        if let Some(j) = used.iter().position(|u| !u) {
            let key = &pairs[j].0;
            // a leftover key that spells a parameter name (a str subclass that failed the
            // lookup) is reported differently from an unknown name
            let bad_sub = names.iter().flatten().filter(|n| !n.is_empty()).filter_map(|n| std::str::from_utf8(n).ok()).any(|n| pystr::to_string_strict(key).as_deref() == Some(n));
            clear_exc();
            if bad_sub {
                let _ = type_error(&format!("invalid keyword argument for {}{}", if f.fname.is_none() { "this function" } else { &fnm }, fsuf));
            } else {
                // invalid UTF-8 keyword names in the list are compared by decoding them
                for n in names.iter().flatten() {
                    if std::str::from_utf8(n).is_err() && !n.is_empty() {
                        let _ = crate::types::exceptions::unicode_decode_error("utf-8", n, 0, n.len(), "invalid utf-8");
                        return 0;
                    }
                }
                let rep = super::unicode::PyUnicode_FromObject(key.as_ptr());
                let k2 = if rep.is_null() { clear_exc(); key.clone() } else { PyRef::from_raw(rep) };
                // the message embeds the key itself (it may hold lone surrogates)
                let head = pystr::new_str(&format!(
                    "{}{} got an unexpected keyword argument '",
                    if f.fname.is_none() { "this function".to_string() } else { fnm.clone() },
                    fsuf
                ));
                let msg = pystr::concat(&pystr::concat(&head, &k2), &pystr::new_str("'"));
                let _ = crate::errors::set_err_obj(crate::types::exceptions::TypeError(), msg);
            }
            return 0;
        }
        let mut ctx = Ctx { ap, cleanups: Vec::new() };
        for i in 0..len {
            let u = f.units[i];
            let val: Option<PyRef> = if i < nargs { Some((*args[i]).to_ref()) } else { kwvals[i].take() };
            match val {
                Some(v) => {
                    // keep the borrowed pointer valid: argument objects outlive the call
                    let vp = v.as_ptr();
                    drop(v);
                    if let Err(e) = convert_item(&mut ctx, u, &*vp) {
                        set_conv_error(e, i + 1, &f);
                        ctx.cleanup();
                        return 0;
                    }
                }
                None => {
                    if i < f.min {
                        if i < pos {
                            let m = pos.min(f.min);
                            let _ = type_error(&format!(
                                "{}{} takes {} {} positional argument{} ({} given)",
                                fnm,
                                fsuf,
                                if m < max_pos { "at least" } else { "exactly" },
                                m,
                                if m == 1 { "" } else { "s" },
                                nargs
                            ));
                        } else {
                            let n = names[i].as_ref().map(|v| String::from_utf8_lossy(v).into_owned()).unwrap_or_default();
                            let _ = type_error(&format!("{}{} missing required argument '{}' (pos {})", fnm, fsuf, n, i + 1));
                        }
                        ctx.cleanup();
                        return 0;
                    }
                    if skip_unit(&mut ctx, u).is_err() {
                        ctx.cleanup();
                        return 0;
                    }
                }
            }
        }
        1
    }
}

unsafe fn tuple_args<'a>(args: *mut PyObject) -> Option<&'a [*mut PyObject]> {
    unsafe {
        if args.is_null() || !tuple::is_tuple(&*args) {
            let _ = system_error("new style getargs format but argument is not a tuple");
            return None;
        }
        Some(std::slice::from_raw_parts(tuple::item_ptr(args), tuple::tuple_len(&*args)))
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyArg_ParseTuple(args: *mut PyObject, fmt: *const c_char, mut ap: ...) -> c_int {
    unsafe {
        let Some(a) = tuple_args(args) else { return 0 };
        parse_positional(a, fmt, &mut ap, None)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyArg_ParseTuple_SizeT(args: *mut PyObject, fmt: *const c_char, mut ap: ...) -> c_int {
    unsafe {
        let Some(a) = tuple_args(args) else { return 0 };
        parse_positional(a, fmt, &mut ap, None)
    }
}
// stable-ABI-only "_SizeT" variants (Py_ssize_t lengths are the only kind now) (capi2)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyArg_VaParse_SizeT(args: *mut PyObject, fmt: *const c_char, mut ap: VaList) -> c_int {
    unsafe {
        let Some(a) = tuple_args(args) else { return 0 };
        parse_positional(a, fmt, &mut ap, None)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyArg_Parse_SizeT(arg: *mut PyObject, fmt: *const c_char, mut ap: ...) -> c_int {
    unsafe {
        if arg.is_null() {
            let _ = bad_internal();
            return 0;
        }
        parse_positional(&[arg], fmt, &mut ap, Some(&*arg))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyArg_VaParseTupleAndKeywords_SizeT(args: *mut PyObject, kw: *mut PyObject, fmt: *const c_char, kwlist: *const *const c_char, ap: VaList) -> c_int {
    unsafe { PyArg_VaParseTupleAndKeywords(args, kw, fmt, kwlist, ap) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyArg_VaParse(args: *mut PyObject, fmt: *const c_char, mut ap: VaList) -> c_int {
    unsafe {
        let Some(a) = tuple_args(args) else { return 0 };
        parse_positional(a, fmt, &mut ap, None)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyArg_Parse(arg: *mut PyObject, fmt: *const c_char, mut ap: ...) -> c_int {
    unsafe {
        if arg.is_null() {
            let _ = bad_internal();
            return 0;
        }
        parse_positional(&[arg], fmt, &mut ap, Some(&*arg))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyArg_ParseArray(args: *const *mut PyObject, nargs: Py_ssize_t, fmt: *const c_char, mut ap: ...) -> c_int {
    unsafe {
        let a = if nargs <= 0 { &[][..] } else { std::slice::from_raw_parts(args, nargs as usize) };
        parse_positional(a, fmt, &mut ap, None)
    }
}

unsafe fn check_kw(kw: *mut PyObject, kwlist: *const *const c_char, fmt: *const c_char) -> bool {
    unsafe {
        if (!kw.is_null() && !dict::is_dict(&*kw)) || fmt.is_null() || kwlist.is_null() {
            let _ = bad_internal();
            return false;
        }
        true
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyArg_ParseTupleAndKeywords(args: *mut PyObject, kw: *mut PyObject, fmt: *const c_char, kwlist: *const *const c_char, mut ap: ...) -> c_int {
    unsafe {
        if !check_kw(kw, kwlist, fmt) {
            return 0;
        }
        let Some(a) = tuple_args(args) else { return 0 };
        parse_keywords(a, opt(kw).map_or(Kw::None, Kw::Dict), fmt, kwlist, &mut ap)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyArg_ParseTupleAndKeywords_SizeT(args: *mut PyObject, kw: *mut PyObject, fmt: *const c_char, kwlist: *const *const c_char, mut ap: ...) -> c_int {
    unsafe {
        if !check_kw(kw, kwlist, fmt) {
            return 0;
        }
        let Some(a) = tuple_args(args) else { return 0 };
        parse_keywords(a, opt(kw).map_or(Kw::None, Kw::Dict), fmt, kwlist, &mut ap)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyArg_VaParseTupleAndKeywords(args: *mut PyObject, kw: *mut PyObject, fmt: *const c_char, kwlist: *const *const c_char, mut ap: VaList) -> c_int {
    unsafe {
        if !check_kw(kw, kwlist, fmt) {
            return 0;
        }
        let Some(a) = tuple_args(args) else { return 0 };
        parse_keywords(a, opt(kw).map_or(Kw::None, Kw::Dict), fmt, kwlist, &mut ap)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyArg_ParseArrayAndKeywords(
    args: *const *mut PyObject,
    nargs: Py_ssize_t,
    kwnames: *mut PyObject,
    fmt: *const c_char,
    kwlist: *const *const c_char,
    mut ap: ...
) -> c_int {
    unsafe {
        let n = nargs.max(0) as usize;
        let a = if n == 0 { &[][..] } else { std::slice::from_raw_parts(args, n) };
        let kw = if kwnames.is_null() || tuple::tuple_len(&*kwnames) == 0 {
            Kw::None
        } else {
            let nk = tuple::tuple_len(&*kwnames);
            Kw::Names(&*kwnames, std::slice::from_raw_parts(args.add(n), nk))
        };
        parse_keywords(a, kw, fmt, kwlist, &mut ap)
    }
}

/// struct _PyArg_Parser (cpython/modsupport.h)
#[repr(C)]
pub struct PyArgParser {
    pub format: *const c_char,
    pub keywords: *const *const c_char,
    pub fname: *const c_char,
    pub custom_msg: *const c_char,
    pub once: u8,
    pub is_kwtuple_owned: c_int,
    pub pos: c_int,
    pub min: c_int,
    pub max: c_int,
    pub kwtuple: *mut PyObject,
    pub next: *mut PyArgParser,
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyArg_ParseTupleAndKeywordsFast(args: *mut PyObject, kw: *mut PyObject, parser: *mut PyArgParser, mut ap: ...) -> c_int {
    unsafe {
        let Some(a) = tuple_args(args) else { return 0 };
        let p = &*parser;
        if p.format.is_null() {
            let _ = system_error("_PyArg_ParseTupleAndKeywordsFast: parser without format");
            return 0;
        }
        // compose "format:fname" when the parser carries the name separately
        let mut f = CStr::from_ptr(p.format).to_bytes().to_vec();
        if !p.fname.is_null() && !f.contains(&b':') && !f.contains(&b';') {
            f.push(b':');
            f.extend_from_slice(CStr::from_ptr(p.fname).to_bytes());
        }
        f.push(0);
        parse_keywords(a, opt(kw).map_or(Kw::None, Kw::Dict), f.as_ptr() as *const c_char, p.keywords, &mut ap)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyArg_UnpackTuple(args: *mut PyObject, name: *const c_char, min: Py_ssize_t, max: Py_ssize_t, mut ap: ...) -> c_int {
    unsafe {
        if args.is_null() || !tuple::is_tuple(&*args) {
            let _ = system_error("PyArg_UnpackTuple() argument list is not a tuple");
            return 0;
        }
        let n = tuple::tuple_len(&*args) as isize;
        let nm = if name.is_null() { None } else { Some(cs(name).into_owned()) };
        if n < min || n > max {
            let (which, cnt) = if n < min { (if min == max { "" } else { "at least " }, min) } else { (if min == max { "" } else { "at most " }, max) };
            let _ = type_error(&format!(
                "{}{} expected {}{} argument{}, got {}",
                nm.clone().unwrap_or_else(|| "unpacked tuple".into()),
                if nm.is_some() { "" } else { "" },
                which,
                cnt,
                if cnt == 1 { "" } else { "s" },
                n
            ));
            return 0;
        }
        let items = tuple::item_ptr(args);
        for i in 0..n {
            let p: *mut *mut PyObject = ap.next_arg::<*mut c_void>() as *mut *mut PyObject;
            *p = *items.add(i as usize);
        }
        1
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyArg_ValidateKeywordArguments(kw: *mut PyObject) -> c_int {
    unsafe {
        if kw.is_null() || !dict::is_dict(&*kw) {
            let _ = bad_internal();
            return 0;
        }
        for (k, _) in dict::items_vec(&*kw) {
            if !pystr::is_str(&k) {
                let _ = type_error("keywords must be strings");
                return 0;
            }
        }
        1
    }
}

#[allow(dead_code)]
fn _unused() -> (PyRef, PyRef) {
    (list::new_list_empty(), complex::new_complex(0.0, 0.0))
}
