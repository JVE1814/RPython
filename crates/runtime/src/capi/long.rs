//! longobject.h / boolobject.h / floatobject.h / complexobject.h / pyhash.h.

use super::*;
use crate::abstract_ as ab;
use crate::types::int::{self, SHIFT};
use crate::types::{bool_, complex, float, str as pystr, typeobject};
use std::ffi::{c_long, c_longlong, c_uint, c_ulong, c_ulonglong, c_void};

// ---------------------------------------------------------------- helpers

/// Get an int from `o`: ints pass through, others go through __index__ if allowed.
unsafe fn get_int(o: *mut PyObject, use_index: bool) -> PyResult<PyRef> {
    unsafe {
        if o.is_null() {
            return Err(bad_internal());
        }
        let r = &*o;
        if int::is_int(r) {
            return Ok(r.to_ref());
        }
        if use_index {
            return ab::index(r);
        }
        Err(type_error("an integer is required"))
    }
}

/// Magnitude of an int as little-endian bytes (minimal length).
fn mag_bytes(o: &PyObject) -> Vec<u8> {
    let ds = int::digits(o);
    let mut out = Vec::with_capacity(ds.len() * 4);
    let mut acc: u64 = 0;
    let mut bits = 0;
    for &d in ds {
        acc |= (d as u64) << bits;
        bits += SHIFT;
        while bits >= 8 {
            out.push(acc as u8);
            acc >>= 8;
            bits -= 8;
        }
    }
    if bits > 0 {
        out.push(acc as u8);
    }
    while out.last() == Some(&0) {
        out.pop();
    }
    out
}

fn num_bits(o: &PyObject) -> u64 {
    let ds = int::digits(o);
    match ds.last() {
        None => 0,
        Some(&top) => (ds.len() as u64 - 1) * SHIFT as u64 + (32 - top.leading_zeros()) as u64,
    }
}

/// Low `n` bytes of the two's complement representation (little endian).
fn twos_complement(o: &PyObject, n: usize) -> Vec<u8> {
    let mag = mag_bytes(o);
    let mut v: Vec<u8> = (0..n).map(|i| *mag.get(i).unwrap_or(&0)).collect();
    if int::sign(o) < 0 {
        let mut carry = 1u16;
        for b in v.iter_mut() {
            let x = (!*b) as u16 + carry;
            *b = x as u8;
            carry = x >> 8;
        }
    }
    v
}

/// Build an int from little-endian bytes.
fn from_le_bytes(b: &[u8], signed: bool) -> PyRef {
    let neg = signed && b.last().is_some_and(|&x| x & 0x80 != 0);
    let mag: Vec<u8> = if neg {
        let mut v: Vec<u8> = b.iter().map(|x| !x).collect();
        let mut carry = 1u16;
        for x in v.iter_mut() {
            let s = *x as u16 + carry;
            *x = s as u8;
            carry = s >> 8;
        }
        v
    } else {
        b.to_vec()
    };
    let mut ds = Vec::with_capacity(mag.len() / 4 + 2);
    let mut acc: u64 = 0;
    let mut bits = 0;
    for &x in &mag {
        acc |= (x as u64) << bits;
        bits += 8;
        if bits >= SHIFT {
            ds.push((acc & int::MASK as u64) as u32);
            acc >>= SHIFT;
            bits -= SHIFT;
        }
    }
    if bits > 0 {
        ds.push(acc as u32);
    }
    int::from_digits(neg, &ds)
}

fn too_large(what: &str) -> PyErr {
    overflow_error(&format!("Python int too large to convert to C {}", what))
}

macro_rules! as_signed {
    ($name:ident, $t:ty, $what:expr, $index:expr) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(o: *mut PyObject) -> $t {
            unsafe {
                let v = match get_int(o, $index) {
                    Ok(v) => v,
                    Err(_) => return -1,
                };
                match int::as_i64(&v) {
                    Some(x) if x >= <$t>::MIN as i64 && x <= <$t>::MAX as i64 => x as $t,
                    _ => {
                        let _ = too_large($what);
                        -1
                    }
                }
            }
        }
    };
}
as_signed!(PyLong_AsLong, c_long, "long", true);
as_signed!(PyLong_AsLongLong, c_longlong, "long long", true);
as_signed!(PyLong_AsSsize_t, Py_ssize_t, "ssize_t", false);
as_signed!(PyLong_AsInt, c_int, "int", true);

