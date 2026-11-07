//! The `_warnings` accelerator module: warn()/warn_explicit() implemented in
//! Rust, sharing state (filters, onceregistry, defaultaction, the filters
//! version and a recursive lock) with the pure-Python `warnings` module.
//!
//! As in CPython, the Python-level `warnings` module (when imported) is the
//! source of truth for `filters` (via `_get_filters()`), `onceregistry`,
//! `defaultaction` and for displaying warnings (`_showwarnmsg`); the internal
//! copies are used before it is imported or when attributes are missing.
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::{dict, int, list, none, str as pystr, tuple};
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};

static FILTERS: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
static ONCEREGISTRY: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
static DEFAULTACTION: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
static FILTERS_VERSION: AtomicU64 = AtomicU64::new(1);

// ---------------------------------------------------------------- the lock

struct RLock {
    state: Mutex<(usize, usize)>, // (owner thread id, recursion count)
    cv: Condvar,
}

static LOCK: RLock = RLock { state: Mutex::new((0, 0)), cv: Condvar::new() };

fn lock_acquire() {
    let me = crate::object::thread_id();
    let mut g = LOCK.state.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if g.1 == 0 {
            *g = (me, 1);
            return;
        }
        if g.0 == me {
            g.1 += 1;
            return;
        }
        g = LOCK.cv.wait(g).unwrap_or_else(|e| e.into_inner());
    }
}

fn lock_release() -> bool {
    let me = crate::object::thread_id();
    let mut g = LOCK.state.lock().unwrap_or_else(|e| e.into_inner());
    if g.1 == 0 || g.0 != me {
        return false;
    }
    g.1 -= 1;
    if g.1 == 0 {
        g.0 = 0;
        LOCK.cv.notify_one();
    }
    true
}

struct LockGuard;
impl LockGuard {
    fn new() -> LockGuard {
        lock_acquire();
        LockGuard
    }
}
impl Drop for LockGuard {
    fn drop(&mut self) {
        lock_release();
    }
}

// ---------------------------------------------------------------- state

fn state_obj(p: &AtomicPtr<PyObject>) -> PyRef {
    let o = p.load(Ordering::Acquire);
    unsafe { PyRef::from_borrowed(o) }
}

fn init_state() {
    if !FILTERS.load(Ordering::Acquire).is_null() {
        return;
    }
    let filters = list::new_list_empty();
    let none = none::none();
    let mk = |action: &str, cat: *mut PyTypeObject, module: Option<&str>| -> PyRef {
        tuple::new_tuple(vec![
            pystr::new_str(action),
            none.to_ref(),
            crate::types::typeobject::type_ref(cat),
            module.map(pystr::new_str).unwrap_or_else(none::none_ref),
            int::new_int(0),
        ])
    };
    // Default filters of a release build.
    list::append(&filters, mk("default", crate::exc_type!(DeprecationWarning), Some("__main__")));
    list::append(&filters, mk("ignore", crate::exc_type!(DeprecationWarning), None));
    list::append(&filters, mk("ignore", crate::exc_type!(PendingDeprecationWarning), None));
    list::append(&filters, mk("ignore", crate::exc_type!(ImportWarning), None));
    list::append(&filters, mk("ignore", crate::exc_type!(ResourceWarning), None));
    FILTERS.store(filters.into_raw(), Ordering::Release);
    ONCEREGISTRY.store(dict::new_dict().into_raw(), Ordering::Release);
    DEFAULTACTION.store(pystr::new_str("default").into_raw(), Ordering::Release);
}

fn warnings_module(try_import: bool) -> Option<PyRef> {
    if let Some(m) = crate::import::get_loaded_module("warnings") {
        if !m.is(none::none()) {
            return Some(m);
        }
        return None;
    }
    if try_import {
        match crate::import::import_absolute("warnings") {
            Ok(m) => return Some(m),
            Err(_) => clear_exc(),
        }
    }
    None
}

/// Look up `attr` on the warnings module, None if missing.
fn warnings_attr(attr: &str, try_import: bool) -> PyResult<Option<PyRef>> {
    match warnings_module(try_import) {
        Some(m) => ab::getattr_opt(&m, &pystr::new_str(attr)),
        None => Ok(None),
    }
}

