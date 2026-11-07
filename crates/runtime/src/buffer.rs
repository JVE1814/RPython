//! PEP 3118 buffer protocol helpers (the Rust side of PyObject_GetBuffer,
//! PyBuffer_Release, PyBuffer_FillInfo, PyBuffer_IsContiguous, ...). The C API
//! workstream exports thin `extern "C"` wrappers around these.
#![allow(non_upper_case_globals)]

use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::tname;
use std::ffi::{c_char, c_int, c_void};

pub const PyBUF_SIMPLE: c_int = 0;
pub const PyBUF_WRITABLE: c_int = 0x0001;
pub const PyBUF_FORMAT: c_int = 0x0004;
pub const PyBUF_ND: c_int = 0x0008;
pub const PyBUF_STRIDES: c_int = 0x0010 | PyBUF_ND;
pub const PyBUF_C_CONTIGUOUS: c_int = 0x0020 | PyBUF_STRIDES;
pub const PyBUF_F_CONTIGUOUS: c_int = 0x0040 | PyBUF_STRIDES;
pub const PyBUF_ANY_CONTIGUOUS: c_int = 0x0080 | PyBUF_STRIDES;
pub const PyBUF_INDIRECT: c_int = 0x0100 | PyBUF_STRIDES;
pub const PyBUF_CONTIG: c_int = PyBUF_ND | PyBUF_WRITABLE;
pub const PyBUF_CONTIG_RO: c_int = PyBUF_ND;
pub const PyBUF_STRIDED: c_int = PyBUF_STRIDES | PyBUF_WRITABLE;
pub const PyBUF_STRIDED_RO: c_int = PyBUF_STRIDES;
pub const PyBUF_RECORDS: c_int = PyBUF_STRIDES | PyBUF_WRITABLE | PyBUF_FORMAT;
pub const PyBUF_RECORDS_RO: c_int = PyBUF_STRIDES | PyBUF_FORMAT;
pub const PyBUF_FULL: c_int = PyBUF_INDIRECT | PyBUF_WRITABLE | PyBUF_FORMAT;
pub const PyBUF_FULL_RO: c_int = PyBUF_INDIRECT | PyBUF_FORMAT;
pub const PyBUF_READ: c_int = 0x100;
pub const PyBUF_WRITE: c_int = 0x200;
pub const PyBUF_MAX_NDIM: usize = 64;

/// An all-zero Py_buffer.
pub fn empty_view() -> Py_buffer {
    unsafe { std::mem::zeroed() }
}

/// PyObject_CheckBuffer.
pub fn check_buffer(o: &PyObject) -> bool {
    let bp = o.ty().tp_as_buffer;
    !bp.is_null() && unsafe { (*bp).bf_getbuffer.is_some() }
}

fn bad_internal_call() -> PyErr {
    system_error("bad argument to internal function")
}

/// PyObject_GetBuffer.
pub fn get_buffer(o: &PyObject, view: &mut Py_buffer, flags: c_int) -> PyResult<()> {
    if flags == PyBUF_READ || flags == PyBUF_WRITE {
        return Err(bad_internal_call());
    }
    let bp = o.ty().tp_as_buffer;
    let f = if bp.is_null() { None } else { unsafe { (*bp).bf_getbuffer } };
    match f {
        None => raise!(TypeError, "a bytes-like object is required, not '{}'", tname(o)),
        Some(f) => {
            if unsafe { f(o.as_ptr(), view, flags) } < 0 {
                // a failing provider may leave junk in view.obj (old providers did);
                // nothing was exported, so it must not be released later
                view.obj = std::ptr::null_mut();
                if !err_occurred() {
                    return Err(system_error("getbuffer returned error without setting an exception"));
                }
                return Err(PyErr);
            }
            Ok(())
        }
    }
}

/// PyBuffer_Release. Safe to call with a pending exception (it is preserved).
pub unsafe fn release_buffer(view: *mut Py_buffer) {
    unsafe {
        let obj = (*view).obj;
        if obj.is_null() {
            return;
        }
        let bp = (*obj).ty().tp_as_buffer;
        if !bp.is_null() {
            if let Some(r) = (*bp).bf_releasebuffer {
                r(obj, view);
            }
        }
        (*view).obj = std::ptr::null_mut();
        decref(obj);
    }
}

