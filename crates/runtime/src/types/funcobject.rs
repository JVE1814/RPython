//! Python functions (`function`), bound methods (`method`), `staticmethod`,
//! `classmethod` and `cell`.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::*;
use crate::types::{dict, none, str as pystr, tuple};
use crate::{name, raise};
use std::ffi::{c_int, c_void};

#[repr(C)]
pub struct PyFunctionObject {
    pub ob_base: PyObject,
    pub func_globals: *mut PyObject,
    pub func_builtins: *mut PyObject,
    pub func_name: *mut PyObject,
    pub func_qualname: *mut PyObject,
    pub func_code: *mut PyObject,
    pub func_defaults: *mut PyObject,
    pub func_kwdefaults: *mut PyObject,
    pub func_closure: *mut PyObject,
    pub func_doc: *mut PyObject,
    pub func_dict: *mut PyObject,
    pub func_weakreflist: *mut PyObject,
    pub func_module: *mut PyObject,
    pub func_annotations: *mut PyObject,
    pub func_annotate: *mut PyObject,
    pub func_typeparams: *mut PyObject,
    pub vectorcall: vectorcallfunc,
    pub func_version: u32,
}

#[repr(C)]
pub struct PyCellObject {
    pub ob_base: PyObject,
    pub ob_ref: *mut PyObject,
}

#[repr(C)]
pub struct StaticOrClassMethod {
    pub ob_base: PyObject,
    pub callable: *mut PyObject,
    pub dict: *mut PyObject,
}

#[unsafe(no_mangle)]
pub static mut PyFunction_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyMethod_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyStaticMethod_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyClassMethod_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyCell_Type: PyTypeObject = PyTypeObject::ZERO;

pub fn function_type() -> *mut PyTypeObject {
    &raw mut PyFunction_Type
}
pub fn method_type() -> *mut PyTypeObject {
    &raw mut PyMethod_Type
}
pub fn staticmethod_type() -> *mut PyTypeObject {
    &raw mut PyStaticMethod_Type
}
pub fn classmethod_type() -> *mut PyTypeObject {
    &raw mut PyClassMethod_Type
}
pub fn cell_type() -> *mut PyTypeObject {
    &raw mut PyCell_Type
}
#[inline]
pub fn is_function(o: &PyObject) -> bool {
    o.is_type(function_type())
}
#[inline]
pub fn is_method(o: &PyObject) -> bool {
    o.is_type(method_type())
}
#[inline]
pub fn func(o: &PyObject) -> &mut PyFunctionObject {
    unsafe { &mut *(o.as_ptr() as *mut PyFunctionObject) }
}

static FUNC_VERSION: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// Create a function from a code object.
pub fn new_function(code: &PyObject, globals: &PyObject, qualname: Option<&PyObject>) -> PyResult {
    unsafe {
        let op = crate::alloc::alloc_object(function_type(), std::mem::size_of::<PyFunctionObject>());
        let f = &mut *(op as *mut PyFunctionObject);
        // header-only accessors: a lazily loaded body stays unloaded until called
        f.func_code = code.to_ref().into_raw();
        f.func_globals = globals.to_ref().into_raw();
        f.func_name = crate::types::code::code_name(code).into_raw();
        f.func_qualname = match qualname {
            Some(q) => q.to_ref().into_raw(),
            None => crate::types::code::code_qualname(code).into_raw(),
        };
        // __builtins__ from globals, else the builtins module dict
        let b = match dict::get_item_str(globals, name!("__builtins__")) {
            Some(b) => {
                if crate::types::module::is_module(&b) {
                    crate::types::module::module_dict(&b)
                } else {
                    b
                }
            }
            None => crate::builtins::builtins_dict(),
        };
        f.func_builtins = b.into_raw();
        f.func_doc = match crate::types::code::code_docstring(code) {
            Some(d) => d.into_raw(),
            None => none::none_ref().into_raw(),
        };
        f.func_module = match dict::get_item_str(globals, name!("__name__")) {
            Some(m) => m.into_raw(),
            None => std::ptr::null_mut(),
        };
        f.vectorcall = Some(crate::interp::function_vectorcall);
        f.func_version = FUNC_VERSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        crate::gc::track(op);
        Ok(PyRef::from_raw(op))
    }
}

macro_rules! clear_field {
    ($f:expr) => {{
        let p = $f;
        $f = std::ptr::null_mut();
        xdecref(p);
    }};
}

unsafe extern "C" fn func_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        let f = &mut *(op as *mut PyFunctionObject);
        crate::types::weakref::clear_weakrefs_at(op, &mut f.func_weakreflist);
        func_clear(op);
        clear_field!(f.func_code);
        clear_field!(f.func_name);
        clear_field!(f.func_qualname);
        crate::alloc::free_object(op);
    }
}

unsafe extern "C" fn func_clear(op: *mut PyObject) -> c_int {
    unsafe {
        let f = &mut *(op as *mut PyFunctionObject);
        clear_field!(f.func_globals);
        clear_field!(f.func_builtins);
        clear_field!(f.func_module);
        clear_field!(f.func_defaults);
        clear_field!(f.func_kwdefaults);
        clear_field!(f.func_doc);
        clear_field!(f.func_dict);
        clear_field!(f.func_closure);
        clear_field!(f.func_annotations);
        clear_field!(f.func_annotate);
        clear_field!(f.func_typeparams);
        0
    }
}

