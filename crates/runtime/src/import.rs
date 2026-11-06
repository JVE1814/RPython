//! The import system core. Phase 1: a Rust implementation of the default import
//! protocol (sys.modules -> builtin modules -> sys.meta_path finders -> path search
//! for source modules/packages). `importlib` (written in Python) builds on this.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::*;
use crate::types::{dict, list, module, none, str as pystr, tuple};
use crate::{name, raise};
use std::collections::HashMap;
use std::sync::Mutex;

pub type ModInit = fn() -> PyResult;

static BUILTIN_MODULES: Mutex<Vec<(&'static str, ModInit)>> = Mutex::new(Vec::new());

pub fn register_builtin(name: &'static str, init: ModInit) {
    BUILTIN_MODULES.lock().unwrap().push((name, init));
}
pub fn builtin_module_names() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = BUILTIN_MODULES.lock().unwrap().iter().map(|(n, _)| *n).collect();
    v.push("sys");
    v.push("builtins");
    v.sort();
    v
}
pub fn find_builtin(name: &str) -> Option<ModInit> {
    BUILTIN_MODULES.lock().unwrap().iter().find(|(n, _)| *n == name).map(|(_, f)| *f)
}

/// Builtin module objects already created, held by weak reference: a re-import
/// returns the same object while it is alive, but a module dropped from
/// sys.modules (and otherwise unreferenced) can be collected and is then
/// initialised afresh by the next import.
static BUILTIN_OBJECTS: Mutex<Vec<(String, usize)>> = Mutex::new(Vec::new());

fn builtin_entry(name: &str) -> Option<PyRef> {
    let wr = BUILTIN_OBJECTS.lock().unwrap().iter().find(|(n, _)| n == name).map(|(_, p)| unsafe { PyRef::from_borrowed(*p as *mut PyObject) })?;
    if crate::types::weakref::is_weakref(&wr) {
        crate::types::weakref::get_object(&wr).filter(|o| !o.is(crate::types::none::none()))
    } else {
        Some(wr)
    }
}

pub fn note_builtin_module(name: &str, m: &PyObject) {
    if builtin_entry(name).is_some_and(|o| o.is(m)) {
        return;
    }
    // modules support weak references; anything else is kept alive
    let held = match crate::types::weakref::new_ref(crate::types::weakref::ref_type(), m, None) {
        Ok(w) => w,
        Err(_) => {
            crate::errors::clear_exc();
            m.to_ref()
        }
    };
    let mut g = BUILTIN_OBJECTS.lock().unwrap();
    let old = match g.iter_mut().find(|(n, _)| n == name) {
        Some(e) => std::mem::replace(&mut e.1, held.into_raw() as usize),
        None => {
            g.push((name.to_string(), held.into_raw() as usize));
            0
        }
    };
    drop(g);
    unsafe { crate::object::xdecref(old as *mut PyObject) };
}
pub fn is_builtin_module_object(m: &PyObject, name: &str) -> bool {
    builtin_entry(name).is_some_and(|o| o.is(m))
}
fn cached_builtin(name: &str) -> Option<PyRef> {
    builtin_entry(name)
}

