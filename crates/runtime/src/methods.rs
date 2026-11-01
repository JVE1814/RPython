//! Building `PyMethodDef` tables for Rust-implemented builtins, and the
//! `PyMethodDef` calling convention (shared with C extensions).
//!
//! Rust builtins have one of these shapes:
//!   NOARGS   : fn(&PyObject) -> PyResult
//!   O        : fn(&PyObject, &PyObject) -> PyResult
//!   FAST     : fn(&PyObject, &[&PyObject]) -> PyResult
//!   FASTKW   : fn(&PyObject, &[&PyObject], Option<&PyObject> /*kwnames tuple*/) -> PyResult
//! and are turned into `extern "C"` shims by monomorphising over the (zero-sized)
//! function item type.

use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use std::ffi::{CStr, c_char, c_void};

#[inline(always)]
fn conjure<F>() -> F {
    assert!(std::mem::size_of::<F>() == 0, "builtin must be a plain fn item");
    unsafe { std::mem::MaybeUninit::<F>::uninit().assume_init() }
}

unsafe extern "C" fn shim_noargs<F: Fn(&PyObject) -> PyResult>(s: *mut PyObject, _a: *mut PyObject) -> *mut PyObject {
    let f: F = conjure();
    unsafe { into_c(f(selfref(s))) }
}
unsafe extern "C" fn shim_o<F: Fn(&PyObject, &PyObject) -> PyResult>(s: *mut PyObject, a: *mut PyObject) -> *mut PyObject {
    let f: F = conjure();
    unsafe { into_c(f(selfref(s), &*a)) }
}
unsafe extern "C" fn shim_fast<F: Fn(&PyObject, &[&PyObject]) -> PyResult>(
    s: *mut PyObject,
    a: *const *mut PyObject,
    n: Py_ssize_t,
) -> *mut PyObject {
    let f: F = conjure();
    unsafe {
        let args: &[&PyObject] = if n == 0 { &[] } else { std::slice::from_raw_parts(a as *const &PyObject, n as usize) };
        into_c(f(selfref(s), args))
    }
}
unsafe extern "C" fn shim_fastkw<F: Fn(&PyObject, &[&PyObject], Option<&PyObject>) -> PyResult>(
    s: *mut PyObject,
    a: *const *mut PyObject,
    n: Py_ssize_t,
    kw: *mut PyObject,
) -> *mut PyObject {
    let f: F = conjure();
    unsafe {
        let nkw = if kw.is_null() { 0 } else { crate::types::tuple::tuple_len(&*kw) };
        let total = n as usize + nkw;
        let args: &[&PyObject] = if total == 0 { &[] } else { std::slice::from_raw_parts(a as *const &PyObject, total) };
        let kw = if kw.is_null() { None } else { Some(&*kw) };
        into_c(f(selfref(s), args, kw))
    }
}

pub fn noargs<F: Fn(&PyObject) -> PyResult + Copy>(_f: F) -> *const c_void {
    shim_noargs::<F> as *const c_void
}
pub fn o<F: Fn(&PyObject, &PyObject) -> PyResult + Copy>(_f: F) -> *const c_void {
    shim_o::<F> as *const c_void
}
pub fn fast<F: Fn(&PyObject, &[&PyObject]) -> PyResult + Copy>(_f: F) -> *const c_void {
    shim_fast::<F> as *const c_void
}
pub fn fastkw<F: Fn(&PyObject, &[&PyObject], Option<&PyObject>) -> PyResult + Copy>(_f: F) -> *const c_void {
    shim_fastkw::<F> as *const c_void
}

fn leak_cstr(s: &str) -> *const c_char {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    Box::leak(v.into_boxed_slice()).as_ptr() as *const c_char
}

pub fn cstr_static(s: &str) -> *const c_char {
    leak_cstr(s)
}

/// One method table entry. `kind` is one of the METH_* combinations.
pub fn def(name: &str, meth: *const c_void, flags: i32, doc: &str) -> PyMethodDef {
    PyMethodDef {
        ml_name: leak_cstr(name),
        ml_meth: meth,
        ml_flags: flags,
        ml_doc: if doc.is_empty() { std::ptr::null() } else { leak_cstr(doc) },
    }
}

/// Finalise a table (adds the NULL sentinel, leaks it).
pub fn table(mut v: Vec<PyMethodDef>) -> *mut PyMethodDef {
    v.push(PyMethodDef { ml_name: std::ptr::null(), ml_meth: std::ptr::null(), ml_flags: 0, ml_doc: std::ptr::null() });
    Box::leak(v.into_boxed_slice()).as_mut_ptr()
}