unsafe extern "C" fn func_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let f = &*(op as *mut PyFunctionObject);
        let v = visit.unwrap();
        for p in [
            f.func_code,
            f.func_globals,
            f.func_builtins,
            f.func_module,
            f.func_defaults,
            f.func_kwdefaults,
            f.func_doc,
            f.func_name,
            f.func_dict,
            f.func_closure,
            f.func_annotations,
            f.func_annotate,
            f.func_qualname,
            f.func_typeparams,
        ] {
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

unsafe extern "C" fn func_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let f = &*(op as *mut PyFunctionObject);
        let q = pystr::to_string_lossy(&*f.func_qualname);
        into_c(Ok(pystr::new_str(&format!("<function {} at {:p}>", q, op))))
    }
}

unsafe extern "C" fn func_descr_get(f: *mut PyObject, obj: *mut PyObject, _tp: *mut PyObject) -> *mut PyObject {
    unsafe {
        if obj.is_null() || (*obj).is(none::none()) && false {
            return (*f).to_ref().into_raw();
        }
        new_method(&*f, &*obj).into_raw()
    }
}

fn opt(p: *mut PyObject) -> PyResult {
    unsafe { Ok(if p.is_null() { none::none_ref() } else { PyRef::from_borrowed(p) }) }
}
fn replace(slot: &mut *mut PyObject, v: Option<&PyObject>) {
    let old = *slot;
    *slot = v.map_or(std::ptr::null_mut(), |x| x.to_ref().into_raw());
    unsafe { xdecref(old) };
}

fn f_code(o: &PyObject) -> PyResult {
    opt(func(o).func_code)
}
fn f_set_code(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        Some(v) if crate::types::code::is_code(v) => v,
        Some(_) | None => raise!(TypeError, "__code__ must be set to a code object"),
    };
    let nfree = crate::types::code::code(v).nfreevars();
    let nclosure = {
        let c = func(o).func_closure;
        if c.is_null() { 0 } else { tuple::tuple_len(unsafe { &*c }) }
    };
    if nfree != nclosure {
        raise!(
            ValueError,
            "{}() requires a code object with {} free vars, not {}",
            pystr::to_string_lossy(unsafe { &*func(o).func_name }),
            nclosure,
            nfree
        );
    }
    // perf-runtime: running call frames borrow their code object from the function
    // (interp::frame, no refcount traffic per call), so the old code object must
    // outlive any frame still executing it: it is retired instead of released.
    let old = std::mem::replace(&mut func(o).func_code, v.to_ref().into_raw());
    if !old.is_null() {
        static RETIRED_CODE: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());
        RETIRED_CODE.lock().unwrap().push(old as usize);
    }
    func(o).func_version = FUNC_VERSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}
fn f_defaults(o: &PyObject) -> PyResult {
    opt(func(o).func_defaults)
}
fn f_set_defaults(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        Some(v) if v.is(none::none()) => None,
        Some(v) if tuple::is_tuple(v) => Some(v),
        None => None,
        Some(_) => raise!(TypeError, "__defaults__ must be set to a tuple object"),
    };
    replace(&mut func(o).func_defaults, v);
    func(o).func_version = FUNC_VERSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}
