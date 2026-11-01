//! _io.FileIO: raw unbuffered file descriptor I/O.
use super::*;
use crate::modules::bufview::BufView;
use crate::modules::posix::path::{errno, fs_encode_str};
use crate::types::bytes;
use std::ffi::{c_int, c_void};

#[repr(C)]
pub struct FileIO {
    pub hdr: IoHeader,
    pub fd: c_int,
    pub created: bool,
    pub readable: bool,
    pub writable: bool,
    pub appending: bool,
    /// opened with 'w' (truncating): reported as "wb"/"wb+"
    pub truncating: bool,
    /// -1 unknown, 0 no, 1 yes
    pub seekable: i8,
    pub closefd: bool,
    pub finalizing: bool,
    pub blksize: i64,
    /// Estimated file size at open time (-1 unknown), used by readall().
    pub size_estimate: i64,
}

pub static mut FILEIO_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub fn fileio_type() -> *mut PyTypeObject {
    &raw mut FILEIO_TYPE
}

#[inline]
pub fn fio(o: &PyObject) -> &mut FileIO {
    unsafe { &mut *(o.as_ptr() as *mut FileIO) }
}

pub fn is_fileio(o: &PyObject) -> bool {
    crate::types::typeobject::is_subtype(o.type_ptr(), fileio_type())
}

/// True if `o` is exactly a FileIO (no Python overrides), enabling fast paths.
pub fn is_exact_fileio(o: &PyObject) -> bool {
    o.is_type(fileio_type())
}

pub fn blksize(o: &PyObject) -> i64 {
    if is_fileio(o) {
        let b = fio(o).blksize;
        if b > 1 {
            return b;
        }
    }
    match ab::getattr_str(o, "_blksize").ok().and_then(|v| int::as_i64(&v)) {
        Some(b) if b > 1 => b,
        _ => {
            clear_exc();
            DEFAULT_BUFFER_SIZE as i64
        }
    }
}

fn os_error() -> PyErr {
    crate::types::exceptions::os_error_from_errno(errno(), None, None)
}

unsafe extern "C" fn fileio_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    unsafe {
        let op = (*tp).tp_alloc.unwrap()(tp, 0);
        if !op.is_null() {
            let f = &mut *(op as *mut FileIO);
            f.fd = -1;
            f.seekable = -1;
            f.closefd = true;
            f.blksize = 0;
            f.size_estimate = -1;
        }
        op
    }
}

fn internal_close(f: &mut FileIO) -> PyResult<()> {
    let fd = f.fd;
    f.fd = -1;
    if fd >= 0 && f.closefd {
        let r = unsafe { libc::close(fd) };
        if r < 0 && errno() != libc::EINTR {
            return Err(os_error());
        }
    }
    Ok(())
}

fn parse_mode(mode: &str) -> PyResult<(bool, bool, bool, bool, bool, bool)> {
    // (created, readable, writable, appending, plus, truncating)
    let (mut c, mut r, mut w, mut a, mut plus, mut t) = (false, false, false, false, false, false);
    let mut rwa = false;
    let bad = || err!(ValueError, "Must have exactly one of create/read/write/append mode and at most one plus");
    for ch in mode.chars() {
        match ch {
            'x' | 'r' | 'w' | 'a' => {
                if rwa {
                    return Err(bad());
                }
                rwa = true;
                match ch {
                    'x' => {
                        c = true;
                        w = true;
                    }
                    'r' => r = true,
                    'w' => {
                        w = true;
                        t = true;
                    }
                    _ => {
                        a = true;
                        w = true;
                    }
                }
            }
            '+' => {
                if plus {
                    return Err(bad());
                }
                r = true;
                w = true;
                plus = true;
            }
            'b' => {}
            _ => raise!(ValueError, "invalid mode: {}", mode),
        }
    }
    if !rwa {
        return Err(bad());
    }
    Ok((c, r, w, a, plus, t))
}

