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
    if let Some(bs) = get_loaded_module("importlib._bootstrap") {
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
    if let Some(m) = get_loaded_module(abs) {
        if module::is_module(&m) && module_is_initializing(&module::module_dict(&m)) {
            // being initialized by this or another thread: return it (circular import)
        }
        return Ok(m);
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

fn find_and_load(abs: &str, path: Option<&PyObject>) -> PyResult {
    // 1. Python-level meta_path finders (importlib), if installed
    if let Some(mp) = crate::modules::sys::get_sys_attr("meta_path") {
        if list::is_list(&mp) && list::list_len(&mp) > 0 {
            if let Some(bs) = get_loaded_module("importlib._bootstrap") {
                let f = ab::getattr_str(&bs, "_find_and_load")?;
                let imp = crate::builtins::builtin("__import__")?;
                return ab::call(&f, &[&pystr::new_str(abs), &imp]);
            }
        }
    }
    // 2. builtin modules
    if path.is_none() {
        if let Some(init) = find_builtin(abs) {
            let m = init()?;
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