/// PyBuffer_FillInfo.
pub unsafe fn fill_info(view: *mut Py_buffer, obj: *mut PyObject, buf: *mut c_void, len: isize, readonly: bool, flags: c_int) -> PyResult<()> {
    unsafe {
        if view.is_null() {
            raise!(BufferError, "PyBuffer_FillInfo: view==NULL argument is obsolete");
        }
        if flags != PyBUF_SIMPLE && (flags == PyBUF_READ || flags == PyBUF_WRITE) {
            return Err(bad_internal_call());
        }
        if flags & PyBUF_WRITABLE == PyBUF_WRITABLE && readonly {
            raise!(BufferError, "Object is not writable.");
        }
        let v = &mut *view;
        xincref(obj);
        v.obj = obj;
        v.buf = buf;
        v.len = len;
        v.readonly = readonly as c_int;
        v.itemsize = 1;
        v.format = if flags & PyBUF_FORMAT == PyBUF_FORMAT { c"B".as_ptr() as *mut c_char } else { std::ptr::null_mut() };
        v.ndim = 1;
        v.shape = if flags & PyBUF_ND == PyBUF_ND { &raw mut v.len } else { std::ptr::null_mut() };
        v.strides = if flags & PyBUF_STRIDES == PyBUF_STRIDES { &raw mut v.itemsize } else { std::ptr::null_mut() };
        v.suboffsets = std::ptr::null_mut();
        v.internal = std::ptr::null_mut();
        Ok(())
    }
}

/// RAII owner of an acquired buffer.
pub struct BufferView {
    pub view: Box<Py_buffer>, // boxed: exporters may store pointers into the view itself
}

impl BufferView {
    pub fn get(o: &PyObject, flags: c_int) -> PyResult<BufferView> {
        let mut b = BufferView { view: Box::new(empty_view()) };
        get_buffer(o, &mut b.view, flags)?;
        Ok(b)
    }
    /// Acquire a simple (C-contiguous, 1-byte) buffer.
    pub fn simple(o: &PyObject) -> PyResult<BufferView> {
        Self::get(o, PyBUF_SIMPLE)
    }
    pub fn len(&self) -> usize {
        self.view.len as usize
    }
    /// The data; only valid for contiguous buffers (always true for SIMPLE requests).
    pub fn as_bytes(&self) -> &[u8] {
        if self.view.len == 0 || self.view.buf.is_null() {
            return &[];
        }
        unsafe { std::slice::from_raw_parts(self.view.buf as *const u8, self.view.len as usize) }
    }
    /// Contents in C order (copies; handles non-contiguous buffers).
    pub fn to_vec(&self) -> Vec<u8> {
        if is_contiguous(&self.view, b'C') {
            return self.as_bytes().to_vec();
        }
        let mut out = vec![0u8; self.view.len as usize];
        unsafe { copy_to_contiguous(out.as_mut_ptr(), &self.view, b'C') };
        out
    }
}

impl Drop for BufferView {
    fn drop(&mut self) {
        unsafe { release_buffer(&mut *self.view) }
    }
}

/// Copy the contents of any object supporting the buffer protocol.
/// Returns None (with no exception set) if `o` is not a buffer provider or the
/// request fails.
pub fn buffer_bytes(o: &PyObject) -> Option<Vec<u8>> {
    if !check_buffer(o) {
        return None;
    }
    match BufferView::get(o, PyBUF_FULL_RO) {
        Ok(b) => Some(b.to_vec()),
        Err(_) => {
            clear_exc();
            None
        }
    }
}

/// Run `f` on the bytes of a buffer provider (C-contiguous copy if needed).
/// Raises TypeError if `o` is not a buffer provider.
pub fn with_buffer<R>(o: &PyObject, f: impl FnOnce(&[u8]) -> R) -> PyResult<R> {
    let b = BufferView::get(o, PyBUF_SIMPLE)?;
    Ok(f(b.as_bytes()))
}

// ------------------------------------------------------------------ geometry

#[inline]
unsafe fn shape_at(v: &Py_buffer, i: usize) -> isize {
    unsafe { *v.shape.add(i) }
}
#[inline]
unsafe fn stride_at(v: &Py_buffer, i: usize) -> isize {
    unsafe { *v.strides.add(i) }
}