fn f_kwdefaults(o: &PyObject) -> PyResult {
    opt(func(o).func_kwdefaults)
}
fn f_set_kwdefaults(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        Some(v) if v.is(none::none()) => None,
        Some(v) if dict::is_dict(v) => Some(v),
        None => None,
        Some(_) => raise!(TypeError, "__kwdefaults__ must be set to a dict object"),
    };
    replace(&mut func(o).func_kwdefaults, v);
    Ok(())
}
fn f_globals(o: &PyObject) -> PyResult {
    opt(func(o).func_globals)
}
fn f_builtins(o: &PyObject) -> PyResult {
    opt(func(o).func_builtins)
}
fn f_closure(o: &PyObject) -> PyResult {
    opt(func(o).func_closure)
}
fn f_name(o: &PyObject) -> PyResult {
    opt(func(o).func_name)
}
fn f_set_name(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match v {
        Some(v) if pystr::is_str(v) => {
            replace(&mut func(o).func_name, Some(v));
            Ok(())
        }
        _ => raise!(TypeError, "__name__ must be set to a string object"),
    }
}
fn f_qualname(o: &PyObject) -> PyResult {
    opt(func(o).func_qualname)
}
fn f_set_qualname(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match v {
        Some(v) if pystr::is_str(v) => {
            replace(&mut func(o).func_qualname, Some(v));
            Ok(())
        }
        _ => raise!(TypeError, "__qualname__ must be set to a string object"),
    }
}
fn f_doc(o: &PyObject) -> PyResult {
    opt(func(o).func_doc)
}
fn f_set_doc(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    replace(&mut func(o).func_doc, Some(v.unwrap_or(none::none())));
    Ok(())
}
fn f_module(o: &PyObject) -> PyResult {
    opt(func(o).func_module)
}
fn f_set_module(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    replace(&mut func(o).func_module, v);
    Ok(())
}
fn f_dict(o: &PyObject) -> PyResult {
    let f = func(o);
    if f.func_dict.is_null() {
        f.func_dict = dict::new_dict().into_raw();
    }
    opt(f.func_dict)
}
fn f_set_dict(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match v {
        Some(v) if dict::is_dict(v) => {
            replace(&mut func(o).func_dict, Some(v));
            Ok(())
        }
        None => raise!(TypeError, "cannot delete __dict__"),
        _ => raise!(TypeError, "__dict__ must be set to a dictionary, not a '{}'", tname(v.unwrap())),
    }
}
fn f_annotate(o: &PyObject) -> PyResult {
    opt(func(o).func_annotate)
}
fn f_set_annotate(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match v {
        None => raise!(TypeError, "__annotate__ cannot be deleted"),
        Some(v) if v.is(none::none()) => {
            replace(&mut func(o).func_annotate, None);
            Ok(())
        }
        Some(v) if ab::is_callable(v) => {
            replace(&mut func(o).func_annotate, Some(v));
            replace(&mut func(o).func_annotations, None);
            Ok(())
        }
        Some(_) => raise!(TypeError, "__annotate__ must be callable or None"),
    }
}
fn f_annotations(o: &PyObject) -> PyResult {
    let f = func(o);
    if f.func_annotations.is_null() {
        if !f.func_annotate.is_null() && !unsafe { &*f.func_annotate }.is(none::none()) {
            let one = crate::types::int::new_int(1);
            let r = ab::call(unsafe { &*f.func_annotate }, &[&one])?;
            if !dict::is_dict(&r) {
                raise!(TypeError, "__annotate__ returned non-dict of type '{}'", tname(&r));
            }
            f.func_annotations = r.into_raw();
        } else {
            f.func_annotations = dict::new_dict().into_raw();
        }
    }
    opt(f.func_annotations)
}
fn f_set_annotations(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        Some(v) if v.is(none::none()) => None,
        Some(v) if dict::is_dict(v) => Some(v),
        None => None,
        Some(_) => raise!(TypeError, "__annotations__ must be set to a dict object"),
    };
    replace(&mut func(o).func_annotations, v);
    replace(&mut func(o).func_annotate, None);
    Ok(())
}
fn f_type_params(o: &PyObject) -> PyResult {
    let p = func(o).func_typeparams;
    if p.is_null() { Ok(tuple::empty_tuple()) } else { opt(p) }
}
fn f_set_type_params(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match v {
        Some(v) if tuple::is_tuple(v) => {
            replace(&mut func(o).func_typeparams, Some(v));
            Ok(())
        }
        _ => raise!(TypeError, "__type_params__ must be set to a tuple"),
    }
}

unsafe extern "C" fn func_new(_tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let a = crate::args::kwdict_to_vectorcall(&*args, kw);
        let (all, kwn) = (&a.0, a.1.as_deref());
        let refs: Vec<&PyObject> = all.iter().map(|r| &**r).collect();
        static P: crate::args::Params = crate::args::Params::new(
            "function",
            &["code", "globals", "name", "argdefs", "closure", "kwdefaults"],
            0,
            2,
        );
        let r = (|| -> PyResult {
            let [code, globals, nm, argdefs, closure, kwdefaults] = crate::args::parse::<6>(&P, &refs, kwn)?;
            let code = code.unwrap();
            let globals = globals.unwrap();
            if !crate::types::code::is_code(code) {
                raise!(TypeError, "function() argument 'code' must be code, not {}", tname(code));
            }
            if !dict::is_dict(globals) {
                raise!(TypeError, "function() argument 'globals' must be dict, not {}", tname(globals));
            }
            let f = new_function(code, globals, None)?;
            if let Some(n) = nm {
                if !n.is(none::none()) {
                    if !pystr::is_str(n) {
                        raise!(TypeError, "arg 3 (name) must be None or string");
                    }
                    replace(&mut func(&f).func_name, Some(n));
                }
            }
            if let Some(d) = argdefs {
                if !d.is(none::none()) {
                    if !tuple::is_tuple(d) {
                        raise!(TypeError, "arg 4 (defaults) must be None or tuple");
                    }
                    replace(&mut func(&f).func_defaults, Some(d));
                }
            }
            let nfree = crate::types::code::code(code).nfreevars();
            match closure {
                Some(c) if !c.is(none::none()) => {
                    if !tuple::is_tuple(c) {
                        raise!(TypeError, "arg 5 (closure) must be tuple");
                    }
                    if tuple::tuple_len(c) != nfree {
                        raise!(
                            ValueError,
                            "{} requires closure of length {}, not {}",
                            pystr::to_string_lossy(&crate::types::code::code(code).co_name),
                            nfree,
                            tuple::tuple_len(c)
                        );
                    }
                    for it in tuple::tuple_items(c) {
                        if !it.is_type(cell_type()) {
                            raise!(TypeError, "arg 5 (closure) expected cell, found {}", tname(it));
                        }
                    }
                    replace(&mut func(&f).func_closure, Some(c));
                }
                _ => {
                    if nfree > 0 {
                        raise!(TypeError, "arg 5 (closure) must be tuple");
                    }
                }
            }
            if let Some(k) = kwdefaults {
                if !k.is(none::none()) {
                    if !dict::is_dict(k) {
                        raise!(TypeError, "arg 6 (kwdefaults) must be None or dict");
                    }
                    replace(&mut func(&f).func_kwdefaults, Some(k));
                }
            }
            Ok(f)
        })();
        into_c(r)
    }
}

