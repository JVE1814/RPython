//! `_struct`: pack/unpack of C structs (accelerator behind Lib/struct.py).

use crate::abstract_ as ab;
use crate::args::{self, Params};
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::*;
use crate::types::{bool_, bytearray, bytes, complex, dict, float, int, int_methods, none, str as pystr, tuple};
use std::ffi::{c_int, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering};

// ------------------------------------------------------------------ module state

static STRUCT_ERROR: crate::subinterp::InterpPtr = crate::subinterp::InterpPtr::new(); // (interp1) per interpreter

fn struct_error_type() -> *mut PyTypeObject {
    STRUCT_ERROR.load(Ordering::Acquire) as *mut PyTypeObject
}

fn serr(msg: &str) -> PyErr {
    new_err(struct_error_type(), msg)
}

macro_rules! sraise {
    ($($arg:tt)*) => {
        return Err(serr(&format!($($arg)*)))
    };
}

// ------------------------------------------------------------------ buffers

/// A contiguous buffer acquired through the buffer protocol (released on drop).
pub struct Buf {
    view: Py_buffer,
    obj: *mut PyObject,
    release: Option<unsafe extern "C" fn(*mut PyObject, *mut Py_buffer)>,
}
impl Buf {
    pub fn get(o: &PyObject, writable: bool) -> PyResult<Buf> {
        unsafe {
            let bp = o.ty().tp_as_buffer;
            if !bp.is_null() {
                if let Some(gb) = (*bp).bf_getbuffer {
                    let mut view: Py_buffer = std::mem::zeroed();
                    let flags = if writable { 1 } else { 0 };
                    if gb(o.as_ptr(), &mut view, flags) < 0 {
                        if !writable {
                            return Err(PyErr);
                        }
                        clear_exc();
                        return Err(Self::type_err(o, writable));
                    }
                    let b = Buf { view, obj: o.to_ref().into_raw(), release: (*bp).bf_releasebuffer };
                    if writable && b.view.readonly != 0 {
                        drop(b);
                        return Err(Self::type_err(o, writable));
                    }
                    if !b.view.strides.is_null() && b.view.ndim > 0 {
                        // require C-contiguity
                        let mut expect = b.view.itemsize.max(1);
                        let mut ok = true;
                        for i in (0..b.view.ndim as usize).rev() {
                            let sh = *b.view.shape.add(i);
                            let st = *b.view.strides.add(i);
                            if sh > 1 && st != expect {
                                ok = false;
                            }
                            expect *= sh;
                        }
                        if !ok {
                            drop(b);
                            if !writable {
                                raise!(BufferError, "memoryview: underlying buffer is not C-contiguous");
                            }
                            return Err(Self::type_err(o, writable));
                        }
                    }
                    return Ok(b);
                }
            }
            if bytearray::is_bytearray(o) || (bytes::is_bytes(o) && !writable) {
                let d = if bytes::is_bytes(o) { bytes::bytes_data(o) } else { bytearray::bytearray_data(o) };
                let mut view: Py_buffer = std::mem::zeroed();
                view.buf = d.as_ptr() as *mut c_void;
                view.len = d.len() as isize;
                view.itemsize = 1;
                return Ok(Buf { view, obj: o.to_ref().into_raw(), release: None });
            }
        }
        Err(Self::type_err(o, writable))
    }
    fn type_err(o: &PyObject, writable: bool) -> PyErr {
        if writable {
            err!(TypeError, "argument must be read-write bytes-like object, not {}", tname(o))
        } else {
            err!(TypeError, "a bytes-like object is required, not '{}'", tname(o))
        }
    }
    pub fn len(&self) -> usize {
        self.view.len as usize
    }
    pub fn data(&self) -> &[u8] {
        if self.view.len == 0 {
            return &[];
        }
        unsafe { std::slice::from_raw_parts(self.view.buf as *const u8, self.view.len as usize) }
    }
    pub fn data_mut(&mut self) -> &mut [u8] {
        if self.view.len == 0 {
            return &mut [];
        }
        unsafe { std::slice::from_raw_parts_mut(self.view.buf as *mut u8, self.view.len as usize) }
    }
}
impl Drop for Buf {
    fn drop(&mut self) {
        unsafe {
            if let Some(r) = self.release {
                r(self.obj, &mut self.view);
            }
            if !self.view.obj.is_null() {
                decref(self.view.obj);
            }
            decref(self.obj);
        }
    }
}

// ------------------------------------------------------------------ format parsing

#[derive(Clone, Copy, PartialEq, Debug)]
enum Order {
    Native,
    Little,
    Big,
}

#[derive(Clone, Copy, Debug)]
struct Code {
    c: u8,
    offset: usize,
    size: usize,
    /// repeat count (s/p: always 1, with `size` = string length)
    num: usize,
}

#[derive(Clone, Debug)]
struct Fmt {
    codes: Vec<Code>,
    size: usize,
    nitems: usize,
    order: Order,
    native: bool,
}

fn native_size_align(c: u8) -> Option<(usize, usize)> {
    Some(match c {
        b'x' | b'c' | b'b' | b'B' | b'?' | b's' | b'p' => (1, 1),
        b'h' | b'H' => (2, 2),
        b'i' | b'I' => (4, 4),
        b'l' | b'L' | b'q' | b'Q' | b'n' | b'N' | b'P' => (8, 8),
        b'e' => (2, 2),
        b'f' => (4, 4),
        b'd' => (8, 8),
        b'F' => (8, 4),
        b'D' => (16, 8),
        _ => return None,
    })
}
fn std_size(c: u8) -> Option<usize> {
    Some(match c {
        b'x' | b'c' | b'b' | b'B' | b'?' | b's' | b'p' => 1,
        b'h' | b'H' | b'e' => 2,
        b'i' | b'I' | b'l' | b'L' | b'f' => 4,
        b'q' | b'Q' | b'd' | b'F' => 8,
        b'D' => 16,
        _ => return None,
    })
}

