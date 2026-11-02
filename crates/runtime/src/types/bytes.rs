//! `bytes` core: allocation and data access. Methods, slots and the type
//! object live in bytes_methods.rs.
use crate::alloc;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;

#[unsafe(no_mangle)]
pub static mut PyBytes_Type: PyTypeObject = PyTypeObject::ZERO;
pub fn bytes_type() -> *mut PyTypeObject {
    &raw mut PyBytes_Type
}
pub fn is_bytes(o: &PyObject) -> bool {
    o.ty().tp_flags & Py_TPFLAGS_BYTES_SUBCLASS != 0
}
pub fn is_bytes_exact(o: &PyObject) -> bool {
    o.is_type(bytes_type())
}
pub const HEADER: usize = std::mem::offset_of!(PyBytesObject, ob_sval);

/// Largest bytes size we accept (keeps size computations from overflowing).
pub const MAX_SIZE: usize = (isize::MAX as usize) - HEADER - 64;

/// Allocate an uninitialised (zeroed) exact bytes object of size `n`, or raise MemoryError.
pub fn try_alloc_bytes(n: usize) -> PyResult<PyRef> {
    if n > MAX_SIZE {
        return Err(memory_error());
    }
    unsafe {
        let op = alloc::alloc_object(bytes_type(), HEADER + n + 1);
        if op.is_null() {
            return Err(memory_error());
        }
        set_var_size(op, n as isize);
        (*(op as *mut PyBytesObject)).ob_shash = -1;
        Ok(PyRef::from_raw(op))
    }
}

/// Allocate a zeroed exact bytes object of size `n` (aborts on allocation failure).
pub fn alloc_bytes(n: usize) -> PyRef {
    match try_alloc_bytes(n) {
        Ok(r) => r,
        Err(_) => {
            clear_exc();
            std::alloc::handle_alloc_error(std::alloc::Layout::from_size_align(n.max(1), 8).unwrap())
        }
    }
}
/// The shared (immortal) empty bytes object: every empty bytes result is this one.
pub fn empty_bytes() -> PyRef {
    static EMPTY: std::sync::atomic::AtomicPtr<PyObject> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
    use std::sync::atomic::Ordering::{AcqRel, Acquire};
    let p = EMPTY.load(Acquire);
    if !p.is_null() {
        return unsafe { PyRef::from_borrowed(p) };
    }
    let r = alloc_bytes(0);
    let op = r.into_raw();
    unsafe {
        crate::object::make_immortal(op);
        match EMPTY.compare_exchange(std::ptr::null_mut(), op, AcqRel, Acquire) {
            Ok(_) => PyRef::from_raw(op),
            // another thread won the race; the loser stays allocated (immortal)
            Err(o) => PyRef::from_borrowed(o),
        }
    }
}
/// The shared (immortal) one-byte bytes object for `c`.
pub fn single_byte(c: u8) -> PyRef {
    static CACHE: [std::sync::atomic::AtomicPtr<PyObject>; 256] = [const { std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()) }; 256];
    use std::sync::atomic::Ordering::{AcqRel, Acquire};
    let slot = &CACHE[c as usize];
    let p = slot.load(Acquire);
    if !p.is_null() {
        return unsafe { PyRef::from_borrowed(p) };
    }
    let r = alloc_bytes(1);
    unsafe {
        *bytes_ptr(&r) = c;
        let op = r.into_raw();
        crate::object::make_immortal(op);
        match slot.compare_exchange(std::ptr::null_mut(), op, AcqRel, Acquire) {
            Ok(_) => PyRef::from_raw(op),
            Err(o) => PyRef::from_borrowed(o),
        }
    }
}
pub fn new_bytes(b: &[u8]) -> PyRef {
    if b.len() <= 1 {
        return if b.is_empty() { empty_bytes() } else { single_byte(b[0]) };
    }
    let r = alloc_bytes(b.len());
    unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), bytes_ptr(&r), b.len()) };
    r
}
pub fn try_new_bytes(b: &[u8]) -> PyResult<PyRef> {
    if b.len() <= 1 {
        return Ok(if b.is_empty() { empty_bytes() } else { single_byte(b[0]) });
    }
    let r = try_alloc_bytes(b.len())?;
    unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), bytes_ptr(&r), b.len()) };
    Ok(r)
}
/// New instance of `tp` (bytes or a subclass) holding `b`.
pub fn new_bytes_of_type(tp: *mut PyTypeObject, b: &[u8]) -> PyResult<PyRef> {
    if tp == bytes_type() {
        return try_new_bytes(b);
    }
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(crate::types::typeobject::generic_alloc);
        let op = alloc(tp, b.len() as isize + 1);
        if op.is_null() {
            if !err_occurred() {
                return Err(memory_error());
            }
            return Err(PyErr);
        }
        set_var_size(op, b.len() as isize);
        (*(op as *mut PyBytesObject)).ob_shash = -1;
        let p = bytes_ptr(&*op);
        std::ptr::copy_nonoverlapping(b.as_ptr(), p, b.len());
        *p.add(b.len()) = 0;
        Ok(PyRef::from_raw(op))
    }
}
#[inline]
pub fn bytes_ptr(o: &PyObject) -> *mut u8 {
    unsafe { (o.as_ptr() as *mut u8).add(HEADER) }
}
#[inline]
pub fn bytes_len(o: &PyObject) -> usize {
    var_size(o) as usize
}
#[inline]
pub fn bytes_data(o: &PyObject) -> &[u8] {
    unsafe { std::slice::from_raw_parts(bytes_ptr(o), var_size(o) as usize) }
}
/// Data of any bytes-like object (bytes, bytearray, memoryview...) as an owned Vec.
pub fn bytes_like_data(o: &PyObject) -> Option<Vec<u8>> {
    if is_bytes(o) {
        return Some(bytes_data(o).to_vec());
    }
    if crate::types::bytearray::is_bytearray(o) {
        return Some(crate::types::bytearray::bytearray_data(o).to_vec());
    }
    crate::buffer::buffer_bytes(o)
}
/// Hash of a bytes object (cached in ob_shash).
pub fn bytes_hash(o: &PyObject) -> isize {
    unsafe {
        let p = o.as_ptr() as *mut PyBytesObject;
        let h = std::ptr::read_volatile(&raw const (*p).ob_shash);
        if h != -1 {
            return h;
        }
        let h = crate::hash::hash_bytes(bytes_data(o));
        std::ptr::write_volatile(&raw mut (*p).ob_shash, h);
        h
    }
}
/// Shrink or grow a uniquely referenced exact bytes object in place
/// (_PyBytes_Resize). Returns the (possibly moved) object, or the original
/// object (unchanged) with MemoryError set.
pub unsafe fn resize_unique(o: PyRef, n: usize) -> Result<PyRef, PyRef> {
    unsafe {
        if n > MAX_SIZE {
            let _ = memory_error();
            return Err(o);
        }
        let op = o.into_raw();
        let np = alloc::realloc_object(op, HEADER + n + 1);
        if np.is_null() {
            let _ = memory_error();
            return Err(PyRef::from_raw(op));
        }
        set_var_size(np, n as isize);
        (*(np as *mut PyBytesObject)).ob_shash = -1;
        *bytes_ptr(&*np).add(n) = 0;
        Ok(PyRef::from_raw(np))
    }
}
