//! pybuffer.h / memoryobject.h / picklebufobject.h: the buffer protocol.

use super::*;
use crate::abstract_ as ab;
use crate::types::typeobject::type_obj;
use crate::types::{bytearray, bytes, tuple};
use std::ffi::c_void;

pub const PYBUF_WRITABLE: c_int = 0x0001;
pub const PYBUF_FORMAT: c_int = 0x0004;
pub const PYBUF_ND: c_int = 0x0008;
pub const PYBUF_STRIDES: c_int = 0x0010 | PYBUF_ND;
pub const PYBUF_C_CONTIGUOUS: c_int = 0x0020 | PYBUF_STRIDES;
pub const PYBUF_F_CONTIGUOUS: c_int = 0x0040 | PYBUF_STRIDES;
pub const PYBUF_ANY_CONTIGUOUS: c_int = 0x0080 | PYBUF_STRIDES;
pub const PYBUF_INDIRECT: c_int = 0x0100 | PYBUF_STRIDES;
pub const PYBUF_READ: c_int = 0x100;
pub const PYBUF_WRITE: c_int = 0x200;

fn has_slot(o: &PyObject) -> bool {
    let bp = o.ty().tp_as_buffer;
    !bp.is_null() && unsafe { (*bp).bf_getbuffer.is_some() }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_CheckBuffer(o: *mut PyObject) -> c_int {
    unsafe { (has_slot(&*o) || bytes::is_bytes(&*o) || bytearray::is_bytearray(&*o)) as c_int }
}

/// Acquire a buffer: the type's bf_getbuffer, or built-in support for bytes/bytearray
/// while their types do not expose the slot themselves.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GetBuffer(o: *mut PyObject, view: *mut Py_buffer, flags: c_int) -> c_int {
    unsafe {
        let ob = &*o;
        let bp = ob.ty().tp_as_buffer;
        if !bp.is_null() {
            if let Some(f) = (*bp).bf_getbuffer {
                return f(o, view, flags);
            }
        }
        if bytes::is_bytes(ob) {
            return PyBuffer_FillInfo(view, o, bytes::bytes_ptr(ob) as *mut c_void, crate::object::var_size(ob), 1, flags);
        }
        if bytearray::is_bytearray(ob) {
            let b = &mut *(o as *mut PyByteArrayObject);
            let p = if b.ob_start.is_null() { c"".as_ptr() as *mut u8 } else { b.ob_start };
            let r = PyBuffer_FillInfo(view, o, p as *mut c_void, b.ob_base.ob_size, 0, flags);
            if r == 0 {
                b.ob_exports += 1;
            }
            return r;
        }
        let _ = type_error(&format!("a bytes-like object is required, not '{}'", tname(ob)));
        -1
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBuffer_Release(view: *mut Py_buffer) {
    unsafe {
        let o = (*view).obj;
        if o.is_null() {
            return;
        }
        let bp = (*o).ty().tp_as_buffer;
        if !bp.is_null() {
            if let Some(r) = (*bp).bf_releasebuffer {
                r(o, view);
            }
        } else if bytearray::is_bytearray(&*o) {
            let b = &mut *(o as *mut PyByteArrayObject);
            b.ob_exports -= 1;
        }
        (*view).obj = std::ptr::null_mut();
        decref(o);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBuffer_FillInfo(view: *mut Py_buffer, obj: *mut PyObject, buf: *mut c_void, len: Py_ssize_t, readonly: c_int, flags: c_int) -> c_int {
    unsafe {
        if view.is_null() {
            let _ = value_error("PyBuffer_FillInfo: view==NULL argument is obsolete");
            return -1;
        }
        if flags != -1 && (flags & PYBUF_WRITABLE) == PYBUF_WRITABLE && readonly == 1 {
            let _ = crate::err!(BufferError, "Object is not writable.");
            return -1;
        }
        (*view).obj = obj;
        xincref(obj);
        (*view).buf = buf;
        (*view).len = len;
        (*view).readonly = readonly;
        (*view).itemsize = 1;
        (*view).format = if flags != -1 && flags & PYBUF_FORMAT == PYBUF_FORMAT { c"B".as_ptr() as *mut c_char } else { std::ptr::null_mut() };
        (*view).ndim = 1;
        (*view).shape = if flags != -1 && flags & PYBUF_ND == PYBUF_ND { &raw mut (*view).len } else { std::ptr::null_mut() };
        (*view).strides = if flags != -1 && flags & PYBUF_STRIDES == PYBUF_STRIDES { &raw mut (*view).itemsize } else { std::ptr::null_mut() };
        (*view).suboffsets = std::ptr::null_mut();
        (*view).internal = std::ptr::null_mut();
        0
    }
}

unsafe fn is_c_contig(v: &Py_buffer) -> bool {
    unsafe {
        if v.len == 0 || v.strides.is_null() {
            return true;
        }
        let mut sd = v.itemsize;
        for i in (0..v.ndim as usize).rev() {
            let dim = *v.shape.add(i);
            if dim > 1 && *v.strides.add(i) != sd {
                return false;
            }
            sd *= dim;
        }
        true
    }
}
unsafe fn is_f_contig(v: &Py_buffer) -> bool {
    unsafe {
        if v.len == 0 {
            return true;
        }
        if v.strides.is_null() {
            if v.ndim <= 1 {
                return true;
            }
            let mut n = 0;
            for i in 0..v.ndim as usize {
                if *v.shape.add(i) > 1 {
                    n += 1;
                }
            }
            return n <= 1;
        }
        let mut sd = v.itemsize;
        for i in 0..v.ndim as usize {
            let dim = *v.shape.add(i);
            if dim > 1 && *v.strides.add(i) != sd {
                return false;
            }
            sd *= dim;
        }
        true
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBuffer_IsContiguous(v: *const Py_buffer, order: c_char) -> c_int {
    unsafe {
        let v = &*v;
        if !v.suboffsets.is_null() {
            return 0;
        }
        (match order as u8 {
            b'C' => is_c_contig(v),
            b'F' => is_f_contig(v),
            b'A' => is_c_contig(v) || is_f_contig(v),
            _ => false,
        }) as c_int
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBuffer_GetPointer(v: *const Py_buffer, indices: *const Py_ssize_t) -> *mut c_void {
    unsafe {
        let v = &*v;
        let mut p = v.buf as *mut u8;
        for i in 0..v.ndim as usize {
            let stride = if v.strides.is_null() {
                let mut s = v.itemsize;
                for j in i + 1..v.ndim as usize {
                    s *= *v.shape.add(j);
                }
                s
            } else {
                *v.strides.add(i)
            };
            p = p.offset(stride * *indices.add(i));
            if !v.suboffsets.is_null() && *v.suboffsets.add(i) >= 0 {
                p = (*(p as *mut *mut u8)).offset(*v.suboffsets.add(i));
            }
        }
        p as *mut c_void
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBuffer_SizeFromFormat(format: *const c_char) -> Py_ssize_t {
    unsafe {
        ret_n((|| -> PyResult<usize> {
            let st = crate::import::import_absolute("struct")?;
            let f = ab::getattr_str(&st, "calcsize")?;
            let fmt = if format.is_null() { crate::types::str::new_str("B") } else { cstr_obj(format)? };
            let r = ab::call(&f, &[&fmt])?;
            Ok(crate::types::int::as_i64(&r).unwrap_or(0) as usize)
        })())
    }
}
/// Iterate over every item address of a buffer in C or Fortran order.
unsafe fn for_each_item(v: &Py_buffer, fortran: bool, mut f: impl FnMut(*mut u8)) {
    unsafe {
        let nd = v.ndim.max(0) as usize;
        if nd == 0 {
            f(v.buf as *mut u8);
            return;
        }
        let shape: Vec<isize> = (0..nd).map(|i| if v.shape.is_null() { v.len / v.itemsize } else { *v.shape.add(i) }).collect();
        if shape.iter().any(|&s| s == 0) {
            return;
        }
        let mut idx = vec![0isize; nd];
        loop {
            f(PyBuffer_GetPointer(v, idx.as_ptr()) as *mut u8);
            // increment
            let mut k = if fortran { 0isize } else { nd as isize - 1 };
            loop {
                if k < 0 || k as usize >= nd {
                    return;
                }
                let ku = k as usize;
                idx[ku] += 1;
                if idx[ku] < shape[ku] {
                    break;
                }
                idx[ku] = 0;
                k += if fortran { 1 } else { -1 };
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBuffer_ToContiguous(buf: *mut c_void, src: *const Py_buffer, len: Py_ssize_t, order: c_char) -> c_int {
    unsafe {
        let v = &*src;
        if len != v.len {
            let _ = value_error("PyBuffer_ToContiguous: len != view->len");
            return -1;
        }
        if PyBuffer_IsContiguous(src, order) != 0 {
            std::ptr::copy(v.buf as *const u8, buf as *mut u8, len as usize);
            return 0;
        }
        let mut out = buf as *mut u8;
        let item = v.itemsize as usize;
        for_each_item(v, order as u8 == b'F', |p| {
            std::ptr::copy(p, out, item);
            out = out.add(item);
        });
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBuffer_FromContiguous(view: *const Py_buffer, buf: *const c_void, len: Py_ssize_t, order: c_char) -> c_int {
    unsafe {
        let v = &*view;
        let len = len.min(v.len);
        if PyBuffer_IsContiguous(view, order) != 0 {
            std::ptr::copy(buf as *const u8, v.buf as *mut u8, len as usize);
            return 0;
        }
        let mut inp = buf as *const u8;
        let end = inp.add(len as usize);
        let item = v.itemsize as usize;
        for_each_item(v, order as u8 == b'F', |p| {
            if inp < end {
                std::ptr::copy(inp, p, item);
                inp = inp.add(item);
            }
        });
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBuffer_FillContiguousStrides(nd: c_int, shape: *mut Py_ssize_t, strides: *mut Py_ssize_t, itemsize: c_int, order: c_char) {
    unsafe {
        let mut sd = itemsize as isize;
        if order as u8 == b'F' {
            for k in 0..nd as usize {
                *strides.add(k) = sd;
                sd *= *shape.add(k);
            }
        } else {
            for k in (0..nd as usize).rev() {
                *strides.add(k) = sd;
                sd *= *shape.add(k);
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_CopyData(dest: *mut PyObject, src: *mut PyObject) -> c_int {
    unsafe {
        let mut vd: Py_buffer = std::mem::zeroed();
        let mut vs: Py_buffer = std::mem::zeroed();
        if PyObject_GetBuffer(dest, &mut vd, PYBUF_WRITABLE | PYBUF_STRIDES | PYBUF_FORMAT) < 0 {
            return -1;
        }
        if PyObject_GetBuffer(src, &mut vs, PYBUF_STRIDES | PYBUF_FORMAT) < 0 {
            PyBuffer_Release(&mut vd);
            return -1;
        }
        if vd.len < vs.len {
            let _ = crate::err!(BufferError, "destination is too small to receive data from source");
            PyBuffer_Release(&mut vd);
            PyBuffer_Release(&mut vs);
            return -1;
        }
        let mut tmp = vec![0u8; vs.len as usize];
        PyBuffer_ToContiguous(tmp.as_mut_ptr() as *mut c_void, &vs, vs.len, b'C' as c_char);
        PyBuffer_FromContiguous(&vd, tmp.as_ptr() as *const c_void, vs.len, b'C' as c_char);
        PyBuffer_Release(&mut vd);
        PyBuffer_Release(&mut vs);
        0
    }
}

// ---------------------------------------------------------------- raw memory exporter

/// A tiny exporter object owning a `Py_buffer` description (used by
/// PyMemoryView_FromMemory / FromBuffer): memoryview(exporter) re-exports it.
#[repr(C)]
struct RawBuffer {
    ob_base: PyObject,
    view: Py_buffer,
    shape: [Py_ssize_t; 64],
    strides: [Py_ssize_t; 64],
    format: [u8; 64],
}
static mut RAWBUF_TYPE: PyTypeObject = PyTypeObject::ZERO;
static mut RAWBUF_PROCS: PyBufferProcs = PyBufferProcs::ZERO;

unsafe extern "C" fn rawbuf_get(o: *mut PyObject, view: *mut Py_buffer, flags: c_int) -> c_int {
    unsafe {
        let r = &mut *(o as *mut RawBuffer);
        if flags & PYBUF_WRITABLE != 0 && r.view.readonly != 0 {
            let _ = crate::err!(BufferError, "Object is not writable.");
            return -1;
        }
        std::ptr::copy_nonoverlapping(&r.view, view, 1);
        (*view).obj = o;
        incref(o);
        0
    }
}
unsafe extern "C" fn rawbuf_dealloc(o: *mut PyObject) {
    unsafe { crate::alloc::free_object(o) }
}
fn rawbuf_type() -> *mut PyTypeObject {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        let t = &mut *(&raw mut RAWBUF_TYPE);
        t.tp_name = c"_capi_buffer".as_ptr();
        t.tp_basicsize = std::mem::size_of::<RawBuffer>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT;
        t.tp_dealloc = Some(rawbuf_dealloc);
        (*(&raw mut RAWBUF_PROCS)).bf_getbuffer = Some(rawbuf_get);
        t.tp_as_buffer = &raw mut RAWBUF_PROCS;
        let _ = crate::types::typeobject::type_ready(t);
    });
    &raw mut RAWBUF_TYPE
}
unsafe fn raw_exporter(src: &Py_buffer) -> PyRef {
    unsafe {
        let tp = rawbuf_type();
        let op = crate::alloc::alloc_object(tp, std::mem::size_of::<RawBuffer>());
        let r = &mut *(op as *mut RawBuffer);
        std::ptr::copy_nonoverlapping(src, &mut r.view, 1);
        r.view.obj = std::ptr::null_mut();
        let nd = (src.ndim.max(0) as usize).min(64);
        if !src.shape.is_null() {
            std::ptr::copy_nonoverlapping(src.shape, r.shape.as_mut_ptr(), nd);
            r.view.shape = r.shape.as_mut_ptr();
        }
        if !src.strides.is_null() {
            std::ptr::copy_nonoverlapping(src.strides, r.strides.as_mut_ptr(), nd);
            r.view.strides = r.strides.as_mut_ptr();
        }
        if !src.format.is_null() {
            let f = CStr::from_ptr(src.format).to_bytes();
            let n = f.len().min(63);
            r.format[..n].copy_from_slice(&f[..n]);
            r.view.format = r.format.as_mut_ptr() as *mut c_char;
        }
        PyRef::from_raw(op)
    }
}
fn memoryview_type() -> PyResult {
    let b = crate::builtins::builtins_module();
    ab::getattr_str(&b, "memoryview")
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMemoryView_FromObject(o: *mut PyObject) -> *mut PyObject {
    unsafe { ret(memoryview_type().and_then(|m| ab::call(&m, &[&*o]))) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMemoryView_FromMemory(mem: *mut c_char, size: Py_ssize_t, flags: c_int) -> *mut PyObject {
    unsafe {
        let mut v: Py_buffer = std::mem::zeroed();
        PyBuffer_FillInfo(&mut v, std::ptr::null_mut(), mem as *mut c_void, size, (flags == PYBUF_READ) as c_int, PYBUF_FORMAT | PYBUF_STRIDES);
        let e = raw_exporter(&v);
        ret(memoryview_type().and_then(|m| ab::call(&m, &[&e])))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMemoryView_FromBuffer(info: *const Py_buffer) -> *mut PyObject {
    unsafe {
        if (*info).buf.is_null() {
            let _ = value_error("PyMemoryView_FromBuffer(): info->buf must not be NULL");
            return std::ptr::null_mut();
        }
        let e = raw_exporter(&*info);
        ret(memoryview_type().and_then(|m| ab::call(&m, &[&e])))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMemoryView_GetContiguous(o: *mut PyObject, buffertype: c_int, order: c_char) -> *mut PyObject {
    unsafe {
        let mv = PyMemoryView_FromObject(o);
        if mv.is_null() {
            return mv;
        }
        let mv = PyRef::from_raw(mv);
        if buffertype == PYBUF_WRITE {
            let ro = ab::getattr_str(&mv, "readonly").and_then(|r| ab::is_true(&r)).unwrap_or(true);
            if ro {
                let _ = crate::err!(BufferError, "underlying buffer is not writable");
                return std::ptr::null_mut();
            }
        }
        let c = ab::getattr_str(&mv, if order as u8 == b'F' { "f_contiguous" } else { "c_contiguous" }).and_then(|r| ab::is_true(&r));
        match c {
            Ok(true) => mv.into_raw(),
            Ok(false) => {
                if buffertype == PYBUF_WRITE {
                    let _ = crate::err!(BufferError, "writable contiguous buffer requested for a non-contiguous object.");
                    return std::ptr::null_mut();
                }
                let o2 = ab::call_method_str(&mv, "tobytes", &[]);
                ret(o2.and_then(|b| memoryview_type().and_then(|m| ab::call(&m, &[&b]))))
            }
            Err(_) => std::ptr::null_mut(),
        }
    }
}

// ---------------------------------------------------------------- PickleBuffer

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyPickleBuffer_FromObject(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        ret((|| -> PyResult {
            let p = crate::import::import_absolute("pickle")?;
            let pb = ab::getattr_str(&p, "PickleBuffer")?;
            ab::call(&pb, &[&*o])
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyPickleBuffer_GetBuffer(o: *mut PyObject) -> *const Py_buffer {
    unsafe {
        // keep a long-lived view per PickleBuffer object (released by PyPickleBuffer_Release)
        static VIEWS: std::sync::Mutex<Vec<(usize, usize)>> = std::sync::Mutex::new(Vec::new());
        let mut g = VIEWS.lock().unwrap();
        if let Some(&(_, v)) = g.iter().find(|(k, _)| *k == o as usize) {
            return v as *const Py_buffer;
        }
        let v = Box::into_raw(Box::new(std::mem::zeroed::<Py_buffer>()));
        if PyObject_GetBuffer(o, v, PYBUF_FORMAT | PYBUF_STRIDES) < 0 {
            drop(Box::from_raw(v));
            return std::ptr::null();
        }
        g.push((o as usize, v as usize));
        v
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyPickleBuffer_Release(o: *mut PyObject) -> c_int {
    unsafe { ret_i(ab::call_method_str(&*o, "release", &[]).map(|_| ())) }
}

#[allow(dead_code)]
fn _unused() -> (&'static PyObject, PyRef) {
    (type_obj(bytes::bytes_type()), tuple::empty_tuple())
}
