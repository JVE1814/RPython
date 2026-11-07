//! The `_io` module: FileIO, BytesIO, StringIO, Buffered*, TextIOWrapper,
//! IncrementalNewlineDecoder, open().
//!
//! Every io object starts with `IoHeader` (instance dict, weakref list, flags) so
//! that Python subclasses of any of the classes get a `__dict__` and the shared
//! `_IOBase` machinery (closed flag, finalization) works uniformly.
pub mod bufferedio;
pub mod bytesio;
pub mod fileio;
pub mod iobase;
pub mod stringio;
pub mod textio;

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::tname;
use crate::types::{bool_, dict, int, module, none, str as pystr, tuple};
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

pub const DEFAULT_BUFFER_SIZE: usize = 128 * 1024;

#[repr(C)]
pub struct IoHeader {
    pub ob_base: PyObject,
    pub dict: *mut PyObject,
    pub weakreflist: *mut PyObject,
    pub ioflags: u32,
    /// Owner thread of the object lock (0 = unlocked).
    pub lock_owner: AtomicUsize,
}

pub const F_CLOSED: u32 = 1;
pub const F_FINALIZING: u32 = 2;

#[inline]
pub fn hdr(o: &PyObject) -> &mut IoHeader {
    unsafe { &mut *(o.as_ptr() as *mut IoHeader) }
}

static UNSUPPORTED: crate::subinterp::InterpPtr = crate::subinterp::InterpPtr::new(); // (interp1) per interpreter

pub fn unsupported_type() -> PyRef {
    unsafe { PyRef::from_borrowed(UNSUPPORTED.load(Ordering::Acquire)) }
}

/// Raise io.UnsupportedOperation(msg).
pub fn unsupported(msg: &str) -> PyErr {
    let t = unsupported_type();
    match ab::call(&t, &[&pystr::new_str(msg)]) {
        Ok(e) => {
            set_exc(e);
            PyErr
        }
        Err(e) => e,
    }
}

pub fn err_closed() -> PyErr {
    err!(ValueError, "I/O operation on closed file.")
}

/// Is `o` (an io object, possibly a Python subclass) closed? Uses the `closed` attribute.
pub fn is_closed(o: &PyObject) -> PyResult<bool> {
    let c = ab::getattr(o, crate::name!("closed"))?;
    ab::is_true(&c)
}

pub fn check_closed(o: &PyObject) -> PyResult<()> {
    if is_closed(o)? {
        return Err(err_closed());
    }
    Ok(())
}

pub fn call0(o: &PyObject, name: &str) -> PyResult {
    ab::call_method_str(o, name, &[])
}
pub fn call1(o: &PyObject, name: &str, a: &PyObject) -> PyResult {
    ab::call_method_str(o, name, &[a])
}

/// Size argument: None or negative -> -1.
pub fn size_arg(o: Option<&PyObject>) -> PyResult<isize> {
    match o {
        None => Ok(-1),
        Some(x) if x.is(none::none()) => Ok(-1),
        Some(x) => {
            if !ab::has_index(x) {
                raise!(TypeError, "argument should be integer or None, not '{}'", tname(x));
            }
            ab::index_as_ssize(x, Some(crate::types::exceptions::OverflowError()))
        }
    }
}

/// Integer argument via __index__ (no floats).
pub fn ssize_arg(o: &PyObject) -> PyResult<isize> {
    if crate::types::float::is_float(o) {
        raise!(TypeError, "'float' object cannot be interpreted as an integer");
    }
    ab::index_as_ssize(o, Some(crate::types::exceptions::OverflowError()))
}

pub fn i64_arg(o: &PyObject) -> PyResult<i64> {
    if crate::types::float::is_float(o) {
        raise!(TypeError, "'float' object cannot be interpreted as an integer");
    }
    ab::as_i64(o)
}

pub fn kwparse<'b, const N: usize>(
    fname: &str,
    names: &[&str],
    posonly: usize,
    required: usize,
    args: &[&'b PyObject],
    kw: Option<&'b PyObject>,
) -> PyResult<[Option<&'b PyObject>; N]> {
    crate::modules::posix::kwparse::<N>(fname, names, posonly, required, 0, args, kw)
}

/// Positional args from a (args tuple, kwargs dict) pair in vectorcall form.
pub fn init_args(args: *mut PyObject, kw: *mut PyObject) -> (Vec<PyRef>, Option<PyRef>) {
    unsafe { crate::args::kwdict_to_vectorcall(&*args, kw) }
}

