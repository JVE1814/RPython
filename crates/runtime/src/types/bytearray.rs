//! `bytearray`: the mutable byte buffer.
//!
//! Layout (PyByteArrayObject, from the ABI): the storage is a uniquely owned,
//! exact `bytes` object (`ob_bytes_object`); `ob_bytes` points at its data,
//! `ob_alloc` is its size (the capacity), `ob_start` is the logical start
//! inside it and `ob_size` the logical length. The data is always followed by
//! a NUL byte. An empty bytearray without storage points `ob_start` at a
//! static, aligned, zeroed buffer.
use crate::abstract_ as ab;
use crate::buffer::{self, BufferView};
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::bytes_methods::{self as bm, SelfGuard};
use crate::types::typeobject::tname;
use crate::types::{bool_, bytes, int, none, slice, str as pystr, tuple};
use std::ffi::c_int;

#[unsafe(no_mangle)]
pub static mut PyByteArray_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyByteArrayIter_Type: PyTypeObject = PyTypeObject::ZERO;

pub fn bytearray_type() -> *mut PyTypeObject {
    &raw mut PyByteArray_Type
}
pub fn is_bytearray(o: &PyObject) -> bool {
    o.is_type(bytearray_type()) || crate::types::typeobject::is_subtype(o.type_ptr(), bytearray_type())
}
pub fn is_bytearray_exact(o: &PyObject) -> bool {
    o.is_type(bytearray_type())
}

#[repr(C, align(16))]
struct EmptyBuf([u8; 16]);
static EMPTY: EmptyBuf = EmptyBuf([0; 16]);
fn empty_ptr() -> *mut u8 {
    EMPTY.0.as_ptr() as *mut u8
}

#[inline]
fn ba(o: &PyObject) -> *mut PyByteArrayObject {
    o.as_ptr() as *mut PyByteArrayObject
}

#[inline]
pub fn bytearray_len(o: &PyObject) -> usize {
    unsafe { (*ba(o)).ob_base.ob_size as usize }
}

/// Start of the logical data (never NULL).
#[inline]
pub fn bytearray_ptr(o: &PyObject) -> *mut u8 {
    unsafe {
        let s = (*ba(o)).ob_start;
        if s.is_null() { empty_ptr() } else { s }
    }
}

// Thread safety: mutations run under the object lock (`wlock`, taken by the
// method entry points after their arguments were converted, never while Python
// code runs). Storage replaced by a mutation is released through
// `gc::defer_decref`, so a reader holding a slice of the old storage stays valid
// until it reaches a safepoint; readers snapshot (start, size) under the lock.
// With a single thread no lock is taken and storage is released immediately.

/// The object lock, when more than one thread exists.
#[inline]
fn wlock(o: &PyObject) -> Option<crate::types::dict::ObGuard> {
    if crate::threads::multi_threaded() { Some(crate::types::dict::ob_lock(o)) } else { None }
}

pub fn bytearray_data(o: &PyObject) -> &[u8] {
    let _g = wlock(o);
    data_unlocked(o)
}

/// The data, with the lock already held (or a single thread).
fn data_unlocked(o: &PyObject) -> &[u8] {
    unsafe {
        let n = (*ba(o)).ob_base.ob_size as usize;
        if n == 0 {
            return &[];
        }
        std::slice::from_raw_parts(bytearray_ptr(o), n)
    }
}

#[allow(clippy::mut_from_ref)]
fn data_mut(o: &PyObject) -> &mut [u8] {
    unsafe {
        let n = (*ba(o)).ob_base.ob_size as usize;
        if n == 0 {
            return &mut [];
        }
        std::slice::from_raw_parts_mut(bytearray_ptr(o), n)
    }
}

pub fn exports(o: &PyObject) -> isize {
    unsafe { (*ba(o)).ob_exports }
}
/// Pin/unpin the storage (buffer exports). Serialized with resizes, which
/// check the count under the same lock.
pub fn add_export(o: &PyObject, delta: isize) {
    let _g = wlock(o);
    unsafe { (*ba(o)).ob_exports += delta }
}

fn can_resize(o: &PyObject) -> PyResult<()> {
    if exports(o) > 0 {
        raise!(BufferError, "Existing exports of data: object cannot be re-sized");
    }
    Ok(())
}

/// Capacity of the backing storage.
fn capacity(o: &PyObject) -> usize {
    unsafe { if (*ba(o)).ob_bytes_object.is_null() { 0 } else { (*ba(o)).ob_alloc as usize } }
}
fn logical_offset(o: &PyObject) -> usize {
    unsafe {
        let b = &*ba(o);
        if b.ob_bytes.is_null() || b.ob_start.is_null() { 0 } else { b.ob_start as usize - b.ob_bytes as usize }
    }
}

/// Install `storage` (exact bytes, uniquely owned) as backing; data at `start`.
unsafe fn set_storage(o: &PyObject, storage: Option<PyRef>, start: usize, size: usize) {
    unsafe {
        let b = &mut *ba(o);
        let old = std::mem::replace(&mut b.ob_bytes_object, std::ptr::null_mut());
        match storage {
            None => {
                b.ob_alloc = 0;
                b.ob_bytes = std::ptr::null_mut();
                b.ob_start = empty_ptr();
                b.ob_base.ob_size = 0;
            }
            Some(s) => {
                // the shared empty / one-byte bytes objects must never become
                // (mutable) bytearray storage
                let s = if s.is_immortal() {
                    let n = bytes::bytes_len(&s);
                    let f = bytes::alloc_bytes(n);
                    std::ptr::copy_nonoverlapping(bytes::bytes_ptr(&s), bytes::bytes_ptr(&f), n);
                    f
                } else {
                    s
                };
                b.ob_alloc = var_size(&s);
                b.ob_bytes = bytes::bytes_ptr(&s);
                b.ob_start = b.ob_bytes.add(start);
                b.ob_base.ob_size = size as isize;
                *b.ob_start.add(size) = 0;
                b.ob_bytes_object = s.into_raw();
            }
        }
        // a concurrent reader may still hold a slice of the old storage
        crate::gc::defer_decref(old);
    }
}

