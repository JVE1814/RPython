//! Argument converters shared by the posix functions (CPython's path_t, dir_fd, fd ...).
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::PyRef;
use crate::raise;
use crate::types::typeobject::tname;
use crate::types::{bytes, int, none, str as pystr};
use std::ffi::CString;

/// A converted path argument.
pub struct PathArg {
    /// The original argument (used as OSError.filename).
    pub object: PyRef,
    /// True if the result of functions returning paths should be bytes.
    pub narrow: bool,
    /// File descriptor (when an int was accepted).
    pub fd: Option<i32>,
    /// Encoded path (empty when fd or None).
    pub c: CString,
    pub is_none: bool,
}

impl PathArg {
    pub fn ptr(&self) -> *const libc::c_char {
        self.c.as_ptr()
    }
    pub fn bytes(&self) -> &[u8] {
        self.c.as_bytes()
    }
    pub fn none() -> PathArg {
        PathArg { object: none::none_ref(), narrow: false, fd: None, c: CString::new(".").unwrap(), is_none: true }
    }
    /// The filename object for exceptions.
    pub fn obj(&self) -> &PyObject {
        &self.object
    }
    /// Encode a returned path in the same flavor (str or bytes) as this argument.
    pub fn wrap(&self, b: &[u8]) -> PyResult {
        if self.narrow { Ok(bytes::new_bytes(b)) } else { fs_decode(b) }
    }
    pub fn err(&self, errno: i32) -> PyErr {
        crate::types::exceptions::os_error_from_errno(errno, Some(&self.object), None)
    }
}

/// UTF-8 + surrogateescape encode of a str.
pub fn fs_encode_str(s: &PyObject) -> PyResult<Vec<u8>> {
    if let Some(a) = pystr::ascii_str(s) {
        return Ok(a.as_bytes().to_vec());
    }
    let mut out = Vec::with_capacity(pystr::str_len(s));
    for (i, c) in pystr::str_data(s).iter().enumerate() {
        match char::from_u32(c) {
            Some(ch) => {
                let mut b = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
            }
            None => {
                if (0xdc80..0xdd00).contains(&c) {
                    out.push((c - 0xdc00) as u8);
                } else {
                    return Err(crate::types::exceptions::unicode_encode_error("utf-8", s, i, i + 1, "surrogates not allowed"));
                }
            }
        }
    }
    Ok(out)
}

/// UTF-8 + surrogateescape decode.
pub fn fs_decode(b: &[u8]) -> PyResult {
    if let Ok(s) = std::str::from_utf8(b) {
        return Ok(pystr::new_str(s));
    }
    crate::modules::codecs_core::decode_utf8(b, "surrogateescape")
}

/// Result of calling os.fspath() semantics: str or bytes.
pub fn fspath(o: &PyObject) -> PyResult {
    if pystr::is_str(o) || bytes::is_bytes(o) {
        return Ok(o.to_ref());
    }
    match lookup_fspath(o)? {
        Some(f) => {
            let r = ab::call_unbound(&f, o, &[])?;
            if pystr::is_str(&r) || bytes::is_bytes(&r) {
                Ok(r)
            } else {
                raise!(TypeError, "expected {}.__fspath__() to return str or bytes, not {}", tname(o), tname(&r))
            }
        }
        None => raise!(TypeError, "expected str, bytes or os.PathLike object, not {}", tname(o)),
    }
}

fn lookup_fspath(o: &PyObject) -> PyResult<Option<PyRef>> {
    match crate::types::typeobject::lookup(o.type_ptr(), crate::name!("__fspath__")) {
        Some(f) if !f.is(none::none()) => Ok(Some(f)),
        _ => Ok(None),
    }
}

pub struct PathSpec<'a> {
    pub func: &'a str,
    pub arg: &'a str,
    pub allow_fd: bool,
    pub nullable: bool,
}