fn parse_format(f: &[u8]) -> PyResult<Fmt> {
    if f.contains(&0) {
        sraise!("embedded null character");
    }
    let (order, native, mut i) = match f.first() {
        Some(b'@') => (Order::Native, true, 1),
        Some(b'=') => (Order::Native, false, 1),
        Some(b'<') => (Order::Little, false, 1),
        Some(b'>') | Some(b'!') => (Order::Big, false, 1),
        _ => (Order::Native, true, 0),
    };
    let too_long = || serr("total struct size too long");
    let mut size: usize = 0;
    let mut nitems: usize = 0;
    let mut codes = Vec::new();
    while i < f.len() {
        let ch = f[i];
        if ch.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let mut num: usize = 1;
        if ch.is_ascii_digit() {
            let mut n: usize = 0;
            while i < f.len() && f[i].is_ascii_digit() {
                n = n.checked_mul(10).and_then(|v| v.checked_add((f[i] - b'0') as usize)).ok_or_else(too_long)?;
                if n > isize::MAX as usize {
                    return Err(too_long());
                }
                i += 1;
            }
            if i >= f.len() {
                sraise!("repeat count given without format specifier");
            }
            num = n;
        }
        let mut c = f[i];
        i += 1;
        if c == b'Z' {
            match f.get(i) {
                Some(b'f') => c = b'F',
                Some(b'd') => c = b'D',
                _ => sraise!("bad char in struct format"),
            }
            i += 1;
        }
        let (isz, align) = if native {
            match native_size_align(c) {
                Some(v) => v,
                None => sraise!("bad char in struct format"),
            }
        } else {
            match std_size(c) {
                Some(v) => (v, 1),
                None => sraise!("bad char in struct format"),
            }
        };
        if native && align > 1 {
            size = size.checked_add(align - 1).ok_or_else(too_long)? / align * align;
        }
        match c {
            b's' | b'p' => {
                codes.push(Code { c, offset: size, size: num, num: 1 });
                size = size.checked_add(num).ok_or_else(too_long)?;
                nitems += 1;
            }
            b'x' => {
                size = size.checked_add(num).ok_or_else(too_long)?;
            }
            _ => {
                if num > 0 {
                    codes.push(Code { c, offset: size, size: isz, num });
                }
                let add = num.checked_mul(isz).ok_or_else(too_long)?;
                size = size.checked_add(add).ok_or_else(too_long)?;
                nitems += num;
            }
        }
        if size > isize::MAX as usize {
            return Err(too_long());
        }
    }
    Ok(Fmt { codes, size, nitems, order, native })
}

// ------------------------------------------------------------------ half floats

fn pack_half(x: f64) -> PyResult<u16> {
    let sign: u16 = if x.is_sign_negative() { 1 } else { 0 };
    let bits: u16;
    if x.is_nan() {
        bits = 0x7e00;
    } else if x.is_infinite() {
        bits = 0x7c00;
    } else if x == 0.0 {
        bits = 0;
    } else {
        let (mut f, mut e) = crate::modules::math::c_frexp(x.abs());
        f *= 2.0;
        e -= 1;
        if e >= 16 {
            raise!(OverflowError, "float too large to pack with e format");
        } else if e < -25 {
            f = 0.0;
            e = 0;
        } else if e < -14 {
            f = crate::modules::math::c_ldexp(f, 14 + e);
            e = 0;
        } else {
            e += 15;
            f -= 1.0;
        }
        f *= 1024.0;
        let mut b = f as u16;
        if f - b as f64 > 0.5 || (f - b as f64 == 0.5 && b % 2 == 1) {
            b += 1;
            if b == 1024 {
                b = 0;
                e += 1;
                if e == 31 {
                    raise!(OverflowError, "float too large to pack with e format");
                }
            }
        }
        bits = b | ((e as u16) << 10);
    }
    Ok(bits | (sign << 15))
}

fn unpack_half(h: u16) -> f64 {
    let sign = (h >> 15) & 1;
    let e = ((h >> 10) & 0x1f) as i32;
    let f = (h & 0x3ff) as u64;
    if e == 0x1f {
        if f == 0 {
            return if sign != 0 { f64::NEG_INFINITY } else { f64::INFINITY };
        }
        // NaN: keep sign and payload
        let bits = ((sign as u64) << 63) | (0x7ffu64 << 52) | (f << 42);
        return f64::from_bits(bits);
    }
    let mut x = f as f64 / 1024.0;
    let ee = if e == 0 {
        -14
    } else {
        x += 1.0;
        e - 15
    };
    x = crate::modules::math::c_ldexp(x, ee);
    if sign != 0 { -x } else { x }
}

fn pack_float4(x: f64) -> PyResult<f32> {
    let y = x as f32;
    if y.is_infinite() && x.is_finite() {
        raise!(OverflowError, "float too large to pack with f format");
    }
    Ok(y)
}

// ------------------------------------------------------------------ packing

fn get_pylong(v: &PyObject) -> PyResult<PyRef> {
    if int::is_int(v) {
        return Ok(v.to_ref());
    }
    if ab::has_index(v) {
        return ab::index(v);
    }
    sraise!("required argument is not an integer")
}

