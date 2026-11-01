//! sys module attributes and functions (core set; extended by later work).

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::*;
use crate::types::{bool_, dict, int, list, none, str as pystr, tuple};
use crate::raise;

pub const VERSION: &str = "3.15.2";
pub const HEXVERSION: i64 = 0x030F02F0;

pub fn source_line(filename: &str, lineno: i32) -> Option<String> {
    if lineno < 1 {
        return None;
    }
    let data = std::fs::read(filename).ok()?;
    let s = String::from_utf8_lossy(&data);
    s.split('\n').nth((lineno - 1) as usize).map(|l| l.to_string())
}

fn sys_exit(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("exit", args, 0, 1)?;
    let code = args.first().map(|a| a.to_ref());
    match code {
        Some(c) => Err(set_err_obj(crate::types::exceptions::SystemExit(), c)),
        None => Err(set_err_noargs(crate::types::exceptions::SystemExit())),
    }
}

fn sys_getframe(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("_getframe", args, 0, 1)?;
    let mut depth = match args.first() {
        Some(a) => ab::as_i64(a)?,
        None => 0,
    };
    let mut f = crate::interp::current_frame_ptr();
    while depth > 0 && !f.is_null() {
        f = unsafe { (*f).prev };
        depth -= 1;
    }
    if f.is_null() {
        raise!(ValueError, "call stack is not deep enough");
    }
    let fo = crate::interp::eval::frame_object(unsafe { &mut *f });
    Ok(unsafe { PyRef::from_borrowed(fo) })
}

fn sys_exception(_m: &PyObject) -> PyResult {
    let ts = crate::threads::current();
    let e = crate::interp::eval::handled_exception(ts);
    Ok(if e.is_null() { none::none_ref() } else { unsafe { PyRef::from_borrowed(e) } })
}

fn sys_exc_info(_m: &PyObject) -> PyResult {
    let ts = crate::threads::current();
    let e = crate::interp::eval::handled_exception(ts);
    if e.is_null() {
        return Ok(tuple::new_tuple(vec![none::none_ref(), none::none_ref(), none::none_ref()]));
    }
    let e = unsafe { PyRef::from_borrowed(e) };
    let t = crate::types::typeobject::type_ref(e.type_ptr());
    let tb = crate::types::exceptions::exc_traceback(&e).unwrap_or_else(none::none_ref);
    Ok(tuple::new_tuple(vec![t, e, tb]))
}

fn sys_getrecursionlimit(_m: &PyObject) -> PyResult {
    Ok(int::new_int(crate::threads::current().c.py_recursion_limit as i64))
}

fn sys_setrecursionlimit(_m: &PyObject, v: &PyObject) -> PyResult {
    if !int::is_int(v) {
        raise!(TypeError, "'{}' object cannot be interpreted as an integer", crate::types::typeobject::tname(v));
    }
    let n = ab::as_i64(v)?;
    if n < 1 {
        raise!(ValueError, "recursion limit must be greater or equal than 1");
    }
    let ts = crate::threads::current();
    let depth = ts.recursion_depth.get() as i64;
    if n <= depth {
        raise!(
            RecursionError,
            "cannot set the recursion limit to {} at the recursion depth {}: the limit is too low",
            n,
            depth
        );
    }
    crate::threads::set_recursion_limit_all(n.min(i32::MAX as i64) as i32);
    Ok(none::none_ref())
}

fn sys_intern(_m: &PyObject, s: &PyObject) -> PyResult {
    if !pystr::is_str_exact(s) {
        raise!(TypeError, "intern() argument must be str, not {}", crate::types::typeobject::tname(s));
    }
    Ok(pystr::intern(s.to_ref()))
}

fn sys_getrefcount(_m: &PyObject, o: &PyObject) -> PyResult {
    Ok(int::new_int(o.refcnt() as i64))
}

fn sys_displayhook(_m: &PyObject, o: &PyObject) -> PyResult {
    if o.is(none::none()) {
        return Ok(none::none_ref());
    }
    let b = crate::builtins::builtins_module();
    ab::setattr(&b, crate::name!("_"), none::none())?;
    let r = ab::repr(o)?;
    let out = crate::modules::sys::get_sys_attr("stdout").ok_or_else(|| err!(RuntimeError, "lost sys.stdout"))?;
    ab::call_method_str(&out, "write", &[&r])?;
    let nl = pystr::new_str("\n");
    ab::call_method_str(&out, "write", &[&nl])?;
    ab::setattr(&b, crate::name!("_"), o)?;
    Ok(none::none_ref())
}

fn sys_excepthook(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("excepthook", args, 3, 3)?;
    let v = args[1];
    if crate::types::exceptions::is_exception_instance(v) {
        crate::modules::traceback_print::print_exception(v);
    } else {
        crate::modules::sys::write_stderr(&format!(
            "TypeError: print_exception(): Exception expected for value, {} found\n",
            crate::types::typeobject::tname(v)
        ));
    }
    Ok(none::none_ref())
}

