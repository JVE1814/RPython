//! Loading C extension modules (.so): dlopen + PyModExport_<name> (PEP 793) or
//! PyInit_<name> (single-phase module object or multi-phase PyModuleDef), plus the
//! `_imp` primitives used by importlib's ExtensionFileLoader.
//!
//! Suffixes (in priority order): `.cpython-315t-x86_64-linux-gnu.so` (version-specific
//! free-threaded ABI), `.abi3t-x86_64-linux-gnu.so` and `.abi3t.so` (PEP 803 free-threaded stable
//! ABI; the GIL-only `.abi3` suffixes are not loadable here), `.so`.

use crate::abstract_ as ab;
use crate::capi;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::{dict, list, module as tmod, none, str as pystr, tuple};
use std::collections::HashMap;
use std::ffi::{CString, c_void};
use std::sync::Mutex;

pub const EXTENSION_SUFFIXES: [&str; 4] = [".cpython-315t-x86_64-linux-gnu.so", ".abi3t-x86_64-linux-gnu.so", ".abi3t.so", ".so"];

pub fn find_extension(dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    for s in EXTENSION_SUFFIXES {
        let p = dir.join(format!("{}{}", name, s));
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

struct Loaded {
    handle: usize,
}
static HANDLES: Mutex<Option<HashMap<String, Loaded>>> = Mutex::new(None);
/// Multi-phase modules created but not yet executed (module address -> def address).
static PENDING_EXEC: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

unsafe fn dlopen(path: &str) -> PyResult<*mut c_void> {
    unsafe {
        if let Some(l) = HANDLES.lock().unwrap().get_or_insert_with(HashMap::new).get(path) {
            return Ok(l.handle as *mut c_void);
        }
        let c = CString::new(path).map_err(|_| value_error("embedded null byte"))?;
        let h = libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if h.is_null() {
            let e = libc::dlerror();
            let msg = if e.is_null() { "dlopen failed".to_string() } else { std::ffi::CStr::from_ptr(e).to_string_lossy().into_owned() };
            return Err(crate::import::import_error_with(&msg, None, Some(&pystr::new_str(path)), None));
        }
        HANDLES.lock().unwrap().get_or_insert_with(HashMap::new).insert(path.to_string(), Loaded { handle: h as usize });
        Ok(h)
    }
}

/// RFC 3492 punycode encoding (for PyInitU_ names of non-ASCII modules).
fn punycode(input: &str) -> String {
    let (base, tmin, tmax, skew, damp) = (36u32, 1u32, 26u32, 38u32, 700u32);
    let cps: Vec<u32> = input.chars().map(|c| c as u32).collect();
    let mut out: String = input.chars().filter(|c| c.is_ascii()).collect();
    let b = out.len() as u32;
    let mut h = b;
    if b > 0 {
        out.push('-');
    }
    let digit = |d: u32| -> char { if d < 26 { (b'a' + d as u8) as char } else { (b'0' + (d - 26) as u8) as char } };
    let adapt = |mut delta: u32, num: u32, first: bool| -> u32 {
        delta = if first { delta / damp } else { delta / 2 };
        delta += delta / num;
        let mut k = 0;
        while delta > ((base - tmin) * tmax) / 2 {
            delta /= base - tmin;
            k += base;
        }
        k + (base - tmin + 1) * delta / (delta + skew)
    };
    let (mut n, mut delta, mut bias) = (128u32, 0u32, 72u32);
    while (h as usize) < cps.len() {
        let m = *cps.iter().filter(|&&c| c >= n).min().unwrap();
        delta += (m - n) * (h + 1);
        n = m;
        for &c in &cps {
            if c < n {
                delta += 1;
            }
            if c == n {
                let mut q = delta;
                let mut k = base;
                loop {
                    let t = if k <= bias { tmin } else if k >= bias + tmax { tmax } else { k - bias };
                    if q < t {
                        break;
                    }
                    out.push(digit(t + (q - t) % (base - t)));
                    q = (q - t) / (base - t);
                    k += base;
                }
                out.push(digit(q));
                bias = adapt(delta, h + 1, h == b);
                delta = 0;
                h += 1;
            }
        }
        delta += 1;
        n += 1;
    }
    out
}

fn hook_names(short: &str) -> (String, String) {
    if short.is_ascii() {
        (format!("PyModExport_{}", short), format!("PyInit_{}", short))
    } else {
        let p = punycode(short).replace('-', "_");
        (format!("PyModExportU_{}", p), format!("PyInitU_{}", p))
    }
}

/// A minimal spec object (used when loading outside importlib).
fn make_spec(name: &str, path: &str) -> PyResult {
    if let Ok(m) = crate::import::import_absolute("importlib.machinery") {
        if let Ok(ms) = ab::getattr_str(&m, "ModuleSpec") {
            let kw = dict::new_dict();
            dict::set_item_str(&kw, "origin", &pystr::new_str(path))?;
            if let Ok(spec) = ab::call_with_kwargs_dict(&ms, &[&pystr::new_str(name), none::none()], Some(&kw)) {
                let _ = ab::setattr(&spec, &pystr::new_str("has_location"), crate::types::bool_::true_obj());
                clear_exc();
                return Ok(spec);
            }
        }
    }
    clear_exc();
    let spec = tmod::new_module("ModuleSpec")?;
    let d = tmod::module_dict(&spec);
    dict::set_item_str(&d, "name", &pystr::new_str(name))?;
    dict::set_item_str(&d, "origin", &pystr::new_str(path))?;
    dict::set_item_str(&d, "loader", none::none())?;
    dict::set_item_str(&d, "parent", &pystr::new_str(name.rsplit_once('.').map_or("", |x| x.0)))?;
    dict::set_item_str(&d, "submodule_search_locations", none::none())?;
    dict::set_item_str(&d, "has_location", crate::types::bool_::true_obj())?;
    dict::set_item_str(&d, "cached", none::none())?;
    Ok(spec)
}

fn is_moduledef(o: &PyObject) -> bool {
    o.is_type(&raw mut tmod::PyModuleDef_Type)
}

/// Call an init function and create (but not necessarily exec) the module.
/// Returns (module, Some(def)) when exec is still pending for a multi-phase module.
unsafe fn create_from_initfunc(
    name: &str,
    f: unsafe extern "C" fn() -> *mut PyObject,
    spec: &PyObject,
) -> PyResult<(PyRef, Option<*mut PyModuleDef>)> {
    unsafe {
        capi::import::push_package_context(name);
        let r = f();
        capi::import::pop_package_context();
        if r.is_null() {
            if !err_occurred() {
                raise!(SystemError, "initialization of {} failed without raising an exception", name.rsplit('.').next().unwrap());
            }
            return Err(PyErr);
        }
        if err_occurred() {
            let cause = take_exc();
            decref(r);
            let _ = crate::err!(SystemError, "initialization of {} raised unreported exception", name.rsplit('.').next().unwrap());
            if let (Some(c), Some(e)) = (cause, take_exc()) {
                crate::types::exceptions::set_exc_cause(&e, Some(&c));
                set_exc(e);
            }
            return Err(PyErr);
        }
        if (*r).type_ptr().is_null() {
            // a PyModuleDef that never went through PyModuleDef_Init (no type,
            // no reference count to give back)
            raise!(SystemError, "init function of {} returned uninitialized object", name.rsplit('.').next().unwrap());
        }
        let ro = PyRef::from_raw(r);
        if is_moduledef(&ro) {
            let def = ro.as_ptr() as *mut PyModuleDef;
            // (interp1) Py_mod_multiple_interpreters decides where it may load
            crate::subinterp::check_multiphase_extension(name, def)?;
            let m = from_c(capi::module::PyModule_FromDefAndSpec2(def, spec.as_ptr(), 1013))?;
            return Ok((m, Some(def)));
        }
        if !tmod::is_module(&ro) {
            raise!(SystemError, "initialization of {} did not return an extension module", name.rsplit('.').next().unwrap());
        }
        // (interp1) single-phase modules cannot be isolated
        crate::subinterp::check_singlephase_extension(name)?;
        Ok((ro, None))
    }
}

/// PEP 793: module from a PyModExport_* slot array.
/// Whether a PySlot array (or an array nested in it through Py_slot_subslots)
/// contains Py_mod_abi.
unsafe fn has_mod_abi_slot(mut s: *const capi::typeobj::PySlot, depth: u32) -> bool {
    const PY_MOD_ABI: u16 = 109;
    const PY_SLOT_SUBSLOTS: u16 = 92;
    unsafe {
        while !s.is_null() && (*s).sl_id != 0 {
            match (*s).sl_id {
                PY_MOD_ABI => return true,
                PY_SLOT_SUBSLOTS if depth < 8 => {
                    if has_mod_abi_slot((*s).sl_ptr as *const capi::typeobj::PySlot, depth + 1) {
                        return true;
                    }
                }
                _ => {}
            }
            s = s.add(1);
        }
    }
    false
}

unsafe fn create_from_export(
    name: &str,
    f: unsafe extern "C" fn() -> *const capi::typeobj::PySlot,
    spec: &PyObject,
) -> PyResult<(PyRef, Option<*mut PyModuleDef>)> {
    unsafe {
        capi::import::push_package_context(name);
        let slots = f();
        capi::import::pop_package_context();
        if slots.is_null() {
            if !err_occurred() {
                raise!(SystemError, "module export hook for {} returned NULL without an exception", name);
            }
            return Err(PyErr);
        }
        // an exported slot array must declare the ABI it was built for
        if !has_mod_abi_slot(slots, 0) {
            raise!(SystemError, "module export hook for {} did not provide a Py_mod_abi slot", name);
        }
        let m = from_c(capi::module::PyModule_FromSlotsAndSpec(slots, spec.as_ptr()))?;
        let def = if tmod::is_module(&m) { tmod::m(&m).md_def } else { std::ptr::null_mut() };
        Ok((m, if def.is_null() { None } else { Some(def) }))
    }
}

unsafe fn exec_pending(m: &PyObject, def: *mut PyModuleDef) -> PyResult<()> {
    unsafe {
        if capi::module::PyModule_ExecDef(m.as_ptr(), def) < 0 {
            return Err(PyErr);
        }
        Ok(())
    }
}

/// Used by PyImport_CreateModuleFromInitfunc: create + exec.
pub unsafe fn run_initfunc(name: &str, f: unsafe extern "C" fn() -> *mut PyObject, spec: &PyObject, path: Option<&str>) -> PyResult {
    unsafe {
        let (m, def) = create_from_initfunc(name, f, spec)?;
        if let Some(p) = path {
            if tmod::is_module(&m) {
                let _ = dict::set_item_str(&tmod::module_dict(&m), "__file__", &pystr::new_str(p));
            }
        }
        if let Some(d) = def {
            exec_pending(&m, d)?;
        }
        Ok(m)
    }
}

/// Load (create) an extension from `path` for module `fullname` with `spec`.
unsafe fn create_dynamic_impl(fullname: &str, path: &str, spec: &PyObject) -> PyResult<(PyRef, Option<*mut PyModuleDef>)> {
    unsafe {
        capi::data::init();
        let short = fullname.rsplit('.').next().unwrap();
        // inittab entries take precedence (embedding)
        if let Some(f) = capi::import::find_inittab(fullname) {
            return create_from_initfunc(fullname, f, spec);
        }
        let h = dlopen(path)?;
        let (export, init) = hook_names(short);
        let ce = CString::new(export).unwrap();
        let p = libc::dlsym(h, ce.as_ptr());
        if !p.is_null() {
            let f: unsafe extern "C" fn() -> *const capi::typeobj::PySlot = std::mem::transmute(p);
            return create_from_export(fullname, f, spec);
        }
        let ci = CString::new(init.clone()).unwrap();
        let p = libc::dlsym(h, ci.as_ptr());
        if p.is_null() {
            let msg = format!("dynamic module does not define module export function ({} or {})", hook_names(short).0, init);
            return Err(crate::import::import_error_with(&msg, Some(&pystr::new_str(fullname)), Some(&pystr::new_str(path)), None));
        }
        let f: unsafe extern "C" fn() -> *mut PyObject = std::mem::transmute(p);
        create_from_initfunc(fullname, f, spec)
    }
}

fn set_file_attrs(m: &PyObject, path: &str, spec: Option<&PyObject>) {
    if tmod::is_module(m) {
        let d = tmod::module_dict(m);
        if dict::get_item_rs(&d, "__file__").is_none() {
            let _ = dict::set_item_str(&d, "__file__", &pystr::new_str(path));
        }
        if let Some(s) = spec {
            let _ = dict::set_item_str(&d, "__spec__", s);
            if let Ok(Some(l)) = ab::getattr_opt(s, crate::name!("loader")) {
                let _ = dict::set_item_str(&d, "__loader__", &l);
            }
        }
        clear_exc();
    }
}

/// Entry point used by the Rust importer (import.rs::find_and_load).
pub fn load_extension(name: &str, path: &std::path::Path) -> PyResult {
    let p = path.to_string_lossy().into_owned();
    let spec = make_spec(name, &p)?;
    unsafe {
        let (m, def) = create_dynamic_impl(name, &p, &spec)?;
        set_file_attrs(&m, &p, Some(&spec));
        let modules = crate::modules::sys::modules_dict();
        dict::set_item_str(&modules, name, &m)?;
        if let Some(d) = def {
            if let Err(e) = exec_pending(&m, d) {
                let _ = dict::del_item(&modules, &pystr::new_str(name));
                return Err(e);
            }
        }
        // the module may have replaced itself in sys.modules
        Ok(dict::get_item_rs(&modules, name).unwrap_or(m))
    }
}


/// `_imp.create_dynamic`: create the module only. A multi-phase module is executed later by
/// `exec_extension` and is not put into sys.modules (importlib does that); a single-phase
/// module is complete once its init function returned and is registered right away.
pub fn create_extension(name: &str, path: &std::path::Path, spec: &PyObject) -> PyResult {
    let p = path.to_string_lossy().into_owned();
    unsafe {
        let (m, def) = create_dynamic_impl(name, &p, spec)?;
        set_file_attrs(&m, &p, Some(spec));
        match def {
            Some(d) => PENDING_EXEC.lock().unwrap_or_else(|e| e.into_inner()).push((m.as_ptr() as usize, d as usize)),
            None => {
                let modules = crate::modules::sys::modules_dict();
                dict::set_item_str(&modules, name, &m)?;
            }
        }
        Ok(m)
    }
}

/// `_imp.exec_dynamic`: run the exec slots of a module made by `create_extension`.
pub fn exec_extension(m: &PyObject) -> PyResult<()> {
    let def = {
        let mut v = PENDING_EXEC.lock().unwrap_or_else(|e| e.into_inner());
        let key = m.as_ptr() as usize;
        v.iter().position(|&(k, _)| k == key).map(|i| v.remove(i).1)
    };
    let Some(d) = def else { return Ok(()) };
    // the address may belong to a newer module if the original one died unexecuted
    if !tmod::is_module(m) || tmod::m(m).md_def as usize != d {
        return Ok(());
    }
    unsafe { exec_pending(m, d as *mut PyModuleDef) }
}

// ---------------------------------------------------------------- _imp

pub fn imp_create_dynamic(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.is_empty() || args.len() > 2 {
        raise!(TypeError, "create_dynamic expected 1 or 2 arguments, got {}", args.len());
    }
    let spec = args[0];
    let name = pystr::to_string_lossy(&*(ab::getattr_str(spec, "name")?));
    let origin = ab::getattr_str(spec, "origin")?;
    let path = if pystr::is_str(&origin) { pystr::to_string_lossy(&origin) } else { String::new() };
    // already loaded (single-phase modules cannot be re-initialised)
    if let Some(m) = crate::import::get_loaded_module(&name) {
        if tmod::is_module(&m) && !tmod::m(&m).md_def.is_null() && unsafe { (*tmod::m(&m).md_def).m_slots.is_null() } {
            return Ok(m);
        }
    }
    unsafe {
        let (m, def) = create_dynamic_impl(&name, &path, spec)?;
        set_file_attrs(&m, &path, None);
        if let Some(d) = def {
            PENDING_EXEC.lock().unwrap().push((m.as_ptr() as usize, d as usize));
        }
        Ok(m)
    }
}
pub fn imp_exec_dynamic(_m: &PyObject, module: &PyObject) -> PyResult {
    let pending = {
        let mut g = PENDING_EXEC.lock().unwrap();
        let pos = g.iter().position(|&(m, _)| m == module.as_ptr() as usize);
        pos.map(|i| g.remove(i).1)
    };
    if let Some(d) = pending {
        unsafe { exec_pending(module, d as *mut PyModuleDef)? };
    }
    Ok(crate::types::int::new_int(0))
}
fn imp_extension_suffixes(_m: &PyObject) -> PyResult {
    Ok(list::new_list(EXTENSION_SUFFIXES.iter().map(|s| pystr::new_str(s)).collect()))
}
fn imp_is_builtin(_m: &PyObject, n: &PyObject) -> PyResult {
    let s = pystr::to_string_lossy(n);
    let b = crate::import::find_builtin(&s).is_some() || s == "sys" || s == "builtins";
    Ok(crate::types::int::new_int(if b { -1 } else { 0 }))
}
fn imp_create_builtin(_m: &PyObject, spec: &PyObject) -> PyResult {
    let name = pystr::to_string_lossy(&*(ab::getattr_str(spec, "name")?));
    if let Some(m) = crate::import::get_loaded_module(&name) {
        return Ok(m);
    }
    if let Some(init) = crate::import::find_builtin(&name) {
        return init();
    }
    if let Some(f) = capi::import::find_inittab(&name) {
        return unsafe { run_initfunc(&name, f, spec, None) };
    }
    raise!(ImportError, "no built-in module named {}", name);
}
fn imp_exec_builtin(_m: &PyObject, _module: &PyObject) -> PyResult {
    Ok(crate::types::int::new_int(0))
}
fn imp_false1(_m: &PyObject, _a: &PyObject) -> PyResult {
    Ok(crate::types::bool_::new_bool(false))
}
fn imp_noop(_m: &PyObject) -> PyResult {
    Ok(none::none_ref())
}
fn imp_false0(_m: &PyObject) -> PyResult {
    Ok(crate::types::bool_::new_bool(false))
}
fn imp_fix_co_filename(_m: &PyObject, _args: &[&PyObject]) -> PyResult {
    Ok(none::none_ref())
}
fn imp_find_frozen(_m: &PyObject, _args: &[&PyObject]) -> PyResult {
    Ok(none::none_ref())
}
fn imp_source_hash(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    let data = args.last().and_then(|b| crate::types::bytes::bytes_like_data(b)).unwrap_or_default();
    let h = crate::hash::hash_bytes(&data) as u64;
    Ok(crate::types::bytes::new_bytes(&h.to_le_bytes()))
}

pub fn init_imp_module() -> PyResult {
    let m = tmod::new_module("_imp")?;
    let d = tmod::module_dict(&m);
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![
            ("create_dynamic", FAST, imp_create_dynamic, "Create an extension module."),
            ("exec_dynamic", O, imp_exec_dynamic, "Initialize an extension module."),
            ("extension_suffixes", NOARGS, imp_extension_suffixes, "Returns the list of file suffixes used to identify extension modules."),
            ("is_builtin", O, imp_is_builtin, "Returns True if the module name corresponds to a built-in module."),
            ("create_builtin", O, imp_create_builtin, "Create an extension module."),
            ("exec_builtin", O, imp_exec_builtin, "Initialize a built-in module."),
            ("is_frozen", O, imp_false1, "Returns True if the module name corresponds to a frozen module."),
            ("is_frozen_package", O, imp_false1, "Returns True if the module name is of a frozen package."),
            ("find_frozen", FAST, imp_find_frozen, "Return info about the corresponding frozen module (if there is one) or None."),
            ("lock_held", NOARGS, imp_false0, "Return True if the import lock is currently held, else False."),
            ("acquire_lock", NOARGS, imp_noop, "Acquires the interpreter's import lock for the current thread."),
            ("release_lock", NOARGS, imp_noop, "Release the interpreter's import lock."),
            ("_fix_co_filename", FAST, imp_fix_co_filename, "Changes code.co_filename to specify the passed-in file path."),
            ("source_hash", FAST, imp_source_hash, "Hash the source."),
        ],
        Some(&m),
    )?;
    dict::set_item_str(&d, "check_hash_based_pycs", &pystr::new_str("never"))?;
    dict::set_item_str(&d, "pyc_magic_number_token", &crate::types::int::new_int(crate::modules::_imp::MAGIC as i64))?;
    let _ = tuple::empty_tuple();
    Ok(m)
}

/// Register `_imp` unless another workstream already provides it.
pub fn register() {
    if crate::import::find_builtin("_imp").is_none() {
        crate::import::register_builtin("_imp", init_imp_module);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn punycode_matches_rfc3492_examples() {
        assert_eq!(super::punycode("bücher"), "bcher-kva");
        assert_eq!(super::punycode("ü"), "tda");
    }
}
