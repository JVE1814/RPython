//! The `sentinel` builtin type (PEP 661, new in 3.15; `PySentinel_Type`).
//!
//! `sentinel(name, /, *, repr=None)` creates a unique marker object whose repr is
//! `repr` (or `name`). `__module__` is taken from the caller's globals; pickling
//! reduces to the qualified `__name__` so module-level/class-level sentinels
//! round-trip by identity. Sentinels participate in `X | Y` unions.

use crate::errors::*;
use crate::ffi::*;
use crate::modules::fncommon::{self as fc, opt, targs};
use crate::object::*;
use crate::types::{dict, none, str as pystr};
use crate::{mdefs, raise};
use std::ffi::{c_char, c_int, c_void, CStr};

#[repr(C)]
pub struct Sentinel {
    ob_base: PyObject,
    name: *mut PyObject,
    module: *mut PyObject,
    repr: *mut PyObject,
}

#[unsafe(no_mangle)]
pub static mut PySentinel_Type: PyTypeObject = PyTypeObject::ZERO;

pub fn sentinel_type() -> *mut PyTypeObject {
    &raw mut PySentinel_Type
}

pub fn is_sentinel(o: &PyObject) -> bool {
    o.is_type(sentinel_type())
}

fn sobj(o: &PyObject) -> &mut Sentinel {
    unsafe { &mut *(o.as_ptr() as *mut Sentinel) }
}

/// Create a sentinel (`PySentinel_New` semantics; `module`/`repr` optional).
pub fn new_sentinel(name: &PyObject, module: Option<&PyObject>, repr: Option<&PyObject>) -> PyResult {
    unsafe {
        let op = fc::alloc(sentinel_type())?;
        let s = &mut *(op as *mut Sentinel);
        s.name = name.to_ref().into_raw();
        s.module = module.map_or(std::ptr::null_mut(), |m| m.to_ref().into_raw());
        s.repr = repr.map_or(std::ptr::null_mut(), |r| r.to_ref().into_raw());
        Ok(PyRef::from_raw(op))
    }
}

/// `__name__` of the calling frame's module (None when there is no Python frame).
fn caller_module() -> PyRef {
    match crate::interp::current_globals() {
        Some(g) => match dict::get_item_str(&g, crate::name!("__name__")) {
            Some(n) => n,
            None => none::none_ref(),
        },
        None => none::none_ref(),
    }
}

unsafe extern "C" fn sentinel_new(_tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let a = targs(args);
            let mut repr_owned: Option<PyRef> = None;
            if let Some(k) = opt(kw) {
                for key in dict::keys_vec(k) {
                    if pystr::is_str(&key) && pystr::to_string_lossy(&key) == "repr" {
                        repr_owned = dict::get_item_str(k, crate::name!("repr"));
                    } else {
                        raise!(TypeError, "sentinel() got an unexpected keyword argument '{}'", pystr::to_string_lossy(&key));
                    }
                }
            }
            if a.is_empty() {
                raise!(TypeError, "sentinel() missing required argument 'name' (pos 1)");
            }
            if a.len() > 1 {
                raise!(TypeError, "sentinel() takes exactly 1 positional argument ({} given)", a.len());
            }
            let name = a[0];
            if !pystr::is_str(name) {
                raise!(TypeError, "sentinel() argument 1 must be str, not {}", fc::tname(name));
            }
            let repr = match repr_owned.as_deref() {
                Some(r) if r.is(none::none()) => None,
                Some(r) if pystr::is_str(r) => Some(r),
                Some(r) => raise!(TypeError, "sentinel() argument 'repr' must be str or None, not {}", fc::tname(r)),
                None => None,
            };
            let module = caller_module();
            new_sentinel(name, Some(&module), repr)
        })())
    }
}

unsafe extern "C" fn sentinel_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let s = sobj(&*o);
        let p = if s.repr.is_null() { s.name } else { s.repr };
        (*p).to_ref().into_raw()
    }
}

fn get_name(o: &PyObject) -> PyResult {
    Ok(unsafe { (*sobj(o).name).to_ref() })
}