// ---------------------------------------------------------------- bound method

#[inline]
pub fn new_method(f: &PyObject, slf: &PyObject) -> PyRef {
    unsafe {
        let op = crate::alloc::alloc_object(method_type(), std::mem::size_of::<PyMethodObject>());
        let m = &mut *(op as *mut PyMethodObject);
        m.im_func = f.to_ref().into_raw();
        m.im_self = slf.to_ref().into_raw();
        m.vectorcall = Some(method_vectorcall);
        crate::gc::track(op);
        PyRef::from_raw(op)
    }
}

pub fn method_parts(o: &PyObject) -> (&PyObject, &PyObject) {
    unsafe {
        let m = &*(o.as_ptr() as *mut PyMethodObject);
        (&*m.im_func, &*m.im_self)
    }
}

unsafe extern "C" fn method_vectorcall(
    m: *mut PyObject,
    args: *const *mut PyObject,
    nargsf: usize,
    kwnames: *mut PyObject,
) -> *mut PyObject {
    unsafe {
        let (f, s) = method_parts(&*m);
        let nargs = nargsf & !PY_VECTORCALL_ARGUMENTS_OFFSET;
        let nkw = if kwnames.is_null() { 0 } else { tuple::tuple_len(&*kwnames) };
        let total = nargs + nkw;
        if nargsf & PY_VECTORCALL_ARGUMENTS_OFFSET != 0 {
            // we may temporarily overwrite args[-1]
            let slot = (args as *mut *mut PyObject).sub(1);
            let saved = *slot;
            *slot = s.as_ptr();
            let r = ab::vectorcall_raw(f, slot, nargs + 1, kwnames);
            *slot = saved;
            return r;
        }
        let mut v: Vec<*mut PyObject> = Vec::with_capacity(total + 2);
        v.push(std::ptr::null_mut());
        v.push(s.as_ptr());
        if total > 0 {
            v.extend_from_slice(std::slice::from_raw_parts(args, total));
        }
        ab::vectorcall_raw(f, v.as_ptr().add(1), (nargs + 1) | PY_VECTORCALL_ARGUMENTS_OFFSET, kwnames)
    }
}

