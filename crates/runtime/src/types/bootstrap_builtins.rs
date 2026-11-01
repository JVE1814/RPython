//! TEMPORARY minimal implementations of range, enumerate, zip, map, filter,
//! reversed and property so that other workstreams can run code. The
//! "containers" workstream replaces these with complete implementations
//! (and then deletes this file).

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::*;
use crate::types::{bool_, int, none, str as pystr, tuple};
use crate::raise;
use std::ffi::{c_int, c_void};

// ------------------------------------------------------------ generic "rust iterator" object

/// An iterator object whose state is a boxed Rust closure.
#[repr(C)]
struct RIter {
    ob_base: PyObject,
    state: *mut RState,
}
struct RState {
    next: Box<dyn FnMut() -> PyResult<Option<PyRef>>>,
    refs: Vec<PyRef>,
}

#[unsafe(export_name = "PyEnum_Type")]
static mut ENUMERATE_TYPE: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(export_name = "PyZip_Type")]
static mut ZIP_TYPE: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(export_name = "PyMap_Type")]
static mut MAP_TYPE: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(export_name = "PyFilter_Type")]
static mut FILTER_TYPE: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(export_name = "PyReversed_Type")]
static mut REVERSED_TYPE: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(export_name = "PyRangeIter_Type")]
static mut RANGE_ITER_TYPE: PyTypeObject = PyTypeObject::ZERO;

fn new_riter(tp: *mut PyTypeObject, refs: Vec<PyRef>, next: Box<dyn FnMut() -> PyResult<Option<PyRef>>>) -> PyResult {
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
        let op = alloc(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        (*(op as *mut RIter)).state = Box::into_raw(Box::new(RState { next, refs }));
        Ok(PyRef::from_raw(op))
    }
}
unsafe extern "C" fn riter_next(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let st = (*(o as *mut RIter)).state;
        if st.is_null() {
            return std::ptr::null_mut();
        }
        match ((*st).next)() {
            Ok(Some(v)) => v.into_raw(),
            Ok(None) => std::ptr::null_mut(),
            Err(_) => std::ptr::null_mut(),
        }
    }
}
unsafe extern "C" fn riter_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let st = (*(o as *mut RIter)).state;
        if !st.is_null() {
            drop(Box::from_raw(st));
        }
        let tp = (*o).type_ptr();
        generic_free_and_type_decref(o, tp);
    }
}
unsafe extern "C" fn riter_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let st = (*(o as *mut RIter)).state;
        if st.is_null() {
            return 0;
        }
        for r in (*st).refs.iter() {
            let x = visit.unwrap()(r.as_ptr(), arg);
            if x != 0 {
                return x;
            }
        }
        0
    }
}
unsafe extern "C" fn self_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}

fn kw_or_args(args: *mut PyObject, kw: *mut PyObject) -> (Vec<PyRef>, Vec<(PyRef, PyRef)>) {
    unsafe {
        let a = tuple::tuple_to_vec(&*args);
        let k = if kw.is_null() { Vec::new() } else { crate::types::dict::items_vec(&*kw) };
        (a, k)
    }
}

unsafe extern "C" fn enumerate_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    let (a, k) = kw_or_args(args, kw);
    let r = (|| -> PyResult {
        let mut iterable = a.first().cloned();
        let mut start = a.get(1).cloned();
        for (key, v) in k {
            match pystr::to_string_lossy(&key).as_str() {
                "iterable" => iterable = Some(v),
                "start" => start = Some(v),
                o => raise!(TypeError, "enumerate() got an unexpected keyword argument '{}'", o),
            }
        }
        let iterable = match iterable {
            Some(i) => i,
            None => raise!(TypeError, "enumerate() missing required argument 'iterable'"),
        };
        let it = ab::get_iter(&iterable)?;
        let mut n = match start {
            Some(s) => ab::index(&s)?,
            None => int::new_int(0),
        };
        let one = int::new_int(1);
        let it2 = it.clone();
        new_riter(
            tp,
            vec![it],
            Box::new(move || match ab::iter_next(&it2)? {
                Some(v) => {
                    let cur = n.clone();
                    n = crate::interp::eval::binary_op(rpy_compiler::bytecode::BinOp::Add, &n, &one)?;
                    Ok(Some(tuple::new_tuple(vec![cur, v])))
                }
                None => Ok(None),
            }),
        )
    })();
    into_c(r)
}

