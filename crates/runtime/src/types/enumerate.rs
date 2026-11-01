//! `enumerate` and `reversed`.
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::*;
use crate::types::{dict, exceptions, int, none, tuple};
use std::ffi::{c_int, c_void};

#[unsafe(no_mangle)]
pub static mut PyEnum_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyReversed_Type: PyTypeObject = PyTypeObject::ZERO;

// ================================================================== enumerate

#[repr(C)]
struct EnumObject {
    ob_base: PyObject,
    index: i64,
    it: *mut PyObject,
    result: *mut PyObject,
    longindex: *mut PyObject,
}
fn en(o: &PyObject) -> &mut EnumObject {
    unsafe { &mut *(o.as_ptr() as *mut EnumObject) }
}

fn alloc_of(tp: *mut PyTypeObject) -> PyResult {
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
        let op = alloc(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        Ok(PyRef::from_raw(op))
    }
}

pub fn new_enumerate(tp: *mut PyTypeObject, iterable: &PyObject, start: Option<&PyObject>) -> PyResult {
    let it = ab::get_iter(iterable)?;
    let (index, longindex) = match start {
        None => (0i64, None),
        Some(s) => {
            let s = ab::index(s)?;
            match int::as_i64(&s) {
                Some(v) if v < i64::MAX => (v, None),
                _ => (i64::MAX, Some(s)),
            }
        }
    };
    let o = alloc_of(tp)?;
    let e = en(&o);
    e.index = index;
    e.it = it.into_raw();
    e.longindex = longindex.map_or(std::ptr::null_mut(), |x| x.into_raw());
    e.result = tuple::new_tuple(vec![none::none_ref(), none::none_ref()]).into_raw();
    Ok(o)
}

unsafe extern "C" fn enum_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let a = tuple::tuple_items(&*args);
            let mut iterable = a.first().copied();
            let mut start = a.get(1).copied();
            if a.len() > 2 {
                raise!(TypeError, "enumerate() takes at most 2 arguments ({} given)", a.len() + if kw.is_null() { 0 } else { dict::dict_len(&*kw) });
            }
            let kws = if kw.is_null() { Vec::new() } else { dict::items_vec(&*kw) };
            for (k, v) in kws.iter() {
                let ks = crate::types::str::to_string_lossy(k);
                match ks.as_str() {
                    "iterable" if iterable.is_none() => iterable = Some(&**v),
                    "start" if start.is_none() => start = Some(&**v),
                    "iterable" | "start" => raise!(TypeError, "argument for enumerate() given by name ('{}') and position", ks),
                    _ => raise!(TypeError, "'{}' is an invalid keyword argument for enumerate()", ks),
                }
            }
            let iterable = match iterable {
                Some(x) => x,
                None => raise!(TypeError, "enumerate() missing required argument 'iterable'"),
            };
            new_enumerate(tp, iterable, start)
        })();
        into_c(r)
    }
}
unsafe extern "C" fn enum_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let e = &mut *(o as *mut EnumObject);
        xdecref(e.it);
        xdecref(e.result);
        xdecref(e.longindex);
        let tp = (*o).type_ptr();
        generic_free_and_type_decref(o, tp);
    }
}
unsafe extern "C" fn enum_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let e = &*(o as *mut EnumObject);
        let f = visit.unwrap();
        for p in [e.it, e.result, e.longindex] {
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
fn make_pair(slot: &mut *mut PyObject, a: PyRef, b: PyRef) -> PyRef {
    if !slot.is_null() {
        let t = unsafe { &**slot };
        if tuple::tuple_reusable(t) {
            tuple::tuple_refill(t, vec![a, b]);
            return t.to_ref();
        }
    }
    tuple::tuple2(a, b)
}
fn enum_next_rs(o: &PyObject) -> PyResult<Option<PyRef>> {
    let e = en(o);
    if e.it.is_null() {
        return Ok(None);
    }
    let item = match ab::iter_next(unsafe { &*e.it })? {
        Some(x) => x,
        None => return Ok(None),
    };
    let idx = if e.longindex.is_null() && e.index < i64::MAX {
        let v = int::new_int(e.index);
        e.index += 1;
        v
    } else {
        if e.longindex.is_null() {
            e.longindex = int::new_int(e.index).into_raw();
        }
        let cur = unsafe { PyRef::from_borrowed(e.longindex) };
        let next = ab::binary_op(&cur, &int::new_int(1), ab::NbSlot::Add)?;
        let old = std::mem::replace(&mut e.longindex, next.into_raw());
        unsafe { decref(old) };
        cur
    };
    Ok(Some(make_pair(&mut e.result, idx, item)))
}
unsafe extern "C" fn enum_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        match enum_next_rs(&*o) {
            Ok(Some(v)) => v.into_raw(),
            _ => std::ptr::null_mut(),
        }
    }
}
fn enum_reduce(o: &PyObject) -> PyResult {
    let e = en(o);
    let idx = if e.longindex.is_null() { int::new_int(e.index) } else { unsafe { PyRef::from_borrowed(e.longindex) } };
    let it = if e.it.is_null() { tuple::empty_tuple() } else { unsafe { PyRef::from_borrowed(e.it) } };
    Ok(tuple::new_tuple(vec![type_ref(o.type_ptr()), tuple::new_tuple(vec![it, idx])]))
}
fn class_getitem_m(cls: &PyObject, item: &PyObject) -> PyResult {
    crate::types::containers_reg::generic_alias(cls, item)
}

