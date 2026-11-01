//! PEP 810 lazy imports.
//!
//! * `lazy_import` objects (`types.LazyImportType`) are placeholders bound by
//!   `lazy import` / `lazy from ... import` statements (or `__lazy_import__`). A *root*
//!   placeholder stands for the result of `__import__(name, globals, locals, fromlist,
//!   level)`; an *attribute* placeholder stands for a chain of `from`-style lookups
//!   on the value of a root placeholder (`lazy from m import a`, `lazy import a.b as c`).
//! * A placeholder is reified when a global/name lookup or a module attribute lookup
//!   finds it (the binding is then replaced by the value), or by `resolve()`. A failed
//!   reification leaves it in place, so the next use retries.
//! * `sys.lazy_modules` holds the names of modules (and `module.attr` entries of lazy
//!   from-imports) that were declared lazily but not imported yet. A missing attribute
//!   of a package whose dotted name is a pending lazy *module* entry imports it.
//! * The import hook (`__import__`) is looked up when the placeholder is reified, in
//!   the `__builtins__` of the globals it was created with.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::fncommon as fc;
use crate::object::*;
use crate::types::{dict, int, none, str as pystr, tuple};
use crate::{mdefs, name, raise};
use std::collections::{HashMap, HashSet};
use std::ffi::{c_int, c_void};
use std::sync::Mutex;

#[repr(C)]
pub struct LazyImport {
    ob_base: PyObject,
    /// root: module name as given; attribute placeholder: NULL
    name: *mut PyObject,
    globals: *mut PyObject,
    locals: *mut PyObject,
    /// None or a tuple of str
    fromlist: *mut PyObject,
    level: *mut PyObject,
    /// attribute placeholder: the root placeholder whose value the names are looked up on
    base: *mut PyObject,
    /// attribute placeholder: tuple of attribute names
    attrs: *mut PyObject,
    /// absolute module name of a root placeholder (str) or NULL
    resolved: *mut PyObject,
    /// tuple of names this placeholder added to sys.lazy_modules
    tracked: *mut PyObject,
    /// where the lazy import was declared ("file", line) for error notes
    origin: *mut PyObject,
}

#[unsafe(no_mangle)]
pub static mut PyLazyImport_Type: PyTypeObject = PyTypeObject::ZERO;

pub fn lazy_import_type() -> *mut PyTypeObject {
    &raw mut PyLazyImport_Type
}

#[inline]
pub fn is_lazy(o: &PyObject) -> bool {
    o.is_type(lazy_import_type())
}

fn li(o: &PyObject) -> &mut LazyImport {
    unsafe { &mut *(o.as_ptr() as *mut LazyImport) }
}

fn opt_ref(p: *mut PyObject) -> Option<PyRef> {
    if p.is_null() { None } else { Some(unsafe { PyRef::from_borrowed(p) }) }
}

fn raw(o: Option<&PyObject>) -> *mut PyObject {
    o.map_or(std::ptr::null_mut(), |o| o.to_ref().into_raw())
}

// ---------------------------------------------------------------- sys.lazy_modules

struct Tracking {
    /// names added by lazy declarations -> number of declarations still pending
    counts: HashMap<String, usize>,
    /// names that are pending *module* imports (not `module.attr` entries)
    modules: HashSet<String>,
}

static TRACK: Mutex<Option<Tracking>> = Mutex::new(None);
static LAZY_MODULES: std::sync::atomic::AtomicPtr<PyObject> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

fn with_track<R>(f: impl FnOnce(&mut Tracking) -> R) -> R {
    let mut g = TRACK.lock().unwrap_or_else(|e| e.into_inner());
    let t = g.get_or_insert_with(|| Tracking { counts: HashMap::new(), modules: HashSet::new() });
    f(t)
}

fn lazy_modules_set() -> Option<PyRef> {
    let p = LAZY_MODULES.load(std::sync::atomic::Ordering::Acquire);
    if p.is_null() { None } else { Some(unsafe { PyRef::from_borrowed(p) }) }
}

fn in_sys_modules(nm: &str) -> bool {
    crate::import::get_loaded_module(nm).is_some()
}

