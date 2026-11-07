//! The `_interpreters` module: the low-level interface to subinterpreters that
//! `concurrent.interpreters` builds on.

use super::xidata::{self, Captured, Mode, Xi};
use super::{Config, Interp};
use crate::abstract_ as ab;
use crate::args::{self, Params};
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::{bool_, dict, int, list, module as pymodule, none, str as pystr, tuple};
use crate::{mdefs, raise};
use std::sync::atomic::{AtomicPtr, Ordering::*};

// --------------------------------------------------------------------- exception types

static INTERPRETER_ERROR: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());
static NOT_FOUND_ERROR: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());
static NOT_SHAREABLE_ERROR: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());

/// The exception classes are created once, in the main interpreter, and shared
/// by all interpreters (like static types: nothing in them refers to the state
/// of a particular interpreter).
fn ensure_types() {
    if !NOT_SHAREABLE_ERROR.load(Acquire).is_null() {
        return;
    }
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = LOCK.lock().unwrap();
    if !NOT_SHAREABLE_ERROR.load(Acquire).is_null() {
        return;
    }
    super::with_interp(super::main(), || {
        let saved = take_exc();
        let mk = |name: &str, base: *mut PyTypeObject, doc: &str| -> *mut PyTypeObject {
            let t = crate::modules::devtools::new_exception("concurrent.interpreters", name, base).expect("exception type");
            let _ = ab::setattr(&t, &pystr::new_str("__doc__"), &pystr::new_str(doc));
            clear_exc();
            t.into_raw() as *mut PyTypeObject
        };
        let ie = mk("InterpreterError", crate::types::exceptions::Exception(), "A cross-interpreter operation failed");
        let nf = mk("InterpreterNotFoundError", ie, "An interpreter was not found");
        let ns = mk("NotShareableError", crate::types::exceptions::TypeError(), "The object cannot be shared with another interpreter");
        INTERPRETER_ERROR.store(ie, Release);
        NOT_FOUND_ERROR.store(nf, Release);
        NOT_SHAREABLE_ERROR.store(ns, Release);
        if let Some(e) = saved {
            set_exc(e);
        }
    });
}

pub fn interpreter_error_type() -> *mut PyTypeObject {
    ensure_types();
    INTERPRETER_ERROR.load(Acquire)
}
pub fn not_found_error_type() -> *mut PyTypeObject {
    ensure_types();
    NOT_FOUND_ERROR.load(Acquire)
}
pub fn not_shareable_type() -> *mut PyTypeObject {
    ensure_types();
    NOT_SHAREABLE_ERROR.load(Acquire)
}

fn interp_error(msg: &str) -> PyErr {
    new_err(interpreter_error_type(), msg)
}

// --------------------------------------------------------------------- argument helpers

/// Convert an interpreter ID argument (an int, or anything with __index__).
pub fn id_arg(o: &PyObject) -> PyResult<i64> {
    if pystr::is_str(o) || crate::types::bytes::is_bytes(o) || crate::types::float::is_float(o) {
        raise!(TypeError, "interpreter ID must be an int, got {}", crate::types::typeobject::tname(o));
    }
    let i = match ab::index(o) {
        Ok(i) => i,
        Err(_) => {
            clear_exc();
            raise!(TypeError, "interpreter ID must be an int, got {}", crate::types::typeobject::tname(o));
        }
    };
    if int::sign(&i) < 0 {
        raise!(ValueError, "interpreter ID must be a non-negative int, got {}", ab::repr_string(&i)?);
    }
    match int::as_i64(&i) {
        Some(v) => Ok(v),
        None => raise!(OverflowError, "interpreter ID too large"),
    }
}

/// Look up an interpreter by ID argument; `restrict`: only interpreters this
/// module created.
pub fn lookup_arg(o: &PyObject, restrict: bool) -> PyResult<&'static Interp> {
    let id = id_arg(o)?;
    let Some(i) = super::lookup(id) else { return Err(super::not_found(id)) };
    if restrict && i.whence.load(Relaxed) != super::WHENCE_STDLIB {
        return Err(interp_error(&format!("unrecognized interpreter ID {}", id)));
    }
    Ok(i)
}

