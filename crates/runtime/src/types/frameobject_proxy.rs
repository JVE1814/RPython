//! PEP 667 `FrameLocalsProxy`: a write-through mapping view of an optimized frame's
//! local variables (fast locals, cells and free variables), plus "extra" locals stored
//! on the frame object for names that are not real local variables.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::interp::frame::Frame;
use crate::object::*;
use crate::types::frameobject::{PyFrameObject, frame_of, is_frame};
use crate::types::typeobject::*;
use crate::types::{dict, funcobject, list, none, str as pystr, tuple};
use crate::raise;
use rpy_compiler::bytecode as bc;
use std::ffi::{c_int, c_void};

#[repr(C)]
pub struct ProxyObject {
    pub ob_base: PyObject,
    pub frame: *mut PyObject,
}

#[unsafe(export_name = "PyFrameLocalsProxy_Type")]
pub static mut PROXY_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub fn proxy_type() -> *mut PyTypeObject {
    &raw mut PROXY_TYPE
}

fn frame_obj(p: &PyObject) -> &PyObject {
    unsafe { &*(*(p.as_ptr() as *mut ProxyObject)).frame }
}

fn frame(p: &PyObject) -> PyResult<&mut Frame> {
    match frame_of(frame_obj(p)) {
        Some(f) => {
            crate::interp::jit::opt::sync_frame(f as *mut Frame); // jit: unboxed locals of tier-2 code
            Ok(f)
        }
        None => raise!(RuntimeError, "frame has been cleared"),
    }
}

/// The frame object's extra-locals dict (created on demand).
fn extra(p: &PyObject, create: bool) -> Option<PyRef> {
    unsafe {
        let fo = &mut *(frame_obj(p).as_ptr() as *mut PyFrameObject);
        if fo.locals_snapshot.is_null() {
            if !create {
                return None;
            }
            fo.locals_snapshot = dict::new_dict().into_raw();
        }
        Some(PyRef::from_borrowed(fo.locals_snapshot))
    }
}

/// Extra locals of a frame object (for locals() snapshots).
pub fn extra_locals_of(frame_obj: *mut PyObject) -> Option<PyRef> {
    if frame_obj.is_null() {
        return None;
    }
    unsafe {
        let fo = &*(frame_obj as *mut PyFrameObject);
        if fo.locals_snapshot.is_null() { None } else { Some(PyRef::from_borrowed(fo.locals_snapshot)) }
    }
}

/// Index of the fast local named by `key` (hash/eq semantics), if any.
fn find_local(f: &Frame, key: &PyObject) -> PyResult<Option<usize>> {
    let h = ab::hash(key)?;
    let cd = crate::types::code::code_data(f.code());
    for i in 0..f.nlocalsplus {
        if cd.localspluskinds[i] & bc::fast::HIDDEN != 0 {
            continue;
        }
        let nm = &cd.localsplusnames[i];
        if nm.is(key) {
            return Ok(Some(i));
        }
        if pystr::str_hash(nm) == h && ab::rich_eq(nm, key)? {
            return Ok(Some(i));
        }
    }
    Ok(None)
}

fn is_cell_kind(f: &Frame, i: usize) -> bool {
    let cd = crate::types::code::code_data(f.code());
    cd.localspluskinds[i] & (bc::fast::CELL | bc::fast::FREE) != 0
}

/// Current value of slot i (None if unbound).
fn slot_value(f: &Frame, i: usize) -> Option<PyRef> {
    let p = f.slots[i];
    if p.is_null() {
        return None;
    }
    let o = unsafe { &*p };
    if is_cell_kind(f, i) && o.is_type(funcobject::cell_type()) {
        return funcobject::cell_get(o);
    }
    Some(o.to_ref())
}

fn set_slot(f: &mut Frame, i: usize, v: PyRef) {
    let p = f.slots[i];
    if is_cell_kind(f, i) && !p.is_null() && unsafe { (*p).is_type(funcobject::cell_type()) } {
        funcobject::cell_set(unsafe { &*p }, Some(v));
        return;
    }
    let old = std::mem::replace(&mut f.slots[i], v.into_raw());
    unsafe { xdecref(old) };
}