fn is_c_contiguous(v: &Py_buffer) -> bool {
    if v.len == 0 || v.strides.is_null() {
        return true;
    }
    unsafe {
        let mut sd = v.itemsize;
        for i in (0..v.ndim as usize).rev() {
            let dim = shape_at(v, i);
            if dim > 1 && stride_at(v, i) != sd {
                return false;
            }
            sd *= dim;
        }
    }
    true
}

fn is_f_contiguous(v: &Py_buffer) -> bool {
    if v.len == 0 {
        return true;
    }
    unsafe {
        if v.strides.is_null() {
            if v.ndim <= 1 {
                return true;
            }
            let mut n = 0;
            for i in 0..v.ndim as usize {
                if shape_at(v, i) > 1 {
                    n += 1;
                }
            }
            return n <= 1;
        }
        let mut sd = v.itemsize;
        for i in 0..v.ndim as usize {
            let dim = shape_at(v, i);
            if dim > 1 && stride_at(v, i) != sd {
                return false;
            }
            sd *= dim;
        }
    }
    true
}

/// PyBuffer_IsContiguous(view, order) with order in b'C', b'F', b'A'.
pub fn is_contiguous(v: &Py_buffer, order: u8) -> bool {
    if !v.suboffsets.is_null() {
        return false;
    }
    match order {
        b'C' => is_c_contiguous(v),
        b'F' => is_f_contiguous(v),
        b'A' => is_c_contiguous(v) || is_f_contiguous(v),
        _ => false,
    }
}

/// PyBuffer_FillContiguousStrides.
pub unsafe fn fill_contiguous_strides(ndim: usize, shape: *const isize, strides: *mut isize, itemsize: isize, order: u8) {
    unsafe {
        let mut sd = itemsize;
        if order == b'F' {
            for k in 0..ndim {
                *strides.add(k) = sd;
                sd *= *shape.add(k);
            }
        } else {
            for k in (0..ndim).rev() {
                *strides.add(k) = sd;
                sd *= *shape.add(k);
            }
        }
    }
}

#[inline]
pub unsafe fn adjust_ptr(ptr: *mut u8, suboffsets: *const isize, dim: usize) -> *mut u8 {
    unsafe {
        if !suboffsets.is_null() && *suboffsets.add(dim) >= 0 {
            (*(ptr as *mut *mut u8)).offset(*suboffsets.add(dim))
        } else {
            ptr
        }
    }
}

/// Effective (shape, strides) of a view, filling in defaults for NULL arrays.
pub fn view_geometry(v: &Py_buffer) -> (Vec<isize>, Vec<isize>) {
    let ndim = v.ndim.max(0) as usize;
    unsafe {
        let shape: Vec<isize> = if v.shape.is_null() {
            if ndim == 0 { vec![] } else { vec![v.len / v.itemsize.max(1)] }
        } else {
            (0..ndim).map(|i| shape_at(v, i)).collect()
        };
        let strides: Vec<isize> = if v.strides.is_null() {
            let mut s = vec![0isize; ndim];
            let mut sd = v.itemsize;
            for k in (0..ndim).rev() {
                s[k] = sd;
                sd *= shape[k];
            }
            s
        } else {
            (0..ndim).map(|i| stride_at(v, i)).collect()
        };
        (shape, strides)
    }
}

/// Call `f` with the address of every item of `v` in the given order
/// (b'C': last index fastest, b'F': first index fastest).
pub unsafe fn for_each_item(v: &Py_buffer, order: u8, f: &mut dyn FnMut(*mut u8)) {
    unsafe {
        let (shape, strides) = view_geometry(v);
        let ndim = shape.len();
        if ndim == 0 {
            f(v.buf as *mut u8);
            return;
        }
        if shape.iter().any(|&s| s == 0) {
            return;
        }
        let sub = v.suboffsets as *const isize;
        let mut idx = vec![0isize; ndim];
        loop {
            // compute pointer
            let mut p = v.buf as *mut u8;
            for d in 0..ndim {
                p = p.offset(strides[d] * idx[d]);
                p = adjust_ptr(p, sub, d);
            }
            f(p);
            // advance
            let mut carry = true;
            if order == b'F' {
                for d in 0..ndim {
                    idx[d] += 1;
                    if idx[d] < shape[d] {
                        carry = false;
                        break;
                    }
                    idx[d] = 0;
                }
            } else {
                for d in (0..ndim).rev() {
                    idx[d] += 1;
                    if idx[d] < shape[d] {
                        carry = false;
                        break;
                    }
                    idx[d] = 0;
                }
            }
            if carry {
                return;
            }
        }
    }
}