fn get_filters() -> PyResult {
    if let Some(m) = warnings_module(false) {
        if let Some(f) = ab::getattr_opt(&m, &pystr::new_str("_get_filters"))? {
            let r = ab::call(&f, &[])?;
            if !list::is_list(&r) {
                raise!(TypeError, "_get_filters() must return a list, not {}", crate::types::typeobject::tname(&r));
            }
            return Ok(r);
        }
        if let Some(f) = ab::getattr_opt(&m, &pystr::new_str("filters"))? {
            if !list::is_list(&f) {
                raise!(TypeError, "warnings.filters must be a list, not {}", crate::types::typeobject::tname(&f));
            }
            return Ok(f);
        }
    }
    Ok(state_obj(&FILTERS))
}

fn get_onceregistry() -> PyResult {
    let r = match warnings_attr("onceregistry", false)? {
        Some(r) => r,
        None => state_obj(&ONCEREGISTRY),
    };
    if !dict::is_dict(&r) {
        raise!(TypeError, "warnings.onceregistry must be a dict, not '{}'", crate::types::typeobject::tname(&r));
    }
    Ok(r)
}

fn get_default_action() -> PyResult {
    let r = match warnings_attr("defaultaction", false)? {
        Some(r) => r,
        None => state_obj(&DEFAULTACTION),
    };
    if !pystr::is_str(&r) {
        raise!(TypeError, "warnings.defaultaction must be a string, not '{}'", crate::types::typeobject::tname(&r));
    }
    Ok(r)
}

fn filters_version() -> u64 {
    FILTERS_VERSION.load(Ordering::Acquire)
}

// ---------------------------------------------------------------- matching

fn check_matched(obj: &PyObject, arg: &PyObject) -> PyResult<bool> {
    if obj.is(none::none()) {
        return Ok(true);
    }
    if pystr::is_str_exact(obj) {
        return ab::rich_eq(obj, arg);
    }
    let r = ab::call_method_str(obj, "match", &[arg])?;
    ab::is_true(&r)
}

/// Dotted module names a file path can stand for: "/a/b/pkg/mod.py" gives
/// "mod", "pkg.mod", "b.pkg.mod" and "a.b.pkg.mod" ("pkg/__init__.py" counts
/// as "pkg"); an extension other than ".py" is kept as part of the name.
pub fn module_names_of_path(filename: &str) -> Vec<String> {
    let mut name = filename;
    if let Some(stem) = name.strip_suffix(".py") {
        name = stem.strip_suffix("/__init__").unwrap_or(stem);
    }
    let parts: Vec<&str> = name.split('/').collect();
    let mut out = Vec::new();
    for i in (0..parts.len()).rev() {
        if parts[i].is_empty() {
            break;
        }
        out.push(parts[i..].join("."));
    }
    out
}