fn range_err(c: u8, size: usize, signed: bool) -> PyErr {
    let bits = size * 8;
    if signed {
        let max: i128 = (1i128 << (bits - 1)) - 1;
        let min: i128 = -(1i128 << (bits - 1));
        serr(&format!("'{}' format requires {} <= number <= {}", c as char, min, max))
    } else {
        let max: u128 = if bits == 128 { u128::MAX } else { (1u128 << bits) - 1 };
        serr(&format!("'{}' format requires 0 <= number <= {}", c as char, max))
    }
}

fn put_int(out: &mut [u8], v: u64, size: usize, big: bool) {
    let le = v.to_le_bytes();
    if big {
        for k in 0..size {
            out[k] = le[size - 1 - k];
        }
    } else {
        out[..size].copy_from_slice(&le[..size]);
    }
}

fn get_uint(inp: &[u8], size: usize, big: bool) -> u64 {
    let mut v: u64 = 0;
    if big {
        for k in 0..size {
            v = (v << 8) | inp[k] as u64;
        }
    } else {
        for k in (0..size).rev() {
            v = (v << 8) | inp[k] as u64;
        }
    }
    v
}

fn is_signed_code(c: u8) -> bool {
    matches!(c, b'b' | b'h' | b'i' | b'l' | b'q' | b'n')
}

fn pack_one(code: &Code, out: &mut [u8], v: &PyObject, big: bool) -> PyResult<()> {
    let size = code.size;
    match code.c {
        b'c' => {
            let d: Option<&[u8]> = if bytes::is_bytes(v) {
                Some(bytes::bytes_data(v))
            } else if bytearray::is_bytearray(v) {
                Some(bytearray::bytearray_data(v))
            } else {
                None
            };
            match d {
                Some(d) if d.len() == 1 => out[0] = d[0],
                _ => sraise!("char format requires a bytes object of length 1"),
            }
        }
        b's' | b'p' => {
            let d: Vec<u8> = if bytes::is_bytes(v) {
                bytes::bytes_data(v).to_vec()
            } else if bytearray::is_bytearray(v) {
                bytearray::bytearray_data(v).to_vec()
            } else {
                sraise!("argument for '{}' must be a bytes object", code.c as char);
            };
            if code.c == b's' {
                let n = d.len().min(size);
                out[..n].copy_from_slice(&d[..n]);
                for b in &mut out[n..size] {
                    *b = 0;
                }
            } else {
                if size == 0 {
                    return Ok(());
                }
                let n = d.len().min(size - 1);
                out[1..1 + n].copy_from_slice(&d[..n]);
                for b in &mut out[1 + n..size] {
                    *b = 0;
                }
                out[0] = n.min(255) as u8;
            }
        }
        b'?' => {
            out[0] = ab::is_true(v)? as u8;
        }
        b'e' => {
            let x = match float_arg(v) {
                Ok(x) => x,
                Err(e) => return Err(e),
            };
            let h = pack_half(x)?;
            put_int(out, h as u64, 2, big);
        }
        b'f' => {
            let x = float_arg(v)?;
            let y = pack_float4(x)?;
            put_int(out, y.to_bits() as u64, 4, big);
        }
        b'd' => {
            let x = float_arg(v)?;
            put_int(out, x.to_bits(), 8, big);
        }
        b'F' | b'D' => {
            let c = match crate::types::complex_methods::as_ccomplex(v) {
                Ok(c) => c,
                Err(_) => {
                    clear_exc();
                    sraise!("required argument is not a complex");
                }
            };
            if code.c == b'F' {
                let re = pack_float4_c(c.0)?;
                let im = pack_float4_c(c.1)?;
                put_int(&mut out[0..4], re.to_bits() as u64, 4, big);
                put_int(&mut out[4..8], im.to_bits() as u64, 4, big);
            } else {
                put_int(&mut out[0..8], c.0.to_bits(), 8, big);
                put_int(&mut out[8..16], c.1.to_bits(), 8, big);
            }
        }
        b'P' => {
            let i = get_pylong(v)?;
            let val: u64 = if int::sign(&i) < 0 {
                match int::as_i64(&i) {
                    Some(x) => x as u64,
                    None => raise!(OverflowError, "Python int too large to convert to C long"),
                }
            } else {
                match rpy_bigint::conv::to_u64(int_methods::iref(&i)) {
                    Some(x) => x,
                    None => raise!(OverflowError, "Python int too large to convert to C unsigned long"),
                }
            };
            put_int(out, val, 8, big);
        }
        c => {
            // integer codes
            let i = get_pylong(v)?;
            let signed = is_signed_code(c);
            let bits = size * 8;
            let val = int::as_i128(&i);
            let ok = match val {
                Some(x) => {
                    if signed {
                        x >= -(1i128 << (bits - 1)) && x < (1i128 << (bits - 1))
                    } else {
                        x >= 0 && x < (1i128 << bits)
                    }
                }
                None => false,
            };
            if !ok {
                return Err(range_err(c, size, signed));
            }
            put_int(out, val.unwrap() as u64, size, big);
        }
    }
    Ok(())
}

fn pack_float4_c(x: f64) -> PyResult<f32> {
    pack_float4(x)
}

fn float_arg(v: &PyObject) -> PyResult<f64> {
    match crate::types::float_math::as_double(v) {
        Ok(x) => Ok(x),
        Err(_) => {
            clear_exc();
            sraise!("required argument is not a float")
        }
    }
}