// ================================================================== reversed

#[repr(C)]
struct ReversedObject {
    ob_base: PyObject,
    index: isize,
    seq: *mut PyObject,
}
fn rv(o: &PyObject) -> &mut ReversedObject {
    unsafe { &mut *(o.as_ptr() as *mut ReversedObject) }
}

fn is_sequence(o: &PyObject) -> bool {
    if dict::is_dict(o) {
        return false;
    }
    let tp = o.ty();
    !tp.tp_as_sequence.is_null() && unsafe { (*tp.tp_as_sequence).sq_item.is_some() }
}

pub fn reversed_of(tp: *mut PyTypeObject, seq: &PyObject) -> PyResult {
    if let Some(m) = lookup(seq.type_ptr(), crate::name!("__reversed__")) {
        if none::is_none(&m) {
            raise!(TypeError, "'{}' object is not reversible", tname(seq));
        }
        let bound = match m.ty().tp_descr_get {
            Some(g) => unsafe { from_c(g(m.as_ptr(), seq.as_ptr(), seq.type_ptr() as *mut PyObject))? },
            None => m,
        };
        return ab::call(&bound, &[]);
    }
    if !is_sequence(seq) {
        raise!(TypeError, "'{}' object is not reversible", tname(seq));
    }
    let n = ab::length(seq)? as isize;
    let o = alloc_of(tp)?;
    let r = rv(&o);
    r.index = n - 1;
    r.seq = seq.to_ref().into_raw();
    Ok(o)
}

