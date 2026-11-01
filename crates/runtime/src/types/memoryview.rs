//! memoryview, the managed buffer object, and the PEP 688 bridge between
//! Python-level `__buffer__`/`__release_buffer__` and the C buffer protocol.
use crate::abstract_ as ab;
use crate::buffer::{self, *};
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::tname;
use crate::types::{bool_, bytes, int, none, slice, str as pystr, tuple};
use std::ffi::{CStr, c_char, c_int, c_void};

pub const MBUF_RELEASED: c_int = 0x001;
pub const MBUF_FREE_FORMAT: c_int = 0x002;
pub const MV_RELEASED: c_int = 0x001;
pub const MV_C: c_int = 0x002;
pub const MV_FORTRAN: c_int = 0x004;
pub const MV_SCALAR: c_int = 0x008;
pub const MV_PIL: c_int = 0x010;
pub const MV_RESTRICTED: c_int = 0x020;

#[repr(C)]
pub struct ManagedBuffer {
    pub ob_base: PyObject,
    pub flags: c_int,
    pub exports: isize,
    pub master: Py_buffer,
}

#[repr(C)]
pub struct PyMemoryViewObject {
    pub ob_base: PyVarObject,
    pub mbuf: *mut ManagedBuffer,
    pub hash: Py_hash_t,
    pub flags: c_int,
    pub exports: isize,
    pub view: Py_buffer,
    pub weakreflist: *mut PyObject,
    pub ob_array: [isize; 1],
}

#[repr(C)]
pub struct BufferWrapper {
    pub ob_base: PyObject,
    pub mv: *mut PyObject,
    pub obj: *mut PyObject,
}

#[repr(C)]
pub struct MemoryIter {
    pub ob_base: PyObject,
    pub index: isize,
    pub length: isize,
    pub fmt: NFmt,
    pub seq: *mut PyObject,
}

#[unsafe(no_mangle)]
pub static mut PyMemoryView_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut _PyManagedBuffer_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut _PyBufferWrapper_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut _PyMemoryIter_Type: PyTypeObject = PyTypeObject::ZERO;

pub fn memoryview_type() -> *mut PyTypeObject {
    &raw mut PyMemoryView_Type
}
pub fn is_memoryview(o: &PyObject) -> bool {
    o.is_type(memoryview_type())
}

#[inline]
fn mv(o: &PyObject) -> &mut PyMemoryViewObject {
    unsafe { &mut *(o.as_ptr() as *mut PyMemoryViewObject) }
}
#[inline]
fn mbuf_of(o: &PyObject) -> &mut ManagedBuffer {
    unsafe { &mut *mv(o).mbuf }
}

fn base_inaccessible(o: &PyObject) -> bool {
    let m = mv(o);
    m.flags & MV_RELEASED != 0 || m.mbuf.is_null() || unsafe { (*m.mbuf).flags & MBUF_RELEASED != 0 }
}
fn check_released(o: &PyObject) -> PyResult<()> {
    if base_inaccessible(o) {
        raise!(ValueError, "operation forbidden on released memoryview object");
    }
    Ok(())
}
fn check_restricted(o: &PyObject) -> PyResult<()> {
    if mv(o).flags & MV_RESTRICTED != 0 {
        raise!(ValueError, "cannot create new view on restricted memoryview");
    }
    Ok(())
}

// ====================================================================== formats

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum NFmt {
    Bool,
    Char,
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    Long,
    ULong,
    LongLong,
    ULongLong,
    SSize,
    Size,
    Half,
    Float,
    Double,
    Ptr,
    CFloat,
    CDouble,
}

impl NFmt {
    pub fn size(self) -> usize {
        use NFmt::*;
        match self {
            Bool | Char | I8 | U8 => 1,
            I16 | U16 | Half => 2,
            I32 | U32 | Float => 4,
            Long | ULong | LongLong | ULongLong | SSize | Size | Double | Ptr | CFloat => 8,
            CDouble => 16,
        }
    }
    pub fn is_byte(self) -> bool {
        matches!(self, NFmt::Char | NFmt::I8 | NFmt::U8)
    }
    fn is_floatish(self) -> bool {
        matches!(self, NFmt::Half | NFmt::Float | NFmt::Double | NFmt::CFloat | NFmt::CDouble)
    }
    /// Static C string for the format (with optional '@' prefix).
    fn cstr(self, at: bool) -> *mut c_char {
        use NFmt::*;
        let s: &'static CStr = match (self, at) {
            (Bool, false) => c"?",
            (Bool, true) => c"@?",
            (Char, false) => c"c",
            (Char, true) => c"@c",
            (I8, false) => c"b",
            (I8, true) => c"@b",
            (U8, false) => c"B",
            (U8, true) => c"@B",
            (I16, false) => c"h",
            (I16, true) => c"@h",
            (U16, false) => c"H",
            (U16, true) => c"@H",
            (I32, false) => c"i",
            (I32, true) => c"@i",
            (U32, false) => c"I",
            (U32, true) => c"@I",
            (Long, false) => c"l",
            (Long, true) => c"@l",
            (ULong, false) => c"L",
            (ULong, true) => c"@L",
            (LongLong, false) => c"q",
            (LongLong, true) => c"@q",
            (ULongLong, false) => c"Q",
            (ULongLong, true) => c"@Q",
            (SSize, false) => c"n",
            (SSize, true) => c"@n",
            (Size, false) => c"N",
            (Size, true) => c"@N",
            (Half, false) => c"e",
            (Half, true) => c"@e",
            (Float, false) => c"f",
            (Float, true) => c"@f",
            (Double, false) => c"d",
            (Double, true) => c"@d",
            (Ptr, false) => c"P",
            (Ptr, true) => c"@P",
            (CFloat, false) => c"Zf",
            (CFloat, true) => c"@Zf",
            (CDouble, false) => c"Zd",
            (CDouble, true) => c"@Zd",
        };
        s.as_ptr() as *mut c_char
    }
}

/// Parse a native single-item format ("B", "@i", "Zd", ...). Returns (fmt, had '@').
pub fn native_fmt(f: &[u8]) -> Option<(NFmt, bool)> {
    use NFmt::*;
    let (f, at) = if f.first() == Some(&b'@') { (&f[1..], true) } else { (f, false) };
    let r = match f {
        b"?" => Bool,
        b"c" => Char,
        b"b" => I8,
        b"B" => U8,
        b"h" => I16,
        b"H" => U16,
        b"i" => I32,
        b"I" => U32,
        b"l" => Long,
        b"L" => ULong,
        b"q" => LongLong,
        b"Q" => ULongLong,
        b"n" => SSize,
        b"N" => Size,
        b"e" => Half,
        b"f" => Float,
        b"d" => Double,
        b"P" => Ptr,
        b"Zf" => CFloat,
        b"Zd" => CDouble,
        _ => return None,
    };
    Some((r, at))
}

fn fmt_bytes(v: &Py_buffer) -> &[u8] {
    if v.format.is_null() {
        return b"B";
    }
    unsafe { CStr::from_ptr(v.format).to_bytes() }
}
fn fmt_string(v: &Py_buffer) -> String {
    String::from_utf8_lossy(fmt_bytes(v)).into_owned()
}

fn adjust_fmt(v: &Py_buffer) -> PyResult<NFmt> {
    match native_fmt(fmt_bytes(v)) {
        Some((f, _)) => Ok(f),
        None => raise!(NotImplementedError, "memoryview: unsupported format {}", fmt_string(v)),
    }
}

// ---------------------------------------------------------------------- half floats

pub fn pack_half(x: f64) -> Option<u16> {
    let sign: u16 = if x.is_sign_negative() { 0x8000 } else { 0 };
    if x.is_nan() {
        return Some(sign | 0x7e00);
    }
    if x.is_infinite() {
        return Some(sign | 0x7c00);
    }
    let ax = x.abs();
    if ax == 0.0 {
        return Some(sign);
    }
    let (mut f, mut e) = crate::hash::frexp(ax);
    // f in [0.5, 1) -> [1, 2)
    f *= 2.0;
    e -= 1;
    if e >= 16 {
        return None;
    }
    if e < -25 {
        f = 0.0;
        e = 0;
    } else if e < -14 {
        f = f * 2f64.powi(14 + e);
        e = 0;
    } else {
        e += 15;
        f -= 1.0;
    }
    f *= 1024.0;
    let mut bits = f as u32;
    let g = f - bits as f64;
    if g > 0.5 || (g == 0.5 && bits % 2 == 1) {
        bits += 1;
        if bits == 1024 {
            bits = 0;
            e += 1;
            if e == 31 {
                return None;
            }
        }
    }
    Some(sign | ((e as u16) << 10) | bits as u16)
}

pub fn unpack_half(h: u16) -> f64 {
    let neg = h & 0x8000 != 0;
    let e = ((h >> 10) & 0x1f) as i32;
    let f = (h & 0x3ff) as f64;
    let v = if e == 0x1f {
        if f == 0.0 { f64::INFINITY } else { f64::NAN }
    } else if e == 0 {
        f / 1024.0 * 2f64.powi(-14)
    } else {
        (1.0 + f / 1024.0) * 2f64.powi(e - 15)
    };
    if neg { -v } else { v }
}

// ---------------------------------------------------------------------- unpack / pack

/// Rust-side item value (used for comparisons).
#[derive(Clone, Copy, Debug)]
enum Val {
    I(i128),
    F(f64),
    C(f64, f64),
    Byte(u8),
}

