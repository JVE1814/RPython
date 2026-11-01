//! Simple C types: per-code memory accessors ("field descriptors"), the
//! `_SimpleCData` base class and the `PyCSimpleType` metaclass.

use super::cdata::*;
use super::*;
use rpy_runtime::types::{bool_, bytes, float};

pub const SIMPLE_TYPE_CHARS: &str = "cbBhHiIlLdfuzZqQPOv?g";

/// (size, alignment) of a simple type code. Complex codes are 'F', 'D', 'G'
/// internally ("Zf", "Zd", "Zg" in `_type_`).
pub fn code_size(code: u8) -> Option<(usize, usize)> {
    Some(match code {
        b'c' | b'b' | b'B' | b'?' => (1, 1),
        b'h' | b'H' | b'v' => (2, 2),
        b'i' | b'I' | b'f' => (4, 4),
        b'u' => (4, 4),
        b'l' | b'L' | b'q' | b'Q' | b'd' | b'P' | b'z' | b'Z' | b'O' | b's' | b'U' => (8, 8),
        b'g' => (16, 16),
        b'F' => (8, 4),
        b'D' => (16, 8),
        b'G' => (32, 16),
        _ => return None,
    })
}

pub fn code_ffi(code: u8) -> *mut libffi::FfiType {
    let t = libffi::types();
    match code {
        b'c' | b'B' | b'?' => t.uint8,
        b'b' => t.sint8,
        b'h' | b'v' => t.sint16,
        b'H' => t.uint16,
        b'i' => t.sint32,
        b'I' | b'u' => t.uint32,
        b'l' | b'q' => t.sint64,
        b'L' | b'Q' => t.uint64,
        b'f' => t.float,
        b'd' => t.double,
        b'g' => t.longdouble,
        b'F' => t.complex_float,
        b'D' => t.complex_double,
        b'G' => t.complex_longdouble,
        _ => t.pointer,
    }
}

/// PEP 3118 format of a simple type code.
pub fn code_format(code: u8, swapped: bool) -> String {
    let little = cfg!(target_endian = "little") != swapped;
    let e = if little { '<' } else { '>' };
    let c = match code {
        b'l' => 'q',
        b'L' => 'Q',
        b'F' => return format!("{}Zf", e),
        b'D' => return format!("{}Zd", e),
        b'G' => return format!("{}Zg", e),
        b'z' | b'Z' | b'P' => 'P',
        b'v' => 'h',
        b'u' => 'u',
        x => x as char,
    };
    format!("{}{}", e, c)
}

// ------------------------------------------------------------------ raw integer access

fn load_uint(ptr: *const u8, size: usize, swapped: bool) -> u64 {
    let mut b = [0u8; 8];
    unsafe { std::ptr::copy_nonoverlapping(ptr, b.as_mut_ptr(), size) };
    let little = cfg!(target_endian = "little") != swapped;
    if little {
        u64::from_le_bytes(b)
    } else {
        let mut v = 0u64;
        for &x in &b[..size] {
            v = (v << 8) | x as u64;
        }
        v
    }
}

fn store_uint(ptr: *mut u8, size: usize, swapped: bool, v: u64) {
    let little = cfg!(target_endian = "little") != swapped;
    let mut b = [0u8; 8];
    if little {
        b = v.to_le_bytes();
    } else {
        for i in 0..size {
            b[i] = (v >> (8 * (size - 1 - i))) as u8;
        }
    }
    unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), ptr, size) };
}

fn sign_extend(v: u64, bits: u32) -> i64 {
    if bits >= 64 {
        v as i64
    } else {
        let shift = 64 - bits;
        ((v << shift) as i64) >> shift
    }
}

fn is_signed(code: u8) -> bool {
    matches!(code, b'b' | b'h' | b'i' | b'l' | b'q' | b'v')
}

/// Read an integer-like code, honouring bit fields.
fn get_int_bits(code: u8, swapped: bool, ptr: *const u8, size: usize, bits: Option<(u32, u32)>) -> (u64, u32) {
    let raw = load_uint(ptr, size, swapped);
    match bits {
        Some((nbits, off)) if nbits > 0 => {
            let mask = if nbits >= 64 { u64::MAX } else { (1u64 << nbits) - 1 };
            ((raw >> off) & mask, nbits)
        }
        _ => {
            let _ = code;
            (raw, (size * 8) as u32)
        }
    }
}

fn set_int_bits(swapped: bool, ptr: *mut u8, size: usize, bits: Option<(u32, u32)>, val: u64) {
    match bits {
        Some((nbits, off)) if nbits > 0 => {
            let raw = load_uint(ptr, size, swapped);
            let mask = if nbits >= 64 { u64::MAX } else { (1u64 << nbits) - 1 };
            let nv = (raw & !(mask << off)) | ((val & mask) << off);
            store_uint(ptr, size, swapped, nv);
        }
        _ => store_uint(ptr, size, swapped, val),
    }
}