unsafe extern "C" fn method_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        let m = &mut *(op as *mut PyMethodObject);
        crate::types::weakref::clear_weakrefs_at(op, &mut m.im_weakreflist);
        xdecref(m.im_func);
        xdecref(m.im_self);
        crate::alloc::free_object(op);
    }
}
unsafe extern "C" fn method_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let m = &*(op as *mut PyMethodObject);
        let v = visit.unwrap();
        for p in [m.im_func, m.im_self] {
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
unsafe extern "C" fn method_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let (f, s) = method_parts(&*op);
        let fname = match ab::getattr_opt(f, name!("__qualname__")) {
            Ok(Some(n)) if pystr::is_str(&n) => pystr::to_string_lossy(&n),
            _ => match ab::getattr_opt(f, name!("__name__")) {
                Ok(Some(n)) if pystr::is_str(&n) => pystr::to_string_lossy(&n),
                _ => "?".into(),
            },
        };
        let srepr = match ab::repr(s) {
            Ok(r) => pystr::to_string_lossy(&r),
            Err(_) => return std::ptr::null_mut(),
        };
        into_c(Ok(pystr::new_str(&format!("<bound method {} of {}>", fname, srepr))))
    }
}
unsafe extern "C" fn method_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        if (op != Py_EQ && op != Py_NE) || !is_method(&*b) {
            return none::not_implemented().to_ref().into_raw();
        }
        let (fa, sa) = method_parts(&*a);
        let (fb, sb) = method_parts(&*b);
        let eq = match ab::rich_eq(fa, fb) {
            Ok(e) => e && sa.is(sb),
            Err(_) => return std::ptr::null_mut(),
        };
        crate::types::bool_::new_bool(if op == Py_EQ { eq } else { !eq }).into_raw()
    }
}
unsafe extern "C" fn method_hash(op: *mut PyObject) -> Py_hash_t {
    unsafe {
        let (f, s) = method_parts(&*op);
        let a = crate::hash::hash_pointer(s.as_ptr() as usize);
        let b = match ab::hash(f) {
            Ok(h) => h,
            Err(_) => return -1,
        };
        let h = a ^ b;
        if h == -1 { -2 } else { h }
    }
}
unsafe extern "C" fn method_getattro(op: *mut PyObject, nm: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        let n = &*nm;
        // descriptors on the method type take priority, then delegate to the function
        if let Some(d) = lookup(o.type_ptr(), n) {
            if let Some(g) = d.ty().tp_descr_get {
                return g(d.as_ptr(), op, o.type_ptr() as *mut PyObject);
            }
            return d.into_raw();
        }
        let (f, _) = method_parts(o);
        into_c(ab::getattr(f, n))
    }
}
unsafe extern "C" fn method_descr_get(m: *mut PyObject, _obj: *mut PyObject, _tp: *mut PyObject) -> *mut PyObject {
    unsafe { (*m).to_ref().into_raw() }
}
fn m_func(o: &PyObject) -> PyResult {
    Ok(method_parts(o).0.to_ref())
}
fn m_self(o: &PyObject) -> PyResult {
    Ok(method_parts(o).1.to_ref())
}
fn m_doc(o: &PyObject) -> PyResult {
    ab::getattr(method_parts(o).0, name!("__doc__"))
}
fn m_reduce(o: &PyObject) -> PyResult {
    let (f, s) = method_parts(o);
    let getattr = crate::builtins::builtin("getattr")?;
    let mut nm = ab::getattr(f, name!("__name__"))?;
    // A private method is reachable through getattr() only under its
    // mangled name; the defining class is the next-to-last qualname part.
    if let Some(n) = pystr::to_string_strict(&nm) {
        if n.starts_with("__") && !n.ends_with("__") {
            if let Ok(Some(q)) = ab::getattr_opt(f, name!("__qualname__")) {
                let q = pystr::to_string_lossy(&q);
                let parts: Vec<&str> = q.split('.').collect();
                if parts.len() >= 2 {
                    let owner = parts[parts.len() - 2].trim_start_matches('_');
                    if !owner.is_empty() && owner != "<locals>" {
                        nm = pystr::new_str(&format!("_{}{}", owner, n));
                    }
                }
            }
        }
    }
    Ok(tuple::new_tuple(vec![getattr, tuple::new_tuple(vec![s.to_ref(), nm])]))
}
unsafe extern "C" fn method_new(_tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        if !kw.is_null() && crate::types::dict::dict_len(&*kw) > 0 {
            return into_c(Err(err!(TypeError, "method() takes no keyword arguments")));
        }
        let a = tuple::tuple_items(&*args);
        if a.len() != 2 {
            return into_c(Err(err!(TypeError, "method expected 2 arguments, got {}", a.len())));
        }
        if !ab::is_callable(a[0]) {
            return into_c(Err(err!(TypeError, "first argument must be callable")));
        }
        if a[1].is(none::none()) {
            return into_c(Err(err!(TypeError, "instance must not be None")));
        }
        new_method(a[0], a[1]).into_raw()
    }
}

// ---------------------------------------------------------------- staticmethod / classmethod

fn sm(o: &PyObject) -> &mut StaticOrClassMethod {
    unsafe { &mut *(o.as_ptr() as *mut StaticOrClassMethod) }
}

pub fn new_staticmethod(f: PyRef) -> PyRef {
    unsafe {
        let op = crate::types::typeobject::generic_alloc(staticmethod_type(), 0);
        sm(&*op).callable = f.into_raw();
        let r = PyRef::from_raw(op);
        // copy __name__ etc. like the constructor does
        let c = (*sm(&r).callable).to_ref();
        if copy_wrapped_attrs(&r, &c).is_err() {
            clear_exc();
        }
        r
    }
}
pub fn new_classmethod(f: PyRef) -> PyRef {
    unsafe {
        let op = crate::types::typeobject::generic_alloc(classmethod_type(), 0);
        sm(&*op).callable = f.into_raw();
        let r = PyRef::from_raw(op);
        // copy __name__ etc. like the constructor does
        let c = (*sm(&r).callable).to_ref();
        if copy_wrapped_attrs(&r, &c).is_err() {
            clear_exc();
        }
        r
    }
}
pub fn sm_callable(o: &PyObject) -> Option<&PyObject> {
    let p = sm(o).callable;
    if p.is_null() { None } else { Some(unsafe { &*p }) }
}