unsafe fn read_val(p: *const u8, f: NFmt) -> Val {
    unsafe {
        use NFmt::*;
        macro_rules! rd {
            ($t:ty) => {
                std::ptr::read_unaligned(p as *const $t)
            };
        }
        match f {
            Bool => Val::I((*p != 0) as i128),
            Char => Val::Byte(*p),
            I8 => Val::I(rd!(i8) as i128),
            U8 => Val::I(*p as i128),
            I16 => Val::I(rd!(i16) as i128),
            U16 => Val::I(rd!(u16) as i128),
            I32 => Val::I(rd!(i32) as i128),
            U32 => Val::I(rd!(u32) as i128),
            Long | LongLong | SSize => Val::I(rd!(i64) as i128),
            ULong | ULongLong | Size | Ptr => Val::I(rd!(u64) as i128),
            Half => Val::F(unpack_half(rd!(u16))),
            Float => Val::F(rd!(f32) as f64),
            Double => Val::F(rd!(f64)),
            CFloat => Val::C(rd!(f32) as f64, std::ptr::read_unaligned((p as *const f32).add(1)) as f64),
            CDouble => Val::C(rd!(f64), std::ptr::read_unaligned((p as *const f64).add(1))),
        }
    }
}

unsafe fn unpack_single(p: *const u8, f: NFmt) -> PyResult {
    unsafe {
        Ok(match f {
            NFmt::Bool => bool_::new_bool(*p != 0),
            NFmt::Char => bytes::new_bytes(&[*p]),
            _ => match read_val(p, f) {
                Val::I(i) => int::new_int_i128(i),
                Val::F(x) => crate::types::float::new_float(x),
                Val::C(re, im) => crate::types::complex::new_complex(re, im),
                Val::Byte(b) => bytes::new_bytes(&[b]),
            },
        })
    }
}

fn type_error_int(fmt: &str) -> PyErr {
    err!(TypeError, "memoryview: invalid type for format '{}'", fmt)
}
fn value_error_int(fmt: &str) -> PyErr {
    err!(ValueError, "memoryview: invalid value for format '{}'", fmt)
}
fn fix_error_int(fmt: &str) -> PyErr {
    if exc_matches(crate::types::exceptions::TypeError()) {
        clear_exc();
        return type_error_int(fmt);
    }
    if exc_matches(crate::types::exceptions::OverflowError()) || exc_matches(crate::types::exceptions::ValueError()) {
        clear_exc();
        return value_error_int(fmt);
    }
    PyErr
}

fn as_float_for_pack(item: &PyObject) -> PyResult<f64> {
    ab::float_value(item)
}

/// Convert `item` to the bytes of one item of format `f` (into `out`).
fn pack_single(item: &PyObject, f: NFmt, fmtname: &str, out: &mut [u8; 16]) -> PyResult<()> {
    use NFmt::*;
    let int_of = |item: &PyObject| -> PyResult<i128> {
        let i = ab::index(item).map_err(|_| fix_error_int(fmtname))?;
        match int::as_i128(&i) {
            Some(v) => Ok(v),
            None => Err(value_error_int(fmtname)),
        }
    };
    match f {
        I8 | U8 | I16 | U16 | I32 | U32 | Long | ULong | LongLong | ULongLong | SSize | Size | Ptr => {
            let v = int_of(item)?;
            let (lo, hi): (i128, i128) = match f {
                I8 => (i8::MIN as i128, i8::MAX as i128),
                U8 => (0, u8::MAX as i128),
                I16 => (i16::MIN as i128, i16::MAX as i128),
                U16 => (0, u16::MAX as i128),
                I32 => (i32::MIN as i128, i32::MAX as i128),
                U32 => (0, u32::MAX as i128),
                Long | LongLong | SSize => (i64::MIN as i128, i64::MAX as i128),
                Ptr => (i64::MIN as i128, u64::MAX as i128),
                _ => (0, u64::MAX as i128),
            };
            if v < lo || v > hi {
                return Err(value_error_int(fmtname));
            }
            let b = (v as i64 as u64).to_ne_bytes();
            let b = if matches!(f, ULong | ULongLong | Size | Ptr) { (v as u64).to_ne_bytes() } else { b };
            let n = f.size();
            // little/big endian aware truncation via native conversions
            match n {
                1 => out[0] = v as u8,
                2 => out[..2].copy_from_slice(&(v as u16).to_ne_bytes()),
                4 => out[..4].copy_from_slice(&(v as u32).to_ne_bytes()),
                _ => out[..8].copy_from_slice(&b),
            }
        }
        Bool => {
            out[0] = ab::is_true(item)? as u8;
        }
        Char => {
            if !bytes::is_bytes(item) {
                return Err(type_error_int(fmtname));
            }
            let d = bytes::bytes_data(item);
            if d.len() != 1 {
                return Err(value_error_int(fmtname));
            }
            out[0] = d[0];
        }
        Half | Float | Double => {
            let x = as_float_for_pack(item).map_err(|_| fix_error_int(fmtname))?;
            match f {
                Half => match pack_half(x) {
                    Some(h) => out[..2].copy_from_slice(&h.to_ne_bytes()),
                    None => return Err(value_error_int(fmtname)),
                },
                Float => {
                    let y = x as f32;
                    if y.is_infinite() && !x.is_infinite() {
                        return Err(value_error_int(fmtname));
                    }
                    out[..4].copy_from_slice(&y.to_ne_bytes());
                }
                _ => out[..8].copy_from_slice(&x.to_ne_bytes()),
            }
        }
        CFloat | CDouble => {
            let (re, im) = if crate::types::complex::is_complex(item) {
                let c = crate::types::complex::complex_value(item);
                (c.real, c.imag)
            } else {
                (as_float_for_pack(item).map_err(|_| fix_error_int(fmtname))?, 0.0)
            };
            if f == CFloat {
                let (a, b) = (re as f32, im as f32);
                if (a.is_infinite() && !re.is_infinite()) || (b.is_infinite() && !im.is_infinite()) {
                    return Err(value_error_int(fmtname));
                }
                out[..4].copy_from_slice(&a.to_ne_bytes());
                out[4..8].copy_from_slice(&b.to_ne_bytes());
            } else {
                out[..8].copy_from_slice(&re.to_ne_bytes());
                out[8..16].copy_from_slice(&im.to_ne_bytes());
            }
        }
    }
    Ok(())
}

// ====================================================================== managed buffer

unsafe fn mbuf_alloc() -> *mut ManagedBuffer {
    unsafe {
        let op = crate::alloc::alloc_object(&raw mut _PyManagedBuffer_Type, std::mem::size_of::<ManagedBuffer>()) as *mut ManagedBuffer;
        (*op).flags = 0;
        (*op).exports = 0;
        (*op).master = empty_view();
        crate::gc::track(op as *mut PyObject);
        op
    }
}

unsafe fn mbuf_release(m: *mut ManagedBuffer) {
    unsafe {
        if (*m).flags & MBUF_RELEASED != 0 {
            return;
        }
        (*m).flags |= MBUF_RELEASED;
        crate::gc::untrack(m as *mut PyObject);
        release_buffer(&mut (*m).master);
    }
}

unsafe extern "C" fn mbuf_dealloc(o: *mut PyObject) {
    unsafe {
        let m = o as *mut ManagedBuffer;
        mbuf_release(m);
        if (*m).flags & MBUF_FREE_FORMAT != 0 && !(*m).master.format.is_null() {
            drop(std::ffi::CString::from_raw((*m).master.format));
            (*m).master.format = std::ptr::null_mut();
        }
        crate::alloc::free_object(o);
    }
}
unsafe extern "C" fn mbuf_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let m = o as *mut ManagedBuffer;
        let obj = (*m).master.obj;
        if obj.is_null() { 0 } else { visit.unwrap()(obj, arg) }
    }
}
unsafe extern "C" fn mbuf_clear(o: *mut PyObject) -> c_int {
    unsafe {
        mbuf_release(o as *mut ManagedBuffer);
        0
    }
}

/// _PyManagedBuffer_FromObject.
fn mbuf_from_object(base: &PyObject, flags: c_int) -> PyResult<PyRef> {
    unsafe {
        let m = mbuf_alloc();
        let r = PyRef::from_raw(m as *mut PyObject);
        get_buffer(base, &mut (*m).master, flags)?;
        Ok(r)
    }
}

// ====================================================================== memoryview construction

unsafe fn memory_alloc(ndim: usize) -> *mut PyMemoryViewObject {
    unsafe {
        let n = 3 * ndim.max(1);
        let size = std::mem::offset_of!(PyMemoryViewObject, ob_array) + n * std::mem::size_of::<isize>();
        let op = crate::alloc::alloc_object(memoryview_type(), size) as *mut PyMemoryViewObject;
        let m = &mut *op;
        m.ob_base.ob_size = (3 * ndim) as isize;
        m.mbuf = std::ptr::null_mut();
        m.hash = -1;
        m.flags = 0;
        m.exports = 0;
        m.view = empty_view();
        m.view.ndim = ndim as c_int;
        let arr = (&raw mut m.ob_array) as *mut isize;
        m.view.shape = arr;
        m.view.strides = arr.add(ndim);
        m.view.suboffsets = arr.add(2 * ndim);
        m.weakreflist = std::ptr::null_mut();
        op
    }
}

unsafe fn init_shared_values(dest: &mut Py_buffer, src: &Py_buffer) {
    dest.obj = src.obj;
    dest.buf = src.buf;
    dest.len = src.len;
    dest.itemsize = src.itemsize;
    dest.readonly = src.readonly;
    dest.format = if src.format.is_null() { c"B".as_ptr() as *mut c_char } else { src.format };
    dest.internal = src.internal;
}

unsafe fn init_strides_from_shape(v: &mut Py_buffer) {
    unsafe {
        let ndim = v.ndim as usize;
        if ndim == 0 {
            return;
        }
        *v.strides.add(ndim - 1) = v.itemsize;
        for i in (0..ndim - 1).rev() {
            *v.strides.add(i) = *v.strides.add(i + 1) * *v.shape.add(i + 1);
        }
    }
}
unsafe fn init_fortran_strides_from_shape(v: &mut Py_buffer) {
    unsafe {
        let ndim = v.ndim as usize;
        if ndim == 0 {
            return;
        }
        *v.strides = v.itemsize;
        for i in 1..ndim {
            *v.strides.add(i) = *v.strides.add(i - 1) * *v.shape.add(i - 1);
        }
    }
}

