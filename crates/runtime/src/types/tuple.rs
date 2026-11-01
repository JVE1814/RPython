//! `tuple` core: layout `PyTupleObject` (ob_item inline), creation and access.

use crate::alloc;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;

#[unsafe(no_mangle)]
pub static mut PyTuple_Type: PyTypeObject = PyTypeObject::ZERO;

#[inline(always)]
pub fn tuple_type() -> *mut PyTypeObject {
    &raw mut PyTuple_Type
}
#[inline(always)]
pub fn is_tuple(o: &PyObject) -> bool {
    o.is_type(tuple_type()) || o.ty().tp_flags & Py_TPFLAGS_TUPLE_SUBCLASS != 0
}
#[inline(always)]
pub fn is_tuple_exact(o: &PyObject) -> bool {
    o.is_type(tuple_type())
}

const HEADER: usize = std::mem::offset_of!(PyTupleObject, ob_item);

/// Allocate a tuple of type `tp` with `n` NULL items (caller must fill all).
pub unsafe fn alloc_tuple_of(tp: *mut PyTypeObject, n: usize) -> *mut PyObject {
    unsafe {
        let basic = (*tp).tp_basicsize as usize;
        let size = basic.max(HEADER) + n * std::mem::size_of::<*mut PyObject>();
        let op = alloc::alloc_object(tp, size);
        set_var_size(op, n as isize);
        (*(op as *mut PyTupleObject)).ob_hash = -1;
        op
    }
}

pub fn empty_tuple() -> PyRef {
    static EMPTY: std::sync::atomic::AtomicPtr<PyObject> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
    let p = EMPTY.load(std::sync::atomic::Ordering::Acquire);
    if !p.is_null() {
        return unsafe { PyRef::from_borrowed(p) };
    }
    unsafe {
        let op = alloc_tuple_of(tuple_type(), 0);
        make_immortal(op);
        match EMPTY.compare_exchange(std::ptr::null_mut(), op, std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire) {
            Ok(_) => PyRef::from_raw(op),
            Err(o) => PyRef::from_borrowed(o),
        }
    }
}

/// Create a tuple from owned references.
pub fn new_tuple(items: Vec<PyRef>) -> PyRef {
    if items.is_empty() {
        return empty_tuple();
    }
    unsafe {
        let n = items.len();
        let op = alloc_tuple_of(tuple_type(), n);
        let base = item_ptr(op);
        for (i, it) in items.into_iter().enumerate() {
            *base.add(i) = it.into_raw();
        }
        crate::gc::track(op);
        PyRef::from_raw(op)
    }
}

pub fn new_tuple_from_slice(items: &[PyRef]) -> PyRef {
    new_tuple(items.to_vec())
}

/// Create from borrowed raw pointers.
pub unsafe fn new_tuple_from_ptrs(items: &[*mut PyObject]) -> PyRef {
    if items.is_empty() {
        return empty_tuple();
    }
    unsafe {
        let op = alloc_tuple_of(tuple_type(), items.len());
        let base = item_ptr(op);
        for (i, &it) in items.iter().enumerate() {
            incref(it);
            *base.add(i) = it;
        }
        crate::gc::track(op);
        PyRef::from_raw(op)
    }
}

pub fn tuple2(a: PyRef, b: PyRef) -> PyRef {
    new_tuple(vec![a, b])
}

#[inline(always)]
pub unsafe fn item_ptr(op: *mut PyObject) -> *mut *mut PyObject {
    unsafe { (op as *mut u8).add(HEADER) as *mut *mut PyObject }
}

#[inline(always)]
pub fn tuple_len(t: &PyObject) -> usize {
    var_size(t) as usize
}

/// Borrowed item slice.
#[inline(always)]
pub fn tuple_items(t: &PyObject) -> &[&PyObject] {
    unsafe { std::slice::from_raw_parts(item_ptr(t.as_ptr()) as *const &PyObject, tuple_len(t)) }
}

#[inline(always)]
pub fn tuple_get(t: &PyObject, i: usize) -> &PyObject {
    tuple_items(t)[i]
}

pub fn tuple_to_vec(t: &PyObject) -> Vec<PyRef> {
    tuple_items(t).iter().map(|o| o.to_ref()).collect()
}

pub unsafe extern "C" fn tuple_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        let n = var_size(&*op) as usize;
        let base = item_ptr(op);
        for i in 0..n {
            xdecref(*base.add(i));
        }
        let tp = (*op).type_ptr();
        crate::types::typeobject::generic_free_and_type_decref(op, tp);
    }
}

pub unsafe extern "C" fn tuple_traverse(op: *mut PyObject, visit: visitproc, arg: *mut std::ffi::c_void) -> std::ffi::c_int {
    unsafe {
        let n = var_size(&*op) as usize;
        let base = item_ptr(op);
        let f = visit.unwrap();
        for i in 0..n {
            let it = *base.add(i);
            if !it.is_null() {
                let r = f(it, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        0
    }
}

pub fn tuple_hash(t: &PyObject) -> PyResult<isize> {
    let items = tuple_items(t);
    let mut h = crate::hash::TupleHasher::new(items.len());
    for it in items {
        h.add(crate::abstract_::hash(it)?);
    }
    Ok(h.finish())
}