unsafe extern "C" fn sm_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        let s = sm(&*op);
        clear_field!(s.callable);
        clear_field!(s.dict);
        let tp = (*op).type_ptr();
        generic_free_and_type_decref(op, tp);
    }
}
unsafe extern "C" fn sm_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let s = sm(&*op);
        let v = visit.unwrap();
        for p in [s.callable, s.dict] {
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
unsafe extern "C" fn sm_clear(op: *mut PyObject) -> c_int {
    unsafe {
        let s = sm(&*op);
        clear_field!(s.callable);
        clear_field!(s.dict);
        0
    }
}
/// Copy __module__, __name__, __qualname__, __doc__ (and __annotations__ lazily) like functools.wraps.
fn copy_wrapped_attrs(o: &PyObject, f: &PyObject) -> PyResult<()> {
    let d = f_dict_of(o);
    for n in ["__module__", "__name__", "__qualname__", "__doc__"] {
        let k = pystr::intern_str(n);
        if let Some(v) = ab::getattr_opt(f, &k)? {
            dict::set_item(&d, &k, &v)?;
        }
    }
    Ok(())
}
fn f_dict_of(o: &PyObject) -> PyRef {
    let s = sm(o);
    if s.dict.is_null() {
        s.dict = dict::new_dict().into_raw();
    }
    unsafe { PyRef::from_borrowed(s.dict) }
}
unsafe extern "C" fn sm_init(op: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let o = &*op;
        let nm = if o.is_type(classmethod_type()) || is_subtype(o.type_ptr(), classmethod_type()) {
            "classmethod"
        } else {
            "staticmethod"
        };
        if !kw.is_null() && dict::dict_len(&*kw) > 0 {
            let _ = err!(TypeError, "{}() takes no keyword arguments", nm);
            return -1;
        }
        let a = tuple::tuple_items(&*args);
        if a.len() != 1 {
            let _ = err!(TypeError, "{} expected 1 argument, got {}", nm, a.len());
            return -1;
        }
        let s = sm(o);
        let old = s.callable;
        s.callable = a[0].to_ref().into_raw();
        xdecref(old);
        int_into_c(copy_wrapped_attrs(o, a[0]))
    }
}
unsafe extern "C" fn sm_descr_get(op: *mut PyObject, _obj: *mut PyObject, _tp: *mut PyObject) -> *mut PyObject {
    unsafe {
        match sm_callable(&*op) {
            Some(c) => c.to_ref().into_raw(),
            None => into_c(Err(err!(RuntimeError, "uninitialized staticmethod object"))),
        }
    }
}
unsafe extern "C" fn sm_call(op: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        match sm_callable(&*op) {
            Some(c) => into_c(ab::call_with_kwargs_dict(c, tuple::tuple_items(&*args), if kw.is_null() { None } else { Some(&*kw) })),
            None => into_c(Err(err!(RuntimeError, "uninitialized staticmethod object"))),
        }
    }
}
unsafe extern "C" fn cm_descr_get(op: *mut PyObject, obj: *mut PyObject, tp: *mut PyObject) -> *mut PyObject {
    unsafe {
        let c = match sm_callable(&*op) {
            Some(c) => c,
            None => return into_c(Err(err!(RuntimeError, "uninitialized classmethod object"))),
        };
        let t = if tp.is_null() { (*obj).type_ptr() as *mut PyObject } else { tp };
        new_method(c, &*t).into_raw()
    }
}
fn sm_func(o: &PyObject) -> PyResult {
    match sm_callable(o) {
        Some(c) => Ok(c.to_ref()),
        None => Ok(none::none_ref()),
    }
}
fn sm_isabstract(o: &PyObject) -> PyResult {
    match sm_callable(o) {
        Some(c) => match ab::getattr_opt(c, name!("__isabstractmethod__"))? {
            Some(v) => Ok(crate::types::bool_::new_bool(ab::is_true(&v)?)),
            None => Ok(crate::types::bool_::new_bool(false)),
        },
        None => Ok(crate::types::bool_::new_bool(false)),
    }
}
fn sm_get_dict(o: &PyObject) -> PyResult {
    Ok(f_dict_of(o))
}
fn sm_annotations(o: &PyObject) -> PyResult {
    let d = f_dict_of(o);
    if let Some(v) = dict::get_item_str(&d, name!("__annotations__")) {
        return Ok(v);
    }
    match sm_callable(o) {
        Some(c) => {
            let v = ab::getattr(c, name!("__annotations__"))?;
            dict::set_item(&d, name!("__annotations__"), &v)?;
            Ok(v)
        }
        None => Ok(dict::new_dict()),
    }
}
fn sm_set_annotations(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let d = f_dict_of(o);
    match v {
        Some(v) => dict::set_item(&d, name!("__annotations__"), v),
        None => {
            if !dict::del_item(&d, name!("__annotations__"))? {
                raise!(AttributeError, "__annotations__");
            }
            Ok(())
        }
    }
}
fn sm_annotate(o: &PyObject) -> PyResult {
    let d = f_dict_of(o);
    if let Some(v) = dict::get_item_str(&d, name!("__annotate__")) {
        return Ok(v);
    }
    match sm_callable(o) {
        Some(c) => {
            let v = ab::getattr(c, name!("__annotate__"))?;
            dict::set_item(&d, name!("__annotate__"), &v)?;
            Ok(v)
        }
        None => Ok(none::none_ref()),
    }
}
fn sm_set_annotate(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let d = f_dict_of(o);
    match v {
        Some(v) => dict::set_item(&d, name!("__annotate__"), v),
        None => {
            if !dict::del_item(&d, name!("__annotate__"))? {
                raise!(AttributeError, "__annotate__");
            }
            Ok(())
        }
    }
}
unsafe extern "C" fn sm_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        let n = type_name(o.ty());
        let inner = match sm_callable(o) {
            Some(c) => match ab::repr(c) {
                Ok(r) => pystr::to_string_lossy(&r),
                Err(_) => return std::ptr::null_mut(),
            },
            None => "None".into(),
        };
        into_c(Ok(pystr::new_str(&format!("<{}({})>", n, inner))))
    }
}

// ---------------------------------------------------------------- cell