/// Change the logical size, keeping the data at the logical start; new bytes
/// are zeroed. Does not check exports.
fn raw_resize(o: &PyObject, newsize: usize) -> PyResult<()> {
    raw_resize_z(o, newsize, true)
}

/// (perf4) `raw_resize`; with `zero == false` the bytes added at the end are left
/// unset (the caller overwrites them: slice assignment, extend), only the trailing
/// NUL is written.
fn raw_resize_z(o: &PyObject, newsize: usize, zero: bool) -> PyResult<()> {
    unsafe {
        let size = bytearray_len(o);
        if newsize == size && !(*ba(o)).ob_start.is_null() {
            return Ok(());
        }
        if newsize > bytes::MAX_SIZE {
            return Err(memory_error());
        }
        if newsize == 0 {
            set_storage(o, None, 0, 0);
            return Ok(());
        }
        let alloc = capacity(o);
        let offset = logical_offset(o);
        let new_alloc;
        if offset + newsize <= alloc {
            if newsize < alloc / 2 {
                new_alloc = newsize; // major downsize
            } else {
                let p = bytearray_ptr(o);
                if newsize > size && zero {
                    std::ptr::write_bytes(p.add(size), 0, newsize - size);
                }
                (*ba(o)).ob_base.ob_size = newsize as isize;
                *p.add(newsize) = 0;
                return Ok(());
            }
        } else if (newsize as f64) <= (alloc as f64 + 1.0) * 1.125 {
            new_alloc = (newsize + (newsize >> 3) + if newsize < 9 { 3 } else { 6 } - 1).min(bytes::MAX_SIZE);
        } else {
            new_alloc = newsize;
        }
        let keep = size.min(newsize);
        let storage = if offset == 0 && !(*ba(o)).ob_bytes_object.is_null() && !crate::threads::multi_threaded() {
            // realloc in place (single-threaded only: realloc frees the old block)
            let old = PyRef::from_raw(std::mem::replace(&mut (*ba(o)).ob_bytes_object, std::ptr::null_mut()));
            match bytes::resize_unique(old, new_alloc) {
                Ok(s) => {
                    let p = bytes::bytes_ptr(&s);
                    if zero {
                        std::ptr::write_bytes(p.add(keep), 0, new_alloc + 1 - keep);
                    } else {
                        *p.add(newsize) = 0;
                    }
                    s
                }
                Err(old) => {
                    // restore the unchanged storage
                    (*ba(o)).ob_bytes_object = old.into_raw();
                    return Err(PyErr);
                }
            }
        } else {
            // (perf4) no zeroing of the part that is copied over
            let s = bytes::try_alloc_bytes_uninit(new_alloc)?;
            let p = bytes::bytes_ptr(&s);
            std::ptr::copy_nonoverlapping(bytearray_ptr(o), p, keep);
            if zero {
                std::ptr::write_bytes(p.add(keep), 0, new_alloc + 1 - keep);
            } else {
                *p.add(newsize) = 0;
            }
            s
        };
        set_storage(o, Some(storage), 0, newsize);
        Ok(())
    }
}

/// PyByteArray_Resize: resize with the export check.
pub fn resize(o: &PyObject, newsize: usize) -> PyResult<()> {
    let _g = wlock(o);
    resize_nl(o, newsize)
}

/// `resize` with the lock held by the caller.
fn resize_nl(o: &PyObject, newsize: usize) -> PyResult<()> {
    if newsize == bytearray_len(o) {
        return Ok(());
    }
    can_resize(o)?;
    raw_resize(o, newsize)
}

fn set_contents(o: &PyObject, v: &[u8]) -> PyResult<()> {
    let _g = wlock(o);
    resize_nl(o, v.len())?;
    unsafe { std::ptr::copy_nonoverlapping(v.as_ptr(), bytearray_ptr(o), v.len()) };
    Ok(())
}

/// Append one byte (lock taken here).
fn append_byte(o: &PyObject, v: u8) -> PyResult<()> {
    let _g = wlock(o);
    let n = bytearray_len(o);
    if n >= bytes::MAX_SIZE {
        raise!(OverflowError, "cannot add more objects to bytearray");
    }
    resize_nl(o, n + 1)?;
    unsafe { *bytearray_ptr(o).add(n) = v };
    Ok(())
}

/// Append `data` at the end (lock taken here).
fn append_bytes(o: &PyObject, data: &[u8]) -> PyResult<()> {
    let _g = wlock(o);
    let n = bytearray_len(o);
    if n.checked_add(data.len()).is_none_or(|t| t > bytes::MAX_SIZE) {
        return Err(memory_error());
    }
    resize_nl(o, n + data.len())?;
    unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), bytearray_ptr(o).add(n), data.len()) };
    Ok(())
}

unsafe fn init_empty(op: *mut PyObject) {
    unsafe {
        let b = &mut *(op as *mut PyByteArrayObject);
        b.ob_alloc = 0;
        b.ob_bytes = std::ptr::null_mut();
        b.ob_start = empty_ptr();
        b.ob_exports = 0;
        b.ob_bytes_object = std::ptr::null_mut();
        b.ob_base.ob_size = 0;
    }
}

fn alloc_of_type(tp: *mut PyTypeObject) -> PyResult {
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(crate::types::typeobject::generic_alloc);
        let op = alloc(tp, 0);
        if op.is_null() {
            if !err_occurred() {
                return Err(memory_error());
            }
            return Err(PyErr);
        }
        init_empty(op);
        Ok(PyRef::from_raw(op))
    }
}

/// New exact bytearray holding a copy of `v`.
pub fn new_bytearray(v: &[u8]) -> PyResult {
    let r = alloc_of_type(bytearray_type())?;
    if !v.is_empty() {
        raw_resize(&r, v.len())?;
        unsafe { std::ptr::copy_nonoverlapping(v.as_ptr(), bytearray_ptr(&r), v.len()) };
    }
    Ok(r)
}

/// PyByteArray_FromObject.
pub fn bytearray_from_object(o: &PyObject) -> PyResult {
    let r = alloc_of_type(bytearray_type())?;
    init_impl(&r, Some(o), None, None)?;
    Ok(r)
}