/// The importlib bootstrap module once installed.
pub fn bootstrap() -> Option<PyRef> {
    let p = BOOTSTRAP.load(std::sync::atomic::Ordering::Acquire);
    if p.is_null() { None } else { Some(unsafe { PyRef::from_borrowed(p) }) }
}
static BOOTSTRAP: std::sync::atomic::AtomicPtr<PyObject> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
/// True once the path-based finders (PathFinder) are installed.
static EXTERNAL_INSTALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Code object of an importlib bootstrap module. Compiling it from source on every
/// start dominated startup time, so the compiled code is cached next to the source
/// (`__pycache__/<stem>.rpython-boot`, marshal format) keyed on the source size and
/// mtime and on the interpreter executable's size and mtime (a rebuilt compiler
/// invalidates it). Any problem with the cache falls back to compiling.
fn bootstrap_code(src: &[u8], file: &str, display: &str) -> PyResult {
    fn stamp(p: &std::path::Path) -> Option<(u64, u128)> {
        let m = std::fs::metadata(p).ok()?;
        let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
        Some((m.len(), t))
    }
    let path = std::path::Path::new(file);
    let key = (|| {
        let (sl, st) = stamp(path)?;
        let (el, et) = stamp(&std::env::current_exe().ok()?)?;
        Some(format!("rpython-boot v1{} {} {} {} {} {}\n", if crate::types::code::public_mode() { "w" } else { "" }, sl, st, el, et, display))
    })();
    let cache = path.parent().map(|dir| {
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        dir.join("__pycache__").join(format!("{}.rpython-boot", stem))
    });
    if let (Some(key), Some(cache)) = (&key, &cache) {
        if let Ok(data) = std::fs::read(cache) {
            if data.len() > key.len() && &data[..key.len()] == key.as_bytes() {
                let body = crate::types::bytes::new_bytes(&data[key.len()..]);
                match crate::modules::marshal::loads_bytes(&body, true) {
                    Ok(c) if crate::types::code::is_code(&c) => return Ok(c),
                    Ok(_) => {}
                    Err(_) => clear_exc(),
                }
            }
        }
    }
    let c = crate::compile::compile_bytes(src, display, crate::compile::Mode::Exec, 0, 0)?;
    if let (Some(key), Some(cache)) = (&key, &cache) {
        if let Ok(body) = crate::modules::marshal::dumps_vec(&c.code, 5, true) {
            let mut out = key.clone().into_bytes();
            out.extend_from_slice(&body);
            if let Some(dir) = cache.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            // write atomically (another process may be reading)
            let tmp = cache.with_extension(format!("tmp{}", std::process::id()));
            if std::fs::write(&tmp, &out).is_ok() && std::fs::rename(&tmp, cache).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        } else {
            clear_exc();
        }
    }
    Ok(c.code)
}

fn exec_bootstrap_module(name: &str, file: &str, display: &str) -> PyResult {
    let src = match std::fs::read(file) {
        Ok(s) => s,
        Err(e) => {
            return Err(crate::types::exceptions::os_error_from_errno(e.raw_os_error().unwrap_or(0), Some(&pystr::new_str(file)), None));
        }
    };
    let m = module::new_module(name)?;
    let d = module::module_dict(&m);
    dict::set_item_str(&d, "__builtins__", &crate::builtins::builtins_module())?;
    dict::set_item_str(&crate::modules::sys::modules_dict(), name, &m)?;
    let r = (|| -> PyResult<()> {
        let code = bootstrap_code(&src, file, display)?;
        crate::interp::eval_code(&code, &d, Some(&d))?;
        Ok(())
    })();
    if r.is_err() {
        let _ = dict::del_item(&crate::modules::sys::modules_dict(), &pystr::new_str(name));
        return Err(PyErr);
    }
    Ok(m)
}

/// Install importlib as the import implementation (CPython's frozen
/// `_frozen_importlib` / `_frozen_importlib_external`, loaded from Lib/importlib).
pub fn install_importlib(stdlib: &str) -> PyResult<()> {
    if std::env::var("RPYTHON_NO_IMPORTLIB").map(|v| !v.is_empty()).unwrap_or(false) {
        return Ok(());
    }
    let bs_file = format!("{}/importlib/_bootstrap.py", stdlib);
    if !std::path::Path::new(&bs_file).is_file() {
        return Ok(());
    }
    let imp = import_absolute("_imp")?;
    let bs = exec_bootstrap_module("_frozen_importlib", &bs_file, "<frozen importlib._bootstrap>")?;
    let sys = crate::modules::sys::sys_module();
    ab::call(&*ab::getattr_str(&bs, "_install")?, &[&sys, &imp])?;
    BOOTSTRAP.store(bs.clone().into_raw(), std::sync::atomic::Ordering::Release);
    let ext_file = format!("{}/importlib/_bootstrap_external.py", stdlib);
    let r = (|| -> PyResult<()> {
        exec_bootstrap_module("_frozen_importlib_external", &ext_file, "<frozen importlib._bootstrap_external>")?;
        ab::call(&*ab::getattr_str(&bs, "_install_external_importers")?, &[])?;
        Ok(())
    })();
    if r.is_err() {
        let e = take_exc();
        // fall back to the Rust import implementation entirely
        BOOTSTRAP.store(std::ptr::null_mut(), std::sync::atomic::Ordering::Release);
        if let Some(mp) = crate::modules::sys::get_sys_attr("meta_path") {
            list::clear(&mp);
        }
        if crate::modules::sys_full::config().verbose > 0 {
            crate::modules::sys::write_stderr("# could not install the path-based importers; using the builtin fallback\n");
            if let Some(e) = e {
                crate::modules::traceback_print::print_exception(&e);
            }
        }
        return Ok(());
    }
    EXTERNAL_INSTALLED.store(true, std::sync::atomic::Ordering::Release);
    install_zipimport_hook();
    Ok(())
}