fn unpack_one(code: &Code, inp: &[u8], big: bool, out: &mut Vec<PyRef>) {
    let size = code.size;
    match code.c {
        b'c' => out.push(bytes::new_bytes(&inp[..1])),
        b's' => out.push(bytes::new_bytes(&inp[..size])),
        b'p' => {
            if size == 0 {
                out.push(bytes::new_bytes(b""));
            } else {
                let n = (inp[0] as usize).min(size - 1);
                out.push(bytes::new_bytes(&inp[1..1 + n]));
            }
        }
        b'?' => out.push(bool_::new_bool(inp[0] != 0)),
        b'e' => out.push(float::new_float(unpack_half(get_uint(inp, 2, big) as u16))),
        b'f' => out.push(float::new_float(f32::from_bits(get_uint(inp, 4, big) as u32) as f64)),
        b'd' => out.push(float::new_float(f64::from_bits(get_uint(inp, 8, big)))),
        b'F' => {
            let re = f32::from_bits(get_uint(&inp[0..4], 4, big) as u32) as f64;
            let im = f32::from_bits(get_uint(&inp[4..8], 4, big) as u32) as f64;
            out.push(complex::new_complex(re, im));
        }
        b'D' => {
            let re = f64::from_bits(get_uint(&inp[0..8], 8, big));
            let im = f64::from_bits(get_uint(&inp[8..16], 8, big));
            out.push(complex::new_complex(re, im));
        }
        b'P' => out.push(int::new_int_u128(get_uint(inp, 8, big) as u128)),
        c => {
            let u = get_uint(inp, size, big);
            if is_signed_code(c) {
                let shift = 64 - size * 8;
                let v = ((u << shift) as i64) >> shift;
                out.push(int::new_int(v));
            } else {
                out.push(int::new_int_u128(u as u128));
            }
        }
    }
}

impl Fmt {
    fn big(&self) -> bool {
        self.order == Order::Big
    }
    fn pack_into(&self, out: &mut [u8], args: &[&PyObject]) -> PyResult<()> {
        let big = self.big();
        let mut ai = 0;
        for code in &self.codes {
            let mut off = code.offset;
            for _ in 0..code.num {
                pack_one(code, &mut out[off..], args[ai], big)?;
                ai += 1;
                off += code.size;
            }
        }
        Ok(())
    }
    fn unpack(&self, inp: &[u8]) -> PyRef {
        let big = self.big();
        let mut out = Vec::with_capacity(self.nitems);
        for code in &self.codes {
            let mut off = code.offset;
            for _ in 0..code.num {
                unpack_one(code, &inp[off..], big, &mut out);
                off += code.size;
            }
        }
        tuple::new_tuple(out)
    }
    fn check_args(&self, n: usize) -> PyResult<()> {
        if n != self.nitems {
            sraise!("pack expected {} items for packing (got {})", self.nitems, n);
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ Struct type

#[repr(C)]
pub struct StructObj {
    ob_base: PyObject,
    /// Packed size and number of items, right after the header: C test helpers
    /// (_testbuffer) read them directly from the object.
    s_size: isize,
    s_len: isize,
    st: *mut StructState,
}
struct StructState {
    fmt: Fmt,
    format: PyRef,
}

#[unsafe(no_mangle)]
pub static mut _PyStruct_Type: PyTypeObject = PyTypeObject::ZERO;
static mut UNPACKITER_TYPE: PyTypeObject = PyTypeObject::ZERO;

fn struct_type() -> *mut PyTypeObject {
    &raw mut _PyStruct_Type
}
fn is_struct(o: &PyObject) -> bool {
    is_subtype(o.type_ptr(), struct_type())
}
fn state(o: &PyObject) -> PyResult<&StructState> {
    unsafe {
        let p = (*(o.as_ptr() as *mut StructObj)).st;
        if p.is_null() {
            raise!(RuntimeError, "Struct object is not initialized");
        }
        Ok(&*p)
    }
}

/// Convert a format argument to (format text as str object, ascii bytes).
fn format_bytes(f: &PyObject) -> PyResult<(PyRef, Vec<u8>)> {
    if pystr::is_str(f) {
        let d = pystr::str_data(f);
        let mut v = Vec::with_capacity(d.len());
        for c in d.iter() {
            if c >= 128 {
                raise!(ValueError, "non-ASCII character in struct format");
            }
            v.push(c as u8);
        }
        return Ok((f.to_ref(), v));
    }
    if bytes::is_bytes(f) {
        let d = bytes::bytes_data(f).to_vec();
        if d.iter().any(|&b| b >= 128) {
            raise!(ValueError, "non-ASCII character in struct format");
        }
        let s = pystr::new_str(std::str::from_utf8(&d).unwrap_or(""));
        return Ok((s, d));
    }
    raise!(TypeError, "Struct() argument 1 must be a str or bytes object, not {}", tname(f))
}

fn build_state(f: &PyObject) -> PyResult<Box<StructState>> {
    let (s, b) = format_bytes(f)?;
    let fmt = parse_format(&b)?;
    Ok(Box::new(StructState { fmt, format: s }))
}

unsafe fn set_state(o: &PyObject, st: Option<Box<StructState>>) {
    unsafe {
        let so = &mut *(o.as_ptr() as *mut StructObj);
        (so.s_size, so.s_len) = match &st {
            Some(s) => (s.fmt.size as isize, s.fmt.nitems as isize),
            None => (0, 0),
        };
        let old = std::mem::replace(&mut so.st, st.map(Box::into_raw).unwrap_or(std::ptr::null_mut()));
        if !old.is_null() {
            drop(Box::from_raw(old));
        }
    }
}

fn init_overridden(tp: *mut PyTypeObject) -> bool {
    unsafe { (*tp).tp_init.map(|f| f as usize) != Some(struct_init as usize) }
}
fn new_overridden(tp: *mut PyTypeObject) -> bool {
    unsafe { (*tp).tp_new.map(|f| f as usize) != Some(struct_new as usize) }
}

/// Parse the (format) argument of Struct.__new__/__init__: Ok(Some(fmt)) / Ok(None) if missing,
/// Err(message) for a call-signature problem.
fn struct_args(args: &PyObject, kw: *mut PyObject) -> Result<Option<PyRef>, String> {
    let pos = tuple::tuple_items(args);
    let nkw = if kw.is_null() { 0 } else { dict::dict_len(unsafe { &*kw }) };
    let total = pos.len() + nkw;
    if total > 1 {
        return Err(format!("Struct() takes at most 1 argument ({} given)", total));
    }
    if pos.len() == 1 {
        return Ok(Some(pos[0].to_ref()));
    }
    if nkw == 1 {
        if let Some(v) = dict::get_item_rs(unsafe { &*kw }, "format") {
            return Ok(Some(v));
        }
        return Err("__unexpected__".into());
    }
    Ok(None)
}

unsafe extern "C" fn struct_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
            let op = alloc(tp, 0);
            if op.is_null() {
                return Err(PyErr);
            }
            (*(op as *mut StructObj)).st = std::ptr::null_mut();
            let obj = PyRef::from_raw(op);
            // When only __init__ is customised, the class call hands __new__ the
            // arguments meant for __init__, so bad ones merely warn here.  A custom
            // __new__ calls us with arguments of its own choosing: check strictly.
            let lenient = init_overridden(tp) && !new_overridden(tp);
            let warn = |msg: &str| crate::modules::warnings::warn(crate::types::exceptions::DeprecationWarning(), msg, 1);
            match struct_args(&*args, kw) {
                Ok(Some(f)) => match build_state(&f) {
                    Ok(st) => set_state(&obj, Some(st)),
                    Err(e) => {
                        if !lenient {
                            return Err(e);
                        }
                        let ex = take_exc().map(|e| ab::str_string(&e).unwrap_or_default()).unwrap_or_default();
                        warn(&format!("Invalid 'format' argument for Struct.__new__(): {}", ex))?;
                    }
                },
                Ok(None) => {
                    if lenient {
                        warn("Struct() missing required argument 'format' (pos 1)")?;
                    } else if new_overridden(tp) {
                        // an explicit super().__new__(cls) without a format
                        raise!(TypeError, "Struct() missing required argument 'format' (pos 1)");
                    }
                }
                Err(m) => {
                    if !lenient {
                        if m == "__unexpected__" {
                            let k = dict::keys_vec(&*kw);
                            let name = k.first().map(|x| pystr::to_string_lossy(x)).unwrap_or_default();
                            raise!(TypeError, "Struct() got an unexpected keyword argument '{}'", name);
                        }
                        raise!(TypeError, "{}", m);
                    }
                    if m == "__unexpected__" {
                        warn("Struct() missing required argument 'format' (pos 1)")?;
                    } else {
                        warn(&m)?;
                    }
                }
            }
            Ok(obj)
        })();
        into_c(r)
    }
}