fn load_bytes<const N: usize>(ptr: *const u8, swapped: bool) -> [u8; N] {
    let mut b = [0u8; N];
    unsafe { std::ptr::copy_nonoverlapping(ptr, b.as_mut_ptr(), N) };
    if swapped {
        b.reverse();
    }
    b
}

fn store_bytes<const N: usize>(ptr: *mut u8, swapped: bool, mut b: [u8; N]) {
    if swapped {
        b.reverse();
    }
    unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), ptr, N) };
}

// ------------------------------------------------------------------ x87 long double

pub fn f64_to_f80(v: f64) -> [u8; 16] {
    let bits = v.to_bits();
    let sign = (bits >> 63) as u16;
    let exp = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1u64 << 52) - 1);
    let (e, m): (u16, u64) = if exp == 0x7ff {
        (0x7fff, if frac == 0 { 1u64 << 63 } else { (1u64 << 63) | (1u64 << 62) | (frac << 11) })
    } else if exp == 0 {
        if frac == 0 {
            (0, 0)
        } else {
            // subnormal: normalise
            let lz = frac.leading_zeros() as i32; // >= 12
            let m = frac << lz;
            let e = 1 - 1023 + 16383 - (lz - 11);
            (e as u16, m)
        }
    } else {
        ((exp - 1023 + 16383) as u16, (1u64 << 63) | (frac << 11))
    };
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&m.to_le_bytes());
    out[8..10].copy_from_slice(&(e | (sign << 15)).to_le_bytes());
    out
}

pub fn f80_to_f64(b: &[u8]) -> f64 {
    let m = u64::from_le_bytes(b[..8].try_into().unwrap());
    let se = u16::from_le_bytes([b[8], b[9]]);
    let sign = (se >> 15) as u64;
    let e = (se & 0x7fff) as i32;
    if e == 0x7fff {
        let frac = m & !(1u64 << 63);
        if frac == 0 {
            return f64::from_bits((sign << 63) | (0x7ffu64 << 52));
        }
        return f64::from_bits((sign << 63) | (0x7ffu64 << 52) | (1u64 << 51) | (frac >> 11));
    }
    if m == 0 {
        return f64::from_bits(sign << 63);
    }
    // value = m * 2^(e - 16383 - 63)
    let mag = (m as f64) * 2f64.powi(0);
    let _ = mag;
    // exact rounding: compose via integer math
    let lz = m.leading_zeros() as i32;
    let m = m << lz;
    let mut exp = e - 16383 - lz; // exponent of the leading bit
    // 53-bit mantissa with round-to-nearest-even
    let mut mant = m >> 11;
    let rest = m & 0x7ff;
    if rest > 0x400 || (rest == 0x400 && mant & 1 == 1) {
        mant += 1;
        if mant >> 53 != 0 {
            mant >>= 1;
            exp += 1;
        }
    }
    if exp > 1023 {
        return f64::from_bits((sign << 63) | (0x7ffu64 << 52));
    }
    if exp < -1022 {
        // subnormal
        let shift = -1022 - exp;
        if shift > 53 {
            return f64::from_bits(sign << 63);
        }
        let full = m >> 11; // 53 bits, unrounded
        let total_shift = shift as u32;
        let mut sm = full >> total_shift;
        let lost = full & ((1u64 << total_shift) - 1);
        let half = 1u64 << (total_shift - 1);
        if lost > half || (lost == half && sm & 1 == 1) {
            sm += 1;
        }
        return f64::from_bits((sign << 63) | sm);
    }
    f64::from_bits((sign << 63) | (((exp + 1023) as u64) << 52) | (mant & ((1u64 << 52) - 1)))
}

fn load_ld(ptr: *const u8, swapped: bool) -> f64 {
    let mut b = [0u8; 16];
    unsafe { std::ptr::copy_nonoverlapping(ptr, b.as_mut_ptr(), 16) };
    if swapped {
        b.reverse();
    }
    f80_to_f64(&b)
}

fn store_ld(ptr: *mut u8, swapped: bool, v: f64) {
    let mut b = f64_to_f80(v);
    if swapped {
        b.reverse();
    }
    unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), ptr, 16) };
}

// ------------------------------------------------------------------ get

unsafe fn wcslen(p: *const i32) -> usize {
    let mut n = 0;
    unsafe {
        while std::ptr::read_unaligned(p.add(n)) != 0 {
            n += 1;
        }
    }
    n
}

pub fn wide_to_str(p: *const i32, n: usize) -> PyResult {
    if n == 0 {
        return Ok(new_str(""));
    }
    if (p as usize) % 4 != 0 {
        let mut buf: Vec<i32> = vec![0; n];
        unsafe { std::ptr::copy_nonoverlapping(p as *const u8, buf.as_mut_ptr() as *mut u8, n * 4) };
        return from_c(unsafe { capi::PyUnicode_FromWideChar(buf.as_ptr(), n as isize) });
    }
    from_c(unsafe { capi::PyUnicode_FromWideChar(p, n as isize) })
}