/// Put `zipimport.zipimporter` first in sys.path_hooks (as CPython does at startup);
/// failures only disable importing from ZIP archives.
fn install_zipimport_hook() {
    let r = (|| -> PyResult<()> {
        let zi = import_absolute("zipimport")?;
        let hook = ab::getattr_str(&zi, "zipimporter")?;
        if let Some(hooks) = crate::modules::sys::get_sys_attr("path_hooks") {
            list::insert(&hooks, 0, hook);
        }
        Ok(())
    })();
    if r.is_err() {
        let _ = take_exc();
        if crate::modules::sys_full::config().verbose > 0 {
            crate::modules::sys::write_stderr("# can't import zipimport\n");
        }
    }
}

pub fn importlib_active() -> bool {
    EXTERNAL_INSTALLED.load(std::sync::atomic::Ordering::Acquire)
}

pub fn get_loaded_module(name: &str) -> Option<PyRef> {
    let m = crate::modules::sys::modules_dict();
    dict::get_item_rs(&m, name).filter(|m| !m.is(none::none()))
}

pub fn import_error_with(msg: &str, name: Option<&PyObject>, path: Option<&PyObject>, name_from: Option<&PyObject>) -> PyErr {
    import_error_typed(crate::types::exceptions::ImportError(), msg, name, path, name_from)
}

pub fn import_error_typed(
    tp: *mut crate::ffi::PyTypeObject,
    msg: &str,
    name: Option<&PyObject>,
    path: Option<&PyObject>,
    name_from: Option<&PyObject>,
) -> PyErr {
    match crate::types::exceptions::make_exception(tp, &[pystr::new_str(msg)]) {
        Ok(e) => {
            unsafe {
                let ie = &mut *(e.as_ptr() as *mut crate::types::exceptions::ImportErrorExc);
                for (slot, v) in [(&mut ie.name, name), (&mut ie.path, path), (&mut ie.name_from, name_from)] {
                    if let Some(v) = v {
                        let old = std::mem::replace(slot, v.to_ref().into_raw());
                        xdecref(old);
                    }
                }
            }
            set_exc(e);
        }
        Err(_) => {}
    }
    PyErr
}

pub fn cannot_import_name(module: &PyObject, pkgname: &PyObject, nm: &PyObject) -> PyResult {
    let file = ab::getattr_opt(module, name!("__file__")).ok().flatten();
    let n = pystr::to_string_lossy(nm);
    let p = pystr::to_string_lossy(pkgname);
    if module_is_initializing(&crate::types::module::module_dict(module)) {
        return Err(import_error_with(
            &format!(
                "cannot import name '{}' from partially initialized module '{}' (most likely due to a circular import) ({})",
                n,
                p,
                file.as_ref().map(|f| pystr::to_string_lossy(f)).unwrap_or_else(|| "unknown location".into())
            ),
            Some(pkgname),
            file.as_deref(),
            Some(nm),
        ));
    }
    let msg = match &file {
        Some(f) if pystr::is_str(f) => format!("cannot import name '{}' from '{}' ({})", n, p, pystr::to_string_lossy(f)),
        _ => format!("cannot import name '{}' from '{}' (unknown location)", n, p),
    };
    Err(import_error_with(&msg, Some(pkgname), file.as_deref(), Some(nm)))
}