/// _getbytevalue: an int (via __index__) in range(256).
fn getbytevalue(o: &PyObject) -> PyResult<u8> {
    let i = ab::index(o)?;
    match int::as_i64(&i) {
        Some(v) if (0..256).contains(&v) => Ok(v as u8),
        _ => raise!(ValueError, "byte must be in range(0, 256)"),
    }
}

// ---------------------------------------------------------------------- construction

fn init_impl(o: &PyObject, source: Option<&PyObject>, enc: Option<&PyObject>, errs: Option<&PyObject>) -> PyResult<()> {
    bm::check_encoding_args("bytearray", enc, errs)?;
    if bytearray_len(o) != 0 {
        resize(o, 0)?;
    } else if exports(o) > 0 && source.is_some() {
        can_resize(o)?;
    }
    let src = match source {
        None => {
            if enc.is_some() {
                raise!(TypeError, "encoding without a string argument");
            }
            if errs.is_some() {
                raise!(TypeError, "errors without a string argument");
            }
            return Ok(());
        }
        Some(s) => s,
    };
    if pystr::is_str(src) {
        let e = match enc {
            None => raise!(TypeError, "string argument without an encoding"),
            Some(e) => e,
        };
        let encoded = bm::encode_for_ctor(src, e, errs)?;
        return set_contents(o, bytes::bytes_data(&encoded));
    }
    if enc.is_some() {
        raise!(TypeError, "encoding without a string argument");
    }
    if errs.is_some() {
        raise!(TypeError, "errors without a string argument");
    }
    if ab::has_index(src) {
        match ab::index_as_ssize(src, Some(crate::types::exceptions::OverflowError())) {
            Ok(n) => {
                if n < 0 {
                    raise!(ValueError, "negative count");
                }
                return resize(o, n as usize);
            }
            Err(e) => {
                if !exc_matches(crate::types::exceptions::TypeError()) {
                    return Err(e);
                }
                clear_exc();
            }
        }
    }
    if buffer::check_buffer(src) {
        let data = {
            let b = BufferView::get(src, buffer::PyBUF_FULL_RO)?;
            b.to_vec()
        };
        return set_contents(o, &data);
    }
    let it = match ab::get_iter(src) {
        Ok(it) => it,
        Err(e) => {
            if exc_matches(crate::types::exceptions::TypeError()) {
                clear_exc();
                raise!(TypeError, "cannot convert '{}' object to bytearray", tname(src));
            }
            return Err(e);
        }
    };
    let r = (|| -> PyResult<()> {
        while let Some(item) = ab::iter_next(&it)? {
            let v = getbytevalue(&item)?;
            append_byte(o, v)?;
        }
        Ok(())
    })();
    if r.is_err() {
        let _g = wlock(o);
        if exports(o) == 0 {
            let _ = raw_resize(o, 0);
        }
    }
    r
}

unsafe extern "C" fn bytearray_new(tp: *mut PyTypeObject, _args: *mut PyObject, _kw: *mut PyObject) -> *mut PyObject {
    into_c(alloc_of_type(tp))
}

unsafe extern "C" fn bytearray_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let (v, names) = crate::args::kwdict_to_vectorcall(&*args, kw);
        let refs: Vec<&PyObject> = v.iter().map(|r| &**r).collect();
        static P: crate::args::Params = crate::args::Params::new("bytearray", &["source", "encoding", "errors"], 0, 0);
        let r = (|| {
            let [x, enc, errs] = crate::args::parse::<3>(&P, &refs, names.as_deref())?;
            init_impl(&*o, x, enc, errs)
        })();
        int_into_c(r)
    }
}

unsafe extern "C" fn bytearray_dealloc(o: *mut PyObject) {
    unsafe {
        let b = &mut *(o as *mut PyByteArrayObject);
        xdecref(std::mem::replace(&mut b.ob_bytes_object, std::ptr::null_mut()));
        crate::types::typeobject::generic_free_and_type_decref(o, (*o).type_ptr());
    }
}

// ---------------------------------------------------------------------- slots

fn ni() -> *mut PyObject {
    none::not_implemented().to_ref().into_raw()
}

unsafe extern "C" fn bytearray_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        let name = crate::types::typeobject::type_name(o.ty());
        let s = bm::bytes_repr_string(bytearray_data(o), &format!("{}(b", name), ")");
        pystr::new_str(&s).into_raw()
    }
}
unsafe extern "C" fn bytearray_str(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        if crate::modules::sys::bytes_warning() > 0 {
            if crate::modules::warnings::warn(crate::types::exceptions::BytesWarning(), "str() on a bytearray instance", 1).is_err() {
                return std::ptr::null_mut();
            }
        }
        bytearray_repr(o)
    }
}

unsafe extern "C" fn bytearray_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if pystr::is_str(a) || pystr::is_str(b) {
            if bm::cmp_warning(if pystr::is_str(a) { a } else { b }, op, "bytearray").is_err() {
                return std::ptr::null_mut();
            }
            return ni();
        }
        if !buffer::check_buffer(a) || !buffer::check_buffer(b) {
            return ni();
        }
        let va = match BufferView::simple(a) {
            Ok(v) => v,
            Err(_) => {
                clear_exc();
                return ni();
            }
        };
        let vb = match BufferView::simple(b) {
            Ok(v) => v,
            Err(_) => {
                clear_exc();
                return ni();
            }
        };
        let (x, y) = (va.as_bytes(), vb.as_bytes());
        if (op == Py_EQ || op == Py_NE) && x.len() != y.len() {
            return bool_::new_bool(op == Py_NE).into_raw();
        }
        bool_::new_bool(bm::cmp_result(x.cmp(y), op)).into_raw()
    }
}

unsafe extern "C" fn bytearray_len_slot(o: *mut PyObject) -> Py_ssize_t {
    unsafe { bytearray_len(&*o) as isize }
}

unsafe extern "C" fn bytearray_item(o: *mut PyObject, i: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let d = bytearray_data(&*o);
        if i < 0 || i as usize >= d.len() {
            let _ = err!(IndexError, "bytearray index out of range");
            return std::ptr::null_mut();
        }
        int::new_int(d[i as usize] as i64).into_raw()
    }
}