fn track(nm: &str, is_module: bool) -> PyResult<()> {
    with_track(|t| {
        *t.counts.entry(nm.to_string()).or_insert(0) += 1;
        if is_module {
            t.modules.insert(nm.to_string());
        }
    });
    if let Some(s) = lazy_modules_set() {
        crate::types::set::set_add(&s, &pystr::new_str(nm))?;
    }
    Ok(())
}

fn untrack(nm: &str, force: bool) {
    let remove = with_track(|t| {
        let c = t.counts.entry(nm.to_string()).or_insert(0);
        *c = c.saturating_sub(1);
        if *c == 0 || force {
            t.counts.remove(nm);
            t.modules.remove(nm);
            true
        } else {
            false
        }
    });
    if remove {
        if let Some(s) = lazy_modules_set() {
            let _ = crate::types::set::set_discard(&s, &pystr::new_str(nm));
            clear_exc_if_set();
        }
    }
}

fn clear_exc_if_set() {
    if err_occurred() {
        clear_exc();
    }
}

/// Is `full` a module name declared by a lazy import that is still pending?
fn pending_module(full: &str) -> bool {
    let known = with_track(|t| t.modules.contains(full));
    if !known {
        return false;
    }
    match lazy_modules_set() {
        Some(s) => crate::types::set::set_contains(&s, &pystr::new_str(full)).unwrap_or(false),
        None => false,
    }
}

// ---------------------------------------------------------------- creation

fn module_dict_value(m: &PyObject, attr: &PyObject) -> Option<PyRef> {
    if !m.is_type(crate::types::module::module_type()) {
        return None;
    }
    let d = crate::types::module::module_dict(m);
    dict::get_item_str(&d, attr)
}

fn current_origin() -> PyRef {
    let f = crate::interp::current_frame_ptr();
    if f.is_null() {
        return none::none_ref();
    }
    let f = unsafe { &mut *f };
    let line = crate::types::code::line_for_instr(f.code(), f.lasti as usize).map_or(-1, |l| l as i64);
    let file = crate::types::code::code_data(f.code()).filename.clone();
    tuple::new_tuple(vec![file, int::new_int(line)])
}

fn alloc_lazy() -> PyResult<*mut PyObject> {
    unsafe { fc::alloc(lazy_import_type()) }
}

fn absolute_name(name: &PyObject, globals: Option<&PyObject>, level: i64) -> PyResult<String> {
    let n = pystr::to_string_lossy(name);
    if level == 0 {
        return Ok(n);
    }
    crate::import::resolve_relative(&n, globals, level)
}

/// Create a root placeholder (no filter check).
fn new_root(name: &PyObject, globals: Option<&PyObject>, locals: Option<&PyObject>, fromlist: &PyObject, level: &PyObject, resolved: &str) -> PyResult {
    let op = alloc_lazy()?;
    let l = unsafe { &mut *(op as *mut LazyImport) };
    l.name = name.to_ref().into_raw();
    l.globals = raw(globals);
    l.locals = raw(locals);
    l.fromlist = fromlist.to_ref().into_raw();
    l.level = level.to_ref().into_raw();
    l.resolved = pystr::new_str(resolved).into_raw();
    l.origin = current_origin().into_raw();
    let obj = unsafe { PyRef::from_raw(op) };
    // pending module names: every not yet imported prefix (plain import) or the
    // module itself (from-import)
    let mut tracked: Vec<PyRef> = Vec::new();
    let has_from = ab::is_true(fromlist)?;
    let mut names: Vec<String> = Vec::new();
    if has_from {
        names.push(resolved.to_string());
    } else {
        let mut acc = String::new();
        for part in resolved.split('.') {
            if !acc.is_empty() {
                acc.push('.');
            }
            acc.push_str(part);
            names.push(acc.clone());
        }
    }
    for n in names {
        if !in_sys_modules(&n) {
            track(&n, true)?;
            tracked.push(pystr::new_str(&n));
        }
    }
    l.tracked = tuple::new_tuple(tracked).into_raw();
    Ok(obj)
}