/// Copy the contents of `src` into `dst` (which must hold src.len bytes) laid
/// out contiguously in `order` (b'A' means C).
pub unsafe fn copy_to_contiguous(dst: *mut u8, src: &Py_buffer, order: u8) {
    unsafe {
        let o = if order == b'F' { b'F' } else { b'C' };
        if is_contiguous(src, o) || (order == b'A' && is_contiguous(src, b'A')) {
            if src.len > 0 {
                std::ptr::copy(src.buf as *const u8, dst, src.len as usize);
            }
            return;
        }
        let isz = src.itemsize as usize;
        let mut off = 0usize;
        for_each_item(src, o, &mut |p| {
            std::ptr::copy(p as *const u8, dst.add(off), isz);
            off += isz;
        });
    }
}

/// PyBuffer_ToContiguous.
pub unsafe fn to_contiguous(buf: *mut u8, src: &Py_buffer, len: isize, order: u8) -> PyResult<()> {
    if len != src.len {
        raise!(ValueError, "PyBuffer_ToContiguous: len != view->len");
    }
    unsafe { copy_to_contiguous(buf, src, order) };
    Ok(())
}

/// PyBuffer_FromContiguous: copy `len` bytes from `buf` into `view` in `order`.
pub unsafe fn from_contiguous(view: &Py_buffer, buf: *const u8, len: isize, order: u8) -> PyResult<()> {
    unsafe {
        let len = len.min(view.len).max(0) as usize;
        let o = if order == b'F' { b'F' } else { b'C' };
        if is_contiguous(view, o) {
            std::ptr::copy(buf, view.buf as *mut u8, len);
            return Ok(());
        }
        let isz = view.itemsize as usize;
        let mut off = 0usize;
        for_each_item(view, o, &mut |p| {
            if off + isz <= len {
                std::ptr::copy(buf.add(off), p, isz);
            }
            off += isz;
        });
        Ok(())
    }
}

/// PyBuffer_GetPointer.
pub unsafe fn get_pointer(view: &Py_buffer, indices: &[isize]) -> *mut u8 {
    unsafe {
        let mut p = view.buf as *mut u8;
        for (i, &ix) in indices.iter().enumerate().take(view.ndim as usize) {
            p = p.offset(stride_at(view, i) * ix);
            p = adjust_ptr(p, view.suboffsets, i);
        }
        p
    }
}

/// PyObject_CopyData(dest, src).
pub fn object_copy_data(dest: &PyObject, src: &PyObject) -> PyResult<()> {
    if !check_buffer(dest) || !check_buffer(src) {
        raise!(TypeError, "both destination and source must be bytes-like objects");
    }
    let d = BufferView::get(dest, PyBUF_FULL)?;
    let s = BufferView::get(src, PyBUF_FULL_RO)?;
    if d.view.len < s.view.len {
        raise!(BufferError, "destination is too small to receive data from source");
    }
    let data = s.to_vec();
    unsafe { from_contiguous(&d.view, data.as_ptr(), data.len() as isize, b'C') }
}

/// PyBuffer_SizeFromFormat (uses struct.calcsize).
pub fn size_from_format(format: &str) -> PyResult<isize> {
    let m = crate::import::import_absolute("struct")?;
    let f = crate::abstract_::getattr_str(&m, "calcsize")?;
    let s = crate::types::str::new_str(format);
    let r = crate::abstract_::call(&f, &[&s])?;
    crate::abstract_::index_as_ssize(&r, Some(crate::types::exceptions::OverflowError()))
}

/// Format string of a view ("B" if NULL).
pub fn view_format(v: &Py_buffer) -> String {
    if v.format.is_null() {
        return "B".into();
    }
    unsafe { std::ffi::CStr::from_ptr(v.format).to_string_lossy().into_owned() }
}