unsafe extern "C" fn zip_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    let (a, k) = kw_or_args(args, kw);
    let r = (|| -> PyResult {
        let mut strict = false;
        for (key, v) in k {
            if pystr::str_eq_ascii(&key, "strict") {
                strict = ab::is_true(&v)?;
            } else {
                raise!(TypeError, "zip() got an unexpected keyword argument '{}'", pystr::to_string_lossy(&key));
            }
        }
        let mut its = Vec::new();
        for x in a.iter() {
            its.push(ab::get_iter(x)?);
        }
        let its2 = its.clone();
        let mut done = false;
        new_riter(
            tp,
            its,
            Box::new(move || {
                if done || its2.is_empty() {
                    return Ok(None);
                }
                let mut out = Vec::with_capacity(its2.len());
                for (i, it) in its2.iter().enumerate() {
                    match ab::iter_next(it)? {
                        Some(v) => out.push(v),
                        None => {
                            done = true;
                            if strict {
                                if i > 0 {
                                    raise!(ValueError, "zip() argument {} is shorter than argument{} 1{}", i + 1, if i == 1 { "" } else { "s" }, if i == 1 { String::new() } else { format!("-{}", i) });
                                }
                                for (j, it) in its2.iter().enumerate().skip(1) {
                                    if ab::iter_next(it)?.is_some() {
                                        raise!(ValueError, "zip() argument {} is longer than argument{} 1{}", j + 1, if j == 1 { "" } else { "s" }, if j == 1 { String::new() } else { format!("-{}", j) });
                                    }
                                }
                            }
                            return Ok(None);
                        }
                    }
                }
                Ok(Some(tuple::new_tuple(out)))
            }),
        )
    })();
    into_c(r)
}

unsafe extern "C" fn map_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    let (a, _k) = kw_or_args(args, kw);
    let r = (|| -> PyResult {
        if a.len() < 2 {
            raise!(TypeError, "map() must have at least two arguments.");
        }
        let f = a[0].clone();
        let mut its = Vec::new();
        for x in &a[1..] {
            its.push(ab::get_iter(x)?);
        }
        let its2 = its.clone();
        let mut refs = its;
        refs.push(f.clone());
        new_riter(
            tp,
            refs,
            Box::new(move || {
                let mut vals = Vec::with_capacity(its2.len());
                for it in its2.iter() {
                    match ab::iter_next(it)? {
                        Some(v) => vals.push(v),
                        None => return Ok(None),
                    }
                }
                let refs: Vec<&PyObject> = vals.iter().map(|v| &**v).collect();
                ab::call(&f, &refs).map(Some)
            }),
        )
    })();
    into_c(r)
}

unsafe extern "C" fn filter_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    let (a, _k) = kw_or_args(args, kw);
    let r = (|| -> PyResult {
        if a.len() != 2 {
            raise!(TypeError, "filter expected 2 arguments, got {}", a.len());
        }
        let f = a[0].clone();
        let it = ab::get_iter(&a[1])?;
        let it2 = it.clone();
        new_riter(
            tp,
            vec![it, f.clone()],
            Box::new(move || loop {
                match ab::iter_next(&it2)? {
                    Some(v) => {
                        let keep = if f.is(none::none()) { ab::is_true(&v)? } else { ab::is_true(&*ab::call(&f, &[&v])?)? };
                        if keep {
                            return Ok(Some(v));
                        }
                    }
                    None => return Ok(None),
                }
            }),
        )
    })();
    into_c(r)
}

unsafe extern "C" fn reversed_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    let (a, _k) = kw_or_args(args, kw);
    let r = (|| -> PyResult {
        if a.len() != 1 {
            raise!(TypeError, "reversed expected 1 argument, got {}", a.len());
        }
        let seq = a[0].clone();
        if let Some(rev) = ab::lookup_special(&seq, crate::name!("__reversed__"))? {
            if rev.is(none::none()) {
                raise!(TypeError, "'{}' object is not reversible", tname(&seq));
            }
            return ab::call(&rev, &[]);
        }
        let n = match ab::length(&seq) {
            Ok(n) => n,
            Err(_) => {
                clear_exc();
                raise!(TypeError, "'{}' object is not reversible", tname(&seq));
            }
        };
        let mut i = n as i64;
        let s2 = seq.clone();
        new_riter(
            tp,
            vec![seq],
            Box::new(move || {
                if i <= 0 {
                    return Ok(None);
                }
                i -= 1;
                ab::getitem(&s2, &int::new_int(i)).map(Some)
            }),
        )
    })();
    into_c(r)
}

