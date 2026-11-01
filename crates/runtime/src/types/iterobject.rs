//! Sequence iterator (iter() over __getitem__) and callable iterator.
use crate::abstract_ as ab;
use crate::ffi::*;
use crate::object::*;
use crate::types::exceptions as exc;
use std::ffi::{c_int, c_void};

#[repr(C)]
pub struct SeqIter {
    pub ob_base: PyObject,
    pub index: isize,
    pub seq: *mut PyObject,
}
#[unsafe(no_mangle)]
pub static mut PySeqIter_Type: PyTypeObject = PyTypeObject::ZERO;
pub fn seqiter_type() -> *mut PyTypeObject {
    &raw mut PySeqIter_Type
}
pub fn new_seq_iter(seq: PyRef) -> PyRef {
    unsafe {
        let op = crate::alloc::alloc_object(seqiter_type(), std::mem::size_of::<SeqIter>());
        (*(op as *mut SeqIter)).seq = seq.into_raw();
        crate::gc::track(op);
        PyRef::from_raw(op)
    }
}
unsafe extern "C" fn seqiter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let it = &mut *(o as *mut SeqIter);
        if it.seq.is_null() {
            return std::ptr::null_mut();
        }
        let idx = crate::types::int::new_int(it.index as i64);
        match ab::getitem(&*it.seq, &idx) {
            Ok(v) => {
                it.index += 1;
                v.into_raw()
            }
            Err(_) => {
                if crate::errors::exc_matches(exc::IndexError()) || crate::errors::exc_matches(exc::StopIteration()) {
                    crate::errors::clear_exc();
                    let s = std::mem::replace(&mut it.seq, std::ptr::null_mut());
                    xdecref(s);
                }
                std::ptr::null_mut()
            }
        }
    }
}
unsafe extern "C" fn self_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}
unsafe extern "C" fn seqiter_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        xdecref((*(o as *mut SeqIter)).seq);
        crate::alloc::free_object(o);
    }
}
unsafe extern "C" fn seqiter_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let s = (*(o as *mut SeqIter)).seq;
        if s.is_null() { 0 } else { visit.unwrap()(s, arg) }
    }
}
pub fn init_types() {
    unsafe {
        let t = &mut *seqiter_type();
        t.tp_name = c"iterator".as_ptr();
        t.tp_basicsize = std::mem::size_of::<SeqIter>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
        t.tp_dealloc = Some(seqiter_dealloc);
        t.tp_traverse = Some(seqiter_traverse);
        t.tp_iter = Some(self_iter);
        t.tp_iternext = Some(seqiter_next);
        t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
    }
}

/// iter(callable, sentinel)
#[repr(C)]
pub struct CallIter {
    pub ob_base: PyObject,
    pub func: *mut PyObject,
    pub sentinel: *mut PyObject,
}
#[unsafe(no_mangle)]
pub static mut PyCallIter_Type: PyTypeObject = PyTypeObject::ZERO;
pub fn new_callable_iter(f: &PyObject, sentinel: &PyObject) -> crate::errors::PyResult {
    if !ab::is_callable(f) {
        crate::raise!(TypeError, "iter(v, w): v must be callable");
    }
    unsafe {
        let t = &raw mut PyCallIter_Type;
        if (*t).tp_name.is_null() {
            let tt = &mut *t;
            tt.tp_name = c"callable_iterator".as_ptr();
            tt.tp_basicsize = std::mem::size_of::<CallIter>() as isize;
            tt.tp_flags = Py_TPFLAGS_DEFAULT;
            tt.tp_iter = Some(self_iter);
            tt.tp_iternext = Some(calliter_next);
            tt.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
            crate::types::typeobject::type_ready(t)?;
        }
        let op = crate::alloc::alloc_object(t, std::mem::size_of::<CallIter>());
        (*(op as *mut CallIter)).func = f.to_ref().into_raw();
        (*(op as *mut CallIter)).sentinel = sentinel.to_ref().into_raw();
        Ok(PyRef::from_raw(op))
    }
}
unsafe extern "C" fn calliter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let it = &mut *(o as *mut CallIter);
        if it.func.is_null() {
            return std::ptr::null_mut();
        }
        match ab::call(&*it.func, &[]) {
            Ok(v) => match ab::rich_eq(&v, &*it.sentinel) {
                Ok(true) => {
                    let f = std::mem::replace(&mut it.func, std::ptr::null_mut());
                    decref(f);
                    std::ptr::null_mut()
                }
                Ok(false) => v.into_raw(),
                Err(_) => std::ptr::null_mut(),
            },
            Err(_) => {
                if crate::errors::exc_matches(exc::StopIteration()) {
                    crate::errors::clear_exc();
                    let f = std::mem::replace(&mut it.func, std::ptr::null_mut());
                    decref(f);
                }
                std::ptr::null_mut()
            }
        }
    }
}