pub fn module_is_initializing(d: &PyObject) -> bool {
    if let Some(spec) = dict::get_item_str(d, name!("__spec__")) {
        if spec.is(none::none()) {
            return false;
        }
        if let Ok(Some(v)) = ab::getattr_opt(&spec, name!("_initializing")) {
            return ab::is_true(&v).unwrap_or(false);
        }
        clear_exc();
    }
    false
}

pub fn module_repr(m: &PyObject) -> PyResult {
    let d = module::module_dict(m);
    // delegate to importlib if available
    if let Some(bs) = bootstrap() {
        if let Ok(f) = ab::getattr_str(&bs, "_module_repr") {
            return ab::call(&f, &[m]);
        }
        clear_exc();
    }
    let nm = dict::get_item_str(&d, name!("__name__")).map(|n| ab::repr_string(&n).unwrap_or_default()).unwrap_or_else(|| "'?'".into());
    if let Some(f) = dict::get_item_str(&d, name!("__file__")) {
        return Ok(pystr::new_str(&format!("<module {} from {}>", nm, ab::repr_string(&f)?)));
    }
    if let Some(l) = dict::get_item_str(&d, name!("__loader__")) {
        if !l.is(none::none()) {
            return Ok(pystr::new_str(&format!("<module {} ({})>", nm, ab::repr_string(&l)?)));
        }
    }
    let is_builtin = dict::get_item_str(&d, name!("__name__")).map_or(false, |n| find_builtin(&pystr::to_string_lossy(&n)).is_some());
    if is_builtin {
        return Ok(pystr::new_str(&format!("<module {} (built-in)>", nm)));
    }
    Ok(pystr::new_str(&format!("<module {}>", nm)))
}

/// `from module import *`
pub fn import_all_from(locals: &PyObject, module: &PyObject) -> PyResult<()> {
    let all = ab::getattr_opt(module, name!("__all__"))?;
    let (names, skip_private) = match all {
        Some(a) => (ab::collect_iterable(&a)?, false),
        None => {
            let d = match ab::getattr_opt(module, name!("__dict__"))? {
                Some(d) => d,
                None => raise!(ImportError, "from-import-* object has no __dict__ and no __all__"),
            };
            let keys = ab::call_method_str(&d, "keys", &[])?;
            (ab::collect_iterable(&keys)?, true)
        }
    };
    for n in names {
        if !pystr::is_str(&n) {
            let modname = ab::getattr_opt(module, name!("__name__"))?.map(|m| pystr::to_string_lossy(&m)).unwrap_or_default();
            if skip_private {
                raise!(TypeError, "Key in {}.__dict__ must be str, not {}", modname, crate::types::typeobject::tname(&n));
            }
            raise!(TypeError, "Item in {}.__all__ must be str, not {}", modname, crate::types::typeobject::tname(&n));
        }
        if skip_private && pystr::str_data(&n).len() > 0 && pystr::str_data(&n).get(0) == '_' as u32 {
            continue;
        }
        let v = ab::getattr(module, &n)?;
        if dict::is_dict_exact(locals) {
            dict::set_item(locals, &n, &v)?;
        } else {
            ab::setitem(locals, &n, Some(&v))?;
        }
    }
    Ok(())
}

/// Implementation of the IMPORT_NAME opcode.
pub fn import_name_from_frame(
    globals: &PyObject,
    locals: &PyObject,
    nm: &PyObject,
    fromlist: &PyObject,
    level: &PyObject,
    builtins: &PyObject,
) -> PyResult {
    let imp = if dict::is_dict(builtins) { dict::get_item_str(builtins, name!("__import__")) } else { ab::getitem(builtins, name!("__import__")).ok() };
    let imp = match imp {
        Some(i) => i,
        None => raise!(ImportError, "__import__ not found"),
    };
    if crate::builtins::is_our_import(&imp) {
        let lvl = ab::as_i64(level)?;
        return import_module_level(nm, Some(globals), fromlist, lvl);
    }
    ab::call(&imp, &[nm, globals, locals, fromlist, level])
}

