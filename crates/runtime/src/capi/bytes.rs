//! bytesobject.h / bytearrayobject.h: bytes, bytearray and PyBytesWriter (3.15).

use super::*;
use crate::abstract_ as ab;
use crate::types::typeobject::type_obj;
use crate::types::{bytearray, bytes, int, none, slice, str as pystr};
use std::ffi::{VaList, c_void};

unsafe fn chk_bytes<'a>(o: *mut PyObject) -> PyResult<&'a PyObject> {
    unsafe {
        if o.is_null() || !bytes::is_bytes(&*o) {
            return Err(type_error(&format!(
                "expected bytes, {} found",
                if o.is_null() { "NULL".into() } else { tname(&*o) }
            )));
        }
        Ok(&*o)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_FromStringAndSize(s: *const c_char, n: Py_ssize_t) -> *mut PyObject {
    unsafe {
        if n < 0 {
            let _ = system_error("Negative size passed to PyBytes_FromStringAndSize");
            return std::ptr::null_mut();
        }
        if n == 0 {
            return bytes::empty_bytes().into_raw();
        }
        if n == 1 && !s.is_null() {
            // (with s == NULL the caller fills the object in: it must be fresh)
            return bytes::single_byte(*(s as *const u8)).into_raw();
        }
        const HEADER: usize = std::mem::offset_of!(PyBytesObject, ob_sval);
        if n as usize >= isize::MAX as usize - HEADER - 1 {
            let _ = overflow_error("byte string is too large");
            return std::ptr::null_mut();
        }
        if n as usize > crate::tlab::MAX_SMALL {
            // probe the system allocator first so huge requests raise MemoryError
            let probe = libc::malloc(n as usize + HEADER + 1);
            if probe.is_null() {
                let _ = memory_error();
                return std::ptr::null_mut();
            }
            libc::free(probe);
        }
        let b = bytes::alloc_bytes(n as usize);
        if !s.is_null() && n > 0 {
            std::ptr::copy_nonoverlapping(s as *const u8, bytes::bytes_ptr(&b), n as usize);
        }
        *bytes::bytes_ptr(&b).add(n as usize) = 0;
        b.into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_FromString(s: *const c_char) -> *mut PyObject {
    unsafe { PyBytes_FromStringAndSize(s, libc::strlen(s) as Py_ssize_t) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_FromObject(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        if o.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        if (*o).is_type(bytes::bytes_type()) {
            return (*o).to_ref().into_raw();
        }
        if pystr::is_str(&*o) || int::is_int(&*o) {
            let _ = type_error(&format!("cannot convert '{}' object to bytes", tname(&*o)));
            return std::ptr::null_mut();
        }
        ret(ab::call(type_obj(bytes::bytes_type()), &[&*o]))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_Size(o: *mut PyObject) -> Py_ssize_t {
    unsafe {
        match chk_bytes(o) {
            Ok(b) => crate::object::var_size(b),
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_AsString(o: *mut PyObject) -> *mut c_char {
    unsafe {
        match chk_bytes(o) {
            Ok(b) => bytes::bytes_ptr(b) as *mut c_char,
            Err(_) => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_AsStringAndSize(o: *mut PyObject, s: *mut *mut c_char, len: *mut Py_ssize_t) -> c_int {
    unsafe {
        if s.is_null() {
            let _ = bad_internal();
            return -1;
        }
        let b = match chk_bytes(o) {
            Ok(b) => b,
            Err(_) => return -1,
        };
        *s = bytes::bytes_ptr(b) as *mut c_char;
        let n = crate::object::var_size(b);
        if len.is_null() {
            if bytes::bytes_data(b).contains(&0) {
                let _ = value_error("embedded null byte");
                return -1;
            }
        } else {
            *len = n;
        }
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_Concat(p: *mut *mut PyObject, w: *mut PyObject) {
    unsafe {
        if p.is_null() {
            return;
        }
        let left = *p;
        if left.is_null() {
            return;
        }
        if w.is_null() {
            decref(left);
            *p = std::ptr::null_mut();
            return;
        }
        let r = ab::binary_op(&*left, &*w, ab::NbSlot::Add);
        decref(left);
        *p = ret(r);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_ConcatAndDel(p: *mut *mut PyObject, w: *mut PyObject) {
    unsafe {
        PyBytes_Concat(p, w);
        xdecref(w);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_Join(sep: *mut PyObject, it: *mut PyObject) -> *mut PyObject {
    unsafe {
        if sep.is_null() || !bytes::is_bytes(&*sep) {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        ret(ab::call_method_str(&*sep, "join", &[&*it]))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_Repr(o: *mut PyObject, _smartquotes: c_int) -> *mut PyObject {
    unsafe { ret(ab::repr(&*o)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_DecodeEscape(s: *const c_char, len: Py_ssize_t, errors: *const c_char, _unicode: Py_ssize_t, _recode: *const c_char) -> *mut PyObject {
    unsafe {
        ret((|| -> PyResult {
            let c = crate::import::import_absolute("_codecs")?;
            let f = ab::getattr_str(&c, "escape_decode")?;
            if len > (bytes::MAX_SIZE / 4) as Py_ssize_t {
                return Err(crate::err!(OverflowError, "excessive input length for escape decoding"));
            }
            let b = if s.is_null() { bytes::new_bytes(b"") } else { bytes::try_new_bytes(std::slice::from_raw_parts(s as *const u8, len.max(0) as usize))? };
            let err = if errors.is_null() { none::none_ref() } else { cstr_obj(errors)? };
            let r = ab::call(&f, &[&b, &err])?;
            Ok(crate::types::tuple::tuple_get(&r, 0).to_ref())
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyBytes_Resize(p: *mut *mut PyObject, newsize: Py_ssize_t) -> c_int {
    unsafe {
        let o = *p;
        if o.is_null() || !bytes::is_bytes(&*o) || newsize < 0 {
            *p = std::ptr::null_mut();
            xdecref(o);
            let _ = bad_internal();
            return -1;
        }
        let old = crate::object::var_size(&*o);
        if old == newsize {
            return 0;
        }
        let n = PyBytes_FromStringAndSize(std::ptr::null(), newsize);
        std::ptr::copy_nonoverlapping(bytes::bytes_ptr(&*o), bytes::bytes_ptr(&*n), old.min(newsize) as usize);
        decref(o);
        *p = n;
        0
    }
}

/// printf-like formatting into bytes (PyBytes_FromFormatV subset of C printf).
unsafe fn bytes_format(fmt: *const c_char, ap: &mut VaList) -> PyResult<Vec<u8>> {
    unsafe {
        let f = CStr::from_ptr(fmt).to_bytes();
        let mut out = Vec::with_capacity(f.len());
        let mut i = 0;
        while i < f.len() {
            if f[i] != b'%' {
                out.push(f[i]);
                i += 1;
                continue;
            }
            let start = i;
            i += 1;
            let mut prec: Option<usize> = None;
            // skip width
            while i < f.len() && f[i].is_ascii_digit() {
                i += 1;
            }
            if i < f.len() && f[i] == b'.' {
                i += 1;
                let mut p = 0;
                while i < f.len() && f[i].is_ascii_digit() {
                    p = p * 10 + (f[i] - b'0') as usize;
                    i += 1;
                }
                prec = Some(p);
            }
            let mut lm = 0u8;
            if i < f.len() && f[i] == b'l' {
                lm = b'l';
                i += 1;
                if i < f.len() && f[i] == b'l' {
                    lm = b'L';
                    i += 1;
                }
            } else if i < f.len() && f[i] == b'z' {
                lm = b'z';
                i += 1;
            }
            if i >= f.len() {
                out.extend_from_slice(&f[start..]);
                break;
            }
            let c = f[i];
            i += 1;
            match c {
                b'%' => out.push(b'%'),
                b'c' => {
                    let v: c_int = ap.next_arg::<c_int>();
                    if !(0..256).contains(&v) {
                        return Err(overflow_error("PyBytes_FromFormatV(): %c format expects an integer in range [0; 255]"));
                    }
                    out.push(v as u8);
                }
                b'd' | b'i' => {
                    let v: i64 = match lm {
                        0 => ap.next_arg::<c_int>() as i64,
                        b'z' => ap.next_arg::<isize>() as i64,
                        _ => ap.next_arg::<i64>(),
                    };
                    out.extend_from_slice(v.to_string().as_bytes());
                }
                b'u' => {
                    let v: u64 = match lm {
                        0 => ap.next_arg::<std::ffi::c_uint>() as u64,
                        b'z' => ap.next_arg::<usize>() as u64,
                        _ => ap.next_arg::<u64>(),
                    };
                    out.extend_from_slice(v.to_string().as_bytes());
                }
                b'x' => {
                    let v: c_int = ap.next_arg::<c_int>();
                    out.extend_from_slice(format!("{:x}", v as u32).as_bytes());
                }
                b's' => {
                    let p: *const c_char = ap.next_arg::<*const c_char>();
                    let s = CStr::from_ptr(p).to_bytes();
                    let s = match prec {
                        Some(n) if n < s.len() => &s[..n],
                        _ => s,
                    };
                    out.extend_from_slice(s);
                }
                b'p' => {
                    let p: *const c_void = ap.next_arg::<*const c_void>();
                    out.extend_from_slice(format!("{:#x}", p as usize).as_bytes());
                }
                _ => {
                    // unknown: copy the rest verbatim
                    out.extend_from_slice(&f[start..]);
                    break;
                }
            }
        }
        Ok(out)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_FromFormatV(fmt: *const c_char, mut ap: VaList) -> *mut PyObject {
    unsafe {
        match bytes_format(fmt, &mut ap) {
            Ok(v) => bytes::new_bytes(&v).into_raw(),
            Err(_) => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytes_FromFormat(fmt: *const c_char, mut args: ...) -> *mut PyObject {
    unsafe {
        match bytes_format(fmt, &mut args) {
            Ok(v) => bytes::new_bytes(&v).into_raw(),
            Err(_) => std::ptr::null_mut(),
        }
    }
}

// ---------------------------------------------------------------- bytearray

unsafe fn chk_ba<'a>(o: *mut PyObject) -> PyResult<&'a PyObject> {
    unsafe {
        if o.is_null() || !bytearray::is_bytearray(&*o) {
            return Err(bad_internal());
        }
        Ok(&*o)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyByteArray_FromStringAndSize(s: *const c_char, n: Py_ssize_t) -> *mut PyObject {
    unsafe {
        if n < 0 {
            let _ = system_error("Negative size passed to PyByteArray_FromStringAndSize");
            return std::ptr::null_mut();
        }
        if n as usize >= isize::MAX as usize - 64 {
            let _ = overflow_error("byte string is too large");
            return std::ptr::null_mut();
        }
        let mut v: Vec<u8> = Vec::new();
        if v.try_reserve(n as usize).is_err() {
            let _ = memory_error();
            return std::ptr::null_mut();
        }
        let data = if s.is_null() { vec![0u8; n as usize] } else { std::slice::from_raw_parts(s as *const u8, n as usize).to_vec() };
        ret(ab::call(type_obj(bytearray::bytearray_type()), &[&bytes::new_bytes(&data)]))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyByteArray_FromObject(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        if o.is_null() {
            return ret(ab::call(type_obj(bytearray::bytearray_type()), &[]));
        }
        ret(ab::call(type_obj(bytearray::bytearray_type()), &[&*o]))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyByteArray_Size(o: *mut PyObject) -> Py_ssize_t {
    unsafe {
        match chk_ba(o) {
            Ok(b) => crate::object::var_size(b),
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyByteArray_AsString(o: *mut PyObject) -> *mut c_char {
    unsafe {
        let Ok(_) = chk_ba(o) else { return std::ptr::null_mut() };
        let b = &*(o as *mut PyByteArrayObject);
        if b.ob_start.is_null() { c"".as_ptr() as *mut c_char } else { b.ob_start as *mut c_char }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyByteArray_Concat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        ret((|| -> PyResult {
            let da = contiguous_bytes(&*a);
            let db = contiguous_bytes(&*b);
            match (da, db) {
                (Some(mut x), Some(y)) => {
                    x.extend_from_slice(&y);
                    ab::call(type_obj(bytearray::bytearray_type()), &[&bytes::new_bytes(&x)])
                }
                _ => Err(type_error(&format!("can't concat {} to {}", tname(&*b), tname(&*a)))),
            }
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyByteArray_Resize(o: *mut PyObject, n: Py_ssize_t) -> c_int {
    unsafe {
        let Ok(b) = chk_ba(o) else { return -1 };
        if n < 0 {
            let _ = value_error(&format!("Can only resize to positive sizes, got {}", n));
            return -1;
        }
        let old = crate::object::var_size(b);
        let r = if n < old {
            let s = slice::new_slice(&int::new_int(n as i64), none::none(), none::none());
            ab::setitem(b, &s, None)
        } else if n > old {
            let mut z: Vec<u8> = Vec::new();
            if n as usize >= isize::MAX as usize - 64 || z.try_reserve((n - old) as usize).is_err() {
                let _ = memory_error();
                return -1;
            }
            z.resize((n - old) as usize, 0);
            ab::call_method_str(b, "extend", &[&bytes::new_bytes(&z)]).map(|_| ())
        } else {
            Ok(())
        };
        ret_i(r)
    }
}

// ---------------------------------------------------------------- PyBytesWriter (3.15)

pub struct BytesWriter {
    buf: Vec<u8>,
    /// finish() makes a bytearray (_PyBytesWriter_CreateByteArray) instead of bytes
    bytearray: bool,
}
/// Set `buf` to `n` bytes (new bytes zeroed); MemoryError instead of an abort.
fn bw_set_len(buf: &mut Vec<u8>, n: usize) -> PyResult<()> {
    if n > buf.len() && buf.try_reserve(n - buf.len()).is_err() {
        return Err(crate::errors::memory_error());
    }
    buf.resize(n, 0);
    Ok(())
}
unsafe fn bw_create(size: Py_ssize_t, bytearray: bool) -> *mut BytesWriter {
    if size < 0 {
        let _ = value_error("size must be >= 0");
        return std::ptr::null_mut();
    }
    let mut buf = Vec::new();
    if bw_set_len(&mut buf, size as usize).is_err() {
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(BytesWriter { buf, bytearray }))
}
/// Private helper used by _testcapi: a writer whose result is a bytearray.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyBytesWriter_CreateByteArray(size: Py_ssize_t) -> *mut BytesWriter {
    unsafe { bw_create(size, true) }
}
unsafe fn bw<'a>(w: *mut BytesWriter) -> &'a mut BytesWriter {
    unsafe { &mut *w }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytesWriter_Create(size: Py_ssize_t) -> *mut BytesWriter {
    unsafe { bw_create(size, false) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytesWriter_Discard(w: *mut BytesWriter) {
    if !w.is_null() {
        drop(unsafe { Box::from_raw(w) });
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytesWriter_GetData(w: *mut BytesWriter) -> *mut c_void {
    unsafe {
        let b = bw(w);
        if b.buf.capacity() == 0 {
            b.buf.reserve(1);
        }
        b.buf.as_mut_ptr() as *mut c_void
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytesWriter_GetSize(w: *mut BytesWriter) -> Py_ssize_t {
    unsafe { bw(w).buf.len() as Py_ssize_t }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytesWriter_FinishWithSize(w: *mut BytesWriter, size: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let b = Box::from_raw(w);
        if size < 0 || size as usize > b.buf.len() {
            let _ = value_error("invalid size");
            return std::ptr::null_mut();
        }
        let data = &b.buf[..size as usize];
        if b.bytearray {
            return ret(crate::types::bytearray::new_bytearray(data));
        }
        ret(bytes::try_new_bytes(data))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytesWriter_Finish(w: *mut BytesWriter) -> *mut PyObject {
    unsafe {
        let n = bw(w).buf.len() as Py_ssize_t;
        PyBytesWriter_FinishWithSize(w, n)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytesWriter_FinishWithPointer(w: *mut BytesWriter, buf: *mut c_void) -> *mut PyObject {
    unsafe {
        let start = bw(w).buf.as_ptr() as usize;
        let size = (buf as usize).wrapping_sub(start) as Py_ssize_t;
        if (buf as usize) < start || size as usize > bw(w).buf.len() {
            let _ = value_error("invalid end pointer");
            PyBytesWriter_Discard(w);
            return std::ptr::null_mut();
        }
        PyBytesWriter_FinishWithSize(w, size)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytesWriter_Resize(w: *mut BytesWriter, size: Py_ssize_t) -> c_int {
    unsafe {
        if size < 0 {
            let _ = value_error("size must be >= 0");
            return -1;
        }
        ret_i(bw_set_len(&mut bw(w).buf, size as usize))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytesWriter_Grow(w: *mut BytesWriter, size: Py_ssize_t) -> c_int {
    unsafe {
        let n = (bw(w).buf.len() as Py_ssize_t).saturating_add(size);
        if n < 0 {
            let _ = value_error("invalid size");
            return -1;
        }
        PyBytesWriter_Resize(w, n)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytesWriter_GrowAndUpdatePointer(w: *mut BytesWriter, size: Py_ssize_t, buf: *mut c_void) -> *mut c_void {
    unsafe {
        let off = (buf as usize).wrapping_sub(bw(w).buf.as_ptr() as usize);
        if PyBytesWriter_Grow(w, size) < 0 {
            return std::ptr::null_mut();
        }
        bw(w).buf.as_mut_ptr().add(off) as *mut c_void
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytesWriter_WriteBytes(w: *mut BytesWriter, data: *const c_void, size: Py_ssize_t) -> c_int {
    unsafe {
        let n = if size < 0 { libc::strlen(data as *const c_char) } else { size as usize };
        bw(w).buf.extend_from_slice(std::slice::from_raw_parts(data as *const u8, n));
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBytesWriter_Format(w: *mut BytesWriter, fmt: *const c_char, mut args: ...) -> c_int {
    unsafe {
        match bytes_format(fmt, &mut args) {
            Ok(v) => {
                bw(w).buf.extend_from_slice(&v);
                0
            }
            Err(_) => -1,
        }
    }
}

/// Data of a C-contiguous buffer exporter (None if `o` does not export one).
pub(crate) unsafe fn contiguous_bytes(o: &PyObject) -> Option<Vec<u8>> {
    unsafe {
        if super::buffer::PyObject_CheckBuffer(o.as_ptr()) == 0 {
            return None;
        }
        let mut view: Py_buffer = std::mem::zeroed();
        if super::buffer::PyObject_GetBuffer(o.as_ptr(), &mut view, 0) < 0 {
            clear_exc();
            return None;
        }
        let ok = super::buffer::PyBuffer_IsContiguous(&view, b'C' as c_char) != 0;
        let v = if !ok {
            None
        } else if view.buf.is_null() || view.len == 0 {
            Some(Vec::new())
        } else {
            Some(std::slice::from_raw_parts(view.buf as *const u8, view.len as usize).to_vec())
        };
        super::buffer::PyBuffer_Release(&mut view);
        v
    }
}
