//! The `sys` module: attributes and functions (the module object itself and the
//! bootstrap streams live in sys.rs).

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::{PyObject, PyTypeObject};
use crate::modules::sys_structseq as ss;
use crate::object::*;
use crate::raise;
use crate::types::{bool_, dict, float, int, list, none, str as pystr, tuple};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering::*};
use std::sync::{Mutex, OnceLock};

pub const VERSION: &str = "3.15.2";
pub const HEXVERSION: i64 = 0x030F02F0;
pub const CACHE_TAG: &str = "cpython-315";
pub const MULTIARCH: &str = "x86_64-linux-gnu";

/// Interpreter configuration (filled by the launcher from the command line/environment).
#[derive(Clone, Debug)]
pub struct RtConfig {
    pub debug: i32,
    pub inspect: i32,
    pub interactive: i32,
    pub optimize: i32,
    pub dont_write_bytecode: i32,
    pub no_user_site: i32,
    pub no_site: i32,
    pub ignore_environment: i32,
    pub verbose: i32,
    pub bytes_warning: i32,
    pub quiet: i32,
    pub hash_randomization: i32,
    pub isolated: i32,
    pub dev_mode: bool,
    pub utf8_mode: i32,
    pub warn_default_encoding: i32,
    pub safe_path: bool,
    pub int_max_str_digits: i32,
    /// -X gil / PYTHON_GIL: None (-1) = default.
    pub gil: i32,
    pub thread_inherit_context: i32,
    pub context_aware_warnings: i32,
    /// -1 = default ("normal"), 0 = none, 1 = all
    pub lazy_imports: i32,
    pub unbuffered: bool,
    pub warnoptions: Vec<String>,
    pub xoptions: Vec<String>,
    pub pycache_prefix: Option<String>,
    pub orig_argv: Vec<String>,
    pub faulthandler: bool,
    pub tracemalloc: i32,
    pub check_hash_based_pycs: String,
    pub stdlib_dir: Option<String>,
}

impl Default for RtConfig {
    fn default() -> Self {
        RtConfig {
            debug: 0,
            inspect: 0,
            interactive: 0,
            optimize: 0,
            dont_write_bytecode: 0,
            no_user_site: 0,
            no_site: 0,
            ignore_environment: 0,
            verbose: 0,
            bytes_warning: 0,
            quiet: 0,
            hash_randomization: 1,
            isolated: 0,
            dev_mode: false,
            utf8_mode: 1,
            warn_default_encoding: 0,
            safe_path: false,
            int_max_str_digits: 4300,
            gil: -1,
            thread_inherit_context: 1,
            context_aware_warnings: 1,
            lazy_imports: -1,
            unbuffered: false,
            warnoptions: Vec::new(),
            xoptions: Vec::new(),
            pycache_prefix: None,
            orig_argv: Vec::new(),
            faulthandler: false,
            tracemalloc: 0,
            check_hash_based_pycs: "default".into(),
            stdlib_dir: None,
        }
    }
}

static CONFIG: OnceLock<RtConfig> = OnceLock::new();

pub fn set_config(c: RtConfig) {
    let _ = CONFIG.set(c);
}
pub fn config() -> &'static RtConfig {
    CONFIG.get_or_init(RtConfig::default)
}

pub fn source_line(filename: &str, lineno: i32) -> Option<String> {
    if lineno < 1 {
        return None;
    }
    let data = std::fs::read(filename).ok()?;
    let s = String::from_utf8_lossy(&data);
    s.split('\n').nth((lineno - 1) as usize).map(|l| l.trim_end_matches('\r').to_string())
}

// ------------------------------------------------------------------ global state

static SWITCH_INTERVAL: AtomicU64 = AtomicU64::new(0x3F747AE147AE147B); // 0.005
pub static FINALIZING: AtomicBool = AtomicBool::new(false);
static TRAMPOLINE: AtomicBool = AtomicBool::new(false);
static AUDIT_HOOKS: Mutex<Vec<usize>> = Mutex::new(Vec::new());
pub static HAVE_AUDIT_HOOKS: AtomicBool = AtomicBool::new(false);
static LAZY_MODE: Mutex<Option<usize>> = Mutex::new(None);
static LAZY_FILTER: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());

static FLAGS_TYPE: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());
static UNRAISABLE_ARGS_TYPE: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());
static VERSION_INFO_TYPE: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());

pub fn unraisable_args_type() -> *mut PyTypeObject {
    UNRAISABLE_ARGS_TYPE.load(Acquire)
}

pub fn is_finalizing() -> bool {
    FINALIZING.load(Relaxed)
}

// ------------------------------------------------------------------ audit

/// `sys.audit(event, *args)` from Rust. Fast when no hooks are installed.
pub fn audit(event: &str, args: &[&PyObject]) -> PyResult<()> {
    if !HAVE_AUDIT_HOOKS.load(Relaxed) {
        return Ok(());
    }
    let t = tuple::new_tuple(args.iter().map(|a| a.to_ref()).collect());
    audit_tuple(&pystr::new_str(event), &t)
}

fn audit_tuple(event: &PyObject, args: &PyObject) -> PyResult<()> {
    let hooks: Vec<PyRef> = AUDIT_HOOKS.lock().unwrap().iter().map(|&p| unsafe { PyRef::from_borrowed(p as *mut PyObject) }).collect();
    for h in hooks {
        // hooks are not traced or profiled unless they opt in with __cantrace__
        let suppress = crate::modules::sys::tracing_active()
            && match ab::getattr_opt(&h, crate::name!("__cantrace__")) {
                Ok(Some(v)) => !ab::is_true(&v)?,
                Ok(None) => true,
                Err(e) => return Err(e),
            };
        if suppress {
            let ts = crate::threads::current();
            ts.tracing_depth.set(ts.tracing_depth.get() + 1);
            let r = ab::call(&h, &[event, args]);
            ts.tracing_depth.set(ts.tracing_depth.get() - 1);
            r?;
        } else {
            ab::call(&h, &[event, args])?;
        }
    }
    Ok(())
}

fn sys_audit(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.is_empty() {
        raise!(TypeError, "audit() missing 1 required positional argument: 'event'");
    }
    if !pystr::is_str(args[0]) {
        raise!(TypeError, "expected str for argument 'event', not {}", crate::types::typeobject::tname(args[0]));
    }
    if HAVE_AUDIT_HOOKS.load(Relaxed) {
        let t = tuple::new_tuple(args[1..].iter().map(|a| a.to_ref()).collect());
        audit_tuple(args[0], &t)?;
    }
    Ok(none::none_ref())
}

fn sys_addaudithook(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("addaudithook", &["hook"], 0, 1);
    let [hook] = crate::args::parse::<1>(&P, args, kw)?;
    let hook = hook.unwrap();
    // existing hooks may veto with RuntimeError (silently ignored)
    if let Err(e) = audit("sys.addaudithook", &[]) {
        if exc_matches(crate::types::exceptions::RuntimeError()) {
            clear_exc();
            return Ok(none::none_ref());
        }
        return Err(e);
    }
    AUDIT_HOOKS.lock().unwrap().push(hook.to_ref().into_raw() as usize);
    HAVE_AUDIT_HOOKS.store(true, Release);
    Ok(none::none_ref())
}

// ------------------------------------------------------------------ functions

fn sys_exit(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("exit", args, 0, 1)?;
    match args.first() {
        Some(c) => Err(set_err_obj(crate::types::exceptions::SystemExit(), c.to_ref())),
        None => Err(set_err_noargs(crate::types::exceptions::SystemExit())),
    }
}

fn frame_at_depth(depth: i64) -> *mut crate::interp::frame::Frame {
    let mut depth = depth;
    let mut f = crate::interp::current_frame_ptr();
    while depth > 0 && !f.is_null() {
        f = unsafe { (*f).prev };
        depth -= 1;
    }
    f
}