fn check_fromlist(fromlist: &PyObject) -> PyResult<()> {
    if fromlist.is(none::none()) {
        return Ok(());
    }
    let items = ab::collect_iterable(fromlist)?;
    for it in items {
        if !pystr::is_str(&it) {
            raise!(TypeError, "Item in ``from list'' must be str, not {}", crate::types::typeobject::tname(&it));
        }
    }
    Ok(())
}

fn normalize_fromlist(fromlist: Option<&PyObject>) -> PyResult<PyRef> {
    match fromlist {
        None => Ok(none::none_ref()),
        Some(f) if f.is(none::none()) => Ok(none::none_ref()),
        Some(f) if tuple::is_tuple(f) => Ok(f.to_ref()),
        Some(f) if pystr::is_str(f) => Ok(tuple::new_tuple(vec![f.to_ref()])),
        Some(f) => Ok(tuple::new_tuple(ab::collect_iterable(f)?)),
    }
}

fn call_filter(globals: Option<&PyObject>, resolved: &str, fromlist: &PyObject) -> PyResult<bool> {
    let Some(filt) = crate::modules::sys_full::lazy_imports_filter() else { return Ok(true) };
    let importer = globals.and_then(|g| if dict::is_dict(g) { dict::get_item_str(g, name!("__name__")) } else { None }).unwrap_or_else(none::none_ref);
    let fl = if fromlist.is(none::none()) || !ab::is_true(fromlist)? { none::none_ref() } else { fromlist.to_ref() };
    let r = ab::call(&filt, &[&importer, &pystr::new_str(resolved), &fl])?;
    ab::is_true(&r)
}

/// The import hook for a placeholder created with `globals`.
fn import_hook(globals: Option<&PyObject>) -> PyResult {
    if let Some(g) = globals {
        if dict::is_dict(g) {
            if let Some(b) = dict::get_item_str(g, name!("__builtins__")) {
                let f = if dict::is_dict(&b) {
                    dict::get_item_str(&b, name!("__import__"))
                } else if b.is_type(crate::types::module::module_type()) {
                    dict::get_item_str(&crate::types::module::module_dict(&b), name!("__import__"))
                } else {
                    ab::getitem(&b, name!("__import__")).ok()
                };
                clear_exc_if_set();
                if let Some(f) = f {
                    return Ok(f);
                }
                raise!(ImportError, "__import__ not found");
            }
        }
    }
    let bm = crate::builtins::builtins_module();
    match dict::get_item_str(&crate::types::module::module_dict(&bm), name!("__import__")) {
        Some(f) => Ok(f),
        None => raise!(ImportError, "__import__ not found"),
    }
}

fn eager_import(name: &PyObject, globals: Option<&PyObject>, locals: Option<&PyObject>, fromlist: &PyObject, level: &PyObject) -> PyResult {
    let hook = import_hook(globals)?;
    let n = none::none_ref();
    if crate::builtins::is_our_import(&hook) {
        let lvl = ab::as_i64(level)?;
        return crate::import::import_module_level(name, globals, fromlist, lvl);
    }
    ab::call(&hook, &[name, globals.unwrap_or(&n), locals.unwrap_or(&n), fromlist, level])
}

/// builtins.__lazy_import__(name, globals=None, locals=None, fromlist=(), level=0)
fn b_lazy_import(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("__lazy_import__", &["name", "globals", "locals", "fromlist", "level"], 0, 1);
    let [nm, globals, locals, fromlist, level] = crate::args::parse::<5>(&P, args, kw)?;
    let nm = nm.unwrap();
    if !pystr::is_str(nm) {
        raise!(TypeError, "module name must be str, not {}", crate::types::typeobject::tname(nm));
    }
    let lvl = match level {
        Some(l) => ab::as_i64(l)?,
        None => 0,
    };
    if lvl < 0 {
        raise!(ValueError, "level must be >= 0");
    }
    let globals = globals.filter(|g| !g.is(none::none()));
    if let Some(g) = globals {
        if !dict::is_dict(g) {
            raise!(TypeError, "globals must be a dict");
        }
    }
    let frame_globals = if globals.is_none() { crate::interp::current_globals() } else { None };
    if globals.is_none() && frame_globals.is_none() {
        raise!(TypeError, "__lazy_import__() missing globals when called without a frame");
    }
    let globals = globals.or(frame_globals.as_deref());
    let locals = locals.filter(|l| !l.is(none::none()));
    let fl = normalize_fromlist(fromlist)?;
    check_fromlist(&fl)?;
    let level_obj = int::new_int(lvl);
    lazy_import_impl(nm, globals, locals, &fl, &level_obj)
}