pub fn get(code: u8, swapped: bool, ptr: *mut u8, size: isize, bits: Option<(u32, u32)>) -> PyResult {
    let sz = code_size(code).map_or(size.max(0) as usize, |x| x.0);
    Ok(match code {
        b'b' | b'h' | b'i' | b'l' | b'q' => {
            let (v, nb) = get_int_bits(code, swapped, ptr, sz, bits);
            new_int(sign_extend(v, nb))
        }
        b'B' | b'H' | b'I' | b'L' | b'Q' => {
            let (v, _) = get_int_bits(code, swapped, ptr, sz, bits);
            new_uint(v)
        }
        b'?' => {
            let (v, _) = get_int_bits(code, swapped, ptr, 1, bits);
            bool_::new_bool(v != 0)
        }
        b'v' => {
            let (v, nb) = get_int_bits(code, swapped, ptr, 2, bits);
            bool_::new_bool(sign_extend(v, nb) != 0)
        }
        b'f' => float::new_float(f32::from_ne_bytes(load_bytes::<4>(ptr, swapped)) as f64),
        b'd' => float::new_float(f64::from_ne_bytes(load_bytes::<8>(ptr, swapped))),
        b'g' => float::new_float(load_ld(ptr, swapped)),
        b'F' => {
            let re = f32::from_ne_bytes(load_bytes::<4>(ptr, swapped)) as f64;
            let im = f32::from_ne_bytes(load_bytes::<4>(unsafe { ptr.add(4) }, swapped)) as f64;
            from_c(unsafe { capi::PyComplex_FromDoubles(re, im) })?
        }
        b'D' => {
            let re = f64::from_ne_bytes(load_bytes::<8>(ptr, swapped));
            let im = f64::from_ne_bytes(load_bytes::<8>(unsafe { ptr.add(8) }, swapped));
            from_c(unsafe { capi::PyComplex_FromDoubles(re, im) })?
        }
        b'G' => {
            let re = load_ld(ptr, swapped);
            let im = load_ld(unsafe { ptr.add(16) }, swapped);
            from_c(unsafe { capi::PyComplex_FromDoubles(re, im) })?
        }
        b'c' => bytes::new_bytes(unsafe { std::slice::from_raw_parts(ptr, 1) }),
        b'u' => {
            let c = unsafe { *(ptr as *const i32) };
            wide_to_str(&c, 1)?
        }
        b's' => {
            let n = size.max(0) as usize;
            let s = unsafe { std::slice::from_raw_parts(ptr, n) };
            let end = s.iter().position(|&x| x == 0).unwrap_or(n);
            bytes::new_bytes(&s[..end])
        }
        b'U' => {
            let n = (size.max(0) as usize) / 4;
            let s = unsafe { std::slice::from_raw_parts(ptr as *const i32, n) };
            let end = s.iter().position(|&x| x == 0).unwrap_or(n);
            wide_to_str(s.as_ptr(), end)?
        }
        b'z' => {
            let p = unsafe { *(ptr as *const *const i8) };
            if p.is_null() {
                none()
            } else {
                from_c(unsafe { capi::PyBytes_FromString(p) })?
            }
        }
        b'Z' => {
            let p = unsafe { *(ptr as *const *const i32) };
            if p.is_null() {
                none()
            } else {
                wide_to_str(p, unsafe { wcslen(p) })?
            }
        }
        b'P' => {
            let p = load_uint(ptr, 8, swapped);
            if p == 0 { none() } else { new_uint(p) }
        }
        b'O' => {
            let p = unsafe { *(ptr as *const *mut PyObject) };
            if p.is_null() {
                raise!(ValueError, "PyObject is NULL");
            }
            unsafe { PyRef::from_borrowed(p) }
        }
        _ => raise!(SystemError, "unknown ctypes type code '{}'", code as char),
    })
}

// ------------------------------------------------------------------ set

fn int_value(value: &PyObject) -> PyResult<u64> {
    if rpy_runtime::types::float::is_float(value) {
        raise!(TypeError, "int expected instead of float");
    }
    if !is_int(value) && !rpy_runtime::abstract_::has_index(value) {
        raise!(TypeError, "int expected instead of {}", tname(value));
    }
    int_mask(value)
}

fn float_value(value: &PyObject) -> PyResult<f64> {
    let v = unsafe { capi::PyFloat_AsDouble(value.as_ptr()) };
    if v == -1.0 && err_occurred() {
        return Err(PyErr);
    }
    Ok(v)
}

fn complex_value(value: &PyObject) -> PyResult<(f64, f64)> {
    let ct = rpy_runtime::builtins::builtins_module();
    let complex = getattr(&ct, "complex")?;
    let c = call(&complex, &[value])?;
    let re = unsafe { capi::PyComplex_RealAsDouble(c.as_ptr()) };
    let im = unsafe { capi::PyComplex_ImagAsDouble(c.as_ptr()) };
    if err_occurred() {
        return Err(PyErr);
    }
    Ok((re, im))
}