/// Simple per-object lock, reentrancy-detecting (like CPython's buffered lock).
pub struct ObjLock<'a> {
    h: &'a IoHeader,
}
impl Drop for ObjLock<'_> {
    fn drop(&mut self) {
        self.h.lock_owner.store(0, Ordering::Release);
    }
}
pub fn lock(o: &PyObject) -> PyResult<ObjLock<'_>> {
    let h = unsafe { &*(o.as_ptr() as *const IoHeader) };
    let me = crate::object::thread_id().max(1);
    loop {
        match h.lock_owner.compare_exchange(0, me, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return Ok(ObjLock { h }),
            Err(cur) if cur == me => {
                let r = ab::repr_string(o).unwrap_or_default();
                raise!(RuntimeError, "reentrant call inside {}", r);
            }
            Err(_) => {
                crate::threads::blocking(|| std::thread::yield_now());
            }
        }
    }
}

// ---------------------------------------------------------------- type helpers

/// Fill the fields shared by all io types.
pub unsafe fn setup_type(
    t: *mut PyTypeObject,
    name: &'static std::ffi::CStr,
    size: usize,
    base: *mut PyTypeObject,
    doc: &str,
) {
    unsafe {
        let t = &mut *t;
        t.tp_name = name.as_ptr();
        t.tp_basicsize = size as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_GC;
        t.tp_base = base;
        t.tp_dictoffset = std::mem::offset_of!(IoHeader, dict) as isize;
        t.tp_weaklistoffset = std::mem::offset_of!(IoHeader, weakreflist) as isize;
        t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
        t.tp_setattro = Some(crate::types::typeobject::object_generic_setattro);
        t.tp_alloc = Some(crate::types::typeobject::generic_alloc);
        t.tp_free = Some(crate::types::typeobject::generic_free);
        t.tp_finalize = Some(iobase::iobase_finalize);
        if !doc.is_empty() {
            t.tp_doc = crate::methods::cstr_static(doc);
        }
    }
}

/// Common start of every io tp_dealloc. Returns true if the object was resurrected
/// by its finalizer (the caller must then return immediately).
pub unsafe fn dealloc_begin(op: *mut PyObject) -> bool {
    unsafe {
        if (*(*op).type_ptr()).tp_finalize.is_some() && !crate::gc::is_finalized(op) {
            if crate::gc::call_finalizer_from_dealloc(op) {
                return true;
            }
        }
        crate::gc::untrack(op);
        let h = &mut *(op as *mut IoHeader);
        if !h.weakreflist.is_null() {
            crate::types::weakref::clear_weakrefs(op);
        }
        let d = std::mem::replace(&mut h.dict, std::ptr::null_mut());
        xdecref(d);
        false
    }
}

pub unsafe fn dealloc_end(op: *mut PyObject) {
    unsafe {
        let tp = (*op).type_ptr();
        crate::types::typeobject::generic_free_and_type_decref(op, tp);
    }
}

pub unsafe fn visit_ptr(p: *mut PyObject, visit: visitproc, arg: *mut std::ffi::c_void) -> std::ffi::c_int {
    unsafe { if p.is_null() { 0 } else { visit.unwrap()(p, arg) } }
}

pub unsafe fn visit_header(op: *mut PyObject, visit: visitproc, arg: *mut std::ffi::c_void) -> std::ffi::c_int {
    unsafe {
        let h = &*(op as *mut IoHeader);
        let tp = (*op).type_ptr();
        if crate::types::typeobject::is_heap_type(tp) {
            let r = visit.unwrap()(tp as *mut PyObject, arg);
            if r != 0 {
                return r;
            }
        }
        visit_ptr(h.dict, visit, arg)
    }
}

pub unsafe fn clear_slot(p: &mut *mut PyObject) {
    unsafe {
        let old = std::mem::replace(p, std::ptr::null_mut());
        xdecref(old);
    }
}

pub unsafe fn set_slot(p: &mut *mut PyObject, v: Option<&PyObject>) {
    unsafe {
        let new = v.map_or(std::ptr::null_mut(), |x| x.to_ref().into_raw());
        let old = std::mem::replace(p, new);
        xdecref(old);
    }
}