fn fileio_init_impl(o: &PyObject, file: &PyObject, mode: &str, closefd: bool, opener: Option<&PyObject>) -> PyResult<()> {
    let f = fio(o);
    if f.fd >= 0 {
        if f.closefd {
            internal_close(f)?;
        } else {
            f.fd = -1;
        }
    }
    let mut fd: c_int = -1;
    if crate::types::float::is_float(file) {
        raise!(TypeError, "integer argument expected, got float");
    }
    let mut path: Option<std::ffi::CString> = None;
    if int::is_int(file) {
        if crate::types::bool_::is_bool(file) {
            crate::modules::warnings::warn(
                crate::types::exceptions::RuntimeWarning(),
                "bool is used as a file descriptor",
                1,
            )?;
        }
        match int::as_i64(file) {
            Some(v) if v >= i32::MIN as i64 && v <= i32::MAX as i64 => {
                if v < 0 {
                    raise!(ValueError, "negative file descriptor");
                }
                fd = v as c_int;
            }
            _ => raise!(TypeError, "expected str, bytes or os.PathLike object, not int"),
        }
    } else {
        let p = crate::modules::posix::path::fspath(file)?;
        let b = if bytes::is_bytes(&p) { bytes::bytes_data(&p).to_vec() } else { fs_encode_str(&p)? };
        if b.contains(&0) {
            raise!(ValueError, "embedded null byte");
        }
        path = Some(std::ffi::CString::new(b).unwrap());
    }
    let (created, readable, writable, appending, plus, truncating) = parse_mode(mode)?;
    let mut flags = if readable && writable {
        libc::O_RDWR
    } else if readable {
        libc::O_RDONLY
    } else {
        libc::O_WRONLY
    };
    if created {
        flags |= libc::O_EXCL | libc::O_CREAT;
    } else if appending {
        flags |= libc::O_APPEND | libc::O_CREAT;
    } else if truncating {
        flags |= libc::O_TRUNC | libc::O_CREAT;
    }
    let _ = plus;
    flags |= libc::O_CLOEXEC;
    f.created = created;
    f.readable = readable;
    f.writable = writable;
    f.appending = appending;
    f.truncating = truncating;
    f.seekable = -1;
    f.blksize = 0;
    let fd_is_own;
    if fd >= 0 {
        fd_is_own = false;
        f.fd = fd;
        f.closefd = closefd;
    } else {
        fd_is_own = true;
        f.closefd = true;
        if !closefd {
            raise!(ValueError, "Cannot use closefd=False with file name");
        }
        let cpath = path.unwrap();
        match opener {
            None => {
                let r = crate::modules::posix::path::retry_eintr(-1, || unsafe { libc::open(cpath.as_ptr(), flags, 0o666) })?;
                if r < 0 {
                    return Err(crate::types::exceptions::os_error_from_errno(errno(), Some(file), None));
                }
                fd = r;
            }
            Some(op) => {
                let r = ab::call(op, &[file, &int::new_int(flags as i64)])?;
                if !int::is_int(&r) {
                    raise!(TypeError, "expected integer from opener");
                }
                let v = ab::as_i64(&r)?;
                if v < 0 {
                    if !err_occurred() {
                        raise!(ValueError, "opener returned {}", v);
                    }
                    return Err(PyErr);
                }
                fd = v as c_int;
                unsafe { crate::modules::posix::fs::set_inheritable_fd(fd, false)? };
            }
        }
        f.fd = fd;
    }
    let fail = |f: &mut FileIO| {
        if !fd_is_own {
            f.fd = -1;
        }
    };
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::fstat(f.fd, &mut st) };
    if r < 0 {
        let e = errno();
        fail(f);
        if e == libc::EBADF {
            return Err(crate::types::exceptions::os_error_from_errno(e, None, None));
        }
    } else {
        if st.st_mode & libc::S_IFMT == libc::S_IFDIR {
            let _ = internal_close(f);
            if !fd_is_own {
                f.fd = -1;
            }
            return Err(crate::types::exceptions::os_error_from_errno(libc::EISDIR, Some(file), None));
        }
        f.blksize = if st.st_blksize > 1 { st.st_blksize } else { DEFAULT_BUFFER_SIZE as i64 };
        f.size_estimate = if st.st_mode & libc::S_IFMT == libc::S_IFREG { st.st_size } else { -1 };
    }
    if let Err(e) = ab::setattr(o, crate::name!("name"), file) {
        fail(fio(o));
        return Err(e);
    }
    let f = fio(o);
    if f.appending {
        unsafe { libc::lseek(f.fd, 0, libc::SEEK_END) };
    }
    Ok(())
}