fn truth(o: Option<&PyObject>) -> PyResult<bool> {
    match o {
        Some(v) => ab::is_true(v),
        None => Ok(false),
    }
}

fn check_dict_arg(fname: &str, argname: &str, o: Option<&PyObject>) -> PyResult<Option<PyRef>> {
    match o {
        None => Ok(None),
        Some(d) if dict::is_dict(d) => Ok(Some(d.to_ref())),
        Some(d) => raise!(TypeError, "{}() argument '{}' must be dict, not {}", fname, argname, crate::types::typeobject::tname(d)),
    }
}

// --------------------------------------------------------------------- configs

fn gil_name(g: i32) -> &'static str {
    match g {
        1 => "shared",
        2 => "own",
        _ => "default",
    }
}

pub fn config_to_ns(c: &Config) -> PyResult {
    let b = |v: bool| bool_::new_bool(v);
    xidata::namespace(&[
        ("use_main_obmalloc", b(c.use_main_obmalloc)),
        ("allow_fork", b(c.allow_fork)),
        ("allow_exec", b(c.allow_exec)),
        ("allow_threads", b(c.allow_threads)),
        ("allow_daemon_threads", b(c.allow_daemon_threads)),
        ("check_multi_interp_extensions", b(c.check_multi_interp_extensions)),
        ("gil", pystr::new_str(gil_name(c.gil))),
    ])
}

const EMPTY: Config = Config {
    use_main_obmalloc: false,
    allow_fork: false,
    allow_exec: false,
    allow_threads: false,
    allow_daemon_threads: false,
    check_multi_interp_extensions: false,
    gil: 0,
};

fn named_config(name: &str) -> PyResult<Config> {
    Ok(match name {
        "" | "default" | "isolated" => Config::ISOLATED,
        "legacy" => Config::LEGACY,
        "empty" => EMPTY,
        _ => raise!(ValueError, "unsupported config name '{}'", name),
    })
}

const FIELDS: [&str; 7] = ["use_main_obmalloc", "allow_fork", "allow_exec", "allow_threads", "allow_daemon_threads", "check_multi_interp_extensions", "gil"];

fn set_field(c: &mut Config, k: &str, v: &PyObject) -> PyResult<()> {
    if k == "gil" {
        if !pystr::is_str(v) {
            raise!(TypeError, "config.gil must be a str, got {}", crate::types::typeobject::tname(v));
        }
        c.gil = match pystr::to_string_lossy(v).as_str() {
            "default" => 0,
            "shared" => 1,
            "own" => 2,
            other => raise!(ValueError, "unsupported config.gil value '{}'", other),
        };
        return Ok(());
    }
    if !v.is(bool_::true_obj()) && !v.is(bool_::false_obj()) {
        raise!(TypeError, "config.{} must be a bool, got {}", k, crate::types::typeobject::tname(v));
    }
    let b = v.is(bool_::true_obj());
    match k {
        "use_main_obmalloc" => c.use_main_obmalloc = b,
        "allow_fork" => c.allow_fork = b,
        "allow_exec" => c.allow_exec = b,
        "allow_threads" => c.allow_threads = b,
        "allow_daemon_threads" => c.allow_daemon_threads = b,
        "check_multi_interp_extensions" => c.check_multi_interp_extensions = b,
        _ => raise!(ValueError, "unsupported config field '{}'", k),
    }
    Ok(())
}