pub fn get_slot(p: *mut PyObject) -> Option<PyRef> {
    if p.is_null() { None } else { Some(unsafe { PyRef::from_borrowed(p) }) }
}

/// Build a bytes object from a slice.
pub fn new_bytes(b: &[u8]) -> PyRef {
    crate::types::bytes::new_bytes(b)
}

pub fn new_bytearray(n: usize) -> PyResult {
    ab::call(
        crate::types::typeobject::type_obj(crate::types::bytearray::bytearray_type()),
        &[&int::new_int(n as i64)],
    )
}

pub fn type_ref(t: *mut PyTypeObject) -> PyRef {
    crate::types::typeobject::type_ref(t)
}

/// Emit "unclosed file <repr>" ResourceWarning for `source`.
pub fn resource_warning(source: &PyObject) -> PyResult<()> {
    let r = ab::repr_string(source)?;
    crate::modules::warnings::warn_with_source(
        crate::types::exceptions::ResourceWarning(),
        &format!("unclosed file {}", r),
        1,
        Some(source),
    )
}

// ---------------------------------------------------------------- open()

struct Mode {
    creating: bool,
    reading: bool,
    writing: bool,
    appending: bool,
    updating: bool,
    text: bool,
    binary: bool,
}

fn parse_open_mode(mode: &str) -> PyResult<Mode> {
    let mut m = Mode { creating: false, reading: false, writing: false, appending: false, updating: false, text: false, binary: false };
    let mut seen = String::new();
    for c in mode.chars() {
        if !"axrwb+t".contains(c) || seen.contains(c) {
            raise!(ValueError, "invalid mode: '{}'", mode);
        }
        seen.push(c);
        match c {
            'x' => m.creating = true,
            'r' => m.reading = true,
            'w' => m.writing = true,
            'a' => m.appending = true,
            '+' => m.updating = true,
            't' => m.text = true,
            'b' => m.binary = true,
            _ => {}
        }
    }
    Ok(m)
}

fn open_impl(
    file: &PyObject,
    mode: &str,
    buffering: isize,
    encoding: Option<&PyObject>,
    errors: Option<&PyObject>,
    newline: Option<&PyObject>,
    closefd: bool,
    opener: Option<&PyObject>,
) -> PyResult {
    let is_fd = int::is_int(file);
    let fspathed;
    let file = if !is_fd && !pystr::is_str(file) && !crate::types::bytes::is_bytes(file) {
        // must be path-like: errors raised by __fspath__ propagate
        fspathed = crate::modules::posix::path::fspath(file)?;
        &*fspathed
    } else {
        file
    };
    let m = parse_open_mode(mode)?;
    if m.text && m.binary {
        raise!(ValueError, "can't have text and binary mode at once");
    }
    if (m.creating as u8 + m.reading as u8 + m.writing as u8 + m.appending as u8) > 1 {
        raise!(ValueError, "must have exactly one of create/read/write/append mode");
    }
    if !(m.creating || m.reading || m.writing || m.appending) {
        raise!(ValueError, "Must have exactly one of create/read/write/append mode and at most one plus");
    }
    if m.binary {
        if let Some(e) = encoding.filter(|e| !e.is(none::none())) {
            let _ = e;
            raise!(ValueError, "binary mode doesn't take an encoding argument");
        }
        if errors.map_or(false, |e| !e.is(none::none())) {
            raise!(ValueError, "binary mode doesn't take an errors argument");
        }
        if newline.map_or(false, |e| !e.is(none::none())) {
            raise!(ValueError, "binary mode doesn't take a newline argument");
        }
    }
    if m.binary && buffering == 1 {
        crate::modules::warnings::warn(
            crate::types::exceptions::RuntimeWarning(),
            "line buffering (buffering=1) isn't supported in binary mode, the default buffer size will be used",
            1,
        )?;
    }
    let mut rawmode = String::new();
    if m.creating {
        rawmode.push('x');
    }
    if m.reading {
        rawmode.push('r');
    }
    if m.writing {
        rawmode.push('w');
    }
    if m.appending {
        rawmode.push('a');
    }
    if m.updating {
        rawmode.push('+');
    }
    let raw = fileio::new_fileio(file, &rawmode, closefd, opener)?;
    // the outermost layer built so far: closed (with everything below) on error
    let mut result = raw.clone();
    let r = (|| -> PyResult {
        let mut buffering = buffering;
        let mut line_buffering = false;
        if buffering == 1 || (buffering < 0 && ab::is_true(&*call0(&raw, "_isatty_open_only")?)?) {
            buffering = -1;
            line_buffering = true;
        }
        if buffering < 0 {
            // the file system block size, bounded to [DEFAULT_BUFFER_SIZE, 8 MiB]
            buffering = (fileio::blksize(&raw) as isize).min(8192 * 1024).max(DEFAULT_BUFFER_SIZE as isize);
        }
        if buffering < 0 {
            raise!(ValueError, "invalid buffering size");
        }
        if buffering == 0 {
            if m.binary {
                return Ok(raw.clone());
            }
            raise!(ValueError, "can't have unbuffered text I/O");
        }
        let bsize = int::new_int(buffering as i64);
        let buffer = if m.updating {
            ab::call(&type_ref(bufferedio::bufrandom_type()), &[&raw, &bsize])?
        } else if m.creating || m.writing || m.appending {
            ab::call(&type_ref(bufferedio::bufwriter_type()), &[&raw, &bsize])?
        } else {
            ab::call(&type_ref(bufferedio::bufreader_type()), &[&raw, &bsize])?
        };
        result = buffer.clone();
        if m.binary {
            return Ok(buffer);
        }
        let enc = encoding.map(|e| e.to_ref()).unwrap_or_else(none::none_ref);
        let err = errors.map(|e| e.to_ref()).unwrap_or_else(none::none_ref);
        let nl = newline.map(|e| e.to_ref()).unwrap_or_else(none::none_ref);
        let lb = bool_::new_bool(line_buffering);
        let wrapper = ab::call(&type_ref(textio::textiowrapper_type()), &[&buffer, &enc, &err, &nl, &lb])?;
        result = wrapper.clone();
        ab::setattr(&wrapper, crate::name!("mode"), &pystr::new_str(mode))?;
        Ok(wrapper)
    })();
    match r {
        Ok(v) => Ok(v),
        Err(e) => {
            let saved = take_exc();
            if call0(&result, "close").is_err() {
                clear_exc();
            }
            if let Some(s) = saved {
                set_exc(s);
            }
            Err(e)
        }
    }
}