pub fn new_cell(v: Option<PyRef>) -> PyRef {
    unsafe {
        let op = crate::alloc::alloc_object(cell_type(), std::mem::size_of::<PyCellObject>());
        (*(op as *mut PyCellObject)).ob_ref = v.map_or(std::ptr::null_mut(), |v| v.into_raw());
        crate::gc::track(op);
        PyRef::from_raw(op)
    }
}
#[inline]
pub fn cell_get(c: &PyObject) -> Option<PyRef> {
    unsafe {
        let p = (*(c.as_ptr() as *mut PyCellObject)).ob_ref;
        if p.is_null() { None } else { Some(PyRef::from_borrowed(p)) }
    }
}
#[inline]
pub fn cell_set(c: &PyObject, v: Option<PyRef>) {
    unsafe {
        let slot = &mut (*(c.as_ptr() as *mut PyCellObject)).ob_ref;
        let old = *slot;
        *slot = v.map_or(std::ptr::null_mut(), |v| v.into_raw());
        xdecref(old);
    }
}
unsafe extern "C" fn cell_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        xdecref((*(op as *mut PyCellObject)).ob_ref);
        crate::alloc::free_object(op);
    }
}
unsafe extern "C" fn cell_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let p = (*(op as *mut PyCellObject)).ob_ref;
        if !p.is_null() {
            return visit.unwrap()(p, arg);
        }
        0
    }
}
unsafe extern "C" fn cell_clear(op: *mut PyObject) -> c_int {
    unsafe {
        let c = &mut (*(op as *mut PyCellObject)).ob_ref;
        clear_field!(*c);
        0
    }
}
unsafe extern "C" fn cell_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let s = match cell_get(&*op) {
            None => format!("<cell at {:p}: empty>", op),
            Some(v) => format!("<cell at {:p}: {} object at {:p}>", op, type_name(v.ty()), v.as_ptr()),
        };
        into_c(Ok(pystr::new_str(&s)))
    }
}
unsafe extern "C" fn cell_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        if !(*b).is_type(cell_type()) {
            return none::not_implemented().to_ref().into_raw();
        }
        let x = cell_get(&*a);
        let y = cell_get(&*b);
        match (x, y) {
            (Some(x), Some(y)) => into_c(ab::rich_compare(&x, &y, op)),
            (x, y) => {
                let (ex, ey) = (x.is_some() as i32, y.is_some() as i32);
                let r = match op {
                    Py_LT => ex < ey,
                    Py_LE => ex <= ey,
                    Py_EQ => ex == ey,
                    Py_NE => ex != ey,
                    Py_GT => ex > ey,
                    _ => ex >= ey,
                };
                crate::types::bool_::new_bool(r).into_raw()
            }
        }
    }
}
fn cell_contents(o: &PyObject) -> PyResult {
    match cell_get(o) {
        Some(v) => Ok(v),
        None => raise!(ValueError, "Cell is empty"),
    }
}
fn cell_set_contents(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    cell_set(o, v.map(|v| v.to_ref()));
    Ok(())
}
unsafe extern "C" fn cell_new(_tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        if !kw.is_null() && dict::dict_len(&*kw) > 0 {
            return into_c(Err(err!(TypeError, "cell() takes no keyword arguments")));
        }
        let a = tuple::tuple_items(&*args);
        if a.len() > 1 {
            return into_c(Err(err!(TypeError, "cell expected at most 1 argument, got {}", a.len())));
        }
        new_cell(a.first().map(|x| x.to_ref())).into_raw()
    }
}