unsafe fn init_shape_strides(dest: &mut Py_buffer, src: &Py_buffer) {
    unsafe {
        let ndim = src.ndim as usize;
        if ndim == 0 {
            dest.shape = std::ptr::null_mut();
            dest.strides = std::ptr::null_mut();
            return;
        }
        if ndim == 1 {
            *dest.shape = if src.shape.is_null() { src.len / src.itemsize.max(1) } else { *src.shape };
            *dest.strides = if src.strides.is_null() { src.itemsize } else { *src.strides };
            return;
        }
        for i in 0..ndim {
            *dest.shape.add(i) = *src.shape.add(i);
        }
        if src.strides.is_null() {
            init_strides_from_shape(dest);
        } else {
            for i in 0..ndim {
                *dest.strides.add(i) = *src.strides.add(i);
            }
        }
    }
}

unsafe fn init_suboffsets(dest: &mut Py_buffer, src: &Py_buffer) {
    unsafe {
        if src.suboffsets.is_null() {
            dest.suboffsets = std::ptr::null_mut();
            return;
        }
        for i in 0..src.ndim as usize {
            *dest.suboffsets.add(i) = *src.suboffsets.add(i);
        }
    }
}

unsafe fn init_len(v: &mut Py_buffer) {
    unsafe {
        let mut len = 1isize;
        for i in 0..v.ndim as usize {
            len *= *v.shape.add(i);
        }
        v.len = len * v.itemsize;
    }
}

fn init_flags(m: &mut PyMemoryViewObject) {
    let v = &m.view;
    let mut flags = 0;
    unsafe {
        match v.ndim {
            0 => flags |= MV_SCALAR | MV_C | MV_FORTRAN,
            1 => {
                if *v.shape == 1 || *v.strides == v.itemsize {
                    flags |= MV_C | MV_FORTRAN;
                }
            }
            _ => {
                let tmp = Py_buffer { suboffsets: std::ptr::null_mut(), ..std::ptr::read(v) };
                if is_contiguous(&tmp, b'C') {
                    flags |= MV_C;
                }
                if is_contiguous(&tmp, b'F') {
                    flags |= MV_FORTRAN;
                }
                std::mem::forget(tmp);
            }
        }
    }
    if !v.suboffsets.is_null() {
        flags |= MV_PIL;
        flags &= !(MV_C | MV_FORTRAN);
    }
    m.flags = flags;
}

/// mbuf_add_view: a new memoryview over `mbuf` with the geometry of `src`
/// (the master buffer if None).
unsafe fn mbuf_add_view(mbuf: *mut ManagedBuffer, src: Option<&Py_buffer>) -> PyResult {
    unsafe {
        let src: &Py_buffer = match src {
            Some(s) => s,
            None => &(*mbuf).master,
        };
        if src.ndim as usize > PyBUF_MAX_NDIM {
            raise!(ValueError, "memoryview: number of dimensions must not exceed {}", PyBUF_MAX_NDIM);
        }
        let m = memory_alloc(src.ndim.max(0) as usize);
        let r = PyRef::from_raw(m as *mut PyObject);
        let dest = &mut (*m).view;
        init_shared_values(dest, src);
        init_shape_strides(dest, src);
        init_suboffsets(dest, src);
        init_flags(&mut *m);
        (*m).mbuf = mbuf;
        incref(mbuf as *mut PyObject);
        (*mbuf).exports += 1;
        crate::gc::track(m as *mut PyObject);
        Ok(r)
    }
}

/// mbuf_add_incomplete_view: shared values only (for casts).
unsafe fn mbuf_add_incomplete_view(mbuf: *mut ManagedBuffer, src: &Py_buffer, ndim: usize) -> PyResult {
    unsafe {
        let m = memory_alloc(ndim);
        let r = PyRef::from_raw(m as *mut PyObject);
        init_shared_values(&mut (*m).view, src);
        (*m).mbuf = mbuf;
        incref(mbuf as *mut PyObject);
        (*mbuf).exports += 1;
        crate::gc::track(m as *mut PyObject);
        Ok(r)
    }
}

/// PyMemoryView_FromObjectAndFlags.
pub fn from_object_flags(v: &PyObject, flags: c_int) -> PyResult {
    if is_memoryview(v) {
        check_released(v)?;
        check_restricted(v)?;
        let m = mv(v);
        return unsafe { mbuf_add_view(m.mbuf, Some(&m.view)) };
    }
    if check_buffer(v) {
        let mbuf = mbuf_from_object(v, flags)?;
        return unsafe { mbuf_add_view(mbuf.as_ptr() as *mut ManagedBuffer, None) };
    }
    raise!(TypeError, "memoryview: a bytes-like object is required, not '{}'", tname(v));
}

/// PyMemoryView_FromObject.
pub fn from_object(v: &PyObject) -> PyResult {
    from_object_flags(v, PyBUF_FULL_RO)
}

/// PyMemoryView_FromBuffer (the view's obj is not referenced; the view's
/// arrays are copied).
pub unsafe fn from_buffer(info: &Py_buffer) -> PyResult {
    unsafe {
        if info.buf.is_null() {
            raise!(ValueError, "PyMemoryView_FromBuffer(): info->buf must not be NULL");
        }
        let m = mbuf_alloc();
        let r = PyRef::from_raw(m as *mut PyObject);
        (*m).master = std::ptr::read(info);
        (*m).master.obj = std::ptr::null_mut();
        let res = mbuf_add_view(m, None);
        drop(r);
        res
    }
}

/// PyMemoryView_FromMemory(mem, size, PyBUF_READ|PyBUF_WRITE).
pub unsafe fn from_memory(mem: *mut u8, size: isize, flags: c_int) -> PyResult {
    unsafe {
        if flags != PyBUF_READ && flags != PyBUF_WRITE {
            return Err(system_error("bad argument to internal function"));
        }
        let m = mbuf_alloc();
        let r = PyRef::from_raw(m as *mut PyObject);
        let readonly = flags != PyBUF_WRITE;
        fill_info(&mut (*m).master, std::ptr::null_mut(), mem as *mut c_void, size, readonly, PyBUF_FULL_RO)?;
        let res = mbuf_add_view(m, None);
        drop(r);
        res
    }
}

/// _PyMemoryView_FromBufferProc: used by the `__buffer__` wrapper of builtin types.
pub fn from_bufferproc(o: &PyObject, flags: isize, proc: getbufferproc) -> PyResult {
    if flags > i32::MAX as isize || flags < i32::MIN as isize {
        raise!(OverflowError, "buffer flags out of range");
    }
    let flags = flags as c_int;
    if flags == PyBUF_READ || flags == PyBUF_WRITE {
        return Err(system_error("bad argument to internal function"));
    }
    let proc = match proc {
        Some(p) => p,
        None => raise!(TypeError, "a bytes-like object is required, not '{}'", tname(o)),
    };
    unsafe {
        let m = mbuf_alloc();
        let r = PyRef::from_raw(m as *mut PyObject);
        if proc(o.as_ptr(), &mut (*m).master, flags) < 0 {
            if !err_occurred() {
                return Err(system_error("getbuffer failed without setting an exception"));
            }
            return Err(PyErr);
        }
        let res = mbuf_add_view(m, None);
        drop(r);
        res
    }
}

/// Entry used by slots.rs for `__buffer__` wrappers (kept for compatibility).
pub fn memoryview_from_object_flags(o: &PyObject, flags: c_int) -> PyResult {
    from_object_flags(o, flags)
}

/// PyMemoryView_GetContiguous.
pub fn get_contiguous(obj: &PyObject, buffertype: c_int, order: u8) -> PyResult {
    let m = from_object(obj)?;
    let view = &mv(&m).view;
    if buffertype == PyBUF_WRITE && view.readonly != 0 {
        raise!(BufferError, "underlying buffer is not writable");
    }
    if is_contiguous(view, order) {
        return Ok(m);
    }
    if buffertype == PyBUF_WRITE {
        raise!(BufferError, "writable contiguous buffer requested for a non-contiguous object.");
    }
    unsafe { memory_from_contiguous_copy(view, order) }
}

unsafe fn memory_from_contiguous_copy(src: &Py_buffer, order: u8) -> PyResult {
    unsafe {
        let b = bytes::try_alloc_bytes(src.len as usize)?;
        copy_to_contiguous(bytes::bytes_ptr(&b), src, if order == b'F' { b'F' } else { b'C' });
        let mbuf = mbuf_from_object(&b, PyBUF_FULL_RO)?;
        let mb = mbuf.as_ptr() as *mut ManagedBuffer;
        // keep the source format
        let fmt = std::ffi::CString::new(fmt_bytes(src).to_vec()).unwrap_or_default();
        (*mb).master.format = fmt.into_raw();
        (*mb).flags |= MBUF_FREE_FORMAT;
        let r = mbuf_add_incomplete_view(mb, src, src.ndim.max(0) as usize)?;
        let m = mv(&r);
        m.view.buf = (*mb).master.buf;
        m.view.obj = (*mb).master.obj;
        m.view.format = (*mb).master.format;
        m.view.readonly = 1;
        init_shape_strides(&mut m.view, src);
        if src.ndim > 1 {
            if order == b'F' {
                init_fortran_strides_from_shape(&mut m.view);
            } else {
                init_strides_from_shape(&mut m.view);
            }
        } else if src.ndim == 1 {
            *m.view.strides = m.view.itemsize;
        }
        m.view.suboffsets = std::ptr::null_mut();
        init_flags(m);
        Ok(r)
    }
}

// ====================================================================== release