/// Create a placeholder for `__import__(name, globals, locals, fromlist, level)`,
/// unless the lazy-imports filter rejects it (then import eagerly).
pub fn lazy_import_impl(nm: &PyObject, globals: Option<&PyObject>, locals: Option<&PyObject>, fromlist: &PyObject, level: &PyObject) -> PyResult {
    let lvl = ab::as_i64(level)?;
    if lvl == 0 && pystr::str_len(nm) == 0 {
        raise!(ValueError, "Empty module name");
    }
    let resolved = absolute_name(nm, globals, lvl)?;
    if !call_filter(globals, &resolved, fromlist)? {
        return eager_import(nm, globals, locals, fromlist, level);
    }
    new_root(nm, globals, locals, fromlist, level, &resolved)
}

/// IMPORT_NAME with the lazy bits: `explicit` (`lazy` keyword) or `eligible`
/// (module level outside try/except: lazy in "all" mode or via `__lazy_modules__`).
pub fn import_name(
    globals: &PyObject,
    locals: &PyObject,
    nm: &PyObject,
    fromlist: &PyObject,
    level: &PyObject,
    builtins: &PyObject,
    explicit: bool,
    eligible: bool,
    module_level: bool,
) -> PyResult {
    if explicit && !module_level {
        let what = if ab::is_true(fromlist)? { "lazy from ... import" } else { "lazy import" };
        let _ = crate::compile::raise_syntax_error(
            crate::types::exceptions::SyntaxError(),
            &format!("{} only allowed at module level", what),
            "<string>",
            0,
            0,
            None,
            None,
            None,
        );
        return Err(PyErr);
    }
    let lazy = explicit || (eligible && module_level && implicitly_lazy(globals, nm, level)?);
    if !lazy {
        return crate::import::import_name_from_frame(globals, locals, nm, fromlist, level, builtins);
    }
    let hook = if dict::is_dict(builtins) {
        dict::get_item_str(builtins, name!("__lazy_import__"))
    } else {
        let r = ab::getitem(builtins, name!("__lazy_import__")).ok();
        clear_exc_if_set();
        r
    };
    let Some(hook) = hook else { raise!(ImportError, "__lazy_import__ not found") };
    let fl = normalize_fromlist(Some(fromlist))?;
    if hook.as_ptr() == LAZY_FN.load(std::sync::atomic::Ordering::Acquire) {
        return lazy_import_impl(nm, Some(globals), Some(locals), &fl, level);
    }
    ab::call(&hook, &[nm, globals, locals, &fl, level])
}

fn implicitly_lazy(globals: &PyObject, nm: &PyObject, level: &PyObject) -> PyResult<bool> {
    if crate::modules::sys_full::lazy_imports_mode() == "all" {
        return Ok(true);
    }
    if !dict::is_dict(globals) {
        return Ok(false);
    }
    let Some(lm) = dict::get_item_str(globals, name!("__lazy_modules__")) else { return Ok(false) };
    let lvl = ab::as_i64(level)?;
    if ab::contains(&lm, nm)? {
        return Ok(true);
    }
    if lvl > 0 {
        if let Ok(abs) = absolute_name(nm, Some(globals), lvl) {
            return ab::contains(&lm, &pystr::new_str(&abs));
        }
        clear_exc_if_set();
    }
    Ok(false)
}

