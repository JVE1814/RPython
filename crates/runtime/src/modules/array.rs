//! The `array` module: compact homogeneous arrays of numbers / characters
//! (data workstream).
//!
//! Items are stored as raw native-endian bytes in a `Vec<u8>`; each typecode
//! knows how to convert between its raw form and Python objects.  Arrays
//! export their storage through the buffer protocol; while exports exist,
//! size-changing operations raise BufferError.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::*;
use crate::types::{bytearray, bytes, dict, float, int, list, none, slice, str as pystr, tuple};
use crate::{mdefs, name, raise};
use std::ffi::{c_int, c_void};

// ===================================================================== item kinds

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F16,
    F32,
    F64,
    C32,
    C64,
    Wchar,
    Ucs4,
}

struct Code {
    code: &'static str,
    kind: Kind,
    size: usize,
    format: &'static [u8], // NUL-terminated buffer format
    mformat: i64,
}

// Machine format codes (shared with _array_reconstructor callers).
const UNSIGNED_INT8: i64 = 0;
const SIGNED_INT8: i64 = 1;
const UNSIGNED_INT16_LE: i64 = 2;
const SIGNED_INT16_LE: i64 = 4;
const UNSIGNED_INT32_LE: i64 = 6;
const SIGNED_INT32_LE: i64 = 8;
const UNSIGNED_INT64_LE: i64 = 10;
const SIGNED_INT64_LE: i64 = 12;
const FLOAT_LE: i64 = 14;
const DOUBLE_LE: i64 = 16;
const UTF16_LE: i64 = 18;
const UTF32_LE: i64 = 20;
const FLOAT_COMPLEX_LE: i64 = 22;
const DOUBLE_COMPLEX_LE: i64 = 24;
const FLOAT16_LE: i64 = 26;
const MFORMAT_MAX: i64 = 27;

static CODES: [Code; 17] = [
    Code { code: "b", kind: Kind::I8, size: 1, format: b"b\0", mformat: SIGNED_INT8 },
    Code { code: "B", kind: Kind::U8, size: 1, format: b"B\0", mformat: UNSIGNED_INT8 },
    Code { code: "u", kind: Kind::Wchar, size: 4, format: b"w\0", mformat: UTF32_LE },
    Code { code: "w", kind: Kind::Ucs4, size: 4, format: b"w\0", mformat: UTF32_LE },
    Code { code: "h", kind: Kind::I16, size: 2, format: b"h\0", mformat: SIGNED_INT16_LE },
    Code { code: "H", kind: Kind::U16, size: 2, format: b"H\0", mformat: UNSIGNED_INT16_LE },
    Code { code: "i", kind: Kind::I32, size: 4, format: b"i\0", mformat: SIGNED_INT32_LE },
    Code { code: "I", kind: Kind::U32, size: 4, format: b"I\0", mformat: UNSIGNED_INT32_LE },
    Code { code: "l", kind: Kind::I64, size: 8, format: b"l\0", mformat: SIGNED_INT64_LE },
    Code { code: "L", kind: Kind::U64, size: 8, format: b"L\0", mformat: UNSIGNED_INT64_LE },
    Code { code: "q", kind: Kind::I64, size: 8, format: b"q\0", mformat: SIGNED_INT64_LE },
    Code { code: "Q", kind: Kind::U64, size: 8, format: b"Q\0", mformat: UNSIGNED_INT64_LE },
    Code { code: "e", kind: Kind::F16, size: 2, format: b"e\0", mformat: FLOAT16_LE },
    Code { code: "f", kind: Kind::F32, size: 4, format: b"f\0", mformat: FLOAT_LE },
    Code { code: "d", kind: Kind::F64, size: 8, format: b"d\0", mformat: DOUBLE_LE },
    Code { code: "Zf", kind: Kind::C32, size: 8, format: b"Zf\0", mformat: FLOAT_COMPLEX_LE },
    Code { code: "Zd", kind: Kind::C64, size: 16, format: b"Zd\0", mformat: DOUBLE_COMPLEX_LE },
];

fn find_code(s: &str) -> Option<&'static Code> {
    CODES.iter().find(|c| c.code == s)
}

fn is_int_kind(k: Kind) -> bool {
    matches!(k, Kind::I8 | Kind::U8 | Kind::I16 | Kind::U16 | Kind::I32 | Kind::U32 | Kind::I64 | Kind::U64)
}
fn is_char_kind(k: Kind) -> bool {
    matches!(k, Kind::Wchar | Kind::Ucs4)
}

fn int_range(k: Kind) -> (i128, i128) {
    match k {
        Kind::I8 => (i8::MIN as i128, i8::MAX as i128),
        Kind::U8 => (0, u8::MAX as i128),
        Kind::I16 => (i16::MIN as i128, i16::MAX as i128),
        Kind::U16 => (0, u16::MAX as i128),
        Kind::I32 => (i32::MIN as i128, i32::MAX as i128),
        Kind::U32 => (0, u32::MAX as i128),
        Kind::I64 => (i64::MIN as i128, i64::MAX as i128),
        Kind::U64 => (0, u64::MAX as i128),
        _ => (0, 0),
    }
}

fn int_kind_name(k: Kind) -> &'static str {
    match k {
        Kind::I8 => "signed char",
        Kind::U8 => "unsigned byte integer",
        Kind::I16 => "signed short integer",
        Kind::U16 => "unsigned short",
        Kind::I32 => "signed integer",
        Kind::U32 => "unsigned int",
        Kind::I64 => "signed long integer",
        Kind::U64 => "unsigned long",
        _ => "value",
    }
}

/// Convert a Python object to the raw item bytes for `code`.
fn encode_item(code: &Code, v: &PyObject, out: &mut [u8]) -> PyResult<()> {
    let k = code.kind;
    if is_int_kind(k) {
        if float::is_float(v) {
            raise!(TypeError, "'float' object cannot be interpreted as an integer");
        }
        let i = ab::index(v)?;
        let (lo, hi) = int_range(k);
        let x = match int::as_i128(&i) {
            Some(x) => x,
            None => {
                if int::sign(&i) < 0 {
                    raise!(OverflowError, "{} is less than minimum", int_kind_name(k));
                }
                raise!(OverflowError, "{} is greater than maximum", int_kind_name(k));
            }
        };
        if x < lo {
            raise!(OverflowError, "{} is less than minimum", int_kind_name(k));
        }
        if x > hi {
            raise!(OverflowError, "{} is greater than maximum", int_kind_name(k));
        }
        let le = (x as u128).to_le_bytes();
        out.copy_from_slice(&le[..code.size]);
        return Ok(());
    }
    match k {
        Kind::F16 => {
            let x = ab::float_value(v)?;
            match crate::types::memoryview::pack_half(x) {
                Some(h) => out.copy_from_slice(&h.to_le_bytes()),
                None => raise!(OverflowError, "float too large to pack with e format"),
            }
        }
        Kind::F32 => {
            let x = ab::float_value(v)?;
            out.copy_from_slice(&(x as f32).to_le_bytes());
        }
        Kind::F64 => {
            let x = ab::float_value(v)?;
            out.copy_from_slice(&x.to_le_bytes());
        }
        Kind::C32 => {
            let (r, i) = crate::types::complex_methods::as_ccomplex(v)?;
            out[..4].copy_from_slice(&(r as f32).to_le_bytes());
            out[4..].copy_from_slice(&(i as f32).to_le_bytes());
        }
        Kind::C64 => {
            let (r, i) = crate::types::complex_methods::as_ccomplex(v)?;
            out[..8].copy_from_slice(&r.to_le_bytes());
            out[8..].copy_from_slice(&i.to_le_bytes());
        }
        Kind::Wchar | Kind::Ucs4 => {
            if !pystr::is_str(v) {
                raise!(TypeError, "array item must be a unicode character, not {}", tname(v));
            }
            if pystr::str_len(v) != 1 {
                raise!(TypeError, "array item must be a unicode character, not a string of length {}", pystr::str_len(v));
            }
            let c = pystr::str_data(v).get(0);
            out.copy_from_slice(&c.to_le_bytes());
        }
        _ => unreachable!(),
    }
    Ok(())
}