fn memory_release(o: &PyObject) -> PyResult<()> {
    let m = mv(o);
    if m.flags & MV_RELEASED != 0 {
        return Ok(());
    }
    if m.exports == 0 {
        m.flags |= MV_RELEASED;
        if !m.mbuf.is_null() {
            unsafe {
                (*m.mbuf).exports -= 1;
                if (*m.mbuf).exports == 0 {
                    mbuf_release(m.mbuf);
                }
            }
        }
        return Ok(());
    }
    if m.exports > 0 {
        raise!(BufferError, "memoryview has {} exported buffer{}", m.exports, if m.exports == 1 { "" } else { "s" });
    }
    Err(system_error("_memory_release(): negative export count"))
}

unsafe extern "C" fn memory_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let ob = &*o;
        if mv(ob).exports == 0 {
            let _ = memory_release(ob);
        }
        let m = mv(ob);
        let mb = std::mem::replace(&mut m.mbuf, std::ptr::null_mut());
        crate::types::weakref::clear_weakrefs_at(o, &mut m.weakreflist);
        xdecref(mb as *mut PyObject);
        crate::alloc::free_object(o);
    }
}
unsafe extern "C" fn memory_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let m = mv(&*o);
        if m.mbuf.is_null() { 0 } else { visit.unwrap()(m.mbuf as *mut PyObject, arg) }
    }
}
unsafe extern "C" fn memory_clear(o: *mut PyObject) -> c_int {
    unsafe {
        let ob = &*o;
        if memory_release(ob).is_err() {
            clear_exc();
        }
        let m = mv(ob);
        let mb = std::mem::replace(&mut m.mbuf, std::ptr::null_mut());
        xdecref(mb as *mut PyObject);
        0
    }
}

// ====================================================================== buffer export

unsafe extern "C" fn memory_getbuf(o: *mut PyObject, view: *mut Py_buffer, flags: c_int) -> c_int {
    unsafe {
        let ob = &*o;
        if check_released(ob).is_err() || check_restricted(ob).is_err() {
            return -1;
        }
        let m = mv(ob);
        let base = &m.view;
        let baseflags = m.flags;
        let v = &mut *view;
        *v = std::ptr::read(base);
        v.obj = std::ptr::null_mut();
        if flags & PyBUF_WRITABLE != 0 && base.readonly != 0 {
            let _ = err!(BufferError, "memoryview: underlying buffer is not writable");
            return -1;
        }
        if flags & PyBUF_FORMAT == 0 {
            v.format = std::ptr::null_mut();
        }
        if flags & PyBUF_C_CONTIGUOUS == PyBUF_C_CONTIGUOUS && baseflags & MV_C == 0 {
            let _ = err!(BufferError, "memoryview: underlying buffer is not C-contiguous");
            return -1;
        }
        if flags & PyBUF_F_CONTIGUOUS == PyBUF_F_CONTIGUOUS && baseflags & MV_FORTRAN == 0 {
            let _ = err!(BufferError, "memoryview: underlying buffer is not Fortran contiguous");
            return -1;
        }
        if flags & PyBUF_ANY_CONTIGUOUS == PyBUF_ANY_CONTIGUOUS && baseflags & (MV_C | MV_FORTRAN) == 0 {
            let _ = err!(BufferError, "memoryview: underlying buffer is not contiguous");
            return -1;
        }
        if flags & PyBUF_INDIRECT != PyBUF_INDIRECT && baseflags & MV_PIL != 0 {
            let _ = err!(BufferError, "memoryview: underlying buffer requires suboffsets");
            return -1;
        }
        if flags & PyBUF_STRIDES != PyBUF_STRIDES {
            if baseflags & MV_C == 0 {
                let _ = err!(BufferError, "memoryview: underlying buffer is not C-contiguous");
                return -1;
            }
            v.strides = std::ptr::null_mut();
        }
        if flags & PyBUF_ND != PyBUF_ND {
            if !v.format.is_null() {
                let _ = err!(BufferError, "memoryview: cannot cast to unsigned bytes if the format flag is present");
                return -1;
            }
            v.ndim = 1;
            v.shape = std::ptr::null_mut();
        }
        v.obj = ob.to_ref().into_raw();
        m.exports += 1;
        0
    }
}
unsafe extern "C" fn memory_releasebuf(o: *mut PyObject, _view: *mut Py_buffer) {
    unsafe {
        mv(&*o).exports -= 1;
    }
}

// ====================================================================== PEP 688

/// `type.__release_buffer__(self, buffer)` for builtin exporters.
pub fn release_buffer_wrapper(o: &PyObject, b: &PyObject) -> PyResult {
    if !is_memoryview(b) {
        raise!(TypeError, "expected a memoryview object");
    }
    let m = mv(b);
    if m.view.obj.is_null() {
        return Ok(none::none_ref());
    }
    if m.view.obj != o.as_ptr() {
        raise!(ValueError, "memoryview's buffer is not this object");
    }
    if m.flags & MV_RELEASED != 0 {
        raise!(ValueError, "memoryview's buffer has already been released");
    }
    memory_release(b)?;
    Ok(none::none_ref())
}

fn is_generic_releasebuffer(tp: *mut PyTypeObject) -> bool {
    unsafe {
        let bp = (*tp).tp_as_buffer;
        if bp.is_null() {
            return false;
        }
        match (*bp).bf_releasebuffer {
            Some(f) => f as *mut c_void == crate::types::slots::Slot::BfReleasebuffer.generic(),
            None => false,
        }
    }
}

/// slot_bf_getbuffer for classes defining `__buffer__`.
pub unsafe fn slot_getbuffer_via_dunder(o: &PyObject, view: *mut Py_buffer, flags: c_int) -> c_int {
    let r = (|| -> PyResult<()> {
        let fl = int::new_int(flags as i64);
        let ret = match crate::types::slots::call_special(o, crate::name!("__buffer__"), &[&fl])? {
            Some(r) => r,
            None => raise!(TypeError, "a bytes-like object is required, not '{}'", tname(o)),
        };
        if !is_memoryview(&ret) {
            raise!(TypeError, "__buffer__ returned non-memoryview object");
        }
        unsafe {
            get_buffer(&ret, &mut *view, flags)?;
            let w = crate::alloc::alloc_object(&raw mut _PyBufferWrapper_Type, std::mem::size_of::<BufferWrapper>()) as *mut BufferWrapper;
            (*w).mv = ret.into_raw();
            (*w).obj = o.to_ref().into_raw();
            crate::gc::track(w as *mut PyObject);
            let old = std::mem::replace(&mut (*view).obj, w as *mut PyObject);
            xdecref(old);
        }
        Ok(())
    })();
    int_into_c(r)
}

unsafe fn releasebuffer_call_python(o: &PyObject, view: *mut Py_buffer) {
    unsafe {
        let saved = take_exc();
        let vobj = (*view).obj;
        let is_wrapper = !vobj.is_null() && (*vobj).is_type(&raw mut _PyBufferWrapper_Type);
        let mvobj: Option<PyRef> = if is_wrapper {
            let w = vobj as *mut BufferWrapper;
            if (*w).mv.is_null() { None } else { Some((*(*w).mv).to_ref()) }
        } else {
            match from_buffer(&*view) {
                Ok(m) => {
                    mv(&m).flags |= MV_RESTRICTED;
                    Some(m)
                }
                Err(_) => {
                    crate::modules::sys::write_unraisable_with_obj(
                        &format!("Exception ignored in bf_releasebuffer of {}", crate::types::typeobject::type_name(o.ty())),
                        None,
                    );
                    None
                }
            }
        };
        if let Some(m) = mvobj {
            if crate::types::slots::call_special(o, crate::name!("__release_buffer__"), &[&m]).is_err() {
                crate::modules::sys::write_unraisable_with_obj(
                    &format!("Exception ignored in __release_buffer__ of {}", crate::types::typeobject::type_name(o.ty())),
                    None,
                );
            }
            if !is_wrapper {
                if memory_release(&m).is_err() {
                    crate::modules::sys::write_unraisable_with_obj(
                        &format!("Exception ignored in bf_releasebuffer of {}", crate::types::typeobject::type_name(o.ty())),
                        None,
                    );
                }
            }
        }
        if let Some(e) = saved {
            set_exc(e);
        }
    }
}

unsafe fn releasebuffer_maybe_call_super(o: &PyObject, view: *mut Py_buffer) {
    unsafe {
        let tp = o.type_ptr();
        let mro = (*tp).tp_mro;
        if mro.is_null() {
            return;
        }
        let items = tuple::tuple_items(&*mro);
        let mut i = 0;
        while i < items.len() && items[i].as_ptr() != tp as *mut PyObject {
            i += 1;
        }
        i += 1;
        while i < items.len() {
            let b = items[i];
            if crate::types::typeobject::is_type(b) {
                let bt = b.as_ptr() as *mut PyTypeObject;
                let bp = (*bt).tp_as_buffer;
                if !bp.is_null() {
                    if let Some(f) = (*bp).bf_releasebuffer {
                        if !is_generic_releasebuffer(bt) {
                            f(o.as_ptr(), view);
                            return;
                        }
                    }
                }
            }
            i += 1;
        }
    }
}

/// slot_bf_releasebuffer for classes defining `__release_buffer__`.
pub unsafe fn slot_releasebuffer_via_dunder(o: &PyObject, view: *mut Py_buffer) {
    unsafe {
        releasebuffer_call_python(o, view);
        releasebuffer_maybe_call_super(o, view);
    }
}