/// Keep-alive storage for a `wchar_t*` built from a str: a bytes object holding the
/// NUL-terminated UTF-32 data.
pub fn str_to_wide_bytes(value: &PyObject) -> PyResult<PyRef> {
    let mut n: isize = 0;
    let w = unsafe { capi::PyUnicode_AsWideCharString(value.as_ptr(), &mut n) };
    if w.is_null() {
        return Err(PyErr);
    }
    let b = bytes::new_bytes(unsafe { std::slice::from_raw_parts(w as *const u8, (n as usize + 1) * 4) });
    unsafe { capi::PyMem_Free(w as *mut std::ffi::c_void) };
    Ok(b)
}

pub fn set(code: u8, swapped: bool, ptr: *mut u8, value: &PyObject, size: isize, bits: Option<(u32, u32)>) -> PyResult<Option<PyRef>> {
    let sz = code_size(code).map_or(size.max(0) as usize, |x| x.0);
    match code {
        b'b' | b'h' | b'i' | b'l' | b'q' | b'B' | b'H' | b'I' | b'L' | b'Q' => {
            let v = int_value(value)?;
            set_int_bits(swapped, ptr, sz, bits, v);
        }
        b'?' => {
            let v = rpy_runtime::abstract_::is_true(value)? as u64;
            set_int_bits(swapped, ptr, 1, bits, v);
        }
        b'v' => {
            let v: u64 = if rpy_runtime::abstract_::is_true(value)? { 0xffff } else { 0 };
            set_int_bits(swapped, ptr, 2, bits, v);
        }
        b'f' => {
            let v = float_value(value)? as f32;
            store_bytes::<4>(ptr, swapped, v.to_ne_bytes());
        }
        b'd' => {
            let v = float_value(value)?;
            store_bytes::<8>(ptr, swapped, v.to_ne_bytes());
        }
        b'g' => {
            let v = float_value(value)?;
            store_ld(ptr, swapped, v);
        }
        b'F' => {
            let (re, im) = complex_value(value)?;
            store_bytes::<4>(ptr, swapped, (re as f32).to_ne_bytes());
            store_bytes::<4>(unsafe { ptr.add(4) }, swapped, (im as f32).to_ne_bytes());
        }
        b'D' => {
            let (re, im) = complex_value(value)?;
            store_bytes::<8>(ptr, swapped, re.to_ne_bytes());
            store_bytes::<8>(unsafe { ptr.add(8) }, swapped, im.to_ne_bytes());
        }
        b'G' => {
            let (re, im) = complex_value(value)?;
            store_ld(ptr, swapped, re);
            store_ld(unsafe { ptr.add(16) }, swapped, im);
        }
        b'c' => {
            let byte = if is_bytes(value) && bytes::bytes_len(value) == 1 {
                bytes::bytes_data(value)[0]
            } else if rpy_runtime::types::bytearray::is_bytearray(value) && rpy_runtime::types::bytearray::bytearray_len(value) == 1 {
                rpy_runtime::types::bytearray::bytearray_data(value)[0]
            } else if is_int(value) {
                match rpy_runtime::types::int::as_i64(value) {
                    Some(v) if (0..256).contains(&v) => v as u8,
                    _ => raise!(TypeError, "one character bytes, bytearray, or an integer in range(256) expected, not int out of range"),
                }
            } else if is_bytes(value) {
                raise!(TypeError, "one character bytes, bytearray, or an integer in range(256) expected, not bytes of length {}", bytes::bytes_len(value));
            } else if rpy_runtime::types::bytearray::is_bytearray(value) {
                raise!(
                    TypeError,
                    "one character bytes, bytearray, or an integer in range(256) expected, not bytearray of length {}",
                    rpy_runtime::types::bytearray::bytearray_len(value)
                );
            } else {
                raise!(TypeError, "one character bytes, bytearray, or an integer in range(256) expected, not {}", tname(value));
            };
            unsafe { *ptr = byte };
        }
        b'u' => {
            if !is_str(value) {
                raise!(TypeError, "a unicode character expected, not instance of {}", tname(value));
            }
            let n = rpy_runtime::types::str::str_len(value);
            if n != 1 {
                raise!(TypeError, "a unicode character expected, not a string of length {}", n);
            }
            let c = unsafe { capi::PyUnicode_ReadChar(value.as_ptr(), 0) };
            unsafe { *(ptr as *mut i32) = c as i32 };
        }
        b's' => {
            if !is_bytes(value) {
                raise!(TypeError, "expected bytes, {} found", tname(value));
            }
            let full = bytes::bytes_data(value);
            // C string semantics: stop at the first NUL
            let data = &full[..full.iter().position(|&b| b == 0).unwrap_or(full.len())];
            let n = size.max(0) as usize;
            if data.len() > n {
                raise!(ValueError, "bytes too long ({}, maximum length {})", data.len(), n);
            }
            unsafe {
                std::ptr::copy(data.as_ptr(), ptr, data.len());
                if data.len() < n {
                    *ptr.add(data.len()) = 0;
                }
            }
        }
        b'U' => {
            if !is_str(value) {
                raise!(TypeError, "unicode string expected instead of {} instance", tname(value));
            }
            let w = str_to_wide_bytes(value)?;
            let wd = bytes::bytes_data(&w);
            let nchars = wd.len() / 4 - 1;
            let cap = (size.max(0) as usize) / 4;
            if nchars > cap {
                raise!(ValueError, "string too long ({}, maximum length {})", nchars, cap);
            }
            let ncopy = if nchars < cap { nchars + 1 } else { nchars };
            unsafe { std::ptr::copy(wd.as_ptr(), ptr, ncopy * 4) };
        }
        b'z' => {
            if is_none(value) {
                unsafe { *(ptr as *mut *mut u8) = std::ptr::null_mut() };
                return Ok(None);
            }
            if is_bytes(value) {
                unsafe { *(ptr as *mut *mut u8) = bytes::bytes_ptr(value) };
                return Ok(Some(value.to_ref()));
            }
            if is_int(value) {
                let v = int_mask(value)?;
                unsafe { *(ptr as *mut u64) = v };
                return Ok(None);
            }
            raise!(TypeError, "bytes or integer address expected instead of {} instance", tname(value));
        }
        b'Z' => {
            if is_none(value) {
                unsafe { *(ptr as *mut *mut u8) = std::ptr::null_mut() };
                return Ok(None);
            }
            if is_int(value) {
                let v = int_mask(value)?;
                unsafe { *(ptr as *mut u64) = v };
                return Ok(None);
            }
            if !is_str(value) {
                raise!(TypeError, "unicode string or integer address expected instead of {} instance", tname(value));
            }
            let w = str_to_wide_bytes(value)?;
            unsafe { *(ptr as *mut *mut u8) = bytes::bytes_ptr(&w) };
            return Ok(Some(w));
        }
        b'P' => {
            if is_none(value) {
                store_uint(ptr, 8, swapped, 0);
                return Ok(None);
            }
            if !is_int(value) {
                raise!(TypeError, "cannot be converted to pointer");
            }
            let v = int_mask(value)?;
            store_uint(ptr, 8, swapped, v);
        }
        b'O' => {
            unsafe { *(ptr as *mut *mut PyObject) = value.as_ptr() };
            return Ok(Some(value.to_ref()));
        }
        _ => raise!(SystemError, "unknown ctypes type code '{}'", code as char),
    }
    Ok(None)
}