fn resolve_name(nm: &str, globals: Option<&PyObject>, level: i64) -> PyResult<String> {
    let g = match globals {
        Some(g) if dict::is_dict(g) => g,
        _ => raise!(KeyError, "'__name__' not in globals"),
    };
    let package = dict::get_item_str(g, name!("__package__"));
    let spec = dict::get_item_str(g, name!("__spec__"));
    let pkg: String = match package {
        Some(p) if !p.is(none::none()) => {
            if !pystr::is_str(&p) {
                raise!(TypeError, "package must be a string");
            }
            pystr::to_string_lossy(&p)
        }
        _ => match spec.filter(|s| !s.is(none::none())) {
            Some(s) => pystr::to_string_lossy(&*ab::getattr(&s, name!("parent"))?),
            None => {
                let n = match dict::get_item_str(g, name!("__name__")) {
                    Some(n) => n,
                    None => raise!(KeyError, "'__name__' not in globals"),
                };
                let n = pystr::to_string_lossy(&n);
                if dict::get_item_str(g, name!("__path__")).is_some() {
                    n
                } else {
                    match n.rfind('.') {
                        Some(i) => n[..i].to_string(),
                        None => String::new(),
                    }
                }
            }
        },
    };
    if pkg.is_empty() {
        raise!(ImportError, "attempted relative import with no known parent package");
    }
    let mut base = pkg.as_str();
    for _ in 1..level {
        match base.rfind('.') {
            Some(i) => base = &base[..i],
            None => raise!(ImportError, "attempted relative import beyond top-level package"),
        }
    }
    Ok(if nm.is_empty() { base.to_string() } else { format!("{}.{}", base, nm) })
}

/// PyImport_ImportModuleLevelObject.
pub fn import_module_level(nm: &PyObject, globals: Option<&PyObject>, fromlist: &PyObject, level: i64) -> PyResult {
    if !pystr::is_str(nm) {
        raise!(TypeError, "module name must be str, not {}", crate::types::typeobject::tname(nm));
    }
    if level < 0 {
        raise!(ValueError, "level must be >= 0");
    }
    if level == 0 && pystr::to_string_strict(nm).is_none() {
        // a name with lone surrogates has no Rust string form; it can only
        // name a module already placed in sys.modules (grind-lib)
        let modules = crate::modules::sys::modules_dict();
        if let Some(m) = dict::get_item(&modules, nm)? {
            if !m.is(none::none()) {
                return Ok(m);
            }
        }
    }
    let n = pystr::to_string_lossy(nm);
    let abs = if level > 0 {
        resolve_name(&n, globals, level)?
    } else {
        if n.is_empty() {
            raise!(ValueError, "Empty module name");
        }
        n.clone()
    };
    let m = import_absolute(&abs)?;
    let fl_empty = fromlist.is(none::none()) || !ab::is_true(fromlist)?;
    if fl_empty {
        if level == 0 || !n.is_empty() {
            if level == 0 {
                let first = n.split('.').next().unwrap();
                return match get_loaded_module(first) {
                    Some(m) => Ok(m),
                    None => import_absolute(first),
                };
            }
            // relative: return the module corresponding to the first component of `n`
            let cut = abs.len() - n.len() + n.split('.').next().unwrap().len();
            let top = &abs[..cut];
            return match get_loaded_module(top) {
                Some(m) => Ok(m),
                None => import_absolute(top),
            };
        }
        return Ok(m);
    }
    // handle fromlist for packages
    if ab::hasattr(&m, name!("__path__"))? {
        handle_fromlist(&m, fromlist, &abs)?;
    }
    Ok(m)
}