fn io_open(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [file, mode, buffering, encoding, errors, newline, closefd, opener] = kwparse::<8>(
        "open",
        &["file", "mode", "buffering", "encoding", "errors", "newline", "closefd", "opener"],
        0,
        1,
        args,
        kw,
    )?;
    let file = file.unwrap();
    let mode = match mode {
        None => "r".to_string(),
        Some(m) => {
            if !pystr::is_str(m) {
                raise!(TypeError, "open() argument 'mode' must be str, not {}", tname(m));
            }
            pystr::to_string_lossy(m)
        }
    };
    let buffering = match buffering {
        None => -1,
        Some(b) => ssize_arg(b)?,
    };
    for (o, n) in [(encoding, "encoding"), (errors, "errors"), (newline, "newline")] {
        if let Some(o) = o {
            if !o.is(none::none()) && !pystr::is_str(o) {
                raise!(TypeError, "open() argument '{}' must be str or None, not {}", n, tname(o));
            }
        }
    }
    let closefd = match closefd {
        None => true,
        Some(c) => ab::is_true(c)?,
    };
    let opener = opener.filter(|o| !o.is(none::none()));
    open_impl(file, &mode, buffering, encoding, errors, newline, closefd, opener)
}

fn io_open_code(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [path] = kwparse::<1>("open_code", &["path"], 0, 1, args, kw)?;
    let p = path.unwrap();
    if !pystr::is_str(p) {
        raise!(TypeError, "open_code() argument 'path' must be str, not {}", tname(p));
    }
    open_impl(p, "rb", -1, None, None, None, true, None)
}