// ------------------------------------------------------------ range

#[repr(C)]
pub struct RangeObject {
    ob_base: PyObject,
    start: *mut PyObject,
    stop: *mut PyObject,
    step: *mut PyObject,
    length: *mut PyObject,
}
#[unsafe(no_mangle)]
pub static mut PyRange_Type: PyTypeObject = PyTypeObject::ZERO;
pub fn range_type() -> *mut PyTypeObject {
    &raw mut PyRange_Type
}
fn rparts(o: &PyObject) -> (i64, i64, i64, i64) {
    unsafe {
        let r = &*(o.as_ptr() as *mut RangeObject);
        (
            int::as_i64(&*r.start).unwrap_or(0),
            int::as_i64(&*r.stop).unwrap_or(0),
            int::as_i64(&*r.step).unwrap_or(1),
            int::as_i64(&*r.length).unwrap_or(0),
        )
    }
}
unsafe extern "C" fn range_new(_tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    let (a, k) = kw_or_args(args, kw);
    let r = (|| -> PyResult {
        if !k.is_empty() {
            raise!(TypeError, "range() takes no keyword arguments");
        }
        let (start, stop, step) = match a.len() {
            1 => (int::new_int(0), ab::index(&a[0])?, int::new_int(1)),
            2 => (ab::index(&a[0])?, ab::index(&a[1])?, int::new_int(1)),
            3 => (ab::index(&a[0])?, ab::index(&a[1])?, ab::index(&a[2])?),
            0 => raise!(TypeError, "range expected at least 1 argument, got 0"),
            n => raise!(TypeError, "range expected at most 3 arguments, got {}", n),
        };
        let (s, e, st) = match (int::as_i64(&start), int::as_i64(&stop), int::as_i64(&step)) {
            (Some(s), Some(e), Some(st)) => (s, e, st),
            _ => raise!(OverflowError, "range() with huge arguments is not supported yet"),
        };
        if st == 0 {
            raise!(ValueError, "range() arg 3 must not be zero");
        }
        let len = if st > 0 {
            if e > s { ((e as i128 - s as i128 - 1) / st as i128 + 1) as i64 } else { 0 }
        } else if s > e {
            ((s as i128 - e as i128 - 1) / (-(st as i128)) + 1) as i64
        } else {
            0
        };
        let op = crate::alloc::alloc_object(range_type(), std::mem::size_of::<RangeObject>());
        let ro = &mut *(op as *mut RangeObject);
        ro.start = start.into_raw();
        ro.stop = stop.into_raw();
        ro.step = step.into_raw();
        ro.length = int::new_int(len).into_raw();
        Ok(PyRef::from_raw(op))
    })();
    into_c(r)
}
unsafe extern "C" fn range_dealloc(o: *mut PyObject) {
    unsafe {
        let r = &*(o as *mut RangeObject);
        xdecref(r.start);
        xdecref(r.stop);
        xdecref(r.step);
        xdecref(r.length);
        crate::alloc::free_object(o);
    }
}
unsafe extern "C" fn range_len(o: *mut PyObject) -> Py_ssize_t {
    unsafe { rparts(&*o).3 as isize }
}
unsafe extern "C" fn range_item(o: *mut PyObject, i: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let (s, _e, st, n) = rparts(&*o);
        let i = i as i64;
        if i < 0 || i >= n {
            return into_c(Err(err!(IndexError, "range object index out of range")));
        }
        int::new_int(s + i * st).into_raw()
    }
}
unsafe extern "C" fn range_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (s, _e, st, n) = rparts(&*o);
        let k = &*k;
        if crate::types::slice::is_slice(k) {
            let r = (|| -> PyResult {
                let (a, b, c, _) = crate::types::slice::indices(k, n as isize)?;
                let start = int::new_int(s + a as i64 * st);
                let stop = int::new_int(s + b as i64 * st);
                let step = int::new_int(st * c as i64);
                ab::call(type_obj(range_type()), &[&start, &stop, &step])
            })();
            return into_c(r);
        }
        let mut i = match ab::index_as_ssize(k, Some(crate::types::exceptions::IndexError())) {
            Ok(i) => i,
            Err(_) => return std::ptr::null_mut(),
        };
        if i < 0 {
            i += n as isize;
        }
        range_item(o, i)
    }
}
unsafe extern "C" fn range_contains(o: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let v = &*v;
        let (s, e, st, _n) = rparts(&*o);
        if int::is_int_exact(v) || bool_::is_bool(v) {
            if let Some(x) = int::as_i64(v) {
                let inr = if st > 0 { x >= s && x < e } else { x <= s && x > e };
                return (inr && (x - s) % st == 0) as c_int;
            }
            return 0;
        }
        match ab::iter_search_contains(&*o, v) {
            Ok(b) => b as c_int,
            Err(_) => -1,
        }
    }
}
unsafe extern "C" fn range_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (s, _e, st, n) = rparts(&*o);
        let mut i = 0i64;
        into_c(new_riter(
            &raw mut RANGE_ITER_TYPE,
            Vec::new(),
            Box::new(move || {
                if i >= n {
                    return Ok(None);
                }
                let v = s + i * st;
                i += 1;
                Ok(Some(int::new_int(v)))
            }),
        ))
    }
}
unsafe extern "C" fn range_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = &*(o as *mut RangeObject);
        let (s, e, st) = (int::as_i64(&*r.start).unwrap_or(0), int::as_i64(&*r.stop).unwrap_or(0), int::as_i64(&*r.step).unwrap_or(1));
        let txt = if st == 1 { format!("range({}, {})", s, e) } else { format!("range({}, {}, {})", s, e, st) };
        pystr::new_str(&txt).into_raw()
    }
}
fn range_start(o: &PyObject) -> PyResult {
    unsafe { Ok(PyRef::from_borrowed((*(o.as_ptr() as *mut RangeObject)).start)) }
}
fn range_stop(o: &PyObject) -> PyResult {
    unsafe { Ok(PyRef::from_borrowed((*(o.as_ptr() as *mut RangeObject)).stop)) }
}
fn range_step(o: &PyObject) -> PyResult {
    unsafe { Ok(PyRef::from_borrowed((*(o.as_ptr() as *mut RangeObject)).step)) }
}