unsafe extern "C" fn bufferwrapper_releasebuf(o: *mut PyObject, view: *mut Py_buffer) {
    unsafe {
        let w = o as *mut BufferWrapper;
        if (*w).mv.is_null() || (*w).obj.is_null() {
            return;
        }
        let m = &*(*w).mv;
        let obj = &*(*w).obj;
        memory_releasebuf(m.as_ptr(), view);
        if mv(m).view.obj != obj.as_ptr() && is_generic_releasebuffer(obj.type_ptr()) {
            releasebuffer_call_python(obj, view);
        }
    }
}
unsafe extern "C" fn bufferwrapper_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let w = o as *mut BufferWrapper;
        xdecref(std::mem::replace(&mut (*w).mv, std::ptr::null_mut()));
        xdecref(std::mem::replace(&mut (*w).obj, std::ptr::null_mut()));
        crate::alloc::free_object(o);
    }
}
unsafe extern "C" fn bufferwrapper_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let w = o as *mut BufferWrapper;
        for p in [(*w).mv, (*w).obj] {
            if !p.is_null() {
                let r = visit.unwrap()(p, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        0
    }
}

// ====================================================================== item access

unsafe fn ptr_from_index(v: &Py_buffer, index: isize, dim: usize) -> PyResult<*mut u8> {
    unsafe {
        let nitems = *v.shape.add(dim);
        let mut i = index;
        if i < 0 {
            i += nitems;
        }
        if i < 0 || i >= nitems {
            raise!(IndexError, "index out of bounds on dimension {}", dim + 1);
        }
        let p = (v.buf as *mut u8).offset(*v.strides.add(dim) * i);
        Ok(adjust_ptr(p, v.suboffsets, dim))
    }
}

unsafe fn lookup_dimension(v: &Py_buffer, ptr: *mut u8, dim: usize, index: isize) -> PyResult<*mut u8> {
    unsafe {
        let nitems = *v.shape.add(dim);
        let mut i = index;
        if i < 0 {
            i += nitems;
        }
        if i < 0 || i >= nitems {
            raise!(IndexError, "index out of bounds on dimension {}", dim + 1);
        }
        let p = ptr.offset(*v.strides.add(dim) * i);
        Ok(adjust_ptr(p, v.suboffsets, dim))
    }
}

/// Indices of a multi-index key (converted before any pointer is formed).
fn tuple_indices(v: &Py_buffer, key: &PyObject) -> PyResult<Vec<isize>> {
    let items = tuple::tuple_items(key);
    if items.len() > v.ndim as usize {
        raise!(TypeError, "cannot index {}-dimension view with {}-element tuple", v.ndim, items.len());
    }
    let mut out = Vec::with_capacity(items.len());
    for it in items {
        out.push(ab::index_as_ssize(it, Some(crate::types::exceptions::IndexError()))?);
    }
    Ok(out)
}

unsafe fn ptr_from_indices(v: &Py_buffer, idx: &[isize]) -> PyResult<*mut u8> {
    unsafe {
        let mut p = v.buf as *mut u8;
        for (dim, &i) in idx.iter().enumerate() {
            p = lookup_dimension(v, p, dim, i)?;
        }
        Ok(p)
    }
}

fn is_multiindex(key: &PyObject) -> bool {
    tuple::is_tuple(key) && tuple::tuple_items(key).iter().all(|x| ab::has_index(x))
}
fn is_multislice(key: &PyObject) -> bool {
    tuple::is_tuple(key) && tuple::tuple_items(key).iter().all(|x| slice::is_slice(x))
}

fn memory_item(o: &PyObject, index: isize) -> PyResult {
    check_released(o)?;
    let v = &mv(o).view;
    let f = adjust_fmt(v)?;
    if v.ndim == 0 {
        raise!(TypeError, "invalid indexing of 0-dim memory");
    }
    if v.ndim == 1 {
        unsafe {
            let p = ptr_from_index(v, index, 0)?;
            return unpack_single(p, f);
        }
    }
    raise!(NotImplementedError, "multi-dimensional sub-views are not implemented");
}

unsafe fn init_slice(base: &mut Py_buffer, key: &PyObject, dim: usize) -> PyResult<()> {
    unsafe {
        let (start, _stop, step, slicelen) = slice::indices(key, *base.shape.add(dim))?;
        if base.suboffsets.is_null() || dim == 0 {
            base.buf = (base.buf as *mut u8).offset(*base.strides.add(dim) * start) as *mut c_void;
        } else {
            let mut n = dim as isize - 1;
            while n >= 0 && *base.suboffsets.add(n as usize) < 0 {
                n -= 1;
            }
            if n < 0 {
                base.buf = (base.buf as *mut u8).offset(*base.strides.add(dim) * start) as *mut c_void;
            } else {
                *base.suboffsets.add(n as usize) += *base.strides.add(dim) * start;
            }
        }
        *base.shape.add(dim) = slicelen;
        *base.strides.add(dim) *= step;
        Ok(())
    }
}

fn subscript_impl(o: &PyObject, key: &PyObject) -> PyResult {
    check_released(o)?;
    let v = &mv(o).view;
    if v.ndim == 0 {
        if tuple::is_tuple(key) && tuple::tuple_len(key) == 0 {
            let f = adjust_fmt(v)?;
            return unsafe { unpack_single(v.buf as *const u8, f) };
        }
        if key.is(none::ellipsis()) {
            return Ok(o.to_ref());
        }
        raise!(TypeError, "invalid indexing of 0-dim memory");
    }
    if ab::has_index(key) {
        let i = ab::index_as_ssize(key, Some(crate::types::exceptions::IndexError()))?;
        return memory_item(o, i);
    }
    if slice::is_slice(key) {
        check_restricted(o)?;
        let m = mv(o);
        let sliced = unsafe { mbuf_add_view(m.mbuf, Some(&m.view))? };
        unsafe {
            let sv = &mut mv(&sliced).view;
            init_slice(sv, key, 0)?;
            init_len(sv);
        }
        init_flags(mv(&sliced));
        return Ok(sliced);
    }
    if is_multiindex(key) {
        let f = adjust_fmt(v)?;
        if tuple::tuple_len(key) < v.ndim as usize {
            raise!(NotImplementedError, "sub-views are not implemented");
        }
        let idx = tuple_indices(v, key)?;
        check_released(o)?;
        let v = &mv(o).view;
        unsafe {
            let p = ptr_from_indices(v, &idx)?;
            return unpack_single(p, f);
        }
    }
    if is_multislice(key) {
        raise!(NotImplementedError, "multi-dimensional slicing is not implemented");
    }
    raise!(TypeError, "memoryview: invalid slice key");
}

unsafe extern "C" fn memory_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(subscript_impl(&*o, &*k)) }
}
unsafe extern "C" fn memory_sq_item(o: *mut PyObject, i: Py_ssize_t) -> *mut PyObject {
    unsafe { into_c(memory_item(&*o, i)) }
}

fn equiv_format(d: &Py_buffer, s: &Py_buffer) -> bool {
    let strip = |f: &[u8]| -> Vec<u8> { if f.first() == Some(&b'@') { f[1..].to_vec() } else { f.to_vec() } };
    strip(fmt_bytes(d)) == strip(fmt_bytes(s)) && d.itemsize == s.itemsize
}
fn equiv_shape(d: &Py_buffer, s: &Py_buffer) -> bool {
    if d.ndim != s.ndim {
        return false;
    }
    let (ds, _) = view_geometry(d);
    let (ss, _) = view_geometry(s);
    for i in 0..ds.len() {
        if ds[i] != ss[i] {
            return false;
        }
        if ds[i] == 0 {
            break;
        }
    }
    true
}

fn ass_sub_impl(o: &PyObject, key: &PyObject, value: Option<&PyObject>) -> PyResult<()> {
    check_released(o)?;
    let v = &mv(o).view;
    let fname = fmt_string(v);
    let f = adjust_fmt(v)?;
    if v.readonly != 0 {
        raise!(TypeError, "cannot modify read-only memory");
    }
    let value = match value {
        None => raise!(TypeError, "cannot delete memory"),
        Some(x) => x,
    };
    let mut tmp = [0u8; 16];
    if v.ndim == 0 {
        if key.is(none::ellipsis()) || (tuple::is_tuple(key) && tuple::tuple_len(key) == 0) {
            pack_single(value, f, &fname, &mut tmp)?;
            check_released(o)?;
            let v = &mv(o).view;
            unsafe { std::ptr::copy_nonoverlapping(tmp.as_ptr(), v.buf as *mut u8, f.size()) };
            return Ok(());
        }
        raise!(TypeError, "invalid indexing of 0-dim memory");
    }
    if ab::has_index(key) {
        if v.ndim > 1 {
            raise!(NotImplementedError, "sub-views are not implemented");
        }
        let i = ab::index_as_ssize(key, Some(crate::types::exceptions::IndexError()))?;
        let p = unsafe { ptr_from_index(&mv(o).view, i, 0)? };
        pack_single(value, f, &fname, &mut tmp)?;
        check_released(o)?;
        unsafe { std::ptr::copy_nonoverlapping(tmp.as_ptr(), p, f.size()) };
        return Ok(());
    }
    if slice::is_slice(key) && v.ndim == 1 {
        let src = BufferView::get(value, PyBUF_FULL_RO)?;
        let mut arrays = [0isize; 3];
        let mut dest = unsafe { std::ptr::read(v) };
        unsafe {
            arrays[0] = *v.shape;
            arrays[1] = *v.strides;
            dest.shape = arrays.as_mut_ptr();
            dest.strides = arrays.as_mut_ptr().add(1);
            if !v.suboffsets.is_null() {
                arrays[2] = *v.suboffsets;
                dest.suboffsets = arrays.as_mut_ptr().add(2);
            }
            init_slice(&mut dest, key, 0)?;
            dest.len = *dest.shape * dest.itemsize;
        }
        check_released(o)?;
        if !equiv_format(&dest, &src.view) || !equiv_shape(&dest, &src.view) {
            raise!(ValueError, "memoryview assignment: lvalue and rvalue have different structures");
        }
        let data = src.to_vec();
        unsafe { from_contiguous(&dest, data.as_ptr(), data.len() as isize, b'C')? };
        std::mem::forget(dest);
        return Ok(());
    }
    if is_multiindex(key) {
        if tuple::tuple_len(key) < v.ndim as usize {
            raise!(NotImplementedError, "sub-views are not implemented");
        }
        let idx = tuple_indices(v, key)?;
        let p = unsafe { ptr_from_indices(&mv(o).view, &idx)? };
        pack_single(value, f, &fname, &mut tmp)?;
        check_released(o)?;
        unsafe { std::ptr::copy_nonoverlapping(tmp.as_ptr(), p, f.size()) };
        return Ok(());
    }
    if slice::is_slice(key) || is_multislice(key) {
        raise!(NotImplementedError, "memoryview slice assignments are currently restricted to ndim = 1");
    }
    raise!(TypeError, "memoryview: invalid slice key");
}
unsafe extern "C" fn memory_ass_sub(o: *mut PyObject, k: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe { int_into_c(ass_sub_impl(&*o, &*k, if v.is_null() { None } else { Some(&*v) })) }
}