/// A filter's module pattern matches the module name, or (for a warning
/// located in a file) one of the dotted names the file path stands for.
thread_local! {
    /// Set while a warning with a caller-chosen module name (compile(..., module=...)) is
    /// filtered: only that name is matched, not the names derived from the file path.
    static EXACT_MODULE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// warn_explicit() for a warning whose module name the caller chose explicitly.
pub fn warn_explicit_exact_module(category: *mut PyTypeObject, msg: &PyObject, filename: &PyObject, lineno: i64, module: &PyObject) -> PyResult<()> {
    let prev = EXACT_MODULE.with(|c| c.replace(true));
    let r = warn_explicit(category, msg, filename, lineno, Some(module), None);
    EXACT_MODULE.with(|c| c.set(prev));
    r
}

fn module_matches(pattern: &PyObject, module: &PyObject, filename: &PyObject) -> PyResult<bool> {
    if check_matched(pattern, module)? {
        return Ok(true);
    }
    if pattern.is(none::none()) || !pystr::is_str(filename) || EXACT_MODULE.with(|c| c.get()) {
        return Ok(false);
    }
    for cand in module_names_of_path(&pystr::to_string_lossy(filename)) {
        if check_matched(pattern, &pystr::new_str(&cand))? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Returns (action, matching filter item or None).
fn get_filter(category: &PyObject, text: &PyObject, lineno: i64, module: &PyObject, filename: &PyObject) -> PyResult<(PyRef, PyRef)> {
    let filters = get_filters()?;
    let mut i = 0;
    while i < list::list_len(&filters) {
        let item = match list::get(&filters, i) {
            Some(x) => x,
            None => break,
        };
        i += 1;
        if !tuple::is_tuple(&item) || tuple::tuple_len(&item) != 5 {
            raise!(ValueError, "warnings.filters item {} isn't a 5-tuple", i - 1);
        }
        let action = tuple::tuple_get(&item, 0).to_ref();
        let msg = tuple::tuple_get(&item, 1).to_ref();
        let cat = tuple::tuple_get(&item, 2).to_ref();
        let modu = tuple::tuple_get(&item, 3).to_ref();
        let ln = tuple::tuple_get(&item, 4).to_ref();
        if !pystr::is_str(&action) {
            raise!(TypeError, "action must be a string, not '{}'", crate::types::typeobject::tname(&action));
        }
        if !check_matched(&msg, text)? {
            continue;
        }
        if !ab::issubclass(category, &cat)? {
            continue;
        }
        if !module_matches(&modu, module, filename)? {
            continue;
        }
        let ln_v = ab::as_i64(&ln)?;
        if ln_v == 0 || ln_v == lineno {
            return Ok((action, item));
        }
    }
    let action = get_default_action()?;
    Ok((action, none::none_ref()))
}

fn already_warned(registry: Option<&PyObject>, key: &PyObject, should_set: bool) -> PyResult<bool> {
    let reg = match registry {
        Some(r) if !r.is(none::none()) => r,
        _ => return Ok(false),
    };
    let ver_key = pystr::new_str("version");
    let cur = filters_version();
    match dict::get_item(reg, &ver_key)? {
        Some(v) if int::is_int(&v) && ab::as_i64(&v).ok() == Some(cur as i64) => {
            if let Some(already) = dict::get_item(reg, key)? {
                if ab::is_true(&already)? {
                    return Ok(true);
                }
            }
        }
        _ => {
            dict::clear(reg);
            dict::set_item(reg, &ver_key, &int::new_int(cur as i64))?;
        }
    }
    if should_set {
        dict::set_item(reg, key, crate::types::bool_::true_obj())?;
    }
    Ok(false)
}

fn update_registry(registry: Option<&PyObject>, text: &PyObject, category: &PyObject, add_zero: bool) -> PyResult<bool> {
    let altkey = if add_zero {
        tuple::new_tuple(vec![text.to_ref(), category.to_ref(), int::new_int(0)])
    } else {
        tuple::new_tuple(vec![text.to_ref(), category.to_ref()])
    };
    already_warned(registry, &altkey, true)
}

fn normalize_module(filename: &PyObject) -> PyResult {
    let s = pystr::to_string_lossy(filename);
    if s.is_empty() {
        return Ok(pystr::new_str("<unknown>"));
    }
    if s.len() >= 3 && s[s.len() - 3..].eq_ignore_ascii_case(".py") {
        return ab::getitem(filename, &crate::types::slice::new_slice(&none::none_ref(), &int::new_int(-3), &none::none_ref()));
    }
    Ok(filename.to_ref())
}

// ---------------------------------------------------------------- display

fn show_warning_fallback(filename: &PyObject, lineno: i64, text: &PyObject, category: &PyObject, source_line: Option<&PyObject>) -> PyResult<()> {
    let name = ab::getattr_str(category, "__name__").and_then(|n| ab::str_string(&n)).unwrap_or_else(|_| {
        clear_exc();
        "<unknown>".into()
    });
    let mut s = format!("{}:{}: {}: {}\n", ab::str_string(filename)?, lineno, name, ab::str_string(text)?);
    let line = match source_line {
        Some(l) => Some(pystr::to_string_lossy(l)),
        None => {
            if let Ok(lc) = crate::import::import_absolute("linecache") {
                match ab::call_method_str(&lc, "getline", &[filename, &int::new_int(lineno)]) {
                    Ok(l) if pystr::is_str(&l) => Some(pystr::to_string_lossy(&l)),
                    _ => {
                        clear_exc();
                        None
                    }
                }
            } else {
                clear_exc();
                None
            }
        }
    };
    if let Some(l) = line {
        let t = l.trim_matches(|c: char| c == ' ' || c == '\t' || c == '\x0c' || c == '\n' || c == '\r');
        if !t.is_empty() {
            s.push_str(&format!("  {}\n", t));
        }
    }
    match crate::modules::sys::get_sys_attr("stderr") {
        Some(f) if !f.is(none::none()) => {
            ab::call_method_str(&f, "write", &[&pystr::new_str(&s)])?;
        }
        _ => {}
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn call_show_warning(
    category: &PyObject,
    text: &PyObject,
    message: &PyObject,
    filename: &PyObject,
    lineno: i64,
    source_line: Option<&PyObject>,
    source: Option<&PyObject>,
    module: &PyObject,
) -> PyResult<()> {
    let show_fn = warnings_attr("_showwarnmsg", true)?;
    let show_fn = match show_fn {
        Some(f) => f,
        None => return show_warning_fallback(filename, lineno, text, category, source_line),
    };
    if !ab::is_callable(&show_fn) {
        raise!(TypeError, "warnings._showwarnmsg() must be set to a callable");
    }
    let cls = match warnings_attr("WarningMessage", false)? {
        Some(c) => c,
        None => raise!(RuntimeError, "unable to get warnings.WarningMessage"),
    };
    let nn = none::none_ref();
    let line = source_line.map(|l| l.to_ref()).unwrap_or_else(none::none_ref);
    let src = source.map(|s| s.to_ref()).unwrap_or_else(none::none_ref);
    let kwn = tuple::new_tuple(vec![pystr::new_str("module")]);
    let msg = ab::call_kw(&cls, &[message, category, filename, &int::new_int(lineno), &nn, &line, &src, module], Some(&kwn))?;
    ab::call(&show_fn, &[&msg])?;
    Ok(())
}

/// Get the source line `lineno` through the loader named by `module_globals`
/// (validating the loader like importlib does).
fn get_source_line(module_globals: &PyObject, lineno: i64) -> PyResult<Option<PyRef>> {
    let lc = crate::import::import_absolute("linecache")?;
    let loader = ab::call_method_str(&lc, "_bless_my_loader", &[module_globals])?;
    if loader.is(none::none()) {
        return Ok(None);
    }
    // the spec's name is the one the loader knows (__name__ is "__main__"
    // for a module run with -m)
    let spec_name = match dict::get_item_rs(module_globals, "__spec__") {
        Some(s) if !s.is(none::none()) => ab::getattr_opt(&s, &pystr::new_str("name"))?.filter(|n| pystr::is_str(n)),
        _ => None,
    };
    let name = match spec_name.or_else(|| dict::get_item_rs(module_globals, "__name__")) {
        Some(n) => n,
        None => return Ok(None),
    };
    let get_source = match ab::getattr_opt(&loader, &pystr::new_str("get_source"))? {
        Some(g) => g,
        None => return Ok(None),
    };
    let src = match ab::call(&get_source, &[&name]) {
        Ok(s) => s,
        Err(e) => {
            // a loader that cannot provide this module's source (e.g. the
            // built-in importer for "-c" code) just means no source line
            if exc_matches(crate::types::exceptions::ImportError()) {
                clear_exc();
                return Ok(None);
            }
            return Err(e);
        }
    };
    if src.is(none::none()) {
        return Ok(None);
    }
    let lines = ab::call_method_str(&src, "splitlines", &[])?;
    if !list::is_list(&lines) {
        return Ok(None);
    }
    if lineno < 1 || lineno as usize > list::list_len(&lines) {
        return Ok(None);
    }
    let l = list::get(&lines, (lineno - 1) as usize).unwrap();
    Ok(Some(l))
}

// ---------------------------------------------------------------- core

#[allow(clippy::too_many_arguments)]
fn warn_explicit_impl(
    category: Option<&PyObject>,
    message: &PyObject,
    filename: &PyObject,
    lineno: i64,
    module: Option<&PyObject>,
    registry: Option<&PyObject>,
    module_globals: Option<&PyObject>,
    source: Option<&PyObject>,
) -> PyResult<()> {
    init_state();
    if let Some(r) = registry {
        if !r.is(none::none()) && !dict::is_dict(r) {
            raise!(TypeError, "'registry' must be a dict or None");
        }
    }
    // the dotted names a file path stands for are only tried for a warning
    // whose module is unknown (derived from its file name); a known module
    // name (the caller's __name__, or one passed in) is matched as it is
    let module_known = module.is_some_and(|m| !m.is(none::none()));
    let module: PyRef = match module {
        Some(m) if !m.is(none::none()) => m.to_ref(),
        _ => normalize_module(filename)?,
    };
    let warning_t = crate::exc_type!(Warning);
    let (text, category, message): (PyRef, PyRef, PyRef) = if ab::isinstance(message, &crate::types::typeobject::type_ref(warning_t))? {
        let t = ab::str(message)?;
        let c = crate::types::typeobject::type_ref(message.type_ptr());
        (t, c, message.to_ref())
    } else {
        let c = match category {
            Some(c) if !c.is(none::none()) => c.to_ref(),
            _ => raise!(TypeError, "category must be a Warning subclass, not 'NoneType'"),
        };
        let m = ab::call(&c, &[message])?;
        (message.to_ref(), c, m)
    };
    let key = tuple::new_tuple(vec![text.clone(), category.clone(), int::new_int(lineno)]);
    let registry = registry.filter(|r| !r.is(none::none()));
    {
        let _g = LockGuard::new();
        if already_warned(registry, &key, false)? {
            return Ok(());
        }
        let prev_exact = if module_known { Some(EXACT_MODULE.with(|c| c.replace(true))) } else { None };
        let found = get_filter(&category, &text, lineno, &module, filename);
        if let Some(p) = prev_exact {
            EXACT_MODULE.with(|c| c.set(p));
        }
        let (action, item) = found?;
        let a = pystr::to_string_lossy(&action);
        match a.as_str() {
            "error" => {
                set_exc(message.clone());
                return Err(PyErr);
            }
            "ignore" => return Ok(()),
            "once" => {
                if let Some(r) = registry {
                    dict::set_item(r, &key, crate::types::bool_::true_obj())?;
                }
                let once = get_onceregistry()?;
                if update_registry(Some(&once), &text, &category, false)? {
                    return Ok(());
                }
            }
            "always" | "all" => {}
            "module" => {
                if let Some(r) = registry {
                    dict::set_item(r, &key, crate::types::bool_::true_obj())?;
                    if update_registry(Some(r), &text, &category, true)? {
                        return Ok(());
                    }
                }
            }
            "default" => {
                if let Some(r) = registry {
                    dict::set_item(r, &key, crate::types::bool_::true_obj())?;
                }
            }
            _ => {
                let ar = ab::repr_string(&action)?;
                let ir = ab::str_string(&item)?;
                raise!(RuntimeError, "Unrecognized action ({}) in warnings.filters:\n {}", ar, ir);
            }
        }
    }
    let source_line = match module_globals {
        Some(g) if dict::is_dict(g) => get_source_line(g, lineno)?,
        _ => None,
    };
    call_show_warning(&category, &text, &message, filename, lineno, source_line.as_deref(), source, &module)
}

fn is_internal_filename(f: &str) -> bool {
    f.contains("importlib") && f.contains("_bootstrap")
}

fn frame_filename(f: &crate::interp::frame::Frame) -> String {
    pystr::to_string_lossy(&crate::types::code::code_data(f.code()).filename)
}

fn skip_by_prefix(name: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|p| name.starts_with(p.as_str()))
}

/// Find the frame `stacklevel` levels up, skipping importlib internals and
/// `skip_file_prefixes`. Returns (globals, filename, lineno).
fn setup_context(mut stacklevel: i64, prefixes: &[String]) -> PyResult<(PyRef, PyRef, i64)> {
    let mut f = crate::interp::current_frame_ptr();
    let first_internal = !f.is_null() && is_internal_filename(&frame_filename(unsafe { &*f }));
    if stacklevel <= 1 || first_internal {
        while {
            stacklevel -= 1;
            stacklevel > 0
        } && !f.is_null()
        {
            f = unsafe { (*f).prev };
        }
    } else {
        while {
            stacklevel -= 1;
            stacklevel > 0
        } && !f.is_null()
        {
            // next external frame
            f = unsafe { (*f).prev };
            while !f.is_null() {
                let fname = frame_filename(unsafe { &*f });
                if is_internal_filename(&fname) || skip_by_prefix(&fname, prefixes) {
                    f = unsafe { (*f).prev };
                } else {
                    break;
                }
            }
        }
    }
    if f.is_null() {
        let globals = crate::modules::sys::sys_dict();
        return Ok((globals, pystr::new_str("<sys>"), 0));
    }
    let fr = unsafe { &*f };
    let globals = fr.globals().to_ref();
    let filename = crate::types::code::code_data(fr.code()).filename.clone();
    let lineno = fr.lineno() as i64;
    Ok((globals, filename, lineno))
}

fn get_category(message: &PyObject, category: Option<&PyObject>) -> PyResult {
    let warning_t = crate::exc_type!(Warning);
    if ab::isinstance(message, &crate::types::typeobject::type_ref(warning_t))? {
        return Ok(crate::types::typeobject::type_ref(message.type_ptr()));
    }
    let c = match category {
        None => return Ok(crate::types::typeobject::type_ref(crate::exc_type!(UserWarning))),
        Some(c) if c.is(none::none()) => return Ok(crate::types::typeobject::type_ref(crate::exc_type!(UserWarning))),
        Some(c) => c,
    };
    if !crate::types::typeobject::is_type(c) || !crate::types::typeobject::is_subtype(c.as_ptr() as *mut PyTypeObject, warning_t) {
        let desc = if crate::types::typeobject::is_type(c) {
            format!("class '{}'", crate::types::typeobject::type_name(unsafe { &*(c.as_ptr() as *mut PyTypeObject) }))
        } else {
            format!("'{}'", crate::types::typeobject::tname(c))
        };
        raise!(TypeError, "category must be a Warning subclass, not {}", desc);
    }
    Ok(c.to_ref())
}

fn do_warn(message: &PyObject, category: &PyObject, stacklevel: i64, source: Option<&PyObject>, prefixes: &[String]) -> PyResult<()> {
    let (globals, filename, lineno) = setup_context(stacklevel, prefixes)?;
    let module = match dict::get_item_rs(&globals, "__name__") {
        Some(n) if pystr::is_str(&n) => n,
        _ => pystr::new_str("<string>"),
    };
    let reg_key = pystr::new_str("__warningregistry__");
    let registry = match dict::get_item(&globals, &reg_key)? {
        Some(r) => r,
        None => {
            let r = dict::new_dict();
            dict::set_item(&globals, &reg_key, &r)?;
            r
        }
    };
    warn_explicit_impl(Some(category), message, &filename, lineno, Some(&module), Some(&registry), None, source)
}

// ---------------------------------------------------------------- Python API

fn py_warn(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params =
        crate::args::Params::new("warn", &["message", "category", "stacklevel", "source", "skip_file_prefixes"], 0, 1).kwonly(1);
    let [message, category, stacklevel, source, skip] = crate::args::parse::<5>(&P, args, kw)?;
    let message = message.unwrap();
    let category = get_category(message, category)?;
    let mut stacklevel = match stacklevel {
        Some(s) => ab::index_as_ssize(s, None)? as i64,
        None => 1,
    };
    let mut prefixes = Vec::new();
    if let Some(s) = skip {
        if !tuple::is_tuple(s) {
            raise!(TypeError, "warn() argument 'skip_file_prefixes' must be tuple, not {}", crate::types::typeobject::tname(s));
        }
        for i in 0..tuple::tuple_len(s) {
            let p = tuple::tuple_get(s, i);
            if !pystr::is_str(p) {
                raise!(TypeError, "Found non-str '{}' in skip_file_prefixes.", crate::types::typeobject::tname(p));
            }
            prefixes.push(pystr::to_string_lossy(p));
        }
        if !prefixes.is_empty() && stacklevel < 2 {
            stacklevel = 2;
        }
    }
    let source = source.filter(|s| !s.is(none::none()));
    do_warn(message, &category, stacklevel, source, &prefixes)?;
    Ok(none::none_ref())
}

fn py_warn_explicit(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new(
        "warn_explicit",
        &["message", "category", "filename", "lineno", "module", "registry", "module_globals", "source"],
        0,
        4,
    );
    let [message, category, filename, lineno, module, registry, module_globals, source] = crate::args::parse::<8>(&P, args, kw)?;
    let message = message.unwrap();
    let filename = filename.unwrap();
    let lineno = lineno.unwrap();
    if !pystr::is_str(filename) {
        raise!(TypeError, "warn_explicit() argument 3 must be str, not {}", crate::types::typeobject::tname(filename));
    }
    if !int::is_int(lineno) {
        raise!(TypeError, "'{}' object cannot be interpreted as an integer", crate::types::typeobject::tname(lineno));
    }
    let lineno = ab::as_i64(lineno)?;
    if let Some(g) = module_globals {
        if !g.is(none::none()) && !dict::is_dict(g) {
            raise!(TypeError, "module_globals must be a dict, not '{}'", crate::types::typeobject::tname(g));
        }
    }
    let source = source.filter(|s| !s.is(none::none()));
    warn_explicit_impl(category, message, filename, lineno, module, registry, module_globals.filter(|g| !g.is(none::none())), source)?;
    Ok(none::none_ref())
}

fn py_filters_mutated(_m: &PyObject) -> PyResult {
    FILTERS_VERSION.fetch_add(1, Ordering::AcqRel);
    Ok(none::none_ref())
}

fn py_acquire_lock(_m: &PyObject) -> PyResult {
    lock_acquire();
    Ok(none::none_ref())
}

fn py_release_lock(_m: &PyObject) -> PyResult {
    if !lock_release() {
        raise!(RuntimeError, "cannot release un-acquired lock");
    }
    Ok(none::none_ref())
}

pub fn init_module() -> PyResult {
    init_state();
    let m = crate::types::module::new_module("_warnings")?;
    let d = crate::types::module::module_dict(&m);
    let methods = crate::mdefs![
        ("warn", FASTKW, py_warn, "warn($module, /, message, category=None, stacklevel=1, source=None, *,\n     skip_file_prefixes=())\n--\n\nIssue a warning: show it, ignore it or raise it, as the filters decide."),
        ("warn_explicit", FASTKW, py_warn_explicit, "warn_explicit($module, /, message, category, filename, lineno,\n              module=<unrepresentable>, registry=None,\n              module_globals=None, source=None)\n--\n\nIssue a warning with an explicitly given location."),
        ("_filters_mutated_lock_held", NOARGS, py_filters_mutated),
        ("_acquire_lock", NOARGS, py_acquire_lock),
        ("_release_lock", NOARGS, py_release_lock),
    ];
    crate::builtins::add_methods_to_dict(&d, methods, Some(&m))?;
    dict::set_item_str(&d, "filters", &state_obj(&FILTERS))?;
    dict::set_item_str(&d, "_onceregistry", &state_obj(&ONCEREGISTRY))?;
    dict::set_item_str(&d, "_defaultaction", &state_obj(&DEFAULTACTION))?;
    // A ContextVar holding the warnings context (used when
    // sys.flags.context_aware_warnings is set), if contextvars is available.
    match crate::import::import_absolute("_contextvars") {
        Ok(cv) => {
            let cls = ab::getattr_str(&cv, "ContextVar")?;
            let var = ab::call(&cls, &[&pystr::new_str("warnings_context")])?;
            dict::set_item_str(&d, "_warnings_context", &var)?;
        }
        Err(_) => clear_exc(),
    }
    Ok(m)
}

// ---------------------------------------------------------------- Rust API

/// PyErr_WarnEx equivalent: warn with `category` at `stacklevel`.
pub fn warn_ex(category: *mut PyTypeObject, msg: &PyObject, stacklevel: i64, source: Option<&PyObject>) -> PyResult<()> {
    init_state();
    let cat = crate::types::typeobject::type_ref(category);
    do_warn(msg, &cat, stacklevel, source, &[])
}

/// PyErr_WarnExplicit equivalent.
pub fn warn_explicit(category: *mut PyTypeObject, msg: &PyObject, filename: &PyObject, lineno: i64, module: Option<&PyObject>, registry: Option<&PyObject>) -> PyResult<()> {
    let cat = crate::types::typeobject::type_ref(category);
    warn_explicit_impl(Some(&cat), msg, filename, lineno, module, registry, None, None)
}

/// In a forked child only the forking thread survives: a warnings lock held by any other
/// thread at fork time is released (its owner no longer exists).
pub fn after_fork_child() {
    let me = crate::object::thread_id();
    let mut g = match LOCK.state.try_lock() {
        Ok(g) => g,
        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
        // the inner mutex is only held for a few instructions; a fork inside that window
        // leaves the lock as it is
        Err(std::sync::TryLockError::WouldBlock) => return,
    };
    if g.1 != 0 && g.0 != me {
        *g = (0, 0);
    }
}