/// A config object (namespace with exactly the config fields; `gil` may also
/// be an int as in PyInterpreterConfig), a name, or None.
pub fn config_arg(o: Option<&PyObject>) -> PyResult<Config> {
    let Some(o) = o.filter(|o| !o.is(none::none())) else { return Ok(Config::ISOLATED) };
    if pystr::is_str(o) {
        // the name must be encodable (no lone surrogates)
        pystr::as_utf8(o)?;
        return named_config(&pystr::to_string_lossy(o));
    }
    let d = match ab::getattr_opt(o, &pystr::new_str("__dict__"))? {
        Some(d) if dict::is_dict(&d) => d,
        _ => raise!(TypeError, "bad config {}", ab::repr_string(o)?),
    };
    let mut c = EMPTY;
    let mut seen = 0;
    for (k, v) in dict::items_vec(&d) {
        let ks = pystr::to_string_lossy(&k);
        if !FIELDS.contains(&ks.as_str()) {
            raise!(ValueError, "unsupported config field '{}'", ks);
        }
        seen += 1;
        if ks == "gil" && int::is_int(&v) && !v.is(bool_::true_obj()) && !v.is(bool_::false_obj()) {
            c.gil = int::as_i64(&v).unwrap_or(-1) as i32;
            continue;
        }
        if ks != "gil" && int::is_int(&v) && !v.is(bool_::true_obj()) && !v.is(bool_::false_obj()) {
            set_bool(&mut c, &ks, int::as_i64(&v).unwrap_or(0) != 0);
            continue;
        }
        set_field(&mut c, &ks, &v)?;
    }
    if seen != FIELDS.len() {
        let missing: Vec<&str> = FIELDS.iter().copied().filter(|f| dict::get_item_rs(&d, f).is_none()).collect();
        raise!(ValueError, "missing config fields {:?}", missing);
    }
    Ok(c)
}

fn set_bool(c: &mut Config, k: &str, b: bool) {
    match k {
        "use_main_obmalloc" => c.use_main_obmalloc = b,
        "allow_fork" => c.allow_fork = b,
        "allow_exec" => c.allow_exec = b,
        "allow_threads" => c.allow_threads = b,
        "allow_daemon_threads" => c.allow_daemon_threads = b,
        "check_multi_interp_extensions" => c.check_multi_interp_extensions = b,
        _ => {}
    }
}

fn m_new_config(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let (pos, kws) = args::split(args, kw);
    if pos.len() > 1 {
        raise!(TypeError, "new_config() takes at most 1 positional argument ({} given)", pos.len());
    }
    let mut c = match pos.first() {
        Some(n) if !n.is(none::none()) => {
            if !pystr::is_str(n) {
                raise!(TypeError, "new_config() argument 1 must be str, not {}", crate::types::typeobject::tname(n));
            }
            named_config(&pystr::to_string_lossy(n))?
        }
        _ => Config::ISOLATED,
    };
    for (k, v) in kws {
        let ks = pystr::to_string_lossy(k);
        if !FIELDS.contains(&ks.as_str()) {
            raise!(ValueError, "unsupported config field '{}'", ks);
        }
        set_field(&mut c, &ks, v)?;
    }
    config_to_ns(&c)
}

// --------------------------------------------------------------------- lifecycle

fn whence_tuple(i: &Interp) -> PyRef {
    tuple::new_tuple(vec![int::new_int(i.id), int::new_int(i.whence.load(Relaxed) as i64)])
}

/// Create an interpreter, initialise it and come back; returns it.
pub fn create_interp(config: Config, whence: i32) -> PyResult<&'static Interp> {
    let (ts, prev) = super::create(config, whence)?;
    let i = super::of_ts(ts);
    // the initial thread state is not needed: code runs in fresh ones
    super::leave_created(ts, prev);
    Ok(i)
}

fn m_create(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("create", &["config", "reqrefs"], 0, 0).kwonly(1);
    let [cfg, reqrefs] = args::parse::<2>(&P, args, kw)?;
    let config = config_arg(cfg)?;
    let reqrefs = truth(reqrefs)?;
    let i = create_interp(config, super::WHENCE_STDLIB)?;
    if reqrefs {
        i.requires_idref.store(true, Relaxed);
    }
    Ok(int::new_int(i.id))
}

fn m_destroy(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("destroy", &["id", "restrict"], 0, 1).kwonly(1);
    let [id, restrict] = args::parse::<2>(&P, args, kw)?;
    let i = lookup_arg(id.unwrap(), truth(restrict)?)?;
    super::destroy(i)?;
    Ok(none::none_ref())
}