fn subscript_impl(o: &PyObject, k: &PyObject) -> PyResult {
    if ab::has_index(k) {
        let mut i = ab::index_as_ssize(k, Some(crate::types::exceptions::IndexError()))?;
        let d = bytearray_data(o);
        if i < 0 {
            i += d.len() as isize;
        }
        if i < 0 || i as usize >= d.len() {
            raise!(IndexError, "bytearray index out of range");
        }
        return Ok(int::new_int(d[i as usize] as i64));
    }
    if slice::is_slice(k) {
        let (start, _s, step, len) = slice::indices(k, bytearray_len(o) as isize)?;
        let v = bm::slice_bytes(bytearray_data(o), start, step, len);
        return new_bytearray(&v);
    }
    raise!(TypeError, "bytearray indices must be integers or slices, not {}", tname(k));
}
unsafe extern "C" fn bytearray_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(subscript_impl(&*o, &*k)) }
}

/// Replace [lo, hi) with `bytes` (a linear slice assignment).
fn setslice_linear(o: &PyObject, lo: usize, hi: usize, bytes: &[u8]) -> PyResult<()> {
    let size = bytearray_len(o);
    let hi = hi.min(size);
    let lo = lo.min(hi);
    let needed = bytes.len();
    let avail = hi - lo;
    if needed < avail {
        can_resize(o)?;
        let shrink = avail - needed;
        unsafe {
            if lo == 0 {
                // advance the logical start
                let b = &mut *ba(o);
                if !b.ob_bytes_object.is_null() {
                    b.ob_start = b.ob_start.add(shrink);
                    b.ob_base.ob_size -= shrink as isize;
                    let newsize = size - shrink;
                    // apply the downsize policy
                    let cap = capacity(o);
                    if newsize < cap / 2 || newsize == 0 {
                        if newsize == 0 {
                            set_storage(o, None, 0, 0);
                        } else {
                            // (perf4) copied straight from the old storage (still owned
                            // until set_storage), no temporary vector, no zeroing
                            let s = bytes::try_alloc_bytes_uninit(newsize)?;
                            std::ptr::copy_nonoverlapping(bytearray_ptr(o), bytes::bytes_ptr(&s), newsize);
                            set_storage(o, Some(s), 0, newsize);
                        }
                    } else {
                        *bytearray_ptr(o).add(newsize) = 0;
                    }
                }
            } else {
                let p = bytearray_ptr(o);
                std::ptr::copy(p.add(hi), p.add(lo + needed), size - hi);
                raw_resize(o, size - shrink)?;
            }
            if needed > 0 {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), bytearray_ptr(o).add(lo), needed);
            }
        }
    } else if needed > avail {
        let growth = needed - avail;
        if size.checked_add(growth).is_none_or(|n| n > bytes::MAX_SIZE) {
            return Err(memory_error());
        }
        // (perf4) the grown part is overwritten below: not zeroed first
        can_resize(o)?;
        raw_resize_z(o, size + growth, false)?;
        unsafe {
            let p = bytearray_ptr(o);
            std::ptr::copy(p.add(hi), p.add(hi + growth), size - hi);
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.add(lo), needed);
        }
    } else if needed > 0 {
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), bytearray_ptr(o).add(lo), needed) };
    }
    Ok(())
}

/// Values for a slice assignment: None (deletion) or the bytes to store.
fn slice_values(o: &PyObject, v: &PyObject) -> PyResult<Vec<u8>> {
    if v.is(o) {
        return Ok(bytearray_data(o).to_vec());
    }
    if is_bytearray(v) {
        return Ok(bytearray_data(v).to_vec());
    }
    if pystr::is_str(v) || is_number(v) {
        raise!(TypeError, "can assign only bytes, buffers, or iterables of ints in range(0, 256)");
    }
    let tmp = bytearray_from_object(v)?;
    Ok(bytearray_data(&tmp).to_vec())
}

fn is_number(v: &PyObject) -> bool {
    if int::is_int(v) || crate::types::float::is_float(v) || crate::types::complex::is_complex(v) {
        return true;
    }
    let nb = v.ty().tp_as_number;
    !nb.is_null() && unsafe { (*nb).nb_index.is_some() || (*nb).nb_int.is_some() || (*nb).nb_float.is_some() }
}

fn ass_subscript_impl(o: &PyObject, k: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    if ab::has_index(k) {
        let i = ab::index_as_ssize(k, Some(crate::types::exceptions::IndexError()))?;
        let b = match v {
            None => None,
            Some(v) => Some(getbytevalue(v)?),
        };
        return set_or_del_index(o, i, b);
    }
    if !slice::is_slice(k) {
        raise!(TypeError, "bytearray indices must be integers or slices, not {}", tname(k));
    }
    // convert everything that may run Python code before taking the lock
    let values: Option<Vec<u8>> = match v {
        None => None,
        Some(v) => Some(slice_values(o, v)?),
    };
    let (mut start, mut stop, step) = slice::unpack(k)?;
    let _g = wlock(o);
    let slicelen = slice::adjust_indices(bytearray_len(o) as isize, &mut start, &mut stop, step);
    match values {
        None => del_slice_nl(o, start, step, slicelen),
        Some(vals) => {
            if (step < 0 && start < stop) || (step > 0 && start > stop) {
                stop = start;
            }
            if step == 1 {
                return setslice_linear(o, start as usize, stop.max(start) as usize, &vals);
            }
            if vals.is_empty() && slicelen > 0 {
                // b[::2] = b"" deletes
                return del_slice_nl(o, start, step, slicelen);
            }
            if vals.len() as isize != slicelen {
                raise!(ValueError, "attempt to assign bytes of size {} to extended slice of size {}", vals.len(), slicelen);
            }
            let d = data_mut(o);
            let mut i = start;
            for &c in &vals {
                d[i as usize] = c;
                i = i.wrapping_add(step);
            }
            Ok(())
        }
    }
}