unsafe extern "C" fn memory_length(o: *mut PyObject) -> Py_ssize_t {
    unsafe {
        let ob = &*o;
        if check_released(ob).is_err() {
            return -1;
        }
        let v = &mv(ob).view;
        if v.ndim == 0 { 1 } else { *v.shape }
    }
}

// ====================================================================== tolist / tobytes / hex

unsafe fn tolist_rec(p: *mut u8, v: &Py_buffer, dim: usize, shape: &[isize], strides: &[isize], f: NFmt) -> PyResult {
    unsafe {
        let n = shape[dim];
        let mut items = Vec::with_capacity(n.max(0) as usize);
        for i in 0..n {
            let q = adjust_ptr(p.offset(strides[dim] * i), v.suboffsets, dim);
            if dim + 1 == shape.len() {
                items.push(unpack_single(q, f)?);
            } else {
                items.push(tolist_rec(q, v, dim + 1, shape, strides, f)?);
            }
        }
        Ok(crate::types::list::new_list(items))
    }
}

fn m_tolist(o: &PyObject) -> PyResult {
    check_released(o)?;
    let v = &mv(o).view;
    let f = adjust_fmt(v)?;
    unsafe {
        if v.ndim == 0 {
            return unpack_single(v.buf as *const u8, f);
        }
        let (shape, strides) = view_geometry(v);
        tolist_rec(v.buf as *mut u8, v, 0, &shape, &strides, f)
    }
}

fn view_bytes(v: &Py_buffer, order: u8) -> Vec<u8> {
    let mut out = vec![0u8; v.len.max(0) as usize];
    unsafe { copy_to_contiguous(out.as_mut_ptr(), v, order) };
    out
}

fn m_tobytes(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("tobytes", &["order"], 0, 0);
    let [order] = crate::args::parse::<1>(&P, args, kw)?;
    check_released(o)?;
    let ord = match order {
        None => b'C',
        Some(x) if none::is_none(x) => b'C',
        Some(x) => {
            if !pystr::is_str(x) {
                raise!(TypeError, "tobytes() argument 'order' must be str or None, not {}", tname(x));
            }
            match pystr::to_string_lossy(x).as_str() {
                "C" => b'C',
                "F" => b'F',
                "A" => b'A',
                _ => raise!(ValueError, "order must be 'C', 'F' or 'A'"),
            }
        }
    };
    let v = &mv(o).view;
    bytes::try_new_bytes(&view_bytes(v, ord))
}

fn m_hex(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    check_released(o)?;
    static P: crate::args::Params = crate::args::Params::new("hex", &["sep", "bytes_per_sep"], 0, 0);
    let [sep, bps] = crate::args::parse::<2>(&P, args, kw)?;
    let bps = match bps {
        None => 1,
        Some(b) => crate::types::bytes_methods::ssize_arg(b)?,
    };
    mv(o).exports += 1;
    let sc = crate::types::bytes_methods::hex_sep(sep);
    mv(o).exports -= 1;
    let sc = sc?;
    check_released(o)?;
    let v = &mv(o).view;
    let data = view_bytes(v, b'C');
    Ok(pystr::new_str(&crate::types::bytes_methods::strhex_raw(&data, sc, bps)))
}

// ====================================================================== cast / toreadonly / release

fn m_cast(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("cast", &["format", "shape"], 0, 1);
    let [format, shape] = crate::args::parse::<2>(&P, args, kw)?;
    let format = format.unwrap();
    if !pystr::is_str(format) {
        raise!(TypeError, "cast() argument 'format' must be str, not {}", tname(format));
    }
    check_released(o)?;
    check_restricted(o)?;
    let m = mv(o);
    if m.flags & MV_C == 0 {
        raise!(TypeError, "memoryview: casts are restricted to C-contiguous views");
    }
    let zero_in_shape = unsafe { (0..m.view.ndim as usize).any(|i| *m.view.shape.add(i) == 0) };
    if (shape.is_some() || m.view.ndim != 1) && zero_in_shape {
        raise!(TypeError, "memoryview: cannot cast view with zeros in shape or strides");
    }
    let mut ndim = 1usize;
    let mut shape_items: Vec<PyRef> = Vec::new();
    if let Some(s) = shape {
        if !crate::types::list::is_list(s) && !tuple::is_tuple(s) {
            raise!(TypeError, "shape must be a list or a tuple");
        }
        shape_items = ab::collect_iterable(s)?;
        ndim = shape_items.len();
        if ndim > PyBUF_MAX_NDIM {
            raise!(ValueError, "memoryview: number of dimensions must not exceed {}", PyBUF_MAX_NDIM);
        }
        if m.view.ndim != 1 && ndim != 1 {
            raise!(TypeError, "memoryview: cast must be 1D -> ND or ND -> 1D");
        }
    }
    let r = unsafe { mbuf_add_incomplete_view(m.mbuf, &m.view, if ndim == 0 { 1 } else { ndim })? };
    let nm = mv(&r);
    // cast_to_1D
    let fs = pystr::to_string_lossy(format);
    let (dest, at) = match native_fmt(fs.as_bytes()) {
        Some(x) if fs.is_ascii() => x,
        _ => raise!(ValueError, "memoryview: destination format must be a native single character format prefixed with an optional '@'"),
    };
    let src_is_byte = native_fmt(fmt_bytes(&nm.view)).is_some_and(|(f, _)| f.is_byte());
    if !src_is_byte && !dest.is_byte() {
        raise!(TypeError, "memoryview: cannot cast between two non-byte formats");
    }
    let itemsize = dest.size() as isize;
    if nm.view.len % itemsize != 0 {
        raise!(TypeError, "memoryview: length is not a multiple of itemsize");
    }
    nm.view.format = dest.cstr(at);
    nm.view.itemsize = itemsize;
    nm.view.ndim = 1;
    unsafe {
        *nm.view.shape = nm.view.len / itemsize;
        *nm.view.strides = itemsize;
    }
    nm.view.suboffsets = std::ptr::null_mut();
    init_flags(nm);
    // cast_to_ND
    if shape.is_some() {
        nm.view.ndim = ndim as c_int;
        let len: isize;
        if ndim == 0 {
            nm.view.shape = std::ptr::null_mut();
            nm.view.strides = std::ptr::null_mut();
            len = itemsize;
        } else {
            let mut l = itemsize;
            for (i, x) in shape_items.iter().enumerate() {
                if !int::is_int(x) {
                    raise!(TypeError, "memoryview.cast(): elements of shape must be integers");
                }
                let v = ab::index_as_ssize(x, Some(crate::types::exceptions::OverflowError()))?;
                if v <= 0 {
                    raise!(ValueError, "memoryview.cast(): elements of shape must be integers > 0");
                }
                if v > isize::MAX / l {
                    raise!(ValueError, "memoryview.cast(): product(shape) > SSIZE_MAX");
                }
                l *= v;
                unsafe { *nm.view.shape.add(i) = v };
            }
            len = l;
            unsafe { init_strides_from_shape(&mut nm.view) };
        }
        if nm.view.len != len {
            raise!(TypeError, "memoryview: product(shape) * itemsize != buffer size");
        }
        init_flags(nm);
    }
    Ok(r)
}

fn m_toreadonly(o: &PyObject) -> PyResult {
    check_released(o)?;
    check_restricted(o)?;
    let m = mv(o);
    let r = unsafe { mbuf_add_view(m.mbuf, Some(&m.view))? };
    mv(&r).view.readonly = 1;
    Ok(r)
}

fn m_release(o: &PyObject) -> PyResult {
    memory_release(o)?;
    Ok(none::none_ref())
}
fn m_enter(o: &PyObject) -> PyResult {
    check_released(o)?;
    Ok(o.to_ref())
}
fn m_exit(o: &PyObject, _args: &[&PyObject]) -> PyResult {
    m_release(o)
}

fn m_count(o: &PyObject, value: &PyObject) -> PyResult {
    let it = ab::get_iter(o)?;
    let mut c = 0i64;
    while let Some(x) = ab::iter_next(&it)? {
        if ab::rich_compare_bool(&x, value, Py_EQ)? {
            c += 1;
        }
    }
    Ok(int::new_int(c))
}

fn m_index(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("index", args, 1, 3)?;
    let value = args[0];
    let mut start = crate::types::bytes_methods::slice_index(args.get(1).copied(), 0)?;
    let mut stop = crate::types::bytes_methods::slice_index(args.get(2).copied(), isize::MAX)?;
    check_released(o)?;
    let v = &mv(o).view;
    if v.ndim == 0 {
        raise!(TypeError, "invalid lookup on 0-dim memory");
    }
    if v.ndim == 1 {
        let n = unsafe { *v.shape };
        if start < 0 {
            start = (start + n).max(0);
        }
        if stop < 0 {
            stop = (stop + n).max(0);
        }
        stop = stop.min(n);
        let mut i = start;
        while i < stop {
            let item = memory_item(o, i)?;
            if ab::rich_compare_bool(&item, value, Py_EQ)? {
                return Ok(int::new_int(i as i64));
            }
            i += 1;
        }
        raise!(ValueError, "memoryview.index(x): x not found");
    }
    raise!(NotImplementedError, "multi-dimensional lookup is not implemented");
}