fn rd_u64(b: &[u8]) -> u64 {
    let mut a = [0u8; 8];
    a[..b.len()].copy_from_slice(b);
    u64::from_le_bytes(a)
}

/// Raw item bytes -> Python object.
fn decode_item(code: &Code, b: &[u8]) -> PyResult {
    Ok(match code.kind {
        Kind::I8 => int::new_int(b[0] as i8 as i64),
        Kind::U8 => int::new_int(b[0] as i64),
        Kind::I16 => int::new_int(i16::from_le_bytes([b[0], b[1]]) as i64),
        Kind::U16 => int::new_int(u16::from_le_bytes([b[0], b[1]]) as i64),
        Kind::I32 => int::new_int(i32::from_le_bytes(b[..4].try_into().unwrap()) as i64),
        Kind::U32 => int::new_int(u32::from_le_bytes(b[..4].try_into().unwrap()) as i64),
        Kind::I64 => int::new_int(i64::from_le_bytes(b[..8].try_into().unwrap())),
        Kind::U64 => int::new_int_u128(rd_u64(b) as u128),
        Kind::F16 => float::new_float(crate::types::memoryview::unpack_half(u16::from_le_bytes([b[0], b[1]]))),
        Kind::F32 => float::new_float(f32::from_le_bytes(b[..4].try_into().unwrap()) as f64),
        Kind::F64 => float::new_float(f64::from_le_bytes(b[..8].try_into().unwrap())),
        Kind::C32 => crate::types::complex::new_complex(
            f32::from_le_bytes(b[..4].try_into().unwrap()) as f64,
            f32::from_le_bytes(b[4..8].try_into().unwrap()) as f64,
        ),
        Kind::C64 => crate::types::complex::new_complex(f64::from_le_bytes(b[..8].try_into().unwrap()), f64::from_le_bytes(b[8..16].try_into().unwrap())),
        Kind::Wchar | Kind::Ucs4 => {
            let c = u32::from_le_bytes(b[..4].try_into().unwrap());
            if c > 0x10ffff {
                raise!(ValueError, "character U+{:x} is not in range [U+0000; U+10ffff]", c);
            }
            pystr::from_codepoints(&[c])
        }
    })
}

// ===================================================================== object

#[repr(C)]
struct ArrayObj {
    ob_base: PyVarObject,
    data: *mut Vec<u8>,
    allocated: isize,
    code: *const Code,
    weakreflist: *mut PyObject,
    exports: c_int,
}

static mut ARRAY_TYPE: PyTypeObject = PyTypeObject::ZERO;
static mut ITER_TYPE: PyTypeObject = PyTypeObject::ZERO;

fn array_type() -> *mut PyTypeObject {
    &raw mut ARRAY_TYPE
}
fn iter_type() -> *mut PyTypeObject {
    &raw mut ITER_TYPE
}

fn arr(o: &PyObject) -> &mut ArrayObj {
    unsafe { &mut *(o.as_ptr() as *mut ArrayObj) }
}

fn data(o: &PyObject) -> &mut Vec<u8> {
    let a = arr(o);
    unsafe {
        if a.data.is_null() {
            a.data = Box::into_raw(Box::new(Vec::new()));
        }
        &mut *a.data
    }
}

fn code_of(o: &PyObject) -> &'static Code {
    unsafe { &*arr(o).code }
}

fn nitems(o: &PyObject) -> usize {
    data(o).len() / code_of(o).size
}

fn sync_size(o: &PyObject) {
    let n = nitems(o) as isize;
    let a = arr(o);
    a.ob_base.ob_size = n;
    a.allocated = (data(o).capacity() / code_of(o).size) as isize;
}

fn is_array(o: &PyObject) -> bool {
    is_subtype(o.type_ptr(), array_type())
}

fn check_resizable(o: &PyObject) -> PyResult<()> {
    if arr(o).exports > 0 {
        raise!(BufferError, "cannot resize an array that is exporting buffers");
    }
    Ok(())
}

fn new_array(tp: *mut PyTypeObject, code: &'static Code, bytes_: Vec<u8>) -> PyResult {
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
        let op = alloc(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        let a = &mut *(op as *mut ArrayObj);
        a.data = Box::into_raw(Box::new(bytes_));
        a.code = code;
        let r = PyRef::from_raw(op);
        sync_size(&r);
        Ok(r)
    }
}

fn item_bytes(o: &PyObject, i: usize) -> &[u8] {
    let sz = code_of(o).size;
    &data(o)[i * sz..(i + 1) * sz]
}

fn get_item(o: &PyObject, i: usize) -> PyResult {
    decode_item(code_of(o), item_bytes(o, i))
}

fn encode_value(o: &PyObject, v: &PyObject) -> PyResult<Vec<u8>> {
    let c = code_of(o);
    let mut buf = vec![0u8; c.size];
    encode_item(c, v, &mut buf)?;
    Ok(buf)
}

fn append_obj(o: &PyObject, v: &PyObject) -> PyResult<()> {
    let b = encode_value(o, v)?;
    check_resizable(o)?;
    data(o).extend_from_slice(&b);
    sync_size(o);
    Ok(())
}

/// Append all items of an iterable (rolled back on error).
fn extend_iter(o: &PyObject, it: &PyObject) -> PyResult<()> {
    let c = code_of(o);
    let mut buf = Vec::new();
    let iter = ab::get_iter(it)?;
    let mut tmp = vec![0u8; c.size];
    while let Some(v) = ab::iter_next(&iter)? {
        encode_item(c, &v, &mut tmp)?;
        buf.extend_from_slice(&tmp);
    }
    check_resizable(o)?;
    data(o).extend_from_slice(&buf);
    sync_size(o);
    Ok(())
}

fn extend_bytes(o: &PyObject, b: &[u8]) -> PyResult<()> {
    let sz = code_of(o).size;
    if b.len() % sz != 0 {
        raise!(ValueError, "bytes length not a multiple of item size");
    }
    check_resizable(o)?;
    data(o).extend_from_slice(b);
    sync_size(o);
    Ok(())
}

fn extend_str(o: &PyObject, s: &PyObject) -> PyResult<()> {
    let d = pystr::str_data(s);
    let mut buf = Vec::with_capacity(d.len() * 4);
    for c in d.iter() {
        buf.extend_from_slice(&c.to_le_bytes());
    }
    check_resizable(o)?;
    data(o).extend_from_slice(&buf);
    sync_size(o);
    Ok(())
}

/// Items of another array converted to this array's typecode.
fn extend_from_array(o: &PyObject, other: &PyObject) -> PyResult<()> {
    let (c, oc) = (code_of(o), code_of(other));
    if std::ptr::eq(c, oc) || c.code == oc.code {
        let b = data(other).clone();
        return extend_bytes(o, &b);
    }
    let n = nitems(other);
    let mut buf = Vec::with_capacity(n * c.size);
    let mut tmp = vec![0u8; c.size];
    for i in 0..n {
        let v = get_item(other, i)?;
        encode_item(c, &v, &mut tmp)?;
        buf.extend_from_slice(&tmp);
    }
    check_resizable(o)?;
    data(o).extend_from_slice(&buf);
    sync_size(o);
    Ok(())
}