// ------------------------------------------------------------------ _SimpleCData

pub static mut SIMPLE_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut SIMPLE_META: PyTypeObject = PyTypeObject::ZERO;

pub fn simple_type() -> *mut PyTypeObject {
    &raw mut SIMPLE_TYPE
}
pub fn simple_meta() -> *mut PyTypeObject {
    &raw mut SIMPLE_META
}

fn info_of(o: &PyObject) -> PyResult<&'static mut StgInfo> {
    match stginfo_of_obj(o) {
        Some(i) => Ok(i),
        None => raise!(TypeError, "abstract class"),
    }
}

pub fn simple_value_get(o: &PyObject) -> PyResult {
    let info = info_of(o)?;
    let c = cd(o);
    get(info.code, info.swapped, c.b_ptr, c.b_size, None)
}

pub fn simple_value_set(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        Some(v) => v,
        None => raise!(TypeError, "can't delete attribute"),
    };
    let info = info_of(o)?;
    let c = cd(o);
    let keep = set(info.code, info.swapped, c.b_ptr, v, c.b_size, None)?;
    keep_ref(o, 0, keep)
}

unsafe extern "C" fn simple_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let a = rpy_runtime::modules::fncommon::targs(args);
        let kwd = rpy_runtime::modules::fncommon::opt(kw);
        let r = (|| -> PyResult<()> {
            let mut value: Option<PyRef> = None;
            if a.len() > 1 {
                raise!(TypeError, "__init__() takes at most 1 argument ({} given)", a.len());
            }
            if a.len() == 1 {
                value = Some(a[0].to_ref());
            }
            if let Some(k) = kwd {
                for (key, val) in rpy_runtime::types::dict::items_vec(k) {
                    if str_of(&key) == "value" && value.is_none() {
                        value = Some(val);
                    } else {
                        raise!(TypeError, "'{}' is an invalid keyword argument for this function", str_of(&key));
                    }
                }
            }
            if let Some(v) = value {
                simple_value_set(&*o, Some(&v))?;
            }
            Ok(())
        })();
        int_into_c(r)
    }
}

unsafe extern "C" fn simple_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let ob = &*o;
        let r = (|| -> PyResult {
            let tp = ob.type_ptr();
            if (*tp).tp_base != simple_type() {
                return Ok(new_str(&format!("<{} object at {:#x}>", type_name(tp), o as usize)));
            }
            let v = simple_value_get(ob)?;
            Ok(new_str(&format!("{}({})", type_name(tp), repr(&v)?)))
        })();
        into_c(r)
    }
}