fn sys_getdefaultencoding(_m: &PyObject) -> PyResult {
    Ok(pystr::new_str("utf-8"))
}
fn sys_getfilesystemencoding(_m: &PyObject) -> PyResult {
    Ok(pystr::new_str("utf-8"))
}
fn sys_getfilesystemencodeerrors(_m: &PyObject) -> PyResult {
    Ok(pystr::new_str("surrogateescape"))
}
fn sys_is_gil_enabled(_m: &PyObject) -> PyResult {
    Ok(bool_::new_bool(false))
}
fn sys_settrace(_m: &PyObject, f: &PyObject) -> PyResult {
    crate::modules::sys_trace_api::settrace(f)
}
fn sys_gettrace(_m: &PyObject) -> PyResult {
    crate::modules::sys_trace_api::gettrace()
}
fn sys_setprofile(_m: &PyObject, f: &PyObject) -> PyResult {
    crate::modules::sys_trace_api::setprofile(f)
}
fn sys_getprofile(_m: &PyObject) -> PyResult {
    crate::modules::sys_trace_api::getprofile()
}
fn sys_getsizeof(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("getsizeof", &["object", "default"], 0, 1);
    let [o, default] = crate::args::parse::<2>(&P, args, kw)?;
    let o = o.unwrap();
    match ab::call_special_opt(o, crate::name!("__sizeof__"), &[]) {
        Ok(Some(r)) => {
            let n = ab::as_i64(&r)?;
            if n < 0 {
                raise!(ValueError, "__sizeof__() should return >= 0");
            }
            let extra = if crate::alloc::type_is_gc(o.type_ptr()) { crate::alloc::GC_HEAD as i64 } else { 0 };
            Ok(int::new_int(n + extra))
        }
        Ok(None) => match default {
            Some(d) => Ok(d.to_ref()),
            None => raise!(TypeError, "Type {} doesn't define __sizeof__", crate::types::typeobject::tname(o)),
        },
        Err(e) => {
            if let Some(d) = default {
                if exc_matches(crate::types::exceptions::TypeError()) {
                    clear_exc();
                    return Ok(d.to_ref());
                }
            }
            Err(e)
        }
    }
}

fn exe_path() -> String {
    std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Installation prefix: <prefix>/bin/python3.15t -> <prefix>
pub fn prefix() -> String {
    if let Ok(p) = std::env::var("RPYTHON_PREFIX") {
        return p;
    }
    let exe = std::path::PathBuf::from(exe_path());
    exe.parent().and_then(|b| b.parent()).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "/usr/local".into())
}

pub fn populate(d: &PyObject) -> PyResult<()> {
    let s = |v: &str| pystr::new_str(v);
    let set = |k: &str, v: &PyObject| dict::set_item_str(d, k, v);
    set("version", &s(&format!("{} free-threading build (rpython) [rustc]", VERSION)))?;
    set("hexversion", &int::new_int(HEXVERSION))?;
    set(
        "version_info",
        &tuple::new_tuple(vec![int::new_int(3), int::new_int(15), int::new_int(2), s("final"), int::new_int(0)]),
    )?;
    set("api_version", &int::new_int(1013))?;
    set("platform", &s("linux"))?;
    set("byteorder", &s("little"))?;
    set("maxsize", &int::new_int(i64::MAX))?;
    set("maxunicode", &int::new_int(0x10ffff))?;
    set("abiflags", &s("t"))?;
    set("executable", &s(&exe_path()))?;
    set("_base_executable", &s(&exe_path()))?;
    let p = prefix();
    for k in ["prefix", "base_prefix", "exec_prefix", "base_exec_prefix"] {
        set(k, &s(&p))?;
    }
    set("platlibdir", &s("lib"))?;
    set("pycache_prefix", none::none())?;
    set("dont_write_bytecode", &bool_::new_bool(true))?;
    set("warnoptions", &list::new_list_empty())?;
    set("_xoptions", &dict::new_dict())?;
    set("copyright", &s("Copyright (c) 2001 Python Software Foundation.\nAll Rights Reserved."))?;
    set("float_repr_style", &s("short"))?;
    set("builtin_module_names", &tuple::new_tuple(crate::import::builtin_module_names().iter().map(|n| s(n)).collect()))?;
    let m = crate::types::module::new_module("sys.implementation")?;
    crate::types::module::add_object(&m, "name", &s("cpython"))?;
    crate::types::module::add_object(&m, "cache_tag", none::none())?;
    crate::types::module::add_object(&m, "_multiarch", &s("x86_64-linux-gnu"))?;
    crate::types::module::add_object(
        &m,
        "version",
        &tuple::new_tuple(vec![int::new_int(3), int::new_int(15), int::new_int(2), s("final"), int::new_int(0)]),
    )?;
    crate::types::module::add_object(&m, "hexversion", &int::new_int(HEXVERSION))?;
    crate::types::module::add_object(&m, "supports_isolated_interpreters", &bool_::new_bool(false))?;
    set("implementation", &m)?;
    let methods = crate::mdefs![
        ("exit", FAST, sys_exit),
        ("_getframe", FAST, sys_getframe),
        ("exception", NOARGS, sys_exception),
        ("exc_info", NOARGS, sys_exc_info),
        ("getrecursionlimit", NOARGS, sys_getrecursionlimit),
        ("setrecursionlimit", O, sys_setrecursionlimit),
        ("intern", O, sys_intern),
        ("getrefcount", O, sys_getrefcount),
        ("displayhook", O, sys_displayhook),
        ("excepthook", FAST, sys_excepthook),
        ("getdefaultencoding", NOARGS, sys_getdefaultencoding),
        ("getfilesystemencoding", NOARGS, sys_getfilesystemencoding),
        ("getfilesystemencodeerrors", NOARGS, sys_getfilesystemencodeerrors),
        ("_is_gil_enabled", NOARGS, sys_is_gil_enabled),
        ("settrace", O, sys_settrace),
        ("gettrace", NOARGS, sys_gettrace),
        ("setprofile", O, sys_setprofile),
        ("getprofile", NOARGS, sys_getprofile),
        ("getsizeof", FASTKW, sys_getsizeof),
    ];
    crate::builtins::add_methods_to_dict(d, methods, None)?;
    // keep originals
    if let Some(h) = dict::get_item_rs(d, "displayhook") {
        set("__displayhook__", &h)?;
    }
    if let Some(h) = dict::get_item_rs(d, "excepthook") {
        set("__excepthook__", &h)?;
    }
    Ok(())
}