fn sys_getframe(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("_getframe", args, 0, 1)?;
    let depth = match args.first() {
        Some(a) => ab::as_i64(a)?,
        None => 0,
    };
    let f = frame_at_depth(depth);
    if f.is_null() {
        raise!(ValueError, "call stack is not deep enough");
    }
    audit("sys._getframe", &[])?;
    let fo = crate::interp::eval::frame_object(unsafe { &mut *f });
    Ok(unsafe { PyRef::from_borrowed(fo) })
}

fn sys_getframemodulename(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("_getframemodulename", &["depth"], 0, 0);
    let [d] = crate::args::parse::<1>(&P, args, kw)?;
    let depth = match d {
        Some(a) => ab::as_i64(a)?,
        None => 0,
    };
    audit("sys._getframemodulename", &[&int::new_int(depth)])?;
    let f = frame_at_depth(depth);
    if f.is_null() {
        return Ok(none::none_ref());
    }
    let fr = unsafe { &*f };
    if !fr.func.is_null() {
        if let Ok(Some(m)) = ab::getattr_opt(unsafe { &*fr.func }, crate::name!("__module__")) {
            return Ok(m);
        }
        clear_exc();
    }
    Ok(dict::get_item_str(fr.globals(), crate::name!("__name__")).unwrap_or_else(none::none_ref))
}

fn sys_exception(_m: &PyObject) -> PyResult {
    let ts = crate::threads::current();
    let e = crate::interp::eval::handled_exception(ts);
    Ok(if e.is_null() { none::none_ref() } else { unsafe { PyRef::from_borrowed(e) } })
}

fn exc_info_tuple(e: *mut PyObject) -> PyRef {
    if e.is_null() {
        return tuple::new_tuple(vec![none::none_ref(), none::none_ref(), none::none_ref()]);
    }
    let e = unsafe { PyRef::from_borrowed(e) };
    let t = crate::types::typeobject::type_ref(e.type_ptr());
    let tb = crate::types::exceptions::exc_traceback(&e).unwrap_or_else(none::none_ref);
    tuple::new_tuple(vec![t, e, tb])
}

fn sys_exc_info(_m: &PyObject) -> PyResult {
    let ts = crate::threads::current();
    Ok(exc_info_tuple(crate::interp::eval::handled_exception(ts)))
}

fn sys_getrecursionlimit(_m: &PyObject) -> PyResult {
    Ok(int::new_int(crate::threads::current().c.py_recursion_limit as i64))
}

fn sys_setrecursionlimit(_m: &PyObject, v: &PyObject) -> PyResult {
    if crate::types::float::is_float(v) {
        raise!(TypeError, "'float' object cannot be interpreted as an integer");
    }
    if !int::is_int(v) {
        raise!(TypeError, "'{}' object cannot be interpreted as an integer", crate::types::typeobject::tname(v));
    }
    let n = match int::as_i64(v) {
        Some(n) if n <= i32::MAX as i64 => n,
        Some(n) if n < 0 => n,
        _ => raise!(OverflowError, "Python int too large to convert to C int"),
    };
    if n < 1 {
        raise!(ValueError, "recursion limit must be greater or equal than 1");
    }
    let ts = crate::threads::current();
    let depth = ts.recursion_depth.get() as i64;
    if n <= depth {
        raise!(RecursionError, "cannot set the recursion limit to {} at the recursion depth {}: the limit is too low", n, depth);
    }
    crate::threads::set_recursion_limit_all(n as i32);
    Ok(none::none_ref())
}

fn sys_intern(_m: &PyObject, s: &PyObject) -> PyResult {
    if !pystr::is_str_exact(s) {
        raise!(TypeError, "intern() argument must be str, not {}", crate::types::typeobject::tname(s));
    }
    Ok(pystr::intern(s.to_ref()))
}
fn sys_is_interned(_m: &PyObject, s: &PyObject) -> PyResult {
    if !pystr::is_str(s) {
        raise!(TypeError, "_is_interned() argument must be str, not {}", crate::types::typeobject::tname(s));
    }
    Ok(bool_::new_bool(pystr::is_interned(s)))
}
fn sys_is_immortal(_m: &PyObject, o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(o.is_immortal()))
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
    let out = match crate::modules::sys::get_sys_attr("stdout") {
        Some(o) if !o.is(none::none()) => o,
        _ => raise!(RuntimeError, "lost sys.stdout"),
    };
    let r = ab::repr(o)?;
    if let Err(e) = ab::call_method_str(&out, "write", &[&r]) {
        if !exc_matches(crate::types::exceptions::UnicodeEncodeError()) {
            return Err(e);
        }
        clear_exc();
        // fall back to backslashreplace with the stream's encoding
        let enc = ab::getattr_str(&out, "encoding")?;
        let b = ab::call_method_str(&r, "encode", &[&enc, &pystr::new_str("backslashreplace")])?;
        if let Ok(Some(buf)) = ab::getattr_opt(&out, &pystr::new_str("buffer")) {
            ab::call_method_str(&buf, "write", &[&b])?;
        } else {
            clear_exc();
            let s = ab::call_method_str(&b, "decode", &[&enc, &pystr::new_str("strict")])?;
            ab::call_method_str(&out, "write", &[&s])?;
        }
    }
    ab::call_method_str(&out, "write", &[&pystr::new_str("\n")])?;
    ab::setattr(&b, crate::name!("_"), o)?;
    Ok(none::none_ref())
}

/// Print an exception (with traceback and chained exceptions) to sys.stderr, as
/// sys.excepthook and PyErr_Display do; a non-exception is reported as such.
pub fn display_exception(v: &PyObject) {
    if crate::types::exceptions::is_exception_instance(v) {
        print_exception_full(v);
    } else {
        crate::modules::sys::write_stderr(&format!(
            "TypeError: print_exception(): Exception expected for value, {} found\n",
            crate::types::typeobject::tname(v)
        ));
    }
}

fn sys_excepthook(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("excepthook", args, 3, 3)?;
    display_exception(args[1]);
    Ok(none::none_ref())
}

/// Print an exception with the `traceback` module if it can be imported, else natively.
pub fn print_exception_full(v: &PyObject) {
    if let Some(tbm) = crate::import::get_loaded_module("traceback").or_else(|| match crate::import::import_absolute("traceback") {
        Ok(m) => Some(m),
        Err(_) => {
            clear_exc();
            None
        }
    }) {
        if let Ok(f) = ab::getattr_str(&tbm, "print_exception") {
            if let Some(err) = crate::modules::sys::get_sys_attr("stderr") {
                if !err.is(none::none()) {
                    let kwn = tuple::new_tuple(vec![pystr::new_str("file")]);
                    if ab::call_kw(&f, &[v, &err], Some(&kwn)).is_ok() {
                        return;
                    }
                }
            }
        }
        clear_exc();
    }
    crate::modules::traceback_print::print_exception(v);
}

fn sys_breakpointhook(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let env = if config().ignore_environment != 0 { None } else { std::env::var("PYTHONBREAKPOINT").ok() };
    let hookname = match env.as_deref() {
        None | Some("") => "pdb.set_trace".to_string(),
        Some("0") => return Ok(none::none_ref()),
        Some(s) => s.to_string(),
    };
    let (modname, attr) = match hookname.rfind('.') {
        Some(i) => (hookname[..i].to_string(), hookname[i + 1..].to_string()),
        None => ("builtins".to_string(), hookname.clone()),
    };
    let r = (|| -> PyResult {
        let m = crate::import::import_absolute(&modname)?;
        ab::getattr_str(&m, &attr)
    })();
    let hook = match r {
        Ok(h) => h,
        Err(_) => {
            if exc_matches(crate::types::exceptions::ImportError()) || exc_matches(crate::types::exceptions::AttributeError()) {
                clear_exc();
                crate::modules::warnings::warn(
                    crate::types::exceptions::RuntimeWarning(),
                    &format!("Ignoring unimportable $PYTHONBREAKPOINT: \"{}\"", hookname),
                    1,
                )?;
                return Ok(none::none_ref());
            }
            return Err(PyErr);
        }
    };
    ab::call_kw(&hook, args, kw)
}