unsafe extern "C" fn simple_bool(o: *mut PyObject) -> c_int {
    unsafe {
        let c = cdp(o);
        let n = c.b_size.max(0) as usize;
        let s = std::slice::from_raw_parts(c.b_ptr, n);
        if s.iter().any(|&b| b != 0) { 1 } else { 0 }
    }
}

fn simple_from_outparam(o: &PyObject) -> PyResult {
    if simple_instance(o.type_ptr()) {
        return Ok(o.to_ref());
    }
    simple_value_get(o)
}

static mut SIMPLE_AS_NUMBER: PyNumberMethods = unsafe { std::mem::zeroed() };

// ------------------------------------------------------------------ metaclass

/// Parse `_type_` into an internal code.
fn parse_type_code(proto: &PyObject) -> PyResult<u8> {
    if !is_str(proto) {
        raise!(TypeError, "class must define a '_type_' string attribute");
    }
    let s = str_of(proto);
    let code = match s.as_str() {
        "Zf" => b'F',
        "Zd" => b'D',
        "Zg" => b'G',
        _ => {
            if s.chars().count() != 1 {
                raise!(ValueError, "class must define a '_type_' attribute which must be a string of length 1");
            }
            let c = s.as_bytes()[0];
            if s.len() != 1 || !SIMPLE_TYPE_CHARS.as_bytes().contains(&c) {
                raise!(
                    AttributeError,
                    "class must define a '_type_' attribute which must be\na single character string containing one of '{}' or one of 'Zf', 'Zd', 'Zg'.",
                    SIMPLE_TYPE_CHARS
                );
            }
            c
        }
    };
    Ok(code)
}

fn make_simple_info(code: u8, swapped: bool, proto: PyRef) -> StgInfo {
    let (size, align) = code_size(code).unwrap();
    let mut info = StgInfo::new(Kind::Simple);
    info.size = size as isize;
    info.align = align as isize;
    info.code = code;
    info.swapped = swapped;
    info.proto = Some(proto);
    *info.ffi = unsafe { *code_ffi(code) };
    info.format = Some(code_format(code, swapped));
    if code == b'z' || code == b'Z' || code == b'P' {
        info.dict_flags |= TYPEFLAG_ISPOINTER;
    }
    info
}

fn swappable(code: u8) -> bool {
    matches!(code, b'h' | b'H' | b'i' | b'I' | b'l' | b'L' | b'q' | b'Q' | b'f' | b'd' | b'F' | b'D' | b'G')
}

fn self_swapped(code: u8) -> bool {
    matches!(code, b'c' | b'b' | b'B' | b'?')
}

/// PyCSimpleType.__init__
unsafe extern "C" fn simple_meta_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let tp_init = (*rpy_runtime::types::typeobject::type_type()).tp_init.unwrap();
        if tp_init(o, args, kw) < 0 {
            return -1;
        }
        int_into_c(simple_meta_init_impl(&*o))
    }
}

fn simple_meta_init_impl(cls: &PyObject) -> PyResult<()> {
    let tp = cls.as_ptr() as *mut PyTypeObject;
    if has_stginfo_slot_set(tp) {
        raise!(SystemError, "StgInfo of '{}' is already initialized.", type_name(tp));
    }
    let proto = match getattr_opt(cls, "_type_")? {
        Some(p) => p,
        None => raise!(AttributeError, "class must define a '_type_' attribute"),
    };
    let code = parse_type_code(&proto)?;
    let mut info = make_simple_info(code, false, proto.clone());
    // a subclass of another simple type inherits its "swapped" state
    unsafe {
        if let Some(bi) = stginfo((*tp).tp_base) {
            if bi.kind == Kind::Simple && bi.code == code {
                info.swapped = bi.swapped;
                info.format = bi.format.clone();
            }
        }
    }
    let swapped = info.swapped;
    init_stginfo(tp, info)?;
    // special from_param for c_char_p, c_wchar_p, c_void_p direct subclasses
    unsafe {
        if (*tp).tp_base == simple_type() {
            let fp = match code {
                b'z' => Some(c_char_p_from_param as fn(&PyObject, &PyObject) -> PyResult),
                b'Z' => Some(c_wchar_p_from_param as fn(&PyObject, &PyObject) -> PyResult),
                b'P' => Some(c_void_p_from_param as fn(&PyObject, &PyObject) -> PyResult),
                _ => None,
            };
            if let Some(f) = fp {
                let defs = match code {
                    b'z' => rpy_runtime::mdefs![("from_param", O, c_char_p_from_param; METH_CLASS)],
                    b'Z' => rpy_runtime::mdefs![("from_param", O, c_wchar_p_from_param; METH_CLASS)],
                    _ => rpy_runtime::mdefs![("from_param", O, c_void_p_from_param; METH_CLASS)],
                };
                let _ = f;
                let descr = rpy_runtime::types::descr::new_classmethod_descr(tp, defs);
                let d = &*(*tp).tp_dict;
                rpy_runtime::types::dict::set_item(d, &new_str("from_param"), &descr)?;
                capi::PyType_Modified(tp);
            }
        }
        if ob_type(cls.as_ptr()) == simple_meta() {
            // byte-order variants
            if !swapped && (swappable(code) || self_swapped(code)) {
                let (native, other) = if cfg!(target_endian = "little") { ("__ctype_le__", "__ctype_be__") } else { ("__ctype_be__", "__ctype_le__") };
                if self_swapped(code) {
                    setattr(cls, native, cls)?;
                    setattr(cls, other, cls)?;
                } else {
                    let sw = create_swapped_type(cls, code)?;
                    setattr(cls, native, cls)?;
                    setattr(cls, other, &sw)?;
                    setattr(&sw, native, cls)?;
                    setattr(&sw, other, &sw)?;
                }
            }
        }
    }
    Ok(())
}

