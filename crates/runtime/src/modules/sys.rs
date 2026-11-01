//! `sys` module core: module registry, standard streams bootstrap, tracing hooks,
//! unraisable hook. (Full sys API is filled in by sys_full.rs.)

use crate::errors::*;
use crate::ffi::*;
use crate::interp::frame::Frame;
use crate::interp::Exit;
use crate::object::*;
use crate::threads::ThreadState;
use crate::types::{dict, list, none, str as pystr, tuple};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, Ordering::*};

static SYS_MODULE: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
static MODULES: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
pub static TRACING: AtomicBool = AtomicBool::new(false);
pub static OPTIMIZE: AtomicI32 = AtomicI32::new(0);

pub fn sys_module() -> PyRef {
    unsafe { PyRef::from_borrowed(SYS_MODULE.load(Acquire)) }
}
pub fn sys_dict() -> PyRef {
    crate::types::module::module_dict(&sys_module())
}
pub fn modules_dict() -> PyRef {
    unsafe { PyRef::from_borrowed(MODULES.load(Acquire)) }
}
pub fn get_sys_attr(nm: &str) -> Option<PyRef> {
    let p = SYS_MODULE.load(Acquire);
    if p.is_null() {
        return None;
    }
    dict::get_item_rs(&sys_dict(), nm)
}
pub fn set_sys_attr(nm: &str, v: &PyObject) -> PyResult<()> {
    dict::set_item_str(&sys_dict(), nm, v)
}
pub fn optimize_flag() -> i32 {
    OPTIMIZE.load(Relaxed)
}
#[inline(always)]
pub fn tracing_active() -> bool {
    TRACING.load(Relaxed)
}

// ---------------------------------------------------------------- tracing hooks
pub fn trace_call(ts: &ThreadState, f: &mut Frame) -> PyResult<()> {
    crate::modules::sys_trace::trace_call(ts, f)
}
pub fn trace_instr(ts: &ThreadState, f: &mut Frame) -> PyResult<()> {
    crate::modules::sys_trace::trace_instr(ts, f)
}
pub fn trace_exit(ts: &ThreadState, f: &mut Frame, e: &Exit) -> PyResult<()> {
    crate::modules::sys_trace::trace_exit(ts, f, e)
}
pub fn trace_exception(ts: &ThreadState, f: &mut Frame, e: &PyObject) -> PyResult<()> {
    crate::modules::sys_trace::trace_exception(ts, f, e)
}
pub fn trace_unwind(ts: &ThreadState, f: &mut Frame) -> PyResult<()> {
    crate::modules::sys_trace::trace_unwind(ts, f)
}

// ---------------------------------------------------------------- bootstrap stdio

#[repr(C)]
struct FdWriter {
    ob_base: PyObject,
    fd: i32,
}
static mut FDWRITER_TYPE: PyTypeObject = PyTypeObject::ZERO;

fn fdw_write(o: &PyObject, s: &PyObject) -> PyResult {
    let fd = unsafe { (*(o.as_ptr() as *const FdWriter)).fd };
    let text = if pystr::is_str(s) { pystr::to_string_lossy(s) } else { crate::abstract_::str_string(s)? };
    write_fd(fd, text.as_bytes());
    Ok(crate::types::int::new_int(pystr::str_len(s) as i64))
}
fn fdw_flush(_o: &PyObject) -> PyResult {
    Ok(none::none_ref())
}
pub fn write_fd(fd: i32, mut b: &[u8]) {
    while !b.is_empty() {
        let n = unsafe { libc::write(fd, b.as_ptr() as *const libc::c_void, b.len()) };
        if n <= 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return;
        }
        b = &b[n as usize..];
    }
}

fn new_fdwriter(fd: i32) -> PyRef {
    unsafe {
        let op = crate::alloc::alloc_object(&raw mut FDWRITER_TYPE, std::mem::size_of::<FdWriter>());
        (*(op as *mut FdWriter)).fd = fd;
        PyRef::from_raw(op)
    }
}

/// Write text to sys.stderr (or fd 2 as fallback).
pub fn write_stderr(s: &str) {
    if let Some(err) = get_sys_attr("stderr") {
        if !err.is(none::none()) {
            let t = pystr::new_str(s);
            if crate::abstract_::call_method_str(&err, "write", &[&t]).is_ok() {
                return;
            }
            clear_exc();
        }
    }
    write_fd(2, s.as_bytes());
}

/// sys.unraisablehook equivalent: print "msg: obj" and the current exception.
pub fn write_unraisable_with_obj(msg: &str, obj: Option<&PyObject>) {
    crate::modules::sys_full::write_unraisable(Some(msg), obj)
}

pub fn init_bootstrap_types() {
    unsafe {
        let t = &mut *(&raw mut FDWRITER_TYPE);
        t.tp_name = c"_bootstrap_stream".as_ptr();
        t.tp_basicsize = std::mem::size_of::<FdWriter>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT;
        t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
        t.tp_methods = crate::mdefs![("write", O, fdw_write), ("flush", NOARGS, fdw_flush)];
        let _ = crate::types::typeobject::type_ready(&raw mut FDWRITER_TYPE);
    }
}