/// (key, value) pairs in CPython's order: fast locals, then extra locals.
pub fn items(p: &PyObject) -> PyResult<Vec<(PyRef, PyRef)>> {
    let f = frame(p)?;
    let cd = crate::types::code::code_data(f.code());
    let mut v = Vec::new();
    for i in 0..f.nlocalsplus {
        if cd.localspluskinds[i] & bc::fast::HIDDEN != 0 {
            continue;
        }
        if let Some(val) = slot_value(f, i) {
            v.push((cd.localsplusnames[i].clone(), val));
        }
    }
    if let Some(e) = extra(p, false) {
        v.extend(dict::items_vec(&e));
    }
    Ok(v)
}

pub fn new_proxy(frame_obj: &PyObject) -> PyResult {
    let f = match frame_of(frame_obj) {
        Some(f) => f,
        None => raise!(RuntimeError, "frame has been cleared"),
    };
    if !f.locals.is_null() {
        return Ok(unsafe { (*f.locals).to_ref() });
    }
    unsafe {
        let op = crate::alloc::alloc_object(proxy_type(), std::mem::size_of::<ProxyObject>());
        (*(op as *mut ProxyObject)).frame = frame_obj.to_ref().into_raw();
        crate::gc::track(op);
        Ok(PyRef::from_raw(op))
    }
}

fn getitem(p: &PyObject, key: &PyObject) -> PyResult {
    let f = frame(p)?;
    if let Some(i) = find_local(f, key)? {
        if let Some(v) = slot_value(f, i) {
            return Ok(v);
        }
    }
    if let Some(e) = extra(p, false) {
        if let Some(v) = dict::get_item(&e, key)? {
            return Ok(v);
        }
    }
    crate::types::dict_methods::raise_key_error(key);
    Err(PyErr)
}

fn setitem(p: &PyObject, key: &PyObject, v: &PyObject) -> PyResult<()> {
    let f = frame(p)?;
    if let Some(i) = find_local(f, key)? {
        set_slot(f, i, v.to_ref());
        return Ok(());
    }
    let e = extra(p, true).unwrap();
    dict::set_item(&e, key, v)
}

fn delitem(p: &PyObject, key: &PyObject) -> PyResult<()> {
    let f = frame(p)?;
    if let Some(i) = find_local(f, key)? {
        if slot_value(f, i).is_some() {
            raise!(ValueError, "cannot remove local variables from FrameLocalsProxy");
        }
    }
    if let Some(e) = extra(p, false) {
        if dict::del_item(&e, key)? {
            return Ok(());
        }
    }
    crate::types::dict_methods::raise_key_error(key);
    Err(PyErr)
}

fn contains(p: &PyObject, key: &PyObject) -> PyResult<bool> {
    let f = frame(p)?;
    if let Some(i) = find_local(f, key)? {
        if slot_value(f, i).is_some() {
            return Ok(true);
        }
    }
    match extra(p, false) {
        Some(e) => dict::contains(&e, key),
        None => Ok(false),
    }
}

fn to_dict(p: &PyObject) -> PyResult {
    let d = dict::new_dict();
    for (k, v) in items(p)? {
        dict::set_item(&d, &k, &v)?;
    }
    Ok(d)
}

fn update_from(p: &PyObject, other: &PyObject) -> PyResult<()> {
    if other.is(p) {
        return Ok(());
    }
    let pairs: Vec<(PyRef, PyRef)> = if dict::is_dict(other) {
        dict::items_vec(other)
    } else if other.is_type(proxy_type()) {
        items(other)?
    } else if ab::hasattr(other, crate::name!("keys"))? {
        let keys = ab::call_method_str(other, "keys", &[])?;
        let mut v = Vec::new();
        for k in ab::collect_iterable(&keys)? {
            let val = ab::getitem(other, &k)?;
            v.push((k, val));
        }
        v
    } else {
        raise!(TypeError, "update() argument must be dict or another FrameLocalsProxy");
    };
    for (k, v) in pairs {
        setitem(p, &k, &v)?;
    }
    Ok(())
}

