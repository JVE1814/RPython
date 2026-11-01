//! Shared plumbing for the compression and hashing modules (`zlib`, `_bz2`, `_lzma`, `_zstd`,
//! `_md5`, `_sha*`, `_blake2`, `_hashlib`, `_hmac`): objects carrying Rust state behind a
//! mutex, argument helpers and exception creation.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::_struct::Buf;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::*;
use crate::types::{bytes, dict, int, str as pystr, tuple};
use std::sync::{Mutex, MutexGuard};

/// Register every module of the compress workstream.
pub fn register() {
    use crate::import::register_builtin as reg;
    use crate::modules::hashes;
    reg("zlib", crate::modules::zlib::init_module);
    reg("_md5", hashes::init_md5);
    reg("_sha1", hashes::init_sha1);
    reg("_sha2", hashes::init_sha2);
    reg("_sha3", hashes::init_sha3);
    reg("_blake2", hashes::init_blake2);
    reg("_hashlib", hashes::init_hashlib);
    reg("_hmac", hashes::init_hmac);
    reg("_bz2", crate::modules::_bz2::init_module);
    reg("_lzma", crate::modules::_lzma::init_module);
}

/// Object layout: header + pointer to the boxed, mutex-protected Rust state.
#[repr(C)]
pub struct StateObj<T> {
    ob_base: PyObject,
    state: *mut Mutex<T>,
}

/// Allocate an instance of `tp` holding `val`.
pub fn new_obj<T: Send + 'static>(tp: *mut PyTypeObject, val: T) -> PyResult {
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
        let op = alloc(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        let so = &mut *(op as *mut StateObj<T>);
        so.state = Box::into_raw(Box::new(Mutex::new(val)));
        Ok(PyRef::from_raw(op))
    }
}

/// Allocate an instance with an empty state slot (for `tp_new` of subclassable types).
pub fn state_ptr<T>(o: &PyObject) -> *mut Mutex<T> {
    unsafe { (*(o.as_ptr() as *mut StateObj<T>)).state }
}

pub fn set_state<T: Send + 'static>(o: &PyObject, val: T) {
    unsafe {
        let so = &mut *(o.as_ptr() as *mut StateObj<T>);
        if so.state.is_null() {
            so.state = Box::into_raw(Box::new(Mutex::new(val)));
        } else {
            let mut g = (*so.state).lock().unwrap_or_else(|e| e.into_inner());
            *g = val;
        }
    }
}

/// Lock the state.  Waiting happens in "blocked" thread mode so a stop-the-world GC can
/// proceed while another thread holds the lock during a long (blocking) operation.
pub fn lock<T>(o: &PyObject) -> PyResult<MutexGuard<'static, T>> {
    let p = state_ptr::<T>(o);
    if p.is_null() {
        raise!(ValueError, "object is not initialized");
    }
    let m: &'static Mutex<T> = unsafe { &*p };
    if let Ok(g) = m.try_lock() {
        return Ok(g);
    }
    Ok(crate::threads::blocking(|| m.lock().unwrap_or_else(|e| e.into_inner())))
}

pub unsafe extern "C" fn state_dealloc<T>(o: *mut PyObject) {
    unsafe {
        let tp = (*o).type_ptr();
        let so = &mut *(o as *mut StateObj<T>);
        if !so.state.is_null() {
            drop(Box::from_raw(so.state));
            so.state = std::ptr::null_mut();
        }
        generic_free_and_type_decref(o, tp);
    }
}

pub fn basicsize<T>() -> isize {
    std::mem::size_of::<StateObj<T>>() as isize
}

/// Fill the common slots of a state-carrying type.
pub unsafe fn init_state_type<T>(t: *mut PyTypeObject, name: &'static std::ffi::CStr, flags: u64) {
    unsafe {
        let t = &mut *t;
        t.tp_name = name.as_ptr();
        t.tp_basicsize = basicsize::<T>();
        t.tp_flags = (Py_TPFLAGS_DEFAULT as u64 | flags) as _;
        t.tp_dealloc = Some(state_dealloc::<T>);
        t.tp_alloc = Some(generic_alloc);
        t.tp_getattro = Some(object_generic_getattro);
    }
}

/// Run `f` with the thread in blocked mode when the work is large enough to matter.
#[inline]
pub fn maybe_blocking<R>(work: usize, f: impl FnOnce() -> R) -> R {
    if work >= 64 * 1024 { crate::threads::blocking(f) } else { f() }
}

// ------------------------------------------------------------------ arguments

pub fn buffer(o: &PyObject) -> PyResult<Buf> {
    Buf::get(o, false)
}