unsafe extern "C" fn struct_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let r = (|| -> PyResult<()> {
            let o = &*o;
            // A subclass with its own __new__ (and the inherited __init__) has been
            // fully set up by that __new__; the class call's arguments are for it.
            if new_overridden(o.type_ptr()) && !init_overridden(o.type_ptr()) && !(*(o.as_ptr() as *mut StructObj)).st.is_null() {
                return Ok(());
            }
            let f = match struct_args(&*args, kw) {
                Ok(Some(f)) => f,
                Ok(None) => raise!(TypeError, "Struct() missing required argument 'format' (pos 1)"),
                Err(m) => {
                    if m == "__unexpected__" {
                        let k = dict::keys_vec(&*kw);
                        let name = k.first().map(|x| pystr::to_string_lossy(x)).unwrap_or_default();
                        raise!(TypeError, "Struct() got an unexpected keyword argument '{}'", name);
                    }
                    raise!(TypeError, "{}", m);
                }
            };
            let cur = (*(o.as_ptr() as *mut StructObj)).st;
            if !cur.is_null() {
                // same format: nothing to do
                let same = match format_bytes(&f) {
                    Ok((_, b)) => {
                        let (_, cb) = format_bytes(&(*cur).format).unwrap_or((none::none_ref(), Vec::new()));
                        b == cb
                    }
                    Err(_) => {
                        clear_exc();
                        false
                    }
                };
                if same {
                    return Ok(());
                }
                let msg = if init_overridden(o.type_ptr()) {
                    "Different format arguments for __new__() and __init__() methods of Struct"
                } else {
                    "Re-initialization of Struct by calling the __init__() method will not work in future Python versions"
                };
                crate::modules::warnings::warn(crate::types::exceptions::FutureWarning(), msg, 1)?;
            }
            let st = build_state(&f)?;
            set_state(o, Some(st));
            Ok(())
        })();
        int_into_c(r)
    }
}

unsafe extern "C" fn struct_dealloc(o: *mut PyObject) {
    unsafe {
        let tp = (*o).type_ptr();
        set_state(&*o, None);
        generic_free_and_type_decref(o, tp);
    }
}

unsafe extern "C" fn struct_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let st = state(&*o)?;
            let r = ab::repr_string(&st.format)?;
            Ok(pystr::new_str(&format!("{}({})", type_name((*o).ty()), r)))
        })();
        into_c(r)
    }
}

fn s_pack(o: &PyObject, a: &[&PyObject]) -> PyResult {
    let st = state(o)?;
    st.fmt.check_args(a.len())?;
    let mut out = vec![0u8; st.fmt.size];
    st.fmt.pack_into(&mut out, a)?;
    Ok(bytes::new_bytes(&out))
}