unsafe extern "C" fn fileio_init(op: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let o = &*op;
        let r = (|| -> PyResult<()> {
            let (vals, kws) = init_args(args, kw);
            let refs: Vec<&PyObject> = vals.iter().map(|x| &**x).collect();
            let [file, mode, closefd, opener] =
                kwparse::<4>("FileIO", &["file", "mode", "closefd", "opener"], 0, 1, &refs, kws.as_deref())?;
            let mode = match mode {
                None => "r".to_string(),
                Some(m) => {
                    if !pystr::is_str(m) {
                        raise!(TypeError, "FileIO() argument 'mode' must be str, not {}", tname(m));
                    }
                    pystr::to_string_lossy(m)
                }
            };
            let closefd = match closefd {
                None => true,
                Some(c) => ab::is_true(c)?,
            };
            let opener = opener.filter(|x| !x.is(none::none()));
            fileio_init_impl(o, file.unwrap(), &mode, closefd, opener)
        })();
        int_into_c(r)
    }
}

/// Create a FileIO (used by open()).
pub fn new_fileio(file: &PyObject, mode: &str, closefd: bool, opener: Option<&PyObject>) -> PyResult {
    unsafe {
        let op = fileio_new(fileio_type(), std::ptr::null_mut(), std::ptr::null_mut());
        if op.is_null() {
            return Err(PyErr);
        }
        let o = PyRef::from_raw(op);
        fileio_init_impl(&o, file, mode, closefd, opener)?;
        Ok(o)
    }
}

unsafe extern "C" fn fileio_dealloc(op: *mut PyObject) {
    unsafe {
        if dealloc_begin(op) {
            return;
        }
        let f = &mut *(op as *mut FileIO);
        if f.fd >= 0 && f.closefd {
            libc::close(f.fd);
        }
        f.fd = -1;
        dealloc_end(op);
    }
}

unsafe extern "C" fn fileio_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe { visit_header(op, visit, arg) }
}
unsafe extern "C" fn fileio_clear(op: *mut PyObject) -> c_int {
    unsafe {
        clear_slot(&mut hdr(&*op).dict);
        0
    }
}

fn check_open(f: &FileIO) -> PyResult<()> {
    if f.fd < 0 {
        raise!(ValueError, "I/O operation on closed file");
    }
    Ok(())
}
fn check_readable(f: &FileIO) -> PyResult<()> {
    check_open(f)?;
    if !f.readable {
        return Err(unsupported("File not open for reading"));
    }
    Ok(())
}
fn check_writable(f: &FileIO) -> PyResult<()> {
    check_open(f)?;
    if !f.writable {
        return Err(unsupported("File not open for writing"));
    }
    Ok(())
}

/// Read into `buf`; Ok(None) on EAGAIN.
pub fn raw_read_into(fd: c_int, buf: &mut [u8]) -> PyResult<Option<usize>> {
    let n = buf.len().min(isize::MAX as usize);
    let r = crate::modules::posix::path::retry_eintr(-1, || unsafe { libc::read(fd, buf.as_mut_ptr() as *mut c_void, n) })?;
    if r < 0 {
        if errno() == libc::EAGAIN {
            return Ok(None);
        }
        return Err(os_error());
    }
    Ok(Some(r as usize))
}

/// Write `buf`; Ok(None) on EAGAIN.
pub fn raw_write(fd: c_int, buf: &[u8]) -> PyResult<Option<usize>> {
    let r = crate::modules::posix::path::retry_eintr(-1, || unsafe { libc::write(fd, buf.as_ptr() as *const c_void, buf.len()) })?;
    if r < 0 {
        if errno() == libc::EAGAIN {
            return Ok(None);
        }
        return Err(os_error());
    }
    Ok(Some(r as usize))
}