fn get_module(o: &PyObject) -> PyResult {
    let s = sobj(o);
    if s.module.is_null() {
        raise!(AttributeError, "'sentinel' object has no attribute '__module__'");
    }
    Ok(unsafe { (*s.module).to_ref() })
}

fn set_module(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let s = sobj(o);
    if v.is_none() && s.module.is_null() {
        raise!(AttributeError, "'sentinel' object has no attribute '__module__'");
    }
    let old = std::mem::replace(&mut s.module, v.map_or(std::ptr::null_mut(), |x| x.to_ref().into_raw()));
    unsafe { xdecref(old) };
    Ok(())
}

fn sentinel_reduce(o: &PyObject) -> PyResult {
    get_name(o)
}

fn sentinel_copy(o: &PyObject) -> PyResult {
    Ok(o.to_ref())
}

fn sentinel_deepcopy(o: &PyObject, _memo: &PyObject) -> PyResult {
    Ok(o.to_ref())
}

unsafe extern "C" fn sentinel_or(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(crate::types::union::union_or(&*a, &*b)) }
}

unsafe extern "C" fn sentinel_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let s = &mut *(o as *mut Sentinel);
        fc::clear_field(&raw mut s.name);
        fc::clear_field(&raw mut s.module);
        fc::clear_field(&raw mut s.repr);
        fc::free(o);
    }
}

unsafe extern "C" fn sentinel_traverse(o: *mut PyObject, v: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let s = &*(o as *mut Sentinel);
        for p in [s.name, s.module, s.repr] {
            let r = fc::visit(p, v, arg);
            if r != 0 {
                return r;
            }
        }
        0
    }
}

unsafe extern "C" fn sentinel_clear(o: *mut PyObject) -> c_int {
    unsafe {
        let s = &mut *(o as *mut Sentinel);
        fc::clear_field(&raw mut s.module);
        fc::clear_field(&raw mut s.repr);
    }
    0
}

static mut SENTINEL_NUM: PyNumberMethods = PyNumberMethods::ZERO;

pub fn init_types() {
    unsafe {
        let t = sentinel_type();
        fc::init_type(t, c"sentinel", std::mem::size_of::<Sentinel>(), Py_TPFLAGS_HAVE_GC);
        (*t).tp_new = Some(sentinel_new);
        (*t).tp_repr = Some(sentinel_repr);
        (*t).tp_dealloc = Some(sentinel_dealloc);
        (*t).tp_traverse = Some(sentinel_traverse);
        (*t).tp_clear = Some(sentinel_clear);
        (*t).tp_doc = c"sentinel(name, /, *, repr=None)\n--\n\nCreate a unique sentinel object with the given name.".as_ptr();
        let n = &mut *(&raw mut SENTINEL_NUM);
        n.nb_or = Some(sentinel_or);
        (*t).tp_as_number = n;
        (*t).tp_methods = mdefs![
            ("__reduce__", NOARGS, sentinel_reduce),
            ("__copy__", NOARGS, sentinel_copy),
            ("__deepcopy__", O, sentinel_deepcopy),
        ];
        (*t).tp_getset = crate::getset_table![("__name__", get_name), ("__module__", get_module, set_module),];
    }
}

pub fn ready() -> PyResult<()> {
    crate::types::typeobject::type_ready(sentinel_type())
}

/// `PySentinel_New(name, module_name, repr)` (C API).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySentinel_New(name: *const c_char, module_name: *const c_char, repr: *const c_char) -> *mut PyObject {
    unsafe {
        let s = |p: *const c_char| -> Option<PyRef> {
            if p.is_null() { None } else { Some(pystr::new_str(&CStr::from_ptr(p).to_string_lossy())) }
        };
        if name.is_null() {
            return into_c(Err(err!(SystemError, "PySentinel_New: name must not be NULL")));
        }
        let n = s(name).unwrap();
        // (capi3) without a module name, __module__ is None
        let m = Some(s(module_name).unwrap_or_else(crate::types::none::none_ref));
        let r = s(repr);
        into_c(new_sentinel(&n, m.as_deref(), r.as_deref()))
    }
}