macro_rules! as_unsigned {
    ($name:ident, $t:ty, $what:expr, $index:expr) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(o: *mut PyObject) -> $t {
            unsafe {
                let v = match get_int(o, $index) {
                    Ok(v) => v,
                    Err(_) => return <$t>::MAX,
                };
                if int::sign(&v) < 0 {
                    let what = if $what == "size_t" { "size_t" } else { "unsigned int" };
                    let _ = overflow_error(&format!("can't convert negative value to {}", what));
                    return <$t>::MAX;
                }
                match int::as_i128(&v) {
                    Some(x) if x <= <$t>::MAX as i128 => x as $t,
                    _ => {
                        let _ = too_large($what);
                        <$t>::MAX
                    }
                }
            }
        }
    };
}
as_unsigned!(PyLong_AsUnsignedLong, c_ulong, "unsigned long", false);
as_unsigned!(PyLong_AsUnsignedLongLong, c_ulonglong, "unsigned long long", false);
as_unsigned!(PyLong_AsSize_t, usize, "size_t", false);

macro_rules! as_mask {
    ($name:ident, $t:ty) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(o: *mut PyObject) -> $t {
            unsafe {
                let v = match get_int(o, true) {
                    Ok(v) => v,
                    Err(_) => return <$t>::MAX,
                };
                let b = twos_complement(&v, 8);
                u64::from_le_bytes(b.try_into().unwrap()) as $t
            }
        }
    };
}
as_mask!(PyLong_AsUnsignedLongMask, c_ulong);
as_mask!(PyLong_AsUnsignedLongLongMask, c_ulonglong);

