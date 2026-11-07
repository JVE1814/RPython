//! Sequence iterator (iter() over __getitem__) and callable iterator (iter(f, sentinel)).
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::exceptions as exc;
use crate::types::{int, none, tuple};
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
        if it.index == isize::MAX {
            return into_c(Err(crate::err!(OverflowError, "iter index too large")));
        }
        let seq = &*it.seq;
        let tp = seq.ty();
        let r = if !tp.tp_as_sequence.is_null() && (*tp.tp_as_sequence).sq_item.is_some() {
            from_c((*tp.tp_as_sequence).sq_item.unwrap()(seq.as_ptr(), it.index))
        } else {
            ab::getitem(seq, &int::new_int(it.index as i64))
        };
        match r {
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
fn si(o: &PyObject) -> &mut SeqIter {
    unsafe { &mut *(o.as_ptr() as *mut SeqIter) }
}
fn seqiter_len(o: &PyObject) -> PyResult {
    let it = si(o);
    if it.seq.is_null() {
        return Ok(int::new_int(0));
    }
    match ab::length(unsafe { &*it.seq }) {
        Ok(n) => Ok(int::new_int((n as isize - it.index).max(0) as i64)),
        Err(e) => {
            if exc_matches(exc::TypeError()) {
                clear_exc();
                return Ok(none::not_implemented().to_ref());
            }
            Err(e)
        }
    }
}
fn seqiter_reduce(o: &PyObject) -> PyResult {
    let it = si(o);
    let iter_fn = crate::builtins::eval_builtin("iter")?;
    if it.seq.is_null() {
        return Ok(tuple::new_tuple(vec![iter_fn, tuple::new_tuple(vec![tuple::empty_tuple()])]));
    }
    Ok(tuple::new_tuple(vec![iter_fn, tuple::new_tuple(vec![unsafe { PyRef::from_borrowed(it.seq) }]), int::new_int(it.index as i64)]))
}
fn seqiter_setstate(o: &PyObject, state: &PyObject) -> PyResult {
    let it = si(o);
    let idx = ab::index_as_ssize(state, None)?;
    if !it.seq.is_null() {
        it.index = idx.max(0);
    }
    Ok(none::none_ref())
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
pub fn new_callable_iter(f: &PyObject, sentinel: &PyObject) -> PyResult {
    if !ab::is_callable(f) {
        crate::raise!(TypeError, "iter(v, w): v must be callable");
    }
    unsafe {
        let op = crate::alloc::alloc_object(&raw mut PyCallIter_Type, std::mem::size_of::<CallIter>());
        (*(op as *mut CallIter)).func = f.to_ref().into_raw();
        (*(op as *mut CallIter)).sentinel = sentinel.to_ref().into_raw();
        crate::gc::track(op);
        Ok(PyRef::from_raw(op))
    }
}
unsafe extern "C" fn calliter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let it = &mut *(o as *mut CallIter);
        if it.func.is_null() {
            return std::ptr::null_mut();
        }
        let f = PyRef::from_borrowed(it.func);
        match ab::call(&f, &[]) {
            Ok(v) => {
                if it.sentinel.is_null() {
                    // exhausted re-entrantly during the call
                    drop(v);
                    return std::ptr::null_mut();
                }
                let s = PyRef::from_borrowed(it.sentinel);
                match ab::rich_compare_bool(&s, &v, Py_EQ) {
                    Ok(true) => {
                        let f = std::mem::replace(&mut it.func, std::ptr::null_mut());
                        let s = std::mem::replace(&mut it.sentinel, std::ptr::null_mut());
                        xdecref(f);
                        xdecref(s);
                        std::ptr::null_mut()
                    }
                    Ok(false) => v.into_raw(),
                    Err(_) => std::ptr::null_mut(),
                }
            }
            Err(_) => {
                if crate::errors::exc_matches(exc::StopIteration()) {
                    crate::errors::clear_exc();
                    let f = std::mem::replace(&mut it.func, std::ptr::null_mut());
                    let s = std::mem::replace(&mut it.sentinel, std::ptr::null_mut());
                    xdecref(f);
                    xdecref(s);
                }
                std::ptr::null_mut()
            }
        }
    }
}
unsafe extern "C" fn calliter_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let it = &*(o as *mut CallIter);
        xdecref(it.func);
        xdecref(it.sentinel);
        crate::alloc::free_object(o);
    }
}
unsafe extern "C" fn calliter_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let it = &*(o as *mut CallIter);
        let f = visit.unwrap();
        for p in [it.func, it.sentinel] {
            if !p.is_null() {
                let r = f(p, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        0
    }
}
fn calliter_reduce(o: &PyObject) -> PyResult {
    let it = unsafe { &*(o.as_ptr() as *mut CallIter) };
    let iter_fn = crate::builtins::eval_builtin("iter")?;
    if it.func.is_null() || it.sentinel.is_null() {
        return Ok(tuple::new_tuple(vec![iter_fn, tuple::new_tuple(vec![tuple::empty_tuple()])]));
    }
    Ok(tuple::new_tuple(vec![
        iter_fn,
        tuple::new_tuple(vec![unsafe { PyRef::from_borrowed(it.func) }, unsafe { PyRef::from_borrowed(it.sentinel) }]),
    ]))
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
        t.tp_methods = crate::mdefs![
            ("__length_hint__", NOARGS, seqiter_len, "Private method returning an estimate of len(list(it))."),
            ("__reduce__", NOARGS, seqiter_reduce, "Return state information for pickling."),
            ("__setstate__", O, seqiter_setstate, "Set state information for unpickling."),
        ];
        let t = &mut *(&raw mut PyCallIter_Type);
        t.tp_name = c"callable_iterator".as_ptr();
        t.tp_basicsize = std::mem::size_of::<CallIter>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
        t.tp_dealloc = Some(calliter_dealloc);
        t.tp_traverse = Some(calliter_traverse);
        t.tp_iter = Some(self_iter);
        t.tp_iternext = Some(calliter_next);
        t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
        t.tp_methods = crate::mdefs![("__reduce__", NOARGS, calliter_reduce, "Return state information for pickling.")];
    }
}

pub fn ready() -> PyResult<()> {
    crate::types::typeobject::type_ready(&raw mut PyCallIter_Type)
}