/// `del o[start::step]` (slicelen items), lock held.
fn del_slice_nl(o: &PyObject, start: isize, step: isize, slicelen: isize) -> PyResult<()> {
    if slicelen <= 0 {
        return Ok(());
    }
    if step == 1 {
        return setslice_linear(o, start as usize, (start + slicelen) as usize, &[]);
    }
    can_resize(o)?;
    let (mut lo, mut st) = (start, step);
    if st < 0 {
        lo = start + st * (slicelen - 1);
        st = -st;
    }
    let d = data_unlocked(o).to_vec();
    let mut out = Vec::with_capacity(d.len() - slicelen as usize);
    let mut next_del = lo as usize;
    let mut deleted = 0;
    for (i, &c) in d.iter().enumerate() {
        if deleted < slicelen as usize && i == next_del {
            deleted += 1;
            next_del = next_del.saturating_add(st as usize);
            continue;
        }
        out.push(c);
    }
    let n = out.len();
    raw_resize(o, n)?;
    unsafe { std::ptr::copy_nonoverlapping(out.as_ptr(), bytearray_ptr(o), n) };
    Ok(())
}

/// `o[i] = b` / `del o[i]` (negative i counts from the end); takes the lock.
fn set_or_del_index(o: &PyObject, i: isize, b: Option<u8>) -> PyResult<()> {
    let _g = wlock(o);
    let n = bytearray_len(o) as isize;
    let i = if i < 0 { i + n } else { i };
    if i < 0 || i >= n {
        raise!(IndexError, "bytearray index out of range");
    }
    match b {
        None => setslice_linear(o, i as usize, i as usize + 1, &[]),
        Some(b) => {
            data_mut(o)[i as usize] = b;
            Ok(())
        }
    }
}
unsafe extern "C" fn bytearray_ass_subscript(o: *mut PyObject, k: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe { int_into_c(ass_subscript_impl(&*o, &*k, if v.is_null() { None } else { Some(&*v) })) }
}
unsafe extern "C" fn bytearray_ass_item(o: *mut PyObject, i: Py_ssize_t, v: *mut PyObject) -> c_int {
    unsafe {
        let o = &*o;
        let r = (|| -> PyResult<()> {
            if i < 0 {
                raise!(IndexError, "bytearray index out of range");
            }
            let b = if v.is_null() { None } else { Some(getbytevalue(&*v)?) };
            set_or_del_index(o, i, b)
        })();
        int_into_c(r)
    }
}

unsafe extern "C" fn bytearray_concat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        let r = (|| {
            let (va, vb) = match (BufferView::simple(a), BufferView::simple(b)) {
                (Ok(x), Ok(y)) => (x, y),
                _ => {
                    clear_exc();
                    raise!(TypeError, "can't concat {} to {}", tname(b), tname(a));
                }
            };
            let mut v = Vec::new();
            v.try_reserve_exact(va.len() + vb.len()).map_err(|_| memory_error())?;
            v.extend_from_slice(va.as_bytes());
            v.extend_from_slice(vb.as_bytes());
            new_bytearray(&v)
        })();
        into_c(r)
    }
}
unsafe extern "C" fn bytearray_iconcat(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        let r = (|| {
            let data = match BufferView::simple(b) {
                Ok(v) => v.as_bytes().to_vec(),
                Err(_) => {
                    clear_exc();
                    raise!(TypeError, "can't concat {} to {}", tname(b), tname(a));
                }
            };
            append_bytes(a, &data)?;
            Ok(a.to_ref())
        })();
        into_c(r)
    }
}
unsafe extern "C" fn bytearray_repeat(a: *mut PyObject, n: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let a = &*a;
        into_c(bm::repeat_bytes(bytearray_data(a), n, "bytearray").and_then(|v| new_bytearray(&v)))
    }
}
unsafe extern "C" fn bytearray_irepeat(a: *mut PyObject, n: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let a = &*a;
        let r = (|| {
            let v = bm::repeat_bytes(bytearray_data(a), n, "bytearray")?;
            set_contents(a, &v)?;
            Ok(a.to_ref())
        })();
        into_c(r)
    }
}
unsafe extern "C" fn bytearray_contains(o: *mut PyObject, x: *mut PyObject) -> c_int {
    unsafe {
        match bm::contains(&*o, &*x) {
            Ok(b) => b as c_int,
            Err(_) => -1,
        }
    }
}
unsafe extern "C" fn bytearray_getbuffer(o: *mut PyObject, view: *mut Py_buffer, flags: c_int) -> c_int {
    unsafe {
        let ob = &*o;
        if view.is_null() {
            let _ = err!(BufferError, "bytearray_getbuffer: view==NULL argument is obsolete");
            return -1;
        }
        // the storage seen by the view and the pin are taken atomically
        let _g = wlock(ob);
        let r = buffer::fill_info(view, o, bytearray_ptr(ob) as *mut _, bytearray_len(ob) as isize, false, flags);
        if r.is_err() {
            return -1;
        }
        (*ba(ob)).ob_exports += 1;
        0
    }
}
unsafe extern "C" fn bytearray_releasebuffer(o: *mut PyObject, _view: *mut Py_buffer) {
    unsafe { add_export(&*o, -1) }
}
unsafe extern "C" fn bytearray_mod(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let a = &*a;
        if !is_bytearray(a) {
            return ni();
        }
        let _g = SelfGuard::new(a);
        into_c(bm::percent_format(bytearray_data(a), &*b).and_then(|v| new_bytearray(&v)))
    }
}
unsafe extern "C" fn bytearray_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let op = crate::alloc::alloc_object(&raw mut PyByteArrayIter_Type, std::mem::size_of::<BaIter>());
        (*(op as *mut BaIter)).seq = (*o).to_ref().into_raw();
        crate::gc::track(op);
        op
    }
}

// ---------------------------------------------------------------------- methods

fn m_append(o: &PyObject, item: &PyObject) -> PyResult {
    let v = getbytevalue(item)?;
    append_byte(o, v)?;
    Ok(none::none_ref())
}

