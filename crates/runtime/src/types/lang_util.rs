//! Small helpers shared by the "lang" workstream's object types (typing objects,
//! GenericAlias, UnionType, async generator helpers): objects whose layout is the
//! object header followed only by `*mut PyObject` fields (null allowed). The field at
//! `tp_weaklistoffset` (if any) is the weakref list and is handled specially.
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::*;
use std::ffi::{c_int, c_void};

pub const HEADER: usize = std::mem::size_of::<PyObject>();

/// Nearest static base type (the one that defines the pointer layout).
unsafe fn static_base(tp: *mut PyTypeObject) -> *mut PyTypeObject {
    let mut b = tp;
    unsafe {
        while is_heap_type(b) && !(*b).tp_base.is_null() {
            b = (*b).tp_base;
        }
    }
    b
}

unsafe fn ptr_slots(op: *mut PyObject) -> (*mut *mut PyObject, usize, isize) {
    unsafe {
        let b = static_base((*op).type_ptr());
        let n = ((*b).tp_basicsize as usize - HEADER) / 8;
        ((op as *mut u8).add(HEADER) as *mut *mut PyObject, n, (*b).tp_weaklistoffset)
    }
}

pub unsafe extern "C" fn ptrs_dealloc(op: *mut PyObject) {
    unsafe {
        let tp = (*op).type_ptr();
        if (*tp).tp_finalize.is_some() && !crate::gc::is_finalized(op) && crate::gc::call_finalizer_from_dealloc(op) {
            return;
        }
        ptrs_dealloc_unfinalized(op);
    }
}

/// (jit6) `ptrs_dealloc` for an object whose finalizer has nothing to do (the caller
/// knows; asyncgen.rs: an awaitable that already completed).
pub unsafe fn ptrs_dealloc_unfinalized(op: *mut PyObject) {
    unsafe {
        let tp = (*op).type_ptr();
        crate::gc::untrack(op);
        let (p, n, wl) = ptr_slots(op);
        if wl > 0 {
            crate::types::weakref::clear_weakrefs_at(op, (op as *mut u8).offset(wl) as *mut *mut PyObject);
        }
        for i in 0..n {
            if wl > 0 && (HEADER + i * 8) as isize == wl {
                continue;
            }
            let s = p.add(i);
            let old = *s;
            *s = std::ptr::null_mut();
            xdecref(old);
        }
        generic_free_and_type_decref(op, tp);
    }
}

pub unsafe extern "C" fn ptrs_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let (p, n, wl) = ptr_slots(op);
        let v = visit.unwrap();
        for i in 0..n {
            if wl > 0 && (HEADER + i * 8) as isize == wl {
                continue;
            }
            let x = *p.add(i);
            if !x.is_null() {
                let r = v(x, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        0
    }
}

pub unsafe extern "C" fn ptrs_clear(op: *mut PyObject) -> c_int {
    unsafe {
        let (p, n, wl) = ptr_slots(op);
        for i in 0..n {
            if wl > 0 && (HEADER + i * 8) as isize == wl {
                continue;
            }
            let s = p.add(i);
            let old = *s;
            *s = std::ptr::null_mut();
            xdecref(old);
        }
        0
    }
}

/// Fill the common parts of a pointer-field type.
pub unsafe fn setup_ptr_type(tp: *mut PyTypeObject, name: &str, size: usize, flags: u64) {
    unsafe {
        let t = &mut *tp;
        t.tp_name = crate::methods::cstr_static(name);
        t.tp_basicsize = size as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_IMMUTABLETYPE | flags;
        t.tp_dealloc = Some(ptrs_dealloc);
        t.tp_traverse = Some(ptrs_traverse);
        t.tp_clear = Some(ptrs_clear);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_setattro = Some(object_generic_setattro);
        t.tp_alloc = Some(generic_alloc);
        t.tp_free = Some(generic_free);
    }
}

/// type_ready, marking types without tp_new as not instantiable from Python.
pub fn ready_type(tp: *mut PyTypeObject) -> PyResult<()> {
    unsafe {
        if (*tp).tp_new.is_none() {
            (*tp).tp_flags |= Py_TPFLAGS_DISALLOW_INSTANTIATION;
        }
    }
    type_ready(tp)
}

/// Allocate an instance of `tp` (via tp_alloc).
pub fn alloc(tp: *mut PyTypeObject) -> PyResult {
    unsafe {
        let a = (*tp).tp_alloc.unwrap_or(generic_alloc);
        from_c(a(tp, 0))
    }
}

/// Field `i` (0-based pointer slot after the header).
#[inline]
pub fn field(o: &PyObject, i: usize) -> Option<&PyObject> {
    unsafe {
        let p = *((o.as_ptr() as *mut u8).add(HEADER + i * 8) as *mut *mut PyObject);
        if p.is_null() { None } else { Some(&*p) }
    }
}
#[inline]
pub fn field_ref(o: &PyObject, i: usize) -> Option<PyRef> {
    field(o, i).map(|x| x.to_ref())
}
pub fn set_field(o: &PyObject, i: usize, v: Option<PyRef>) {
    unsafe {
        let s = (o.as_ptr() as *mut u8).add(HEADER + i * 8) as *mut *mut PyObject;
        let old = std::mem::replace(&mut *s, v.map_or(std::ptr::null_mut(), |x| x.into_raw()));
        xdecref(old);
    }
}

/// Set a function into a type's dict as a classmethod descriptor (for builtin types
/// defined elsewhere, after they are ready).
pub fn add_classmethod(tp: *mut PyTypeObject, ml: *mut PyMethodDef) {
    let d = crate::types::descr::new_classmethod_descr(tp, ml);
    let nm = crate::methods::method_name(ml);
    type_dict_set(tp, &nm, &d);
}
pub fn add_method(tp: *mut PyTypeObject, ml: *mut PyMethodDef) {
    let d = crate::types::descr::new_method_descr(tp, ml);
    let nm = crate::methods::method_name(ml);
    type_dict_set(tp, &nm, &d);
}

/// Import a module and get an attribute (used to call into typing.py).
pub fn import_attr(module: &str, attr: &str) -> PyResult {
    let m = crate::import::import_absolute(module)?;
    crate::abstract_::getattr_str(&m, attr)
}

/// repr() as a Rust String.
pub fn repr_s(o: &PyObject) -> PyResult<String> {
    crate::abstract_::repr_string(o)
}