static LAZY_FN: std::sync::atomic::AtomicPtr<PyObject> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// IMPORT_FROM on a placeholder: an attribute placeholder (or the value itself when
/// the module is already loaded and has it).
pub fn import_from(p: &PyObject, attr: &PyObject) -> PyResult {
    let l = li(p);
    let (root, mut attrs): (PyRef, Vec<PyRef>) = if l.base.is_null() {
        (p.to_ref(), Vec::new())
    } else {
        (unsafe { PyRef::from_borrowed(l.base) }, tuple::tuple_items(unsafe { &*l.attrs }).iter().map(|a| a.to_ref()).collect())
    };
    let r = li(&root);
    let from_form = !r.fromlist.is_null() && ab::is_true(unsafe { &*r.fromlist })?;
    let mut tracked: Vec<PyRef> = Vec::new();
    if attrs.is_empty() && from_form {
        let resolved = pystr::to_string_lossy(unsafe { &*r.resolved });
        if let Some(m) = crate::import::get_loaded_module(&resolved) {
            if let Some(v) = module_dict_value(&m, attr) {
                return Ok(v);
            }
        }
        let full = format!("{}.{}", resolved, pystr::to_string_lossy(attr));
        if !in_sys_modules(&full) {
            track(&full, false)?;
            tracked.push(pystr::new_str(&full));
        }
    }
    attrs.push(attr.to_ref());
    let op = alloc_lazy()?;
    let n = unsafe { &mut *(op as *mut LazyImport) };
    n.base = root.into_raw();
    n.attrs = tuple::new_tuple(attrs).into_raw();
    n.tracked = tuple::new_tuple(tracked).into_raw();
    n.origin = current_origin().into_raw();
    Ok(unsafe { PyRef::from_raw(op) })
}

// ---------------------------------------------------------------- reification

/// `from`-style lookup used while replaying attribute names.
fn lookup_attr(v: &PyObject, attr: &PyObject) -> PyResult {
    match ab::getattr(v, attr) {
        Ok(x) => Ok(x),
        Err(e) => {
            if !exc_matches(crate::types::exceptions::AttributeError()) && !exc_matches(crate::types::exceptions::ImportError()) {
                return Err(e);
            }
            let cause = take_exc();
            // a submodule that is not (yet) an attribute of its package
            if let Ok(Some(pkg)) = ab::getattr_opt(v, name!("__name__")) {
                if pystr::is_str(&pkg) {
                    let full = format!("{}.{}", pystr::to_string_lossy(&pkg), pystr::to_string_lossy(attr));
                    if let Some(m) = crate::import::get_loaded_module(&full) {
                        return Ok(m);
                    }
                }
            }
            clear_exc_if_set();
            let pkgname = ab::getattr_opt(v, name!("__name__")).ok().flatten().filter(|n| pystr::is_str(n));
            clear_exc_if_set();
            let r = match pkgname {
                Some(pn) => crate::import::cannot_import_name(v, &pn, attr),
                None => Err(crate::import::import_error_with(
                    &format!("cannot import name '{}' from <unknown module name>", pystr::to_string_lossy(attr)),
                    None,
                    None,
                    Some(attr),
                )),
            };
            if let Some(c) = cause {
                if let Some(e) = take_exc() {
                    crate::types::exceptions::set_exc_cause(&e, Some(&c));
                    set_exc(e);
                }
            }
            r
        }
    }
}

fn add_origin_note(p: &PyObject) {
    let l = li(p);
    let Some(e) = take_exc() else { return };
    if crate::types::exceptions::is_exception_instance(&e) {
        let what = display_name(p);
        let origin = match opt_ref(l.origin) {
            Some(o) if tuple::is_tuple(&o) && tuple::tuple_len(&o) == 2 => format!(
                "{}, line {}",
                pystr::to_string_lossy(tuple::tuple_get(&o, 0)),
                ab::str_string(tuple::tuple_get(&o, 1)).unwrap_or_default()
            ),
            _ => "<unknown>".to_string(),
        };
        let note = pystr::new_str(&format!("lazy import of '{}' declared in {}", what, origin));
        let _ = ab::call_method_str(&e, "add_note", &[&note]);
        clear_exc_if_set();
    }
    set_exc(e);
}