fn handle_fromlist(m: &PyObject, fromlist: &PyObject, modname: &str) -> PyResult<()> {
    if let Some(bs) = bootstrap() {
        if importlib_active() {
            let f = ab::getattr_str(&bs, "_handle_fromlist")?;
            let imp = crate::builtins::builtin("__import__")?;
            ab::call(&f, &[m, fromlist, &imp]).map_err(|e| {
                remove_importlib_frames();
                e
            })?;
            return Ok(());
        }
    }
    for x in ab::collect_iterable(fromlist)? {
        if !pystr::is_str(&x) {
            raise!(TypeError, "Item in {}.__all__ must be str, not {}", modname, crate::types::typeobject::tname(&x));
        }
        let xs = pystr::to_string_lossy(&x);
        if xs == "*" {
            if let Some(all) = ab::getattr_opt(m, name!("__all__"))? {
                handle_fromlist(m, &all, modname)?;
            }
            continue;
        }
        if !ab::hasattr(m, &x)? {
            let sub = format!("{}.{}", modname, xs);
            match import_absolute(&sub) {
                Ok(_) => {}
                Err(e) => {
                    // ModuleNotFoundError for exactly this name is ignored
                    if exc_matches(crate::types::exceptions::ModuleNotFoundError()) {
                        let ex = take_exc().unwrap();
                        let ename = ab::getattr_opt(&ex, name!("name"))?;
                        if ename.map_or(false, |n| pystr::is_str(&n) && pystr::to_string_lossy(&n) == sub) {
                            continue;
                        }
                        set_exc(ex);
                    }
                    return Err(e);
                }
            }
        }
    }
    Ok(())
}

static IMPORT_LOCKS: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);

/// Import a module by absolute dotted name.
pub fn import_absolute(abs: &str) -> PyResult {
    let active = importlib_active();
    if let Some(m) = get_loaded_module(abs) {
        // fast path: fully initialised module in sys.modules
        if !active || !spec_initializing(&m) {
            return Ok(m);
        }
    }
    if active {
        let bs = bootstrap().unwrap();
        let f = ab::getattr_str(&bs, "_find_and_load")?;
        let imp = crate::builtins::builtin("__import__")?;
        return ab::call(&f, &[&pystr::new_str(abs), &imp]).map_err(|e| {
            remove_importlib_frames();
            e
        });
    }
    let modules = crate::modules::sys::modules_dict();
    if let Some(v) = dict::get_item_rs(&modules, abs) {
        if v.is(none::none()) {
            let nmo = pystr::new_str(abs);
            return Err(import_error_typed(
                crate::types::exceptions::ModuleNotFoundError(),
                &format!("import of {} halted; None in sys.modules", abs),
                Some(&nmo),
                None,
                None,
            ));
        }
    }
    let _ = &IMPORT_LOCKS;
    // parent first
    let (parent, path) = match abs.rfind('.') {
        Some(i) => {
            let parent_name = &abs[..i];
            let parent = import_absolute(parent_name)?;
            if let Some(m) = get_loaded_module(abs) {
                return Ok(m);
            }
            let path = match ab::getattr_opt(&parent, name!("__path__"))? {
                Some(p) => p,
                None => {
                    let nmo = pystr::new_str(abs);
                    return Err(import_error_typed(
                        crate::types::exceptions::ModuleNotFoundError(),
                        &format!("No module named '{}'; '{}' is not a package", abs, parent_name),
                        Some(&nmo),
                        None,
                        None,
                    ));
                }
            };
            (Some(parent), Some(path))
        }
        None => (None, None),
    };
    let m = find_and_load(abs, path.as_deref())?;
    if let Some(p) = parent {
        let child = &abs[abs.rfind('.').unwrap() + 1..];
        // the loaded module may have replaced itself in sys.modules
        let m2 = get_loaded_module(abs).unwrap_or(m.clone());
        if let Err(e) = ab::setattr(&p, &pystr::new_str(child), &m2) {
            let _ = e;
            clear_exc();
        }
        return Ok(m2);
    }
    Ok(get_loaded_module(abs).unwrap_or(m))
}

fn spec_initializing(m: &PyObject) -> bool {
    if !module::is_module(m) {
        return match ab::getattr_opt(m, name!("__spec__")) {
            Ok(Some(s)) => !s.is(none::none()) && matches!(ab::getattr_opt(&s, name!("_initializing")), Ok(Some(v)) if v.is(crate::types::bool_::true_obj())),
            _ => {
                clear_exc();
                false
            }
        };
    }
    module_is_initializing(&module::module_dict(m))
}

