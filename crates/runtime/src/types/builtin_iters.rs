//! `zip` (with strict=), `map` (with strict=, 3.14) and `filter`.
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::*;
use crate::types::{bool_, dict, exceptions, none, str as pystr, tuple};
use std::ffi::{c_int, c_void};

#[unsafe(no_mangle)]
pub static mut PyZip_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyMap_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyFilter_Type: PyTypeObject = PyTypeObject::ZERO;

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

/// Parse a lone `strict=` keyword; `fname` for messages.
fn parse_strict(kw: *mut PyObject, fname: &str) -> PyResult<bool> {
    if kw.is_null() {
        return Ok(false);
    }
    let mut strict = false;
    for (k, v) in dict::items_vec(unsafe { &*kw }) {
        let ks = pystr::to_string_lossy(&k);
        if ks == "strict" {
            strict = ab::is_true(&v)?;
        } else {
            raise!(TypeError, "{}() got an unexpected keyword argument '{}'", fname, ks);
        }
    }
    Ok(strict)
}

/// Shared "next tuple of items" logic for zip and map (strict handling).
fn next_items(iters: &PyObject, strict: bool, fname: &str) -> PyResult<Option<Vec<PyRef>>> {
    let its = tuple::tuple_items(iters);
    let mut out = Vec::with_capacity(its.len());
    for (i, it) in its.iter().enumerate() {
        match ab::iter_next(it)? {
            Some(x) => out.push(x),
            None => {
                if !strict {
                    return Ok(None);
                }
                if i > 0 {
                    let plural = if i == 1 { " ".to_string() } else { "s 1-".to_string() };
                    raise!(ValueError, "{}() argument {} is shorter than argument{}{}", fname, i + 1, plural, i);
                }
                for (j, other) in its.iter().enumerate().skip(1) {
                    if ab::iter_next(other)?.is_some() {
                        let plural = if j == 1 { " ".to_string() } else { "s 1-".to_string() };
                        raise!(ValueError, "{}() argument {} is longer than argument{}{}", fname, j + 1, plural, j);
                    }
                }
                return Ok(None);
            }
        }
    }
    Ok(Some(out))
}

fn iters_of(args: &[&PyObject]) -> PyResult<PyRef> {
    let mut v = Vec::with_capacity(args.len());
    for a in args {
        v.push(ab::get_iter(a)?);
    }
    Ok(tuple::new_tuple(v))
}

// ================================================================== zip