fn sys_unraisablehook(_m: &PyObject, a: &PyObject) -> PyResult {
    let tp = unraisable_args_type();
    if !a.is_type(tp) {
        raise!(TypeError, "sys.unraisablehook argument type must be UnraisableHookArgs");
    }
    let et = ss::get_field(a, 0);
    let ev = ss::get_field(a, 1);
    let tb = ss::get_field(a, 2);
    let msg = ss::get_field(a, 3);
    let obj = ss::get_field(a, 4);
    let mut out = String::new();
    if !obj.is(none::none()) {
        let r = match ab::repr_string(&obj) {
            Ok(s) => s,
            Err(_) => {
                clear_exc();
                "<object repr() failed>".into()
            }
        };
        if !msg.is(none::none()) {
            out.push_str(&format!("{}: {}\n", pystr::to_string_lossy(&msg), r));
        } else {
            out.push_str(&format!("Exception ignored in: {}\n", r));
        }
    } else if !msg.is(none::none()) {
        out.push_str(&format!("{}:\n", pystr::to_string_lossy(&msg)));
    }
    let file = crate::modules::sys::get_sys_attr("stderr").filter(|e| !e.is(none::none()));
    let write = |s: &str| {
        if s.is_empty() {
            return;
        }
        match &file {
            Some(f) => {
                if ab::call_method_str(f, "write", &[&pystr::new_str(s)]).is_err() {
                    clear_exc();
                }
            }
            None => crate::modules::sys::write_fd(2, s.as_bytes()),
        }
    };
    write(&out);
    if crate::types::exceptions::is_exception_instance(&ev) {
        // use traceback.print_exception when available for full formatting
        let mut done = false;
        if let Some(tbm) = crate::import::get_loaded_module("traceback") {
            if let (Ok(f), Some(fl)) = (ab::getattr_str(&tbm, "print_exception"), &file) {
                let kwn = tuple::new_tuple(vec![pystr::new_str("file")]);
                if ab::call_kw(&f, &[&et, &ev, &tb, fl], Some(&kwn)).is_ok() {
                    done = true;
                }
            }
            clear_exc();
        }
        if !done {
            write(&crate::modules::traceback_print::format_exception(&ev));
        }
    } else if !et.is(none::none()) {
        let tn = if crate::types::typeobject::is_type(&et) {
            crate::types::typeobject::type_fqname(crate::types::typeobject::as_type(&et))
        } else {
            ab::repr_string(&et).unwrap_or_default()
        };
        if ev.is(none::none()) {
            write(&format!("{}\n", tn));
        } else {
            let s = ab::str_string(&ev).unwrap_or_else(|_| {
                clear_exc();
                "<exception str() failed>".into()
            });
            write(&format!("{}: {}\n", tn, s));
        }
    }
    if let Some(f) = &file {
        if ab::call_method_str(f, "flush", &[]).is_err() {
            clear_exc();
        }
    }
    Ok(none::none_ref())
}

/// PyErr_WriteUnraisable / PyErr_FormatUnraisable implementation.
pub fn write_unraisable(err_msg: Option<&str>, obj: Option<&PyObject>) {
    write_unraisable_obj(err_msg.map(pystr::new_str), obj)
}

