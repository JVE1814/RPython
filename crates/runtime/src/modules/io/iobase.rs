//! _IOBase, _RawIOBase, _BufferedIOBase, _TextIOBase.
use super::*;
use crate::modules::bufview::BufView;
use crate::types::{bytes, list};
use std::ffi::{c_int, c_void};

pub static mut IOBASE_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut RAWIOBASE_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut BUFFEREDIOBASE_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut TEXTIOBASE_TYPE: PyTypeObject = PyTypeObject::ZERO;

pub fn iobase_type() -> *mut PyTypeObject {
    &raw mut IOBASE_TYPE
}
pub fn rawiobase_type() -> *mut PyTypeObject {
    &raw mut RAWIOBASE_TYPE
}
pub fn bufferediobase_type() -> *mut PyTypeObject {
    &raw mut BUFFEREDIOBASE_TYPE
}
pub fn textiobase_type() -> *mut PyTypeObject {
    &raw mut TEXTIOBASE_TYPE
}

// ---------------------------------------------------------------- finalization

/// tp_finalize shared by all io classes: close() the object unless already closed.
pub unsafe extern "C" fn iobase_finalize(op: *mut PyObject) {
    unsafe {
        let o = &*op;
        let saved = take_exc();
        let closed = match ab::getattr(o, crate::name!("closed")) {
            Ok(c) => match ab::is_true(&c) {
                Ok(b) => Some(b),
                Err(_) => {
                    clear_exc();
                    None
                }
            },
            Err(_) => {
                clear_exc();
                None
            }
        };
        if closed == Some(false) {
            hdr(o).ioflags |= F_FINALIZING;
            if ab::setattr(o, &pystr::intern_str("_finalizing"), bool_::true_obj()).is_err() {
                clear_exc();
            }
            if call0(o, "close").is_err() {
                let msg = format!(
                    "Exception ignored while finalizing file {}",
                    ab::repr_string(o).unwrap_or_else(|_| {
                        clear_exc();
                        "<object repr() failed>".into()
                    })
                );
                crate::modules::sys::write_unraisable_with_obj(&msg, None);
            }
        }
        if let Some(e) = saved {
            set_exc(e);
        }
    }
}

unsafe extern "C" fn header_dealloc(op: *mut PyObject) {
    unsafe {
        if dealloc_begin(op) {
            return;
        }
        dealloc_end(op);
    }
}
unsafe extern "C" fn header_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe { visit_header(op, visit, arg) }
}
unsafe extern "C" fn header_clear(op: *mut PyObject) -> c_int {
    unsafe {
        clear_slot(&mut hdr(&*op).dict);
        0
    }
}

unsafe extern "C" fn header_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    unsafe { (*tp).tp_alloc.unwrap()(tp, 0) }
}

// ---------------------------------------------------------------- _IOBase methods

fn iob_seek(_o: &PyObject, _args: &[&PyObject]) -> PyResult {
    Err(unsupported("seek"))
}
fn iob_tell(o: &PyObject) -> PyResult {
    ab::call_method_str(o, "seek", &[&int::new_int(0), &int::new_int(1)])
}
fn iob_truncate(_o: &PyObject, _args: &[&PyObject]) -> PyResult {
    Err(unsupported("truncate"))
}

/// Closed flag of the plain _IOBase implementation.
pub fn base_closed(o: &PyObject) -> bool {
    hdr(o).ioflags & F_CLOSED != 0
}

fn iob_flush(o: &PyObject) -> PyResult {
    if base_closed(o) {
        return Err(err_closed());
    }
    Ok(none::none_ref())
}

fn iob_close(o: &PyObject) -> PyResult {
    if base_closed(o) {
        return Ok(none::none_ref());
    }
    let r = call0(o, "flush");
    hdr(o).ioflags |= F_CLOSED;
    r.map(|_| none::none_ref())
}

fn iob_get_closed(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(base_closed(o)))
}

fn iob_false(_o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(false))
}

fn call_bool(o: &PyObject, name: &str) -> PyResult<bool> {
    let r = call0(o, name)?;
    if !r.is_type(crate::types::bool_::bool_type()) {
        raise!(TypeError, "{}() should return a bool", name);
    }
    Ok(r.is(bool_::true_obj()))
}