fn m_from_flags(_cls: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("_from_flags", &["object", "flags"], 2, 2);
    let [obj, flags] = crate::args::parse::<2>(&P, args, kw)?;
    let f = ab::index_as_ssize(flags.unwrap(), Some(crate::types::exceptions::OverflowError()))?;
    if f > i32::MAX as isize || f < i32::MIN as isize {
        raise!(OverflowError, "Python int too large to convert to C int");
    }
    from_object_flags(obj.unwrap(), f as c_int)
}

fn m_class_getitem(cls: &PyObject, arg: &PyObject) -> PyResult {
    let types = crate::import::import_absolute("types")?;
    let ga = ab::getattr_str(&types, "GenericAlias")?;
    ab::call(&ga, &[cls, arg])
}

// ====================================================================== attributes

fn g_obj(o: &PyObject) -> PyResult {
    check_released(o)?;
    let p = mv(o).view.obj;
    if p.is_null() { Ok(none::none_ref()) } else { Ok(unsafe { (*p).to_ref() }) }
}
fn g_nbytes(o: &PyObject) -> PyResult {
    check_released(o)?;
    Ok(int::new_int(mv(o).view.len as i64))
}
fn g_readonly(o: &PyObject) -> PyResult {
    check_released(o)?;
    Ok(bool_::new_bool(mv(o).view.readonly != 0))
}
fn g_itemsize(o: &PyObject) -> PyResult {
    check_released(o)?;
    Ok(int::new_int(mv(o).view.itemsize as i64))
}
fn g_format(o: &PyObject) -> PyResult {
    check_released(o)?;
    Ok(pystr::new_str(&fmt_string(&mv(o).view)))
}
fn g_ndim(o: &PyObject) -> PyResult {
    check_released(o)?;
    Ok(int::new_int(mv(o).view.ndim as i64))
}
fn ssize_tuple(p: *const isize, n: usize) -> PyRef {
    if p.is_null() {
        return tuple::empty_tuple();
    }
    let v: Vec<PyRef> = (0..n).map(|i| int::new_int(unsafe { *p.add(i) } as i64)).collect();
    tuple::new_tuple(v)
}
fn g_shape(o: &PyObject) -> PyResult {
    check_released(o)?;
    let v = &mv(o).view;
    Ok(ssize_tuple(v.shape, v.ndim as usize))
}
fn g_strides(o: &PyObject) -> PyResult {
    check_released(o)?;
    let v = &mv(o).view;
    Ok(ssize_tuple(v.strides, v.ndim as usize))
}
fn g_suboffsets(o: &PyObject) -> PyResult {
    check_released(o)?;
    let v = &mv(o).view;
    Ok(ssize_tuple(v.suboffsets, v.ndim as usize))
}
fn g_c_contiguous(o: &PyObject) -> PyResult {
    check_released(o)?;
    Ok(bool_::new_bool(mv(o).flags & MV_C != 0))
}
fn g_f_contiguous(o: &PyObject) -> PyResult {
    check_released(o)?;
    Ok(bool_::new_bool(mv(o).flags & MV_FORTRAN != 0))
}
fn g_contiguous(o: &PyObject) -> PyResult {
    check_released(o)?;
    Ok(bool_::new_bool(mv(o).flags & (MV_C | MV_FORTRAN) != 0))
}

// ====================================================================== repr / hash / compare

unsafe extern "C" fn memory_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let ob = &*o;
        let s = if base_inaccessible(ob) { format!("<released memory at {:#x}>", o as usize) } else { format!("<memory at {:#x}>", o as usize) };
        pystr::new_str(&s).into_raw()
    }
}

unsafe extern "C" fn memory_hash(o: *mut PyObject) -> Py_hash_t {
    unsafe {
        let ob = &*o;
        let r = (|| -> PyResult<isize> {
            let m = mv(ob);
            if m.hash != -1 {
                return Ok(m.hash);
            }
            check_released(ob)?;
            let v = &m.view;
            if v.readonly == 0 {
                raise!(ValueError, "cannot hash writable memoryview object");
            }
            match native_fmt(fmt_bytes(v)) {
                Some((f, _)) if f.is_byte() => {}
                _ => raise!(ValueError, "memoryview: hashing is restricted to formats 'B', 'b' or 'c'"),
            }
            if !v.obj.is_null() {
                m.exports += 1;
                let h = ab::hash(&*v.obj);
                mv(ob).exports -= 1;
                h?;
            }
            check_released(ob)?;
            let v = &mv(ob).view;
            let data = view_bytes(v, b'C');
            let h = crate::hash::hash_bytes(&data);
            mv(ob).hash = h;
            Ok(h)
        })();
        match r {
            Ok(h) => h,
            Err(_) => -1,
        }
    }
}

fn val_eq(a: Val, b: Val) -> bool {
    fn int_float_eq(i: i128, f: f64) -> bool {
        if !f.is_finite() || f.fract() != 0.0 {
            return false;
        }
        if f.abs() >= 1.7e38 {
            return false;
        }
        (f as i128) == i
    }
    match (a, b) {
        (Val::Byte(x), Val::Byte(y)) => x == y,
        (Val::Byte(_), _) | (_, Val::Byte(_)) => false,
        (Val::I(x), Val::I(y)) => x == y,
        (Val::I(x), Val::F(y)) | (Val::F(y), Val::I(x)) => int_float_eq(x, y),
        (Val::F(x), Val::F(y)) => x == y,
        (Val::C(r, i), Val::C(r2, i2)) => r == r2 && i == i2,
        (Val::C(r, i), Val::F(f)) | (Val::F(f), Val::C(r, i)) => i == 0.0 && r == f,
        (Val::C(r, i), Val::I(x)) | (Val::I(x), Val::C(r, i)) => i == 0.0 && int_float_eq(x, r),
    }
}

fn item_ptrs(v: &Py_buffer) -> Vec<*mut u8> {
    let mut out = Vec::new();
    unsafe { for_each_item(v, b'C', &mut |p| out.push(p)) };
    out
}

/// Unpack one item with the struct module (non-native formats).
fn struct_unpack(unpacker: &PyObject, p: *const u8, itemsize: usize) -> PyResult {
    let b = bytes::new_bytes(unsafe { std::slice::from_raw_parts(p, itemsize) });
    let t = ab::call(unpacker, &[&b])?;
    if tuple::is_tuple(&t) && tuple::tuple_len(&t) == 1 {
        return Ok(tuple::tuple_get(&t, 0).to_ref());
    }
    Ok(t)
}
fn struct_unpacker(v: &Py_buffer) -> PyResult<PyRef> {
    let m = crate::import::import_absolute("struct")?;
    let st = ab::getattr_str(&m, "Struct")?;
    let s = ab::call(&st, &[&pystr::new_str(&fmt_string(v))])?;
    let size = ab::getattr_str(&s, "size")?;
    if ab::index_as_ssize(&size, None)? != v.itemsize {
        raise!(ValueError, "memoryview: struct size mismatch");
    }
    ab::getattr_str(&s, "unpack")
}

/// Compare two buffers for equality; Ok(None) means "not implemented".
fn buffers_equal(vv: &Py_buffer, ww: &Py_buffer, same_obj: bool) -> PyResult<bool> {
    if !equiv_shape(vv, ww) {
        return Ok(false);
    }
    let vf = native_fmt(fmt_bytes(vv)).map(|x| x.0);
    let wf = native_fmt(fmt_bytes(ww)).map(|x| x.0);
    if let (Some(a), Some(b)) = (vf, wf) {
        if same_obj && a == b && !a.is_floatish() {
            return Ok(true);
        }
        let pv = item_ptrs(vv);
        let pw = item_ptrs(ww);
        for (x, y) in pv.iter().zip(pw.iter()) {
            let (xa, yb) = unsafe { (read_val(*x, a), read_val(*y, b)) };
            if !val_eq(xa, yb) {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    // struct module fallback
    let uv = match struct_unpacker(vv) {
        Ok(u) => u,
        Err(_) => {
            clear_exc();
            return Ok(false);
        }
    };
    let uw = match struct_unpacker(ww) {
        Ok(u) => u,
        Err(_) => {
            clear_exc();
            return Ok(false);
        }
    };
    let pv = item_ptrs(vv);
    let pw = item_ptrs(ww);
    for (x, y) in pv.iter().zip(pw.iter()) {
        let a = match struct_unpack(&uv, *x, vv.itemsize as usize) {
            Ok(a) => a,
            Err(_) => {
                clear_exc();
                return Ok(false);
            }
        };
        let b = match struct_unpack(&uw, *y, ww.itemsize as usize) {
            Ok(b) => b,
            Err(_) => {
                clear_exc();
                return Ok(false);
            }
        };
        if !ab::rich_compare_bool(&a, &b, Py_EQ)? {
            return Ok(false);
        }
    }
    Ok(true)
}

unsafe extern "C" fn memory_richcompare(v: *mut PyObject, w: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (v, w) = (&*v, &*w);
        if op != Py_EQ && op != Py_NE || !is_memoryview(v) {
            return none::not_implemented().to_ref().into_raw();
        }
        let r = (|| -> PyResult<Option<bool>> {
            if base_inaccessible(v) {
                return Ok(Some(v.is(w)));
            }
            let wbuf: Option<BufferView>;
            let ww: &Py_buffer = if is_memoryview(w) {
                if base_inaccessible(w) {
                    return Ok(Some(v.is(w)));
                }
                wbuf = None;
                &mv(w).view
            } else {
                match BufferView::get(w, PyBUF_FULL_RO) {
                    Ok(b) => {
                        wbuf = Some(b);
                        &wbuf.as_ref().unwrap().view
                    }
                    Err(_) => {
                        clear_exc();
                        return Ok(None);
                    }
                }
            };
            let eq = buffers_equal(&mv(v).view, ww, v.is(w))?;
            drop(wbuf);
            Ok(Some(eq))
        })();
        match r {
            Err(_) => std::ptr::null_mut(),
            Ok(None) => none::not_implemented().to_ref().into_raw(),
            Ok(Some(eq)) => bool_::new_bool(if op == Py_EQ { eq } else { !eq }).into_raw(),
        }
    }
}

// ====================================================================== iterator

unsafe extern "C" fn memory_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let ob = &*o;
        let r = (|| -> PyResult {
            check_released(ob)?;
            let v = &mv(ob).view;
            if v.ndim == 0 {
                raise!(TypeError, "invalid indexing of 0-dim memory");
            }
            if v.ndim != 1 {
                raise!(NotImplementedError, "multi-dimensional sub-views are not implemented");
            }
            let f = adjust_fmt(v)?;
            let it = crate::alloc::alloc_object(&raw mut _PyMemoryIter_Type, std::mem::size_of::<MemoryIter>()) as *mut MemoryIter;
            (*it).index = 0;
            (*it).length = *v.shape;
            (*it).fmt = f;
            (*it).seq = ob.to_ref().into_raw();
            crate::gc::track(it as *mut PyObject);
            Ok(PyRef::from_raw(it as *mut PyObject))
        })();
        into_c(r)
    }
}
unsafe extern "C" fn memoryiter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let it = &mut *(o as *mut MemoryIter);
        if it.seq.is_null() {
            return std::ptr::null_mut();
        }
        if it.index < it.length {
            let seq = &*it.seq;
            if check_released(seq).is_err() {
                return std::ptr::null_mut();
            }
            let v = &mv(seq).view;
            let p = adjust_ptr((v.buf as *mut u8).offset(*v.strides * it.index), v.suboffsets, 0);
            it.index += 1;
            return into_c(unpack_single(p, it.fmt));
        }
        let s = std::mem::replace(&mut it.seq, std::ptr::null_mut());
        decref(s);
        std::ptr::null_mut()
    }
}
unsafe extern "C" fn memoryiter_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        xdecref((*(o as *mut MemoryIter)).seq);
        crate::alloc::free_object(o);
    }
}
unsafe extern "C" fn memoryiter_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let s = (*(o as *mut MemoryIter)).seq;
        if s.is_null() { 0 } else { visit.unwrap()(s, arg) }
    }
}