fn m_list_all(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("list_all", &["require_ready"], 0, 0).kwonly(1);
    let [ready] = args::parse::<1>(&P, args, kw)?;
    let ready = truth(ready)?;
    let items: Vec<PyRef> = super::list_all().into_iter().filter(|i| !ready || i.ready.load(Acquire) || i.is_main()).map(whence_tuple).collect();
    Ok(list::new_list(items))
}

fn m_get_current(_m: &PyObject) -> PyResult {
    Ok(whence_tuple(super::current()))
}

fn m_get_main(_m: &PyObject) -> PyResult {
    Ok(whence_tuple(super::main()))
}

fn m_whence(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("whence", &["id"], 0, 1);
    let [id] = args::parse::<1>(&P, args, kw)?;
    let i = lookup_arg(id.unwrap(), false)?;
    Ok(int::new_int(i.whence.load(Relaxed) as i64))
}

fn m_get_config(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("get_config", &["id", "restrict"], 0, 1).kwonly(1);
    let [id, restrict] = args::parse::<2>(&P, args, kw)?;
    let i = lookup_arg(id.unwrap(), truth(restrict)?)?;
    config_to_ns(&i.config)
}

fn m_is_running(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("is_running", &["id", "restrict"], 0, 1).kwonly(1);
    let [id, restrict] = args::parse::<2>(&P, args, kw)?;
    let i = lookup_arg(id.unwrap(), truth(restrict)?)?;
    Ok(bool_::new_bool(i.is_running()))
}

fn m_incref(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("incref", &["id", "implieslink", "restrict"], 0, 1).kwonly(2);
    let [id, implieslink, restrict] = args::parse::<3>(&P, args, kw)?;
    let i = lookup_arg(id.unwrap(), truth(restrict)?)?;
    if truth(implieslink)? {
        i.requires_idref.store(true, Relaxed);
    }
    i.refs.fetch_add(1, AcqRel);
    Ok(none::none_ref())
}

fn m_decref(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("decref", &["id", "restrict"], 0, 1).kwonly(1);
    let [id, restrict] = args::parse::<2>(&P, args, kw)?;
    let i = lookup_arg(id.unwrap(), truth(restrict)?)?;
    decref_interp(i);
    Ok(none::none_ref())
}

/// Drop one reference; an interpreter that requires references goes away with
/// the last one (unless it is busy, the main one or the current one).
pub fn decref_interp(i: &'static Interp) {
    let before = i.refs.fetch_sub(1, AcqRel);
    if before <= 0 {
        i.refs.fetch_add(1, AcqRel);
        return;
    }
    if before == 1 && i.requires_idref.load(Relaxed) && !i.is_main() && !std::ptr::eq(super::current(), i) && i.running.load(Acquire) == 0 {
        if super::destroy(i).is_err() {
            clear_exc();
        }
    }
}

// --------------------------------------------------------------------- running code

/// Runs `body` in `i` as its `__main__` code: claims the interpreter (it must
/// not be running already), enters it, and comes back.  `body` runs with the
/// target interpreter current and returns data that outlives the switch.
fn run_in<R>(i: &'static Interp, body: impl FnOnce() -> R) -> PyResult<R> {
    if !i.alive.load(Acquire) {
        return Err(super::not_found(i.id));
    }
    super::claim_running(i)?;
    let r = super::with_interp(i, body);
    super::release_running(i);
    Ok(r)
}

/// Result of running something in another interpreter.
enum Outcome<T> {
    Ok(T),
    /// the code raised: excinfo
    Failed(Captured),
    /// setting things up in the target failed (e.g. unpickling)
    Setup(Captured),
}

fn exec_code_data(code: &[u8], shared: &[(String, Xi)]) -> Outcome<()> {
    let main = match super::main_module() {
        Ok(m) => m,
        Err(_) => return Outcome::Setup(Captured::take_pending(false, false)),
    };
    let ns = pymodule::module_dict(&main);
    if xidata::apply_shared(&ns, shared).is_err() {
        return Outcome::Setup(Captured::take_pending(false, false));
    }
    let r = crate::modules::marshal::loads_slice(code, true).and_then(|c| crate::interp::eval_code(&c, &ns, Some(&ns)));
    match r {
        Ok(_) => Outcome::Ok(()),
        Err(_) => Outcome::Failed(Captured::take_pending(true, false)),
    }
}