pub fn check_seekable(o: &PyObject) -> PyResult<()> {
    if !ab::is_true(&*call0(o, "seekable")?)? {
        return Err(unsupported("File or stream is not seekable."));
    }
    Ok(())
}
pub fn check_readable(o: &PyObject) -> PyResult<()> {
    if !ab::is_true(&*call0(o, "readable")?)? {
        return Err(unsupported("File or stream is not readable."));
    }
    Ok(())
}
pub fn check_writable(o: &PyObject) -> PyResult<()> {
    if !ab::is_true(&*call0(o, "writable")?)? {
        return Err(unsupported("File or stream is not writable."));
    }
    Ok(())
}

fn iob_check_closed(o: &PyObject, _args: &[&PyObject]) -> PyResult {
    check_closed(o)?;
    Ok(none::none_ref())
}
fn iob_check_seekable(o: &PyObject, _args: &[&PyObject]) -> PyResult {
    let _ = call_bool;
    check_seekable(o)?;
    Ok(bool_::new_bool(true))
}
fn iob_check_readable(o: &PyObject, _args: &[&PyObject]) -> PyResult {
    check_readable(o)?;
    Ok(bool_::new_bool(true))
}
fn iob_check_writable(o: &PyObject, _args: &[&PyObject]) -> PyResult {
    check_writable(o)?;
    Ok(bool_::new_bool(true))
}

fn iob_enter(o: &PyObject) -> PyResult {
    check_closed(o)?;
    Ok(o.to_ref())
}
fn iob_exit(o: &PyObject, _args: &[&PyObject]) -> PyResult {
    call0(o, "close")
}
fn iob_fileno(_o: &PyObject) -> PyResult {
    Err(unsupported("fileno"))
}
fn iob_isatty(o: &PyObject) -> PyResult {
    check_closed(o)?;
    Ok(bool_::new_bool(false))
}

/// Generic readline built on peek()/read().
fn iob_readline(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("readline", args, 0, 1)?;
    let limit = size_arg(args.first().copied())?;
    let peek = ab::getattr_opt(o, &pystr::intern_str("peek"))?;
    let mut out: Vec<u8> = Vec::new();
    while limit < 0 || (out.len() as isize) < limit {
        let mut nreadahead = 1usize;
        if let Some(p) = &peek {
            let ra = ab::call(p, &[&int::new_int(1)])?;
            if !bytes::is_bytes(&ra) {
                raise!(OSError, "peek() should have returned a bytes object, not '{}'", tname(&ra));
            }
            let d = bytes::bytes_data(&ra);
            if !d.is_empty() {
                let mut n = 0;
                let lim = if limit >= 0 { (limit as usize).min(d.len()) } else { d.len() };
                while n < lim {
                    let c = d[n];
                    n += 1;
                    if c == b'\n' {
                        break;
                    }
                }
                nreadahead = n;
            }
        }
        let b = ab::call_method_str(o, "read", &[&int::new_int(nreadahead as i64)])?;
        if b.is(none::none()) {
            // non-blocking stream with no data
            break;
        }
        if !bytes::is_bytes(&b) {
            raise!(OSError, "read() should have returned a bytes object, not '{}'", tname(&b));
        }
        let d = bytes::bytes_data(&b);
        if d.is_empty() {
            break;
        }
        out.extend_from_slice(d);
        if out.last() == Some(&b'\n') {
            break;
        }
    }
    Ok(new_bytes(&out))
}

fn iob_readlines(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("readlines", args, 0, 1)?;
    let hint = size_arg(args.first().copied())?;
    let result = list::new_list_empty();
    if hint <= 0 {
        let it = ab::get_iter(o)?;
        while let Some(line) = ab::iter_next(&it)? {
            list::append(&result, line);
        }
        return Ok(result);
    }
    let it = ab::get_iter(o)?;
    let mut total = 0isize;
    while let Some(line) = ab::iter_next(&it)? {
        let n = ab::length(&line)? as isize;
        list::append(&result, line);
        total += n;
        if total >= hint {
            break;
        }
    }
    Ok(result)
}