/// `mdefs![ ("name", O, func, "doc"), ... ]`
#[macro_export]
macro_rules! mdefs {
    ($( ($name:expr, $kind:ident, $f:expr $(, $doc:expr)? $(; $extra:expr)?) ),* $(,)?) => {{
        let mut v: Vec<$crate::ffi::PyMethodDef> = Vec::new();
        $(
            v.push($crate::mdefs!(@one $name, $kind, $f, ($($doc)?), ($($extra)?)));
        )*
        $crate::methods::table(v)
    }};
    (@one $name:expr, NOARGS, $f:expr, ($($doc:expr)?), ($($extra:expr)?)) => {
        $crate::methods::def($name, $crate::methods::noargs($f), $crate::ffi::METH_NOARGS $(| $extra)?, $crate::mdefs!(@doc $($doc)?))
    };
    (@one $name:expr, O, $f:expr, ($($doc:expr)?), ($($extra:expr)?)) => {
        $crate::methods::def($name, $crate::methods::o($f), $crate::ffi::METH_O $(| $extra)?, $crate::mdefs!(@doc $($doc)?))
    };
    (@one $name:expr, FAST, $f:expr, ($($doc:expr)?), ($($extra:expr)?)) => {
        $crate::methods::def($name, $crate::methods::fast($f), $crate::ffi::METH_FASTCALL $(| $extra)?, $crate::mdefs!(@doc $($doc)?))
    };
    (@one $name:expr, FASTKW, $f:expr, ($($doc:expr)?), ($($extra:expr)?)) => {
        $crate::methods::def($name, $crate::methods::fastkw($f), $crate::ffi::METH_FASTCALL | $crate::ffi::METH_KEYWORDS $(| $extra)?, $crate::mdefs!(@doc $($doc)?))
    };
    (@doc) => { "" };
    (@doc $d:expr) => { $d };
}

pub fn method_name(ml: *const PyMethodDef) -> String {
    unsafe { CStr::from_ptr((*ml).ml_name).to_string_lossy().into_owned() }
}

/// Call a PyMethodDef with vectorcall-style arguments.
/// `kwnames` is a tuple of keyword names (values follow positionals in `args`).
pub unsafe fn call_methoddef(
    ml: *mut PyMethodDef,
    slf: *mut PyObject,
    defining_class: *mut PyTypeObject,
    args: &[*mut PyObject],
    kwnames: *mut PyObject,
    qualname: &dyn Fn() -> String,
) -> PyResult {
    unsafe {
        let flags = (*ml).ml_flags & !(METH_CLASS | METH_STATIC | METH_COEXIST);
        let nkw = if kwnames.is_null() { 0 } else { crate::types::tuple::tuple_len(&*kwnames) };
        let npos = args.len() - nkw;
        let meth = (*ml).ml_meth;
        let r = match flags {
            METH_NOARGS => {
                if nkw != 0 {
                    raise!(TypeError, "{}() takes no keyword arguments", qualname());
                }
                if npos != 0 {
                    raise!(TypeError, "{}() takes no arguments ({} given)", qualname(), npos);
                }
                let f: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject = std::mem::transmute(meth);
                f(slf, std::ptr::null_mut())
            }
            METH_O => {
                if nkw != 0 {
                    raise!(TypeError, "{}() takes no keyword arguments", qualname());
                }
                if npos != 1 {
                    raise!(TypeError, "{}() takes exactly one argument ({} given)", qualname(), npos);
                }
                let f: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject = std::mem::transmute(meth);
                f(slf, args[0])
            }
            METH_FASTCALL => {
                if nkw != 0 {
                    raise!(TypeError, "{}() takes no keyword arguments", qualname());
                }
                let f: unsafe extern "C" fn(*mut PyObject, *const *mut PyObject, Py_ssize_t) -> *mut PyObject =
                    std::mem::transmute(meth);
                f(slf, args.as_ptr(), npos as Py_ssize_t)
            }
            x if x == METH_FASTCALL | METH_KEYWORDS => {
                let f: unsafe extern "C" fn(*mut PyObject, *const *mut PyObject, Py_ssize_t, *mut PyObject) -> *mut PyObject =
                    std::mem::transmute(meth);
                f(slf, args.as_ptr(), npos as Py_ssize_t, if nkw == 0 { std::ptr::null_mut() } else { kwnames })
            }
            x if x == METH_METHOD | METH_FASTCALL | METH_KEYWORDS => {
                let f: unsafe extern "C" fn(
                    *mut PyObject,
                    *mut PyTypeObject,
                    *const *mut PyObject,
                    Py_ssize_t,
                    *mut PyObject,
                ) -> *mut PyObject = std::mem::transmute(meth);
                f(slf, defining_class, args.as_ptr(), npos as Py_ssize_t, if nkw == 0 { std::ptr::null_mut() } else { kwnames })
            }
            METH_VARARGS => {
                if nkw != 0 {
                    raise!(TypeError, "{}() takes no keyword arguments", qualname());
                }
                let t = crate::types::tuple::new_tuple_from_ptrs(args);
                let f: unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject = std::mem::transmute(meth);
                f(slf, t.as_ptr())
            }
            x if x == METH_VARARGS | METH_KEYWORDS => {
                let t = crate::types::tuple::new_tuple_from_ptrs(&args[..npos]);
                let kw = if nkw == 0 {
                    None
                } else {
                    let d = crate::types::dict::new_dict();
                    for i in 0..nkw {
                        crate::types::dict::set_item(&d, crate::types::tuple::tuple_get(&*kwnames, i), &*args[npos + i])?;
                    }
                    Some(d)
                };
                let f: unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject =
                    std::mem::transmute(meth);
                f(slf, t.as_ptr(), kw.as_ref().map_or(std::ptr::null_mut(), |d| d.as_ptr()))
            }
            _ => {
                raise!(SystemError, "{}() method: bad call flags", qualname());
            }
        };
        from_c(r)
    }
}