/// io.text_encoding(encoding, stacklevel=2)
fn io_text_encoding(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("text_encoding", args, 1, 2)?;
    let enc = args[0];
    if !enc.is(none::none()) {
        return Ok(enc.to_ref());
    }
    let stacklevel = if args.len() > 1 { ssize_arg(args[1])? as i32 } else { 2 };
    let utf8_mode = crate::modules::sys::get_sys_attr("flags")
        .and_then(|f| ab::getattr_str(&f, "utf8_mode").ok())
        .and_then(|v| crate::types::int::as_i64(&v))
        .unwrap_or(0);
    let warn_default = crate::modules::sys::get_sys_attr("flags")
        .and_then(|f| ab::getattr_str(&f, "warn_default_encoding").ok())
        .and_then(|v| crate::types::int::as_i64(&v))
        .unwrap_or(0);
    if warn_default != 0 {
        crate::modules::warnings::warn(crate::types::exceptions::EncodingWarning(), "'encoding' argument not specified", stacklevel)?;
    }
    if utf8_mode != 0 { Ok(pystr::new_str("utf-8")) } else { Ok(pystr::new_str("locale")) }
}

pub fn init() -> PyResult {
    let m = module::new_module("_io")?;
    let d = module::module_dict(&m);
    // UnsupportedOperation(OSError, ValueError)
    if UNSUPPORTED.load(Ordering::Acquire).is_null() {
        let ns = dict::new_dict();
        dict::set_item_str(&ns, "__module__", &pystr::new_str("io"))?;
        let bases = tuple::new_tuple(vec![
            type_ref(crate::types::exceptions::OSError()),
            type_ref(crate::types::exceptions::ValueError()),
        ]);
        let t = ab::call(
            crate::types::typeobject::type_obj(crate::types::typeobject::type_type()),
            &[&pystr::new_str("UnsupportedOperation"), &bases, &ns],
        )?;
        UNSUPPORTED.store(t.into_raw(), Ordering::Release);
    }
    iobase::init_types()?;
    fileio::init_types()?;
    bytesio::init_types()?;
    stringio::init_types()?;
    bufferedio::init_types()?;
    textio::init_types()?;
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![
            ("open", FASTKW, io_open, "open($module, /, file, mode='r', buffering=-1, encoding=None,\n     errors=None, newline=None, closefd=True, opener=None)\n--\n\nOpen a file and return a stream object for it.\n\nfile is a path (str, bytes or os.PathLike) or an integer file descriptor.\nmode combines 'r', 'w', 'x' or 'a' with optional '+' (update), and 'b'\n(binary) or 't' (text, the default).  Text streams decode with encoding\n(locale.getencoding() if None) using the errors handler, and translate\nline endings according to newline.  buffering selects the buffer size\n(0 = unbuffered, binary only; 1 = line buffered, text only).  When file\nis a descriptor, closefd=False keeps it open after the stream is closed.\nopener(path, flags) may supply the descriptor to use."),
            ("open_code", FASTKW, io_open_code, "open_code($module, /, path)\n--\n\nOpen a file meant to be executed as code, in binary read mode."),
            ("text_encoding", FAST, io_text_encoding, "text_encoding($module, encoding, stacklevel=2, /)\n--\n\nReturn the encoding a text stream would use for the given encoding argument.\n\nNone maps to \"locale\" (or \"utf-8\" in UTF-8 mode) and may emit an\nEncodingWarning; other values are returned unchanged."),
        ],
        Some(&m),
    )?;
    dict::set_item_str(&d, "DEFAULT_BUFFER_SIZE", &int::new_int(DEFAULT_BUFFER_SIZE as i64))?;
    dict::set_item_str(&d, "UnsupportedOperation", &unsupported_type())?;
    dict::set_item_str(&d, "BlockingIOError", &type_ref(crate::types::exceptions::BlockingIOError()))?;
    for (n, t) in [
        ("_IOBase", iobase::iobase_type()),
        ("_RawIOBase", iobase::rawiobase_type()),
        ("_BufferedIOBase", iobase::bufferediobase_type()),
        ("_TextIOBase", iobase::textiobase_type()),
        ("FileIO", fileio::fileio_type()),
        ("BytesIO", bytesio::bytesio_type()),
        ("StringIO", stringio::stringio_type()),
        ("BufferedReader", bufferedio::bufreader_type()),
        ("BufferedWriter", bufferedio::bufwriter_type()),
        ("BufferedRandom", bufferedio::bufrandom_type()),
        ("BufferedRWPair", bufferedio::bufrwpair_type()),
        ("TextIOWrapper", textio::textiowrapper_type()),
        ("IncrementalNewlineDecoder", textio::nldecoder_type()),
        ("_BytesIOBuffer", &raw mut bytesio::BYTESIOBUF_TYPE),
    ] {
        dict::set_item_str(&d, n, &type_ref(t))?;
    }
    Ok(m)
}