fn exec_common(fname: &str, args: &[&PyObject], kw: Option<&PyObject>, names: &'static [&'static str]) -> PyResult {
    let p = Params::new(fname, names, 0, 2).kwonly(1);
    let [id, code, shared, restrict] = args::parse::<4>(&p, args, kw)?;
    let shared = check_dict_arg(fname, "shared", shared)?;
    let i = lookup_arg(id.unwrap(), truth(restrict)?)?;
    let code = code.unwrap();
    let data = match fname {
        "run_string" => {
            if !pystr::is_str(code) {
                raise!(TypeError, "run_string() argument 'script' must be str, not {}", crate::types::typeobject::tname(code));
            }
            xidata::script_code(code, "script")?
        }
        "run_func" => {
            if !crate::types::funcobject::is_function(code) && !crate::types::code::is_code(code) {
                raise!(TypeError, "run_func() argument 'func' must be a function, not {}", crate::types::typeobject::tname(code));
            }
            xidata::script_code(code, "func")?
        }
        _ => {
            if crate::types::bytes::is_bytes(code) || !(pystr::is_str(code) || crate::types::funcobject::is_function(code) || crate::types::code::is_code(code)) {
                raise!(TypeError, "exec() argument 'code' must be a string, a function or a code object, not {}", crate::types::typeobject::tname(code));
            }
            xidata::script_code(code, "code")?
        }
    };
    let shared_xi = match &shared {
        Some(d) => xidata::shared_dict(d)?,
        None => Vec::new(),
    };
    match run_in(i, || exec_code_data(&data, &shared_xi))? {
        Outcome::Ok(()) => Ok(none::none_ref()),
        Outcome::Failed(c) => c.to_namespace(),
        Outcome::Setup(c) => Err(c.raise()),
    }
}

fn m_exec(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    exec_common("exec", args, kw, &["id", "code", "shared", "restrict"])
}
fn m_run_string(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    exec_common("run_string", args, kw, &["id", "script", "shared", "restrict"])
}
fn m_run_func(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    exec_common("run_func", args, kw, &["id", "func", "shared", "restrict"])
}

fn m_set_main_attrs(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("set___main___attrs", &["id", "updates", "restrict"], 0, 2).kwonly(1);
    let [id, updates, restrict] = args::parse::<3>(&P, args, kw)?;
    let updates = check_dict_arg("set___main___attrs", "updates", updates)?.unwrap();
    let i = lookup_arg(id.unwrap(), truth(restrict)?)?;
    let shared = xidata::shared_dict(&updates)?;
    if i.running.load(Acquire) != 0 {
        return Err(interp_error("interpreter running"));
    }
    let r = super::with_interp(i, || -> Result<(), Captured> {
        let main = super::main_module().map_err(|_| Captured::take_pending(false, false))?;
        let ns = pymodule::module_dict(&main);
        xidata::apply_shared(&ns, &shared).map_err(|_| Captured::take_pending(false, false))
    });
    match r {
        Ok(()) => Ok(none::none_ref()),
        Err(c) => Err(c.raise()),
    }
}