fn resolve_root(p: &PyObject) -> PyResult {
    let l = li(p);
    let globals = opt_ref(l.globals);
    let locals = opt_ref(l.locals);
    let name = unsafe { PyRef::from_borrowed(l.name) };
    let fromlist = opt_ref(l.fromlist).unwrap_or_else(none::none_ref);
    let level = opt_ref(l.level).unwrap_or_else(|| int::new_int(0));
    let v = eager_import(&name, globals.as_deref(), locals.as_deref(), &fromlist, &level)?;
    finish(p);
    Ok(v)
}

/// Untrack the names of a successfully reified placeholder.
fn finish(p: &PyObject) {
    let l = li(p);
    if let Some(t) = opt_ref(l.tracked) {
        for n in tuple::tuple_items(&t) {
            let s = pystr::to_string_lossy(n);
            let loaded = in_sys_modules(&s);
            untrack(&s, loaded);
        }
        let old = std::mem::replace(&mut l.tracked, tuple::empty_tuple().into_raw());
        unsafe { xdecref(old) };
    }
}

/// Reify a placeholder (does not replace any binding).
pub fn resolve(p: &PyObject) -> PyResult {
    let ts = crate::threads::current();
    crate::interp::enter_recursive_call(ts, " while resolving a lazy import")?;
    let r = (|| -> PyResult {
        let l = li(p);
        if l.base.is_null() {
            return resolve_root(p);
        }
        let root = unsafe { PyRef::from_borrowed(l.base) };
        let mut v = resolve_root(&root)?;
        let attrs = unsafe { PyRef::from_borrowed(l.attrs) };
        for a in tuple::tuple_items(&attrs) {
            v = lookup_attr(&v, a)?;
            if is_lazy(&v) {
                v = resolve(&v)?;
            }
        }
        finish(p);
        Ok(v)
    })();
    crate::interp::leave_recursive_call(ts);
    if r.is_err() {
        add_origin_note(p);
    }
    r
}

/// Reify `v` found under `key` in `d` and replace the binding if it still holds `v`.
pub fn reify_in_dict(d: &PyObject, key: &PyObject, v: &PyObject) -> PyResult {
    let r = resolve(v)?;
    if dict::is_dict(d) {
        if let Some(cur) = dict::get_item_str(d, key) {
            if cur.is(v) {
                dict::set_item(d, key, &r)?;
            }
        }
    } else if let Ok(cur) = ab::getitem(d, key) {
        if cur.is(v) {
            ab::setitem(d, key, Some(&r))?;
        }
    } else {
        clear_exc_if_set();
    }
    Ok(r)
}

/// Module attribute fallback: a pending lazily declared submodule `module.attr`.
pub fn pending_submodule(module: &PyObject, attr: &PyObject) -> PyResult<Option<PyRef>> {
    if !pystr::is_str(attr) {
        return Ok(None);
    }
    let d = crate::types::module::module_dict(module);
    let Some(mn) = dict::get_item_str(&d, name!("__name__")) else { return Ok(None) };
    if !pystr::is_str(&mn) {
        return Ok(None);
    }
    let full = format!("{}.{}", pystr::to_string_lossy(&mn), pystr::to_string_lossy(attr));
    if !pending_module(&full) {
        return Ok(None);
    }
    let fl = tuple::new_tuple(vec![attr.to_ref()]);
    crate::import::import_module_level(&pystr::new_str(&full), None, &fl, 0)?;
    untrack(&full, true);
    if let Some(v) = dict::get_item_str(&d, attr) {
        return Ok(Some(v));
    }
    Ok(crate::import::get_loaded_module(&full))
}

fn display_name(p: &PyObject) -> String {
    let l = li(p);
    if l.base.is_null() {
        return pystr::to_string_lossy(unsafe { &*l.resolved });
    }
    let root = li(unsafe { &*l.base });
    let rn = pystr::to_string_lossy(unsafe { &*root.resolved });
    let from_form = !root.fromlist.is_null() && ab::is_true(unsafe { &*root.fromlist }).unwrap_or(false);
    let mut s = if from_form { rn } else { rn.split('.').next().unwrap_or("").to_string() };
    for a in tuple::tuple_items(unsafe { &*l.attrs }) {
        s.push('.');
        s.push_str(&pystr::to_string_lossy(a));
    }
    s
}

// ---------------------------------------------------------------- type