fn iob_writelines(o: &PyObject, lines: &PyObject) -> PyResult {
    check_closed(o)?;
    let it = ab::get_iter(lines)?;
    while let Some(line) = ab::iter_next(&it)? {
        loop {
            match call1(o, "write", &line) {
                Ok(_) => break,
                Err(e) => {
                    if exc_matches(crate::types::exceptions::InterruptedError()) {
                        clear_exc();
                        continue;
                    }
                    return Err(e);
                }
            }
        }
    }
    Ok(none::none_ref())
}

unsafe extern "C" fn iob_iter(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        into_c(check_closed(o).map(|_| o.to_ref()))
    }
}
unsafe extern "C" fn iob_iternext(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        let r = (|| -> PyResult<Option<PyRef>> {
            let line = call0(o, "readline")?;
            if ab::object_len_or_zero(&line) == 0 {
                if ab::length(&line)? == 0 {
                    return Ok(None);
                }
            }
            Ok(Some(line))
        })();
        match r {
            Ok(Some(l)) => l.into_raw(),
            Ok(None) => std::ptr::null_mut(),
            Err(_) => std::ptr::null_mut(),
        }
    }
}

fn iob_get_dict(o: &PyObject) -> PyResult {
    Ok(crate::types::typeobject::instance_dict(o).unwrap_or_else(dict::new_dict))
}

// ---------------------------------------------------------------- _RawIOBase

fn raw_read(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("read", args, 0, 1)?;
    let n = size_arg(args.first().copied())?;
    if n < 0 {
        return call0(o, "readall");
    }
    let b = new_bytearray(n as usize)?;
    let r = call1(o, "readinto", &b)?;
    if r.is(none::none()) {
        return Ok(r);
    }
    let got = ab::index_as_ssize(&r, Some(crate::types::exceptions::OverflowError()))?;
    if got < 0 || got > n {
        raise!(ValueError, "readinto returned {} outside buffer size {}", got, n);
    }
    let v = BufView::get(&b, false)?;
    Ok(new_bytes(&v.as_slice()[..got as usize]))
}

fn raw_readall(o: &PyObject) -> PyResult {
    let mut out: Vec<u8> = Vec::new();
    let chunk = int::new_int(DEFAULT_BUFFER_SIZE as i64);
    loop {
        let data = match call1(o, "read", &chunk) {
            Ok(d) => d,
            Err(e) => {
                if exc_matches(crate::types::exceptions::InterruptedError()) {
                    clear_exc();
                    continue;
                }
                return Err(e);
            }
        };
        if data.is(none::none()) {
            if out.is_empty() {
                return Ok(data);
            }
            break;
        }
        if !bytes::is_bytes(&data) {
            raise!(TypeError, "read() should return bytes");
        }
        let d = bytes::bytes_data(&data);
        if d.is_empty() {
            break;
        }
        out.extend_from_slice(d);
    }
    Ok(new_bytes(&out))
}

fn not_implemented(_o: &PyObject, _args: &[&PyObject]) -> PyResult {
    raise!(NotImplementedError, "");
}

// ---------------------------------------------------------------- _BufferedIOBase

fn bio_read(_o: &PyObject, _args: &[&PyObject]) -> PyResult {
    Err(unsupported("read"))
}
fn bio_read1(_o: &PyObject, _args: &[&PyObject]) -> PyResult {
    Err(unsupported("read1"))
}
fn bio_write(_o: &PyObject, _args: &[&PyObject]) -> PyResult {
    Err(unsupported("write"))
}
fn bio_detach(_o: &PyObject) -> PyResult {
    Err(unsupported("detach"))
}

fn readinto_generic(o: &PyObject, b: &PyObject, method: &str) -> PyResult {
    let mut view = BufView::get_msg(b, true, Some(&format!("{}() argument", if method == "read" { "readinto" } else { "readinto1" })))?;
    let len = view.len();
    let data = call1(o, method, &int::new_int(len as i64))?;
    if !bytes::is_bytes(&data) {
        raise!(TypeError, "{}() should return bytes", method);
    }
    let d = bytes::bytes_data(&data);
    if d.len() > len {
        raise!(ValueError, "{}() returned too much data: {} bytes requested, {} returned", method, len, d.len());
    }
    view.as_mut_slice()[..d.len()].copy_from_slice(d);
    Ok(int::new_int(d.len() as i64))
}
fn bio_readinto(o: &PyObject, b: &PyObject) -> PyResult {
    readinto_generic(o, b, "read")
}
fn bio_readinto1(o: &PyObject, b: &PyObject) -> PyResult {
    readinto_generic(o, b, "read1")
}

