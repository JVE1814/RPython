//! `builtin_function_or_method` (PyCFunctionObject) and `builtin_method` (PyCMethodObject).

use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::*;
use crate::types::{none, str as pystr, tuple};
use std::ffi::{CStr, c_int, c_void};

#[unsafe(no_mangle)]
pub static mut PyCFunction_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyCMethod_Type: PyTypeObject = PyTypeObject::ZERO;

pub fn cfunction_type() -> *mut PyTypeObject {
    &raw mut PyCFunction_Type
}
pub fn cmethod_type() -> *mut PyTypeObject {
    &raw mut PyCMethod_Type
}
pub fn is_cfunction(o: &PyObject) -> bool {
    o.is_type(cfunction_type()) || o.is_type(cmethod_type()) || is_subtype(o.type_ptr(), cfunction_type())
}

/// (tail3) Is this a builtin function/method using the tuple-and-dict convention
/// (METH_VARARGS|METH_KEYWORDS)?  CPython calls those through tp_call, so a
/// `**kwargs` dict reaches the C function unchanged (non-str keys included).
pub fn takes_args_and_dict(o: &PyObject) -> bool {
    is_cfunction(o) && unsafe { (*(*fptr(o)).m_ml).ml_flags & !METH_COEXIST == METH_VARARGS | METH_KEYWORDS }
}

/// Call a [`takes_args_and_dict`] builtin with the argument tuple and keyword dict as is.
pub fn call_args_and_dict(o: &PyObject, args: &PyObject, kw: Option<&PyObject>) -> PyResult {
    unsafe {
        let ff = fptr(o);
        let f: unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject =
            std::mem::transmute((*(*ff).m_ml).ml_meth);
        let slf = if (*(*ff).m_ml).ml_flags & METH_STATIC != 0 && !(*ff).m_self.is_null() && is_type(&*(*ff).m_self) {
            std::ptr::null_mut()
        } else {
            (*ff).m_self
        };
        from_c(f(slf, args.as_ptr(), kw.map_or(std::ptr::null_mut(), |d| d.as_ptr())))
    }
}

/// Create a builtin function/method object. `slf` is the bound object (or module).
pub fn new_cfunction(ml: *mut PyMethodDef, slf: Option<PyRef>, module: Option<PyRef>) -> PyRef {
    unsafe {
        let op = crate::alloc::alloc_object(cfunction_type(), std::mem::size_of::<PyCMethodObject>());
        let f = op as *mut PyCFunctionObject;
        (*f).m_ml = ml;
        (*f).m_self = slf.map_or(std::ptr::null_mut(), |s| s.into_raw());
        (*f).m_module = module.map_or(std::ptr::null_mut(), |m| m.into_raw());
        (*f).vectorcall = Some(cfunction_vectorcall);
        crate::gc::track(op);
        PyRef::from_raw(op)
    }
}

pub fn set_defining_class(f: &PyObject, cls: *mut PyTypeObject) {
    unsafe {
        let m = f.as_ptr() as *mut PyCMethodObject;
        incref(cls as *mut PyObject);
        let old = (*m).mm_class;
        (*m).mm_class = cls;
        xdecref(old as *mut PyObject);
        (*f.as_ptr()).ob_type.store(cmethod_type(), std::sync::atomic::Ordering::Relaxed);
    }
}

fn fptr(o: &PyObject) -> *mut PyCFunctionObject {
    o.as_ptr() as *mut PyCFunctionObject
}

pub fn cfunction_name(o: &PyObject) -> String {
    unsafe { CStr::from_ptr((*(*fptr(o)).m_ml).ml_name).to_string_lossy().into_owned() }
}

fn self_is_meaningful(o: &PyObject) -> Option<&PyObject> {
    unsafe {
        let s = (*fptr(o)).m_self;
        if s.is_null() || crate::types::module::is_module(&*s) {
            None
        } else {
            Some(&*s)
        }
    }
}

pub fn cfunction_qualname(o: &PyObject) -> String {
    let n = cfunction_name(o);
    match self_is_meaningful(o) {
        None => n,
        Some(s) => {
            let t = if is_type(s) { unsafe { &*as_type(s) } } else { s.ty() };
            format!("{}.{}", type_qualname(t), n)
        }
    }
}