/// Create the opposite-byte-order variant of a simple type.
fn create_swapped_type(cls: &PyObject, code: u8) -> PyResult {
    let tp = cls.as_ptr() as *mut PyTypeObject;
    let name = new_str(&type_name(tp));
    let bases = unsafe { PyRef::from_borrowed((*tp).tp_bases) };
    let d = rpy_runtime::types::dict::new_dict();
    unsafe {
        let src = &*(*tp).tp_dict;
        for k in ["_type_", "__module__", "__qualname__"] {
            if let Some(v) = rpy_runtime::types::dict::get_item_str(src, &new_str(k)) {
                rpy_runtime::types::dict::set_item(&d, &new_str(k), &v)?;
            }
        }
    }
    let meta = rpy_runtime::types::typeobject::type_ref(ob_type(cls.as_ptr()));
    // create the type without running our __init__ (which would recurse)
    let args = rpy_runtime::types::tuple::new_tuple(vec![name, bases, d]);
    let sw = rpy_runtime::types::object_type::type_new(meta.as_ptr() as *mut PyTypeObject, &args, None)?;
    let proto = getattr(cls, "_type_")?;
    init_stginfo(sw.as_ptr() as *mut PyTypeObject, make_simple_info(code, true, proto))?;
    Ok(sw)
}

// ------------------------------------------------------------------ from_param

fn carg_from_value(code: u8, value: &PyObject) -> PyResult {
    let (size, _) = code_size(code).unwrap();
    let a = funcptr::new_carg(code, code_ffi(code))?;
    let ca = funcptr::carg(&a);
    let keep = set(code, false, ca.value.as_mut_ptr() as *mut u8, value, size as isize, None)?;
    funcptr::carg_set_obj(&a, keep.unwrap_or_else(|| value.to_ref()));
    Ok(a)
}

/// from_param of ordinary simple types.
pub fn simple_from_param(cls: &PyObject, value: &PyObject) -> PyResult {
    if isinstance(value, cls)? {
        return Ok(value.to_ref());
    }
    let info = match stginfo_of_type(cls) {
        Some(i) => i,
        None => raise!(TypeError, "abstract class"),
    };
    if info.swapped {
        // byte-swapped types are not passed by value
        raise!(TypeError, "cannot be converted to a parameter");
    }
    match carg_from_value(info.code, value) {
        Ok(a) => Ok(a),
        Err(e) => {
            let saved = rpy_runtime::errors::take_exc();
            match getattr_opt(value, "_as_parameter_")? {
                Some(asp) => rpy_runtime::modules::fncommon::with_recursion(" while processing _as_parameter_", || {
                    rpy_runtime::abstract_::call_method_str(cls, "from_param", &[&asp])
                }),
                None => {
                    if let Some(s) = saved {
                        rpy_runtime::errors::set_exc(s);
                    }
                    Err(e)
                }
            }
        }
    }
}

fn type_is_pointer_to(value: &PyObject, code: u8) -> bool {
    // POINTER(c_char)/c_char array instance etc.
    if let Some(vi) = stginfo_of_obj(value) {
        if vi.kind == Kind::Array || vi.kind == Kind::Pointer {
            if let Some(p) = &vi.proto {
                if let Some(pi) = stginfo_of_type(p) {
                    return pi.kind == Kind::Simple && pi.code == code;
                }
            }
        }
    }
    false
}

fn pointer_like_from_param(cls: &PyObject, value: &PyObject, code: u8, accept_int: bool) -> PyResult<Option<PyRef>> {
    if is_none(value) {
        let a = funcptr::new_carg(b'P', libffi::types().pointer)?;
        funcptr::carg(&a).set_ptr(std::ptr::null_mut());
        return Ok(Some(a));
    }
    if isinstance(value, cls)? {
        return Ok(Some(value.to_ref()));
    }
    if accept_int && is_int(value) {
        let a = funcptr::new_carg(b'P', libffi::types().pointer)?;
        let v = int_mask(value)?;
        funcptr::carg(&a).set_ptr(v as usize as *mut u8);
        return Ok(Some(a));
    }
    if is_cdata(value) && type_is_pointer_to(value, code) {
        return Ok(Some(value.to_ref()));
    }
    if funcptr::is_carg(value) {
        let a = funcptr::carg(value);
        if a.tag == b'P' && !a.obj.is_null() && type_is_pointer_to_or_inst(r(a.obj), code) {
            return Ok(Some(value.to_ref()));
        }
    }
    Ok(None)
}