fn offset_arg(o: &PyObject) -> PyResult<isize> {
    ab::index_as_ssize(o, Some(crate::types::exceptions::OverflowError()))
}

fn do_pack_into(st: &StructState, a: &[&PyObject]) -> PyResult {
    let size = st.fmt.size;
    if a.len() < 2 {
        if a.is_empty() {
            raise!(TypeError, "pack_into expected buffer argument");
        }
        raise!(TypeError, "pack_into expected offset argument");
    }
    if a.len() - 2 != st.fmt.nitems {
        sraise!("pack_into expected {} items for packing (got {})", st.fmt.nitems, a.len() - 2);
    }
    let mut buf = Buf::get(a[0], true)?;
    let mut offset = offset_arg(a[1])?;
    let blen = buf.len() as isize;
    if offset < 0 {
        if offset + size as isize > 0 {
            sraise!("no space to pack {} bytes at offset {}", size, offset);
        }
        if offset + blen < 0 {
            sraise!("offset {} out of range for {}-byte buffer", offset, blen);
        }
        offset += blen;
    }
    if (blen - offset) < size as isize {
        let need = (offset as i128) + size as i128;
        sraise!(
            "pack_into requires a buffer of at least {} bytes for packing {} bytes at offset {} (actual buffer size is {})",
            need,
            size,
            offset,
            blen
        );
    }
    let mut tmp = vec![0u8; size];
    st.fmt.pack_into(&mut tmp, &a[2..])?;
    let off = offset as usize;
    buf.data_mut()[off..off + size].copy_from_slice(&tmp);
    Ok(none::none_ref())
}

fn s_pack_into(o: &PyObject, a: &[&PyObject]) -> PyResult {
    let st = state(o)?;
    do_pack_into(st, a)
}

fn do_unpack(st: &StructState, buffer: &PyObject) -> PyResult {
    let buf = Buf::get(buffer, false)?;
    if buf.len() != st.fmt.size {
        sraise!("unpack requires a buffer of {} bytes", st.fmt.size);
    }
    Ok(st.fmt.unpack(buf.data()))
}

fn s_unpack(o: &PyObject, b: &PyObject) -> PyResult {
    let st = state(o)?;
    do_unpack(st, b)
}

fn do_unpack_from(st: &StructState, buffer: &PyObject, offset: Option<&PyObject>) -> PyResult {
    let buf = Buf::get(buffer, false)?;
    let mut offset = match offset {
        Some(o) => offset_arg(o)?,
        None => 0,
    };
    let size = st.fmt.size;
    let blen = buf.len() as isize;
    if offset < 0 {
        if offset + size as isize > 0 {
            sraise!("not enough data to unpack {} bytes at offset {}", size, offset);
        }
        if offset + blen < 0 {
            sraise!("offset {} out of range for {}-byte buffer", offset, blen);
        }
        offset += blen;
    }
    if (blen - offset) < size as isize {
        let need = (offset as i128) + size as i128;
        sraise!(
            "unpack_from requires a buffer of at least {} bytes for unpacking {} bytes at offset {} (actual buffer size is {})",
            need,
            size,
            offset,
            blen
        );
    }
    let off = offset as usize;
    Ok(st.fmt.unpack(&buf.data()[off..off + size]))
}

fn s_unpack_from(o: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("unpack_from", &["buffer", "offset"], 0, 1);
    let [b, off] = args::parse::<2>(&P, a, kw)?;
    let st = state(o)?;
    do_unpack_from(st, b.unwrap(), off)
}

// unpack iterator
#[repr(C)]
struct UnpackIter {
    ob_base: PyObject,
    structobj: *mut PyObject,
    data: *mut Vec<u8>,
    index: usize,
}

fn do_iter_unpack(so: &PyObject, buffer: &PyObject) -> PyResult {
    let st = state(so)?;
    if st.fmt.size == 0 {
        sraise!("cannot iteratively unpack with a struct of length 0");
    }
    let buf = Buf::get(buffer, false)?;
    if buf.len() % st.fmt.size != 0 {
        sraise!("iterative unpacking requires a buffer of a multiple of {} bytes", st.fmt.size);
    }
    let data = Box::new(buf.data().to_vec());
    unsafe {
        let tp = &raw mut UNPACKITER_TYPE;
        let op = crate::alloc::alloc_object(tp, std::mem::size_of::<UnpackIter>());
        let it = &mut *(op as *mut UnpackIter);
        it.structobj = so.to_ref().into_raw();
        it.data = Box::into_raw(data);
        it.index = 0;
        Ok(PyRef::from_raw(op))
    }
}
fn s_iter_unpack(o: &PyObject, b: &PyObject) -> PyResult {
    do_iter_unpack(o, b)
}
unsafe extern "C" fn unpackiter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let it = &mut *(o as *mut UnpackIter);
        if it.data.is_null() {
            return std::ptr::null_mut();
        }
        let st = match state(&*it.structobj) {
            Ok(s) => s,
            Err(_) => return std::ptr::null_mut(),
        };
        let d = &*it.data;
        if it.index + st.fmt.size > d.len() {
            drop(Box::from_raw(it.data));
            it.data = std::ptr::null_mut();
            return std::ptr::null_mut();
        }
        let r = st.fmt.unpack(&d[it.index..it.index + st.fmt.size]);
        it.index += st.fmt.size;
        r.into_raw()
    }
}
unsafe extern "C" fn unpackiter_dealloc(o: *mut PyObject) {
    unsafe {
        let it = &mut *(o as *mut UnpackIter);
        if !it.data.is_null() {
            drop(Box::from_raw(it.data));
        }
        xdecref(it.structobj);
        crate::alloc::free_object(o);
    }
}
unsafe extern "C" fn self_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}
fn unpackiter_length_hint(o: &PyObject) -> PyResult {
    unsafe {
        let it = &*(o.as_ptr() as *mut UnpackIter);
        if it.data.is_null() {
            return Ok(int::new_int(0));
        }
        let st = state(&*it.structobj)?;
        let d = &*it.data;
        Ok(int::new_int(((d.len() - it.index) / st.fmt.size) as i64))
    }
}