unsafe extern "C" fn cfunction_vectorcall(
    f: *mut PyObject,
    args: *const *mut PyObject,
    nargsf: usize,
    kwnames: *mut PyObject,
) -> *mut PyObject {
    unsafe {
        let ff = fptr(&*f);
        let nargs = nargsf & !PY_VECTORCALL_ARGUMENTS_OFFSET;
        let nkw = if kwnames.is_null() { 0 } else { tuple::tuple_len(&*kwnames) };
        let all: &[*mut PyObject] = if nargs + nkw == 0 { &[] } else { std::slice::from_raw_parts(args, nargs + nkw) };
        let cls = if (*f).is_type(cmethod_type()) { (*(f as *mut PyCMethodObject)).mm_class } else { std::ptr::null_mut() };
        let fo = &*f;
        // Static methods of builtin types keep their type as __self__ (for
        // __qualname__ and pickling) but are called without a self.
        let slf = if (*(*ff).m_ml).ml_flags & METH_STATIC != 0 && !(*ff).m_self.is_null() && is_type(&*(*ff).m_self) { std::ptr::null_mut() } else { (*ff).m_self };
        if nkw != 0 && (*(*ff).m_ml).ml_flags & METH_KEYWORDS == 0 {
            return into_c(Err(crate::err!(TypeError, "{}() takes no keyword arguments", cfunction_func_str(fo))));
        }
        into_c(crate::methods::call_methoddef((*ff).m_ml, slf, cls, all, kwnames, &|| cfunction_func_str(fo)))
    }
}

/// The function's name in call errors: `module.qualname` when its `__module__` is a str
/// other than "builtins" (also with a meaningful self: cffi's `lib` functions read
/// `_mod.Lib.foo0() takes no arguments`), else the qualified name.
fn cfunction_func_str(fo: &PyObject) -> String {
    let n = cfunction_qualname(fo);
    unsafe {
        let m = (*fptr(fo)).m_module;
        if !m.is_null() && crate::types::str::is_str(&*m) {
            let m = crate::types::str::to_string_lossy(&*m);
            if m != "builtins" {
                return format!("{}.{}", m, n);
            }
        }
    }
    n
}

unsafe extern "C" fn cfunction_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        let f = fptr(&*op);
        xdecref((*f).m_self);
        xdecref((*f).m_module);
        if (*op).is_type(cmethod_type()) {
            xdecref((*(op as *mut PyCMethodObject)).mm_class as *mut PyObject);
        }
        crate::types::weakref::clear_weakrefs_at(op, &raw mut (*f).m_weakreflist);
        crate::alloc::free_object(op);
    }
}

unsafe extern "C" fn cfunction_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let f = fptr(&*op);
        let v = visit.unwrap();
        for p in [(*f).m_self, (*f).m_module] {
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

unsafe extern "C" fn cfunction_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        let s = match self_is_meaningful(o) {
            None => format!("<built-in function {}>", cfunction_name(o)),
            Some(slf) => format!(
                "<built-in method {} of {} object at {:p}>",
                cfunction_name(o),
                tp_name_of(slf),
                slf.as_ptr()
            ),
        };
        into_c(Ok(pystr::new_str(&s)))
    }
}

unsafe extern "C" fn cfunction_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        if (op != Py_EQ && op != Py_NE) || !is_cfunction(&*b) {
            return none::not_implemented().to_ref().into_raw();
        }
        let x = fptr(&*a);
        let y = fptr(&*b);
        let eq = (*x).m_self == (*y).m_self && (*(*x).m_ml).ml_meth == (*(*y).m_ml).ml_meth;
        crate::types::bool_::new_bool(if op == Py_EQ { eq } else { !eq }).into_raw()
    }
}

unsafe extern "C" fn cfunction_hash(op: *mut PyObject) -> Py_hash_t {
    unsafe {
        let f = fptr(&*op);
        let a = crate::hash::hash_pointer((*f).m_self as usize);
        let b = crate::hash::hash_pointer((*(*f).m_ml).ml_meth as usize);
        let h = a ^ b;
        if h == -1 { -2 } else { h }
    }
}