// ===================================================================== construction

fn typecode_arg(v: &PyObject) -> PyResult<&'static Code> {
    if !pystr::is_str(v) {
        raise!(TypeError, "array() argument 1 must be a unicode character, not {}", tname(v));
    }
    let s = pystr::to_string_lossy(v);
    match find_code(&s) {
        Some(c) => Ok(c),
        None => raise!(ValueError, "bad typecode (must be b, B, u, w, h, H, i, I, l, L, q, Q, e, f, d, Zf or Zd)"),
    }
}

fn warn_u() -> PyResult<()> {
    crate::modules::warnings::warn(
        crate::types::exceptions::DeprecationWarning(),
        "The 'u' type code is deprecated and will be removed in Python 3.16",
        1,
    )
}

fn construct(tp: *mut PyTypeObject, args: &[&PyObject]) -> PyResult {
    if args.is_empty() || args.len() > 2 {
        raise!(TypeError, "array() takes at most 2 arguments ({} given)", args.len());
    }
    let code = typecode_arg(args[0])?;
    if code.code == "u" {
        warn_u()?;
    }
    let a = new_array(tp, code, Vec::new())?;
    if let Some(init) = args.get(1) {
        let init = *init;
        if pystr::is_str(init) {
            if !is_char_kind(code.kind) {
                raise!(TypeError, "cannot use a str to initialize an array with typecode '{}'", code.code);
            }
            extend_str(&a, init)?;
        } else if is_array(init) {
            if is_char_kind(code_of(init).kind) && !is_char_kind(code.kind) {
                raise!(TypeError, "cannot use a unicode array to initialize an array with typecode '{}'", code.code);
            }
            extend_from_array(&a, init)?;
        } else if bytes::is_bytes(init) || bytearray::is_bytearray(init) {
            let b = crate::buffer::buffer_bytes(init).unwrap_or_default();
            extend_bytes(&a, &b)?;
        } else if init.is(none::none()) {
            raise!(TypeError, "cannot use NoneType to initialize an array");
        } else {
            extend_iter(&a, init)?;
        }
        let v = data(&a);
        v.shrink_to_fit();
        sync_size(&a);
    }
    Ok(a)
}

unsafe extern "C" fn array_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            if tp == array_type() && !kw.is_null() && dict::dict_len(&*kw) > 0 {
                raise!(TypeError, "array.array() takes no keyword arguments");
            }
            let a = tuple::tuple_items(&*args);
            construct(tp, a)
        })();
        into_c(r)
    }
}

unsafe extern "C" fn array_dealloc(o: *mut PyObject) {
    unsafe {
        let tp = (*o).type_ptr();
        let a = &mut *(o as *mut ArrayObj);
        if !a.weakreflist.is_null() {
            crate::types::weakref::clear_weakrefs(o);
        }
        if !a.data.is_null() {
            drop(Box::from_raw(a.data));
            a.data = std::ptr::null_mut();
        }
        let free = (*tp).tp_free;
        crate::types::typeobject::generic_free_and_type_decref(o, tp);
        let _ = free;
    }
}

// ===================================================================== methods

fn m_append(o: &PyObject, v: &PyObject) -> PyResult {
    append_obj(o, v)?;
    Ok(none::none_ref())
}

fn m_buffer_info(o: &PyObject) -> PyResult {
    let p = data(o).as_ptr() as usize;
    Ok(tuple::tuple2(int::new_int_usize(p), int::new_int_usize(nitems(o))))
}

fn m_byteswap(o: &PyObject) -> PyResult {
    let c = code_of(o);
    let d = data(o);
    let half = match c.kind {
        Kind::C32 | Kind::C64 => c.size / 2,
        _ => c.size,
    };
    for chunk in d.chunks_mut(half) {
        chunk.reverse();
    }
    Ok(none::none_ref())
}

fn items_equal(o: &PyObject, i: usize, v: &PyObject) -> PyResult<bool> {
    let it = get_item(o, i)?;
    ab::rich_compare_bool(&it, v, Py_EQ)
}

fn m_count(o: &PyObject, v: &PyObject) -> PyResult {
    let mut n = 0;
    let mut i = 0;
    while i < nitems(o) {
        if items_equal(o, i, v)? {
            n += 1;
        }
        i += 1;
    }
    Ok(int::new_int(n))
}

fn m_extend(o: &PyObject, v: &PyObject) -> PyResult {
    if is_array(v) {
        if code_of(v).code != code_of(o).code {
            raise!(TypeError, "can only extend with array of same kind");
        }
        let b = data(v).clone();
        extend_bytes(o, &b)?;
    } else {
        extend_iter(o, v)?;
    }
    Ok(none::none_ref())
}

fn m_frombytes(o: &PyObject, v: &PyObject) -> PyResult {
    if pystr::is_str(v) {
        raise!(TypeError, "a bytes-like object is required, not 'str'");
    }
    let b = match crate::buffer::buffer_bytes(v) {
        Some(b) => b,
        None => raise!(TypeError, "a bytes-like object is required, not '{}'", tname(v)),
    };
    extend_bytes(o, &b)?;
    Ok(none::none_ref())
}

fn m_fromfile(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("fromfile", args, 2, 2)?;
    let n = ab::index_as_ssize(args[1], None)?;
    if n < 0 {
        raise!(ValueError, "negative count");
    }
    let sz = code_of(o).size;
    let want = (n as usize).checked_mul(sz).ok_or_else(|| err!(MemoryError, ""))?;
    let r = ab::call_method(args[0], name!("read"), &[&int::new_int(want as i64)])?;
    if !bytes::is_bytes(&r) {
        raise!(TypeError, "read() didn't return bytes");
    }
    let b = bytes::bytes_data(&r);
    let usable = b.len() - b.len() % sz;
    extend_bytes(o, &b[..usable])?;
    if b.len() != want {
        raise!(EOFError, "read() didn't return enough bytes");
    }
    Ok(none::none_ref())
}

fn m_fromlist(o: &PyObject, v: &PyObject) -> PyResult {
    if !list::is_list(v) {
        raise!(TypeError, "arg must be list");
    }
    let c = code_of(o);
    let mut buf = Vec::new();
    let mut tmp = vec![0u8; c.size];
    let mut i = 0;
    while let Some(item) = list::get(v, i) {
        encode_item(c, &item, &mut tmp)?;
        buf.extend_from_slice(&tmp);
        i += 1;
    }
    check_resizable(o)?;
    data(o).extend_from_slice(&buf);
    sync_size(o);
    Ok(none::none_ref())
}

fn m_fromunicode(o: &PyObject, v: &PyObject) -> PyResult {
    if !pystr::is_str(v) {
        raise!(TypeError, "fromunicode() argument must be str, not {}", tname(v));
    }
    if !is_char_kind(code_of(o).kind) {
        raise!(ValueError, "fromunicode() may only be called on unicode type arrays ('u' or 'w')");
    }
    extend_str(o, v)?;
    Ok(none::none_ref())
}

fn norm_index(i: isize, n: usize) -> isize {
    if i < 0 { (i + n as isize).max(0) } else { i.min(n as isize) }
}