pub fn init_types() {
    unsafe {
        let t = &mut *function_type();
        t.tp_name = c"function".as_ptr();
        t.tp_doc = c"function(code, globals, name=None, argdefs=None, closure=None, kwdefaults=None)\n--\n\nBuild a Python function from a code object and a globals dict.".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyFunctionObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_HAVE_VECTORCALL | Py_TPFLAGS_METHOD_DESCRIPTOR;
        t.tp_dealloc = Some(func_dealloc);
        t.tp_traverse = Some(func_traverse);
        t.tp_clear = Some(func_clear);
        t.tp_repr = Some(func_repr);
        t.tp_call = Some(crate::abstract_::vectorcall_call);
        t.tp_vectorcall_offset = std::mem::offset_of!(PyFunctionObject, vectorcall) as isize;
        t.tp_descr_get = Some(func_descr_get);
        t.tp_dictoffset = std::mem::offset_of!(PyFunctionObject, func_dict) as isize;
        t.tp_weaklistoffset = std::mem::offset_of!(PyFunctionObject, func_weakreflist) as isize;
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_setattro = Some(object_generic_setattro);
        t.tp_new = Some(func_new);
        t.tp_getset = crate::getset_table![
            ("__code__", f_code, f_set_code),
            ("__defaults__", f_defaults, f_set_defaults),
            ("__kwdefaults__", f_kwdefaults, f_set_kwdefaults),
            ("__annotations__", f_annotations, f_set_annotations),
            ("__annotate__", f_annotate, f_set_annotate),
            ("__dict__", f_dict, f_set_dict),
            ("__name__", f_name, f_set_name),
            ("__qualname__", f_qualname, f_set_qualname),
            ("__type_params__", f_type_params, f_set_type_params),
        ];
        t.tp_members = crate::member_table![
            ("__closure__", Py_T_OBJECT, std::mem::offset_of!(PyFunctionObject, func_closure), Py_READONLY),
            ("__doc__", Py_T_OBJECT, std::mem::offset_of!(PyFunctionObject, func_doc), 0),
            ("__globals__", Py_T_OBJECT, std::mem::offset_of!(PyFunctionObject, func_globals), Py_READONLY),
            ("__module__", Py_T_OBJECT, std::mem::offset_of!(PyFunctionObject, func_module), 0),
            ("__builtins__", Py_T_OBJECT, std::mem::offset_of!(PyFunctionObject, func_builtins), Py_READONLY),
        ];
        let _ = (f_globals, f_builtins, f_closure, f_doc, f_set_doc, f_module, f_set_module);

        let t = &mut *method_type();
        t.tp_name = c"method".as_ptr();
        t.tp_doc = c"method(function, instance, /)\n--\n\nBind a callable to an instance.".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyMethodObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_HAVE_VECTORCALL;
        t.tp_dealloc = Some(method_dealloc);
        t.tp_traverse = Some(method_traverse);
        t.tp_repr = Some(method_repr);
        t.tp_richcompare = Some(method_richcompare);
        t.tp_hash = Some(method_hash);
        t.tp_call = Some(crate::abstract_::vectorcall_call);
        t.tp_vectorcall_offset = std::mem::offset_of!(PyMethodObject, vectorcall) as isize;
        t.tp_weaklistoffset = std::mem::offset_of!(PyMethodObject, im_weakreflist) as isize;
        t.tp_getattro = Some(method_getattro);
        t.tp_setattro = Some(object_generic_setattro);
        t.tp_descr_get = Some(method_descr_get);
        t.tp_new = Some(method_new);
        t.tp_getset = crate::getset_table![("__func__", m_func), ("__self__", m_self), ("__doc__", m_doc)];
        t.tp_methods = crate::mdefs![("__reduce__", NOARGS, m_reduce, "__reduce__($self, /)\n--\n\nHelper for pickle.")];

        for (tp, nm, get) in [
            (staticmethod_type(), c"staticmethod", sm_descr_get as unsafe extern "C" fn(_, _, _) -> _),
            (classmethod_type(), c"classmethod", cm_descr_get as unsafe extern "C" fn(_, _, _) -> _),
        ] {
            let t = &mut *tp;
            t.tp_name = nm.as_ptr();
            t.tp_doc = if tp == staticmethod_type() {
                c"staticmethod(function, /)\n--\n\nWrap a function so that it is not bound when looked up on a class or instance.".as_ptr()
            } else {
                c"classmethod(function, /)\n--\n\nWrap a function so that it receives the class as its first argument.".as_ptr()
            };
            t.tp_basicsize = std::mem::size_of::<StaticOrClassMethod>() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_BASETYPE;
            t.tp_dealloc = Some(sm_dealloc);
            t.tp_traverse = Some(sm_traverse);
            t.tp_clear = Some(sm_clear);
            t.tp_descr_get = Some(get);
            t.tp_init = Some(sm_init);
            t.tp_new = Some(crate::types::object_type::generic_new);
            t.tp_alloc = Some(generic_alloc);
            t.tp_free = Some(generic_free);
            t.tp_repr = Some(sm_repr);
            t.tp_dictoffset = std::mem::offset_of!(StaticOrClassMethod, dict) as isize;
            t.tp_getattro = Some(object_generic_getattro);
            t.tp_setattro = Some(object_generic_setattro);
            t.tp_members = crate::member_table![
                ("__func__", Py_T_OBJECT, std::mem::offset_of!(StaticOrClassMethod, callable), Py_READONLY),
                ("__wrapped__", Py_T_OBJECT, std::mem::offset_of!(StaticOrClassMethod, callable), Py_READONLY),
            ];
            t.tp_getset = crate::getset_table![
                ("__isabstractmethod__", sm_isabstract),
                ("__dict__", sm_get_dict),
                ("__annotations__", sm_annotations, sm_set_annotations),
                ("__annotate__", sm_annotate, sm_set_annotate),
            ];
            let _ = sm_func;
        }
        (*staticmethod_type()).tp_call = Some(sm_call);

        let t = &mut *cell_type();
        t.tp_name = c"cell".as_ptr();
        t.tp_doc = c"cell(contents=<unrepresentable>, /)\n--\n\nA box holding one variable shared between nested scopes.".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyCellObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
        t.tp_dealloc = Some(cell_dealloc);
        t.tp_traverse = Some(cell_traverse);
        t.tp_clear = Some(cell_clear);
        t.tp_repr = Some(cell_repr);
        t.tp_richcompare = Some(cell_richcompare);
        t.tp_hash = Some(crate::types::slots::hash_not_implemented);
        t.tp_new = Some(cell_new);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_getset = crate::getset_table![("cell_contents", cell_contents, cell_set_contents)];
    }
}
