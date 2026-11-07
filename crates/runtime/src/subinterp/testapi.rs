//! The `_testinternalcapi` functions the subinterpreter tests use: creating,
//! running and destroying interpreters the way C code does, interpreter IDs and
//! reference counts, per-interpreter settings, and raw cross-interpreter data.

use super::module::{config_arg, id_arg, interpreter_error_type, lookup_arg};
use super::xidata::{self, Mode, Xi};
use super::Config;
use crate::abstract_ as ab;
use crate::args::{self, Params};
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::{bool_, dict, int, module as pymodule, none, str as pystr};
use crate::raise;
use std::sync::atomic::Ordering::*;

fn truth(o: Option<&PyObject>) -> PyResult<bool> {
    match o {
        Some(v) => ab::is_true(v),
        None => Ok(false),
    }
}

/// Run source code in the current interpreter's __main__ like
/// PyRun_SimpleString: an uncaught exception is printed; 0 or -1.
fn run_simple(src: &str) -> i32 {
    let r = (|| -> PyResult<()> {
        let main = super::main_module()?;
        let d = pymodule::module_dict(&main);
        let code = crate::compile::compile_str(src, "<string>", crate::compile::Mode::Exec, 0, -1)?;
        crate::interp::eval_code(&code.code, &d, Some(&d))?;
        Ok(())
    })();
    match r {
        Ok(()) => 0,
        Err(_) => {
            crate::print_exception_and_clear();
            -1
        }
    }
}

fn source_arg(o: &PyObject) -> PyResult<String> {
    if !pystr::is_str(o) {
        raise!(TypeError, "expected str, got {}", crate::types::typeobject::tname(o));
    }
    Ok(pystr::as_utf8(o)?.to_string())
}

/// config: None (legacy), a name, or a namespace (gil may be an int)
fn capi_config(o: Option<&PyObject>) -> PyResult<Config> {
    match o {
        None => Ok(Config::LEGACY),
        Some(c) if c.is(none::none()) => Ok(Config::LEGACY),
        Some(c) => config_arg(Some(c)),
    }
}

fn t_run_in_subinterp_with_config(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("run_in_subinterp_with_config", &["code", "config", "xi"], 0, 1).kwonly(1);
    let [code, config, xi] = args::parse::<3>(&P, args, kw)?;
    let src = source_arg(code.unwrap())?;
    let config = capi_config(config)?;
    let whence = if truth(xi)? { super::WHENCE_XI } else { super::WHENCE_CAPI };
    let (ts, prev) = super::create(config, whence)?;
    let rc = run_simple(&src);
    unsafe { super::end_interpreter(ts) };
    super::reattach(prev);
    Ok(int::new_int(rc as i64))
}

fn t_create_interpreter(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("create_interpreter", &["config", "whence"], 0, 0).kwonly(1);
    let [config, whence] = args::parse::<2>(&P, args, kw)?;
    let config = capi_config(config)?;
    let whence = match whence {
        Some(w) if !w.is(none::none()) => int::as_i64(w).unwrap_or(super::WHENCE_XI as i64) as i32,
        _ => super::WHENCE_XI,
    };
    let i = super::module::create_interp(config, whence)?;
    Ok(int::new_int(i.id))
}

fn t_destroy_interpreter(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("destroy_interpreter", &["id", "basic"], 0, 1).kwonly(1);
    let [id, _basic] = args::parse::<2>(&P, args, kw)?;
    let i = lookup_arg(id.unwrap(), false)?;
    super::destroy(i)?;
    Ok(none::none_ref())
}

fn t_exec_interpreter(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("exec_interpreter", &["id", "code", "main"], 0, 2).kwonly(1);
    let [id, code, main] = args::parse::<3>(&P, args, kw)?;
    let i = lookup_arg(id.unwrap(), false)?;
    let src = source_arg(code.unwrap())?;
    let as_main = truth(main)?;
    if as_main {
        super::claim_running(i)?;
    }
    let rc = super::with_interp(i, || run_simple(&src));
    if as_main {
        super::release_running(i);
    }
    Ok(int::new_int(rc as i64))
}

fn t_normalize_interp_id(_m: &PyObject, o: &PyObject) -> PyResult {
    Ok(int::new_int(id_arg(o)?))
}

fn t_unused_interpreter_id(_m: &PyObject) -> PyResult {
    // well above anything handed out so far
    Ok(int::new_int(super::next_id() + 1_000_000))
}

fn t_next_interpreter_id(_m: &PyObject) -> PyResult {
    Ok(int::new_int(super::next_id()))
}

fn t_interpreter_exists(_m: &PyObject, o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(super::lookup(id_arg(o)?).is_some()))
}

fn t_get_interpreter_refcount(_m: &PyObject, o: &PyObject) -> PyResult {
    let i = lookup_arg(o, false)?;
    Ok(int::new_int(i.refs.load(Acquire)))
}

fn t_link_interpreter_refcount(_m: &PyObject, o: &PyObject) -> PyResult {
    lookup_arg(o, false)?.requires_idref.store(true, Relaxed);
    Ok(none::none_ref())
}

fn t_unlink_interpreter_refcount(_m: &PyObject, o: &PyObject) -> PyResult {
    lookup_arg(o, false)?.requires_idref.store(false, Relaxed);
    Ok(none::none_ref())
}

fn t_interpreter_refcount_linked(_m: &PyObject, o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(lookup_arg(o, false)?.requires_idref.load(Relaxed)))
}