fn m_index(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("index", args, 1, 3)?;
    let n = nitems(o);
    let start = match args.get(1) {
        Some(s) => norm_index(slice::slice_index(s)?, n),
        None => 0,
    };
    let stop = match args.get(2) {
        Some(s) => norm_index(slice::slice_index(s)?, n),
        None => n as isize,
    };
    let mut i = start;
    while i < stop && (i as usize) < nitems(o) {
        if items_equal(o, i as usize, args[0])? {
            return Ok(int::new_int(i as i64));
        }
        i += 1;
    }
    raise!(ValueError, "array.index(x): x not in array");
}

fn m_insert(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("insert", args, 2, 2)?;
    let i = ab::index_as_ssize(args[0], None)?;
    let b = encode_value(o, args[1])?;
    check_resizable(o)?;
    let n = nitems(o);
    let pos = norm_index(i, n) as usize;
    let sz = code_of(o).size;
    let d = data(o);
    d.splice(pos * sz..pos * sz, b);
    sync_size(o);
    Ok(none::none_ref())
}

fn m_pop(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("pop", args, 0, 1)?;
    let i = match args.first() {
        Some(a) => ab::index_as_ssize(a, None)?,
        None => -1,
    };
    let n = nitems(o) as isize;
    if n == 0 {
        raise!(IndexError, "pop from empty array");
    }
    let idx = if i < 0 { i + n } else { i };
    if idx < 0 || idx >= n {
        raise!(IndexError, "pop index out of range");
    }
    let v = get_item(o, idx as usize)?;
    check_resizable(o)?;
    let sz = code_of(o).size;
    let s = idx as usize * sz;
    data(o).drain(s..s + sz);
    sync_size(o);
    Ok(v)
}

fn m_remove(o: &PyObject, v: &PyObject) -> PyResult {
    let mut i = 0;
    while i < nitems(o) {
        if items_equal(o, i, v)? {
            check_resizable(o)?;
            let sz = code_of(o).size;
            data(o).drain(i * sz..(i + 1) * sz);
            sync_size(o);
            return Ok(none::none_ref());
        }
        i += 1;
    }
    raise!(ValueError, "array.remove(x): x not in array");
}

fn m_reverse(o: &PyObject) -> PyResult {
    let sz = code_of(o).size;
    let n = nitems(o);
    let d = data(o);
    for i in 0..n / 2 {
        let j = n - 1 - i;
        for k in 0..sz {
            d.swap(i * sz + k, j * sz + k);
        }
    }
    Ok(none::none_ref())
}

fn m_tobytes(o: &PyObject) -> PyResult {
    Ok(bytes::new_bytes(data(o)))
}

fn m_tofile(o: &PyObject, f: &PyObject) -> PyResult {
    const BLOCK: usize = 64 * 1024;
    let all = data(o).clone();
    if all.is_empty() {
        return Ok(none::none_ref());
    }
    for chunk in all.chunks(BLOCK) {
        ab::call_method(f, name!("write"), &[&bytes::new_bytes(chunk)])?;
    }
    Ok(none::none_ref())
}

fn m_tolist(o: &PyObject) -> PyResult {
    let n = nitems(o);
    let mut v = Vec::with_capacity(n);
    for i in 0..n {
        v.push(get_item(o, i)?);
    }
    Ok(list::new_list(v))
}

fn to_unicode(o: &PyObject) -> PyResult {
    let mut cps = Vec::with_capacity(nitems(o));
    for chunk in data(o).chunks(4) {
        let c = u32::from_le_bytes(chunk.try_into().unwrap());
        if c > 0x10ffff {
            raise!(ValueError, "character U+{:x} is not in range [U+0000; U+10ffff]", c);
        }
        cps.push(c);
    }
    Ok(pystr::from_codepoints(&cps))
}

fn m_tounicode(o: &PyObject) -> PyResult {
    if !is_char_kind(code_of(o).kind) {
        raise!(ValueError, "tounicode() may only be called on unicode type arrays ('u' or 'w')");
    }
    to_unicode(o)
}

fn m_clear(o: &PyObject) -> PyResult {
    check_resizable(o)?;
    data(o).clear();
    sync_size(o);
    Ok(none::none_ref())
}

fn copy_array(o: &PyObject) -> PyResult {
    new_array(o.type_ptr(), code_of(o), data(o).clone())
}

fn m_copy(o: &PyObject) -> PyResult {
    copy_array(o)
}

fn m_deepcopy(o: &PyObject, _memo: &PyObject) -> PyResult {
    copy_array(o)
}

fn m_sizeof(o: &PyObject) -> PyResult {
    let base = unsafe { (*o.type_ptr()).tp_basicsize } as usize;
    Ok(int::new_int((base + data(o).capacity()) as i64))
}

fn module_obj() -> PyResult {
    crate::import::import_absolute("array")
}

fn m_reduce_ex(o: &PyObject, proto: &PyObject) -> PyResult {
    let p = ab::index_as_ssize(proto, None)?;
    let c = code_of(o);
    let dict = match ab::getattr_opt(o, name!("__dict__"))? {
        Some(d) => d,
        None => none::none_ref(),
    };
    let dict = if dict::is_dict(&dict) && dict::dict_len(&dict) == 0 { none::none_ref() } else { dict };
    let cls = type_ref(o.type_ptr());
    if p < 3 {
        let l = m_tolist(o)?;
        let args = tuple::tuple2(pystr::new_str(c.code), l);
        return Ok(tuple::new_tuple(vec![cls, args, dict]));
    }
    let rec = ab::getattr_str(&*module_obj()?, "_array_reconstructor")?;
    let args = tuple::new_tuple(vec![cls, pystr::new_str(c.code), int::new_int(c.mformat), bytes::new_bytes(data(o))]);
    Ok(tuple::new_tuple(vec![rec, args, dict]))
}

fn m_class_getitem(cls: &PyObject, item: &PyObject) -> PyResult {
    crate::types::containers_reg::generic_alias(cls, item)
}

fn get_typecode(o: &PyObject) -> PyResult {
    Ok(pystr::new_str(code_of(o).code))
}
fn get_itemsize(o: &PyObject) -> PyResult {
    Ok(int::new_int(code_of(o).size as i64))
}

// ===================================================================== protocols

fn compare(a: &PyObject, b: &PyObject, op: c_int) -> PyResult {
    let (na, nb) = (nitems(a), nitems(b));
    if (op == Py_EQ || op == Py_NE) && na != nb {
        return Ok(crate::types::bool_::new_bool(op == Py_NE));
    }
    // identical typecodes of integer kind: compare raw values
    let mut i = 0;
    while i < na.min(nb) && i < nitems(a) && i < nitems(b) {
        let x = get_item(a, i)?;
        let y = get_item(b, i)?;
        if !ab::rich_compare_bool(&x, &y, Py_EQ)? {
            if op == Py_EQ {
                return Ok(crate::types::bool_::new_bool(false));
            }
            if op == Py_NE {
                return Ok(crate::types::bool_::new_bool(true));
            }
            return ab::rich_compare(&x, &y, op);
        }
        i += 1;
    }
    let (na, nb) = (nitems(a), nitems(b));
    let r = match op {
        Py_LT => na < nb,
        Py_LE => na <= nb,
        Py_EQ => na == nb,
        Py_NE => na != nb,
        Py_GT => na > nb,
        _ => na >= nb,
    };
    Ok(crate::types::bool_::new_bool(r))
}

unsafe extern "C" fn array_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        if !is_array(&*b) {
            return none::not_implemented().to_ref().into_raw();
        }
        into_c(compare(&*a, &*b, op))
    }
}