use crate::raise;

// ---------------------------------------------------------------- getset / members

unsafe extern "C" fn shim_get<F: Fn(&PyObject) -> PyResult>(s: *mut PyObject, _c: *mut c_void) -> *mut PyObject {
    let f: F = conjure();
    unsafe { into_c(f(&*s)) }
}
unsafe extern "C" fn shim_set<F: Fn(&PyObject, Option<&PyObject>) -> PyResult<()>>(
    s: *mut PyObject,
    v: *mut PyObject,
    _c: *mut c_void,
) -> std::ffi::c_int {
    let f: F = conjure();
    unsafe { int_into_c(f(&*s, if v.is_null() { None } else { Some(&*v) })) }
}
pub fn getter<F: Fn(&PyObject) -> PyResult + Copy>(_f: F) -> getter {
    Some(shim_get::<F>)
}
pub fn setter<F: Fn(&PyObject, Option<&PyObject>) -> PyResult<()> + Copy>(_f: F) -> setter {
    Some(shim_set::<F>)
}
pub fn gsdef(name: &str, get: getter, set: setter, doc: &str) -> PyGetSetDef {
    PyGetSetDef {
        name: leak_cstr(name),
        get,
        set,
        doc: if doc.is_empty() { std::ptr::null() } else { leak_cstr(doc) },
        closure: std::ptr::null_mut(),
    }
}
pub fn gstable(mut v: Vec<PyGetSetDef>) -> *mut PyGetSetDef {
    v.push(PyGetSetDef { name: std::ptr::null(), get: None, set: None, doc: std::ptr::null(), closure: std::ptr::null_mut() });
    Box::leak(v.into_boxed_slice()).as_mut_ptr()
}

/// `getset_table![("name", getfn), ("name2", getfn, setfn), ...]`
#[macro_export]
macro_rules! getset_table {
    ($( ($name:expr, $g:expr $(, $s:expr)? $(; $doc:expr)?) ),* $(,)?) => {{
        let mut v: Vec<$crate::ffi::PyGetSetDef> = Vec::new();
        $(
            v.push($crate::methods::gsdef($name, $crate::methods::getter($g),
                $crate::getset_table!(@set $($s)?), $crate::getset_table!(@doc $($doc)?)));
        )*
        $crate::methods::gstable(v)
    }};
    (@set) => { None };
    (@set $s:expr) => { $crate::methods::setter($s) };
    (@doc) => { "" };
    (@doc $d:expr) => { $d };
}

pub fn mbdef(name: &str, type_code: std::ffi::c_int, offset: usize, flags: std::ffi::c_int, doc: &str) -> PyMemberDef {
    PyMemberDef {
        name: leak_cstr(name),
        type_code,
        offset: offset as Py_ssize_t,
        flags,
        doc: if doc.is_empty() { std::ptr::null() } else { leak_cstr(doc) },
    }
}
pub fn mbtable(mut v: Vec<PyMemberDef>) -> *mut PyMemberDef {
    v.push(PyMemberDef { name: std::ptr::null(), type_code: 0, offset: 0, flags: 0, doc: std::ptr::null() });
    Box::leak(v.into_boxed_slice()).as_mut_ptr()
}

/// `member_table![("name", Py_T_OBJECT, offset, flags), ...]`
#[macro_export]
macro_rules! member_table {
    ($( ($name:expr, $t:expr, $off:expr, $flags:expr $(; $doc:expr)?) ),* $(,)?) => {{
        let mut v: Vec<$crate::ffi::PyMemberDef> = Vec::new();
        $( v.push($crate::methods::mbdef($name, $t, $off, $flags, $crate::member_table!(@doc $($doc)?))); )*
        $crate::methods::mbtable(v)
    }};
    (@doc) => { "" };
    (@doc $d:expr) => { $d };
}

unsafe extern "C" fn shim_binary<F: Fn(&PyObject, &PyObject) -> PyResult>(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    let f: F = conjure();
    unsafe { into_c(f(&*a, &*b)) }
}
/// Turn a Rust binary function into a C binaryfunc slot.
pub fn binary_shim<F: Fn(&PyObject, &PyObject) -> PyResult + Copy>(_f: F) -> unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject {
    shim_binary::<F>
}

/// Module-level functions created without a module have a NULL self; pass None instead.
#[inline(always)]
unsafe fn selfref<'a>(s: *mut PyObject) -> &'a PyObject {
    if s.is_null() { crate::types::none::none() } else { unsafe { &*s } }
}
