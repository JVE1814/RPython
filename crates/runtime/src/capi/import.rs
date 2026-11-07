//! import.h: importing from C, the module dictionary, inittab, code execution.

use super::*;
use crate::abstract_ as ab;
use crate::types::{dict, module as tmod, none, str as pystr, tuple};
use std::cell::RefCell;

thread_local! {
    /// Full dotted name of the extension being initialised (CPython's package context):
    /// PyModule_Create2 uses it instead of the short m_name.
    static PACKAGE_CONTEXT: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}
pub(crate) fn package_context() -> Option<String> {
    PACKAGE_CONTEXT.with(|c| c.borrow().last().cloned())
}
pub(crate) fn push_package_context(s: &str) {
    PACKAGE_CONTEXT.with(|c| c.borrow_mut().push(s.to_string()));
}
pub(crate) fn pop_package_context() {
    PACKAGE_CONTEXT.with(|c| {
        c.borrow_mut().pop();
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_GetModuleDict() -> *mut PyObject {
    let d = crate::modules::sys::modules_dict();
    let p = d.as_ptr();
    drop(d);
    p
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_GetModule(name: *mut PyObject) -> *mut PyObject {
    unsafe {
        let d = crate::modules::sys::modules_dict();
        match dict::get_item(&d, &*name) {
            Ok(Some(m)) if !none::is_none(&m) => m.into_raw(),
            _ => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_AddModuleRef(name: *const c_char) -> *mut PyObject {
    unsafe {
        match cstr_obj(name) {
            Ok(n) => add_module(&n),
            Err(_) => std::ptr::null_mut(),
        }
    }
}
unsafe fn add_module(n: &PyObject) -> *mut PyObject {
    let d = crate::modules::sys::modules_dict();
    match dict::get_item(&d, n) {
        Ok(Some(m)) if tmod::is_module(&m) => m.into_raw(),
        Ok(_) => ret((|| -> PyResult {
            let m = tmod::new_module_obj(n)?;
            dict::set_item(&d, n, &m)?;
            Ok(m)
        })()),
        Err(_) => std::ptr::null_mut(),
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_AddModuleObject(name: *mut PyObject) -> *mut PyObject {
    unsafe {
        let m = add_module(&*name);
        if !m.is_null() {
            decref(m); // borrowed (sys.modules keeps it)
        }
        m
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_AddModule(name: *const c_char) -> *mut PyObject {
    unsafe {
        let m = PyImport_AddModuleRef(name);
        if !m.is_null() {
            decref(m);
        }
        m
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_ImportModule(name: *const c_char) -> *mut PyObject {
    unsafe {
        match cstr_obj(name) {
            Ok(n) => PyImport_Import(n.as_ptr()),
            Err(_) => std::ptr::null_mut(),
        }
    }
}
/// PyImport_Import: goes through builtins.__import__ (honours import hooks).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_Import(name: *mut PyObject) -> *mut PyObject {
    unsafe {
        if name.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        ret((|| -> PyResult {
            let n = &*name;
            let b = crate::builtins::builtins_module();
            let imp = ab::getattr_str(&b, "__import__")?;
            let fromlist = tuple::new_tuple(vec![pystr::new_str("__doc__")]);
            let g = crate::interp::current_globals().unwrap_or_else(dict::new_dict);
            ab::call(&imp, &[n, &g, &g, &fromlist, &crate::types::int::new_int(0)])?;
            let d = crate::modules::sys::modules_dict();
            match dict::get_item(&d, n)? {
                Some(m) => Ok(m),
                None => crate::import::import_absolute(&pystr::to_string_lossy(n)),
            }
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_ImportModuleLevelObject(
    name: *mut PyObject,
    globals: *mut PyObject,
    _locals: *mut PyObject,
    fromlist: *mut PyObject,
    level: c_int,
) -> *mut PyObject {
    unsafe {
        if name.is_null() {
            let _ = value_error("Empty module name");
            return std::ptr::null_mut();
        }
        if !globals.is_null() && !dict::is_dict(&*globals) {
            let _ = type_error("globals must be a dict");
            return std::ptr::null_mut();
        }
        let fl = opt(fromlist).unwrap_or(none::none());
        ret(crate::import::import_module_level(&*name, opt(globals), fl, level as i64))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_ImportModuleLevel(
    name: *const c_char,
    globals: *mut PyObject,
    locals: *mut PyObject,
    fromlist: *mut PyObject,
    level: c_int,
) -> *mut PyObject {
    unsafe {
        match cstr_obj(name) {
            Ok(n) => PyImport_ImportModuleLevelObject(n.as_ptr(), globals, locals, fromlist, level),
            Err(_) => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_ImportModuleAttr(modname: *mut PyObject, attr: *mut PyObject) -> *mut PyObject {
    unsafe {
        let m = PyImport_Import(modname);
        if m.is_null() {
            return m;
        }
        let m = PyRef::from_raw(m);
        ret(ab::getattr(&m, &*attr))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_ImportModuleAttrString(modname: *const c_char, attr: *const c_char) -> *mut PyObject {
    unsafe {
        let (Ok(m), Ok(a)) = (cstr_obj(modname), cstr_obj(attr)) else { return std::ptr::null_mut() };
        PyImport_ImportModuleAttr(m.as_ptr(), a.as_ptr())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_ReloadModule(m: *mut PyObject) -> *mut PyObject {
    unsafe {
        ret((|| -> PyResult {
            let il = crate::import::import_absolute("importlib")?;
            let r = ab::getattr_str(&il, "reload")?;
            ab::call(&r, &[&*m])
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_GetImporter(path: *mut PyObject) -> *mut PyObject {
    unsafe {
        ret((|| -> PyResult {
            let cache = crate::modules::sys::get_sys_attr("path_importer_cache").ok_or(PyErr).or_else(|_| Err(system_error("no path_importer_cache")))?;
            if let Some(i) = dict::get_item(&cache, &*path)? {
                return Ok(i);
            }
            let hooks = crate::modules::sys::get_sys_attr("path_hooks").ok_or_else(|| system_error("no path_hooks"))?;
            for h in ab::collect_iterable(&hooks)? {
                match ab::call(&h, &[&*path]) {
                    Ok(imp) => {
                        dict::set_item(&cache, &*path, &imp)?;
                        return Ok(imp);
                    }
                    Err(e) => {
                        if exc_matches(crate::types::exceptions::ImportError()) {
                            clear_exc();
                            continue;
                        }
                        return Err(e);
                    }
                }
            }
            Ok(none::none_ref())
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_GetMagicNumber() -> std::ffi::c_long {
    crate::modules::_imp::MAGIC as std::ffi::c_long
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_GetMagicTag() -> *const c_char {
    c"cpython-315".as_ptr()
}

pub(crate) unsafe fn exec_code_module(name: &PyObject, code: &PyObject, pathname: Option<&PyObject>) -> PyResult {
    unsafe { exec_code_module_paths(name, code, pathname, None) }
}

/// Run `code` in module `name` (created in sys.modules if needed). The module gets a
/// spec, loader and `__file__` for the source path: `pathname`, else the source of the
/// cached file `cpathname`, else the code's filename. (capi2)
pub(crate) unsafe fn exec_code_module_paths(name: &PyObject, code: &PyObject, pathname: Option<&PyObject>, cpathname: Option<&PyObject>) -> PyResult {
    let d = crate::modules::sys::modules_dict();
    // the import machinery treats the name as a dotted str (an unhashable name already
    // fails the sys.modules lookup with TypeError)
    if !pystr::is_str(name) {
        ab::hash(name)?;
        return Err(crate::err!(AttributeError, "'{}' object has no attribute 'rpartition'", tname(name)));
    }
    let m = match dict::get_item(&d, name)? {
        Some(m) => m,
        None => {
            let m = tmod::new_module_obj(name)?;
            dict::set_item(&d, name, &m)?;
            m
        }
    };
    let md = tmod::module_dict(&m);
    if dict::get_item_rs(&md, "__builtins__").is_none() {
        dict::set_item_str(&md, "__builtins__", &crate::builtins::builtins_module())?;
    }
    let ext = crate::import::import_absolute("importlib._bootstrap_external")?;
    let path: PyRef = match (pathname, cpathname) {
        (Some(p), _) => p.to_ref(),
        (None, Some(c)) => match ab::call(&*ab::getattr_str(&ext, "source_from_cache")?, &[c]) {
            Ok(p) => p,
            Err(_) => {
                clear_exc();
                c.to_ref()
            }
        },
        (None, None) => ab::getattr_str(code, "co_filename")?,
    };
    let cp = cpathname.map(|c| c.to_ref()).unwrap_or_else(none::none_ref);
    let fix_up = ab::getattr_str(&ext, "_fix_up_module")?;
    if let Err(e) = ab::call(&fix_up, &[&md, name, &path, &cp]) {
        let _ = dict::del_item(&d, name);
        return Err(e);
    }
    if let Err(e) = crate::interp::eval_code(code, &md, Some(&md)) {
        let _ = dict::del_item(&d, name);
        return Err(e);
    }
    match dict::get_item(&d, name)? {
        Some(m) => Ok(m),
        None => Err(crate::err!(ImportError, "Loaded module {} not found in sys.modules", pystr::to_string_lossy(name))),
    }
}
/// A path argument of the char* ExecCodeModule variants (filesystem encoding).
unsafe fn fs_path_arg(p: *const c_char) -> PyResult<Option<PyRef>> {
    unsafe { if p.is_null() { Ok(None) } else { super::unicode::decode_fs(CStr::from_ptr(p).to_bytes()).map(Some) } }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_ExecCodeModuleObject(name: *mut PyObject, co: *mut PyObject, pathname: *mut PyObject, cpathname: *mut PyObject) -> *mut PyObject {
    unsafe { ret(exec_code_module_paths(&*name, &*co, opt(pathname), opt(cpathname))) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_ExecCodeModuleWithPathnames(name: *const c_char, co: *mut PyObject, pathname: *const c_char, cpathname: *const c_char) -> *mut PyObject {
    unsafe {
        ret((|| -> PyResult {
            let n = cstr_obj(name)?;
            let p = fs_path_arg(pathname)?;
            let c = fs_path_arg(cpathname)?;
            exec_code_module_paths(&n, &*co, p.as_deref(), c.as_deref())
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_ExecCodeModuleEx(name: *const c_char, co: *mut PyObject, pathname: *const c_char) -> *mut PyObject {
    unsafe { PyImport_ExecCodeModuleWithPathnames(name, co, pathname, std::ptr::null()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_ExecCodeModule(name: *const c_char, co: *mut PyObject) -> *mut PyObject {
    unsafe { PyImport_ExecCodeModuleWithPathnames(name, co, std::ptr::null(), std::ptr::null()) }
}

// ---------------------------------------------------------------- inittab / frozen

#[repr(C)]
pub struct InittabEntry {
    pub name: *const c_char,
    pub initfunc: Option<unsafe extern "C" fn() -> *mut PyObject>,
}
#[repr(C)]
pub struct Frozen {
    pub name: *const c_char,
    pub code: *const u8,
    pub size: c_int,
    pub is_package: c_int,
}
unsafe impl Sync for Frozen {}
unsafe impl Sync for InittabEntry {}

pub(crate) static FROZEN_END: [Frozen; 1] = [Frozen { name: std::ptr::null(), code: std::ptr::null(), size: 0, is_package: 0 }];
#[unsafe(no_mangle)]
pub static mut PyImport_FrozenModules: *const Frozen = FROZEN_END.as_ptr();
static INITTAB_END: [InittabEntry; 1] = [InittabEntry { name: std::ptr::null(), initfunc: None }];
#[unsafe(no_mangle)]
pub static mut PyImport_Inittab: *const InittabEntry = INITTAB_END.as_ptr();

static EXTRA_INITTAB: std::sync::Mutex<Vec<(String, usize)>> = std::sync::Mutex::new(Vec::new());

pub(crate) fn find_inittab(name: &str) -> Option<unsafe extern "C" fn() -> *mut PyObject> {
    let g = EXTRA_INITTAB.lock().unwrap();
    g.iter().find(|(n, _)| n == name).map(|&(_, f)| unsafe { std::mem::transmute::<usize, unsafe extern "C" fn() -> *mut PyObject>(f) })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_AppendInittab(name: *const c_char, initfunc: Option<unsafe extern "C" fn() -> *mut PyObject>) -> c_int {
    unsafe {
        let Some(f) = initfunc else { return -1 };
        EXTRA_INITTAB.lock().unwrap().push((cs(name).into_owned(), f as usize));
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_ExtendInittab(newtab: *mut InittabEntry) -> c_int {
    unsafe {
        let mut p = newtab;
        while !(*p).name.is_null() {
            PyImport_AppendInittab((*p).name, (*p).initfunc);
            p = p.add(1);
        }
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_ImportFrozenModuleObject(name: *mut PyObject) -> c_int {
    unsafe {
        // 1: found and executed, 0: not a frozen module, -1: error (capi2)
        if name.is_null() || !pystr::is_str(&*name) {
            return 0;
        }
        let n = pystr::to_string_lossy(&*name);
        let Some(e) = crate::frozen::lookup(&n, true) else { return 0 };
        let r = (|| -> PyResult<()> {
            let code = crate::modules::_imp::frozen_code(e)?;
            let m = from_c(add_module(&*name))?;
            let md = tmod::module_dict(&m);
            if dict::get_item_rs(&md, "__builtins__").is_none() {
                dict::set_item_str(&md, "__builtins__", &crate::builtins::builtins_module())?;
            }
            if e.is_package {
                dict::set_item_str(&md, "__path__", &crate::types::list::new_list(vec![]))?;
            }
            crate::interp::eval_code(&code, &md, Some(&md))?;
            Ok(())
        })();
        match r {
            Ok(()) => 1,
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_ImportFrozenModule(name: *const c_char) -> c_int {
    unsafe {
        match cstr_obj(name) {
            Ok(n) => PyImport_ImportFrozenModuleObject(n.as_ptr()),
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_CreateModuleFromInitfunc(spec: *mut PyObject, initfunc: Option<unsafe extern "C" fn() -> *mut PyObject>) -> *mut PyObject {
    unsafe {
        let Some(f) = initfunc else {
            let _ = bad_internal();
            return std::ptr::null_mut();
        };
        let name = match ab::getattr_str(&*spec, "name") {
            Ok(n) => n,
            Err(_) => return std::ptr::null_mut(),
        };
        ret(crate::capi_ext::run_initfunc(&pystr::to_string_lossy(&name), f, &*spec, None))
    }
}

// ---------------------------------------------------------------- lazy imports (PEP 810)

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_GetLazyImportsMode() -> c_int {
    (crate::modules::sys_full::lazy_imports_mode() == "all") as c_int
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_SetLazyImportsMode(mode: c_int) -> c_int {
    crate::modules::sys_full::set_lazy_imports_all(mode == 1);
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_GetLazyImportsFilter() -> *mut PyObject {
    crate::modules::sys_full::lazy_imports_filter().unwrap_or_else(none::none_ref).into_raw()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_SetLazyImportsFilter(f: *mut PyObject) -> c_int {
    unsafe {
        if !f.is_null() && !none::is_none(&*f) && !crate::abstract_::is_callable(&*f) {
            let _ = crate::err!(ValueError, "filter provided but is not callable");
            return -1;
        }
        crate::modules::sys_full::set_lazy_imports_filter_obj(if f.is_null() { None } else { Some(&*f) });
        0
    }
}