unsafe extern "C" fn array_length(o: *mut PyObject) -> isize {
    unsafe { nitems(&*o) as isize }
}

unsafe extern "C" fn array_item(o: *mut PyObject, i: isize) -> *mut PyObject {
    unsafe {
        let o = &*o;
        let n = nitems(o) as isize;
        if i < 0 || i >= n {
            return into_c(Err(err!(IndexError, "array index out of range")));
        }
        into_c(get_item(o, i as usize))
    }
}

unsafe extern "C" fn array_contains(o: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let o = &*o;
        let r = (|| -> PyResult<bool> {
            let mut i = 0;
            while i < nitems(o) {
                if items_equal(o, i, &*v)? {
                    return Ok(true);
                }
                i += 1;
            }
            Ok(false)
        })();
        match r {
            Ok(b) => b as c_int,
            Err(_) => -1,
        }
    }
}

fn concat_err(b: &PyObject) -> PyErr {
    err!(TypeError, "can only append array (not \"{}\") to array", tname(b))
}

unsafe extern "C" fn array_concat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        let r = (|| -> PyResult {
            if !is_array(b) {
                return Err(concat_err(b));
            }
            if code_of(a).code != code_of(b).code {
                raise!(TypeError, "bad argument type for built-in operation");
            }
            let mut v = data(a).clone();
            v.extend_from_slice(data(b));
            new_array(array_type(), code_of(a), v)
        })();
        into_c(r)
    }
}

fn repeat_bytes(src: &[u8], n: isize) -> PyResult<Vec<u8>> {
    if n <= 0 || src.is_empty() {
        return Ok(Vec::new());
    }
    let total = src.len().checked_mul(n as usize).filter(|t| *t <= isize::MAX as usize);
    let total = match total {
        Some(t) => t,
        None => return Err(err!(MemoryError, "")),
    };
    let mut v = Vec::new();
    if v.try_reserve_exact(total).is_err() {
        return Err(err!(MemoryError, ""));
    }
    for _ in 0..n {
        v.extend_from_slice(src);
    }
    Ok(v)
}

unsafe extern "C" fn array_repeat(a: *mut PyObject, n: isize) -> *mut PyObject {
    unsafe {
        let a = &*a;
        into_c(repeat_bytes(data(a), n).and_then(|v| new_array(array_type(), code_of(a), v)))
    }
}

unsafe extern "C" fn array_inplace_concat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (ao, bo) = (&*a, &*b);
        let r = (|| -> PyResult {
            if !is_array(bo) {
                return Err(concat_err(bo));
            }
            if code_of(ao).code != code_of(bo).code {
                raise!(TypeError, "can only extend with array of same kind");
            }
            let v = data(bo).clone();
            extend_bytes(ao, &v)?;
            Ok(ao.to_ref())
        })();
        into_c(r)
    }
}

unsafe extern "C" fn array_inplace_repeat(a: *mut PyObject, n: isize) -> *mut PyObject {
    unsafe {
        let ao = &*a;
        let r = (|| -> PyResult {
            if data(ao).is_empty() {
                return Ok(ao.to_ref());
            }
            check_resizable(ao)?;
            let v = repeat_bytes(data(ao), n)?;
            *data(ao) = v;
            sync_size(ao);
            Ok(ao.to_ref())
        })();
        into_c(r)
    }
}

fn subscript(o: &PyObject, k: &PyObject) -> PyResult {
    if slice::is_slice(k) {
        let (mut start, mut stop, step) = slice::unpack(k)?;
        let n = nitems(o) as isize;
        let len = slice::adjust_indices(n, &mut start, &mut stop, step);
        let sz = code_of(o).size;
        let src = data(o);
        let mut v = Vec::with_capacity(len as usize * sz);
        let mut i = start;
        for _ in 0..len {
            let s = i as usize * sz;
            v.extend_from_slice(&src[s..s + sz]);
            i = i.wrapping_add(step);
        }
        return new_array(array_type(), code_of(o), v);
    }
    if !ab::has_index(k) {
        raise!(TypeError, "array indices must be integers");
    }
    let i = ab::index_as_ssize(k, Some(crate::types::exceptions::IndexError()))?;
    let n = nitems(o) as isize;
    let j = if i < 0 { i + n } else { i };
    if j < 0 || j >= n {
        raise!(IndexError, "array index out of range");
    }
    get_item(o, j as usize)
}

unsafe extern "C" fn array_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(subscript(&*o, &*k)) }
}

fn ass_subscript(o: &PyObject, k: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let sz = code_of(o).size;
    if !slice::is_slice(k) {
        if !ab::has_index(k) {
            raise!(TypeError, "array indices must be integers");
        }
        let i = ab::index_as_ssize(k, Some(crate::types::exceptions::IndexError()))?;
        let n = nitems(o) as isize;
        let j = if i < 0 { i + n } else { i };
        if j < 0 || j >= n {
            raise!(IndexError, "array assignment index out of range");
        }
        let j = j as usize;
        match v {
            Some(v) => {
                let b = encode_value(o, v)?;
                // converting the value may have run code that shrank the array
                if j >= nitems(o) {
                    raise!(IndexError, "array assignment index out of range");
                }
                data(o)[j * sz..(j + 1) * sz].copy_from_slice(&b);
            }
            None => {
                check_resizable(o)?;
                data(o).drain(j * sz..(j + 1) * sz);
                sync_size(o);
            }
        }
        return Ok(());
    }
    let (mut start, mut stop, step) = slice::unpack(k)?;
    let n = nitems(o) as isize;
    let len = slice::adjust_indices(n, &mut start, &mut stop, step) as usize;
    let newdata: Option<Vec<u8>> = match v {
        None => None,
        Some(v) => {
            if !is_array(v) {
                raise!(TypeError, "can only assign array (not \"{}\") to array slice", tname(v));
            }
            if code_of(v).code != code_of(o).code {
                raise!(TypeError, "bad argument type for built-in operation");
            }
            Some(data(v).clone())
        }
    };
    if step == 1 {
        let s = start.max(0) as usize;
        let e = (s + len).max(s);
        let repl = newdata.unwrap_or_default();
        if repl.len() != len * sz {
            check_resizable(o)?;
        }
        data(o).splice(s * sz..e * sz, repl);
        sync_size(o);
        return Ok(());
    }
    // extended slice
    let idx: Vec<usize> = (0..len).map(|t| (start + t as isize * step) as usize).collect();
    match newdata {
        Some(repl) => {
            let rn = repl.len() / sz;
            if rn != len {
                raise!(ValueError, "attempt to assign array of size {} to extended slice of size {}", rn, len);
            }
            let d = data(o);
            for (t, &i) in idx.iter().enumerate() {
                d[i * sz..(i + 1) * sz].copy_from_slice(&repl[t * sz..(t + 1) * sz]);
            }
        }
        None => {
            if len == 0 {
                return Ok(());
            }
            check_resizable(o)?;
            let mut kill = vec![false; n as usize];
            for &i in idx.iter() {
                kill[i] = true;
            }
            let old = std::mem::take(data(o));
            let mut keep = Vec::with_capacity(old.len());
            for (i, chunk) in old.chunks(sz).enumerate() {
                if !kill[i] {
                    keep.extend_from_slice(chunk);
                }
            }
            *data(o) = keep;
            sync_size(o);
        }
    }
    Ok(())
}