#[repr(C)]
struct ZipObject {
    ob_base: PyObject,
    iters: *mut PyObject,
    result: *mut PyObject,
    strict: c_int,
}
fn zo(o: &PyObject) -> &mut ZipObject {
    unsafe { &mut *(o.as_ptr() as *mut ZipObject) }
}
unsafe extern "C" fn zip_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let strict = parse_strict(kw, "zip")?;
            let iters = iters_of(tuple::tuple_items(&*args))?;
            let n = tuple::tuple_len(&iters);
            let o = alloc_of(tp)?;
            let z = zo(&o);
            z.iters = iters.into_raw();
            z.strict = strict as c_int;
            z.result = if n > 0 { tuple::new_tuple((0..n).map(|_| none::none_ref()).collect()).into_raw() } else { std::ptr::null_mut() };
            Ok(o)
        })();
        into_c(r)
    }
}
unsafe extern "C" fn zip_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let z = zo(&*o);
        let iters = &*z.iters;
        if tuple::tuple_len(iters) == 0 {
            return std::ptr::null_mut();
        }
        match next_items(iters, z.strict != 0, "zip") {
            Ok(Some(items)) => {
                if !z.result.is_null() && tuple::tuple_reusable(&*z.result) {
                    tuple::tuple_refill(&*z.result, items);
                    return (*z.result).to_ref().into_raw();
                }
                tuple::new_tuple(items).into_raw()
            }
            _ => std::ptr::null_mut(),
        }
    }
}
unsafe extern "C" fn zip_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let z = &*(o as *mut ZipObject);
        xdecref(z.iters);
        xdecref(z.result);
        let tp = (*o).type_ptr();
        generic_free_and_type_decref(o, tp);
    }
}
unsafe extern "C" fn zip_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let z = &*(o as *mut ZipObject);
        let f = visit.unwrap();
        for p in [z.iters, z.result] {
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
fn zip_reduce(o: &PyObject) -> PyResult {
    let z = zo(o);
    let iters = unsafe { PyRef::from_borrowed(z.iters) };
    if z.strict != 0 {
        return Ok(tuple::new_tuple(vec![type_ref(o.type_ptr()), iters, bool_::new_bool(true)]));
    }
    Ok(tuple::new_tuple(vec![type_ref(o.type_ptr()), iters]))
}
fn zip_setstate(o: &PyObject, state: &PyObject) -> PyResult {
    zo(o).strict = ab::is_true(state)? as c_int;
    Ok(none::none_ref())
}

// ================================================================== map

#[repr(C)]
struct MapObject {
    ob_base: PyObject,
    iters: *mut PyObject,
    func: *mut PyObject,
    strict: c_int,
}
fn mo(o: &PyObject) -> &mut MapObject {
    unsafe { &mut *(o.as_ptr() as *mut MapObject) }
}
unsafe extern "C" fn map_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let strict = parse_strict(kw, "map")?;
            let a = tuple::tuple_items(&*args);
            if a.len() < 2 {
                raise!(TypeError, "map() must have at least two arguments.");
            }
            let iters = iters_of(&a[1..])?;
            let o = alloc_of(tp)?;
            let m = mo(&o);
            m.iters = iters.into_raw();
            m.func = a[0].to_ref().into_raw();
            m.strict = strict as c_int;
            Ok(o)
        })();
        into_c(r)
    }
}
unsafe extern "C" fn map_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let m = mo(&*o);
        match next_items(&*m.iters, m.strict != 0, "map") {
            Ok(Some(items)) => {
                let refs: Vec<&PyObject> = items.iter().map(|x| &**x).collect();
                into_c(ab::call(&*m.func, &refs))
            }
            _ => std::ptr::null_mut(),
        }
    }
}
unsafe extern "C" fn map_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let m = &*(o as *mut MapObject);
        xdecref(m.iters);
        xdecref(m.func);
        let tp = (*o).type_ptr();
        generic_free_and_type_decref(o, tp);
    }
}
unsafe extern "C" fn map_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let m = &*(o as *mut MapObject);
        let f = visit.unwrap();
        for p in [m.iters, m.func] {
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
fn map_reduce(o: &PyObject) -> PyResult {
    let m = mo(o);
    let mut args = vec![unsafe { PyRef::from_borrowed(m.func) }];
    args.extend(tuple::tuple_to_vec(unsafe { &*m.iters }));
    let mut v = vec![type_ref(o.type_ptr()), tuple::new_tuple(args)];
    if m.strict != 0 {
        v.push(bool_::new_bool(true));
    }
    Ok(tuple::new_tuple(v))
}
fn map_setstate(o: &PyObject, state: &PyObject) -> PyResult {
    mo(o).strict = ab::is_true(state)? as c_int;
    Ok(none::none_ref())
}

// ================================================================== filter

#[repr(C)]
struct FilterObject {
    ob_base: PyObject,
    func: *mut PyObject,
    it: *mut PyObject,
}
fn fo(o: &PyObject) -> &mut FilterObject {
    unsafe { &mut *(o.as_ptr() as *mut FilterObject) }
}
unsafe extern "C" fn filter_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            if tp == &raw mut PyFilter_Type && !kw.is_null() && dict::dict_len(&*kw) > 0 {
                raise!(TypeError, "filter() takes no keyword arguments");
            }
            let a = tuple::tuple_items(&*args);
            if a.len() != 2 {
                raise!(TypeError, "filter expected 2 arguments, got {}", a.len());
            }
            let it = ab::get_iter(a[1])?;
            let o = alloc_of(tp)?;
            let f = fo(&o);
            f.func = a[0].to_ref().into_raw();
            f.it = it.into_raw();
            Ok(o)
        })();
        into_c(r)
    }
}
unsafe extern "C" fn filter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let f = fo(&*o);
        let func = &*f.func;
        let check_true = none::is_none(func) || func.is(crate::types::typeobject::type_obj(bool_::bool_type()));
        let r = (|| -> PyResult<Option<PyRef>> {
            loop {
                let item = match ab::iter_next(&*f.it)? {
                    Some(x) => x,
                    None => return Ok(None),
                };
                let ok = if check_true {
                    ab::is_true(&item)?
                } else {
                    let g = ab::call(func, &[&item])?;
                    ab::is_true(&g)?
                };
                if ok {
                    return Ok(Some(item));
                }
            }
        })();
        match r {
            Ok(Some(v)) => v.into_raw(),
            _ => std::ptr::null_mut(),
        }
    }
}
unsafe extern "C" fn filter_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let f = &*(o as *mut FilterObject);
        xdecref(f.func);
        xdecref(f.it);
        let tp = (*o).type_ptr();
        generic_free_and_type_decref(o, tp);
    }
}
unsafe extern "C" fn filter_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let f = &*(o as *mut FilterObject);
        let v = visit.unwrap();
        for p in [f.func, f.it] {
            if !p.is_null() {
                let r = v(p, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        0
    }
}
fn filter_reduce(o: &PyObject) -> PyResult {
    let f = fo(o);
    Ok(tuple::new_tuple(vec![
        type_ref(o.type_ptr()),
        tuple::new_tuple(vec![unsafe { PyRef::from_borrowed(f.func) }, unsafe { PyRef::from_borrowed(f.it) }]),
    ]))
}

// ================================================================== registration

fn base_type(t: &mut PyTypeObject, name: &'static std::ffi::CStr, size: usize) {
    t.tp_name = name.as_ptr();
    t.tp_basicsize = size as isize;
    t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_BASETYPE;
    t.tp_iter = Some(crate::types::list::self_iter);
    t.tp_alloc = Some(generic_alloc);
    t.tp_free = Some(generic_free);
    t.tp_getattro = Some(object_generic_getattro);
}

pub fn init_types() {
    unsafe {
        let t = &mut *(&raw mut PyZip_Type);
        base_type(t, c"zip", std::mem::size_of::<ZipObject>());
        t.tp_new = Some(zip_new);
        t.tp_iternext = Some(zip_next);
        t.tp_dealloc = Some(zip_dealloc);
        t.tp_traverse = Some(zip_traverse);
        t.tp_doc = c"zip(*iterables, strict=False)\n--\n\nThe zip object yields n-length tuples, where n is the number of iterables\npassed as positional arguments to zip().  The i-th element in every tuple\ncomes from the i-th iterable argument to zip().  This continues until the\nshortest argument is exhausted.\n\nIf strict is true and one of the arguments is exhausted before the others,\nraise a ValueError.\n\n   >>> list(zip('abcdefg', range(3), range(4)))\n   [('a', 0, 0), ('b', 1, 1), ('c', 2, 2)]".as_ptr();
        t.tp_methods = crate::mdefs![
            ("__reduce__", NOARGS, zip_reduce, "Return state information for pickling."),
            ("__setstate__", O, zip_setstate, "Set state information for unpickling."),
        ];

        let t = &mut *(&raw mut PyMap_Type);
        base_type(t, c"map", std::mem::size_of::<MapObject>());
        t.tp_new = Some(map_new);
        t.tp_iternext = Some(map_next);
        t.tp_dealloc = Some(map_dealloc);
        t.tp_traverse = Some(map_traverse);
        t.tp_doc = c"map(function, iterable, /, *iterables, strict=False)\n--\n\nMake an iterator that computes the function using arguments from each of the iterables.\n\nStops when the shortest iterable is exhausted.\n\nIf strict is true and one of the arguments is exhausted before the others,\nraise a ValueError.".as_ptr();
        t.tp_methods = crate::mdefs![
            ("__reduce__", NOARGS, map_reduce, "Return state information for pickling."),
            ("__setstate__", O, map_setstate, "Set state information for unpickling."),
        ];

        let t = &mut *(&raw mut PyFilter_Type);
        base_type(t, c"filter", std::mem::size_of::<FilterObject>());
        t.tp_new = Some(filter_new);
        t.tp_iternext = Some(filter_next);
        t.tp_dealloc = Some(filter_dealloc);
        t.tp_traverse = Some(filter_traverse);
        t.tp_doc = c"filter(function, iterable, /)\n--\n\nReturn an iterator yielding those items of iterable for which function(item)\nis true. If function is None, return the items that are true.".as_ptr();
        t.tp_methods = crate::mdefs![("__reduce__", NOARGS, filter_reduce, "Return state information for pickling.")];
    }
}

pub fn ready() -> PyResult<()> {
    type_ready(&raw mut PyZip_Type)?;
    type_ready(&raw mut PyMap_Type)?;
    type_ready(&raw mut PyFilter_Type)
}
pub fn zip_type() -> *mut PyTypeObject {
    &raw mut PyZip_Type
}
pub fn map_type() -> *mut PyTypeObject {
    &raw mut PyMap_Type
}
pub fn filter_type() -> *mut PyTypeObject {
    &raw mut PyFilter_Type
}
#[allow(dead_code)]
fn _unused() -> *mut PyTypeObject {
    exceptions::TypeError()
}
