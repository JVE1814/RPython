//! Assorted: signals (PyErr_CheckSignals), PyOS_* helpers, sys (PySys_*), audit
//! hooks, files (PyFile_*), marshal, time, monitoring (no-op events), perf maps.

use super::*;
use crate::abstract_ as ab;
use crate::types::{bytes, dict, int, none, str as pystr, tuple};
use std::ffi::{VaList, c_long, c_ulong, c_void};
use std::sync::Mutex;

/// Run pending signal handlers (PyErr_CheckSignals).
pub(crate) fn check_signals() -> PyResult<()> {
    crate::modules::signal::run_pending_handlers(crate::threads::current())
}

/// os.fspath semantics.
pub(crate) fn fspath(o: &PyObject) -> PyResult {
    if pystr::is_str(o) || bytes::is_bytes(o) {
        return Ok(o.to_ref());
    }
    match ab::lookup_special(o, crate::name!("__fspath__"))? {
        Some(f) => {
            let r = ab::call(&f, &[])?;
            if pystr::is_str(&r) || bytes::is_bytes(&r) {
                Ok(r)
            } else {
                Err(type_error(&format!(
                    "expected {}.__fspath__() to return str or bytes, not {}",
                    tname(o),
                    tname(&r)
                )))
            }
        }
        None => Err(type_error(&format!("expected str, bytes or os.PathLike object, not {}", tname(o)))),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_CheckSignals() -> c_int {
    ret_i(check_signals())
}
static INTERRUPTED: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SetInterruptEx(signum: c_int) -> c_int {
    if signum < 1 || signum >= 65 {
        return -1;
    }
    INTERRUPTED.store(signum, std::sync::atomic::Ordering::Relaxed);
    unsafe {
        libc::raise(signum);
    }
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SetInterrupt() {
    unsafe {
        PyErr_SetInterruptEx(libc::SIGINT);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_InterruptOccurred() -> c_int {
    (INTERRUPTED.swap(0, std::sync::atomic::Ordering::Relaxed) == libc::SIGINT) as c_int
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySignal_SetWakeupFd(fd: c_int) -> c_int {
    static FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
    FD.swap(fd, std::sync::atomic::Ordering::Relaxed)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_FSPath(o: *mut PyObject) -> *mut PyObject {
    unsafe { ret(fspath(&*o)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_getsig(sig: c_int) -> libc::sighandler_t {
    unsafe {
        let mut old: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(sig, std::ptr::null(), &mut old) == -1 {
            return libc::SIG_ERR;
        }
        old.sa_sigaction
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_setsig(sig: c_int, handler: libc::sighandler_t) -> libc::sighandler_t {
    unsafe {
        let mut new: libc::sigaction = std::mem::zeroed();
        let mut old: libc::sigaction = std::mem::zeroed();
        new.sa_sigaction = handler;
        libc::sigemptyset(&mut new.sa_mask);
        new.sa_flags = libc::SA_ONSTACK;
        if libc::sigaction(sig, &new, &mut old) != 0 {
            return libc::SIG_ERR;
        }
        old.sa_sigaction
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_BeforeFork() {}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_AfterFork_Parent() {}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_AfterFork_Child() {}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_AfterFork() {}
#[unsafe(no_mangle)]
pub static mut PyOS_InputHook: Option<unsafe extern "C" fn() -> c_int> = None;
#[unsafe(no_mangle)]
pub static mut PyOS_ReadlineFunctionPointer: Option<unsafe extern "C" fn(*mut libc::FILE, *mut libc::FILE, *const c_char) -> *mut c_char> = None;
/// PyOS_Readline: prompt on `out`, read a line from `inp` (PyMem_RawMalloc'd result).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_Readline(inp: *mut libc::FILE, out: *mut libc::FILE, prompt: *const c_char) -> *mut c_char {
    unsafe {
        if let Some(f) = PyOS_ReadlineFunctionPointer {
            return f(inp, out, prompt);
        }
        if !prompt.is_null() {
            libc::fputs(prompt, out);
            libc::fflush(out);
        }
        let mut line: *mut c_char = std::ptr::null_mut();
        let mut cap: usize = 0;
        let n = libc::getline(&mut line, &mut cap, inp);
        let res = if n < 0 {
            // EOF: empty string
            let p = super::mem::PyMem_RawMalloc(1) as *mut c_char;
            *p = 0;
            p
        } else {
            let p = super::mem::PyMem_RawMalloc(n as usize + 1) as *mut c_char;
            std::ptr::copy_nonoverlapping(line, p, n as usize + 1);
            p
        };
        libc::free(line as *mut c_void);
        res
    }
}

/// Longest valid float literal prefix (sign, digits/point/exponent, inf/nan) of `s`.
fn float_prefix(s: &[u8]) -> usize {
    let mut i = 0;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        i += 1;
    }
    let rest = &s[i..];
    let lower: Vec<u8> = rest.iter().take(8).map(|c| c.to_ascii_lowercase()).collect();
    if lower.starts_with(b"infinity") {
        return i + 8;
    }
    if lower.starts_with(b"inf") || lower.starts_with(b"nan") {
        return i + 3;
    }
    let start = i;
    let mut digits = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if i < s.len() && s[i] == b'.' {
        i += 1;
        while i < s.len() && s[i].is_ascii_digit() {
            i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return 0;
    }
    if i < s.len() && (s[i] == b'e' || s[i] == b'E') {
        let mut j = i + 1;
        if j < s.len() && (s[j] == b'+' || s[j] == b'-') {
            j += 1;
        }
        let ds = j;
        while j < s.len() && s[j].is_ascii_digit() {
            j += 1;
        }
        if j > ds {
            i = j;
        }
    }
    let _ = start;
    i
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_string_to_double(s: *const c_char, endptr: *mut *mut c_char, overflow_exception: *mut PyObject) -> f64 {
    unsafe {
        let b = CStr::from_ptr(s).to_bytes();
        let n = float_prefix(b);
        let fail = || {
            let _ = value_error(&format!("could not convert string to float: {}", ab::repr_string(&pystr::new_str(&String::from_utf8_lossy(b))).unwrap_or_default()));
        };
        if n == 0 || (endptr.is_null() && n != b.len()) {
            if !endptr.is_null() {
                *endptr = s as *mut c_char;
            }
            fail();
            return -1.0;
        }
        if !endptr.is_null() {
            *endptr = s.add(n) as *mut c_char;
        }
        let txt = std::str::from_utf8_unchecked(&b[..n]);
        let v: f64 = txt.parse().unwrap_or(f64::NAN);
        if v.is_infinite() && !txt.to_ascii_lowercase().contains("inf") && !overflow_exception.is_null() {
            let _ = crate::errors::new_err(
                overflow_exception as *mut PyTypeObject,
                &format!("value too large to convert to float: {}", ab::repr_string(&pystr::new_str(txt)).unwrap_or_default()),
            );
            return -1.0;
        }
        v
    }
}
/// PyOS_double_to_string(val, format_code, precision, flags, type)
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_double_to_string(val: f64, code: c_char, precision: c_int, flags: c_int, ptype: *mut c_int) -> *mut c_char {
    unsafe {
        let code = code as u8;
        let r = (|| -> PyResult<String> {
            let mut s = if code == b'r' {
                ab::repr_string(&crate::types::float::new_float(val))?
            } else {
                let mut spec = String::new();
                if flags & 4 != 0 {
                    spec.push('#');
                }
                spec.push('.');
                spec.push_str(&precision.to_string());
                spec.push(code as char);
                let r = ab::format(&crate::types::float::new_float(val), &pystr::new_str(&spec))?;
                pystr::to_string_lossy(&r)
            };
            if flags & 8 != 0 && s.starts_with('-') && val == 0.0 {
                s.remove(0);
            }
            if flags & 1 != 0 && !s.starts_with('-') {
                s.insert(0, '+');
            }
            if flags & 2 != 0 && val.is_finite() && !s.contains(['.', 'e', 'E']) {
                s.push_str(".0");
            }
            Ok(s)
        })();
        match r {
            Ok(s) => {
                if !ptype.is_null() {
                    *ptype = if val.is_nan() { 2 } else if val.is_infinite() { 1 } else { 0 };
                }
                pymem_cstring(s.as_bytes())
            }
            Err(_) => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_strtoul(s: *const c_char, end: *mut *mut c_char, base: c_int) -> c_ulong {
    unsafe {
        *libc::__errno_location() = 0;
        libc::strtoul(s, end, base)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_strtol(s: *const c_char, end: *mut *mut c_char, base: c_int) -> c_long {
    unsafe {
        *libc::__errno_location() = 0;
        libc::strtol(s, end, base)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_mystrnicmp(a: *const c_char, b: *const c_char, n: Py_ssize_t) -> c_int {
    unsafe {
        let mut i = 0;
        while i < n {
            let x = (*a.offset(i) as u8).to_ascii_lowercase();
            let y = (*b.offset(i) as u8).to_ascii_lowercase();
            if x != y || x == 0 {
                return x as c_int - y as c_int;
            }
            i += 1;
        }
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_mystricmp(a: *const c_char, b: *const c_char) -> c_int {
    unsafe { PyOS_mystrnicmp(a, b, Py_ssize_t::MAX) }
}

// ---------------------------------------------------------------- sys

unsafe fn sys_get(name: &PyObject) -> Option<PyRef> {
    let d = crate::modules::sys::sys_dict();
    dict::get_item(&d, name).ok().flatten()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySys_GetObject(name: *const c_char) -> *mut PyObject {
    unsafe {
        let saved = take_exc();
        let r = match cstr_obj(name) {
            Ok(n) => sys_get(&n).map_or(std::ptr::null_mut(), |v| {
                let p = v.as_ptr();
                drop(v); // borrowed from sys.__dict__
                p
            }),
            Err(_) => std::ptr::null_mut(),
        };
        clear_exc();
        if let Some(e) = saved {
            set_exc(e);
        }
        r
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySys_GetAttr(name: *mut PyObject) -> *mut PyObject {
    unsafe {
        match sys_get(&*name) {
            Some(v) => v.into_raw(),
            None => {
                let _ = crate::err!(RuntimeError, "lost sys.{}", pystr::to_string_lossy(&*name));
                std::ptr::null_mut()
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySys_GetAttrString(name: *const c_char) -> *mut PyObject {
    unsafe {
        match cstr_obj(name) {
            Ok(n) => PySys_GetAttr(n.as_ptr()),
            Err(_) => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySys_GetOptionalAttr(name: *mut PyObject, result: *mut *mut PyObject) -> c_int {
    unsafe {
        match sys_get(&*name) {
            Some(v) => {
                *result = v.into_raw();
                1
            }
            None => {
                *result = std::ptr::null_mut();
                0
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySys_GetOptionalAttrString(name: *const c_char, result: *mut *mut PyObject) -> c_int {
    unsafe {
        match cstr_obj(name) {
            Ok(n) => PySys_GetOptionalAttr(n.as_ptr(), result),
            Err(_) => {
                *result = std::ptr::null_mut();
                -1
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySys_SetObject(name: *const c_char, v: *mut PyObject) -> c_int {
    unsafe {
        let d = crate::modules::sys::sys_dict();
        let Ok(n) = cstr_obj(name) else { return -1 };
        if v.is_null() {
            let _ = dict::del_item(&d, &n);
            return 0;
        }
        ret_i(dict::set_item(&d, &n, &*v))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySys_GetXOptions() -> *mut PyObject {
    unsafe {
        let d = crate::modules::sys::sys_dict();
        match dict::get_item_rs(&d, "_xoptions") {
            Some(x) => {
                let p = x.as_ptr();
                drop(x);
                p
            }
            None => {
                let x = dict::new_dict();
                let _ = dict::set_item_str(&d, "_xoptions", &x);
                let p = x.as_ptr();
                drop(x);
                p
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySys_SetArgvEx(argc: c_int, argv: *mut *mut libc::wchar_t, _updatepath: c_int) {
    unsafe {
        let mut v = Vec::new();
        for i in 0..argc as usize {
            let p = *argv.add(i);
            let o = super::unicode::PyUnicode_FromWideChar(p, -1);
            if o.is_null() {
                clear_exc();
                return;
            }
            v.push(PyRef::from_raw(o));
        }
        let _ = crate::modules::sys::set_sys_attr("argv", &crate::types::list::new_list(v));
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySys_SetArgv(argc: c_int, argv: *mut *mut libc::wchar_t) {
    unsafe { PySys_SetArgvEx(argc, argv, 1) }
}

// audit hooks
type AuditHook = unsafe extern "C" fn(*const c_char, *mut PyObject, *mut c_void) -> c_int;
static AUDIT_HOOKS: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySys_AddAuditHook(hook: Option<AuditHook>, data: *mut c_void) -> c_int {
    unsafe {
        // existing hooks are notified first ("sys.addaudithook" event)
        if PySys_AuditTuple(c"sys.addaudithook".as_ptr(), std::ptr::null_mut()) < 0 {
            if exc_matches(crate::types::exceptions::RuntimeError()) {
                clear_exc();
                return 0;
            }
            return -1;
        }
        let Some(h) = hook else { return -1 };
        AUDIT_HOOKS.lock().unwrap().push((h as usize, data as usize));
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySys_AuditTuple(event: *const c_char, args: *mut PyObject) -> c_int {
    unsafe {
        let hooks: Vec<(usize, usize)> = AUDIT_HOOKS.lock().unwrap().clone();
        let t = match opt(args) {
            Some(a) => {
                if !tuple::is_tuple(a) {
                    let _ = type_error(&format!("args must be tuple, got {}", tname(a)));
                    return -1;
                }
                a.to_ref()
            }
            None => tuple::empty_tuple(),
        };
        for (h, d) in hooks {
            let f: AuditHook = std::mem::transmute(h);
            if f(event, t.as_ptr(), d as *mut c_void) < 0 {
                return -1;
            }
        }
        // Python-level hooks
        if let Some(audit) = crate::modules::sys::get_sys_attr("audit") {
            let ev = match cstr_obj(event) {
                Ok(e) => e,
                Err(_) => return -1,
            };
            let mut a: Vec<&PyObject> = vec![&ev];
            a.extend_from_slice(tuple::tuple_items(&t));
            if ab::call(&audit, &a).is_err() {
                return -1;
            }
        }
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySys_Audit(event: *const c_char, fmt: *const c_char, mut args: ...) -> c_int {
    unsafe {
        let has_hooks = !AUDIT_HOOKS.lock().unwrap().is_empty() || crate::modules::sys::get_sys_attr("audit").is_some();
        if !has_hooks {
            return 0;
        }
        let t = if fmt.is_null() || *fmt == 0 {
            tuple::empty_tuple()
        } else {
            let v = super::varargs::build_value(fmt, &mut args);
            if v.is_null() {
                return -1;
            }
            let v = PyRef::from_raw(v);
            if tuple::is_tuple(&v) { v } else { tuple::new_tuple(vec![v]) }
        };
        PySys_AuditTuple(event, t.as_ptr())
    }
}

// ---------------------------------------------------------------- files

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFile_FromFd(
    fd: c_int,
    name: *const c_char,
    mode: *const c_char,
    buffering: c_int,
    encoding: *const c_char,
    errors: *const c_char,
    newline: *const c_char,
    closefd: c_int,
) -> *mut PyObject {
    unsafe {
        ret((|| -> PyResult {
            let io = crate::import::import_absolute("io")?;
            let open = ab::getattr_str(&io, "open")?;
            let s = |p: *const c_char| -> PyResult { if p.is_null() { Ok(none::none_ref()) } else { cstr_obj(p) } };
            let _ = name;
            ab::call(
                &open,
                &[&int::new_int(fd as i64), &*s(mode)?, &int::new_int(buffering as i64), &*s(encoding)?, &*s(errors)?, &*s(newline)?, &crate::types::bool_::new_bool(closefd != 0)],
            )
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFile_GetLine(f: *mut PyObject, n: c_int) -> *mut PyObject {
    unsafe {
        ret((|| -> PyResult {
            let r = if n <= 0 { ab::call_method_str(&*f, "readline", &[])? } else { ab::call_method_str(&*f, "readline", &[&int::new_int(n as i64)])? };
            if n < 0 {
                let len = ab::length(&r)?;
                if len == 0 {
                    return Err(crate::err!(EOFError, "EOF when reading a line"));
                }
                if pystr::is_str(&r) {
                    let d = pystr::str_data(&r);
                    if d.get(len - 1) == b'\n' as u32 {
                        let v: Vec<u32> = (0..len - 1).map(|i| d.get(i)).collect();
                        return Ok(pystr::from_codepoints(&v));
                    }
                } else if bytes::is_bytes(&r) {
                    let b = bytes::bytes_data(&r);
                    if b.last() == Some(&b'\n') {
                        return Ok(bytes::new_bytes(&b[..len - 1]));
                    }
                }
            }
            Ok(r)
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFile_WriteObject(v: *mut PyObject, f: *mut PyObject, flags: c_int) -> c_int {
    unsafe {
        if f.is_null() {
            let _ = type_error("writeobject with NULL file");
            return -1;
        }
        ret_i((|| -> PyResult<()> {
            // a NULL object is written as <NULL>, like PyObject_Str/Repr(NULL)
            let s = if v.is_null() { pystr::new_str("<NULL>") } else if flags & 1 != 0 { ab::str(&*v)? } else { ab::repr(&*v)? };
            ab::call_method_str(&*f, "write", &[&s])?;
            Ok(())
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFile_WriteString(s: *const c_char, f: *mut PyObject) -> c_int {
    unsafe {
        if f.is_null() {
            if !err_occurred() {
                let _ = system_error("null file for PyFile_WriteString");
            }
            return -1;
        }
        if err_occurred() {
            return -1;
        }
        ret_i((|| -> PyResult<()> {
            ab::call_method_str(&*f, "write", &[&*cstr_obj(s)?])?;
            Ok(())
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFile_NewStdPrinter(fd: c_int) -> *mut PyObject {
    unsafe { PyFile_FromFd(fd, std::ptr::null(), c"w".as_ptr(), -1, c"utf-8".as_ptr(), c"backslashreplace".as_ptr(), std::ptr::null(), 0) }
}
static OPEN_CODE_HOOK: Mutex<(usize, usize)> = Mutex::new((0, 0));
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFile_SetOpenCodeHook(hook: *mut c_void, data: *mut c_void) -> c_int {
    let mut g = OPEN_CODE_HOOK.lock().unwrap();
    if g.0 != 0 {
        return -1;
    }
    *g = (hook as usize, data as usize);
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFile_OpenCodeObject(path: *mut PyObject) -> *mut PyObject {
    unsafe {
        if path.is_null() || !pystr::is_str(&*path) {
            let _ = type_error(&format!("'path' must be 'str', not '{}'", if path.is_null() { "NULL".into() } else { tname(&*path) }));
            return std::ptr::null_mut();
        }
        let (h, d) = *OPEN_CODE_HOOK.lock().unwrap();
        if h != 0 {
            let f: unsafe extern "C" fn(*mut PyObject, *mut c_void) -> *mut PyObject = std::mem::transmute(h);
            return f(path, d as *mut c_void);
        }
        ret((|| -> PyResult {
            let io = crate::import::import_absolute("io")?;
            let open = ab::getattr_str(&io, "open")?;
            ab::call(&open, &[&*path, &pystr::new_str("rb")])
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFile_OpenCode(p: *const c_char) -> *mut PyObject {
    unsafe {
        match cstr_obj(p) {
            Ok(s) => PyFile_OpenCodeObject(s.as_ptr()),
            Err(_) => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_UniversalNewlineFgets(buf: *mut c_char, n: c_int, stream: *mut libc::FILE, _fobj: *mut PyObject) -> *mut c_char {
    unsafe {
        if n <= 0 {
            return std::ptr::null_mut();
        }
        let mut i = 0;
        while i < n - 1 {
            let c = libc::fgetc(stream);
            if c == libc::EOF {
                break;
            }
            let mut c = c as u8;
            if c == b'\r' {
                let c2 = libc::fgetc(stream);
                if c2 != b'\n' as c_int && c2 != libc::EOF {
                    libc::ungetc(c2, stream);
                }
                c = b'\n';
            }
            *buf.add(i as usize) = c as c_char;
            i += 1;
            if c == b'\n' {
                break;
            }
        }
        *buf.add(i as usize) = 0;
        if i == 0 { std::ptr::null_mut() } else { buf }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_fopen(path: *mut PyObject, mode: *const c_char) -> *mut libc::FILE {
    unsafe {
        let p = match fspath(&*path) {
            Ok(p) => p,
            Err(_) => return std::ptr::null_mut(),
        };
        let b = if pystr::is_str(&p) {
            match super::unicode::encode_fs(&p) {
                Ok(b) => b,
                Err(_) => return std::ptr::null_mut(),
            }
        } else {
            p
        };
        let mut c = bytes::bytes_data(&b).to_vec();
        c.push(0);
        let f = libc::fopen(c.as_ptr() as *const c_char, mode);
        if f.is_null() {
            let _ = super::errors::PyErr_SetFromErrnoWithFilenameObject(crate::types::exceptions::OSError() as *mut PyObject, path);
        }
        f
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_fclose(f: *mut libc::FILE) -> c_int {
    unsafe { libc::fclose(f) }
}

// marshal: the PyMarshal_* entry points are exported by modules/marshal.rs.

// ---------------------------------------------------------------- time

fn clock_ns(clock: libc::clockid_t) -> i64 {
    unsafe {
        let mut ts: libc::timespec = std::mem::zeroed();
        libc::clock_gettime(clock, &mut ts);
        ts.tv_sec * 1_000_000_000 + ts.tv_nsec
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyTime_AsSecondsDouble(t: i64) -> f64 {
    if t % 1_000_000_000 == 0 {
        (t / 1_000_000_000) as f64
    } else {
        t as f64 / 1e9
    }
}
macro_rules! clockfn {
    ($name:ident, $clock:expr) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(result: *mut i64) -> c_int {
            unsafe {
                *result = clock_ns($clock);
            }
            0
        }
    };
}
clockfn!(PyTime_Monotonic, libc::CLOCK_MONOTONIC);
clockfn!(PyTime_MonotonicRaw, libc::CLOCK_MONOTONIC);
clockfn!(PyTime_PerfCounter, libc::CLOCK_MONOTONIC);
clockfn!(PyTime_PerfCounterRaw, libc::CLOCK_MONOTONIC);
clockfn!(PyTime_Time, libc::CLOCK_REALTIME);
clockfn!(PyTime_TimeRaw, libc::CLOCK_REALTIME);

// monitoring (PEP 669 C API): see modules/sys_trace_api.rs

// ---------------------------------------------------------------- perf maps / tracebacks

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_PerfMapState_Init() -> c_int {
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_PerfMapState_Fini() {}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_WritePerfMapEntry(_code_addr: *const c_void, _size: std::ffi::c_uint, _name: *const c_char) -> c_int {
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_CopyPerfMapFile(_parent: *const c_char) -> c_int {
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_PerfTrampoline_CompileCode(_co: *mut PyObject) -> c_int {
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_PerfTrampoline_SetPersistAfterFork(_enable: c_int) -> c_int {
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_DumpTraceback(fd: c_int, _ts: *mut c_void) -> *const c_char {
    let msg = b"Stack (most recent call first):\n  <no Python frame>\n";
    unsafe {
        libc::write(fd, msg.as_ptr() as *const c_void, msg.len());
    }
    std::ptr::null()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_DumpTracebackThreads(fd: c_int, _interp: *mut c_void, ts: *mut c_void) -> *const c_char {
    unsafe { PyUnstable_DumpTraceback(fd, ts) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_Exc_PrepReraiseStar(orig: *mut PyObject, excs: *mut PyObject) -> *mut PyObject {
    unsafe {
        // keep the raised exceptions that are not the original's leaves
        let items = crate::types::list::list_to_vec(&*excs);
        let raised: Vec<PyRef> = items.into_iter().filter(|e| !none::is_none(e)).collect();
        if raised.is_empty() {
            return none::none_ref().into_raw();
        }
        if raised.len() == 1 {
            return raised[0].clone().into_raw();
        }
        let _ = orig;
        ret((|| -> PyResult {
            let b = crate::builtins::builtins_module();
            let eg = ab::getattr_str(&b, "ExceptionGroup")?;
            ab::call(&eg, &[&pystr::new_str(""), &crate::types::list::new_list(raised)])
        })())
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_GetThreadLocal_Addr() -> *mut c_void {
    crate::threads::current_mut() as *mut c_void
}

#[allow(dead_code)]
fn _unused(_: VaList) {}