unsafe fn and_overflow(o: *mut PyObject, overflow: *mut c_int) -> i64 {
    unsafe {
        *overflow = 0;
        let v = match get_int(o, true) {
            Ok(v) => v,
            Err(_) => return -1,
        };
        match int::as_i64(&v) {
            Some(x) => x,
            None => {
                *overflow = if int::sign(&v) < 0 { -1 } else { 1 };
                -1
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_AsLongAndOverflow(o: *mut PyObject, overflow: *mut c_int) -> c_long {
    unsafe { and_overflow(o, overflow) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_AsLongLongAndOverflow(o: *mut PyObject, overflow: *mut c_int) -> c_longlong {
    unsafe { and_overflow(o, overflow) }
}

macro_rules! as_fixed {
    ($name:ident, $t:ty, $signed:expr) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(o: *mut PyObject, out: *mut $t) -> c_int {
            unsafe {
                let v = match get_int(o, true) {
                    Ok(v) => v,
                    Err(_) => return -1,
                };
                if !$signed && int::sign(&v) < 0 {
                    let _ = value_error("Cannot convert negative int");
                    return -1;
                }
                match int::as_i128(&v) {
                    Some(x) if x >= <$t>::MIN as i128 && x <= <$t>::MAX as i128 => {
                        *out = x as $t;
                        0
                    }
                    _ => {
                        let _ = overflow_error(&format!("Python int too large to convert to C {}", stringify!($t).replace("i32", "int32_t").replace("i64", "int64_t").replace("u32", "uint32_t").replace("u64", "uint64_t")));
                        -1
                    }
                }
            }
        }
    };
}
as_fixed!(PyLong_AsInt32, i32, true);
as_fixed!(PyLong_AsInt64, i64, true);
as_fixed!(PyLong_AsUInt32, u32, false);
as_fixed!(PyLong_AsUInt64, u64, false);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_AsDouble(o: *mut PyObject) -> f64 {
    unsafe {
        let v = match get_int(o, false) {
            Ok(v) => v,
            Err(_) => {
                if !o.is_null() && !int::is_int(&*o) {
                    clear_exc();
                    let _ = type_error("an integer is required");
                }
                return -1.0;
            }
        };
        int::to_f64(&v).unwrap_or(-1.0)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_AsVoidPtr(o: *mut PyObject) -> *mut c_void {
    unsafe {
        let v = match get_int(o, false) {
            Ok(v) => v,
            Err(_) => return std::ptr::null_mut(),
        };
        match int::as_i128(&v) {
            Some(x) if x >= i64::MIN as i128 && x <= u64::MAX as i128 => (x as u64) as usize as *mut c_void,
            _ => {
                let _ = overflow_error("Python int too large to convert to C pointer");
                std::ptr::null_mut()
            }
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_FromLong(v: c_long) -> *mut PyObject {
    int::new_int(v).into_raw()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_FromUnsignedLong(v: c_ulong) -> *mut PyObject {
    int::new_int_u128(v as u128).into_raw()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_FromLongLong(v: c_longlong) -> *mut PyObject {
    int::new_int(v).into_raw()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_FromUnsignedLongLong(v: c_ulonglong) -> *mut PyObject {
    int::new_int_u128(v as u128).into_raw()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_FromSsize_t(v: Py_ssize_t) -> *mut PyObject {
    int::new_int(v as i64).into_raw()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_FromSize_t(v: usize) -> *mut PyObject {
    int::new_int_usize(v).into_raw()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_FromInt32(v: i32) -> *mut PyObject {
    int::new_int(v as i64).into_raw()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_FromUInt32(v: u32) -> *mut PyObject {
    int::new_int(v as i64).into_raw()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_FromInt64(v: i64) -> *mut PyObject {
    int::new_int(v).into_raw()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_FromUInt64(v: u64) -> *mut PyObject {
    int::new_int_u128(v as u128).into_raw()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_FromVoidPtr(p: *mut c_void) -> *mut PyObject {
    int::new_int_usize(p as usize).into_raw()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_FromDouble(v: f64) -> *mut PyObject {
    let f = float::new_float(v);
    ret(ab::call(typeobject::type_obj(int::int_type()), &[&f]))
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_FromString(s: *const c_char, pend: *mut *mut c_char, base: c_int) -> *mut PyObject {
    unsafe {
        if !(base == 0 || (2..=36).contains(&base)) {
            let _ = value_error("int() arg 2 must be >= 2 and <= 36");
            return std::ptr::null_mut();
        }
        let bytes = CStr::from_ptr(s).to_bytes();
        // the literal extends over sign, prefix, alnum and underscores (after leading spaces)
        let mut i = 0;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        let mut end = i;
        while end < bytes.len() && bytes[end].is_ascii_whitespace() {
            end += 1;
        }
        if !pend.is_null() {
            *pend = s.add(end) as *mut c_char;
        }
        let text = match std::str::from_utf8(bytes) {
            Ok(t) => t,
            Err(_) => {
                let _ = value_error(&format!(
                    "invalid literal for int() with base {}: {}",
                    base,
                    ab::repr_string(&pystr::new_str(&String::from_utf8_lossy(bytes))).unwrap_or_default()
                ));
                return std::ptr::null_mut();
            }
        };
        if end != bytes.len() || text.chars().any(|c| !c.is_ascii()) {
            let _ = value_error(&format!(
                "invalid literal for int() with base {}: {}",
                base,
                ab::repr_string(&pystr::new_str(text)).unwrap_or_default()
            ));
            return std::ptr::null_mut();
        }
        let so = pystr::new_str(text);
        ret(ab::call(typeobject::type_obj(int::int_type()), &[&so, &int::new_int(base as i64)]))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_FromUnicodeObject(u: *mut PyObject, base: c_int) -> *mut PyObject {
    unsafe { ret(ab::call(typeobject::type_obj(int::int_type()), &[&*u, &int::new_int(base as i64)])) }
}

// ---------------------------------------------------------------- native bytes

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_AsNativeBytes(v: *mut PyObject, buffer: *mut c_void, n_bytes: Py_ssize_t, flags: c_int) -> Py_ssize_t {
    unsafe {
        if v.is_null() {
            let _ = bad_internal();
            return -1;
        }
        let (little, unsigned_buf, reject_neg, allow_index) = if flags == -1 {
            (cfg!(target_endian = "little"), false, false, false)
        } else {
            let le = if flags & 2 != 0 { cfg!(target_endian = "little") } else { flags & 1 != 0 };
            (le, flags & 4 != 0, flags & 8 != 0, flags & 16 != 0)
        };
        let o = if int::is_int(&*v) {
            (*v).to_ref()
        } else if allow_index {
            match ab::index(&*v) {
                Ok(o) => o,
                Err(_) => return -1,
            }
        } else {
            let _ = type_error("expect int");
            return -1;
        };
        let neg = int::sign(&o) < 0;
        if reject_neg && neg {
            let _ = value_error("Cannot convert negative int");
            return -1;
        }
        let n = n_bytes.max(0) as usize;
        const SZ: usize = std::mem::size_of::<Py_ssize_t>();
        let write = |le_bytes: &[u8]| {
            let p = buffer as *mut u8;
            for i in 0..n {
                let b = le_bytes[i];
                if little {
                    *p.add(i) = b;
                } else {
                    *p.add(n - 1 - i) = b;
                }
            }
        };
        if int::is_compact(&o) {
            let cv = int::compact_value(&o);
            if n == 0 {
                return SZ as Py_ssize_t;
            }
            write(&twos_complement(&o, n));
            if n >= SZ {
                return SZ as Py_ssize_t;
            }
            let bits = 8 * n as u32;
            let fits = if unsigned_buf && cv >= 0 {
                (cv as u64) < (1u64 << bits)
            } else {
                cv >= -(1i64 << (bits - 1)) && cv < (1i64 << (bits - 1))
            };
            if fits {
                return n as Py_ssize_t;
            }
            // smallest signed width that holds the value
            let mut need = n + 1;
            while need < SZ {
                let b = 8 * need as u32;
                if cv >= -(1i64 << (b - 1)) && cv < (1i64 << (b - 1)) {
                    break;
                }
                need += 1;
            }
            return need as Py_ssize_t;
        }
        let nbits = num_bits(&o) as usize;
        let mut res = nbits / 8 + 1;
        if n > 0 {
            write(&twos_complement(&o, n));
            if res == n + 1 {
                if unsigned_buf && !neg && nbits == 8 * n {
                    res = n;
                } else if neg && nbits == 8 * n {
                    // exactly -2**(8n-1) fits
                    let mag = mag_bytes(&o);
                    let is_pow = mag.iter().take(mag.len() - 1).all(|&b| b == 0) && mag.last() == Some(&0x80);
                    if is_pow {
                        res = n;
                    }
                }
            }
        }
        res as Py_ssize_t
    }
}

unsafe fn from_native(buffer: *const c_void, n: usize, flags: c_int, signed: bool) -> *mut PyObject {
    unsafe {
        if buffer.is_null() && n > 0 {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        let little = if flags == -1 || flags & 2 != 0 { cfg!(target_endian = "little") } else { flags & 1 != 0 };
        let signed = signed && (flags == -1 || flags & 4 == 0);
        let s = std::slice::from_raw_parts(buffer as *const u8, n);
        let le: Vec<u8> = if little { s.to_vec() } else { s.iter().rev().copied().collect() };
        from_le_bytes(&le, signed).into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_FromNativeBytes(buffer: *const c_void, n: usize, flags: c_int) -> *mut PyObject {
    unsafe { from_native(buffer, n, flags, true) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_FromUnsignedNativeBytes(buffer: *const c_void, n: usize, flags: c_int) -> *mut PyObject {
    unsafe { from_native(buffer, n, flags, false) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyLong_FromByteArray(bytes: *const u8, n: usize, little_endian: c_int, is_signed: c_int) -> *mut PyObject {
    unsafe {
        let s = if n == 0 { &[][..] } else { std::slice::from_raw_parts(bytes, n) };
        let le: Vec<u8> = if little_endian != 0 { s.to_vec() } else { s.iter().rev().copied().collect() };
        from_le_bytes(&le, is_signed != 0).into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyLong_AsByteArray(v: *mut PyObject, bytes: *mut u8, n: usize, little_endian: c_int, is_signed: c_int, with_exceptions: c_int) -> c_int {
    unsafe {
        let o = &*v;
        let neg = int::sign(o) < 0;
        if neg && is_signed == 0 {
            if with_exceptions != 0 {
                let _ = overflow_error("can't convert negative int to unsigned");
            }
            return -1;
        }
        let tc = twos_complement(o, n);
        for i in 0..n {
            *bytes.add(if little_endian != 0 { i } else { n - 1 - i }) = tc[i];
        }
        let nbits = num_bits(o) as usize;
        let fits = if is_signed != 0 {
            nbits < 8 * n || (neg && nbits == 8 * n && {
                let mag = mag_bytes(o);
                mag.iter().take(mag.len() - 1).all(|&b| b == 0) && mag.last() == Some(&0x80)
            })
        } else {
            nbits <= 8 * n
        };
        if !fits {
            if with_exceptions != 0 {
                let _ = overflow_error("int too big to convert");
            }
            return -1;
        }
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyLong_NumBits(v: *mut PyObject) -> i64 {
    unsafe { num_bits(&*v) as i64 }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyLong_Copy(v: *mut PyObject) -> *mut PyObject {
    unsafe { int::from_digits(int::sign(&*v) < 0, int::digits(&*v)).into_raw() }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyLong_GCD(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        ret((|| -> PyResult {
            let m = crate::import::import_absolute("math")?;
            let g = ab::getattr_str(&m, "gcd")?;
            ab::call(&g, &[&*a, &*b])
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyLong_New(n: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let op = int::alloc_int_of(int::int_type(), (n.max(1)) as usize);
        int::set_sign_and_ndigits(op, if n == 0 { 0 } else { 1 }, n as usize);
        op
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyLong_FromDigits(negative: c_int, n: Py_ssize_t, digits: *const u32) -> *mut PyObject {
    unsafe {
        let ds = if n <= 0 { &[][..] } else { std::slice::from_raw_parts(digits, n as usize) };
        int::from_digits(negative != 0, ds).into_raw()
    }
}

// ---------------------------------------------------------------- sign & co

unsafe fn int_only(o: *mut PyObject) -> PyResult<&'static PyObject> {
    unsafe {
        if o.is_null() {
            return Err(bad_internal());
        }
        if !int::is_int(&*o) {
            return Err(type_error(&format!("expected int, got {}", tname(&*o))));
        }
        Ok(&*o)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_GetSign(o: *mut PyObject, sign: *mut c_int) -> c_int {
    unsafe {
        match int_only(o) {
            Ok(v) => {
                *sign = int::sign(v);
                0
            }
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyLong_Sign(o: *mut PyObject) -> c_int {
    unsafe { int::sign(&*o) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_IsPositive(o: *mut PyObject) -> c_int {
    unsafe { int_only(o).map_or(-1, |v| (int::sign(v) > 0) as c_int) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_IsNegative(o: *mut PyObject) -> c_int {
    unsafe { int_only(o).map_or(-1, |v| (int::sign(v) < 0) as c_int) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_IsZero(o: *mut PyObject) -> c_int {
    unsafe { int_only(o).map_or(-1, |v| (int::sign(v) == 0) as c_int) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_Long_IsCompact(o: *const PyObject) -> c_int {
    unsafe { int::is_compact(&*o) as c_int }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_Long_CompactValue(o: *const PyObject) -> Py_ssize_t {
    unsafe { int::compact_value(&*o) as Py_ssize_t }
}

#[repr(C)]
pub struct PyLongLayout {
    pub bits_per_digit: u8,
    pub digit_size: u8,
    pub digits_order: i8,
    pub digit_endianness: i8,
}
static LAYOUT: PyLongLayout = PyLongLayout { bits_per_digit: 30, digit_size: 4, digits_order: -1, digit_endianness: -1 };
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_GetNativeLayout() -> *const PyLongLayout {
    &LAYOUT
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_GetInfo() -> *mut PyObject {
    ret(crate::modules::sys::get_sys_attr("int_info").ok_or_else(|| system_error("sys.int_info missing")))
}

#[repr(C)]
pub struct PyLongExport {
    pub value: i64,
    pub negative: u8,
    pub ndigits: Py_ssize_t,
    pub digits: *const c_void,
    pub _reserved: usize,
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_Export(o: *mut PyObject, e: *mut PyLongExport) -> c_int {
    unsafe {
        let v = match int_only(o) {
            Ok(v) => v,
            Err(_) => {
                std::ptr::write_bytes(e, 0, 1);
                return -1;
            }
        };
        match int::as_i64(v) {
            Some(x) => {
                (*e).value = x;
                (*e).negative = 0;
                (*e).ndigits = 0;
                (*e).digits = std::ptr::null();
                (*e)._reserved = 0;
            }
            None => {
                let ds = int::digits(v);
                (*e).value = 0;
                (*e).negative = (int::sign(v) < 0) as u8;
                (*e).ndigits = ds.len() as Py_ssize_t;
                (*e).digits = ds.as_ptr() as *const c_void;
                (*e)._reserved = v.to_ref().into_raw() as usize;
            }
        }
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLong_FreeExport(e: *mut PyLongExport) {
    unsafe {
        let p = (*e)._reserved as *mut PyObject;
        if !p.is_null() {
            (*e)._reserved = 0;
            decref(p);
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLongWriter_Create(negative: c_int, ndigits: Py_ssize_t, digits: *mut *mut c_void) -> *mut PyObject {
    unsafe {
        if ndigits <= 0 {
            let _ = value_error("ndigits must be positive");
            if !digits.is_null() {
                *digits = std::ptr::null_mut();
            }
            return std::ptr::null_mut();
        }
        let op = int::alloc_int_of(int::int_type(), ndigits as usize);
        if op.is_null() {
            let _ = memory_error();
            return op;
        }
        int::set_sign_and_ndigits(op, if negative != 0 { -1 } else { 1 }, ndigits as usize);
        *digits = int::digits_mut(op, ndigits as usize).as_mut_ptr() as *mut c_void;
        op
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLongWriter_Finish(w: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = PyRef::from_raw(w);
        int::from_digits(int::sign(&o) < 0, int::digits(&o)).into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyLongWriter_Discard(w: *mut PyObject) {
    unsafe { xdecref(w) }
}

// ---------------------------------------------------------------- bool

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBool_FromLong(v: c_long) -> *mut PyObject {
    bool_::new_bool(v != 0).into_raw()
}

// ---------------------------------------------------------------- float

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFloat_FromDouble(v: f64) -> *mut PyObject {
    float::new_float(v).into_raw()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFloat_AsDouble(o: *mut PyObject) -> f64 {
    unsafe {
        if o.is_null() {
            let _ = type_error("bad argument type for built-in operation");
            return -1.0;
        }
        if float::is_float(&*o) {
            return float::float_value(&*o);
        }
        ab::float_value(&*o).unwrap_or(-1.0)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFloat_FromString(s: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*s;
        let fl = typeobject::type_obj(float::float_type());
        if crate::types::str::is_str(o) || crate::types::bytes::is_bytes(o) || crate::types::bytearray::is_bytearray(o) {
            return ret(ab::call(fl, &[o]));
        }
        // any other object must export a contiguous buffer holding the text
        let contiguous = !crate::types::memoryview::is_memoryview(o) || ab::getattr_str(o, "contiguous").map(|c| c.is(crate::types::bool_::true_obj())).unwrap_or(false);
        if contiguous && super::buffer::PyObject_CheckBuffer(s) != 0 {
            let mut view: Py_buffer = std::mem::zeroed();
            if super::buffer::PyObject_GetBuffer(s, &mut view, 0) < 0 {
                clear_exc();
            } else {
                let data = std::slice::from_raw_parts(view.buf as *const u8, view.len.max(0) as usize).to_vec();
                super::buffer::PyBuffer_Release(&mut view);
                return ret(ab::call(fl, &[&crate::types::bytes::new_bytes(&data)]));
            }
        }
        let _ = type_error(&format!("float() argument must be a string or a contiguous buffer, not '{}'", tname(o)));
        std::ptr::null_mut()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFloat_GetMax() -> f64 {
    f64::MAX
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFloat_GetMin() -> f64 {
    f64::MIN_POSITIVE
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFloat_GetInfo() -> *mut PyObject {
    ret(crate::modules::sys::get_sys_attr("float_info").ok_or_else(|| system_error("sys.float_info missing")))
}

/// The bits of a NaN `x` in a narrower IEEE format with `mbits` mantissa bits and
/// `ebits` exponent bits: the sign is kept and the mantissa (quiet bit + payload) keeps
/// its high bits. A signalling NaN whose payload is shifted out entirely would read as
/// infinity, so it becomes a quiet NaN.
fn narrow_nan(x: f64, mbits: u32, ebits: u32) -> u64 {
    let b = x.to_bits();
    let sign = b >> 63;
    let mut mant = (b & ((1u64 << 52) - 1)) >> (52 - mbits);
    if mant == 0 {
        mant = 1u64 << (mbits - 1);
    }
    (sign << (mbits + ebits)) | (((1u64 << ebits) - 1) << mbits) | mant
}

/// The double for a NaN in a narrower format (None if `bits` is not a NaN); the payload
/// is moved into the high mantissa bits without quieting a signalling NaN.
fn widen_nan(bits: u64, mbits: u32, ebits: u32) -> Option<f64> {
    let emask = (1u64 << ebits) - 1;
    let mant = bits & ((1u64 << mbits) - 1);
    if (bits >> mbits) & emask != emask || mant == 0 {
        return None;
    }
    let sign = (bits >> (mbits + ebits)) & 1;
    Some(f64::from_bits((sign << 63) | (0x7ffu64 << 52) | (mant << (52 - mbits))))
}

/// IEEE half precision pack/unpack (round half to even; OverflowError on overflow).
fn f64_to_f16(x: f64) -> Result<u16, ()> {
    let sign: u16 = if x.is_sign_negative() { 0x8000 } else { 0 };
    let a = x.abs();
    if x.is_nan() {
        return Ok(narrow_nan(x, 10, 5) as u16);
    }
    if a.is_infinite() {
        return Ok(sign | 0x7c00);
    }
    if a == 0.0 {
        return Ok(sign);
    }
    let (m, e) = frexp(a); // a = m * 2**e, 0.5 <= m < 1
    let mut e = e;
    let mut f = m;
    if e > 16 {
        return Err(());
    }
    let bits: u16;
    if e < -13 {
        // subnormal
        f = libm_ldexp(f, 14 + e);
        e = 0;
    } else {
        e += 14;
        f -= 0.5;
        f *= 2.0;
    }
    f *= 1024.0;
    let mut bits_f = f as u32;
    let rem = f - bits_f as f64;
    if rem > 0.5 || (rem == 0.5 && bits_f % 2 == 1) {
        bits_f += 1;
        if bits_f == 1024 {
            bits_f = 0;
            e += 1;
            if e == 31 {
                return Err(());
            }
        }
    }
    bits = ((e as u16) << 10) | bits_f as u16;
    Ok(sign | bits)
}
fn frexp(x: f64) -> (f64, i32) {
    if x == 0.0 || x.is_nan() || x.is_infinite() {
        return (x, 0);
    }
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    if exp == 0 {
        let (m, e) = frexp(x * (1u64 << 54) as f64);
        return (m, e - 54);
    }
    let m = f64::from_bits((bits & !(0x7ffu64 << 52)) | (1022u64 << 52));
    (m, exp - 1022)
}
fn libm_ldexp(x: f64, e: i32) -> f64 {
    x * 2f64.powi(e)
}
fn f16_to_f64(h: u16) -> f64 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let e = ((h >> 10) & 0x1f) as i32;
    let f = (h & 0x3ff) as f64;
    if e == 0x1f {
        if f == 0.0 { sign * f64::INFINITY } else { f64::NAN.copysign(sign) }
    } else if e == 0 {
        sign * f / 1024.0 * 2f64.powi(-14)
    } else {
        sign * (1.0 + f / 1024.0) * 2f64.powi(e - 15)
    }
}

unsafe fn store(p: *mut u8, b: &[u8], le: c_int) {
    unsafe {
        let n = b.len();
        for i in 0..n {
            *p.add(i) = if le != 0 { b[i] } else { b[n - 1 - i] };
        }
    }
}
unsafe fn load<const N: usize>(p: *const u8, le: c_int) -> [u8; N] {
    unsafe {
        let mut b = [0u8; N];
        for i in 0..N {
            b[i] = if le != 0 { *p.add(i) } else { *p.add(N - 1 - i) };
        }
        b
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFloat_Pack2(x: f64, p: *mut c_char, le: c_int) -> c_int {
    unsafe {
        match f64_to_f16(x) {
            Ok(h) => {
                store(p as *mut u8, &h.to_le_bytes(), le);
                0
            }
            Err(_) => {
                let _ = overflow_error("float too large to pack with e format");
                -1
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFloat_Pack4(x: f64, p: *mut c_char, le: c_int) -> c_int {
    unsafe {
        let y = x as f32;
        if y.is_infinite() && x.is_finite() {
            let _ = overflow_error("float too large to pack with f format");
            return -1;
        }
        // NaNs are converted bit by bit: the hardware conversion would quiet a
        // signalling NaN
        let bits = if x.is_nan() { narrow_nan(x, 23, 8) as u32 } else { y.to_bits() };
        store(p as *mut u8, &bits.to_le_bytes(), le);
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFloat_Pack8(x: f64, p: *mut c_char, le: c_int) -> c_int {
    unsafe {
        store(p as *mut u8, &x.to_bits().to_le_bytes(), le);
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFloat_Unpack2(p: *const c_char, le: c_int) -> f64 {
    unsafe {
        let h = u16::from_le_bytes(load::<2>(p as *const u8, le));
        if let Some(x) = widen_nan(h as u64, 10, 5) {
            return x;
        }
        f16_to_f64(h)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFloat_Unpack4(p: *const c_char, le: c_int) -> f64 {
    unsafe {
        let b = u32::from_le_bytes(load::<4>(p as *const u8, le));
        if let Some(x) = widen_nan(b as u64, 23, 8) {
            return x;
        }
        f32::from_bits(b) as f64
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFloat_Unpack8(p: *const c_char, le: c_int) -> f64 {
    unsafe { f64::from_bits(u64::from_le_bytes(load::<8>(p as *const u8, le))) }
}

// ---------------------------------------------------------------- complex

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyComplex_FromDoubles(re: f64, im: f64) -> *mut PyObject {
    complex::new_complex(re, im).into_raw()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyComplex_FromCComplex(c: Py_complex) -> *mut PyObject {
    complex::new_complex(c.real, c.imag).into_raw()
}
unsafe fn as_complex(o: &PyObject) -> PyResult<Py_complex> {
    if complex::is_complex(o) {
        return Ok(complex::complex_value(o));
    }
    if let Some(r) = ab::call_special_opt(o, crate::name!("__complex__"), &[])? {
        if !complex::is_complex(&r) {
            return Err(type_error(&format!("__complex__ returned non-complex (type {})", tname(&r))));
        }
        return Ok(complex::complex_value(&r));
    }
    let f = ab::float_value(o)?;
    Ok(Py_complex { real: f, imag: 0.0 })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyComplex_AsCComplex(o: *mut PyObject) -> Py_complex {
    unsafe {
        if o.is_null() {
            let _ = bad_internal();
            return Py_complex { real: -1.0, imag: 0.0 };
        }
        as_complex(&*o).unwrap_or(Py_complex { real: -1.0, imag: 0.0 })
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyComplex_RealAsDouble(o: *mut PyObject) -> f64 {
    unsafe {
        if o.is_null() {
            let _ = bad_internal();
            return -1.0;
        }
        if complex::is_complex(&*o) {
            return complex::complex_value(&*o).real;
        }
        match as_complex(&*o) {
            Ok(c) => c.real,
            Err(_) => -1.0,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyComplex_ImagAsDouble(o: *mut PyObject) -> f64 {
    unsafe {
        if o.is_null() {
            let _ = bad_internal();
            return -1.0;
        }
        if complex::is_complex(&*o) {
            return complex::complex_value(&*o).imag;
        }
        match as_complex(&*o) {
            Ok(c) => c.imag,
            Err(_) => -1.0,
        }
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn _Py_c_sum(a: Py_complex, b: Py_complex) -> Py_complex {
    Py_complex { real: a.real + b.real, imag: a.imag + b.imag }
}
#[unsafe(no_mangle)]
pub extern "C" fn _Py_c_diff(a: Py_complex, b: Py_complex) -> Py_complex {
    Py_complex { real: a.real - b.real, imag: a.imag - b.imag }
}
#[unsafe(no_mangle)]
pub extern "C" fn _Py_c_neg(a: Py_complex) -> Py_complex {
    Py_complex { real: -a.real, imag: -a.imag }
}
#[unsafe(no_mangle)]
pub extern "C" fn _Py_c_prod(a: Py_complex, b: Py_complex) -> Py_complex {
    Py_complex { real: a.real * b.real - a.imag * b.imag, imag: a.real * b.imag + a.imag * b.real }
}
#[unsafe(no_mangle)]
pub extern "C" fn _Py_c_quot(a: Py_complex, b: Py_complex) -> Py_complex {
    // Smith's algorithm (same as complex.__truediv__); sets errno on division by zero
    let (abs_br, abs_bi) = (b.real.abs(), b.imag.abs());
    if abs_br >= abs_bi {
        if abs_br == 0.0 {
            unsafe { *libc::__errno_location() = libc::EDOM };
            return Py_complex { real: 0.0, imag: 0.0 };
        }
        let ratio = b.imag / b.real;
        let denom = b.real + b.imag * ratio;
        Py_complex { real: (a.real + a.imag * ratio) / denom, imag: (a.imag - a.real * ratio) / denom }
    } else if abs_bi >= abs_br {
        let ratio = b.real / b.imag;
        let denom = b.real * ratio + b.imag;
        Py_complex { real: (a.real * ratio + a.imag) / denom, imag: (a.imag * ratio - a.real) / denom }
    } else {
        Py_complex { real: f64::NAN, imag: f64::NAN }
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn _Py_c_pow(a: Py_complex, b: Py_complex) -> Py_complex {
    if b.real == 0.0 && b.imag == 0.0 {
        return Py_complex { real: 1.0, imag: 0.0 };
    }
    if a.real == 0.0 && a.imag == 0.0 {
        if b.imag != 0.0 || b.real < 0.0 {
            unsafe { *libc::__errno_location() = libc::EDOM };
        }
        return Py_complex { real: 0.0, imag: 0.0 };
    }
    let vabs = a.real.hypot(a.imag);
    let mut len = vabs.powf(b.real);
    let at = a.imag.atan2(a.real);
    let mut phase = at * b.real;
    if b.imag != 0.0 {
        len /= (at * b.imag).exp();
        phase += b.imag * vabs.ln();
    }
    Py_complex { real: len * phase.cos(), imag: len * phase.sin() }
}
#[unsafe(no_mangle)]
pub extern "C" fn _Py_c_abs(a: Py_complex) -> f64 {
    if a.real.is_infinite() || a.imag.is_infinite() {
        return f64::INFINITY;
    }
    if a.real.is_nan() || a.imag.is_nan() {
        return f64::NAN;
    }
    let r = a.real.hypot(a.imag);
    if r.is_infinite() {
        unsafe { *libc::__errno_location() = libc::ERANGE };
    }
    r
}

// ---------------------------------------------------------------- hashing

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_HashDouble(_inst: *mut PyObject, v: f64) -> Py_hash_t {
    float::float_hash(&float::new_float(v))
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_HashPointer(p: *const c_void) -> Py_hash_t {
    let y = p as usize;
    let h = y.rotate_right(4) as isize;
    if h == -1 { -2 } else { h }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_HashBuffer(p: *const c_void, len: Py_ssize_t) -> Py_hash_t {
    unsafe {
        let s = if len <= 0 { &[][..] } else { std::slice::from_raw_parts(p as *const u8, len as usize) };
        crate::hash::hash_bytes(s)
    }
}

#[repr(C)]
pub struct PyHash_FuncDef {
    pub hash: Option<unsafe extern "C" fn(*const c_void, Py_ssize_t) -> Py_hash_t>,
    pub name: *const c_char,
    pub hash_bits: c_int,
    pub seed_bits: c_int,
}
unsafe impl Sync for PyHash_FuncDef {}
static HASH_FUNCDEF: PyHash_FuncDef = PyHash_FuncDef { hash: Some(Py_HashBuffer), name: c"siphash13".as_ptr(), hash_bits: 64, seed_bits: 128 };
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyHash_GetFuncDef() -> *const PyHash_FuncDef {
    &HASH_FUNCDEF
}

#[allow(dead_code)]
fn _unused(_: c_uint) {}
