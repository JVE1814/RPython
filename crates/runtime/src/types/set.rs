//! `set` / `frozenset` core. Layout prefix matches the public PySetObject.
//! Phase-1 storage: a private dict (keys -> None) referenced from `table`.
//! (A dedicated open-addressing set table replaces this later.)

use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::{dict, none};

#[repr(C)]
pub struct SetEntry {
    pub key: *mut PyObject,
    pub hash: Py_hash_t,
}

#[repr(C)]
pub struct PySetObject {
    pub ob_base: PyObject,
    pub fill: Py_ssize_t,
    pub used: Py_ssize_t,
    pub mask: Py_ssize_t,
    pub table: *mut SetEntry,
    pub hash: Py_hash_t,
    pub finger: Py_ssize_t,
    pub smalltable: [SetEntry; 8],
    pub weakreflist: *mut PyObject,
    /// private storage (phase 1)
    pub storage: *mut PyObject,
}

#[unsafe(no_mangle)]
pub static mut PySet_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyFrozenSet_Type: PyTypeObject = PyTypeObject::ZERO;

pub fn set_type() -> *mut PyTypeObject {
    &raw mut PySet_Type
}
pub fn frozenset_type() -> *mut PyTypeObject {
    &raw mut PyFrozenSet_Type
}
pub fn is_set(o: &PyObject) -> bool {
    o.is_type(set_type()) || crate::types::typeobject::is_subtype(o.type_ptr(), set_type())
}
pub fn is_frozenset(o: &PyObject) -> bool {
    o.is_type(frozenset_type()) || crate::types::typeobject::is_subtype(o.type_ptr(), frozenset_type())
}
pub fn is_anyset(o: &PyObject) -> bool {
    is_set(o) || is_frozenset(o)
}
pub fn so(o: &PyObject) -> &mut PySetObject {
    unsafe { &mut *(o.as_ptr() as *mut PySetObject) }
}
pub fn storage(o: &PyObject) -> &PyObject {
    let s = so(o);
    if s.storage.is_null() {
        s.storage = dict::new_dict().into_raw();
    }
    unsafe { &*s.storage }
}
fn sync(o: &PyObject) {
    let n = dict::dict_len(storage(o)) as isize;
    let s = so(o);
    s.used = n;
    s.fill = n;
}

pub fn alloc_set(tp: *mut PyTypeObject) -> PyResult {
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(crate::types::typeobject::generic_alloc);
        let op = alloc(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        let s = &mut *(op as *mut PySetObject);
        s.hash = -1;
        s.mask = 7;
        s.table = s.smalltable.as_mut_ptr();
        s.storage = dict::new_dict().into_raw();
        Ok(PyRef::from_raw(op))
    }
}

pub fn new_set_from_vec(items: Vec<PyRef>) -> PyResult {
    let s = alloc_set(set_type())?;
    for it in items {
        set_add(&s, &it)?;
    }
    Ok(s)
}
pub fn new_frozenset_from_vec(items: Vec<PyRef>) -> PyResult {
    let s = alloc_set(frozenset_type())?;
    for it in items {
        set_add(&s, &it)?;
    }
    Ok(s)
}
pub fn set_add(s: &PyObject, k: &PyObject) -> PyResult<()> {
    dict::set_item(storage(s), k, none::none())?;
    sync(s);
    Ok(())
}
pub fn set_discard(s: &PyObject, k: &PyObject) -> PyResult<bool> {
    let r = dict::del_item(storage(s), k)?;
    sync(s);
    Ok(r)
}
pub fn set_contains(s: &PyObject, k: &PyObject) -> PyResult<bool> {
    dict::contains(storage(s), k)
}
pub fn set_update(s: &PyObject, it: &PyObject) -> PyResult<()> {
    if is_anyset(it) {
        for k in dict::keys_vec(storage(it)) {
            set_add(s, &k)?;
        }
        return Ok(());
    }
    if dict::is_dict(it) {
        for k in dict::keys_vec(it) {
            set_add(s, &k)?;
        }
        return Ok(());
    }
    for k in crate::abstract_::collect_iterable(it)? {
        set_add(s, &k)?;
    }
    Ok(())
}
pub fn set_items(s: &PyObject) -> Vec<PyRef> {
    dict::keys_vec(storage(s))
}
pub fn set_len(s: &PyObject) -> usize {
    dict::dict_len(storage(s))
}