fn fio_readinto(o: &PyObject, b: &PyObject) -> PyResult {
    let mut view = BufView::get_msg(b, true, Some("readinto() argument"))?;
    let f = fio(o);
    check_readable(f)?;
    match raw_read_into(f.fd, view.as_mut_slice())? {
        Some(n) => Ok(int::new_int(n as i64)),
        None => Ok(none::none_ref()),
    }
}

pub fn readall_fd(f: &mut FileIO) -> PyResult<Option<Vec<u8>>> {
    let mut est = f.size_estimate;
    if est > 0 {
        let pos = unsafe { libc::lseek(f.fd, 0, libc::SEEK_CUR) };
        if pos >= 0 && est >= pos {
            est = est - pos + 1;
        } else {
            est = -1;
        }
    }
    let mut buf: Vec<u8> = Vec::with_capacity(if est > 0 { est as usize } else { 8192 });
    loop {
        if buf.len() == buf.capacity() {
            let add = if buf.len() < 65536 { 8192.max(buf.len()) } else { buf.len() / 4 + 65536 };
            buf.reserve(add);
        }
        let len = buf.len();
        let cap = buf.capacity();
        let spare = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr().add(len), cap - len) };
        match raw_read_into(f.fd, spare)? {
            None => {
                if buf.is_empty() {
                    return Ok(None);
                }
                break;
            }
            Some(0) => break,
            Some(n) => unsafe { buf.set_len(len + n) },
        }
    }
    Ok(Some(buf))
}

fn fio_readall(o: &PyObject) -> PyResult {
    let f = fio(o);
    check_open(f)?;
    match readall_fd(f)? {
        Some(v) => Ok(new_bytes(&v)),
        None => Ok(none::none_ref()),
    }
}

fn fio_read(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("read", args, 0, 1)?;
    let n = size_arg(args.first().copied())?;
    let f = fio(o);
    check_readable(f)?;
    if n < 0 {
        return match readall_fd(f)? {
            Some(v) => Ok(new_bytes(&v)),
            None => Ok(none::none_ref()),
        };
    }
    let mut buf = vec![0u8; n as usize];
    match raw_read_into(f.fd, &mut buf)? {
        Some(k) => Ok(new_bytes(&buf[..k])),
        None => Ok(none::none_ref()),
    }
}

fn fio_write(o: &PyObject, b: &PyObject) -> PyResult {
    let view = BufView::get(b, false)?;
    let f = fio(o);
    check_writable(f)?;
    match raw_write(f.fd, view.as_slice())? {
        Some(n) => Ok(int::new_int(n as i64)),
        None => Ok(none::none_ref()),
    }
}

fn fio_seek(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("seek", args, 1, 2)?;
    let f = fio(o);
    check_open(f)?;
    let pos = i64_arg(args[0])?;
    let whence = if args.len() > 1 { crate::modules::posix::path::c_int(args[1])? } else { 0 };
    let r = unsafe { libc::lseek(f.fd, pos, whence) };
    if r < 0 {
        return Err(os_error());
    }
    if f.seekable < 0 {
        f.seekable = 1;
    }
    Ok(int::new_int(r))
}

fn fio_tell(o: &PyObject) -> PyResult {
    let f = fio(o);
    check_open(f)?;
    let r = unsafe { libc::lseek(f.fd, 0, libc::SEEK_CUR) };
    if r < 0 {
        return Err(os_error());
    }
    Ok(int::new_int(r))
}

fn fio_truncate(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("truncate", args, 0, 1)?;
    let f = fio(o);
    check_open(f)?;
    if !f.writable {
        return Err(unsupported("File not open for writing"));
    }
    let size = match args.first() {
        None => None,
        Some(x) if x.is(none::none()) => None,
        Some(x) => Some(i64_arg(x)?),
    };
    let size = match size {
        Some(s) => s,
        None => {
            let r = unsafe { libc::lseek(f.fd, 0, libc::SEEK_CUR) };
            if r < 0 {
                return Err(os_error());
            }
            r
        }
    };
    let r = crate::modules::posix::path::retry_eintr(-1, || unsafe { libc::ftruncate(f.fd, size) })?;
    if r < 0 {
        return Err(os_error());
    }
    Ok(int::new_int(size))
}