fn m_call(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("call", &["id", "callable", "args", "kwargs", "preserve_exc", "restrict"], 0, 2).kwonly(2);
    let [id, callable, cargs, ckwargs, preserve, restrict] = args::parse::<6>(&P, args, kw)?;
    let i = lookup_arg(id.unwrap(), truth(restrict)?)?;
    let preserve = truth(preserve)?;
    let callable = callable.unwrap();
    // everything the call needs, as data
    let f = xidata::get(callable, Mode::Fallback).map_err(|_| xidata::not_shareable("callable not shareable", true))?;
    let mut pos = Vec::new();
    if let Some(a) = cargs.filter(|a| !a.is(none::none())) {
        if !tuple::is_tuple(a) {
            raise!(TypeError, "call() argument 'args' must be tuple, not {}", crate::types::typeobject::tname(a));
        }
        for it in tuple::tuple_items(a) {
            pos.push(xidata::get(it, Mode::Fallback).map_err(|_| xidata::not_shareable("args not shareable", true))?);
        }
    }
    let mut kws = Vec::new();
    if let Some(k) = ckwargs.filter(|k| !k.is(none::none())) {
        if !dict::is_dict(k) {
            raise!(TypeError, "call() argument 'kwargs' must be dict, not {}", crate::types::typeobject::tname(k));
        }
        for (key, v) in dict::items_vec(k) {
            let ks = pystr::as_utf8(&key)?.to_string();
            kws.push((ks, xidata::get(&v, Mode::Fallback).map_err(|_| xidata::not_shareable("args not shareable", true))?));
        }
    }
    let outcome = run_in(i, || -> Outcome<Xi> {
        let setup = (|| -> PyResult<(PyRef, Vec<PyRef>, PyRef)> {
            let f = f.to_object()?;
            let mut a = Vec::new();
            for x in &pos {
                a.push(x.to_object()?);
            }
            let d = dict::new_dict();
            for (k, x) in &kws {
                dict::set_item_str(&d, k, &*x.to_object()?)?;
            }
            Ok((f, a, d))
        })();
        let (f, a, d) = match setup {
            Ok(t) => t,
            Err(_) => return Outcome::Setup(Captured::take_pending(false, false)),
        };
        let refs: Vec<&PyObject> = a.iter().map(|x| &**x).collect();
        let kwd = if dict::dict_len(&d) > 0 { Some(&*d) } else { None };
        match ab::call_with_kwargs_dict(&f, &refs, kwd) {
            Ok(r) => match xidata::get(&r, Mode::Fallback) {
                Ok(x) => Outcome::Ok(x),
                Err(_) => Outcome::Setup(Captured::take_pending(false, false)),
            },
            Err(_) => Outcome::Failed(Captured::take_pending(true, preserve)),
        }
    })?;
    match outcome {
        Outcome::Ok(x) => {
            let r = x.to_object()?;
            Ok(tuple::new_tuple(vec![r, none::none_ref()]))
        }
        Outcome::Failed(c) => {
            let ns = c.to_namespace()?;
            if preserve {
                let exc = match &c.pickled {
                    Some(p) => match (Xi::Pickled { data: p.clone(), main_file: None }).to_object() {
                        Ok(e) => e,
                        Err(_) => {
                            clear_exc();
                            none::none_ref()
                        }
                    },
                    None => none::none_ref(),
                };
                ab::setattr(&ns, &pystr::new_str("exc"), &exc)?;
            }
            Ok(tuple::new_tuple(vec![none::none_ref(), ns]))
        }
        Outcome::Setup(c) => {
            if c.name == "NotShareableError" && c.msg.as_deref().is_some_and(|m| m.contains("could not be pickled")) {
                // the return value could not be sent back
                Err(xidata::not_shareable_from("return value not shareable", &c))
            } else {
                Err(xidata::not_shareable_from("args not shareable", &c))
            }
        }
    }
}

// --------------------------------------------------------------------- misc

fn m_is_shareable(_m: &PyObject, o: &PyObject) -> PyResult {
    match xidata::get(o, Mode::Default) {
        Ok(_) => Ok(bool_::new_bool(true)),
        Err(_) => {
            clear_exc();
            Ok(bool_::new_bool(false))
        }
    }
}

fn m_capture_exception(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("capture_exception", &["exc"], 0, 0);
    let [exc] = args::parse::<1>(&P, args, kw)?;
    let e = match exc.filter(|e| !e.is(none::none())) {
        Some(e) => e.to_ref(),
        None => {
            let sys = crate::modules::sys::sys_module();
            let cur = ab::call(&*ab::getattr_str(&sys, "exception")?, &[])?;
            if cur.is(none::none()) {
                return Ok(none::none_ref());
            }
            cur
        }
    };
    if !crate::types::typeobject::is_subtype(e.type_ptr(), crate::types::exceptions::BaseException()) {
        raise!(TypeError, "expected exception, got {}", ab::repr_string(&e)?);
    }
    Captured::from_exc(&e, true, false).to_namespace()
}