unsafe extern "C" fn proxy_len(o: *mut PyObject) -> Py_ssize_t {
    unsafe {
        match items(&*o) {
            Ok(v) => v.len() as isize,
            Err(_) => -1,
        }
    }
}
unsafe extern "C" fn proxy_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(getitem(&*o, &*k)) }
}
unsafe extern "C" fn proxy_ass_subscript(o: *mut PyObject, k: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe { int_into_c(if v.is_null() { delitem(&*o, &*k) } else { setitem(&*o, &*k, &*v) }) }
}
unsafe extern "C" fn proxy_contains(o: *mut PyObject, k: *mut PyObject) -> c_int {
    unsafe {
        match contains(&*o, &*k) {
            Ok(b) => b as c_int,
            Err(_) => -1,
        }
    }
}
unsafe extern "C" fn proxy_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| {
            let keys: Vec<PyRef> = items(&*o)?.into_iter().map(|(k, _)| k).collect();
            ab::get_iter(&list::new_list(keys))
        })();
        into_c(r)
    }
}
unsafe extern "C" fn proxy_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        if crate::modules::reprlib_guard::enter(&*o) {
            return pystr::new_str("{...}").into_raw();
        }
        let r = to_dict(&*o).and_then(|d| ab::repr(&d));
        crate::modules::reprlib_guard::leave(&*o);
        into_c(r)
    }
}
unsafe extern "C" fn proxy_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let r = (|| {
            let d = to_dict(&*a)?;
            let other = if (*b).is_type(proxy_type()) { to_dict(&*b)? } else { (*b).to_ref() };
            ab::rich_compare(&d, &other, op)
        })();
        into_c(r)
    }
}
unsafe extern "C" fn proxy_or(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        let r = (|| -> PyResult {
            let lhs = if a.is_type(proxy_type()) { to_dict(a)? } else { a.to_ref() };
            let rhs = if b.is_type(proxy_type()) { to_dict(b)? } else { b.to_ref() };
            if !dict::is_dict(&lhs) || !dict::is_dict(&rhs) {
                return Ok(none::not_implemented().to_ref());
            }
            let d = dict::copy(&lhs)?;
            dict::merge_dict(&d, &rhs, true)?;
            Ok(d)
        })();
        into_c(r)
    }
}
unsafe extern "C" fn proxy_ior(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !dict::is_dict(b) && !b.is_type(proxy_type()) {
            return none::not_implemented().to_ref().into_raw();
        }
        match update_from(a, b) {
            Ok(()) => a.to_ref().into_raw(),
            Err(_) => std::ptr::null_mut(),
        }
    }
}
unsafe extern "C" fn proxy_new(_tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| {
            if !kw.is_null() && dict::dict_len(&*kw) > 0 {
                raise!(TypeError, "FrameLocalsProxy takes no keyword arguments");
            }
            let a = tuple::tuple_items(&*args);
            if a.len() != 1 {
                raise!(TypeError, "FrameLocalsProxy expected 1 argument, got {}", a.len());
            }
            if !is_frame(a[0]) {
                raise!(TypeError, "expect frame, not {}", tname(a[0]));
            }
            new_proxy(a[0])
        })();
        into_c(r)
    }
}
unsafe extern "C" fn proxy_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        xdecref((*(o as *mut ProxyObject)).frame);
        crate::alloc::free_object(o);
    }
}
unsafe extern "C" fn proxy_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let f = (*(o as *mut ProxyObject)).frame;
        if f.is_null() { 0 } else { visit.unwrap()(f, arg) }
    }
}