unsafe extern "C" fn array_ass_subscript(o: *mut PyObject, k: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let v = if v.is_null() { None } else { Some(&*v) };
        int_into_c(ass_subscript(&*o, &*k, v))
    }
}

unsafe extern "C" fn array_ass_item(o: *mut PyObject, i: isize, v: *mut PyObject) -> c_int {
    unsafe {
        let k = int::new_int(i as i64);
        let v = if v.is_null() { None } else { Some(&*v) };
        int_into_c(ass_subscript(&*o, &k, v))
    }
}

fn repr(o: &PyObject) -> PyResult {
    let c = code_of(o);
    if nitems(o) == 0 {
        return Ok(pystr::new_str(&format!("array('{}')", c.code)));
    }
    let inner = if is_char_kind(c.kind) { to_unicode(o)? } else { m_tolist(o)? };
    let r = ab::repr_string(&inner)?;
    Ok(pystr::new_str(&format!("array('{}', {})", c.code, r)))
}

unsafe extern "C" fn array_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(repr(&*o)) }
}

unsafe extern "C" fn array_getbuffer(o: *mut PyObject, view: *mut Py_buffer, flags: c_int) -> c_int {
    unsafe {
        let a = &*o;
        if view.is_null() {
            return int_into_c(Err(err!(BufferError, "array_buffer_getbuf: view==NULL argument is obsolete")));
        }
        let c = code_of(a);
        let d = data(a);
        let v = &mut *view;
        v.buf = if d.is_empty() { EMPTY.as_ptr() as *mut c_void } else { d.as_mut_ptr() as *mut c_void };
        v.obj = a.to_ref().into_raw();
        v.len = d.len() as isize;
        v.readonly = 0;
        v.ndim = 1;
        v.itemsize = c.size as isize;
        v.suboffsets = std::ptr::null_mut();
        v.shape = std::ptr::null_mut();
        if flags & crate::buffer::PyBUF_ND == crate::buffer::PyBUF_ND {
            v.shape = &mut arr(a).ob_base.ob_size;
        }
        v.strides = std::ptr::null_mut();
        if flags & crate::buffer::PyBUF_STRIDES == crate::buffer::PyBUF_STRIDES {
            v.strides = &mut v.itemsize;
        }
        v.format = std::ptr::null_mut();
        v.internal = std::ptr::null_mut();
        if flags & crate::buffer::PyBUF_FORMAT == crate::buffer::PyBUF_FORMAT {
            v.format = c.format.as_ptr() as *mut _;
        }
        arr(a).exports += 1;
        0
    }
}

static EMPTY: [u8; 8] = [0; 8];

unsafe extern "C" fn array_releasebuffer(o: *mut PyObject, _view: *mut Py_buffer) {
    unsafe {
        arr(&*o).exports -= 1;
    }
}

// ===================================================================== iterator

#[repr(C)]
struct IterObj {
    ob_base: PyObject,
    index: isize,
    ao: *mut PyObject,
}

unsafe extern "C" fn array_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let op = generic_alloc(iter_type(), 0);
        if op.is_null() {
            return op;
        }
        let it = &mut *(op as *mut IterObj);
        it.index = 0;
        it.ao = (*o).to_ref().into_raw();
        op
    }
}

unsafe extern "C" fn iter_dealloc(o: *mut PyObject) {
    unsafe {
        let tp = (*o).type_ptr();
        let it = &mut *(o as *mut IterObj);
        if !it.ao.is_null() {
            crate::object::decref(it.ao);
            it.ao = std::ptr::null_mut();
        }
        generic_free_and_type_decref(o, tp);
    }
}

// ---------------------------------------------------------------- thread safety
//
// The storage is a Rust Vec that size-changing operations reallocate, so every slot
// and method runs with the array's object lock (objlock.rs; methods through
// METH_LOCKSELF). Operations on two arrays lock both, in address order.

fn lock2(a: &PyObject, b: &PyObject) -> (crate::objlock::ObjGuard, Option<crate::objlock::ObjGuard>) {
    let other_array = is_array(b) && !a.is(b);
    if !other_array {
        return (crate::objlock::lock(a), None);
    }
    if (a.as_ptr() as usize) < (b.as_ptr() as usize) {
        let g1 = crate::objlock::lock(a);
        (g1, Some(crate::objlock::lock(b)))
    } else {
        let g1 = crate::objlock::lock(b);
        (g1, Some(crate::objlock::lock(a)))
    }
}

macro_rules! locked1 {
    ($name:ident, $f:ident, ($($arg:ident: $t:ty),*) -> $ret:ty) => {
        unsafe extern "C" fn $name(o: *mut PyObject $(, $arg: $t)*) -> $ret {
            unsafe {
                let _g = crate::objlock::lock(&*o);
                $f(o $(, $arg)*)
            }
        }
    };
}
macro_rules! locked2 {
    ($name:ident, $f:ident, ($($arg:ident: $t:ty),*) -> $ret:ty) => {
        unsafe extern "C" fn $name(a: *mut PyObject, b: *mut PyObject $(, $arg: $t)*) -> $ret {
            unsafe {
                let _g = if b.is_null() { (crate::objlock::lock(&*a), None) } else { lock2(&*a, &*b) };
                $f(a, b $(, $arg)*)
            }
        }
    };
}
locked1!(l_length, array_length, () -> isize);
locked1!(l_item, array_item, (i: isize) -> *mut PyObject);
locked1!(l_ass_item, array_ass_item, (i: isize, v: *mut PyObject) -> c_int);
locked1!(l_contains, array_contains, (v: *mut PyObject) -> c_int);
locked1!(l_repeat, array_repeat, (n: isize) -> *mut PyObject);
locked1!(l_inplace_repeat, array_inplace_repeat, (n: isize) -> *mut PyObject);
locked1!(l_subscript, array_subscript, (k: *mut PyObject) -> *mut PyObject);
locked1!(l_repr, array_repr, () -> *mut PyObject);
locked1!(l_getbuffer, array_getbuffer, (view: *mut Py_buffer, flags: c_int) -> c_int);
locked1!(l_releasebuffer, array_releasebuffer, (view: *mut Py_buffer) -> ());
locked2!(l_concat, array_concat, () -> *mut PyObject);
locked2!(l_inplace_concat, array_inplace_concat, () -> *mut PyObject);
locked2!(l_richcompare, array_richcompare, (op: c_int) -> *mut PyObject);

unsafe extern "C" fn l_ass_subscript(o: *mut PyObject, k: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let _g = if v.is_null() { (crate::objlock::lock(&*o), None) } else { lock2(&*o, &*v) };
        array_ass_subscript(o, k, v)
    }
}

unsafe extern "C" fn iter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let _li = crate::objlock::lock(&*o);
        let it = &mut *(o as *mut IterObj);
        if it.ao.is_null() {
            return std::ptr::null_mut();
        }
        let a = &*it.ao;
        let item = {
            let _la = crate::objlock::lock(a);
            if (it.index as usize) < nitems(a) { Some(get_item(a, it.index as usize)) } else { None }
        };
        if let Some(r) = item {
            it.index += 1;
            return into_c(r);
        }
        // Clear the reference before dropping it: the array's finalizer may
        // call next() on this iterator again.
        let ao = std::mem::replace(&mut it.ao, std::ptr::null_mut());
        crate::object::decref(ao);
        std::ptr::null_mut()
    }
}