/// Create the sys module with the minimal attributes needed during startup.
/// Replace the bootstrap writers by real io objects (TextIOWrapper over a buffered
/// FileIO).  Keeps the bootstrap streams if `_io` cannot be set up.
pub fn init_stdio() {
    // builtins.open is io.open
    match crate::import::import_absolute("_io").and_then(|io| crate::abstract_::getattr_str(&io, "open")) {
        Ok(open) => {
            let b = crate::types::module::module_dict(&crate::builtins::builtins_module());
            let _ = dict::set_item_str(&b, "open", &open);
        }
        Err(_) => clear_exc(),
    }
    if let Err(_) = init_stdio_impl() {
        if std::env::var_os("RPY_DEBUG_STDIO").is_some() {
            crate::print_exception_and_clear();
        }
        clear_exc();
    }
}

fn create_stdio(fd: i32, write: bool, name: &str, errors: &str, unbuffered: bool, force_line: bool) -> PyResult<PyRef> {
    use crate::abstract_ as ab;
    use crate::types::bool_;
    // closed / invalid descriptor -> None
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        return Ok(none::none_ref());
    }
    let io = crate::import::import_absolute("_io")?;
    let mode = pystr::new_str(if write { "wb" } else { "rb" });
    let fileio = ab::getattr_str(&io, "FileIO")?;
    let kwn = tuple::new_tuple(vec![pystr::new_str("closefd")]);
    let raw = ab::call_kw(&fileio, &[&crate::types::int::new_int(fd as i64), &mode, bool_::false_obj()], Some(&kwn))?;
    let isatty = unsafe { libc::isatty(fd) } == 1;
    let buffered = if unbuffered && write {
        raw.clone()
    } else {
        let cls = ab::getattr_str(&io, if write { "BufferedWriter" } else { "BufferedReader" })?;
        ab::call(&cls, &[&raw])?
    };
    ab::setattr(&raw, crate::name!("name"), &pystr::new_str(name))?;
    let tw = ab::getattr_str(&io, "TextIOWrapper")?;
    let line_buffering = !unbuffered && (isatty || force_line);
    let write_through = unbuffered && write;
    let kwn = tuple::new_tuple(vec![
        pystr::new_str("encoding"),
        pystr::new_str("errors"),
        pystr::new_str("newline"),
        pystr::new_str("line_buffering"),
        pystr::new_str("write_through"),
    ]);
    let text = ab::call_kw(
        &tw,
        &[
            &buffered,
            &pystr::new_str("utf-8"),
            &pystr::new_str(errors),
            &pystr::new_str("\n"),
            &bool_::new_bool(line_buffering),
            &bool_::new_bool(write_through),
        ],
        Some(&kwn),
    )?;
    ab::setattr(&text, crate::name!("mode"), &pystr::new_str(if write { "w" } else { "r" }))?;
    Ok(text)
}

fn init_stdio_impl() -> PyResult<()> {
    let unbuffered = crate::modules::sys_full::config().unbuffered;
    let stdin = create_stdio(0, false, "<stdin>", "surrogateescape", unbuffered, false)?;
    let stdout = create_stdio(1, true, "<stdout>", "surrogateescape", unbuffered, false)?;
    let stderr = create_stdio(2, true, "<stderr>", "backslashreplace", unbuffered, true)?;
    let d = sys_dict();
    dict::set_item_str(&d, "stdin", &stdin)?;
    dict::set_item_str(&d, "__stdin__", &stdin)?;
    dict::set_item_str(&d, "stdout", &stdout)?;
    dict::set_item_str(&d, "__stdout__", &stdout)?;
    dict::set_item_str(&d, "stderr", &stderr)?;
    dict::set_item_str(&d, "__stderr__", &stderr)?;
    Ok(())
}

pub fn create_sys_module(argv: &[String]) -> PyResult<PyRef> {
    let m = crate::types::module::new_module("sys")?;
    SYS_MODULE.store(m.clone().into_raw(), Release);
    let modules = dict::new_dict();
    MODULES.store(modules.clone().into_raw(), Release);
    let d = crate::types::module::module_dict(&m);
    dict::set_item_str(&d, "modules", &modules)?;
    dict::set_item_str(&modules, "sys", &m)?;
    let argv_l = list::new_list(argv.iter().map(|a| pystr::new_str(a)).collect());
    dict::set_item_str(&d, "argv", &argv_l)?;
    dict::set_item_str(&d, "orig_argv", &list::new_list(argv.iter().map(|a| pystr::new_str(a)).collect()))?;
    let out = new_fdwriter(1);
    let err = new_fdwriter(2);
    dict::set_item_str(&d, "stdout", &out)?;
    dict::set_item_str(&d, "stderr", &err)?;
    dict::set_item_str(&d, "__stdout__", &out)?;
    dict::set_item_str(&d, "__stderr__", &err)?;
    dict::set_item_str(&d, "path", &list::new_list_empty())?;
    dict::set_item_str(&d, "meta_path", &list::new_list_empty())?;
    dict::set_item_str(&d, "path_hooks", &list::new_list_empty())?;
    dict::set_item_str(&d, "path_importer_cache", &dict::new_dict())?;
    crate::modules::sys_full::populate(&d)?;
    crate::types::asyncgen::populate_sys(&d, &m)?;
    Ok(m)
}

pub static INT_MAX_STR_DIGITS: AtomicI32 = AtomicI32::new(4300);
pub static BYTES_WARNING: AtomicI32 = AtomicI32::new(0);
pub fn int_max_str_digits() -> i32 {
    INT_MAX_STR_DIGITS.load(Relaxed)
}
pub fn bytes_warning() -> i32 {
    BYTES_WARNING.load(Relaxed)
}