fn tb_filename_and_name(tb: &PyObject) -> (String, String) {
    let t = unsafe { &*(tb.as_ptr() as *const crate::types::traceback::PyTracebackObject) };
    if t.tb_frame.is_null() {
        return (String::new(), String::new());
    }
    match crate::types::frameobject::frame_of(unsafe { &*t.tb_frame }) {
        Some(f) => {
            let cd = crate::types::code::code_data(f.code());
            (pystr::to_string_lossy(&cd.filename), pystr::to_string_lossy(&cd.co_name))
        }
        None => (String::new(), String::new()),
    }
}

/// Strip importlib frames from the traceback of the current exception, as
/// CPython does: always for ImportError, otherwise only chunks that end in
/// `_call_with_frames_removed`.
pub fn remove_importlib_frames() {
    let e = match take_exc() {
        Some(e) => e,
        None => return,
    };
    if crate::modules::sys_full::config().verbose == 0 {
        if let Some(tb) = crate::types::exceptions::exc_traceback(&e) {
            let always = crate::types::typeobject::is_subtype(e.type_ptr(), crate::types::exceptions::ImportError());
            let mut entries: Vec<PyRef> = Vec::new();
            let mut cur = Some(tb);
            while let Some(t) = cur {
                let nx = unsafe { (*(t.as_ptr() as *const crate::types::traceback::PyTracebackObject)).tb_next };
                entries.push(t);
                cur = if nx.is_null() { None } else { Some(unsafe { PyRef::from_borrowed(nx) }) };
            }
            let mut keep = vec![true; entries.len()];
            let mut chunk_start: Option<usize> = None;
            for (i, t) in entries.iter().enumerate() {
                let (file, name) = tb_filename_and_name(t);
                let imp = file.starts_with("<frozen importlib._bootstrap");
                if imp {
                    if chunk_start.is_none() {
                        chunk_start = Some(i);
                    }
                    if always || name == "_call_with_frames_removed" {
                        for k in chunk_start.unwrap()..=i {
                            keep[k] = false;
                        }
                        chunk_start = None;
                    }
                } else {
                    chunk_start = None;
                }
            }
            if keep.iter().any(|k| !k) {
                let kept: Vec<&PyRef> = entries.iter().zip(keep.iter()).filter(|(_, k)| **k).map(|(t, _)| t).collect();
                for w in 0..kept.len() {
                    let t = kept[w].as_ptr() as *mut crate::types::traceback::PyTracebackObject;
                    let nx = if w + 1 < kept.len() { kept[w + 1].to_ref().into_raw() } else { std::ptr::null_mut() };
                    unsafe {
                        let old = std::mem::replace(&mut (*t).tb_next, nx);
                        xdecref(old);
                    }
                }
                crate::types::exceptions::set_exc_traceback(&e, kept.first().map(|t| &***t));
            }
        }
    }
    set_exc(e);
}

fn find_and_load(abs: &str, path: Option<&PyObject>) -> PyResult {
    // 1. importlib (only builtin modules until the path finders are installed)
    if let Some(bs) = bootstrap() {
        if importlib_active() || (path.is_none() && find_builtin(abs).is_some()) {
            let f = ab::getattr_str(&bs, "_find_and_load")?;
            let imp = crate::builtins::builtin("__import__")?;
            return ab::call(&f, &[&pystr::new_str(abs), &imp]);
        }
    }
    // 2. builtin modules
    if path.is_none() {
        if let Some(init) = find_builtin(abs) {
            let m = match cached_builtin(abs) {
                Some(m) => m,
                None => init()?,
            };
            note_builtin_module(abs, &m);
            let modules = crate::modules::sys::modules_dict();
            dict::set_item_str(&modules, abs, &m)?;
            return Ok(m);
        }
    }
    // 3. path search
    let search: Vec<String> = match path {
        Some(p) => ab::collect_iterable(p)?.iter().filter(|x| pystr::is_str(x)).map(|x| pystr::to_string_lossy(x)).collect(),
        None => match crate::modules::sys::get_sys_attr("path") {
            Some(p) => ab::collect_iterable(&p)?.iter().filter(|x| pystr::is_str(x)).map(|x| pystr::to_string_lossy(x)).collect(),
            None => Vec::new(),
        },
    };
    let last = abs.rsplit('.').next().unwrap();
    for dir in search {
        let base = if dir.is_empty() { std::path::PathBuf::from(".") } else { std::path::PathBuf::from(&dir) };
        let pkg_dir = base.join(last);
        let init = pkg_dir.join("__init__.py");
        if init.is_file() {
            return load_source(abs, &init, Some(&pkg_dir));
        }
        let file = base.join(format!("{}.py", last));
        if file.is_file() {
            return load_source(abs, &file, None);
        }
        if let Some(so) = crate::capi_ext::find_extension(&base, last) {
            return crate::capi_ext::load_extension(abs, &so);
        }
        if pkg_dir.is_dir() {
            // namespace package (simplified)
            return load_namespace(abs, &pkg_dir);
        }
    }
    let nmo = pystr::new_str(abs);
    Err(import_error_typed(
        crate::types::exceptions::ModuleNotFoundError(),
        &format!("No module named '{}'", abs),
        Some(&nmo),
        None,
        None,
    ))
}

