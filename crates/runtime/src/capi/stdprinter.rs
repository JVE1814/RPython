//! (capi3) PyFile_NewStdPrinter: a minimal unbuffered text writer for the standard
//! output/error file descriptors, usable before (or without) the io module. `write`
//! encodes UTF-8 with backslashreplace and returns the number of bytes written; flush
//! and close do nothing; the object cannot be created from Python.

use super::*;
use crate::object::PyRef;
use crate::types::typeobject::*;
use crate::types::{bool_, int, none, str as pystr};
use std::sync::Once;

#[repr(C)]
struct StdPrinter {
    ob_base: PyObject,
    fd: c_int,
}

static mut PRINTER_TYPE: PyTypeObject = PyTypeObject::ZERO;
static INIT: Once = Once::new();

fn fd_of(o: &PyObject) -> c_int {
    unsafe { (*(o.as_ptr() as *const StdPrinter)).fd }
}

fn sp_write(o: &PyObject, s: &PyObject) -> PyResult {
    if !pystr::is_str(s) {
        return Err(type_error(&format!("write() argument must be str, not {}", super::tname(s))));
    }
    let b = crate::abstract_::call_method_str(s, "encode", &[&pystr::new_str("utf-8"), &pystr::new_str("backslashreplace")])?;
    let data = crate::types::bytes::bytes_data(&b);
    let fd = fd_of(o);
    let mut off = 0;
    while off < data.len() {
        let n = unsafe { libc::write(fd, data[off..].as_ptr() as *const std::ffi::c_void, data.len() - off) };
        if n < 0 {
            let e = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if e == libc::EINTR {
                crate::modules::signal::check_signals()?;
                continue;
            }
            // like a closed stream: report nothing written rather than fail
            if e == libc::EAGAIN || e == libc::EBADF {
                return Ok(none::none_ref());
            }
            return Err(crate::types::exceptions::os_error_from_errno(e, None, None));
        }
        off += n as usize;
    }
    Ok(int::new_int(data.len() as i64))
}
fn sp_fileno(o: &PyObject) -> PyResult {
    Ok(int::new_int(fd_of(o) as i64))
}
fn sp_isatty(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(unsafe { libc::isatty(fd_of(o)) } == 1))
}
fn sp_nothing(_o: &PyObject) -> PyResult {
    Ok(none::none_ref())
}
fn sp_closed(_o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(false))
}
fn sp_encoding(_o: &PyObject) -> PyResult {
    Ok(none::none_ref())
}
fn sp_mode(_o: &PyObject) -> PyResult {
    Ok(pystr::new_str("w"))
}
fn sp_repr(o: &PyObject) -> PyResult {
    Ok(pystr::new_str(&format!("<stdprinter(fd={}) object at {:p}>", fd_of(o), o.as_ptr())))
}
unsafe extern "C" fn sp_repr_c(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(sp_repr(&*o)) }
}
unsafe extern "C" fn sp_dealloc(o: *mut PyObject) {
    unsafe { crate::alloc::free_object(o) }
}

fn printer_type() -> PyResult<*mut PyTypeObject> {
    let mut res = Ok(());
    INIT.call_once(|| unsafe {
        let t = &mut *(&raw mut PRINTER_TYPE);
        t.tp_name = c"stderrprinter".as_ptr();
        t.tp_basicsize = std::mem::size_of::<StdPrinter>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_DISALLOW_INSTANTIATION | Py_TPFLAGS_IMMUTABLETYPE;
        t.tp_dealloc = Some(sp_dealloc);
        t.tp_repr = Some(sp_repr_c);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_methods = crate::mdefs![
            ("write", O, sp_write),
            ("fileno", NOARGS, sp_fileno),
            ("isatty", NOARGS, sp_isatty),
            ("flush", NOARGS, sp_nothing),
            ("close", NOARGS, sp_nothing),
        ];
        t.tp_getset = crate::getset_table![("closed", sp_closed), ("encoding", sp_encoding), ("mode", sp_mode)];
        res = type_ready(&raw mut PRINTER_TYPE);
    });
    res?;
    Ok(&raw mut PRINTER_TYPE)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFile_NewStdPrinter(fd: c_int) -> *mut PyObject {
    unsafe {
        if fd != 1 && fd != 2 {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        let tp = match printer_type() {
            Ok(t) => t,
            Err(_) => return std::ptr::null_mut(),
        };
        let op = crate::alloc::alloc_object(tp, std::mem::size_of::<StdPrinter>());
        if op.is_null() {
            let _ = memory_error();
            return op;
        }
        (*(op as *mut StdPrinter)).fd = fd;
        let r: PyRef = PyRef::from_raw(op);
        r.into_raw()
    }
}