fn m_keys(o: &PyObject) -> PyResult {
    Ok(list::new_list(items(o)?.into_iter().map(|(k, _)| k).collect()))
}
fn m_values(o: &PyObject) -> PyResult {
    Ok(list::new_list(items(o)?.into_iter().map(|(_, v)| v).collect()))
}
fn m_items(o: &PyObject) -> PyResult {
    Ok(list::new_list(items(o)?.into_iter().map(|(k, v)| tuple::tuple2(k, v)).collect()))
}
fn m_get(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("get", args, 1, 2)?;
    match getitem(o, args[0]) {
        Ok(v) => Ok(v),
        Err(e) => {
            if exc_matches(crate::types::exceptions::KeyError()) {
                clear_exc();
                Ok(args.get(1).map(|x| x.to_ref()).unwrap_or_else(none::none_ref))
            } else {
                Err(e)
            }
        }
    }
}
fn m_setdefault(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("setdefault", args, 1, 2)?;
    if contains(o, args[0])? {
        return getitem(o, args[0]);
    }
    let d = args.get(1).map(|x| x.to_ref()).unwrap_or_else(none::none_ref);
    setitem(o, args[0], &d)?;
    Ok(d)
}
fn m_pop(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("pop", args, 1, 2)?;
    let f = frame(o)?;
    if find_local(f, args[0])?.is_some() {
        raise!(ValueError, "cannot remove local variables from FrameLocalsProxy");
    }
    if let Some(e) = extra(o, false) {
        if let Some(v) = dict::get_item(&e, args[0])? {
            dict::del_item(&e, args[0])?;
            return Ok(v);
        }
    }
    match args.get(1) {
        Some(d) => Ok(d.to_ref()),
        None => {
            crate::types::dict_methods::raise_key_error(args[0]);
            Err(PyErr)
        }
    }
}
fn m_update(o: &PyObject, other: &PyObject) -> PyResult {
    update_from(o, other)?;
    Ok(none::none_ref())
}
fn m_copy(o: &PyObject) -> PyResult {
    to_dict(o)
}
fn m_reversed(o: &PyObject) -> PyResult {
    let mut keys: Vec<PyRef> = items(o)?.into_iter().map(|(k, _)| k).collect();
    keys.reverse();
    Ok(list::new_list(keys))
}
fn m_reduce_ex(o: &PyObject, _proto: &PyObject) -> PyResult {
    raise!(TypeError, "cannot pickle '{}' object", tname(o));
}
fn m_sizeof(_o: &PyObject) -> PyResult {
    Ok(crate::types::int::new_int(std::mem::size_of::<ProxyObject>() as i64))
}

static mut MAP: PyMappingMethods = PyMappingMethods::ZERO;
static mut SEQ: PySequenceMethods = PySequenceMethods::ZERO;
static mut NUM: PyNumberMethods = PyNumberMethods::ZERO;

pub fn init_types() {
    unsafe {
        let t = &mut *proxy_type();
        t.tp_name = c"FrameLocalsProxy".as_ptr();
        t.tp_doc = c"FrameLocalsProxy(frame)\n--\n\nWrite-through mapping view of the local variables of a frame.".as_ptr();
        t.tp_basicsize = std::mem::size_of::<ProxyObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_MAPPING;
        t.tp_dealloc = Some(proxy_dealloc);
        t.tp_traverse = Some(proxy_traverse);
        t.tp_repr = Some(proxy_repr);
        t.tp_richcompare = Some(proxy_richcompare);
        t.tp_iter = Some(proxy_iter);
        t.tp_new = Some(proxy_new);
        t.tp_hash = Some(crate::types::slots::hash_not_implemented);
        t.tp_getattro = Some(object_generic_getattro);
        let m = &mut *(&raw mut MAP);
        m.mp_length = Some(proxy_len);
        m.mp_subscript = Some(proxy_subscript);
        m.mp_ass_subscript = Some(proxy_ass_subscript);
        t.tp_as_mapping = m;
        let s = &mut *(&raw mut SEQ);
        s.sq_contains = Some(proxy_contains);
        t.tp_as_sequence = s;
        let n = &mut *(&raw mut NUM);
        n.nb_or = Some(proxy_or);
        n.nb_inplace_or = Some(proxy_ior);
        t.tp_as_number = n;
        t.tp_methods = crate::mdefs![
            ("keys", NOARGS, m_keys),
            ("values", NOARGS, m_values),
            ("items", NOARGS, m_items),
            ("get", FAST, m_get),
            ("setdefault", FAST, m_setdefault),
            ("pop", FAST, m_pop),
            ("update", O, m_update),
            ("copy", NOARGS, m_copy),
            ("__reversed__", NOARGS, m_reversed),
            ("__reduce_ex__", O, m_reduce_ex),
            ("__sizeof__", NOARGS, m_sizeof),
        ];
    }
}