/// Feature bits of PyInterpreterState.feature_flags.
const FLAG_OBMALLOC: i64 = 1 << 5;
const FLAG_EXTENSIONS: i64 = 1 << 8;
const FLAG_THREADS: i64 = 1 << 10;
const FLAG_DAEMON_THREADS: i64 = 1 << 11;
const FLAG_FORK: i64 = 1 << 15;
const FLAG_EXEC: i64 = 1 << 16;

fn t_get_interp_settings(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    args::check_pos("get_interp_settings", args, 0, 1)?;
    let i = match args.first() {
        Some(o) if !o.is(none::none()) => lookup_arg(o, false)?,
        _ => super::current(),
    };
    let c = i.config;
    let mut flags = 0;
    for (on, bit) in [
        (c.use_main_obmalloc, FLAG_OBMALLOC),
        (c.check_multi_interp_extensions, FLAG_EXTENSIONS),
        (c.allow_threads, FLAG_THREADS),
        (c.allow_daemon_threads, FLAG_DAEMON_THREADS),
        (c.allow_fork, FLAG_FORK),
        (c.allow_exec, FLAG_EXEC),
    ] {
        if on {
            flags |= bit;
        }
    }
    let d = dict::new_dict();
    dict::set_item_str(&d, "feature_flags", &int::new_int(flags))?;
    dict::set_item_str(&d, "own_gil", &bool_::new_bool(c.gil == 2))?;
    Ok(d)
}

// ------------------------------------------------------------------ raw XI data

const XID_NAME: &std::ffi::CStr = c"_testinternalcapi.xidata";

unsafe extern "C" fn xid_destroy(cap: *mut PyObject) {
    unsafe {
        let p = crate::capi::capsule::PyCapsule_GetPointer(cap, XID_NAME.as_ptr());
        if !p.is_null() {
            drop(Box::from_raw(p as *mut Xi));
        } else {
            clear_exc();
        }
    }
}

fn get_for_mode(o: &PyObject, mode: &str) -> PyResult<Xi> {
    match mode {
        "xidata" => xidata::get(o, Mode::Default),
        "fallback" => xidata::get(o, Mode::Fallback),
        "pickle" => xidata::pickled(o),
        "marshal" => match crate::modules::marshal::dumps_vec(o, 4, true) {
            Ok(d) => Ok(Xi::Marshal(d)),
            Err(_) => Err(xidata::not_shareable("object could not be marshalled", true)),
        },
        "code" => {
            if !crate::types::code::is_code(o) {
                return Err(xidata::not_shareable(&format!("expected a code object, got {}", crate::types::typeobject::tname(o)), false));
            }
            Ok(Xi::Marshal(crate::modules::marshal::dumps_vec(o, 4, true)?))
        }
        "func" => {
            if !crate::types::funcobject::is_function(o) || !xidata::is_stateless_function(o)? {
                return Err(xidata::not_shareable("expected a stateless function", false));
            }
            xidata::get(o, Mode::Fallback)
        }
        "script" | "script-pure" => {
            let pure = mode == "script-pure";
            match xidata::script_data(o, pure) {
                Ok(d) => Ok(Xi::Marshal(d)),
                Err(_) => Err(xidata::not_shareable("object is not a script", true)),
            }
        }
        _ => raise!(ValueError, "unsupported mode {:?}", mode),
    }
}

fn t_get_crossinterp_data(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("get_crossinterp_data", &["obj", "mode"], 0, 1);
    let [obj, mode] = args::parse::<2>(&P, args, kw)?;
    let mode = match mode {
        Some(m) if !m.is(none::none()) => pystr::to_string_lossy(m),
        _ => "xidata".to_string(),
    };
    let x = get_for_mode(obj.unwrap(), &mode)?;
    let p = Box::into_raw(Box::new(x));
    unsafe { from_c(crate::capi::capsule::PyCapsule_New(p as *mut std::ffi::c_void, XID_NAME.as_ptr(), Some(xid_destroy))) }
}

fn t_restore_crossinterp_data(_m: &PyObject, o: &PyObject) -> PyResult {
    let p = unsafe { crate::capi::capsule::PyCapsule_GetPointer(o.as_ptr(), XID_NAME.as_ptr()) };
    if p.is_null() {
        return Err(PyErr);
    }
    unsafe { &*(p as *const Xi) }.to_object()
}

pub fn install(d: &PyObject, m: &PyObject) -> PyResult<()> {
    let _ = interpreter_error_type();
    crate::builtins::add_methods_to_dict(
        d,
        crate::mdefs![
            ("run_in_subinterp_with_config", FASTKW, t_run_in_subinterp_with_config),
            ("create_interpreter", FASTKW, t_create_interpreter),
            ("destroy_interpreter", FASTKW, t_destroy_interpreter),
            ("exec_interpreter", FASTKW, t_exec_interpreter),
            ("normalize_interp_id", O, t_normalize_interp_id),
            ("unused_interpreter_id", NOARGS, t_unused_interpreter_id),
            ("next_interpreter_id", NOARGS, t_next_interpreter_id),
            ("interpreter_exists", O, t_interpreter_exists),
            ("get_interpreter_refcount", O, t_get_interpreter_refcount),
            ("link_interpreter_refcount", O, t_link_interpreter_refcount),
            ("unlink_interpreter_refcount", O, t_unlink_interpreter_refcount),
            ("interpreter_refcount_linked", O, t_interpreter_refcount_linked),
            ("get_interp_settings", FAST, t_get_interp_settings),
            ("get_crossinterp_data", FASTKW, t_get_crossinterp_data),
            ("restore_crossinterp_data", O, t_restore_crossinterp_data),
        ],
        Some(m),
    )
}
