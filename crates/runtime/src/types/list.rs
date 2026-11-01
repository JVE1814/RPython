//! `list` core: `PyListObject` with a separately allocated item array.
//! Methods live in `list_methods.rs`.

use crate::alloc;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;

#[unsafe(no_mangle)]
pub static mut PyList_Type: PyTypeObject = PyTypeObject::ZERO;

#[inline(always)]
pub fn list_type() -> *mut PyTypeObject {
    &raw mut PyList_Type
}
#[inline(always)]
pub fn is_list(o: &PyObject) -> bool {
    o.is_type(list_type()) || o.ty().tp_flags & Py_TPFLAGS_LIST_SUBCLASS != 0
}
#[inline(always)]
pub fn is_list_exact(o: &PyObject) -> bool {
    o.is_type(list_type())
}

#[inline(always)]
fn lp(o: &PyObject) -> *mut PyListObject {
    o.as_ptr() as *mut PyListObject
}

#[inline(always)]
pub fn list_len(o: &PyObject) -> usize {
    var_size(o) as usize
}

/// Borrowed view of the items. Valid until the list is mutated.
#[inline(always)]
pub fn list_items(o: &PyObject) -> &[&PyObject] {
    unsafe {
        let l = lp(o);
        let n = (*l).ob_base.ob_size as usize;
        if n == 0 {
            return &[];
        }
        std::slice::from_raw_parts((*l).ob_item as *const &PyObject, n)
    }
}

pub fn list_to_vec(o: &PyObject) -> Vec<PyRef> {
    list_items(o).iter().map(|x| x.to_ref()).collect()
}

pub fn new_list(items: Vec<PyRef>) -> PyRef {
    unsafe {
        let op = alloc::alloc_object(list_type(), std::mem::size_of::<PyListObject>());
        let l = op as *mut PyListObject;
        let n = items.len();
        if n > 0 {
            let arr = alloc::raw_malloc(n * 8) as *mut *mut PyObject;
            for (i, it) in items.into_iter().enumerate() {
                *arr.add(i) = it.into_raw();
            }
            (*l).ob_item = arr;
            (*l).allocated = n as isize;
        }
        (*l).ob_base.ob_size = n as isize;
        crate::gc::track(op);
        PyRef::from_raw(op)
    }
}

pub fn new_list_empty() -> PyRef {
    new_list(Vec::new())
}

/// Ensure capacity for `n` items.
pub fn reserve(o: &PyObject, n: usize) {
    unsafe {
        let l = lp(o);
        if (*l).allocated as usize >= n {
            return;
        }
        let new_alloc = (n + (n >> 3) + if n < 9 { 3 } else { 6 }).max(4);
        let arr = alloc::raw_realloc((*l).ob_item as *mut u8, new_alloc * 8) as *mut *mut PyObject;
        (*l).ob_item = arr;
        (*l).allocated = new_alloc as isize;
    }
}

pub fn append(o: &PyObject, item: PyRef) {
    unsafe {
        let l = lp(o);
        let n = (*l).ob_base.ob_size as usize;
        if n + 1 > (*l).allocated as usize {
            reserve(o, n + 1);
        }
        *(*l).ob_item.add(n) = item.into_raw();
        (*l).ob_base.ob_size = (n + 1) as isize;
    }
}

pub fn get(o: &PyObject, i: usize) -> Option<PyRef> {
    let items = list_items(o);
    items.get(i).map(|x| x.to_ref())
}

pub fn set(o: &PyObject, i: usize, v: PyRef) {
    unsafe {
        let l = lp(o);
        let slot = (*l).ob_item.add(i);
        let old = *slot;
        *slot = v.into_raw();
        xdecref(old);
    }
}

/// Replace the full contents.
pub fn set_contents(o: &PyObject, items: Vec<PyRef>) {
    unsafe {
        let l = lp(o);
        let old_n = (*l).ob_base.ob_size as usize;
        let old_arr = (*l).ob_item;
        let n = items.len();
        let arr = if n > 0 { alloc::raw_malloc(n * 8) as *mut *mut PyObject } else { std::ptr::null_mut() };
        for (i, it) in items.into_iter().enumerate() {
            *arr.add(i) = it.into_raw();
        }
        (*l).ob_item = arr;
        (*l).allocated = n as isize;
        (*l).ob_base.ob_size = n as isize;
        for i in 0..old_n {
            xdecref(*old_arr.add(i));
        }
        if !old_arr.is_null() {
            alloc::raw_free(old_arr as *mut u8);
        }
    }
}

/// Replace items[lo..hi] with `items`.
pub fn splice(o: &PyObject, lo: usize, hi: usize, items: Vec<PyRef>) {
    let mut v: Vec<PyRef> = list_to_vec(o);
    let removed: Vec<PyRef> = v.splice(lo..hi, items).collect();
    set_contents(o, v);
    drop(removed);
}

pub fn pop(o: &PyObject, i: usize) -> PyRef {
    unsafe {
        let l = lp(o);
        let n = (*l).ob_base.ob_size as usize;
        let arr = (*l).ob_item;
        let it = *arr.add(i);
        std::ptr::copy(arr.add(i + 1), arr.add(i), n - i - 1);
        (*l).ob_base.ob_size = (n - 1) as isize;
        PyRef::from_raw(it)
    }
}

pub fn insert(o: &PyObject, i: usize, v: PyRef) {
    unsafe {
        let l = lp(o);
        let n = (*l).ob_base.ob_size as usize;
        reserve(o, n + 1);
        let arr = (*l).ob_item;
        std::ptr::copy(arr.add(i), arr.add(i + 1), n - i);
        *arr.add(i) = v.into_raw();
        (*l).ob_base.ob_size = (n + 1) as isize;
    }
}

pub fn clear(o: &PyObject) {
    set_contents(o, Vec::new());
}

pub unsafe extern "C" fn list_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        let l = op as *mut PyListObject;
        let n = (*l).ob_base.ob_size as usize;
        let arr = (*l).ob_item;
        for i in 0..n {
            xdecref(*arr.add(i));
        }
        if !arr.is_null() {
            alloc::raw_free(arr as *mut u8);
        }
        let tp = (*op).type_ptr();
        crate::types::typeobject::generic_free_and_type_decref(op, tp);
    }
}

pub unsafe extern "C" fn list_traverse(op: *mut PyObject, visit: visitproc, arg: *mut std::ffi::c_void) -> std::ffi::c_int {
    unsafe {
        let l = op as *mut PyListObject;
        let n = (*l).ob_base.ob_size as usize;
        let f = visit.unwrap();
        for i in 0..n {
            let it = *(*l).ob_item.add(i);
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

pub unsafe extern "C" fn list_clear_slot(op: *mut PyObject) -> std::ffi::c_int {
    unsafe { clear(&*op) };
    0
}

pub fn list_from_iter(it: &PyObject) -> PyResult<PyRef> {
    let v = crate::abstract_::collect_iterable(it)?;
    Ok(new_list(v))
}