fn fio_seekable(o: &PyObject) -> PyResult {
    let f = fio(o);
    check_open(f)?;
    if f.seekable < 0 {
        let r = unsafe { libc::lseek(f.fd, 0, libc::SEEK_CUR) };
        f.seekable = if r < 0 { 0 } else { 1 };
    }
    Ok(bool_::new_bool(f.seekable == 1))
}
fn fio_readable(o: &PyObject) -> PyResult {
    let f = fio(o);
    check_open(f)?;
    Ok(bool_::new_bool(f.readable))
}
fn fio_writable(o: &PyObject) -> PyResult {
    let f = fio(o);
    check_open(f)?;
    Ok(bool_::new_bool(f.writable))
}
fn fio_fileno(o: &PyObject) -> PyResult {
    let f = fio(o);
    check_open(f)?;
    Ok(int::new_int(f.fd as i64))
}
fn fio_isatty(o: &PyObject) -> PyResult {
    let f = fio(o);
    check_open(f)?;
    Ok(bool_::new_bool(unsafe { libc::isatty(f.fd) } == 1))
}

fn fio_close(o: &PyObject) -> PyResult {
    // flush via _RawIOBase.close (the base class close method)
    let base_close = crate::types::typeobject::lookup_str(super::iobase::iobase_type(), "close").unwrap();
    let r = ab::call_unbound(&base_close, o, &[]);
    let f = fio(o);
    if f.finalizing && f.fd >= 0 && f.closefd {
        let saved = take_exc();
        if resource_warning(o).is_err() {
            clear_exc();
        }
        if let Some(s) = saved {
            set_exc(s);
        }
    }
    let rc = internal_close(fio(o));
    r?;
    rc?;
    Ok(none::none_ref())
}

fn fio_dealloc_warn(o: &PyObject, source: &PyObject) -> PyResult {
    let f = fio(o);
    if f.fd >= 0 && f.closefd {
        if resource_warning(source).is_err() {
            if exc_matches(crate::types::exceptions::Warning()) {
                return Err(PyErr);
            }
            clear_exc();
        }
    }
    Ok(none::none_ref())
}

fn fio_get_closed(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(fio(o).fd < 0))
}
fn fio_get_closefd(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(fio(o).closefd))
}
pub fn mode_string(f: &FileIO) -> &'static str {
    if f.created {
        if f.readable { "xb+" } else { "xb" }
    } else if f.appending {
        if f.readable { "ab+" } else { "ab" }
    } else if f.truncating {
        if f.readable { "wb+" } else { "wb" }
    } else if f.readable {
        if f.writable { "rb+" } else { "rb" }
    } else {
        "wb"
    }
}
fn fio_get_mode(o: &PyObject) -> PyResult {
    Ok(pystr::new_str(mode_string(fio(o))))
}
fn fio_get_blksize(o: &PyObject) -> PyResult {
    Ok(int::new_int(fio(o).blksize))
}
fn fio_get_finalizing(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(fio(o).finalizing))
}
fn fio_set_finalizing(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match v {
        None => raise!(TypeError, "can't delete numeric/char attribute"),
        Some(v) if crate::types::bool_::is_bool(v) => {
            fio(o).finalizing = v.is(bool_::true_obj());
            Ok(())
        }
        Some(_) => raise!(TypeError, "attribute value type must be bool"),
    }
}