unsafe extern "C" fn li_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe { pystr::new_str(&format!("<lazy_import '{}'>", display_name(&*o))).into_raw() }
}

fn li_resolve(o: &PyObject) -> PyResult {
    resolve(o)
}

unsafe extern "C" fn li_getattro(o: *mut PyObject, nm: *mut PyObject) -> *mut PyObject {
    unsafe {
        match crate::types::typeobject::generic_getattr(&*o, &*nm) {
            Ok(v) => v.into_raw(),
            Err(_) => {
                if exc_matches(crate::types::exceptions::AttributeError()) {
                    clear_exc();
                    let _ = err!(
                        AttributeError,
                        "cannot access attribute '{}' on unresolved lazy import '{}'",
                        pystr::to_string_lossy(&*nm),
                        display_name(&*o)
                    );
                }
                std::ptr::null_mut()
            }
        }
    }
}

unsafe extern "C" fn li_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let l = &mut *(o as *mut LazyImport);
        for p in [
            &raw mut l.name,
            &raw mut l.globals,
            &raw mut l.locals,
            &raw mut l.fromlist,
            &raw mut l.level,
            &raw mut l.base,
            &raw mut l.attrs,
            &raw mut l.resolved,
            &raw mut l.tracked,
            &raw mut l.origin,
        ] {
            fc::clear_field(p);
        }
        fc::free(o);
    }
}

unsafe extern "C" fn li_traverse(o: *mut PyObject, v: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let l = &*(o as *mut LazyImport);
        for p in [l.name, l.globals, l.locals, l.fromlist, l.level, l.base, l.attrs] {
            let r = fc::visit(p, v, arg);
            if r != 0 {
                return r;
            }
        }
        0
    }
}

unsafe extern "C" fn li_clear(o: *mut PyObject) -> c_int {
    unsafe {
        let l = &mut *(o as *mut LazyImport);
        fc::clear_field(&raw mut l.globals);
        fc::clear_field(&raw mut l.locals);
        fc::clear_field(&raw mut l.base);
    }
    0
}

pub fn init_types() {
    unsafe {
        let t = lazy_import_type();
        fc::init_type(t, c"lazy_import", std::mem::size_of::<LazyImport>(), Py_TPFLAGS_HAVE_GC);
        (*t).tp_repr = Some(li_repr);
        (*t).tp_getattro = Some(li_getattro);
        (*t).tp_dealloc = Some(li_dealloc);
        (*t).tp_traverse = Some(li_traverse);
        (*t).tp_clear = Some(li_clear);
        (*t).tp_doc = c"Placeholder for a module or attribute that is imported lazily (PEP 810).".as_ptr();
        (*t).tp_methods = mdefs![("resolve", NOARGS, li_resolve, "Import the module (or look up the name) now and return it.")];
    }
}

pub fn ready() -> PyResult<()> {
    crate::types::typeobject::type_ready(lazy_import_type())
}

/// Install `__lazy_import__` into the builtins dict.
pub fn install_builtins(d: &PyObject, m: &PyObject) -> PyResult<()> {
    crate::builtins::add_methods_to_dict(
        d,
        mdefs![(
            "__lazy_import__",
            FASTKW,
            b_lazy_import,
            "__lazy_import__(name, globals=None, locals=None, fromlist=(), level=0)\n--\n\nCreate a lazy import placeholder for a module."
        )],
        Some(m),
    )?;
    if let Some(f) = dict::get_item_rs(d, "__lazy_import__") {
        LAZY_FN.store(f.as_ptr(), std::sync::atomic::Ordering::Release);
    }
    Ok(())
}

/// Install `sys.lazy_modules` and `sys._lazy_import_type`.
pub fn install_sys(d: &PyObject) -> PyResult<()> {
    let s = crate::types::set::make_set_empty()?;
    LAZY_MODULES.store(s.clone().into_raw(), std::sync::atomic::Ordering::Release);
    dict::set_item_str(d, "lazy_modules", &s)?;
    dict::set_item_str(d, "_lazy_import_type", crate::types::typeobject::type_obj(lazy_import_type()))?;
    Ok(())
}