fn load_namespace(abs: &str, dir: &std::path::Path) -> PyResult {
    let m = module::new_module(abs)?;
    let d = module::module_dict(&m);
    dict::set_item_str(&d, "__path__", &list::new_list(vec![pystr::new_str(&dir.to_string_lossy())]))?;
    dict::set_item_str(&d, "__package__", &pystr::new_str(abs))?;
    dict::set_item_str(&crate::modules::sys::modules_dict(), abs, &m)?;
    Ok(m)
}

/// Load and execute a Python source file as module `abs`.
pub fn load_source(abs: &str, file: &std::path::Path, pkg_dir: Option<&std::path::Path>) -> PyResult {
    let fname = file.to_string_lossy().into_owned();
    let src = match std::fs::read(file) {
        Ok(s) => s,
        Err(e) => {
            return Err(crate::types::exceptions::os_error_from_errno(e.raw_os_error().unwrap_or(0), Some(&pystr::new_str(&fname)), None));
        }
    };
    let m = module::new_module(abs)?;
    let d = module::module_dict(&m);
    dict::set_item_str(&d, "__file__", &pystr::new_str(&fname))?;
    dict::set_item_str(&d, "__cached__", none::none())?;
    match pkg_dir {
        Some(p) => {
            dict::set_item_str(&d, "__path__", &list::new_list(vec![pystr::new_str(&p.to_string_lossy())]))?;
            dict::set_item_str(&d, "__package__", &pystr::new_str(abs))?;
        }
        None => {
            let parent = abs.rfind('.').map(|i| &abs[..i]).unwrap_or("");
            dict::set_item_str(&d, "__package__", &pystr::new_str(parent))?;
        }
    }
    dict::set_item_str(&d, "__builtins__", &crate::builtins::builtins_module())?;
    if let Some(spec) = crate::import_spec::make_spec(abs, Some(&fname), pkg_dir.is_some())? {
        dict::set_item_str(&d, "__spec__", &spec)?;
        if let Ok(Some(l)) = ab::getattr_opt(&spec, name!("loader")) {
            dict::set_item_str(&d, "__loader__", &l)?;
        }
    }
    let modules = crate::modules::sys::modules_dict();
    dict::set_item_str(&modules, abs, &m)?;
    let r = (|| -> PyResult<()> {
        let c = crate::compile::compile_bytes(&src, &fname, crate::compile::Mode::Exec, 0, -1)?;
        crate::interp::eval_code(&c.code, &d, Some(&d))?;
        Ok(())
    })();
    if let Err(e) = r {
        let _ = dict::del_item(&modules, &pystr::new_str(abs));
        return Err(e);
    }
    crate::import_spec::mark_initialized(&d);
    Ok(m)
}

#[allow(dead_code)]
fn _t(_: &PyObject) -> PyRef {
    tuple::empty_tuple()
}

/// Resolve a relative module name (`level` > 0) against `globals` (PEP 328 rules).
pub fn resolve_relative(nm: &str, globals: Option<&PyObject>, level: i64) -> PyResult<String> {
    resolve_name(nm, globals, level)
}