// ------------------------------------------------------------ property

#[repr(C)]
pub struct PropertyObject {
    ob_base: PyObject,
    fget: *mut PyObject,
    fset: *mut PyObject,
    fdel: *mut PyObject,
    doc: *mut PyObject,
    name: *mut PyObject,
}
#[unsafe(no_mangle)]
pub static mut PyProperty_Type: PyTypeObject = PyTypeObject::ZERO;
pub fn property_type() -> *mut PyTypeObject {
    &raw mut PyProperty_Type
}
fn pp(o: &PyObject) -> &mut PropertyObject {
    unsafe { &mut *(o.as_ptr() as *mut PropertyObject) }
}
fn nn(p: *mut PyObject) -> Option<&'static PyObject> {
    if p.is_null() || unsafe { (*p).is(none::none()) } { None } else { Some(unsafe { &*p }) }
}
unsafe extern "C" fn property_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        static P: crate::args::Params = crate::args::Params::new("property", &["fget", "fset", "fdel", "doc"], 0, 0);
        let r = (|| -> PyResult<()> {
            let (all, kwn) = crate::args::kwdict_to_vectorcall(&*args, kw);
            let refs: Vec<&PyObject> = all.iter().map(|r| &**r).collect();
            let [g, s, d, doc] = crate::args::parse::<4>(&P, &refs, kwn.as_deref())?;
            let p = pp(&*o);
            for (slot, v) in [(&mut p.fget, g), (&mut p.fset, s), (&mut p.fdel, d)] {
                let old = std::mem::replace(slot, v.filter(|x| !x.is(none::none())).map_or(std::ptr::null_mut(), |x| x.to_ref().into_raw()));
                xdecref(old);
            }
            let docv = match doc.filter(|x| !x.is(none::none())) {
                Some(d) => Some(d.to_ref()),
                None => match nn(p.fget) {
                    Some(f) => ab::getattr_opt(f, crate::name!("__doc__"))?,
                    None => None,
                },
            };
            let old = std::mem::replace(&mut p.doc, docv.map_or(std::ptr::null_mut(), |d| d.into_raw()));
            xdecref(old);
            if !(*o).is_type(property_type()) {
                if let Some(d) = nn(p.doc) {
                    let _ = ab::setattr(&*o, crate::name!("__doc__"), d);
                }
            }
            Ok(())
        })();
        int_into_c(r)
    }
}
unsafe extern "C" fn property_get(o: *mut PyObject, obj: *mut PyObject, _tp: *mut PyObject) -> *mut PyObject {
    unsafe {
        if obj.is_null() || (*obj).is(none::none()) && false {
            return (*o).to_ref().into_raw();
        }
        let p = pp(&*o);
        match nn(p.fget) {
            Some(f) => into_c(ab::call(f, &[&*obj])),
            None => into_c(Err(err!(
                AttributeError,
                "property {}of '{}' object has no getter",
                prop_name(&*o),
                tname(&*obj)
            ))),
        }
    }
}
fn prop_name(o: &PyObject) -> String {
    match nn(pp(o).name) {
        Some(n) => format!("'{}' ", pystr::to_string_lossy(n)),
        None => String::new(),
    }
}
unsafe extern "C" fn property_set(o: *mut PyObject, obj: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let p = pp(&*o);
        let r = if v.is_null() {
            match nn(p.fdel) {
                Some(f) => ab::call(f, &[&*obj]).map(|_| ()),
                None => Err(err!(AttributeError, "property {}of '{}' object has no deleter", prop_name(&*o), tname(&*obj))),
            }
        } else {
            match nn(p.fset) {
                Some(f) => ab::call(f, &[&*obj, &*v]).map(|_| ()),
                None => Err(err!(AttributeError, "property {}of '{}' object has no setter", prop_name(&*o), tname(&*obj))),
            }
        };
        int_into_c(r)
    }
}
fn copy_with(o: &PyObject, which: usize, f: &PyObject) -> PyResult {
    let p = pp(o);
    let mut parts: Vec<PyRef> = [p.fget, p.fset, p.fdel]
        .iter()
        .map(|x| if x.is_null() { none::none_ref() } else { unsafe { PyRef::from_borrowed(*x) } })
        .collect();
    parts[which] = f.to_ref();
    let doc = if p.doc.is_null() { none::none_ref() } else { unsafe { PyRef::from_borrowed(p.doc) } };
    let t = type_ref(o.type_ptr());
    let n = ab::call(&t, &[&parts[0], &parts[1], &parts[2], &doc])?;
    if !p.name.is_null() {
        pp(&n).name = unsafe { (*p.name).to_ref().into_raw() };
    }
    Ok(n)
}
fn prop_getter(o: &PyObject, f: &PyObject) -> PyResult {
    copy_with(o, 0, f)
}
fn prop_setter(o: &PyObject, f: &PyObject) -> PyResult {
    copy_with(o, 1, f)
}
fn prop_deleter(o: &PyObject, f: &PyObject) -> PyResult {
    copy_with(o, 2, f)
}
fn prop_set_name(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("__set_name__", args, 2, 2)?;
    let p = pp(o);
    let old = std::mem::replace(&mut p.name, args[1].to_ref().into_raw());
    unsafe { xdecref(old) };
    Ok(none::none_ref())
}
fn opt(p: *mut PyObject) -> PyResult {
    Ok(if p.is_null() { none::none_ref() } else { unsafe { PyRef::from_borrowed(p) } })
}
fn pg_fget(o: &PyObject) -> PyResult {
    opt(pp(o).fget)
}
fn pg_fset(o: &PyObject) -> PyResult {
    opt(pp(o).fset)
}
fn pg_fdel(o: &PyObject) -> PyResult {
    opt(pp(o).fdel)
}
fn pg_doc(o: &PyObject) -> PyResult {
    opt(pp(o).doc)
}
fn ps_doc(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let p = pp(o);
    let old = std::mem::replace(&mut p.doc, v.map_or(std::ptr::null_mut(), |x| x.to_ref().into_raw()));
    unsafe { xdecref(old) };
    Ok(())
}
fn pg_name(o: &PyObject) -> PyResult {
    match nn(pp(o).name) {
        Some(n) => Ok(n.to_ref()),
        None => match nn(pp(o).fget) {
            Some(f) => ab::getattr(f, crate::name!("__name__")),
            None => raise!(AttributeError, "'property' object has no attribute '__name__'"),
        },
    }
}
fn pg_isabstract(o: &PyObject) -> PyResult {
    for p in [pp(o).fget, pp(o).fset, pp(o).fdel] {
        if let Some(f) = nn(p) {
            if let Some(v) = ab::getattr_opt(f, crate::name!("__isabstractmethod__"))? {
                if ab::is_true(&v)? {
                    return Ok(bool_::new_bool(true));
                }
            }
        }
    }
    Ok(bool_::new_bool(false))
}
unsafe extern "C" fn property_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let p = &*(o as *mut PropertyObject);
        for x in [p.fget, p.fset, p.fdel, p.doc, p.name] {
            xdecref(x);
        }
        let tp = (*o).type_ptr();
        generic_free_and_type_decref(o, tp);
    }
}