pub fn path_converter(spec: &PathSpec, o: &PyObject) -> PyResult<PathArg> {
    if spec.nullable && o.is(none::none()) {
        return Ok(PathArg::none());
    }
    let (b, narrow): (Vec<u8>, bool) = if pystr::is_str(o) {
        (fs_encode_str(o)?, false)
    } else if bytes::is_bytes(o) {
        (bytes::bytes_data(o).to_vec(), true)
    } else if let Some(f) = lookup_fspath(o)? {
        let r = ab::call_unbound(&f, o, &[])?;
        if pystr::is_str(&r) {
            (fs_encode_str(&r)?, false)
        } else if bytes::is_bytes(&r) {
            (bytes::bytes_data(&r).to_vec(), true)
        } else {
            raise!(TypeError, "expected {}.__fspath__() to return str or bytes, not {}", tname(o), tname(&r));
        }
    } else if spec.allow_fd && ab::has_index(o) {
        let fd = fd_from_index(o)?;
        return Ok(PathArg { object: o.to_ref(), narrow: false, fd: Some(fd), c: CString::default(), is_none: false });
    } else {
        let allowed = match (spec.allow_fd, spec.nullable) {
            (true, true) => "string, bytes, os.PathLike, integer or None",
            (true, false) => "string, bytes, os.PathLike or integer",
            (false, true) => "string, bytes, os.PathLike or None",
            (false, false) => "string, bytes or os.PathLike",
        };
        raise!(TypeError, "{}: {} should be {}, not {}", spec.func, spec.arg, allowed, tname(o));
    };
    if b.contains(&0) {
        raise!(ValueError, "{}: embedded null character in {}", spec.func, spec.arg);
    }
    Ok(PathArg { object: o.to_ref(), narrow, fd: None, c: CString::new(b).unwrap(), is_none: false })
}

pub fn path(func: &str, o: &PyObject) -> PyResult<PathArg> {
    path_converter(&PathSpec { func, arg: "path", allow_fd: false, nullable: false }, o)
}
pub fn path_fd(func: &str, o: &PyObject) -> PyResult<PathArg> {
    path_converter(&PathSpec { func, arg: "path", allow_fd: true, nullable: false }, o)
}
pub fn path_named(func: &str, arg: &str, o: &PyObject) -> PyResult<PathArg> {
    path_converter(&PathSpec { func, arg, allow_fd: false, nullable: false }, o)
}

/// int -> fd (C int range).
pub fn fd_from_index(o: &PyObject) -> PyResult<i32> {
    if crate::types::float::is_float(o) {
        raise!(TypeError, "'float' object cannot be interpreted as an integer");
    }
    let i = ab::index(o)?;
    match int::as_i64(&i) {
        Some(v) if v > i32::MAX as i64 => raise!(OverflowError, "fd is greater than maximum"),
        Some(v) if v < i32::MIN as i64 => raise!(OverflowError, "fd is less than minimum"),
        Some(v) => Ok(v as i32),
        None => {
            if int::sign(&i) < 0 {
                raise!(OverflowError, "fd is less than minimum")
            } else {
                raise!(OverflowError, "fd is greater than maximum")
            }
        }
    }
}

/// C `int` argument ('i' format).
pub fn c_int(o: &PyObject) -> PyResult<i32> {
    if crate::types::float::is_float(o) {
        raise!(TypeError, "'float' object cannot be interpreted as an integer");
    }
    let i = ab::index(o)?;
    match int::as_i64(&i) {
        Some(v) if v > i32::MAX as i64 => raise!(OverflowError, "signed integer is greater than maximum"),
        Some(v) if v < i32::MIN as i64 => raise!(OverflowError, "signed integer is less than minimum"),
        Some(v) => Ok(v as i32),
        None => {
            if int::sign(&i) < 0 {
                raise!(OverflowError, "signed integer is less than minimum")
            } else {
                raise!(OverflowError, "signed integer is greater than maximum")
            }
        }
    }
}

/// C `long` / `long long`.
pub fn c_long(o: &PyObject) -> PyResult<i64> {
    if crate::types::float::is_float(o) {
        raise!(TypeError, "'float' object cannot be interpreted as an integer");
    }
    ab::as_i64(o)
}