unsafe extern "C" fn reversed_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            if tp == &raw mut PyReversed_Type && !kw.is_null() && dict::dict_len(&*kw) > 0 {
                raise!(TypeError, "reversed() takes no keyword arguments");
            }
            let a = tuple::tuple_items(&*args);
            if a.len() != 1 {
                raise!(TypeError, "reversed expected 1 argument, got {}", a.len());
            }
            reversed_of(tp, a[0])
        })();
        into_c(r)
    }
}
unsafe extern "C" fn reversed_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        xdecref((*(o as *mut ReversedObject)).seq);
        let tp = (*o).type_ptr();
        generic_free_and_type_decref(o, tp);
    }
}
unsafe extern "C" fn reversed_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let s = (*(o as *mut ReversedObject)).seq;
        if s.is_null() { 0 } else { visit.unwrap()(s, arg) }
    }
}
unsafe extern "C" fn reversed_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = rv(&*o);
        if r.index >= 0 && !r.seq.is_null() {
            let seq = &*r.seq;
            let res = match (*seq.ty().tp_as_sequence).sq_item {
                Some(f) => from_c(f(seq.as_ptr(), r.index)),
                None => ab::getitem(seq, &int::new_int(r.index as i64)),
            };
            match res {
                Ok(v) => {
                    r.index -= 1;
                    return v.into_raw();
                }
                Err(_) => {
                    if !(exc_matches(exceptions::IndexError()) || exc_matches(exceptions::StopIteration())) {
                        return std::ptr::null_mut();
                    }
                    clear_exc();
                }
            }
        }
        r.index = -1;
        let s = std::mem::replace(&mut r.seq, std::ptr::null_mut());
        xdecref(s);
        std::ptr::null_mut()
    }
}
fn reversed_len(o: &PyObject) -> PyResult {
    let r = rv(o);
    if r.seq.is_null() {
        return Ok(int::new_int(0));
    }
    let n = ab::length(unsafe { &*r.seq })? as isize;
    let pos = r.index + 1;
    Ok(int::new_int(if n < pos { 0 } else { pos as i64 }))
}
fn reversed_reduce(o: &PyObject) -> PyResult {
    let r = rv(o);
    if r.seq.is_null() {
        return Ok(tuple::new_tuple(vec![type_ref(o.type_ptr()), tuple::new_tuple(vec![tuple::empty_tuple()])]));
    }
    Ok(tuple::new_tuple(vec![
        type_ref(o.type_ptr()),
        tuple::new_tuple(vec![unsafe { PyRef::from_borrowed(r.seq) }]),
        int::new_int(r.index as i64),
    ]))
}
fn reversed_setstate(o: &PyObject, state: &PyObject) -> PyResult {
    let r = rv(o);
    let mut idx = ab::index_as_ssize(state, None)?;
    if !r.seq.is_null() {
        let n = ab::length(unsafe { &*r.seq })? as isize;
        if idx < -1 {
            idx = -1;
        } else if idx > n - 1 {
            idx = n - 1;
        }
        r.index = idx;
    }
    Ok(none::none_ref())
}

pub fn init_types() {
    unsafe {
        let t = &mut *(&raw mut PyEnum_Type);
        t.tp_name = c"enumerate".as_ptr();
        t.tp_basicsize = std::mem::size_of::<EnumObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_BASETYPE;
        t.tp_dealloc = Some(enum_dealloc);
        t.tp_traverse = Some(enum_traverse);
        t.tp_iter = Some(crate::types::list::self_iter);
        t.tp_iternext = Some(enum_next);
        t.tp_new = Some(enum_new);
        t.tp_alloc = Some(generic_alloc);
        t.tp_free = Some(generic_free);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_doc = c"enumerate(iterable, start=0)\n--\n\nReturn an enumerate object.\n\n  iterable\n    an object supporting iteration\n\nThe enumerate object yields pairs containing a count (from start, which\ndefaults to zero) and a value yielded by the iterable argument.\n\nenumerate is useful for obtaining an indexed list:\n    (0, seq[0]), (1, seq[1]), (2, seq[2]), ...".as_ptr();
        t.tp_methods = crate::mdefs![
            ("__reduce__", NOARGS, enum_reduce, "Return state information for pickling."),
            ("__class_getitem__", O, class_getitem_m, "See PEP 585"; METH_CLASS),
        ];

        let t = &mut *(&raw mut PyReversed_Type);
        t.tp_name = c"reversed".as_ptr();
        t.tp_basicsize = std::mem::size_of::<ReversedObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_BASETYPE;
        t.tp_dealloc = Some(reversed_dealloc);
        t.tp_traverse = Some(reversed_traverse);
        t.tp_iter = Some(crate::types::list::self_iter);
        t.tp_iternext = Some(reversed_next);
        t.tp_new = Some(reversed_new);
        t.tp_alloc = Some(generic_alloc);
        t.tp_free = Some(generic_free);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_doc = c"reversed(sequence, /)\n--\n\nReturn a reverse iterator over the values of the given sequence.".as_ptr();
        t.tp_methods = crate::mdefs![
            ("__length_hint__", NOARGS, reversed_len, "Private method returning an estimate of len(list(it))."),
            ("__reduce__", NOARGS, reversed_reduce, "Return state information for pickling."),
            ("__setstate__", O, reversed_setstate, "Set state information for unpickling."),
        ];
    }
}

pub fn ready() -> PyResult<()> {
    type_ready(&raw mut PyEnum_Type)?;
    type_ready(&raw mut PyReversed_Type)
}
pub fn enumerate_type() -> *mut PyTypeObject {
    &raw mut PyEnum_Type
}
pub fn reversed_type() -> *mut PyTypeObject {
    &raw mut PyReversed_Type
}