pub fn init_module() -> PyResult {
    let methods = mdefs![
        ("new_config", FASTKW, m_new_config, "new_config(name='isolated', /, **overides) -> type.SimpleNamespace\n\nReturn a representation of a new PyInterpreterConfig."),
        ("create", FASTKW, m_create, "create([config], *, reqrefs=False) -> ID\n\nCreate a new interpreter and return a unique generated ID."),
        ("destroy", FASTKW, m_destroy, "destroy(id, *, restrict=False)\n\nDestroy the identified interpreter."),
        ("list_all", FASTKW, m_list_all, "list_all() -> [(ID, whence)]\n\nReturn a list containing the ID of every existing interpreter."),
        ("get_current", NOARGS, m_get_current, "get_current() -> (ID, whence)\n\nReturn the ID of current interpreter."),
        ("get_main", NOARGS, m_get_main, "get_main() -> (ID, whence)\n\nReturn the ID of main interpreter."),
        ("set___main___attrs", FASTKW, m_set_main_attrs, "set___main___attrs(id, ns, *, restrict=False)\n\nBind the given attributes in the interpreter's __main__ module."),
        ("exec", FASTKW, m_exec, "exec(id, code, shared=None, *, restrict=False)\n\nExecute the provided code in the identified interpreter."),
        ("call", FASTKW, m_call, "call(id, callable, args=None, kwargs=None, *, restrict=False)\n\nCall the provided object in the identified interpreter."),
        ("run_string", FASTKW, m_run_string, "run_string(id, script, shared=None, *, restrict=False)\n\nExecute the provided string in the identified interpreter."),
        ("run_func", FASTKW, m_run_func, "run_func(id, func, shared=None, *, restrict=False)\n\nExecute the body of the provided function in the identified interpreter."),
        ("is_shareable", O, m_is_shareable, "is_shareable(obj) -> bool\n\nReturn True if the object's data may be shared between interpreters."),
        ("is_running", FASTKW, m_is_running, "is_running(id, *, restrict=False) -> bool\n\nReturn whether or not the identified interpreter is running."),
        ("get_config", FASTKW, m_get_config, "get_config(id, *, restrict=False) -> types.SimpleNamespace\n\nReturn a representation of the config used to initialize the interpreter."),
        ("whence", FASTKW, m_whence, "whence(id) -> int\n\nReturn an identifier for where the interpreter was created."),
        ("incref", FASTKW, m_incref),
        ("decref", FASTKW, m_decref),
        ("capture_exception", FASTKW, m_capture_exception, "capture_exception(exc=None) -> types.SimpleNamespace\n\nReturn a snapshot of an exception."),
    ];
    let (m, d) = crate::modules::fncommon::new_module("_interpreters", methods, Some("This module provides primitive operations to manage Python interpreters.\nThe 'interpreters' module provides a more convenient interface."))?;
    dict::set_item_str(&d, "InterpreterError", crate::types::typeobject::type_obj(interpreter_error_type()))?;
    dict::set_item_str(&d, "InterpreterNotFoundError", crate::types::typeobject::type_obj(not_found_error_type()))?;
    dict::set_item_str(&d, "NotShareableError", crate::types::typeobject::type_obj(not_shareable_type()))?;
    for (n, v) in [
        ("WHENCE_UNKNOWN", super::WHENCE_UNKNOWN),
        ("WHENCE_RUNTIME", super::WHENCE_RUNTIME),
        ("WHENCE_LEGACY_CAPI", super::WHENCE_LEGACY_CAPI),
        ("WHENCE_CAPI", super::WHENCE_CAPI),
        ("WHENCE_XI", super::WHENCE_XI),
        ("WHENCE_STDLIB", super::WHENCE_STDLIB),
    ] {
        dict::set_item_str(&d, n, &int::new_int(v as i64))?;
    }
    Ok(m)
}

/// Register `_interpreters`, `_interpqueues` and `_interpchannels`.
pub fn register() {
    crate::import::register_builtin("_interpreters", init_module);
    crate::import::register_builtin("_interpqueues", super::queues::init_module);
    crate::import::register_builtin("_interpchannels", super::channels::init_module);
}