fn get_name(o: &PyObject) -> PyResult {
    Ok(pystr::new_str(&cfunction_name(o)))
}
fn get_qualname(o: &PyObject) -> PyResult {
    Ok(pystr::new_str(&cfunction_qualname(o)))
}
/// A builtin type's static method keeps its type in m_self (for __qualname__), but has no
/// __self__ and takes __module__ from the type.
fn static_of_type(o: &PyObject) -> Option<&PyObject> {
    unsafe {
        let f = fptr(o);
        let s = (*f).m_self;
        ((*(*f).m_ml).ml_flags & METH_STATIC != 0 && !s.is_null() && is_type(&*s)).then(|| &*s)
    }
}
fn get_self(o: &PyObject) -> PyResult {
    if static_of_type(o).is_some() {
        return Ok(none::none_ref());
    }
    unsafe {
        let s = (*fptr(o)).m_self;
        Ok(if s.is_null() { none::none_ref() } else { PyRef::from_borrowed(s) })
    }
}
fn get_module(o: &PyObject) -> PyResult {
    if let Some(t) = static_of_type(o) {
        if unsafe { (*fptr(o)).m_module.is_null() } {
            // static types: the module is the dotted prefix of tp_name (never fails, also
            // while the builtin types are still being set up)
            let tp = as_type(t);
            if unsafe { (*tp).tp_flags & Py_TPFLAGS_HEAPTYPE == 0 } {
                let n = unsafe { CStr::from_ptr((*tp).tp_name).to_string_lossy() };
                return Ok(pystr::new_str(n.rfind('.').map_or("builtins", |i| &n[..i])));
            }
            if let Ok(Some(m)) = crate::abstract_::getattr_opt(t, crate::name!("__module__")) {
                return Ok(m);
            }
            crate::errors::clear_exc();
        }
    }
    unsafe {
        let s = (*fptr(o)).m_module;
        Ok(if s.is_null() { none::none_ref() } else { PyRef::from_borrowed(s) })
    }
}
fn set_module(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    unsafe {
        let f = fptr(o);
        let old = (*f).m_module;
        (*f).m_module = v.map_or(std::ptr::null_mut(), |v| v.to_ref().into_raw());
        xdecref(old);
    }
    Ok(())
}
fn raw_doc(o: &PyObject) -> Option<String> {
    unsafe {
        let d = (*(*fptr(o)).m_ml).ml_doc;
        if d.is_null() { None } else { Some(CStr::from_ptr(d).to_string_lossy().into_owned()) }
    }
}
fn get_doc(o: &PyObject) -> PyResult {
    // an empty docstring reads as no docstring
    match raw_doc(o).filter(|d| !d.is_empty()).and_then(|d| crate::types::descr::doc_without_signature(&d)) {
        Some(s) => Ok(pystr::new_str(&s)),
        None => Ok(none::none_ref()),
    }
}
fn get_textsig(o: &PyObject) -> PyResult {
    let (flags, slf) = unsafe { ((*(*fptr(o)).m_ml).ml_flags, (*fptr(o)).m_self) };
    let first = if slf.is_null() || flags & METH_STATIC != 0 {
        None
    } else if crate::types::module::is_module(unsafe { &*slf }) {
        Some("$module")
    } else if flags & METH_CLASS != 0 {
        Some("$type")
    } else {
        Some("$self")
    };
    let doc = raw_doc(o);
    match crate::types::descr::method_text_signature(&cfunction_name(o), doc.as_deref(), flags, first) {
        Some(s) => Ok(pystr::new_str(&s)),
        None => Ok(none::none_ref()),
    }
}
fn reduce(o: &PyObject) -> PyResult {
    match self_is_meaningful(o) {
        None => Ok(pystr::new_str(&cfunction_name(o))),
        Some(s) => {
            let getattr = crate::builtins::builtin("getattr")?;
            Ok(tuple::new_tuple(vec![getattr, tuple::new_tuple(vec![s.to_ref(), pystr::new_str(&cfunction_name(o))])]))
        }
    }
}

pub fn init_types() {
    unsafe {
        let t = &mut *cfunction_type();
        t.tp_name = c"builtin_function_or_method".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyCFunctionObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_HAVE_VECTORCALL;
        t.tp_dealloc = Some(cfunction_dealloc);
        t.tp_traverse = Some(cfunction_traverse);
        t.tp_repr = Some(cfunction_repr);
        t.tp_richcompare = Some(cfunction_richcompare);
        t.tp_hash = Some(cfunction_hash);
        t.tp_call = Some(crate::abstract_::vectorcall_call);
        t.tp_vectorcall_offset = std::mem::offset_of!(PyCFunctionObject, vectorcall) as isize;
        t.tp_weaklistoffset = std::mem::offset_of!(PyCFunctionObject, m_weakreflist) as isize;
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_getset = crate::getset_table![
            ("__name__", get_name),
            ("__qualname__", get_qualname),
            ("__self__", get_self),
            ("__doc__", get_doc),
            ("__text_signature__", get_textsig),
            ("__module__", get_module, set_module),
        ];
        t.tp_methods = crate::mdefs![("__reduce__", NOARGS, reduce)];

        let m = &mut *cmethod_type();
        m.tp_name = c"builtin_method".as_ptr();
        m.tp_base = cfunction_type();
        m.tp_basicsize = std::mem::size_of::<PyCMethodObject>() as isize;
        m.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_HAVE_VECTORCALL;
        m.tp_dealloc = Some(cfunction_dealloc);
        m.tp_traverse = Some(cfunction_traverse);
        m.tp_call = Some(crate::abstract_::vectorcall_call);
        m.tp_vectorcall_offset = std::mem::offset_of!(PyCFunctionObject, vectorcall) as isize;
    }
}