// ====================================================================== type

unsafe extern "C" fn memory_new(_tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (v, names) = crate::args::kwdict_to_vectorcall(&*args, kw);
        let refs: Vec<&PyObject> = v.iter().map(|r| &**r).collect();
        static P: crate::args::Params = crate::args::Params::new("memoryview", &["object"], 0, 1);
        let r = (|| {
            let [x] = crate::args::parse::<1>(&P, &refs, names.as_deref())?;
            from_object(x.unwrap())
        })();
        into_c(r)
    }
}

static mut MV_SEQ: PySequenceMethods = PySequenceMethods::ZERO;
static mut MV_MAP: PyMappingMethods = PyMappingMethods::ZERO;
static mut MV_BUF: PyBufferProcs = PyBufferProcs::ZERO;
static mut BW_BUF: PyBufferProcs = PyBufferProcs::ZERO;

pub fn init_types() {
    unsafe {
        let t = &mut *memoryview_type();
        t.tp_name = c"memoryview".as_ptr();
        t.tp_basicsize = std::mem::offset_of!(PyMemoryViewObject, ob_array) as isize;
        t.tp_itemsize = std::mem::size_of::<isize>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_SEQUENCE;
        t.tp_dealloc = Some(memory_dealloc);
        t.tp_traverse = Some(memory_traverse);
        t.tp_clear = Some(memory_clear);
        t.tp_repr = Some(memory_repr);
        t.tp_hash = Some(memory_hash);
        t.tp_richcompare = Some(memory_richcompare);
        t.tp_iter = Some(memory_iter);
        t.tp_new = Some(memory_new);
        t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
        t.tp_weaklistoffset = std::mem::offset_of!(PyMemoryViewObject, weakreflist) as isize;
        t.tp_doc = c"memoryview(object)\n--\n\nCreate a new memoryview object which references the given object.".as_ptr();
        let s = &mut *(&raw mut MV_SEQ);
        s.sq_length = Some(memory_length);
        s.sq_item = Some(memory_sq_item);
        t.tp_as_sequence = s;
        let m = &mut *(&raw mut MV_MAP);
        m.mp_length = Some(memory_length);
        m.mp_subscript = Some(memory_subscript);
        m.mp_ass_subscript = Some(memory_ass_sub);
        t.tp_as_mapping = m;
        let b = &mut *(&raw mut MV_BUF);
        b.bf_getbuffer = Some(memory_getbuf);
        b.bf_releasebuffer = Some(memory_releasebuf);
        t.tp_as_buffer = b;
        t.tp_getset = crate::getset_table![
            ("obj", g_obj; "The underlying object of the memoryview."),
            ("nbytes", g_nbytes; "The amount of space in bytes that the array would use in\n a contiguous representation."),
            ("readonly", g_readonly; "A bool indicating whether the memory is read only."),
            ("itemsize", g_itemsize; "The size in bytes of each element of the memoryview."),
            ("format", g_format; "A string containing the format (in struct module style)\n for each element in the view."),
            ("ndim", g_ndim; "An integer indicating how many dimensions of a multi-dimensional\n array the memory represents."),
            ("shape", g_shape; "A tuple of ndim integers giving the shape of the memory\n as an N-dimensional array."),
            ("strides", g_strides; "A tuple of ndim integers giving the size in bytes to access\n each element for each dimension of the array."),
            ("suboffsets", g_suboffsets; "A tuple of integers used internally for PIL-style arrays."),
            ("c_contiguous", g_c_contiguous; "A bool indicating whether the memory is C contiguous."),
            ("f_contiguous", g_f_contiguous; "A bool indicating whether the memory is Fortran contiguous."),
            ("contiguous", g_contiguous; "A bool indicating whether the memory is contiguous."),
        ];
        t.tp_methods = crate::mdefs![
            ("release", NOARGS, m_release, "release($self, /)\n--\n\nRelease the underlying buffer exposed by the memoryview object."),
            ("tobytes", FASTKW, m_tobytes, "tobytes($self, /, order='C')\n--\n\nReturn the data in the buffer as a byte string."),
            ("hex", FASTKW, m_hex, "hex($self, /, sep=<unrepresentable>, bytes_per_sep=1)\n--\n\nReturn the data in the buffer as a str of hexadecimal numbers."),
            ("tolist", NOARGS, m_tolist, "tolist($self, /)\n--\n\nReturn the data in the buffer as a list of elements."),
            ("cast", FASTKW, m_cast, "cast($self, /, format, shape=<unrepresentable>)\n--\n\nCast a memoryview to a new format or shape."),
            ("toreadonly", NOARGS, m_toreadonly, "toreadonly($self, /)\n--\n\nReturn a readonly version of the memoryview."),
            ("__enter__", NOARGS, m_enter),
            ("__exit__", FAST, m_exit, "__exit__($self, /, *exc_info)\n--\n\nRelease the underlying buffer exposed by the memoryview object."),
            ("count", O, m_count, "count($self, value, /)\n--\n\nCount the number of occurrences of a value."),
            ("index", FAST, m_index, "index($self, value, start=0, stop=sys.maxsize, /)\n--\n\nReturn the index of the first occurrence of a value."),
            ("_from_flags", FASTKW, m_from_flags, "_from_flags($type, object, flags, /)\n--\n\nCreate a new memoryview object which references the given object."; METH_CLASS),
            ("__class_getitem__", O, m_class_getitem, "See PEP 585"; METH_CLASS),
        ];

        let t = &mut *(&raw mut _PyManagedBuffer_Type);
        t.tp_name = c"managedbuffer".as_ptr();
        t.tp_basicsize = std::mem::size_of::<ManagedBuffer>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
        t.tp_dealloc = Some(mbuf_dealloc);
        t.tp_traverse = Some(mbuf_traverse);
        t.tp_clear = Some(mbuf_clear);
        t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);

        let t = &mut *(&raw mut _PyBufferWrapper_Type);
        t.tp_name = c"_buffer_wrapper".as_ptr();
        t.tp_basicsize = std::mem::size_of::<BufferWrapper>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
        t.tp_dealloc = Some(bufferwrapper_dealloc);
        t.tp_traverse = Some(bufferwrapper_traverse);
        t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
        let b = &mut *(&raw mut BW_BUF);
        b.bf_releasebuffer = Some(bufferwrapper_releasebuf);
        t.tp_as_buffer = b;

        let t = &mut *(&raw mut _PyMemoryIter_Type);
        t.tp_name = c"memory_iterator".as_ptr();
        t.tp_basicsize = std::mem::size_of::<MemoryIter>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
        t.tp_dealloc = Some(memoryiter_dealloc);
        t.tp_traverse = Some(memoryiter_traverse);
        t.tp_iter = Some(crate::types::bytes_methods::self_iter);
        t.tp_iternext = Some(memoryiter_next);
        t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
    }
}

pub fn ready() -> PyResult<()> {
    use crate::types::typeobject::type_ready;
    type_ready(memoryview_type())?;
    type_ready(&raw mut _PyManagedBuffer_Type)?;
    type_ready(&raw mut _PyBufferWrapper_Type)?;
    type_ready(&raw mut _PyMemoryIter_Type)?;
    Ok(())
}

/// Silence unused warnings for helpers kept for the C API workstream.
#[allow(dead_code)]
fn _unused() {
    let _ = buffer::view_format;
}