fn type_is_pointer_to_or_inst(o: &PyObject, code: u8) -> bool {
    if let Some(i) = stginfo_of_obj(o) {
        if i.kind == Kind::Simple && i.code == code {
            return true;
        }
    }
    type_is_pointer_to(o, code)
}

fn pointer_fail(cls: &PyObject, value: &PyObject, what: &str) -> PyResult {
    let _ = cls;
    match getattr_opt(value, "_as_parameter_")? {
        Some(asp) => rpy_runtime::modules::fncommon::with_recursion(" while processing _as_parameter_", || {
            rpy_runtime::abstract_::call_method_str(cls, "from_param", &[&asp])
        }),
        None => raise!(TypeError, "'{}' object cannot be interpreted as ctypes.{}", tname(value), what),
    }
}

pub fn c_char_p_from_param(cls: &PyObject, value: &PyObject) -> PyResult {
    if is_bytes(value) {
        return carg_from_value(b'z', value);
    }
    if let Some(r) = pointer_like_from_param(cls, value, b'c', false)? {
        return Ok(r);
    }
    pointer_fail(cls, value, "c_char_p")
}

pub fn c_wchar_p_from_param(cls: &PyObject, value: &PyObject) -> PyResult {
    if is_str(value) {
        return carg_from_value(b'Z', value);
    }
    if let Some(r) = pointer_like_from_param(cls, value, b'u', false)? {
        return Ok(r);
    }
    pointer_fail(cls, value, "c_wchar_p")
}

pub fn c_void_p_from_param(cls: &PyObject, value: &PyObject) -> PyResult {
    if is_none(value) || is_int(value) {
        return pointer_like_from_param(cls, value, 0, true).map(|x| x.unwrap());
    }
    if is_bytes(value) {
        return carg_from_value(b'z', value);
    }
    if is_str(value) {
        return carg_from_value(b'Z', value);
    }
    if isinstance(value, cls)? {
        return Ok(value.to_ref());
    }
    if is_cdata(value) {
        if let Some(vi) = stginfo_of_obj(value) {
            match vi.kind {
                Kind::Array | Kind::Pointer => return Ok(value.to_ref()),
                Kind::FuncPtr => {
                    let a = funcptr::new_carg(b'P', libffi::types().pointer)?;
                    let p = unsafe { *(cd(value).b_ptr as *const *mut u8) };
                    funcptr::carg(&a).set_ptr(p);
                    funcptr::carg_set_obj(&a, value.to_ref());
                    return Ok(a);
                }
                Kind::Simple if vi.code == b'z' || vi.code == b'Z' || vi.code == b'P' => {
                    let a = funcptr::new_carg(b'Z', libffi::types().pointer)?;
                    let p = unsafe { *(cd(value).b_ptr as *const *mut u8) };
                    funcptr::carg(&a).set_ptr(p);
                    funcptr::carg_set_obj(&a, value.to_ref());
                    return Ok(a);
                }
                _ => {}
            }
        }
    }
    if funcptr::is_carg(value) {
        if funcptr::carg(value).tag == b'P' {
            return Ok(value.to_ref());
        }
    }
    pointer_fail(cls, value, "c_void_p")
}

// ------------------------------------------------------------------ init

pub fn init_types() -> PyResult<()> {
    unsafe {
        let m = simple_meta();
        init_meta(m, c"_ctypes.PyCSimpleType", ctype_type());
        (*m).tp_init = Some(simple_meta_init);
        (*m).tp_methods = meta_methods(rpy_runtime::mdefs![("from_param", O, simple_from_param)], vec![]);
        set_meta_number(m);
        rpy_runtime::types::typeobject::type_ready(m)?;

        let t = simple_type();
        init_base_type(t, c"_ctypes._SimpleCData", m, std::mem::size_of::<CDataObject>());
        (*t).tp_init = Some(simple_init);
        (*t).tp_repr = Some(simple_repr);
        SIMPLE_AS_NUMBER.nb_bool = Some(simple_bool);
        (*t).tp_as_number = &raw mut SIMPLE_AS_NUMBER;
        (*t).tp_methods = rpy_runtime::mdefs![("__ctypes_from_outparam__", NOARGS, simple_from_outparam)];
        (*t).tp_getset = rpy_runtime::getset_table![("value", simple_value_get, simple_value_set)];
        rpy_runtime::types::typeobject::type_ready(t)?;
    }
    Ok(())
}