/// Py_ssize_t conversion ("n" format): OverflowError for huge values.
pub fn ssize(o: &PyObject) -> PyResult<isize> {
    if int::is_int(o) && int::is_compact(o) {
        return Ok(int::compact_value(o) as isize);
    }
    if !int::is_int(o) && !ab::has_index(o) {
        raise!(TypeError, "'{}' object cannot be interpreted as an integer", tname(o));
    }
    let i = ab::index(o)?;
    match int::as_i64(&i) {
        Some(v) => Ok(v as isize),
        None => raise!(OverflowError, "Python int too large to convert to C ssize_t"),
    }
}

/// C int conversion ("i" format).
pub fn c_int(o: &PyObject) -> PyResult<i32> {
    if !int::is_int(o) && !ab::has_index(o) {
        raise!(TypeError, "'{}' object cannot be interpreted as an integer", tname(o));
    }
    let i = ab::index(o)?;
    match int::as_i64(&i) {
        Some(v) if v > i32::MAX as i64 => raise!(OverflowError, "signed integer is greater than maximum"),
        Some(v) if v < i32::MIN as i64 => raise!(OverflowError, "signed integer is less than minimum"),
        Some(v) => Ok(v as i32),
        None => raise!(OverflowError, "Python int too large to convert to C long"),
    }
}

/// unsigned int with wrap-around ("I" format, no overflow checking).
pub fn c_uint_wrap(o: &PyObject) -> PyResult<u32> {
    if !int::is_int(o) && !ab::has_index(o) {
        raise!(TypeError, "'{}' object cannot be interpreted as an integer", tname(o));
    }
    let i = ab::index(o)?;
    Ok(rpy_bigint::conv::to_u64_wrapping(crate::types::int_methods::iref(&i)) as u32)
}

/// unsigned long long with range checking.
pub fn c_u64(o: &PyObject) -> PyResult<u64> {
    if !int::is_int(o) && !ab::has_index(o) {
        raise!(TypeError, "'{}' object cannot be interpreted as an integer", tname(o));
    }
    let i = ab::index(o)?;
    if int::sign(&i) < 0 {
        raise!(OverflowError, "can't convert negative int to unsigned");
    }
    match rpy_bigint::conv::to_u64(crate::types::int_methods::iref(&i)) {
        Some(v) => Ok(v),
        None => raise!(OverflowError, "int too big to convert"),
    }
}

pub fn new_bytes(d: &[u8]) -> PyResult {
    bytes::try_new_bytes(d)
}

pub fn is_none(o: &PyObject) -> bool {
    o.is(crate::types::none::none())
}

pub fn none() -> PyRef {
    crate::types::none::none_ref()
}

/// Create an exception class `module.name(base)`.
pub fn new_exception(module: &str, name: &str, base: *mut PyTypeObject) -> PyResult {
    let ns = dict::new_dict();
    dict::set_item_str(&ns, "__module__", &pystr::new_str(module))?;
    let bases = tuple::new_tuple(vec![type_ref(base)]);
    ab::call(type_obj(type_type()), &[&pystr::new_str(name), &bases, &ns])
}

/// A process-wide exception class created once.
pub struct ExcCell(std::sync::OnceLock<usize>);

impl ExcCell {
    pub const fn new() -> ExcCell {
        ExcCell(std::sync::OnceLock::new())
    }
    pub fn get_or_create(&self, module: &str, name: &str, base: *mut PyTypeObject) -> PyResult<*mut PyTypeObject> {
        if let Some(p) = self.0.get() {
            return Ok(*p as *mut PyTypeObject);
        }
        let e = new_exception(module, name, base)?;
        let p = e.into_raw() as usize;
        Ok(*self.0.get_or_init(|| p) as *mut PyTypeObject)
    }
    pub fn ptr(&self) -> *mut PyTypeObject {
        self.0.get().map(|p| *p as *mut PyTypeObject).unwrap_or(std::ptr::null_mut())
    }
}

pub fn raise_on(tp: *mut PyTypeObject, msg: &str) -> PyErr {
    new_err(tp, msg)
}

pub fn set_int(d: &PyObject, name: &str, v: i64) -> PyResult<()> {
    dict::set_item_str(d, name, &int::new_int(v))
}

pub fn set_str(d: &PyObject, name: &str, v: &str) -> PyResult<()> {
    dict::set_item_str(d, name, &pystr::new_str(v))
}

/// `__reduce__` that refuses pickling.
pub fn no_pickle(o: &PyObject) -> PyResult {
    raise!(TypeError, "cannot pickle '{}' object", tname(o));
}
