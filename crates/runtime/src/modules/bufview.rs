//! RAII access to the buffer protocol (PyObject_GetBuffer / PyBuffer_Release) for
//! Rust builtins: `BufView::get(o, writable)`.
use crate::errors::*;
use crate::ffi::*;
use crate::raise;
use crate::types::typeobject::tname;
use crate::types::{bytearray, bytes};

pub const PYBUF_SIMPLE: i32 = 0;
pub const PYBUF_WRITABLE: i32 = 0x0001;
pub const PYBUF_FORMAT: i32 = 0x0004;
pub const PYBUF_ND: i32 = 0x0008;
pub const PYBUF_STRIDES: i32 = 0x0010 | PYBUF_ND;
pub const PYBUF_C_CONTIGUOUS: i32 = 0x0020 | PYBUF_STRIDES;
pub const PYBUF_ANY_CONTIGUOUS: i32 = 0x0080 | PYBUF_STRIDES;
pub const PYBUF_INDIRECT: i32 = 0x0100 | PYBUF_STRIDES;
pub const PYBUF_FULL_RO: i32 = PYBUF_INDIRECT | PYBUF_FORMAT;
pub const PYBUF_FULL: i32 = PYBUF_INDIRECT | PYBUF_FORMAT | PYBUF_WRITABLE;
pub const PYBUF_RECORDS: i32 = PYBUF_STRIDES | PYBUF_WRITABLE | PYBUF_FORMAT;

pub struct BufView {
    view: Py_buffer,
    /// Fast path: borrowed data of an exact bytes object (no release needed).
    direct: bool,
}

impl BufView {
    pub fn has_buffer(o: &PyObject) -> bool {
        let bp = o.ty().tp_as_buffer;
        bytes::is_bytes(o) || bytearray::is_bytearray(o) || (!bp.is_null() && unsafe { (*bp).bf_getbuffer.is_some() })
    }

    /// Acquire a contiguous buffer. Error message mimics the `y*` / `w*` converters.
    pub fn get(o: &PyObject, writable: bool) -> PyResult<BufView> {
        Self::get_msg(o, writable, None)
    }

    /// Like `get`, with a custom TypeError message prefix (e.g. "readinto() argument").
    pub fn get_msg(o: &PyObject, writable: bool, what: Option<&str>) -> PyResult<BufView> {
        if !writable && bytes::is_bytes(o) && o.ty().tp_as_buffer.is_null() {
            let d = bytes::bytes_data(o);
            let mut view: Py_buffer = unsafe { std::mem::zeroed() };
            view.buf = d.as_ptr() as *mut _;
            view.len = d.len() as isize;
            view.readonly = 1;
            view.itemsize = 1;
            return Ok(BufView { view, direct: true });
        }
        let bp = o.ty().tp_as_buffer;
        unsafe {
            if bp.is_null() || (*bp).bf_getbuffer.is_none() {
                if bytearray::is_bytearray(o) {
                    let d = bytearray::bytearray_data(o);
                    let mut view: Py_buffer = std::mem::zeroed();
                    view.buf = d.as_ptr() as *mut _;
                    view.len = d.len() as isize;
                    view.itemsize = 1;
                    return Ok(BufView { view, direct: true });
                }
                if bytes::is_bytes(o) && !writable {
                    let d = bytes::bytes_data(o);
                    let mut view: Py_buffer = std::mem::zeroed();
                    view.buf = d.as_ptr() as *mut _;
                    view.len = d.len() as isize;
                    view.readonly = 1;
                    view.itemsize = 1;
                    return Ok(BufView { view, direct: true });
                }
                return Err(type_err(o, writable, what));
            }
            let mut view: Py_buffer = std::mem::zeroed();
            let flags = if writable { PYBUF_WRITABLE | PYBUF_C_CONTIGUOUS } else { PYBUF_C_CONTIGUOUS };
            if (*bp).bf_getbuffer.unwrap()(o.as_ptr(), &mut view, flags) < 0 {
                if writable && crate::errors::exc_matches(crate::types::exceptions::BufferError()) {
                    crate::errors::clear_exc();
                    return Err(type_err(o, writable, what));
                }
                return Err(PyErr);
            }
            Ok(BufView { view, direct: false })
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.view.len as usize
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.view.len == 0
    }
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        if self.view.len == 0 {
            return &[];
        }
        unsafe { std::slice::from_raw_parts(self.view.buf as *const u8, self.view.len as usize) }
    }
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        if self.view.len == 0 {
            return &mut [];
        }
        unsafe { std::slice::from_raw_parts_mut(self.view.buf as *mut u8, self.view.len as usize) }
    }
    pub fn readonly(&self) -> bool {
        self.view.readonly != 0
    }
}

fn type_err(o: &PyObject, writable: bool, what: Option<&str>) -> PyErr {
    let prefix = what.map(|w| format!("{} must be ", w)).unwrap_or_default();
    if writable {
        if what.is_some() {
            err!(TypeError, "{}read-write bytes-like object, not {}", prefix, tname(o))
        } else {
            err!(TypeError, "read-write bytes-like object is required, not '{}'", tname(o))
        }
    } else if what.is_some() {
        err!(TypeError, "{}bytes-like object, not {}", prefix, tname(o))
    } else {
        err!(TypeError, "a bytes-like object is required, not '{}'", tname(o))
    }
}

impl Drop for BufView {
    fn drop(&mut self) {
        if self.direct {
            return;
        }
        unsafe {
            let o = self.view.obj;
            if !o.is_null() {
                let bp = (*o).ty().tp_as_buffer;
                if !bp.is_null() {
                    if let Some(r) = (*bp).bf_releasebuffer {
                        r(o, &mut self.view);
                    }
                }
                crate::object::decref(o);
                self.view.obj = std::ptr::null_mut();
            }
        }
    }
}

/// Shorthand: copy of the bytes of a bytes-like object, with the standard error.
pub fn bytes_like(o: &PyObject) -> PyResult<Vec<u8>> {
    Ok(BufView::get(o, false)?.as_slice().to_vec())
}

pub fn require_buffer(o: &PyObject) -> PyResult<()> {
    if !BufView::has_buffer(o) {
        raise!(TypeError, "a bytes-like object is required, not '{}'", tname(o));
    }
    Ok(())
}