fn m_extend(o: &PyObject, it: &PyObject) -> PyResult {
    if pystr::is_str(it) {
        raise!(TypeError, "expected iterable of integers; got: 'str'");
    }
    if bytes::is_bytes_exact(it) {
        // (perf4) immutable source: appended without a temporary copy
        let _g = wlock(o);
        let size = bytearray_len(o);
        setslice_linear(o, size, size, bytes::bytes_data(it))?;
        return Ok(none::none_ref());
    }
    if buffer::check_buffer(it) {
        let n = bytearray_len(o);
        let data = {
            let b = BufferView::simple(it)?;
            b.as_bytes().to_vec()
        };
        // the bounds were computed before the buffer was acquired
        let _g = wlock(o);
        let size = bytearray_len(o);
        let lo = n.min(size);
        setslice_linear(o, lo, lo, &data)?;
        return Ok(none::none_ref());
    }
    let iter = match ab::get_iter(it) {
        Ok(i) => i,
        Err(e) => {
            if exc_matches(crate::types::exceptions::TypeError()) {
                clear_exc();
                raise!(TypeError, "can't extend bytearray with {}", tname(it));
            }
            return Err(e);
        }
    };
    let hint = ab::length_hint(it, 64).unwrap_or_else(|_| {
        clear_exc();
        64
    });
    let mut v: Vec<u8> = Vec::with_capacity(hint.min(1 << 20));
    while let Some(item) = ab::iter_next(&iter)? {
        v.push(getbytevalue(&item)?);
    }
    append_bytes(o, &v)?;
    Ok(none::none_ref())
}

fn m_insert(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("insert", args, 2, 2)?;
    let mut w = bm::ssize_arg(args[0])?;
    let v = getbytevalue(args[1])?;
    let _g = wlock(o);
    let n = bytearray_len(o) as isize;
    if n as usize >= bytes::MAX_SIZE {
        raise!(OverflowError, "cannot add more objects to bytearray");
    }
    resize_nl(o, n as usize + 1)?;
    if w < 0 {
        w += n;
        if w < 0 {
            w = 0;
        }
    }
    if w > n {
        w = n;
    }
    unsafe {
        let p = bytearray_ptr(o);
        std::ptr::copy(p.add(w as usize), p.add(w as usize + 1), (n - w) as usize);
        *p.add(w as usize) = v;
    }
    Ok(none::none_ref())
}

fn m_pop(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("pop", args, 0, 1)?;
    let mut i = match args.first() {
        None => -1,
        Some(x) => bm::ssize_arg(x)?,
    };
    let v = {
        let _g = wlock(o);
        let n = bytearray_len(o) as isize;
        if n == 0 {
            raise!(IndexError, "pop from empty bytearray");
        }
        if i < 0 {
            i += n;
        }
        if i < 0 || i >= n {
            raise!(IndexError, "pop index out of range");
        }
        can_resize(o)?;
        let v = data_unlocked(o)[i as usize];
        setslice_linear(o, i as usize, i as usize + 1, &[])?;
        v
    };
    Ok(int::new_int(v as i64))
}

fn m_remove(o: &PyObject, x: &PyObject) -> PyResult {
    let v = getbytevalue(x)?;
    let _g = wlock(o);
    let pos = match bm::memchr(v, data_unlocked(o)) {
        Some(p) => p,
        None => raise!(ValueError, "value not found in bytearray"),
    };
    can_resize(o)?;
    setslice_linear(o, pos, pos + 1, &[])?;
    Ok(none::none_ref())
}

fn m_reverse(o: &PyObject) -> PyResult {
    let _g = wlock(o);
    data_mut(o).reverse();
    Ok(none::none_ref())
}
fn m_clear(o: &PyObject) -> PyResult {
    resize(o, 0)?;
    Ok(none::none_ref())
}
fn m_copy(o: &PyObject) -> PyResult {
    new_bytearray(bytearray_data(o))
}
fn m_resize(o: &PyObject, size: &PyObject) -> PyResult {
    let n = bm::ssize_arg(size)?;
    if n < 0 {
        raise!(ValueError, "Can only resize to positive sizes, got {}", n);
    }
    resize(o, n as usize)?;
    Ok(none::none_ref())
}
fn alloc_value(o: &PyObject) -> usize {
    if unsafe { (*ba(o)).ob_bytes_object.is_null() } { 0 } else { capacity(o) + bytes::HEADER + 1 }
}
fn m_alloc(o: &PyObject) -> PyResult {
    Ok(int::new_int_usize(alloc_value(o)))
}
fn m_sizeof(o: &PyObject) -> PyResult {
    Ok(int::new_int_usize(o.ty().tp_basicsize as usize + alloc_value(o)))
}

fn m_take_bytes(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("take_bytes", args, 0, 1)?;
    let req: Option<isize> = match args.first() {
        None => None,
        Some(x) if none::is_none(x) => None,
        Some(x) => Some(bm::ssize_arg(x)?),
    };
    let _g = wlock(o);
    let size = bytearray_len(o) as isize;
    let mut n = req.unwrap_or(size);
    if n < 0 {
        n += size;
    }
    if n < 0 || n > size {
        raise!(IndexError, "can't take {} bytes from a bytearray of size {}", req.unwrap_or(size), size);
    }
    can_resize(o)?;
    let n = n as usize;
    let size = size as usize;
    if n == 0 {
        return bytes::try_new_bytes(&[]);
    }
    unsafe {
        let offset = logical_offset(o);
        if n * 2 < size {
            // small take: copy out and advance the logical start
            let r = bytes::try_new_bytes(&data_unlocked(o)[..n])?;
            let b = &mut *ba(o);
            b.ob_start = b.ob_start.add(n);
            b.ob_base.ob_size -= n as isize;
            return Ok(r);
        }
        let rest: Option<PyRef> = if n < size { Some(bytes::try_new_bytes(&data_unlocked(o)[n..])?) } else { None };
        if crate::threads::multi_threaded() {
            // other threads may be reading the storage: copy instead of handing
            // it over (resizing it in place would free it under them)
            let taken = bytes::try_new_bytes(&data_unlocked(o)[..n])?;
            match rest {
                None => set_storage(o, None, 0, 0),
                Some(r) => {
                    let len = bytes::bytes_len(&r);
                    set_storage(o, Some(r), 0, len);
                }
            }
            return Ok(taken);
        }
        // large take: hand over the storage (zero copy for the taken part)
        if offset > 0 {
            let p = (*ba(o)).ob_bytes;
            std::ptr::copy(p.add(offset), p, size);
            (*ba(o)).ob_start = p;
        }
        let storage = PyRef::from_raw(std::mem::replace(&mut (*ba(o)).ob_bytes_object, std::ptr::null_mut()));
        let taken = match bytes::resize_unique(storage, n) {
            Ok(t) => t,
            Err(old) => {
                (*ba(o)).ob_bytes_object = old.into_raw();
                return Err(PyErr);
            }
        };
        match rest {
            None => set_storage(o, None, 0, 0),
            Some(r) => {
                let len = bytes::bytes_len(&r);
                set_storage(o, Some(r), 0, len);
            }
        }
        Ok(taken)
    }
}