pub fn init_types() {
    unsafe {
        for (t, n) in [
            (&raw mut ENUMERATE_TYPE, c"enumerate"),
            (&raw mut ZIP_TYPE, c"zip"),
            (&raw mut MAP_TYPE, c"map"),
            (&raw mut FILTER_TYPE, c"filter"),
            (&raw mut REVERSED_TYPE, c"reversed"),
            (&raw mut RANGE_ITER_TYPE, c"range_iterator"),
        ] {
            let t = &mut *t;
            t.tp_name = n.as_ptr();
            t.tp_basicsize = std::mem::size_of::<RIter>() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_BASETYPE;
            t.tp_dealloc = Some(riter_dealloc);
            t.tp_traverse = Some(riter_traverse);
            t.tp_iter = Some(self_iter);
            t.tp_iternext = Some(riter_next);
            t.tp_getattro = Some(object_generic_getattro);
            t.tp_alloc = Some(generic_alloc);
            t.tp_free = Some(generic_free);
        }
        (*(&raw mut ENUMERATE_TYPE)).tp_new = Some(enumerate_new);
        (*(&raw mut ZIP_TYPE)).tp_new = Some(zip_new);
        (*(&raw mut MAP_TYPE)).tp_new = Some(map_new);
        (*(&raw mut FILTER_TYPE)).tp_new = Some(filter_new);
        (*(&raw mut REVERSED_TYPE)).tp_new = Some(reversed_new);
        (*(&raw mut RANGE_ITER_TYPE)).tp_flags &= !Py_TPFLAGS_BASETYPE;

        static mut RANGE_SEQ: PySequenceMethods = PySequenceMethods::ZERO;
        static mut RANGE_MAP: PyMappingMethods = PyMappingMethods::ZERO;
        let t = &mut *range_type();
        t.tp_name = c"range".as_ptr();
        t.tp_basicsize = std::mem::size_of::<RangeObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_SEQUENCE;
        t.tp_dealloc = Some(range_dealloc);
        t.tp_new = Some(range_new);
        t.tp_iter = Some(range_iter);
        t.tp_repr = Some(range_repr);
        t.tp_getattro = Some(object_generic_getattro);
        let s = &mut *(&raw mut RANGE_SEQ);
        s.sq_length = Some(range_len);
        s.sq_item = Some(range_item);
        s.sq_contains = Some(range_contains);
        t.tp_as_sequence = s;
        let m = &mut *(&raw mut RANGE_MAP);
        m.mp_length = Some(range_len);
        m.mp_subscript = Some(range_subscript);
        t.tp_as_mapping = m;
        t.tp_getset = crate::getset_table![("start", range_start), ("stop", range_stop), ("step", range_step)];

        let t = &mut *property_type();
        t.tp_name = c"property".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PropertyObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_BASETYPE;
        t.tp_dealloc = Some(property_dealloc);
        t.tp_init = Some(property_init);
        t.tp_new = Some(crate::types::object_type::generic_new);
        t.tp_alloc = Some(generic_alloc);
        t.tp_free = Some(generic_free);
        t.tp_descr_get = Some(property_get);
        t.tp_descr_set = Some(property_set);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_methods = crate::mdefs![
            ("getter", O, prop_getter),
            ("setter", O, prop_setter),
            ("deleter", O, prop_deleter),
            ("__set_name__", FAST, prop_set_name),
        ];
        t.tp_getset = crate::getset_table![
            ("fget", pg_fget),
            ("fset", pg_fset),
            ("fdel", pg_fdel),
            ("__doc__", pg_doc, ps_doc),
            ("__name__", pg_name),
            ("__isabstractmethod__", pg_isabstract),
        ];
    }
}

pub fn types() -> Vec<(&'static str, *mut PyTypeObject)> {
    unsafe {
        vec![
            ("enumerate", &raw mut ENUMERATE_TYPE),
            ("zip", &raw mut ZIP_TYPE),
            ("map", &raw mut MAP_TYPE),
            ("filter", &raw mut FILTER_TYPE),
            ("reversed", &raw mut REVERSED_TYPE),
            ("range", range_type()),
            ("property", property_type()),
        ]
    }
}
pub fn ready() -> PyResult<()> {
    for (_, t) in types() {
        type_ready(t)?;
    }
    unsafe { type_ready(&raw mut RANGE_ITER_TYPE) }
}