unsafe extern "C" fn fileio_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        let r = (|| -> PyResult {
            let tpname = crate::types::typeobject::type_qualname(o.ty());
            let modname = crate::types::typeobject::type_module(o.type_ptr()).unwrap_or_default();
            let full = if modname.is_empty() || modname == "builtins" { tpname } else { format!("{}.{}", modname, tpname) };
            let f = fio(o);
            if f.fd < 0 {
                return Ok(pystr::new_str(&format!("<{} [closed]>", full)));
            }
            let mode = mode_string(f);
            let closefd = if f.closefd { "True" } else { "False" };
            let name = ab::getattr_opt(o, crate::name!("name"))?;
            match name {
                None => Ok(pystr::new_str(&format!("<{} fd={} mode='{}' closefd={}>", full, f.fd, mode, closefd))),
                Some(n) => {
                    if crate::modules::reprlib_guard::enter(o) {
                        raise!(RuntimeError, "reentrant call inside {}.__repr__", full);
                    }
                    let nr = ab::repr_string(&n);
                    crate::modules::reprlib_guard::leave(o);
                    Ok(pystr::new_str(&format!("<{} name={} mode='{}' closefd={}>", full, nr?, mode, closefd)))
                }
            }
        })();
        into_c(r)
    }
}

fn fio_getstate(o: &PyObject) -> PyResult {
    raise!(TypeError, "cannot pickle '{}' instances", crate::types::typeobject::type_name(o.ty()));
}

pub fn init_types() -> PyResult<()> {
    unsafe {
        if FILEIO_TYPE.tp_flags & Py_TPFLAGS_READY != 0 {
            return Ok(());
        }
        setup_type(
            fileio_type(),
            c"_io.FileIO",
            std::mem::size_of::<FileIO>(),
            super::iobase::rawiobase_type(),
            "FileIO(file, mode='r', closefd=True, opener=None)\n--\n\nOpen a file.\n\nThe mode can be 'r' (default), 'w', 'x' or 'a' for reading,\nwriting, exclusive creation or appending.  The file will be created if it\ndoesn't exist when opened for writing or appending; it will be truncated\nwhen opened for writing.  A FileExistsError will be raised if it already\nexists when opened for creating. Opening a file for creating implies\nwriting so this mode behaves in a similar way to 'w'.Add a '+' to the mode\nto allow simultaneous reading and writing. A custom opener can be used by\npassing a callable as *opener*. The underlying file descriptor for the file\nobject is then obtained by calling opener with (*name*, *flags*).\n*opener* must return an open file descriptor (passing os.open as *opener*\nresults in functionality similar to passing None).",
        );
        let t = &mut *fileio_type();
        t.tp_new = Some(fileio_new);
        t.tp_init = Some(fileio_init);
        t.tp_dealloc = Some(fileio_dealloc);
        t.tp_traverse = Some(fileio_traverse);
        t.tp_clear = Some(fileio_clear);
        t.tp_repr = Some(fileio_repr);
        t.tp_methods = crate::mdefs![
            ("read", FAST, fio_read, "read($self, size=-1, /)\n--\n\nRead at most size bytes with one system call; -1 reads to EOF."),
            ("readall", NOARGS, fio_readall),
            ("readinto", O, fio_readinto),
            ("write", O, fio_write),
            ("seek", FAST, fio_seek, "seek($self, pos, whence=0, /)\n--\n\nMove to a new file position and return it."),
            ("tell", NOARGS, fio_tell),
            ("truncate", FAST, fio_truncate, "truncate($self, size=None, /)\n--\n\nResize the file to size bytes (default: the current position)."),
            ("close", NOARGS, fio_close),
            ("seekable", NOARGS, fio_seekable),
            ("readable", NOARGS, fio_readable),
            ("writable", NOARGS, fio_writable),
            ("fileno", NOARGS, fio_fileno),
            ("isatty", NOARGS, fio_isatty),
            ("_dealloc_warn", O, fio_dealloc_warn),
            ("__getstate__", NOARGS, fio_getstate),
        ];
        t.tp_getset = crate::getset_table![
            ("closed", fio_get_closed),
            ("closefd", fio_get_closefd),
            ("mode", fio_get_mode),
            ("_blksize", fio_get_blksize),
            ("_finalizing", fio_get_finalizing, fio_set_finalizing),
        ];
        crate::types::typeobject::type_ready(fileio_type())?;
    }
    Ok(())
}