fn common_reduce(o: &PyObject, proto: i64) -> PyResult {
    let state = ab::call_method_str(o, "__getstate__", &[])?;
    let tp = crate::types::typeobject::type_ref(o.type_ptr());
    let d = bytearray_data(o);
    if proto < 3 {
        let latin1 = pystr::from_latin1(d);
        let args = tuple::new_tuple(vec![latin1, pystr::new_str("latin-1")]);
        return Ok(tuple::new_tuple(vec![tp, args, state]));
    }
    let args = if d.is_empty() { tuple::empty_tuple() } else { tuple::new_tuple(vec![bytes::new_bytes(d)]) };
    Ok(tuple::new_tuple(vec![tp, args, state]))
}
fn m_reduce(o: &PyObject) -> PyResult {
    common_reduce(o, 2)
}
fn m_reduce_ex(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("__reduce_ex__", args, 0, 1)?;
    let proto = match args.first() {
        None => 0,
        Some(p) => ab::as_i64(p)?,
    };
    common_reduce(o, proto)
}
fn m_fromhex(cls: &PyObject, arg: &PyObject) -> PyResult {
    let v = bm::parse_fromhex(arg)?;
    let r = new_bytearray(&v)?;
    if cls.as_ptr() as *mut PyTypeObject == bytearray_type() {
        return Ok(r);
    }
    ab::call(cls, &[&r])
}

// ---------------------------------------------------------------------- iterator

#[repr(C)]
pub struct BaIter {
    pub ob_base: PyObject,
    pub index: isize,
    pub seq: *mut PyObject,
}
unsafe extern "C" fn baiter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        // an iterator shared between threads is stepped under its own lock (the
        // bytearray it holds must not be released by another thread finishing
        // the iteration meanwhile); the bytearray is released after unlocking
        let mut finished: *mut PyObject = std::ptr::null_mut();
        let r = {
            let _g = wlock(&*o);
            let it = &mut *(o as *mut BaIter);
            if it.seq.is_null() {
                return std::ptr::null_mut();
            }
            let d = bytearray_data(&*it.seq);
            if it.index >= 0 && (it.index as usize) < d.len() {
                let v = d[it.index as usize];
                it.index += 1;
                Some(v)
            } else {
                finished = std::mem::replace(&mut it.seq, std::ptr::null_mut());
                None
            }
        };
        xdecref(finished);
        match r {
            Some(v) => int::new_int(v as i64).into_raw(),
            None => std::ptr::null_mut(),
        }
    }
}
unsafe extern "C" fn baiter_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        xdecref((*(o as *mut BaIter)).seq);
        crate::alloc::free_object(o);
    }
}
unsafe extern "C" fn baiter_traverse(o: *mut PyObject, visit: visitproc, arg: *mut std::ffi::c_void) -> c_int {
    unsafe {
        let s = (*(o as *mut BaIter)).seq;
        if s.is_null() { 0 } else { visit.unwrap()(s, arg) }
    }
}
/// (sequence, index) of an iterator, read under its lock (see baiter_next).
fn baiter_snapshot(o: &PyObject) -> (Option<PyRef>, isize) {
    let _g = wlock(o);
    let it = unsafe { &*(o.as_ptr() as *mut BaIter) };
    (unsafe { it.seq.as_ref() }.map(|s| s.to_ref()), it.index)
}
fn baiter_length_hint(o: &PyObject) -> PyResult {
    let (seq, index) = baiter_snapshot(o);
    let Some(seq) = seq else {
        return Ok(int::new_int(0));
    };
    let n = bytearray_len(&seq) as isize - index;
    Ok(int::new_int(n.max(0) as i64))
}
fn baiter_reduce(o: &PyObject) -> PyResult {
    let f = crate::builtins::eval_builtin("iter")?;
    let (seq, index) = baiter_snapshot(o);
    bm::iter_reduce(f, seq.as_ref().map_or(std::ptr::null_mut(), |s| s.as_ptr()), index)
}
fn baiter_setstate(o: &PyObject, state: &PyObject) -> PyResult {
    let it = unsafe { &mut *(o.as_ptr() as *mut BaIter) };
    let mut idx = ab::index_as_ssize(state, None)?;
    if !it.seq.is_null() {
        let n = bytearray_len(unsafe { &*it.seq }) as isize;
        if idx < 0 {
            idx = 0;
        } else if idx > n {
            idx = n;
        }
        it.index = idx;
    }
    Ok(none::none_ref())
}

// ---------------------------------------------------------------------- type

static mut BA_SEQ: PySequenceMethods = PySequenceMethods::ZERO;
static mut BA_MAP: PyMappingMethods = PyMappingMethods::ZERO;
static mut BA_NUM: PyNumberMethods = PyNumberMethods::ZERO;
static mut BA_BUF: PyBufferProcs = PyBufferProcs::ZERO;

const BYTEARRAY_DOC: &str = "bytearray(iterable_of_ints) -> bytearray\nbytearray(string, encoding[, errors]) -> bytearray\nbytearray(bytes_or_buffer) -> mutable copy of bytes_or_buffer\nbytearray(int) -> bytes array of size given by the parameter initialized with null bytes\nbytearray() -> empty bytes array\n\nConstruct a mutable bytearray object from:\n  - an iterable yielding integers in range(256)\n  - a text string encoded using the specified encoding\n  - a bytes or a buffer object\n  - any object implementing the buffer API.\n  - an integer";