fn iter_reduce(o: &PyObject) -> PyResult {
    let it = unsafe { &*(o.as_ptr() as *const IterObj) };
    let iterf = crate::builtins::builtin("iter")?;
    if it.ao.is_null() {
        return Ok(tuple::tuple2(iterf, tuple::new_tuple(vec![tuple::empty_tuple()])));
    }
    let a = unsafe { (*it.ao).to_ref() };
    Ok(tuple::new_tuple(vec![iterf, tuple::new_tuple(vec![a]), int::new_int(it.index as i64)]))
}

fn iter_setstate(o: &PyObject, st: &PyObject) -> PyResult {
    let it = unsafe { &mut *(o.as_ptr() as *mut IterObj) };
    let mut i = ab::index_as_ssize(st, None)?;
    if i < 0 {
        i = 0;
    }
    if !it.ao.is_null() {
        let n = nitems(unsafe { &*it.ao }) as isize;
        if i > n {
            i = n;
        }
    }
    it.index = i;
    Ok(none::none_ref())
}

// ===================================================================== reconstructor

struct MFormat {
    size: usize,
    kind: char, // 'i' signed, 'u' unsigned, 'f' float, 'c' complex, 's' utf16, 'S' utf32
    big: bool,
}

fn mformat(code: i64) -> MFormat {
    let big = code % 2 == 1 && code >= 2;
    match code {
        0 => MFormat { size: 1, kind: 'u', big: false },
        1 => MFormat { size: 1, kind: 'i', big: false },
        2 | 3 => MFormat { size: 2, kind: 'u', big },
        4 | 5 => MFormat { size: 2, kind: 'i', big },
        6 | 7 => MFormat { size: 4, kind: 'u', big },
        8 | 9 => MFormat { size: 4, kind: 'i', big },
        10 | 11 => MFormat { size: 8, kind: 'u', big },
        12 | 13 => MFormat { size: 8, kind: 'i', big },
        14 | 15 => MFormat { size: 4, kind: 'f', big },
        16 | 17 => MFormat { size: 8, kind: 'f', big },
        18 | 19 => MFormat { size: 2, kind: 's', big },
        20 | 21 => MFormat { size: 4, kind: 'S', big },
        22 | 23 => MFormat { size: 8, kind: 'c', big },
        24 | 25 => MFormat { size: 16, kind: 'c', big },
        _ => MFormat { size: 2, kind: 'h', big },
    }
}

fn ordered(chunk: &[u8], big: bool) -> Vec<u8> {
    let mut v = chunk.to_vec();
    if big {
        v.reverse();
    }
    v
}

fn decode_float(chunk: &[u8], big: bool) -> f64 {
    let v = ordered(chunk, big);
    match v.len() {
        2 => crate::types::memoryview::unpack_half(u16::from_le_bytes([v[0], v[1]])),
        4 => f32::from_le_bytes(v[..4].try_into().unwrap()) as f64,
        _ => f64::from_le_bytes(v[..8].try_into().unwrap()),
    }
}

fn m_reconstructor(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("_array_reconstructor", args, 4, 4)?;
    let (tp, tc, mf, items) = (args[0], args[1], args[2], args[3]);
    if !is_type(tp) {
        raise!(TypeError, "first argument must be a type object, not {}", tname(tp));
    }
    if !is_subtype(as_type(tp), array_type()) {
        raise!(TypeError, "{} is not a subtype of array.array", type_name(unsafe { &*as_type(tp) }));
    }
    if !pystr::is_str(tc) {
        raise!(TypeError, "_array_reconstructor() argument 2 must be a unicode character, not {}", tname(tc));
    }
    if !int::is_int(mf) {
        raise!(TypeError, "'{}' object cannot be interpreted as an integer", tname(mf));
    }
    if !bytes::is_bytes(items) {
        raise!(TypeError, "fourth argument should be bytes, not {}", tname(items));
    }
    let code = match find_code(&pystr::to_string_lossy(tc)) {
        Some(c) => c,
        None => raise!(ValueError, "second argument must be a valid type code"),
    };
    let mfc = match int::as_i64(mf) {
        Some(v) if (0..=MFORMAT_MAX).contains(&v) => v,
        _ => raise!(ValueError, "third argument must be a valid machine format code."),
    };
    let raw = bytes::bytes_data(items);
    if mfc == code.mformat {
        if raw.len() % code.size != 0 {
            raise!(ValueError, "bytes length not a multiple of item size");
        }
        return new_array(as_type(tp), code, raw.to_vec());
    }
    let f = mformat(mfc);
    if raw.len() % f.size != 0 {
        raise!(ValueError, "string length not a multiple of item size");
    }
    let init: PyRef = match f.kind {
        's' | 'S' => {
            let enc = match (f.kind, f.big) {
                ('s', false) => "utf-16-le",
                ('s', true) => "utf-16-be",
                ('S', false) => "utf-32-le",
                _ => "utf-32-be",
            };
            ab::call_method(items, name!("decode"), &[&pystr::new_str(enc)])?
        }
        _ => {
            let mut vals = Vec::with_capacity(raw.len() / f.size);
            for chunk in raw.chunks(f.size) {
                let v = match f.kind {
                    'u' | 'i' => {
                        let o = ordered(chunk, f.big);
                        let mut a = [0u8; 16];
                        a[..f.size].copy_from_slice(&o);
                        let mut x = u128::from_le_bytes(a) as i128;
                        if f.kind == 'i' && o[f.size - 1] & 0x80 != 0 {
                            x -= 1i128 << (8 * f.size);
                        }
                        int::new_int_i128(x)
                    }
                    'f' | 'h' => float::new_float(decode_float(chunk, f.big)),
                    _ => {
                        let h = f.size / 2;
                        crate::types::complex::new_complex(decode_float(&chunk[..h], f.big), decode_float(&chunk[h..], f.big))
                    }
                };
                vals.push(v);
            }
            list::new_list(vals)
        }
    };
    ab::call(tp, &[tc, &init])
}

// ===================================================================== module

static INIT: std::sync::Once = std::sync::Once::new();

const ARRAY_DOC: &str = r#"array(typecode[, initializer])

A mutable sequence whose items all have one machine type, selected by
typecode.  The optional initializer may be a list, a bytes-like object,
a str (for character type codes) or any iterable of suitable items.

Type codes, with the C type used and the smallest item size in bytes:

    'b'    signed char           int      1
    'B'    unsigned char         int      1
    'u'    wchar_t               str      2 (see note)
    'w'    Py_UCS4               str      4
    'h'    signed short          int      2
    'H'    unsigned short        int      2
    'i'    signed int            int      2
    'I'    unsigned int          int      2
    'l'    signed long           int      4
    'L'    unsigned long         int      4
    'q'    signed long long      int      8
    'Q'    unsigned long long    int      8
    'e'    16-bit float          float    2
    'f'    float                 float    4
    'd'    double                float    8
    'Zf'   float complex         complex  8
    'Zd'   double complex        complex  16

Note: the size of 'u' depends on the platform's wchar_t; the type code
is deprecated in favour of 'w'.

Besides the list-like operations, arrays convert to and from bytes
(frombytes/tobytes), lists (fromlist/tolist), str (fromunicode/
tounicode) and files (fromfile/tofile); typecode and itemsize describe
the item type.
"#;