fn s_sizeof(o: &PyObject) -> PyResult {
    let st = state(o)?;
    let n = st.fmt.codes.len();
    Ok(int::new_int((std::mem::size_of::<PyObject>() + 48 + 32 * (n + 1)) as i64))
}
fn get_format(o: &PyObject) -> PyResult {
    unsafe {
        let p = (*(o.as_ptr() as *mut StructObj)).st;
        if p.is_null() {
            raise!(AttributeError, "'Struct' object has no attribute 'format'");
        }
        Ok((*p).format.clone())
    }
}
fn get_size(o: &PyObject) -> PyResult {
    unsafe {
        let p = (*(o.as_ptr() as *mut StructObj)).st;
        if p.is_null() {
            return Ok(int::new_int(-1));
        }
        Ok(int::new_int((*p).fmt.size as i64))
    }
}

// ------------------------------------------------------------------ module-level functions with cache

static CACHE: Mutex<Vec<((bool, Vec<u8>), PyRef)>> = Mutex::new(Vec::new());
const MAXCACHE: usize = 100;

fn cached_struct(f: &PyObject) -> PyResult<PyRef> {
    if is_struct(f) {
        return Ok(f.to_ref());
    }
    let key = if pystr::is_str(f) {
        Some((false, pystr::str_data(f).iter().map(|c| if c < 256 { c as u8 } else { 0xff }).collect::<Vec<u8>>()))
    } else if bytes::is_bytes(f) {
        Some((true, bytes::bytes_data(f).to_vec()))
    } else {
        None
    };
    if let Some(k) = &key {
        let c = CACHE.lock().unwrap();
        if let Some((_, s)) = c.iter().find(|(kk, _)| kk == k) {
            return Ok(s.clone());
        }
    }
    let s = unsafe {
        let args = tuple::new_tuple(vec![f.to_ref()]);
        from_c(struct_new(struct_type(), args.as_ptr(), std::ptr::null_mut()))?
    };
    if let Some(k) = key {
        let mut c = CACHE.lock().unwrap();
        if c.len() >= MAXCACHE {
            c.clear();
        }
        c.push((k, s.clone()));
    }
    Ok(s)
}

fn m_clearcache(_m: &PyObject) -> PyResult {
    CACHE.lock().unwrap().clear();
    Ok(none::none_ref())
}
fn m_calcsize(_m: &PyObject, f: &PyObject) -> PyResult {
    let s = cached_struct(f)?;
    Ok(int::new_int(state(&s)?.fmt.size as i64))
}
fn m_pack(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    if a.is_empty() {
        raise!(TypeError, "pack expected at least 1 argument, got 0");
    }
    let s = cached_struct(a[0])?;
    s_pack(&s, &a[1..])
}
fn m_pack_into(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    if a.is_empty() {
        raise!(TypeError, "pack_into expected at least 1 argument, got 0");
    }
    let s = cached_struct(a[0])?;
    do_pack_into(state(&s)?, &a[1..])
}
fn m_unpack(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("unpack", a, 2, 2)?;
    let s = cached_struct(a[0])?;
    do_unpack(state(&s)?, a[1])
}
fn m_unpack_from(_m: &PyObject, a: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("unpack_from", &["format", "buffer", "offset"], 1, 2);
    let [f, b, off] = args::parse::<3>(&P, a, kw)?;
    let s = cached_struct(f.unwrap())?;
    do_unpack_from(state(&s)?, b.unwrap(), off)
}
fn m_iter_unpack(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    args::check_pos("iter_unpack", a, 2, 2)?;
    let s = cached_struct(a[0])?;
    do_iter_unpack(&s, a[1])
}

static INIT: std::sync::Once = std::sync::Once::new();