pub fn init_types() {
    unsafe {
        let t = &mut *bytearray_type();
        t.tp_name = c"bytearray".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyByteArrayObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE; // not a sequence for `match` (like str/bytes)
        t.tp_dealloc = Some(bytearray_dealloc);
        t.tp_repr = Some(bytearray_repr);
        t.tp_str = Some(bytearray_str);
        t.tp_hash = Some(crate::types::slots::hash_not_implemented);
        t.tp_richcompare = Some(bytearray_richcompare);
        t.tp_iter = Some(bytearray_iter);
        t.tp_new = Some(bytearray_new);
        t.tp_init = Some(bytearray_init);
        t.tp_alloc = Some(crate::types::typeobject::generic_alloc);
        t.tp_free = Some(crate::types::typeobject::generic_free);
        t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
        t.tp_doc = crate::methods::cstr_static(BYTEARRAY_DOC);
        let s = &mut *(&raw mut BA_SEQ);
        s.sq_length = Some(bytearray_len_slot);
        s.sq_concat = Some(bytearray_concat);
        s.sq_repeat = Some(bytearray_repeat);
        s.sq_item = Some(bytearray_item);
        s.sq_ass_item = Some(bytearray_ass_item);
        s.sq_contains = Some(bytearray_contains);
        s.sq_inplace_concat = Some(bytearray_iconcat);
        s.sq_inplace_repeat = Some(bytearray_irepeat);
        t.tp_as_sequence = s;
        let m = &mut *(&raw mut BA_MAP);
        m.mp_length = Some(bytearray_len_slot);
        m.mp_subscript = Some(bytearray_subscript);
        m.mp_ass_subscript = Some(bytearray_ass_subscript);
        t.tp_as_mapping = m;
        let n = &mut *(&raw mut BA_NUM);
        n.nb_remainder = Some(bytearray_mod);
        t.tp_as_number = n;
        let b = &mut *(&raw mut BA_BUF);
        b.bf_getbuffer = Some(bytearray_getbuffer);
        b.bf_releasebuffer = Some(bytearray_releasebuffer);
        t.tp_as_buffer = b;
        let mut v = bm::common_methods();
        use crate::methods::{def, fast, noargs, o};
        v.push(def("__alloc__", noargs(m_alloc), METH_NOARGS, "B.__alloc__() -> int\n\nReturn the number of bytes actually allocated."));
        v.push(def("__reduce__", noargs(m_reduce), METH_NOARGS, "__reduce__($self, /)\n--\n\nReturn state information for pickling."));
        v.push(def("__reduce_ex__", fast(m_reduce_ex), METH_FASTCALL, "__reduce_ex__($self, proto=0, /)\n--\n\nReturn state information for pickling."));
        v.push(def("__sizeof__", noargs(m_sizeof), METH_NOARGS, "__sizeof__($self, /)\n--\n\nReturns the size of the bytearray object in memory, in bytes."));
        v.push(def("append", o(m_append), METH_O, "append($self, item, /)\n--\n\nAppend a single item to the end of the bytearray.\n\n  item\n    The item to be appended."));
        v.push(def("clear", noargs(m_clear), METH_NOARGS, "clear($self, /)\n--\n\nRemove all items from the bytearray."));
        v.push(def("copy", noargs(m_copy), METH_NOARGS, "copy($self, /)\n--\n\nReturn a copy of B."));
        v.push(def("extend", o(m_extend), METH_O, "extend($self, iterable_of_ints, /)\n--\n\nAppend all the items from the iterator or sequence to the end of the bytearray.\n\n  iterable_of_ints\n    The iterable of items to append."));
        v.push(def("fromhex", o(m_fromhex), METH_O | METH_CLASS, "fromhex($type, string, /)\n--\n\nCreate a bytearray object from a string of hexadecimal numbers.\n\nSpaces between two numbers are accepted.\nExample: bytearray.fromhex('B9 01EF') -> bytearray(b'\\\\xb9\\\\x01\\\\xef')"));
        v.push(def("insert", fast(m_insert), METH_FASTCALL, "insert($self, index, item, /)\n--\n\nInsert a single item into the bytearray before the given index.\n\n  index\n    The index where the value is to be inserted.\n  item\n    The item to be inserted."));
        v.push(def("pop", fast(m_pop), METH_FASTCALL, "pop($self, index=-1, /)\n--\n\nRemove and return a single item from B.\n\n  index\n    The index from where to remove the item.\n    -1 (the default value) means remove the last item.\n\nIf no index argument is given, will pop the last item."));
        v.push(def("remove", o(m_remove), METH_O, "remove($self, value, /)\n--\n\nRemove the first occurrence of a value in the bytearray.\n\n  value\n    The value to remove."));
        v.push(def("resize", o(m_resize), METH_O, "resize($self, size, /)\n--\n\nResize the internal buffer of bytearray to len.\n\n  size\n    New size to resize to.."));
        v.push(def("reverse", noargs(m_reverse), METH_NOARGS, "reverse($self, /)\n--\n\nReverse the order of the values in B in place."));
        v.push(def("take_bytes", fast(m_take_bytes), METH_FASTCALL, "take_bytes($self, n=None, /)\n--\n\nTake *n* bytes from the bytearray and return them as a bytes object.\n\n  n\n    Bytes to take, negative indexes from end. None indicates all bytes."));
        t.tp_methods = crate::methods::table(v);

        let t = &mut *(&raw mut PyByteArrayIter_Type);
        t.tp_name = c"bytearray_iterator".as_ptr();
        t.tp_basicsize = std::mem::size_of::<BaIter>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
        t.tp_dealloc = Some(baiter_dealloc);
        t.tp_traverse = Some(baiter_traverse);
        t.tp_iter = Some(bm::self_iter);
        t.tp_iternext = Some(baiter_next);
        t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
        t.tp_methods = crate::mdefs![
            ("__length_hint__", NOARGS, baiter_length_hint, "Private method returning an estimate of len(list(it))."),
            ("__reduce__", NOARGS, baiter_reduce, "Return state information for pickling."),
            ("__setstate__", O, baiter_setstate, "Set state information for unpickling."),
        ];
    }
}

pub fn ready() -> PyResult<()> {
    crate::types::typeobject::type_ready(&raw mut PyByteArrayIter_Type)
}