pub fn init_module() -> PyResult {
    let mut res = Ok(());
    INIT.call_once(|| unsafe {
        res = (|| -> PyResult<()> {
            let t = &mut *array_type();
            t.tp_name = c"array.array".as_ptr();
            t.tp_basicsize = std::mem::size_of::<ArrayObj>() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_IMMUTABLETYPE | Py_TPFLAGS_SEQUENCE;
            t.tp_weaklistoffset = std::mem::offset_of!(ArrayObj, weakreflist) as isize;
            t.tp_new = Some(array_new);
            t.tp_dealloc = Some(array_dealloc);
            t.tp_alloc = Some(generic_alloc);
            t.tp_free = Some(generic_free);
            t.tp_repr = Some(l_repr);
            t.tp_richcompare = Some(l_richcompare);
            t.tp_iter = Some(array_iter);
            t.tp_hash = Some(crate::types::slots::hash_not_implemented);
            t.tp_getattro = Some(object_generic_getattro);
            let doc = std::ffi::CString::new(ARRAY_DOC).unwrap();
            t.tp_doc = Box::leak(doc.into_boxed_c_str()).as_ptr();
            let sq = Box::leak(Box::new(PySequenceMethods {
                sq_length: Some(l_length),
                sq_concat: Some(l_concat),
                sq_repeat: Some(l_repeat),
                sq_item: Some(l_item),
                was_sq_slice: std::ptr::null_mut(),
                sq_ass_item: Some(l_ass_item),
                was_sq_ass_slice: std::ptr::null_mut(),
                sq_contains: Some(l_contains),
                sq_inplace_concat: Some(l_inplace_concat),
                sq_inplace_repeat: Some(l_inplace_repeat),
            }));
            t.tp_as_sequence = sq;
            let mp = Box::leak(Box::new(PyMappingMethods {
                mp_length: Some(l_length),
                mp_subscript: Some(l_subscript),
                mp_ass_subscript: Some(l_ass_subscript),
            }));
            t.tp_as_mapping = mp;
            let bp = Box::leak(Box::new(PyBufferProcs { bf_getbuffer: Some(l_getbuffer), bf_releasebuffer: Some(l_releasebuffer) }));
            t.tp_as_buffer = bp;
            t.tp_methods = mdefs![
                ("append", O, m_append, "append($self, v, /)\n--\n\nAdd v as the last item."),
                ("buffer_info", NOARGS, m_buffer_info, "buffer_info($self, /)\n--\n\nReturn (address, number of items) of the storage; the address is only valid until the array is resized."),
                ("byteswap", NOARGS, m_byteswap, "byteswap($self, /)\n--\n\nReverse the byte order of every item in place."),
                ("clear", NOARGS, m_clear, "clear($self, /)\n--\n\nMake the array empty."),
                ("__copy__", NOARGS, m_copy, "__copy__($self, /)\n--\n\nShallow copy of the array."),
                ("count", O, m_count, "count($self, v, /)\n--\n\nHow many items equal v."),
                ("__deepcopy__", O, m_deepcopy, "__deepcopy__($self, unused, /)\n--\n\nCopy of the array (items are plain values, so this equals a shallow copy)."),
                ("extend", O, m_extend, "extend($self, bb, /)\n--\n\nAdd every item of the iterable (or array of the same type code) bb."),
                ("frombytes", O, m_frombytes, "frombytes($self, buffer, /)\n--\n\nAdd items decoded from the raw machine representation in buffer."),
                ("fromfile", FAST, m_fromfile, "fromfile($self, f, n, /)\n--\n\nRead n items in machine representation from file f and add them; EOFError if fewer are available (those are still added)."),
                ("fromlist", O, m_fromlist, "fromlist($self, list, /)\n--\n\nAdd the items of a list; nothing is added if one of them has the wrong type."),
                ("fromunicode", O, m_fromunicode, "fromunicode($self, ustr, /)\n--\n\nAdd the characters of str ustr (type codes u and w only)."),
                ("index", FAST, m_index, "index($self, v, start=0, stop=sys.maxsize, /)\n--\n\nPosition of the first item equal to v within [start, stop); ValueError if absent."),
                ("insert", FAST, m_insert, "insert($self, i, v, /)\n--\n\nPut v before position i."),
                ("pop", FAST, m_pop, "pop($self, i=-1, /)\n--\n\nRemove and return the item at position i (the last one by default)."),
                ("__reduce_ex__", O, m_reduce_ex, "__reduce_ex__($self, value, /)\n--\n\nPickling support."),
                ("remove", O, m_remove, "remove($self, v, /)\n--\n\nDelete the first item equal to v; ValueError if absent."),
                ("reverse", NOARGS, m_reverse, "reverse($self, /)\n--\n\nReverse the item order in place."),
                ("tofile", O, m_tofile, "tofile($self, f, /)\n--\n\nWrite the items in machine representation to file f."),
                ("tolist", NOARGS, m_tolist, "tolist($self, /)\n--\n\nThe items as a list."),
                ("tobytes", NOARGS, m_tobytes, "tobytes($self, /)\n--\n\nThe items in machine representation, as bytes."),
                ("tounicode", NOARGS, m_tounicode, "tounicode($self, /)\n--\n\nThe characters as a str (type codes u and w only)."),
                ("__sizeof__", NOARGS, m_sizeof, "__sizeof__($self, /)\n--\n\nMemory used by the array object, in bytes."),
                ("__class_getitem__", O, m_class_getitem, "See PEP 585"; METH_CLASS),
            ];
            t.tp_getset = crate::getset_table![
                ("typecode", get_typecode; "the typecode character used to create the array"),
                ("itemsize", get_itemsize; "the size, in bytes, of one array item"),
            ];
            crate::methods::lock_self_methods(array_type());
            type_ready(array_type())?;

            let t = &mut *iter_type();
            t.tp_name = c"array.arrayiterator".as_ptr();
            t.tp_basicsize = std::mem::size_of::<IterObj>() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_IMMUTABLETYPE;
            t.tp_dealloc = Some(iter_dealloc);
            t.tp_iter = Some(iter_self);
            t.tp_iternext = Some(iter_next);
            t.tp_new = Some(iter_new);
            t.tp_getattro = Some(object_generic_getattro);
            t.tp_methods = mdefs![
                ("__reduce__", NOARGS, iter_reduce, "Pickling support."),
                ("__setstate__", O, iter_setstate, "Restore the position when unpickling."),
            ];
            type_ready(iter_type())?;
            Ok(())
        })();
    });
    res?;
    let m = crate::types::module::new_module("array")?;
    let d = crate::types::module::module_dict(&m);
    dict::set_item_str(&d, "__doc__", &pystr::new_str("Typed arrays of numbers or characters with compact storage.\n"))?;
    dict::set_item_str(&d, "array", &type_ref(array_type()))?;
    dict::set_item_str(&d, "ArrayType", &type_ref(array_type()))?;
    let codes: Vec<PyRef> = CODES.iter().map(|c| pystr::new_str(c.code)).collect();
    dict::set_item_str(&d, "typecodes", &tuple::new_tuple(codes))?;
    crate::builtins::add_methods_to_dict(
        &d,
        mdefs![("_array_reconstructor", FAST, m_reconstructor, "_array_reconstructor($module, arraytype, typecode, mformat_code, items, /)\n--\n\nRebuild an array from pickled data (internal).")],
        Some(&m),
    )?;
    // Register with the abstract base classes (fails loudly if they are gone).
    let abc = crate::import::import_absolute("collections.abc")?;
    let ms = ab::getattr_str(&abc, "MutableSequence")?;
    ab::call_method(&ms, name!("register"), &[&type_ref(array_type())])?;
    Ok(m)
}

unsafe extern "C" fn iter_self(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}

unsafe extern "C" fn iter_new(_t: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    into_c(Err(err!(TypeError, "cannot create 'array.arrayiterator' instances")))
}