fn init_types() -> PyResult<()> {
    let mut res = Ok(());
    INIT.call_once(|| unsafe {
        let t = &mut *struct_type();
        t.tp_name = c"_struct.Struct".as_ptr();
        t.tp_basicsize = std::mem::size_of::<StructObj>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE;
        t.tp_new = Some(struct_new);
        t.tp_init = Some(struct_init);
        t.tp_dealloc = Some(struct_dealloc);
        t.tp_repr = Some(struct_repr);
        t.tp_alloc = Some(generic_alloc);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_doc = c"Struct(format)\n--\n\nCreate a compiled struct object.\n\nReturn a new Struct object which writes and reads binary data according to\nthe format string.\n\nSee help(struct) for more on format strings.".as_ptr();
        t.tp_methods = crate::mdefs![
            ("iter_unpack", O, s_iter_unpack, "iter_unpack($self, buffer, /)\n--\n\nReturn an iterator yielding tuples.\n\nTuples are unpacked from the given bytes source, like a repeated\ninvocation of unpack_from().\n\nRequires that the bytes length be a multiple of the struct size."),
            ("pack", FAST, s_pack, "pack($self, /, *values)\n--\n\nPack the values according to the format and return the bytes."),
            ("pack_into", FAST, s_pack_into, "pack_into($self, buffer, offset, /, *values)\n--\n\nPack the values into a writable buffer, starting at offset."),
            ("unpack", O, s_unpack, "unpack($self, buffer, /)\n--\n\nReturn a tuple containing unpacked values.\n\nUnpack according to the format string Struct.format. The buffer's size\nin bytes must be Struct.size.\n\nSee help(struct) for more on format strings."),
            ("unpack_from", FASTKW, s_unpack_from, "unpack_from($self, /, buffer, offset=0)\n--\n\nReturn a tuple containing unpacked values.\n\nValues are unpacked according to the format string Struct.format.\n\nThe buffer's size in bytes, starting at position offset, must be\nat least Struct.size.\n\nSee help(struct) for more on format strings."),
            ("__sizeof__", NOARGS, s_sizeof, "__sizeof__($self, /)\n--\n\nReturn the memory used by the Struct object, in bytes."),
        ];
        t.tp_getset = crate::getset_table![
            ("format", get_format; "struct format string"),
            ("size", get_size; "struct size in bytes"),
        ];
        let u = &mut *(&raw mut UNPACKITER_TYPE);
        u.tp_name = c"_struct.unpack_iterator".as_ptr();
        u.tp_basicsize = std::mem::size_of::<UnpackIter>() as isize;
        u.tp_flags = Py_TPFLAGS_DEFAULT;
        u.tp_dealloc = Some(unpackiter_dealloc);
        u.tp_iter = Some(self_iter);
        u.tp_iternext = Some(unpackiter_next);
        u.tp_getattro = Some(object_generic_getattro);
        u.tp_methods = crate::mdefs![("__length_hint__", NOARGS, unpackiter_length_hint)];
        res = type_ready(struct_type()).and_then(|_| type_ready(&raw mut UNPACKITER_TYPE));
    });
    res
}

pub fn init_module() -> PyResult {
    init_types()?;
    let m = crate::types::module::new_module("_struct")?;
    let d = crate::types::module::module_dict(&m);
    dict::set_item_str(&d, "__doc__", &pystr::new_str(STRUCT_DOC))?;
    if STRUCT_ERROR.load(Ordering::Acquire).is_null() {
        let ns = dict::new_dict();
        dict::set_item_str(&ns, "__module__", &pystr::new_str("struct"))?;
        let bases = tuple::new_tuple(vec![type_ref(crate::types::exceptions::Exception())]);
        let e = ab::call(type_obj(type_type()), &[&pystr::new_str("error"), &bases, &ns])?;
        STRUCT_ERROR.store(e.into_raw(), Ordering::Release);
    }
    let err = unsafe { PyRef::from_borrowed(STRUCT_ERROR.load(Ordering::Acquire)) };
    dict::set_item_str(&d, "error", &err)?;
    dict::set_item_str(&d, "Struct", &type_ref(struct_type()))?;
    let methods = crate::mdefs![
        ("_clearcache", NOARGS, m_clearcache, "_clearcache($module, /)\n--\n\nClear the internal cache."),
        ("calcsize", O, m_calcsize, "calcsize($module, format, /)\n--\n\nReturn size in bytes of the struct described by the format string."),
        ("iter_unpack", FAST, m_iter_unpack, "iter_unpack($module, format, buffer, /)\n--\n\nReturn an iterator yielding tuples unpacked from the given bytes.\n\nThe bytes are unpacked according to the format string, like\na repeated invocation of unpack_from().\n\nRequires that the bytes length be a multiple of the format struct size."),
        ("pack", FAST, m_pack, "pack($module, format, /, *values)\n--\n\nPack the values according to format and return the bytes."),
        ("pack_into", FAST, m_pack_into, "pack_into($module, format, buffer, offset, /, *values)\n--\n\nPack the values according to format into a writable buffer at offset."),
        ("unpack", FAST, m_unpack, "unpack($module, format, buffer, /)\n--\n\nReturn a tuple containing values unpacked according to the format string.\n\nThe buffer's size in bytes must be calcsize(format).\n\nSee help(struct) for more on format strings."),
        ("unpack_from", FASTKW, m_unpack_from, "unpack_from($module, format, /, buffer, offset=0)\n--\n\nReturn a tuple containing values unpacked according to the format string.\n\nThe buffer's size, minus offset, must be at least calcsize(format).\n\nSee help(struct) for more on format strings."),
    ];
    crate::builtins::add_methods_to_dict(&d, methods, Some(&m))?;
    Ok(m)
}

const STRUCT_DOC: &str = "Functions to convert between Python values and C structs.\nPython bytes objects are used to hold the data representing the C struct\nand also as format strings (explained below) to describe the layout of data\nin the C struct.\n\nThe optional first format char indicates byte order, size and alignment:\n  @: native order, size & alignment (default)\n  =: native order, std. size & alignment\n  <: little-endian, std. size & alignment\n  >: big-endian, std. size & alignment\n  !: same as >\n\nThe remaining chars indicate types of args and must match exactly;\nthese can be preceded by a decimal repeat count:\n  x: pad byte (no data); c:char; b:signed byte; B:unsigned byte;\n  ?: _Bool (requires C99; if not available, char is used instead)\n  h:short; H:unsigned short; i:int; I:unsigned int;\n  l:long; L:unsigned long; f:float; d:double; e:half-float.\n  F:float complex; D:double complex.\nSpecial cases (preceding decimal count indicates length):\n  s:string (array of char); p: pascal string (with count byte).\nSpecial cases (only available in native format):\n  n:ssize_t; N:size_t;\n  P:an integer type that is wide enough to hold a pointer.\nSpecial case (not in native mode unless 'long long' in platform C):\n  q:long long; Q:unsigned long long\nWhitespace between formats is ignored.\n\nThe variable struct.error is an exception raised on errors.\n";