/// Py_ssize_t ('n' format).
pub fn ssize(o: &PyObject) -> PyResult<isize> {
    if crate::types::float::is_float(o) {
        raise!(TypeError, "'float' object cannot be interpreted as an integer");
    }
    ab::index_as_ssize(o, Some(crate::types::exceptions::OverflowError()))
}

/// Unsigned C long long, accepting -1 as all-ones for uid_t/gid_t style args.
pub fn c_u64(o: &PyObject) -> PyResult<u64> {
    let i = ab::index(o)?;
    match int::as_i128(&i) {
        Some(v) if v >= 0 && v <= u64::MAX as i128 => Ok(v as u64),
        Some(_) if int::sign(&i) < 0 => raise!(OverflowError, "can't convert negative int to unsigned"),
        _ => raise!(OverflowError, "Python int too large to convert to C unsigned long"),
    }
}

/// dir_fd argument: None -> AT_FDCWD.
pub fn dir_fd(o: Option<&PyObject>) -> PyResult<i32> {
    match o {
        None => Ok(libc::AT_FDCWD),
        Some(x) if x.is(none::none()) => Ok(libc::AT_FDCWD),
        Some(x) => {
            if !ab::has_index(x) {
                raise!(TypeError, "argument should be integer or None, not {}", tname(x));
            }
            fd_from_index(x)
        }
    }
}

/// uid_t / gid_t argument: -1 allowed.
pub fn id_arg(o: &PyObject, what: &str) -> PyResult<u32> {
    if !ab::has_index(o) || crate::types::float::is_float(o) {
        raise!(TypeError, "{} should be integer, not {}", what, tname(o));
    }
    let i = ab::index(o)?;
    match int::as_i128(&i) {
        Some(-1) => Ok(u32::MAX),
        Some(v) if v >= 0 && v < u32::MAX as i128 => Ok(v as u32),
        Some(v) if v < 0 => raise!(OverflowError, "{} is less than minimum", what),
        _ => raise!(OverflowError, "{} is greater than maximum", what),
    }
}

/// uid/gid result: values that do not fit a signed int stay unsigned.
pub fn id_result(v: u32) -> PyRef {
    if v == u32::MAX { int::new_int(-1) } else { int::new_int(v as i64) }
}

/// bool argument.
pub fn truthy(o: Option<&PyObject>, default: bool) -> PyResult<bool> {
    match o {
        None => Ok(default),
        Some(x) => ab::is_true(x),
    }
}

/// Object -> file descriptor (int, or object with fileno()).
pub fn as_file_descriptor(o: &PyObject) -> PyResult<i32> {
    let fd = if int::is_int(o) {
        c_int(o)?
    } else {
        match ab::getattr_opt(o, crate::name!("fileno"))? {
            Some(m) => {
                let r = ab::call(&m, &[])?;
                if !int::is_int(&r) {
                    raise!(TypeError, "fileno() returned a non-integer");
                }
                c_int(&r)?
            }
            None => raise!(TypeError, "argument must be an int, or have a fileno() method."),
        }
    };
    if fd < 0 {
        raise!(ValueError, "file descriptor cannot be a negative integer ({})", fd);
    }
    Ok(fd)
}

/// errno of the last failing libc call.
#[inline]
pub fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// OSError from errno without filename.
pub fn os_err() -> PyErr {
    crate::types::exceptions::os_error_from_errno(errno(), None, None)
}
pub fn os_err_with(p: &PathArg) -> PyErr {
    p.err(errno())
}
pub fn os_err_with2(p: &PathArg, p2: &PathArg) -> PyErr {
    crate::types::exceptions::os_error_from_errno(errno(), Some(&p.object), Some(&p2.object))
}

/// Run `f` retrying on EINTR, checking for pending signals between attempts.
pub fn retry_eintr<T: PartialEq + Copy>(fail: T, mut f: impl FnMut() -> T) -> PyResult<T> {
    loop {
        let r = f();
        if r == fail && errno() == libc::EINTR {
            check_signals()?;
            continue;
        }
        return Ok(r);
    }
}

/// Run pending signal handlers (PyErr_CheckSignals).
pub fn check_signals() -> PyResult<()> {
    crate::modules::signal::run_pending_handlers(crate::threads::current())
}