// ---------------------------------------------------------------- _TextIOBase

fn tio_read(_o: &PyObject, _args: &[&PyObject]) -> PyResult {
    Err(unsupported("read"))
}
fn tio_readline(_o: &PyObject, _args: &[&PyObject]) -> PyResult {
    Err(unsupported("readline"))
}
fn tio_write(_o: &PyObject, _args: &[&PyObject]) -> PyResult {
    Err(unsupported("write"))
}
fn tio_none(_o: &PyObject) -> PyResult {
    Ok(none::none_ref())
}

pub fn init_types() -> PyResult<()> {
    unsafe {
        if IOBASE_TYPE.tp_flags & Py_TPFLAGS_READY != 0 {
            return Ok(());
        }
        let size = std::mem::size_of::<IoHeader>();
        setup_type(iobase_type(), c"_io._IOBase", size, std::ptr::null_mut(), "The abstract base class for all I/O classes.");
        let t = &mut *iobase_type();
        t.tp_new = Some(header_new);
        t.tp_dealloc = Some(header_dealloc);
        t.tp_traverse = Some(header_traverse);
        t.tp_clear = Some(header_clear);
        t.tp_iter = Some(iob_iter);
        t.tp_iternext = Some(iob_iternext);
        t.tp_methods = crate::mdefs![
            ("seek", FAST, iob_seek),
            ("tell", NOARGS, iob_tell),
            ("truncate", FAST, iob_truncate),
            ("flush", NOARGS, iob_flush),
            ("close", NOARGS, iob_close),
            ("seekable", NOARGS, iob_false),
            ("readable", NOARGS, iob_false),
            ("writable", NOARGS, iob_false),
            ("_checkClosed", FAST, iob_check_closed),
            ("_checkSeekable", FAST, iob_check_seekable),
            ("_checkReadable", FAST, iob_check_readable),
            ("_checkWritable", FAST, iob_check_writable),
            ("fileno", NOARGS, iob_fileno),
            ("isatty", NOARGS, iob_isatty),
            ("__enter__", NOARGS, iob_enter),
            ("__exit__", FAST, iob_exit),
            ("readline", FAST, iob_readline),
            ("readlines", FAST, iob_readlines),
            ("writelines", O, iob_writelines),
        ];
        t.tp_getset = crate::getset_table![("__dict__", iob_get_dict), ("closed", iob_get_closed)];
        crate::types::typeobject::type_ready(iobase_type())?;

        setup_type(rawiobase_type(), c"_io._RawIOBase", size, iobase_type(), "Base class for raw binary I/O.");
        let t = &mut *rawiobase_type();
        t.tp_methods = crate::mdefs![
            ("read", FAST, raw_read),
            ("readall", NOARGS, raw_readall),
            ("readinto", FAST, not_implemented),
            ("write", FAST, not_implemented),
        ];
        crate::types::typeobject::type_ready(rawiobase_type())?;

        setup_type(bufferediobase_type(), c"_io._BufferedIOBase", size, iobase_type(), "Base class for buffered IO objects.");
        let t = &mut *bufferediobase_type();
        t.tp_methods = crate::mdefs![
            ("detach", NOARGS, bio_detach),
            ("read", FAST, bio_read),
            ("read1", FAST, bio_read1),
            ("readinto", O, bio_readinto),
            ("readinto1", O, bio_readinto1),
            ("write", FAST, bio_write),
        ];
        crate::types::typeobject::type_ready(bufferediobase_type())?;

        setup_type(textiobase_type(), c"_io._TextIOBase", size, iobase_type(), "Base class for text I/O.");
        let t = &mut *textiobase_type();
        t.tp_methods = crate::mdefs![
            ("detach", NOARGS, bio_detach),
            ("read", FAST, tio_read),
            ("readline", FAST, tio_readline),
            ("write", FAST, tio_write),
        ];
        t.tp_getset = crate::getset_table![("encoding", tio_none), ("newlines", tio_none), ("errors", tio_none)];
        crate::types::typeobject::type_ready(textiobase_type())?;
    }
    Ok(())
}
