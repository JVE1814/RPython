//! pylifecycle.h / initconfig.h: initialisation & finalisation (the runtime is always
//! initialised by the launcher; embedding entry points are mostly no-ops), PyStatus,
//! PyConfig get/set (PEP 741), version strings, constants.

use super::*;
use crate::abstract_ as ab;
use crate::types::{bool_, bytes, int, none, str as pystr, tuple};
use std::ffi::c_void;
use std::sync::Mutex;

#[unsafe(no_mangle)]
pub static Py_Version: std::ffi::c_ulong = 0x030F02F0;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_IsInitialized() -> c_int {
    1
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_Initialize() {}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_InitializeEx(_sigs: c_int) {}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_InitializeMain() -> PyStatus {
    PyStatus_Ok()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_InitializeFromConfig(_c: *const c_void) -> PyStatus {
    PyStatus_Ok()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_PreInitialize(_c: *const c_void) -> PyStatus {
    PyStatus_Ok()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_PreInitializeFromBytesArgs(_c: *const c_void, _argc: Py_ssize_t, _argv: *mut *mut c_char) -> PyStatus {
    PyStatus_Ok()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_PreInitializeFromArgs(_c: *const c_void, _argc: Py_ssize_t, _argv: *mut *mut libc::wchar_t) -> PyStatus {
    PyStatus_Ok()
}
static ATEXIT: Mutex<Vec<usize>> = Mutex::new(Vec::new());
static UNSTABLE_ATEXIT: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

/// Interpreter-level C callbacks (PyUnstable_AtExit): run right after the
/// Python atexit callbacks.
pub fn run_interp_atexit() {
    super::pystate::run_interp_atexit(super::pystate::interp_ptr());
    let v: Vec<(usize, usize)> = std::mem::take(&mut *UNSTABLE_ATEXIT.lock().unwrap());
    for (f, d) in v.into_iter().rev() {
        let f: unsafe extern "C" fn(*mut c_void) = unsafe { std::mem::transmute(f) };
        unsafe { f(d as *mut c_void) };
    }
}
/// Process-level C callbacks (Py_AtExit): the very last step of finalization.
pub fn run_process_atexit() {
    let v: Vec<usize> = std::mem::take(&mut *ATEXIT.lock().unwrap());
    for f in v.into_iter().rev() {
        let f: unsafe extern "C" fn() = unsafe { std::mem::transmute(f) };
        unsafe { f() };
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_FinalizeEx() -> c_int {
    if crate::modules::sys_finalize::finalize() { 0 } else { -1 }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_Finalize() {
    unsafe {
        Py_FinalizeEx();
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_IsFinalizing() -> c_int {
    crate::modules::sys_full::is_finalizing() as c_int
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_Exit(code: c_int) -> ! {
    unsafe {
        Py_FinalizeEx();
        libc::fflush(std::ptr::null_mut());
        std::process::exit(code)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_AtExit(f: Option<unsafe extern "C" fn()>) -> c_int {
    match f {
        Some(f) => {
            ATEXIT.lock().unwrap().push(f as usize);
            0
        }
        None => -1,
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_AtExit(interp: *mut c_void, f: Option<unsafe extern "C" fn(*mut c_void)>, data: *mut c_void) -> c_int {
    match f {
        Some(f) => {
            super::pystate::add_interp_atexit(interp, f as usize, data as usize);
            0
        }
        None => -1,
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_NewInterpreter() -> *mut c_void {
    super::pystate::new_interpreter(crate::subinterp::Config::LEGACY, crate::subinterp::WHENCE_LEGACY_CAPI)
}
/// PyInterpreterConfig (cpython/pylifecycle.h).
#[repr(C)]
pub struct PyInterpreterConfig {
    pub use_main_obmalloc: c_int,
    pub allow_fork: c_int,
    pub allow_exec: c_int,
    pub allow_threads: c_int,
    pub allow_daemon_threads: c_int,
    pub check_multi_interp_extensions: c_int,
    pub gil: c_int,
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_NewInterpreterFromConfig(tstate: *mut *mut c_void, cfg: *const c_void) -> PyStatus {
    unsafe {
        let config = if cfg.is_null() {
            crate::subinterp::Config::ISOLATED
        } else {
            let c = &*(cfg as *const PyInterpreterConfig);
            crate::subinterp::Config {
                use_main_obmalloc: c.use_main_obmalloc != 0,
                allow_fork: c.allow_fork != 0,
                allow_exec: c.allow_exec != 0,
                allow_threads: c.allow_threads != 0,
                allow_daemon_threads: c.allow_daemon_threads != 0,
                check_multi_interp_extensions: c.check_multi_interp_extensions != 0,
                gil: c.gil,
            }
        };
        if let Err(msg) = config.validate() {
            *tstate = std::ptr::null_mut();
            return PyStatus_Error(msg.as_ptr());
        }
        let ts = super::pystate::new_interpreter(config, crate::subinterp::WHENCE_CAPI);
        *tstate = ts;
        if ts.is_null() {
            crate::errors::clear_exc();
            return PyStatus_Error(c"sub-interpreter creation failed".as_ptr());
        }
        PyStatus_Ok()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_EndInterpreter(ts: *mut c_void) {
    unsafe { super::pystate::end_interpreter(ts) }
}

unsafe fn run_main_with_args(args: Vec<String>) -> c_int {
    let mut i = 1;
    while i < args.len() && args[i].starts_with('-') && args[i] != "-c" && args[i] != "-m" {
        i += 1;
    }
    if i < args.len() && args[i] == "-c" {
        return crate::run_string(args.get(i + 1).map_or("", |s| s.as_str()));
    }
    if i < args.len() && args[i] == "-m" {
        return crate::run_module(args.get(i + 1).map_or("", |s| s.as_str()));
    }
    match args.get(i) {
        Some(f) => crate::run_file(f),
        None => 0,
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_BytesMain(argc: c_int, argv: *mut *mut c_char) -> c_int {
    unsafe {
        let args: Vec<String> = (0..argc as usize).map(|i| cs(*argv.add(i)).into_owned()).collect();
        run_main_with_args(args)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_Main(argc: c_int, argv: *mut *mut libc::wchar_t) -> c_int {
    unsafe {
        let args: Vec<String> = (0..argc as usize)
            .map(|i| {
                let p = *argv.add(i);
                let n = libc::wcslen(p);
                std::slice::from_raw_parts(p as *const u32, n).iter().filter_map(|&c| char::from_u32(c)).collect()
            })
            .collect();
        run_main_with_args(args)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_RunMain() -> c_int {
    unsafe { run_main_with_args(std::env::args().collect()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_FrozenMain(argc: c_int, argv: *mut *mut c_char) -> c_int {
    unsafe { Py_BytesMain(argc, argv) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_ExitStatusException(st: PyStatus) -> ! {
    unsafe {
        if st._type == 2 {
            std::process::exit(st.exitcode);
        }
        super::refcount::fatal(&if st.err_msg.is_null() { "error".into() } else { cs(st.err_msg) })
    }
}

// ---------------------------------------------------------------- version info

static VERSION: std::sync::OnceLock<std::ffi::CString> = std::sync::OnceLock::new();
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_GetVersion() -> *const c_char {
    VERSION
        .get_or_init(|| {
            let v = crate::modules::sys::get_sys_attr("version").map(|v| crate::types::str::to_string_lossy(&v)).unwrap_or_else(|| "3.15.2".into());
            std::ffi::CString::new(v).unwrap()
        })
        .as_ptr()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_GetPlatform() -> *const c_char {
    c"linux".as_ptr()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_GetCopyright() -> *const c_char {
    c"Copyright (c) 2001 Python Software Foundation.\nAll Rights Reserved.\n\nCopyright (c) 2000 BeOpen.com.\nAll Rights Reserved.\n\nCopyright (c) 1995-2001 Corporation for National Research Initiatives.\nAll Rights Reserved.\n\nCopyright (c) 1991-1995 Stichting Mathematisch Centrum, Amsterdam.\nAll Rights Reserved.".as_ptr()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_GetCompiler() -> *const c_char {
    c"[rustc]".as_ptr()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_GetBuildInfo() -> *const c_char {
    c"main, Nov  1 2026, 00:00:00".as_ptr()
}
static ARGV: Mutex<usize> = Mutex::new(0);
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_GetArgcArgv(argc: *mut c_int, argv: *mut *mut *mut libc::wchar_t) {
    unsafe {
        let mut g = ARGV.lock().unwrap();
        let args: Vec<String> = std::env::args().collect();
        if *g == 0 {
            let v: Vec<*mut libc::wchar_t> = args
                .iter()
                .map(|a| {
                    let mut w: Vec<libc::wchar_t> = a.chars().map(|c| c as libc::wchar_t).collect();
                    w.push(0);
                    Box::leak(w.into_boxed_slice()).as_mut_ptr()
                })
                .collect();
            *g = Box::leak(v.into_boxed_slice()).as_mut_ptr() as usize;
        }
        *argc = args.len() as c_int;
        *argv = *g as *mut *mut libc::wchar_t;
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_GETENV(name: *const c_char) -> *mut c_char {
    unsafe {
        if std::env::var_os("PYTHONIGNOREENVIRONMENT").is_some() {
            return std::ptr::null_mut();
        }
        libc::getenv(name)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_DecodeLocale(arg: *const c_char, size: *mut usize) -> *mut libc::wchar_t {
    unsafe {
        let b = CStr::from_ptr(arg).to_bytes();
        let s = match crate::modules::codecs_core::decode_utf8(b, "surrogateescape") {
            Ok(s) => s,
            Err(_) => {
                clear_exc();
                if !size.is_null() {
                    *size = usize::MAX - 1;
                }
                return std::ptr::null_mut();
            }
        };
        let d = crate::types::str::str_data(&s);
        let out = super::mem::PyMem_RawMalloc((d.len() + 1) * 4) as *mut libc::wchar_t;
        for i in 0..d.len() {
            *out.add(i) = d.get(i) as libc::wchar_t;
        }
        *out.add(d.len()) = 0;
        if !size.is_null() {
            *size = d.len();
        }
        out
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_EncodeLocale(text: *const libc::wchar_t, error_pos: *mut usize) -> *mut c_char {
    unsafe {
        let n = libc::wcslen(text);
        let cps: Vec<u32> = std::slice::from_raw_parts(text as *const u32, n).to_vec();
        let mut out = Vec::new();
        for (i, &c) in cps.iter().enumerate() {
            if (0xdc80..0xdd00).contains(&c) {
                out.push((c - 0xdc00) as u8);
            } else if let Some(ch) = char::from_u32(c) {
                let mut b = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
            } else {
                if !error_pos.is_null() {
                    *error_pos = i;
                }
                return std::ptr::null_mut();
            }
        }
        if !error_pos.is_null() {
            *error_pos = usize::MAX;
        }
        let p = super::mem::PyMem_Malloc(out.len() + 1) as *mut u8;
        std::ptr::copy_nonoverlapping(out.as_ptr(), p, out.len());
        *p.add(out.len()) = 0;
        p as *mut c_char
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_GetConstantBorrowed(id: std::ffi::c_uint) -> *mut PyObject {
    unsafe {
        let p = Py_GetConstant(id);
        p // all constants are immortal: the new reference can be dropped freely
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_GetConstant(id: std::ffi::c_uint) -> *mut PyObject {
    let o: PyRef = match id {
        0 => none::none_ref(),
        1 => bool_::new_bool(false),
        2 => bool_::new_bool(true),
        3 => none::ellipsis().to_ref(),
        4 => none::not_implemented().to_ref(),
        5 => int::new_int(0),
        6 => int::new_int(1),
        7 => pystr::new_str(""),
        8 => {
            static EMPTY: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
            let p = *EMPTY.get_or_init(|| {
                let b = bytes::new_bytes(b"");
                unsafe { make_immortal(b.as_ptr()) };
                b.into_raw() as usize
            });
            unsafe { PyRef::from_borrowed(p as *mut PyObject) }
        }
        9 => tuple::empty_tuple(),
        _ => {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
    };
    o.into_raw()
}

// ---------------------------------------------------------------- PyStatus

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PyStatus {
    pub _type: c_int,
    pub func: *const c_char,
    pub err_msg: *const c_char,
    pub exitcode: c_int,
}
#[unsafe(no_mangle)]
pub extern "C" fn PyStatus_Ok() -> PyStatus {
    PyStatus { _type: 0, func: std::ptr::null(), err_msg: std::ptr::null(), exitcode: 0 }
}
#[unsafe(no_mangle)]
pub extern "C" fn PyStatus_Error(msg: *const c_char) -> PyStatus {
    PyStatus { _type: 1, func: std::ptr::null(), err_msg: msg, exitcode: 0 }
}
#[unsafe(no_mangle)]
pub extern "C" fn PyStatus_NoMemory() -> PyStatus {
    PyStatus_Error(c"memory allocation failed".as_ptr())
}
#[unsafe(no_mangle)]
pub extern "C" fn PyStatus_Exit(code: c_int) -> PyStatus {
    PyStatus { _type: 2, func: std::ptr::null(), err_msg: std::ptr::null(), exitcode: code }
}
#[unsafe(no_mangle)]
pub extern "C" fn PyStatus_IsError(s: PyStatus) -> c_int {
    (s._type == 1) as c_int
}
#[unsafe(no_mangle)]
pub extern "C" fn PyStatus_IsExit(s: PyStatus) -> c_int {
    (s._type == 2) as c_int
}
#[unsafe(no_mangle)]
pub extern "C" fn PyStatus_Exception(s: PyStatus) -> c_int {
    (s._type != 0) as c_int
}

#[repr(C)]
pub struct PyWideStringList {
    pub length: Py_ssize_t,
    pub items: *mut *mut libc::wchar_t,
}
unsafe fn wcsdup_raw(s: *const libc::wchar_t) -> *mut libc::wchar_t {
    unsafe {
        let n = libc::wcslen(s);
        let p = super::mem::PyMem_RawMalloc((n + 1) * 4) as *mut libc::wchar_t;
        std::ptr::copy_nonoverlapping(s, p, n + 1);
        p
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyWideStringList_Insert(l: *mut PyWideStringList, index: Py_ssize_t, item: *const libc::wchar_t) -> PyStatus {
    unsafe {
        let len = (*l).length;
        if index < 0 {
            return PyStatus_Error(c"PyWideStringList_Insert index must be >= 0".as_ptr());
        }
        let index = index.min(len) as usize;
        let items = super::mem::PyMem_RawRealloc((*l).items as *mut c_void, (len as usize + 1) * 8) as *mut *mut libc::wchar_t;
        if items.is_null() {
            return PyStatus_NoMemory();
        }
        std::ptr::copy(items.add(index), items.add(index + 1), len as usize - index);
        *items.add(index) = wcsdup_raw(item);
        (*l).items = items;
        (*l).length = len + 1;
        PyStatus_Ok()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyWideStringList_Append(l: *mut PyWideStringList, item: *const libc::wchar_t) -> PyStatus {
    unsafe { PyWideStringList_Insert(l, (*l).length, item) }
}

// PyConfig / PyPreConfig: the runtime is configured by the launcher; these accept
// and ignore embedding configuration.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyPreConfig_InitPythonConfig(_c: *mut c_void) {}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyPreConfig_InitIsolatedConfig(_c: *mut c_void) {}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyConfig_InitPythonConfig(_c: *mut c_void) {}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyConfig_InitIsolatedConfig(_c: *mut c_void) {}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyConfig_Clear(_c: *mut c_void) {}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyConfig_SetString(_c: *mut c_void, field: *mut *mut libc::wchar_t, value: *const libc::wchar_t) -> PyStatus {
    unsafe {
        *field = if value.is_null() { std::ptr::null_mut() } else { wcsdup_raw(value) };
        PyStatus_Ok()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyConfig_SetBytesString(c: *mut c_void, field: *mut *mut libc::wchar_t, value: *const c_char) -> PyStatus {
    unsafe {
        if value.is_null() {
            *field = std::ptr::null_mut();
            return PyStatus_Ok();
        }
        let w = Py_DecodeLocale(value, std::ptr::null_mut());
        let r = PyConfig_SetString(c, field, w);
        super::mem::PyMem_RawFree(w as *mut c_void);
        r
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyConfig_Read(_c: *mut c_void) -> PyStatus {
    PyStatus_Ok()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyConfig_SetBytesArgv(_c: *mut c_void, _argc: Py_ssize_t, _argv: *const *mut c_char) -> PyStatus {
    PyStatus_Ok()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyConfig_SetArgv(_c: *mut c_void, _argc: Py_ssize_t, _argv: *const *mut libc::wchar_t) -> PyStatus {
    PyStatus_Ok()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyConfig_SetWideStringList(_c: *mut c_void, list: *mut PyWideStringList, length: Py_ssize_t, items: *mut *mut libc::wchar_t) -> PyStatus {
    unsafe {
        (*list).length = 0;
        (*list).items = std::ptr::null_mut();
        for i in 0..length.max(0) as usize {
            let s = PyWideStringList_Append(list, *items.add(i));
            if s._type != 0 {
                return s;
            }
        }
        PyStatus_Ok()
    }
}

// ---------------------------------------------------------------- PyInitConfig (PEP 741)

pub struct InitConfig {
    error: Option<std::ffi::CString>,
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInitConfig_Create() -> *mut InitConfig {
    Box::into_raw(Box::new(InitConfig { error: None }))
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInitConfig_Free(c: *mut InitConfig) {
    if !c.is_null() {
        drop(unsafe { Box::from_raw(c) });
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInitConfig_GetError(c: *mut InitConfig, err: *mut *const c_char) -> c_int {
    unsafe {
        match &(*c).error {
            Some(e) => {
                *err = e.as_ptr();
                1
            }
            None => {
                *err = std::ptr::null();
                0
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInitConfig_GetExitCode(_c: *mut InitConfig, _code: *mut c_int) -> c_int {
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInitConfig_HasOption(_c: *mut InitConfig, name: *const c_char) -> c_int {
    unsafe {
        let r = super::config::config_get(&cs(name)).is_ok();
        clear_exc();
        r as c_int
    }
}
unsafe fn set_err(c: *mut InitConfig, msg: &str) -> c_int {
    unsafe {
        (*c).error = std::ffi::CString::new(msg).ok();
        -1
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInitConfig_GetInt(c: *mut InitConfig, name: *const c_char, value: *mut i64) -> c_int {
    unsafe {
        match super::config::config_get(&cs(name)) {
            Ok(v) if int::is_int(&v) => {
                *value = int::as_i64(&v).unwrap_or(0);
                0
            }
            _ => {
                clear_exc();
                set_err(c, "invalid config option name or not an int")
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInitConfig_GetStr(c: *mut InitConfig, name: *const c_char, value: *mut *mut c_char) -> c_int {
    unsafe {
        match super::config::config_get(&cs(name)) {
            Ok(v) if pystr::is_str(&v) => {
                *value = pymem_cstring(pystr::to_string_lossy(&v).as_bytes());
                0
            }
            Ok(v) if none::is_none(&v) => {
                *value = std::ptr::null_mut();
                0
            }
            _ => {
                clear_exc();
                set_err(c, "invalid config option name or not a string")
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInitConfig_GetStrList(c: *mut InitConfig, name: *const c_char, length: *mut usize, items: *mut *mut *mut c_char) -> c_int {
    unsafe {
        match super::config::config_get(&cs(name)).and_then(|v| ab::collect_iterable(&v)) {
            Ok(v) => {
                let arr = super::mem::PyMem_Malloc(v.len().max(1) * 8) as *mut *mut c_char;
                for (i, s) in v.iter().enumerate() {
                    *arr.add(i) = pymem_cstring(pystr::to_string_lossy(s).as_bytes());
                }
                *length = v.len();
                *items = arr;
                0
            }
            Err(_) => {
                clear_exc();
                set_err(c, "invalid config option name or not a list")
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInitConfig_FreeStrList(length: usize, items: *mut *mut c_char) {
    unsafe {
        for i in 0..length {
            super::mem::PyMem_Free(*items.add(i) as *mut c_void);
        }
        super::mem::PyMem_Free(items as *mut c_void);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInitConfig_SetInt(_c: *mut InitConfig, _name: *const c_char, _v: i64) -> c_int {
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInitConfig_SetStr(_c: *mut InitConfig, _name: *const c_char, _v: *const c_char) -> c_int {
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInitConfig_SetStrList(_c: *mut InitConfig, _name: *const c_char, _n: usize, _items: *const *mut c_char) -> c_int {
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInitConfig_AddModule(_c: *mut InitConfig, name: *const c_char, initfunc: Option<unsafe extern "C" fn() -> *mut PyObject>) -> c_int {
    unsafe { super::import::PyImport_AppendInittab(name, initfunc) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_InitializeFromInitConfig(_c: *mut InitConfig) -> c_int {
    0
}