/// As write_unraisable, with the message as a str object (None: the hook's
/// default wording). An exception without a traceback gets one for the current
/// frame, so that the report says where it happened.
pub fn write_unraisable_obj(err_msg: Option<PyRef>, obj: Option<&PyObject>) {
    let e = take_exc();
    if let Some(e) = &e {
        if crate::types::exceptions::is_exception_instance(e) && crate::types::exceptions::exc_traceback(e).is_none() {
            let f = crate::interp::current_frame_ptr();
            if !f.is_null() {
                unsafe {
                    let fo = crate::interp::eval::frame_object(&mut *f);
                    let tb = crate::types::traceback::new_traceback(None, &*fo, (*f).lasti as i32, (*f).lineno());
                    crate::types::exceptions::set_exc_traceback(e, Some(&tb));
                }
            }
        }
    }
    let tp = unraisable_args_type();
    let et = e.as_ref().map(|e| crate::types::typeobject::type_ref(e.type_ptr())).unwrap_or_else(none::none_ref);
    let ev = e.clone().unwrap_or_else(none::none_ref);
    let tb = e.as_ref().and_then(|e| crate::types::exceptions::exc_traceback(e)).unwrap_or_else(none::none_ref);
    let msg_text = err_msg.as_ref().map(|m| pystr::to_string_lossy(m));
    let m = err_msg.unwrap_or_else(none::none_ref);
    let o = obj.map(|o| o.to_ref()).unwrap_or_else(none::none_ref);
    if tp.is_null() {
        crate::modules::sys::write_stderr(&format!("{}\n", msg_text.as_deref().unwrap_or("Exception ignored")));
        if let Some(e) = &e {
            crate::modules::traceback_print::print_exception(e);
        }
        return;
    }
    let args = ss::new_instance(tp, vec![et, ev, tb, m, o]);
    let hook = crate::modules::sys::get_sys_attr("unraisablehook");
    let r = match &hook {
        Some(h) if !h.is(none::none()) => {
            if audit("sys.unraisablehook", &[h, &args]).is_err() {
                Err(PyErr)
            } else {
                ab::call(h, &[&args]).map(|_| ())
            }
        }
        _ => sys_unraisablehook(none::none(), &args).map(|_| ()),
    };
    if r.is_err() {
        // the hook failed: report that with the default hook
        let e2 = take_exc();
        let et2 = e2.as_ref().map(|e| crate::types::typeobject::type_ref(e.type_ptr())).unwrap_or_else(none::none_ref);
        let tb2 = e2.as_ref().and_then(|e| crate::types::exceptions::exc_traceback(e)).unwrap_or_else(none::none_ref);
        let ev2 = e2.unwrap_or_else(none::none_ref);
        let a2 = ss::new_instance(
            tp,
            vec![
                et2,
                ev2,
                tb2,
                pystr::new_str("Exception ignored in sys.unraisablehook"),
                hook.unwrap_or_else(none::none_ref),
            ],
        );
        if sys_unraisablehook(none::none(), &a2).is_err() {
            clear_exc();
        }
    }
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
fn sys_settraceallthreads(_m: &PyObject, f: &PyObject) -> PyResult {
    crate::modules::sys_trace_api::settrace_all(f)
}
fn sys_setprofileallthreads(_m: &PyObject, f: &PyObject) -> PyResult {
    crate::modules::sys_trace_api::setprofile_all(f)
}
fn sys_call_tracing(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("call_tracing", args, 2, 2)?;
    if !tuple::is_tuple(args[1]) {
        raise!(TypeError, "call_tracing(): argument 2 must be tuple, not {}", crate::types::typeobject::tname(args[1]));
    }
    let items: Vec<&PyObject> = tuple::tuple_items(args[1]).to_vec();
    crate::modules::sys_trace_api::call_tracing(args[0], &items)
}

fn sys_getsizeof(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("getsizeof", &["object", "default"], 0, 1);
    let [o, default] = crate::args::parse::<2>(&P, args, kw)?;
    let o = o.unwrap();
    match ab::call_special_opt(o, crate::name!("__sizeof__"), &[]) {
        Ok(Some(r)) => {
            if !int::is_int(&r) {
                raise!(TypeError, "an integer is required");
            }
            let n = match int::as_i64(&r) {
                Some(n) => n,
                None => {
                    if int::sign(&r) < 0 {
                        raise!(ValueError, "__sizeof__() should return >= 0");
                    }
                    raise!(OverflowError, "Python int too large to convert to C ssize_t");
                }
            };
            if n < 0 {
                raise!(ValueError, "__sizeof__() should return >= 0");
            }
            // like the free-threaded build: no GC header is reported (ours is internal),
            // only the managed __dict__/__weakref__ pre-header
            let extra = if unsafe { (*o.type_ptr()).tp_flags } & crate::ffi::Py_TPFLAGS_PREHEADER != 0 { crate::alloc::MANAGED as i64 } else { 0 };
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

fn sys_get_int_max_str_digits(_m: &PyObject) -> PyResult {
    Ok(int::new_int(crate::modules::sys::int_max_str_digits() as i64))
}
fn sys_set_int_max_str_digits(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("set_int_max_str_digits", &["maxdigits"], 0, 1);
    let [v] = crate::args::parse::<1>(&P, args, kw)?;
    let v = v.unwrap();
    let n = ab::index(v)?;
    let n = int::as_i64(&n).unwrap_or(i64::MAX);
    if n != 0 && n < 640 {
        raise!(ValueError, "maxdigits must be >= 640 or 0 for unlimited");
    }
    let n = n.min(i32::MAX as i64);
    crate::modules::sys::INT_MAX_STR_DIGITS.store(n as i32, Relaxed);
    Ok(none::none_ref())
}

fn sys_getswitchinterval(_m: &PyObject) -> PyResult {
    Ok(float::new_float(f64::from_bits(SWITCH_INTERVAL.load(Relaxed))))
}
fn sys_setswitchinterval(_m: &PyObject, v: &PyObject) -> PyResult {
    let x = ab::float_value(v)?;
    if x <= 0.0 {
        raise!(ValueError, "switch interval must be strictly positive");
    }
    SWITCH_INTERVAL.store(x.to_bits(), Relaxed);
    Ok(none::none_ref())
}
fn sys_is_finalizing(_m: &PyObject) -> PyResult {
    Ok(bool_::new_bool(is_finalizing()))
}
fn sys_clear_type_cache(_m: &PyObject) -> PyResult {
    crate::modules::warnings::warn(
        crate::types::exceptions::DeprecationWarning(),
        "sys._clear_type_cache() is deprecated and scheduled for removal in a future version. Use sys._clear_internal_caches() instead.",
        1,
    )?;
    Ok(none::none_ref())
}
fn sys_clear_internal_caches(_m: &PyObject) -> PyResult {
    Ok(none::none_ref())
}

fn thread_frames_and_excs(want_exc: bool) -> PyResult {
    audit(if want_exc { "sys._current_exceptions" } else { "sys._current_frames" }, &[])?;
    let d = dict::new_dict();
    let threads: Vec<usize> = crate::threads::ALL_THREADS.lock().unwrap().clone();
    for t in threads {
        let ts = unsafe { &*(t as *const crate::threads::ThreadState) };
        if !crate::threads::is_live(ts) {
            continue;
        }
        let key = int::new_int_u128(ts.c.thread_id as u128);
        if want_exc {
            let e = crate::interp::eval::handled_exception(ts);
            let v = if e.is_null() { none::none_ref() } else { unsafe { PyRef::from_borrowed(e) } };
            dict::set_item(&d, &key, &v)?;
        } else {
            let f = ts.frame.get();
            if f.is_null() {
                continue;
            }
            let fo = crate::interp::eval::frame_object(unsafe { &mut *f });
            dict::set_item(&d, &key, unsafe { &*fo })?;
        }
    }
    Ok(d)
}
fn sys_current_frames(_m: &PyObject) -> PyResult {
    thread_frames_and_excs(false)
}
fn sys_current_exceptions(_m: &PyObject) -> PyResult {
    thread_frames_and_excs(true)
}

fn sys_activate_stack_trampoline(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("activate_stack_trampoline", &["backend"], 1, 1);
    let [b] = crate::args::parse::<1>(&P, args, kw)?;
    let b = b.unwrap();
    if !pystr::is_str(b) {
        raise!(TypeError, "activate_stack_trampoline() argument must be str, not {}", crate::types::typeobject::tname(b));
    }
    let s = pystr::to_string_lossy(b);
    if s == "perf" || s == "perf_jit" {
        TRAMPOLINE.store(true, Relaxed);
        return Ok(none::none_ref());
    }
    raise!(ValueError, "invalid backend: {}", s);
}
fn sys_deactivate_stack_trampoline(_m: &PyObject) -> PyResult {
    TRAMPOLINE.store(false, Relaxed);
    Ok(none::none_ref())
}
fn sys_is_stack_trampoline_active(_m: &PyObject) -> PyResult {
    Ok(bool_::new_bool(TRAMPOLINE.load(Relaxed)))
}
fn sys_is_remote_debug_enabled(_m: &PyObject) -> PyResult {
    Ok(bool_::new_bool(false))
}
fn sys_remote_exec(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("remote_exec", args, 2, 2)?;
    raise!(RuntimeError, "Remote debugging is not enabled");
}
fn sys_getallocatedblocks(_m: &PyObject) -> PyResult {
    Ok(int::new_int(0))
}
fn sys_getunicodeinternedsize(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("getunicodeinternedsize", &["_only_immortal"], 0, 0).kwonly(1);
    let _ = crate::args::parse::<1>(&P, args, kw)?;
    Ok(int::new_int(0))
}
fn sys_debugmallocstats(_m: &PyObject) -> PyResult {
    Ok(none::none_ref())
}
fn sys_get_coroutine_origin_tracking_depth(_m: &PyObject) -> PyResult {
    Ok(int::new_int(crate::threads::current().c.coroutine_origin_tracking_depth as i64))
}
fn sys_set_coroutine_origin_tracking_depth(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("set_coroutine_origin_tracking_depth", &["depth"], 0, 1);
    let [d] = crate::args::parse::<1>(&P, args, kw)?;
    let d = ab::index(d.unwrap())?;
    let n = int::as_i64(&d).unwrap_or(i64::MAX);
    if n < 0 {
        raise!(ValueError, "depth must be >= 0");
    }
    let ts = crate::threads::current_mut();
    unsafe { (*ts).c.coroutine_origin_tracking_depth = n.min(i32::MAX as i64) as i32 };
    Ok(none::none_ref())
}

static ASYNCGEN_HOOKS_TYPE: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());

fn sys_get_asyncgen_hooks(_m: &PyObject) -> PyResult {
    let ts = crate::threads::current();
    let g = |p: *mut PyObject| if p.is_null() { none::none_ref() } else { unsafe { PyRef::from_borrowed(p) } };
    Ok(ss::new_instance(ASYNCGEN_HOOKS_TYPE.load(Acquire), vec![g(ts.c.async_gen_firstiter), g(ts.c.async_gen_finalizer)]))
}
fn sys_set_asyncgen_hooks(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("set_asyncgen_hooks", &["firstiter", "finalizer"], 0, 0);
    let [fi, fz] = crate::args::parse::<2>(&P, args, kw)?;
    for (v, nm) in [(fz, "finalizer"), (fi, "firstiter")] {
        if let Some(v) = v {
            if !v.is(none::none()) && !ab::is_callable(v) {
                raise!(TypeError, "callable {} expected, got {}", nm, crate::types::typeobject::tname(v));
            }
        }
    }
    let ts = crate::threads::current_mut();
    unsafe {
        let set = |slot: *mut *mut PyObject, v: Option<&PyObject>| {
            if let Some(v) = v {
                let nv = if v.is(none::none()) { std::ptr::null_mut() } else { v.to_ref().into_raw() };
                let old = std::mem::replace(&mut *slot, nv);
                xdecref(old);
            }
        };
        set(&raw mut (*ts).c.async_gen_finalizer, fz);
        set(&raw mut (*ts).c.async_gen_firstiter, fi);
    }
    Ok(none::none_ref())
}

static DLOPENFLAGS: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(libc::RTLD_NOW);
fn sys_getdlopenflags(_m: &PyObject) -> PyResult {
    Ok(int::new_int(DLOPENFLAGS.load(Relaxed) as i64))
}
fn sys_setdlopenflags(_m: &PyObject, v: &PyObject) -> PyResult {
    let n = ab::as_i64(v)?;
    DLOPENFLAGS.store(n as i32, Relaxed);
    Ok(none::none_ref())
}
pub fn dlopenflags() -> i32 {
    DLOPENFLAGS.load(Relaxed)
}

// lazy imports (PEP 810)
pub fn lazy_imports_mode() -> String {
    match *LAZY_MODE.lock().unwrap() {
        Some(1) => "all".into(),
        Some(_) => "normal".into(),
        None => match config().lazy_imports {
            1 => "all".into(),
            _ => "normal".into(),
        },
    }
}
fn sys_set_lazy_imports(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("set_lazy_imports", &["mode"], 0, 1);
    let [m] = crate::args::parse::<1>(&P, args, kw)?;
    let m = m.unwrap();
    if !pystr::is_str(m) {
        raise!(TypeError, "mode must be a string, not {}", crate::types::typeobject::tname(m));
    }
    let v = match pystr::to_string_lossy(m).as_str() {
        "normal" => 2,
        "all" => 1,
        _ => raise!(ValueError, "mode must be \"normal\" or \"all\""),
    };
    *LAZY_MODE.lock().unwrap() = Some(v);
    Ok(none::none_ref())
}
fn sys_get_lazy_imports(_m: &PyObject) -> PyResult {
    Ok(pystr::new_str(&lazy_imports_mode()))
}
fn sys_set_lazy_imports_filter(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("set_lazy_imports_filter", &["filter"], 0, 1);
    let [f] = crate::args::parse::<1>(&P, args, kw)?;
    let f = f.unwrap();
    if !f.is(none::none()) && !ab::is_callable(f) {
        raise!(TypeError, "filter must be callable or None");
    }
    let nv = if f.is(none::none()) { std::ptr::null_mut() } else { f.to_ref().into_raw() };
    let old = LAZY_FILTER.swap(nv, AcqRel);
    unsafe { xdecref(old) };
    Ok(none::none_ref())
}
pub fn lazy_imports_filter() -> Option<PyRef> {
    let p = LAZY_FILTER.load(Acquire);
    if p.is_null() { None } else { Some(unsafe { PyRef::from_borrowed(p) }) }
}
fn sys_get_lazy_imports_filter(_m: &PyObject) -> PyResult {
    Ok(lazy_imports_filter().unwrap_or_else(none::none_ref))
}

// sys._jit
fn jit_false(_m: &PyObject) -> PyResult {
    Ok(bool_::new_bool(false))
}

fn exe_path() -> String {
    paths().executable.clone()
}

/// Interpreter location and prefixes (CPython's getpath, simplified; see
/// pkgtools notes).  Computed once.
pub struct PathConfig {
    pub executable: String,
    pub base_executable: String,
    pub prefix: String,
    pub exec_prefix: String,
    pub base_prefix: String,
    pub base_exec_prefix: String,
    pub stdlib_dir: String,
    /// `<base_prefix>/lib/python315t.zip` (listed in sys.path even if absent)
    pub zip_path: String,
    /// `<base_exec_prefix>/lib/python3.15t/lib-dynload`
    pub platstdlib_dir: String,
}

static PATHS: OnceLock<PathConfig> = OnceLock::new();

pub fn paths() -> &'static PathConfig {
    PATHS.get_or_init(compute_paths)
}

/// Lexically normalise an absolute path ('.', '..', duplicate slashes).
fn normalize_abs(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    format!("/{}", parts.join("/"))
}

fn path_str(p: &std::path::Path) -> String {
    p.to_string_lossy().into_owned()
}

fn dirname(p: &str) -> String {
    match p.rfind('/') {
        Some(0) => "/".into(),
        Some(i) => p[..i].into(),
        None => String::new(),
    }
}

fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

fn is_executable_file(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// sys.executable: argv[0] made absolute (symlinks are NOT resolved, so a
/// venv's bin/python finds its pyvenv.cfg), or found on PATH.
fn program_path(real_exe: &str) -> String {
    let argv0 = config()
        .orig_argv
        .first()
        .cloned()
        .or_else(|| std::env::args_os().next().map(|a| a.to_string_lossy().into_owned()))
        .unwrap_or_default();
    if argv0.contains('/') {
        if argv0.starts_with('/') {
            return normalize_abs(&argv0);
        }
        if let Ok(cwd) = std::env::current_dir() {
            return normalize_abs(&format!("{}/{}", path_str(&cwd), argv0));
        }
    } else if !argv0.is_empty() {
        if let Some(path) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path) {
                let cand = dir.join(&argv0);
                if is_executable_file(&cand) {
                    let s = path_str(&cand);
                    if s.starts_with('/') {
                        return normalize_abs(&s);
                    }
                    if let Ok(cwd) = std::env::current_dir() {
                        return normalize_abs(&format!("{}/{}", path_str(&cwd), s));
                    }
                }
            }
        }
    }
    real_exe.to_string()
}

/// Read the `home` key of a pyvenv.cfg file (None if absent).
fn pyvenv_home(cfg: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(cfg).ok()?;
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            if k.trim().eq_ignore_ascii_case("home") {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

fn compute_paths() -> PathConfig {
    let c = config();
    let use_env = c.ignore_environment == 0;
    let real_exe = std::env::current_exe().map(|p| path_str(&p)).unwrap_or_default();
    let executable = match std::env::var("RPYTHON_EXECUTABLE") {
        Ok(p) if !p.is_empty() => p,
        _ => program_path(&real_exe),
    };

    // Virtual environment: pyvenv.cfg next to the executable or one level up.
    let exe_dir = dirname(&executable);
    let mut venv_prefix: Option<String> = None;
    let mut home: Option<String> = None;
    for dir in [exe_dir.clone(), dirname(&exe_dir)] {
        if dir.is_empty() {
            continue;
        }
        let cfg = std::path::Path::new(&dir).join("pyvenv.cfg");
        if cfg.is_file() {
            venv_prefix = Some(dir);
            home = pyvenv_home(&cfg);
            break;
        }
    }

    let base_executable = match &home {
        Some(h) => {
            let name = basename(&executable).to_string();
            let resolved_name = std::fs::canonicalize(&executable)
                .ok()
                .map(|p| basename(&path_str(&p)).to_string());
            let mut cands = vec![format!("{}/{}", h, name)];
            if let Some(r) = resolved_name {
                cands.push(format!("{}/{}", h, r));
            }
            if let Some(found) = cands.iter().find(|p| std::path::Path::new(p).is_file()) {
                found.clone()
            } else if dirname(&real_exe) == *h {
                real_exe.clone()
            } else {
                cands[0].clone()
            }
        }
        None => executable.clone(),
    };

    // The base installation: <prefix>/bin/python3.15t -> <prefix>.
    let pythonhome = if use_env { std::env::var("PYTHONHOME").ok().filter(|v| !v.is_empty()) } else { None };
    let (base_prefix, base_exec_prefix) = if let Ok(p) = std::env::var("RPYTHON_PREFIX") {
        (p.clone(), p)
    } else if let Some(h) = pythonhome {
        match h.split_once(':') {
            Some((a, b)) => (a.to_string(), b.to_string()),
            None => (h.clone(), h),
        }
    } else {
        let anchor = std::fs::canonicalize(&base_executable)
            .map(|p| path_str(&p))
            .unwrap_or_else(|_| real_exe.clone());
        let p = dirname(&dirname(&anchor));
        let p = if p.is_empty() { "/usr/local".to_string() } else { p };
        (p.clone(), p)
    };
    let (prefix, exec_prefix) = match &venv_prefix {
        Some(v) => (v.clone(), v.clone()),
        None => (base_prefix.clone(), base_exec_prefix.clone()),
    };

    let stdlib_dir = if let Some(d) = &c.stdlib_dir {
        d.clone()
    } else if let Ok(d) = std::env::var("RPYTHON_STDLIB") {
        d
    } else {
        let p = format!("{}/lib/python3.15t", base_prefix);
        if std::path::Path::new(&p).join("os.py").is_file() {
            p
        } else {
            let dev = concat!(env!("CARGO_MANIFEST_DIR"), "/../../Lib");
            std::fs::canonicalize(dev).map(|p| path_str(&p)).unwrap_or_else(|_| dev.to_string())
        }
    };
    PathConfig {
        zip_path: format!("{}/lib/python315t.zip", base_prefix),
        platstdlib_dir: format!("{}/lib/python3.15t/lib-dynload", base_exec_prefix),
        executable,
        base_executable,
        prefix,
        exec_prefix,
        base_prefix,
        base_exec_prefix,
        stdlib_dir,
    }
}

/// Installation prefix (sys.prefix): the venv directory inside a virtual
/// environment, else <prefix> of <prefix>/bin/python3.15t.
pub fn prefix() -> String {
    paths().prefix.clone()
}

/// The standard library directory.
pub fn stdlib_dir() -> String {
    paths().stdlib_dir.clone()
}

/// The default module search path after PYTHONPATH: the stdlib zip, the
/// stdlib directory and (if present) lib-dynload.
pub fn default_search_path() -> Vec<String> {
    let p = paths();
    let mut v = vec![p.zip_path.clone(), p.stdlib_dir.clone()];
    if std::path::Path::new(&p.platstdlib_dir).is_dir() {
        v.push(p.platstdlib_dir.clone());
    }
    v
}

pub const STDLIB_MODULE_NAMES: &[&str] = &[
    "__future__", "_abc", "_aix_support", "_android_support", "_apple_support", "_ast", "_asyncio", "_bisect", "_blake2",
    "_bz2", "_codecs", "_codecs_cn", "_codecs_hk", "_codecs_iso2022", "_codecs_jp", "_codecs_kr", "_codecs_tw",
    "_collections", "_collections_abc", "_colorize", "_compat_pickle", "_contextvars", "_csv", "_ctypes", "_curses",
    "_curses_panel", "_datetime", "_dbm", "_decimal", "_elementtree", "_frozen_importlib", "_frozen_importlib_external",
    "_functools", "_gdbm", "_hashlib", "_heapq", "_hmac", "_imp", "_interpchannels", "_interpqueues", "_interpreters",
    "_io", "_ios_support", "_json", "_locale", "_lsprof", "_lzma", "_markupbase", "_md5", "_multibytecodec",
    "_multiprocessing", "_opcode", "_opcode_metadata", "_operator", "_osx_support", "_overlapped", "_pickle",
    "_posixshmem", "_posixsubprocess", "_py_abc", "_py_warnings", "_pydatetime", "_pydecimal", "_pyio", "_pylong",
    "_pyrepl", "_queue", "_random", "_remote_debugging", "_scproxy", "_sha1", "_sha2", "_sha3", "_signal", "_socket",
    "_sqlite3", "_sre", "_ssl", "_stat", "_statistics", "_string", "_strptime", "_struct", "_suggestions", "_symtable",
    "_sysconfig", "_thread", "_threading_local", "_tkinter", "_tokenize", "_tracemalloc", "_types", "_typing", "_uuid",
    "_warnings", "_weakref", "_weakrefset", "_winapi", "_wmi", "_zoneinfo", "_zstd", "abc", "annotationlib", "antigravity",
    "argparse", "array", "ast", "asyncio", "atexit", "base64", "bdb", "binascii", "bisect", "builtins", "bz2", "cProfile",
    "calendar", "cmath", "cmd", "code", "codecs", "codeop", "collections", "colorsys", "compileall", "compression",
    "concurrent", "configparser", "contextlib", "contextvars", "copy", "copyreg", "csv", "ctypes", "curses", "dataclasses",
    "datetime", "dbm", "decimal", "difflib", "dis", "doctest", "email", "encodings", "ensurepip", "enum", "errno",
    "faulthandler", "fcntl", "filecmp", "fileinput", "fnmatch", "fractions", "ftplib", "functools", "gc", "genericpath",
    "getopt", "getpass", "gettext", "glob", "graphlib", "grp", "gzip", "hashlib", "heapq", "hmac", "html", "http", "idlelib",
    "imaplib", "importlib", "inspect", "io", "ipaddress", "itertools", "json", "keyword", "linecache", "locale", "logging",
    "lzma", "mailbox", "marshal", "math", "mimetypes", "mmap", "modulefinder", "msvcrt", "multiprocessing", "netrc", "nt",
    "ntpath", "nturl2path", "numbers", "opcode", "operator", "optparse", "os", "pathlib", "pdb", "pickle", "pickletools",
    "pkgutil", "platform", "plistlib", "poplib", "posix", "posixpath", "pprint", "profile", "profiling", "pstats", "pty",
    "pwd", "py_compile", "pyclbr", "pydoc", "pydoc_data", "pyexpat", "queue", "quopri", "random", "re", "readline",
    "reprlib", "resource", "rlcompleter", "runpy", "sched", "secrets", "select", "selectors", "shelve", "shlex", "shutil",
    "signal", "site", "smtplib", "socket", "socketserver", "sqlite3", "sre_compile", "sre_constants", "sre_parse", "ssl",
    "stat", "statistics", "string", "stringprep", "struct", "subprocess", "symtable", "sys", "sysconfig", "syslog",
    "tabnanny", "tarfile", "tempfile", "termios", "textwrap", "this", "threading", "time", "timeit", "tkinter", "token",
    "tokenize", "tomllib", "trace", "traceback", "tracemalloc", "tty", "turtle", "turtledemo", "types", "typing",
    "unicodedata", "unittest", "urllib", "uuid", "venv", "warnings", "wave", "weakref", "webbrowser", "winreg", "winsound",
    "wsgiref", "xml", "xmlrpc", "zipapp", "zipfile", "zipimport", "zlib", "zoneinfo",
];

fn version_string() -> String {
    format!("{} free-threading build (main, Nov  1 2026, 00:00:00) [rustc]", VERSION)
}

fn thread_lib_version() -> PyRef {
    unsafe {
        let mut buf = [0u8; 128];
        let n = libc::confstr(libc::_CS_GNU_LIBPTHREAD_VERSION, buf.as_mut_ptr() as *mut libc::c_char, buf.len());
        if n > 1 && n <= buf.len() {
            return pystr::new_str(std::str::from_utf8(&buf[..n - 1]).unwrap_or(""));
        }
    }
    none::none_ref()
}

/// Build `sys.flags` from the configuration.
pub fn make_flags() -> PyResult {
    let c = config();
    let mut tp = FLAGS_TYPE.load(Acquire);
    if tp.is_null() {
        tp = ss::make_type(
            "sys.flags",
            &[
                "debug", "inspect", "interactive", "optimize", "dont_write_bytecode", "no_user_site", "no_site",
                "ignore_environment", "verbose", "bytes_warning", "quiet", "hash_randomization", "isolated", "dev_mode",
                "utf8_mode", "warn_default_encoding", "safe_path", "int_max_str_digits", "gil", "thread_inherit_context",
                "context_aware_warnings", "lazy_imports",
            ],
            18,
            false,
        )?;
        FLAGS_TYPE.store(tp, Release);
    }
    let i = |v: i32| int::new_int(v as i64);
    let gil = if c.gil < 0 { none::none_ref() } else { i(c.gil) };
    let lazy = if c.lazy_imports < 0 { none::none_ref() } else { i(c.lazy_imports) };
    Ok(ss::new_instance(
        tp,
        vec![
            i(c.debug),
            i(c.inspect),
            i(c.interactive),
            i(c.optimize),
            i(c.dont_write_bytecode),
            i(c.no_user_site),
            i(c.no_site),
            i(c.ignore_environment),
            i(c.verbose),
            i(c.bytes_warning),
            i(c.quiet),
            i(c.hash_randomization),
            i(c.isolated),
            bool_::new_bool(c.dev_mode),
            i(c.utf8_mode),
            i(c.warn_default_encoding),
            bool_::new_bool(c.safe_path),
            i(c.int_max_str_digits),
            gil,
            i(c.thread_inherit_context),
            i(c.context_aware_warnings),
            lazy,
        ],
    ))
}

pub fn make_version_info() -> PyResult {
    let mut tp = VERSION_INFO_TYPE.load(Acquire);
    if tp.is_null() {
        tp = ss::make_type("sys.version_info", &["major", "minor", "micro", "releaselevel", "serial"], 5, false)?;
        VERSION_INFO_TYPE.store(tp, Release);
    }
    Ok(ss::new_instance(tp, vec![int::new_int(3), int::new_int(15), int::new_int(2), pystr::new_str("final"), int::new_int(0)]))
}

/// Register the builtin modules implemented by the system workstream.
pub fn register_system_modules() {
    crate::import::register_builtin("_thread", crate::modules::_thread::init_module);
    crate::import::register_builtin("_signal", crate::modules::signal::init_module);
    crate::import::register_builtin("_imp", crate::modules::_imp::init_module);
    crate::import::register_builtin("_queue", crate::modules::_queue::init_module);
    crate::import::register_builtin("select", crate::modules::select::init_module);
    crate::import::register_builtin("gc", crate::modules::gc_mod::init_module);
    crate::import::register_builtin("faulthandler", crate::modules::faulthandler::init_module);
}

/// Late initialisation (after sys and builtins exist).
pub fn post_init() -> PyResult<()> {
    if config().faulthandler {
        crate::modules::faulthandler::enable_at_startup();
    }
    crate::import::install_importlib(&stdlib_dir())?;
    if config().tracemalloc > 0 {
        crate::modules::_tracemalloc::start(config().tracemalloc as i64)?;
    }
    Ok(())
}

/// `import site` (unless -S).  Runs after the standard streams and
/// builtins.open exist, as in CPython (site.venv() reads pyvenv.cfg).
pub fn import_site() -> PyResult<()> {
    if config().no_site == 0 && std::path::Path::new(&stdlib_dir()).join("site.py").is_file() {
        if crate::import::import_absolute("site").is_err() {
            // CPython aborts start-up here; we report and continue so that a
            // partially available stdlib remains usable.
            let e = take_exc();
            if config().verbose > 0 {
                crate::modules::sys::write_stderr("Error in site import:\n");
                if let Some(e) = e {
                    crate::modules::traceback_print::print_exception(&e);
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn call_module_func(module: &str, func: &str) {
    if let Some(m) = crate::import::get_loaded_module(module) {
        let r = ab::getattr_str(&m, func).and_then(|f| ab::call(&f, &[]));
        if r.is_err() {
            if exc_matches(crate::types::exceptions::KeyboardInterrupt()) {
                clear_exc();
            } else {
                write_unraisable(Some(&format!("Exception ignored on calling {}.{}", module, func)), None);
            }
        }
    }
}

/// Flush a standard stream; returns false on failure (exception reported).
pub(crate) fn flush_std(name: &str) -> bool {
    if let Some(s) = crate::modules::sys::get_sys_attr(name) {
        if !s.is(none::none()) {
            if ab::call_method_str(&s, "flush", &[]).is_err() {
                write_unraisable(Some(&format!("Exception ignored on flushing sys.{}", name)), None);
                return false;
            }
        }
    }
    true
}

/// Interpreter shutdown. Returns false if flushing sys.stdout failed (exit status 120).
pub fn finalize() -> bool {
    crate::modules::sys_finalize::finalize()
}

pub fn populate(d: &PyObject) -> PyResult<()> {
    let c = config();
    let s = |v: &str| pystr::new_str(v);
    let set = |k: &str, v: &PyObject| dict::set_item_str(d, k, v);
    let strlist = |v: &[String]| list::new_list(v.iter().map(|x| pystr::new_str(x)).collect());
    set("version", &s(&version_string()))?;
    set("hexversion", &int::new_int(HEXVERSION))?;
    let vi = make_version_info()?;
    set("version_info", &vi)?;
    set("api_version", &int::new_int(1013))?;
    set("platform", &s("linux"))?;
    set("byteorder", &s("little"))?;
    set("maxsize", &int::new_int(i64::MAX))?;
    set("maxunicode", &int::new_int(0x10ffff))?;
    set("abiflags", &s("t"))?;
    let pc = paths();
    set("executable", &s(&exe_path()))?;
    set("_base_executable", &s(&pc.base_executable))?;
    set("prefix", &s(&pc.prefix))?;
    set("exec_prefix", &s(&pc.exec_prefix))?;
    set("base_prefix", &s(&pc.base_prefix))?;
    set("base_exec_prefix", &s(&pc.base_exec_prefix))?;
    set("platlibdir", &s(&std::env::var("PYTHONPLATLIBDIR").ok().filter(|_| c.ignore_environment == 0).unwrap_or_else(|| "lib".into())))?;
    set("_stdlib_dir", &s(&stdlib_dir()))?;
    set("pycache_prefix", &c.pycache_prefix.as_deref().map(s).unwrap_or_else(none::none_ref))?;
    set("dont_write_bytecode", &bool_::new_bool(c.dont_write_bytecode != 0))?;
    set("warnoptions", &strlist(&c.warnoptions))?;
    let xo = dict::new_dict();
    for x in &c.xoptions {
        match x.find('=') {
            Some(i) => dict::set_item_str(&xo, &x[..i], &s(&x[i + 1..]))?,
            None => dict::set_item_str(&xo, x, bool_::true_obj())?,
        }
    }
    set("_xoptions", &xo)?;
    if !c.orig_argv.is_empty() {
        set("orig_argv", &strlist(&c.orig_argv))?;
    }
    set("copyright", &s("Copyright (c) 2001 Python Software Foundation.\nAll Rights Reserved.\n\nCopyright (c) 2000 BeOpen.com.\nAll Rights Reserved.\n\nCopyright (c) 1995-2001 Corporation for National Research Initiatives.\nAll Rights Reserved.\n\nCopyright (c) 1991-1995 Stichting Mathematisch Centrum, Amsterdam.\nAll Rights Reserved."))?;
    set("float_repr_style", &s("short"))?;
    set("_git", &tuple::new_tuple(vec![s("CPython"), s("v3.15.2"), s("")]))?;
    set("_framework", &s(""))?;
    set("builtin_module_names", &tuple::new_tuple(crate::import::builtin_module_names().iter().map(|n| s(n)).collect()))?;
    set(
        "stdlib_module_names",
        &*crate::types::set::new_frozenset_from_vec(STDLIB_MODULE_NAMES.iter().map(|n| s(n)).collect())?,
    )?;
    set("flags", &*make_flags()?)?;
    // float_info
    let fi = ss::make_type(
        "sys.float_info",
        &["max", "max_exp", "max_10_exp", "min", "min_exp", "min_10_exp", "dig", "mant_dig", "epsilon", "radix", "rounds"],
        11,
        false,
    )?;
    let fl = |v: f64| float::new_float(v);
    let i = |v: i64| int::new_int(v);
    set(
        "float_info",
        &ss::new_instance(
            fi,
            vec![fl(f64::MAX), i(1024), i(308), fl(f64::MIN_POSITIVE), i(-1021), i(-307), i(15), i(53), fl(f64::EPSILON), i(2), i(1)],
        ),
    )?;
    let ii = ss::make_type(
        "sys.int_info",
        &["bits_per_digit", "sizeof_digit", "default_max_str_digits", "str_digits_check_threshold"],
        4,
        false,
    )?;
    set("int_info", &ss::new_instance(ii, vec![i(30), i(4), i(4300), i(640)]))?;
    let hi = ss::make_type(
        "sys.hash_info",
        &["width", "modulus", "inf", "nan", "imag", "algorithm", "hash_bits", "seed_bits", "cutoff"],
        9,
        false,
    )?;
    set(
        "hash_info",
        &ss::new_instance(
            hi,
            vec![i(64), i((1i64 << 61) - 1), i(314159), i(0), i(1000003), s("siphash13"), i(64), i(128), i(0)],
        ),
    )?;
    let ti = ss::make_type("sys.thread_info", &["name", "lock", "version"], 3, false)?;
    set("thread_info", &ss::new_instance(ti, vec![s("pthread"), s("pymutex"), thread_lib_version()]))?;
    let ua = ss::make_type("UnraisableHookArgs", &["exc_type", "exc_value", "exc_traceback", "err_msg", "object"], 5, false)?;
    UNRAISABLE_ARGS_TYPE.store(ua, Release);
    set("UnraisableHookArgs", crate::types::typeobject::type_obj(ua))?;
    let ah = ss::make_type("asyncgen_hooks", &["firstiter", "finalizer"], 2, false)?;
    ASYNCGEN_HOOKS_TYPE.store(ah, Release);
    // implementation
    let impl_ns = crate::modules::sys_namespace::new_namespace(&[
        ("name", s("cpython")),
        ("cache_tag", s(CACHE_TAG)),
        ("version", vi.clone()),
        ("hexversion", int::new_int(HEXVERSION)),
        ("_multiarch", s(MULTIARCH)),
        ("supports_isolated_interpreters", bool_::new_bool(false)),
    ])?;
    set("implementation", &impl_ns)?;
    let abi = crate::modules::sys_namespace::new_namespace(&[
        ("pointer_bits", i(64)),
        ("free_threaded", bool_::new_bool(true)),
        ("debug", bool_::new_bool(false)),
        ("byteorder", s("little")),
    ])?;
    set("abi_info", &abi)?;
    // sys._jit
    let jit = crate::types::module::new_module("sys._jit")?;
    crate::builtins::add_methods_to_dict(
        &crate::types::module::module_dict(&jit),
        crate::mdefs![("is_available", NOARGS, jit_false), ("is_enabled", NOARGS, jit_false), ("is_active", NOARGS, jit_false)],
        Some(&jit),
    )?;
    set("_jit", &jit)?;
    set("lazy_modules", &dict::new_dict())?;
    let methods = crate::mdefs![
        ("exit", FAST, sys_exit, "exit(status=None, /)\n--\n\nLeave the interpreter by raising SystemExit(status).\n\nNone or no argument means success (exit code 0); an int is used as the\nexit code; any other object is printed to stderr and gives exit code 1."),
        ("_getframe", FAST, sys_getframe),
        ("_getframemodulename", FASTKW, sys_getframemodulename),
        ("exception", NOARGS, sys_exception),
        ("exc_info", NOARGS, sys_exc_info),
        ("getrecursionlimit", NOARGS, sys_getrecursionlimit),
        ("setrecursionlimit", O, sys_setrecursionlimit),
        ("intern", O, sys_intern),
        ("_is_interned", O, sys_is_interned),
        ("_is_immortal", O, sys_is_immortal),
        ("getrefcount", O, sys_getrefcount),
        ("displayhook", O, sys_displayhook),
        ("excepthook", FAST, sys_excepthook, "excepthook(exctype, value, traceback, /)\n--\n\nPrint an uncaught exception and its traceback to sys.stderr."),
        ("breakpointhook", FASTKW, sys_breakpointhook, "breakpointhook(*args, **kws)\n--\n\nCalled by breakpoint(); starts the debugger named by PYTHONBREAKPOINT\n(pdb by default) and passes the arguments on to it."),
        ("unraisablehook", O, sys_unraisablehook),
        ("getdefaultencoding", NOARGS, sys_getdefaultencoding),
        ("getfilesystemencoding", NOARGS, sys_getfilesystemencoding),
        ("getfilesystemencodeerrors", NOARGS, sys_getfilesystemencodeerrors),
        ("_is_gil_enabled", NOARGS, sys_is_gil_enabled),
        ("settrace", O, sys_settrace),
        ("gettrace", NOARGS, sys_gettrace),
        ("setprofile", O, sys_setprofile),
        ("getprofile", NOARGS, sys_getprofile),
        ("_settraceallthreads", O, sys_settraceallthreads),
        ("_setprofileallthreads", O, sys_setprofileallthreads),
        ("call_tracing", FAST, sys_call_tracing, "call_tracing(func, args, /)\n--\n\nCall func(*args) with tracing turned on, even inside a trace function."),
        ("getsizeof", FASTKW, sys_getsizeof),
        ("audit", FAST, sys_audit, "audit(event, /, *args)\n--\n\nPass an auditing event and its arguments to every registered audit hook."),
        ("addaudithook", FASTKW, sys_addaudithook, "addaudithook(hook)\n--\n\nRegister hook(event, args) to be called for every auditing event."),
        ("get_int_max_str_digits", NOARGS, sys_get_int_max_str_digits),
        ("set_int_max_str_digits", FASTKW, sys_set_int_max_str_digits, "set_int_max_str_digits(maxdigits)\n--\n\nSet the largest number of digits allowed in int/str conversions\n(0 removes the limit)."),
        ("getswitchinterval", NOARGS, sys_getswitchinterval),
        ("setswitchinterval", O, sys_setswitchinterval),
        ("is_finalizing", NOARGS, sys_is_finalizing),
        ("_clear_type_cache", NOARGS, sys_clear_type_cache),
        ("_clear_internal_caches", NOARGS, sys_clear_internal_caches),
        ("_current_frames", NOARGS, sys_current_frames),
        ("_current_exceptions", NOARGS, sys_current_exceptions),
        ("activate_stack_trampoline", FASTKW, sys_activate_stack_trampoline, "activate_stack_trampoline(backend, /)\n--\n\nTurn on the perf profiler trampoline for the given backend."),
        ("deactivate_stack_trampoline", NOARGS, sys_deactivate_stack_trampoline),
        ("is_stack_trampoline_active", NOARGS, sys_is_stack_trampoline_active),
        ("is_remote_debug_enabled", NOARGS, sys_is_remote_debug_enabled),
        ("remote_exec", FAST, sys_remote_exec, "remote_exec(pid, script, /)\n--\n\nAsk another Python process to run the script file at its next safe point."),
        ("getallocatedblocks", NOARGS, sys_getallocatedblocks),
        ("getunicodeinternedsize", FASTKW, sys_getunicodeinternedsize, "getunicodeinternedsize(*, _only_immortal=False)\n--\n\nReturn how many strings are interned."),
        ("_debugmallocstats", NOARGS, sys_debugmallocstats),
        ("get_coroutine_origin_tracking_depth", NOARGS, sys_get_coroutine_origin_tracking_depth),
        ("set_coroutine_origin_tracking_depth", FASTKW, sys_set_coroutine_origin_tracking_depth, "set_coroutine_origin_tracking_depth(depth)\n--\n\nRecord this many frames of creation context for new coroutines\n(0 disables the tracking)."),
        ("get_asyncgen_hooks", NOARGS, sys_get_asyncgen_hooks),
        ("set_asyncgen_hooks", FASTKW, sys_set_asyncgen_hooks),
        ("getdlopenflags", NOARGS, sys_getdlopenflags),
        ("setdlopenflags", O, sys_setdlopenflags),
        ("set_lazy_imports", FASTKW, sys_set_lazy_imports, "set_lazy_imports(mode)\n--\n\nChoose how lazy imports behave: \"normal\", \"all\" or \"none\"."),
        ("get_lazy_imports", NOARGS, sys_get_lazy_imports),
        ("set_lazy_imports_filter", FASTKW, sys_set_lazy_imports_filter, "set_lazy_imports_filter(filter)\n--\n\nInstall a callable deciding which imports may be lazy (None removes it)."),
        ("get_lazy_imports_filter", NOARGS, sys_get_lazy_imports_filter),
    ];
    crate::builtins::add_methods_to_dict(d, methods, None)?;
    for (orig, k) in [
        ("__displayhook__", "displayhook"),
        ("__excepthook__", "excepthook"),
        ("__breakpointhook__", "breakpointhook"),
        ("__unraisablehook__", "unraisablehook"),
    ] {
        if let Some(h) = dict::get_item_rs(d, k) {
            set(orig, &h)?;
        }
    }
    crate::modules::sys_trace_api::install_monitoring(d)?;
    crate::types::lazyimport::install_sys(d)?;
    Ok(())
}

/// PyImport_SetLazyImportsMode: true = "all", false = "normal".
pub fn set_lazy_imports_all(all: bool) {
    *LAZY_MODE.lock().unwrap() = Some(if all { 1 } else { 2 });
}
/// PyImport_SetLazyImportsFilter (None/NULL clears the filter).
pub fn set_lazy_imports_filter_obj(f: Option<&PyObject>) {
    let nv = match f {
        Some(f) if !f.is(none::none()) => f.to_ref().into_raw(),
        _ => std::ptr::null_mut(),
    };
    let old = LAZY_FILTER.swap(nv, AcqRel);
    unsafe { xdecref(old) };
}
