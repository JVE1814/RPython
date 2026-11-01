//! `_socket`: the BSD socket interface (Linux).
//!
//! The socket object keeps CPython's blocking/timeout state machine: a timeout
//! of -1 means blocking, 0 non-blocking, > 0 the fd is non-blocking and every
//! operation first waits with poll() until the deadline. Every blocking
//! syscall runs inside `threads::blocking` and EINTR is retried after running
//! pending signal handlers (PEP 475).

pub mod addr;
pub mod consts;
pub mod netdb;

use crate::abstract_ as ab;
use crate::buffer::{BufferView, PyBUF_SIMPLE, PyBUF_WRITABLE};
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::*;
use crate::types::{bool_, bytes, dict, float, int, list, none, str as pystr, tuple};
use addr::SockAddr;
use std::ffi::c_int;
use std::sync::atomic::{AtomicI32, AtomicI64, AtomicPtr, Ordering};

#[repr(C)]
pub struct SockObj {
    ob_base: PyObject,
    pub fd: AtomicI32,
    pub family: i32,
    pub ty: i32,
    pub proto: i32,
    /// Timeout in nanoseconds; -1 = blocking (no timeout).
    pub timeout: AtomicI64,
}

pub static mut SOCK_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub fn sock_type() -> *mut PyTypeObject {
    &raw mut SOCK_TYPE
}

pub static HERROR: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
pub static GAIERROR: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());

/// Default timeout for new sockets (ns, -1 = None).
static DEFAULT_TIMEOUT: AtomicI64 = AtomicI64::new(-1);

pub fn sk(o: &PyObject) -> &SockObj {
    unsafe { &*(o.as_ptr() as *const SockObj) }
}

pub fn errno() -> i32 {
    unsafe { *libc::__errno_location() }
}
pub fn os_error(e: i32) -> PyErr {
    crate::types::exceptions::os_error_from_errno(e, None, None)
}
pub fn timeout_error() -> PyErr {
    new_err(crate::types::exceptions::TimeoutError(), "timed out")
}

pub fn mono_ns() -> i64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as i64 * 1_000_000_000 + ts.tv_nsec as i64
}

fn check_signals() -> PyResult<()> {
    crate::modules::signal::check_signals()
}

// ------------------------------------------------------------------ timeouts

const TS_OVERFLOW: &str = "timestamp too large to convert to C _PyTime_t";

/// Convert a seconds object to nanoseconds, rounding away from zero (timeouts).
pub fn seconds_to_ns(o: &PyObject) -> PyResult<i64> {
    if int::is_int(o) {
        let v = match int::as_i64(o) {
            Some(v) => v,
            None => raise!(OverflowError, "{}", TS_OVERFLOW),
        };
        return match v.checked_mul(1_000_000_000) {
            Some(n) => Ok(n),
            None => raise!(OverflowError, "{}", TS_OVERFLOW),
        };
    }
    let f = if float::is_float(o) {
        float::float_value(o)
    } else if ab::has_index(o) {
        let i = ab::index(o)?;
        return seconds_to_ns(&i);
    } else {
        ab::float_value(o)?
    };
    if f.is_nan() {
        raise!(ValueError, "Invalid value NaN (not a number)");
    }
    let d = f * 1e9;
    let d = if d >= 0.0 { d.ceil() } else { d.floor() };
    if !(d >= -9.223372036854775808e18 && d < 9.223372036854775808e18) {
        raise!(OverflowError, "{}", TS_OVERFLOW);
    }
    Ok(d as i64)
}

/// socket timeout argument: None -> -1; negative -> ValueError.
pub fn parse_timeout(o: &PyObject) -> PyResult<i64> {
    if none::is_none(o) {
        return Ok(-1);
    }
    let ns = seconds_to_ns(o)?;
    if ns < 0 {
        raise!(ValueError, "Timeout value out of range");
    }
    Ok(ns)
}

fn timeout_obj(ns: i64) -> PyRef {
    if ns < 0 {
        none::none_ref()
    } else if ns % 1_000_000_000 == 0 {
        float::new_float((ns / 1_000_000_000) as f64)
    } else {
        float::new_float(ns as f64 / 1e9)
    }
}

// ------------------------------------------------------------------ syscall machinery

pub enum Fail {
    Os(i32),
    Timeout,
}

impl Fail {
    pub fn raise(self) -> PyErr {
        match self {
            Fail::Os(e) => os_error(e),
            Fail::Timeout => timeout_error(),
        }
    }
}

/// poll() the fd. Ok(true) = timed out.
fn internal_select(fd: i32, writing: bool, interval_ns: i64, connect: bool) -> Result<bool, i32> {
    if fd < 0 {
        return Ok(false);
    }
    let mut pfd = libc::pollfd { fd, events: if writing { libc::POLLOUT } else { libc::POLLIN }, revents: 0 };
    if connect {
        pfd.events |= libc::POLLERR;
    }
    let ms: c_int = if interval_ns < 0 {
        -1
    } else {
        let ms = (interval_ns + 999_999) / 1_000_000;
        ms.min(c_int::MAX as i64) as c_int
    };
    let (n, e) = crate::threads::blocking(|| {
        let n = unsafe { libc::poll(&mut pfd, 1, ms) };
        (n, if n < 0 { errno() } else { 0 })
    });
    if n < 0 {
        return Err(e);
    }
    Ok(n == 0)
}

/// Run `f(fd)` (returns < 0 on failure with errno set) with the socket's
/// timeout semantics. Python exceptions (signal handlers) are returned as Err;
/// OS-level failures as Ok(Err(..)).
pub fn sock_call_ex(
    s: &SockObj,
    writing: bool,
    f: &mut dyn FnMut(i32) -> isize,
    connect: bool,
    timeout: i64,
) -> PyResult<Result<isize, Fail>> {
    let has_timeout = timeout > 0;
    let mut deadline = 0i64;
    let mut deadline_init = false;
    loop {
        if has_timeout || connect {
            let fd = s.fd.load(Ordering::Relaxed);
            let res = if has_timeout {
                let interval = if deadline_init {
                    deadline - mono_ns()
                } else {
                    deadline_init = true;
                    deadline = mono_ns() + timeout;
                    timeout
                };
                if interval >= 0 { internal_select(fd, writing, interval, connect) } else { Ok(true) }
            } else {
                internal_select(fd, writing, -1, connect)
            };
            match res {
                Err(e) if e == libc::EINTR => {
                    check_signals()?;
                    continue;
                }
                Err(e) => return Ok(Err(Fail::Os(e))),
                Ok(true) => return Ok(Err(Fail::Timeout)),
                Ok(false) => {}
            }
        }
        let err;
        loop {
            let fd = s.fd.load(Ordering::Relaxed);
            let (r, e) = crate::threads::blocking(|| {
                let r = f(fd);
                (r, if r < 0 { errno() } else { 0 })
            });
            if r >= 0 {
                return Ok(Ok(r));
            }
            if e == libc::EINTR {
                check_signals()?;
                continue;
            }
            err = e;
            break;
        }
        if s.timeout.load(Ordering::Relaxed) > 0 && (err == libc::EWOULDBLOCK || err == libc::EAGAIN) {
            continue;
        }
        return Ok(Err(Fail::Os(err)));
    }
}

pub fn sock_call(s: &SockObj, writing: bool, f: &mut dyn FnMut(i32) -> isize) -> PyResult<isize> {
    let t = s.timeout.load(Ordering::Relaxed);
    match sock_call_ex(s, writing, f, false, t)? {
        Ok(r) => Ok(r),
        Err(e) => Err(e.raise()),
    }
}

fn internal_setblocking(s: &SockObj, block: bool) -> PyResult<()> {
    let fd = s.fd.load(Ordering::Relaxed);
    let r = unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFL);
        if fl < 0 {
            -1
        } else {
            let nfl = if block { fl & !libc::O_NONBLOCK } else { fl | libc::O_NONBLOCK };
            if nfl != fl { libc::fcntl(fd, libc::F_SETFL, nfl) } else { 0 }
        }
    };
    if r < 0 {
        return Err(os_error(errno()));
    }
    Ok(())
}

// ------------------------------------------------------------------ object lifecycle

unsafe extern "C" fn sock_new(tp: *mut PyTypeObject, _args: *mut PyObject, _kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
        let op = alloc(tp, 0);
        if op.is_null() {
            return op;
        }
        let s = &mut *(op as *mut SockObj);
        s.fd = AtomicI32::new(-1);
        s.family = 0;
        s.ty = 0;
        s.proto = 0;
        s.timeout = AtomicI64::new(-1);
        op
    }
}

/// Fill a freshly allocated socket object (init_sockobject).
pub fn init_sockobject(s: &mut SockObj, fd: i32, family: i32, ty: i32, proto: i32) -> PyResult<()> {
    s.fd.store(fd, Ordering::Relaxed);
    s.family = family;
    s.ty = ty & !(libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC);
    s.proto = proto;
    if ty & libc::SOCK_NONBLOCK != 0 {
        s.timeout.store(0, Ordering::Relaxed);
    } else {
        let d = DEFAULT_TIMEOUT.load(Ordering::Relaxed);
        s.timeout.store(d, Ordering::Relaxed);
        if d >= 0 {
            internal_setblocking(s, false)?;
        }
    }
    Ok(())
}

/// New `_socket.socket` wrapping an existing fd.
pub fn new_sockobject(fd: i32, family: i32, ty: i32, proto: i32) -> PyResult {
    unsafe {
        let op = sock_new(sock_type(), std::ptr::null_mut(), std::ptr::null_mut());
        if op.is_null() {
            return Err(PyErr);
        }
        let o = PyRef::from_raw(op);
        if let Err(e) = init_sockobject(&mut *(op as *mut SockObj), fd, family, ty, proto) {
            // do not close the fd twice: the caller owns it on failure
            (*(op as *mut SockObj)).fd.store(-1, Ordering::Relaxed);
            return Err(e);
        }
        Ok(o)
    }
}

fn int_arg(o: &PyObject) -> PyResult<i32> {
    crate::modules::posix::path::c_int(o)
}

unsafe extern "C" fn sock_init(op: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let r = (|| -> PyResult<()> {
            let (v, kwn) = crate::args::kwdict_to_vectorcall(&*args, kw);
            let refs: Vec<&PyObject> = v.iter().map(|x| &**x).collect();
            let a = crate::modules::posix::kwparse::<4>("socket", &["family", "type", "proto", "fileno"], 0, 0, 0, &refs, kwn.as_deref())?;
            let mut family = match a[0] { Some(o) => int_arg(o)?, None => -1 };
            let mut ty = match a[1] { Some(o) => int_arg(o)?, None => -1 };
            let mut proto = match a[2] { Some(o) => int_arg(o)?, None => -1 };
            let fdobj = a[3].filter(|o| !none::is_none(o));
            let s = &mut *(op as *mut SockObj);
            let fd;
            if let Some(fo) = fdobj {
                if float::is_float(fo) {
                    raise!(TypeError, "'float' object cannot be interpreted as an integer");
                }
                let v = ab::index(fo)?;
                let fdv = match int::as_i64(&v) {
                    Some(x) => x,
                    None => raise!(OverflowError, "Python int too large to convert to C int"),
                };
                if fdv < 0 {
                    raise!(ValueError, "negative file descriptor");
                }
                if fdv > i32::MAX as i64 {
                    raise!(OverflowError, "Python int too large to convert to C int");
                }
                fd = fdv as i32;
                let mut sa = SockAddr::new();
                let r = libc::getsockname(fd, sa.as_mut_ptr(), &mut sa.len);
                if r == 0 {
                    if family == -1 {
                        family = sa.family();
                    }
                } else {
                    let e = errno();
                    if e == libc::EBADF || e == libc::ENOTSOCK {
                        return Err(os_error(e));
                    }
                }
                if ty == -1 {
                    let mut v: c_int = 0;
                    let mut l = std::mem::size_of::<c_int>() as libc::socklen_t;
                    if libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_TYPE, &mut v as *mut c_int as *mut _, &mut l) != 0 {
                        return Err(os_error(errno()));
                    }
                    ty = v;
                }
                if proto == -1 {
                    let mut v: c_int = 0;
                    let mut l = std::mem::size_of::<c_int>() as libc::socklen_t;
                    if libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_PROTOCOL, &mut v as *mut c_int as *mut _, &mut l) != 0 {
                        return Err(os_error(errno()));
                    }
                    proto = v;
                }
            } else {
                if family == -1 {
                    family = libc::AF_INET;
                }
                if ty == -1 {
                    ty = libc::SOCK_STREAM;
                }
                if proto == -1 {
                    proto = 0;
                }
                let (f, e) = crate::threads::blocking(|| {
                    let f = libc::socket(family, ty | libc::SOCK_CLOEXEC, proto);
                    (f, if f < 0 { errno() } else { 0 })
                });
                if f < 0 {
                    return Err(os_error(e));
                }
                fd = f;
            }
            // re-initialising an open socket object: drop the previous fd reference (like CPython, no close)
            init_sockobject(s, fd, family, ty, proto)
        })();
        int_into_c(r)
    }
}

unsafe extern "C" fn sock_finalize(op: *mut PyObject) {
    unsafe {
        let s = &*(op as *const SockObj);
        if s.fd.load(Ordering::Relaxed) == -1 {
            return;
        }
        let saved = take_exc();
        let msg = match ab::repr_string(&*op) {
            Ok(r) => format!("unclosed {}", r),
            Err(_) => {
                clear_exc();
                "unclosed socket".to_string()
            }
        };
        if crate::modules::warnings::warn_with_source(crate::types::exceptions::ResourceWarning(), &msg, 1, Some(&*op)).is_err() {
            if crate::errors::exc_matches(crate::types::exceptions::Warning()) {
                crate::modules::sys::write_unraisable_with_obj("Exception ignored while finalizing socket", Some(&*op));
            } else {
                clear_exc();
            }
        }
        let fd = s.fd.swap(-1, Ordering::Relaxed);
        if fd != -1 {
            libc::close(fd);
        }
        if let Some(e) = saved {
            set_exc(e);
        }
    }
}

unsafe extern "C" fn sock_dealloc(op: *mut PyObject) {
    unsafe {
        let tp = (*op).type_ptr();
        if tp == sock_type() {
            if (*tp).tp_finalize.is_some() && !crate::gc::is_finalized(op) && crate::gc::call_finalizer_from_dealloc(op) {
                return;
            }
        }
        let s = &*(op as *const SockObj);
        let fd = s.fd.swap(-1, Ordering::Relaxed);
        if fd != -1 {
            libc::close(fd);
        }
        generic_free_and_type_decref(op, tp);
    }
}

unsafe extern "C" fn sock_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let s = &*(op as *const SockObj);
        let fd = s.fd.load(Ordering::Relaxed);
        into_c(Ok(pystr::new_str(&format!(
            "<socket object, fd={}, family={}, type={}, proto={}>",
            fd, s.family, s.ty, s.proto
        ))))
    }
}

// ------------------------------------------------------------------ getters

fn get_family(o: &PyObject) -> PyResult {
    Ok(int::new_int(sk(o).family as i64))
}
fn get_type(o: &PyObject) -> PyResult {
    Ok(int::new_int(sk(o).ty as i64))
}
fn get_proto(o: &PyObject) -> PyResult {
    Ok(int::new_int(sk(o).proto as i64))
}
fn get_timeout(o: &PyObject) -> PyResult {
    Ok(timeout_obj(sk(o).timeout.load(Ordering::Relaxed)))
}

// ------------------------------------------------------------------ methods

fn m_fileno(o: &PyObject) -> PyResult {
    Ok(int::new_int(sk(o).fd.load(Ordering::Relaxed) as i64))
}

fn m_close(o: &PyObject) -> PyResult {
    let s = sk(o);
    let fd = s.fd.swap(-1, Ordering::Relaxed);
    if fd != -1 {
        let (r, e) = crate::threads::blocking(|| {
            let r = unsafe { libc::close(fd) };
            (r, if r < 0 { errno() } else { 0 })
        });
        // ECONNRESET may be reported by close() when the peer reset the connection
        if r < 0 && e != libc::ECONNRESET {
            return Err(os_error(e));
        }
    }
    Ok(none::none_ref())
}

fn m_detach(o: &PyObject) -> PyResult {
    let fd = sk(o).fd.swap(-1, Ordering::Relaxed);
    Ok(int::new_int(fd as i64))
}

fn m_gettimeout(o: &PyObject) -> PyResult {
    get_timeout(o)
}

fn m_settimeout(o: &PyObject, v: &PyObject) -> PyResult {
    let t = parse_timeout(v)?;
    let s = sk(o);
    s.timeout.store(t, Ordering::Relaxed);
    internal_setblocking(s, t < 0)?;
    Ok(none::none_ref())
}

fn m_setblocking(o: &PyObject, v: &PyObject) -> PyResult {
    let block = ab::is_true(v)?;
    let s = sk(o);
    s.timeout.store(if block { -1 } else { 0 }, Ordering::Relaxed);
    internal_setblocking(s, block)?;
    Ok(none::none_ref())
}

fn m_getblocking(o: &PyObject) -> PyResult {
    let t = sk(o).timeout.load(Ordering::Relaxed);
    Ok(bool_::new_bool(t < 0))
}

fn m_bind(o: &PyObject, a: &PyObject) -> PyResult {
    let s = sk(o);
    let sa = addr::getsockaddrarg(s, a, "bind")?;
    audit("socket.bind", o, a)?;
    let fd = s.fd.load(Ordering::Relaxed);
    let r = unsafe { libc::bind(fd, sa.as_ptr(), sa.len) };
    if r < 0 {
        return Err(os_error(errno()));
    }
    Ok(none::none_ref())
}

fn audit(event: &str, o: &PyObject, a: &PyObject) -> PyResult<()> {
    crate::modules::sys_full::audit(event, &[o, a])
}

fn m_listen(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("listen", args, 0, 1)?;
    let s = sk(o);
    let mut backlog: i32 = if args.is_empty() { libc::SOMAXCONN.min(128) } else { int_arg(args[0])? };
    if backlog < 0 {
        backlog = 0;
    }
    let fd = s.fd.load(Ordering::Relaxed);
    let r = crate::threads::blocking(|| unsafe { libc::listen(fd, backlog) });
    if r < 0 {
        return Err(os_error(errno()));
    }
    Ok(none::none_ref())
}

fn m_accept(o: &PyObject) -> PyResult {
    let s = sk(o);
    let mut sa = SockAddr::new();
    let sap = &mut sa as *mut SockAddr;
    let r = sock_call(s, false, &mut |fd| unsafe {
        (*sap).len = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        libc::accept4(fd, (*sap).as_mut_ptr(), &mut (*sap).len, libc::SOCK_CLOEXEC) as isize
    })?;
    let newfd = r as i32;
    let a = match addr::makesockaddr(&sa, s.proto) {
        Ok(a) => a,
        Err(e) => {
            unsafe { libc::close(newfd) };
            return Err(e);
        }
    };
    Ok(tuple::new_tuple(vec![int::new_int(newfd as i64), a]))
}

/// connect() with the timeout state machine. Returns Ok(0) or Ok(errno) when !raise.
fn internal_connect(s: &SockObj, sa: &SockAddr, raise_: bool) -> PyResult<i32> {
    let fd = s.fd.load(Ordering::Relaxed);
    let (r, e) = crate::threads::blocking(|| {
        let r = unsafe { libc::connect(fd, sa.as_ptr(), sa.len) };
        (r, if r < 0 { errno() } else { 0 })
    });
    if r == 0 {
        return Ok(0);
    }
    let wait_connect;
    let timeout = s.timeout.load(Ordering::Relaxed);
    if e == libc::EINTR {
        check_signals()?;
        // the connection runs asynchronously: wait unless the socket is non-blocking
        wait_connect = timeout != 0;
    } else {
        wait_connect = timeout > 0 && e == libc::EINPROGRESS;
    }
    if !wait_connect {
        if raise_ {
            return Err(os_error(e));
        }
        return Ok(e);
    }
    let mut f = |fd: i32| -> isize {
        let mut err: c_int = 0;
        let mut l = std::mem::size_of::<c_int>() as libc::socklen_t;
        let r = unsafe { libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_ERROR, &mut err as *mut c_int as *mut _, &mut l) };
        if r != 0 {
            return -1;
        }
        if err == libc::EISCONN {
            return 0;
        }
        if err != 0 {
            unsafe { *libc::__errno_location() = err };
            return -1;
        }
        0
    };
    match sock_call_ex(s, true, &mut f, true, timeout)? {
        Ok(_) => Ok(0),
        Err(Fail::Os(e)) => {
            if raise_ {
                Err(os_error(e))
            } else {
                Ok(e)
            }
        }
        Err(Fail::Timeout) => {
            if raise_ {
                Err(timeout_error())
            } else {
                Ok(libc::EWOULDBLOCK)
            }
        }
    }
}

fn m_connect(o: &PyObject, a: &PyObject) -> PyResult {
    let s = sk(o);
    let sa = addr::getsockaddrarg(s, a, "connect")?;
    audit("socket.connect", o, a)?;
    internal_connect(s, &sa, true)?;
    Ok(none::none_ref())
}

fn m_connect_ex(o: &PyObject, a: &PyObject) -> PyResult {
    let s = sk(o);
    let sa = addr::getsockaddrarg(s, a, "connect_ex")?;
    audit("socket.connect", o, a)?;
    let r = internal_connect(s, &sa, false)?;
    Ok(int::new_int(r as i64))
}

fn m_getsockname(o: &PyObject) -> PyResult {
    let s = sk(o);
    let mut sa = SockAddr::new();
    let fd = s.fd.load(Ordering::Relaxed);
    let r = unsafe { libc::getsockname(fd, sa.as_mut_ptr(), &mut sa.len) };
    if r < 0 {
        return Err(os_error(errno()));
    }
    addr::makesockaddr(&sa, s.proto)
}

fn m_getpeername(o: &PyObject) -> PyResult {
    let s = sk(o);
    let mut sa = SockAddr::new();
    let fd = s.fd.load(Ordering::Relaxed);
    let r = unsafe { libc::getpeername(fd, sa.as_mut_ptr(), &mut sa.len) };
    if r < 0 {
        return Err(os_error(errno()));
    }
    addr::makesockaddr(&sa, s.proto)
}

fn m_shutdown(o: &PyObject, how: &PyObject) -> PyResult {
    let h = int_arg(how)?;
    let fd = sk(o).fd.load(Ordering::Relaxed);
    let r = crate::threads::blocking(|| unsafe { libc::shutdown(fd, h) });
    if r < 0 {
        return Err(os_error(errno()));
    }
    Ok(none::none_ref())
}

fn m_getsockopt(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("getsockopt", args, 2, 3)?;
    let s = sk(o);
    let level = int_arg(args[0])?;
    let optname = int_arg(args[1])?;
    let fd = s.fd.load(Ordering::Relaxed);
    if args.len() == 2 {
        if s.family == libc::AF_VSOCK {
            let mut v: u64 = 0;
            let mut l = 8 as libc::socklen_t;
            let r = unsafe { libc::getsockopt(fd, level, optname, &mut v as *mut u64 as *mut _, &mut l) };
            if r < 0 {
                return Err(os_error(errno()));
            }
            return Ok(int::new_int_u128(v as u128));
        }
        let mut v: c_int = 0;
        let mut l = std::mem::size_of::<c_int>() as libc::socklen_t;
        let r = unsafe { libc::getsockopt(fd, level, optname, &mut v as *mut c_int as *mut _, &mut l) };
        if r < 0 {
            return Err(os_error(errno()));
        }
        return Ok(int::new_int(v as i64));
    }
    let buflen = int_arg(args[2])?;
    if buflen <= 0 || buflen > 1024 {
        raise!(OSError, "getsockopt buflen out of range");
    }
    let mut buf = vec![0u8; buflen as usize];
    let mut l = buflen as libc::socklen_t;
    let r = unsafe { libc::getsockopt(fd, level, optname, buf.as_mut_ptr() as *mut _, &mut l) };
    if r < 0 {
        return Err(os_error(errno()));
    }
    Ok(bytes::new_bytes(&buf[..l as usize]))
}

fn m_setsockopt(o: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.len() < 3 {
        raise!(TypeError, "setsockopt() takes at least 3 arguments ({} given)", args.len());
    }
    if args.len() > 4 {
        raise!(TypeError, "setsockopt() takes at most 4 arguments ({} given)", args.len());
    }
    let s = sk(o);
    let level = int_arg(args[0])?;
    let optname = int_arg(args[1])?;
    let fd = s.fd.load(Ordering::Relaxed);
    let v = args[2];
    let r;
    if none::is_none(v) {
        if args.len() != 4 {
            raise!(TypeError, "setsockopt() requires 4 arguments when the third argument is None");
        }
        let optlen = crate::modules::posix::path::c_long(args[3])?;
        if optlen < 0 || optlen > u32::MAX as i64 {
            raise!(OverflowError, "Python int too large to convert to C unsigned int");
        }
        r = unsafe { libc::setsockopt(fd, level, optname, std::ptr::null(), optlen as libc::socklen_t) };
    } else {
        if args.len() == 4 {
            raise!(TypeError, "setsockopt() only takes 4 arguments when the third argument is None");
        }
        if int::is_int(v) || (ab::has_index(v) && !crate::buffer::check_buffer(v)) {
            if s.family == libc::AF_VSOCK {
                let x = ab::index(v)?;
                let val: u64 = match int::as_i128(&x) {
                    Some(n) if n >= 0 && n <= u64::MAX as i128 => n as u64,
                    _ => raise!(OverflowError, "Python int too large to convert to C unsigned long long"),
                };
                r = unsafe { libc::setsockopt(fd, level, optname, &val as *const u64 as *const _, 8) };
            } else {
                let val = int_arg(v)?;
                r = unsafe {
                    libc::setsockopt(fd, level, optname, &val as *const c_int as *const _, std::mem::size_of::<c_int>() as libc::socklen_t)
                };
            }
        } else if crate::buffer::check_buffer(v) {
            let b = BufferView::get(v, PyBUF_SIMPLE)?;
            r = unsafe { libc::setsockopt(fd, level, optname, b.view.buf as *const _, b.len() as libc::socklen_t) };
        } else {
            raise!(TypeError, "socket option should be int, bytes-like object or None");
        }
    }
    if r < 0 {
        return Err(os_error(errno()));
    }
    Ok(none::none_ref())
}

fn bufsize_arg(o: &PyObject) -> PyResult<isize> {
    ab::index_as_ssize(o, Some(crate::types::exceptions::OverflowError()))
}

fn flags_arg(o: Option<&PyObject>) -> PyResult<c_int> {
    match o {
        None => Ok(0),
        Some(f) => int_arg(f),
    }
}

fn recv_into_buf(s: &SockObj, buf: *mut u8, len: usize, flags: c_int) -> PyResult<isize> {
    let b = buf as usize;
    sock_call(s, false, &mut |fd| unsafe { libc::recv(fd, b as *mut libc::c_void, len, flags) })
}

fn m_recv(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("recv", args, 1, 2)?;
    let n = bufsize_arg(args[0])?;
    if n < 0 {
        raise!(ValueError, "negative buffersize in recv");
    }
    let flags = flags_arg(args.get(1).copied())?;
    let mut buf = vec![0u8; n as usize];
    let got = recv_into_buf(sk(o), buf.as_mut_ptr(), n as usize, flags)?;
    Ok(bytes::new_bytes(&buf[..got as usize]))
}

fn writable_buffer(o: &PyObject) -> PyResult<BufferView> {
    BufferView::get(o, PyBUF_WRITABLE)
}

fn m_recv_into(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let a = crate::modules::posix::kwparse::<3>("recv_into", &["buffer", "nbytes", "flags"], 0, 1, 0, args, kw)?;
    let b = writable_buffer(a[0].unwrap())?;
    let mut n = match a[1] { Some(x) => bufsize_arg(x)?, None => 0 };
    let flags = flags_arg(a[2])?;
    if n < 0 {
        raise!(ValueError, "negative buffersize in recv_into");
    }
    let blen = b.len() as isize;
    if n == 0 {
        n = blen;
    }
    if blen < n {
        raise!(ValueError, "buffer too small for requested bytes");
    }
    let got = recv_into_buf(sk(o), b.view.buf as *mut u8, n as usize, flags)?;
    Ok(int::new_int(got as i64))
}

fn recvfrom_guts(s: &SockObj, buf: *mut u8, len: usize, flags: c_int) -> PyResult<(isize, PyRef)> {
    let mut sa = SockAddr::new();
    let sap = &mut sa as *mut SockAddr;
    let b = buf as usize;
    let n = sock_call(s, false, &mut |fd| unsafe {
        (*sap).len = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        libc::recvfrom(fd, b as *mut libc::c_void, len, flags, (*sap).as_mut_ptr(), &mut (*sap).len)
    })?;
    let a = addr::makesockaddr(&sa, s.proto)?;
    Ok((n, a))
}

fn m_recvfrom(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("recvfrom", args, 1, 2)?;
    let n = bufsize_arg(args[0])?;
    if n < 0 {
        raise!(ValueError, "negative buffersize in recvfrom");
    }
    let flags = flags_arg(args.get(1).copied())?;
    let mut buf = vec![0u8; n as usize];
    let (got, a) = recvfrom_guts(sk(o), buf.as_mut_ptr(), n as usize, flags)?;
    Ok(tuple::new_tuple(vec![bytes::new_bytes(&buf[..got as usize]), a]))
}

fn m_recvfrom_into(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let a = crate::modules::posix::kwparse::<3>("recvfrom_into", &["buffer", "nbytes", "flags"], 0, 1, 0, args, kw)?;
    let b = writable_buffer(a[0].unwrap())?;
    let mut n = match a[1] { Some(x) => bufsize_arg(x)?, None => 0 };
    let flags = flags_arg(a[2])?;
    if n < 0 {
        raise!(ValueError, "negative buffersize in recvfrom_into");
    }
    let blen = b.len() as isize;
    if n == 0 {
        n = blen;
    } else if n > blen {
        raise!(ValueError, "nbytes is greater than the length of the buffer");
    }
    let (got, ad) = recvfrom_guts(sk(o), b.view.buf as *mut u8, n as usize, flags)?;
    Ok(tuple::new_tuple(vec![int::new_int(got as i64), ad]))
}

fn m_send(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("send", args, 1, 2)?;
    let b = BufferView::get(args[0], PyBUF_SIMPLE)?;
    let flags = flags_arg(args.get(1).copied())?;
    let p = b.view.buf as usize;
    let len = b.len();
    let n = sock_call(sk(o), true, &mut |fd| unsafe { libc::send(fd, p as *const libc::c_void, len, flags | libc::MSG_NOSIGNAL) })?;
    Ok(int::new_int(n as i64))
}

fn m_sendall(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("sendall", args, 1, 2)?;
    let b = BufferView::get(args[0], PyBUF_SIMPLE)?;
    let flags = flags_arg(args.get(1).copied())?;
    let s = sk(o);
    let base = b.view.buf as usize;
    let total = b.len();
    let mut off = 0usize;
    let timeout = s.timeout.load(Ordering::Relaxed);
    let has_timeout = timeout > 0;
    let deadline = if has_timeout { mono_ns() + timeout } else { 0 };
    loop {
        let interval = if has_timeout {
            let i = deadline - mono_ns();
            if i <= 0 {
                return Err(timeout_error());
            }
            i
        } else {
            timeout
        };
        let rem = total - off;
        let p = base + off;
        let r = sock_call_ex(
            s,
            true,
            &mut |fd| unsafe { libc::send(fd, p as *const libc::c_void, rem, flags | libc::MSG_NOSIGNAL) },
            false,
            interval,
        )?;
        match r {
            Ok(n) => off += n as usize,
            Err(e) => return Err(e.raise()),
        }
        // PEP 475: run signal handlers between partial sends
        check_signals()?;
        if off >= total {
            break;
        }
    }
    Ok(none::none_ref())
}

fn m_sendto(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let (data, flags, address) = match args.len() {
        2 => (args[0], None, args[1]),
        3 => (args[0], Some(args[1]), args[2]),
        n => raise!(TypeError, "sendto() takes 2 or 3 arguments ({} given)", n),
    };
    let b = BufferView::get(data, PyBUF_SIMPLE)?;
    let flags = flags_arg(flags)?;
    let s = sk(o);
    let sa = addr::getsockaddrarg(s, address, "sendto")?;
    audit("socket.sendto", o, address)?;
    let p = b.view.buf as usize;
    let len = b.len();
    let n = sock_call(s, true, &mut |fd| unsafe {
        libc::sendto(fd, p as *const libc::c_void, len, flags | libc::MSG_NOSIGNAL, sa.as_ptr(), sa.len)
    })?;
    Ok(int::new_int(n as i64))
}

// ------------------------------------------------------------------ sendmsg / recvmsg

pub const CMSG_HDR: usize = std::mem::size_of::<libc::cmsghdr>();
const SOCKLEN_T_LIMIT: usize = i32::MAX as usize;
fn cmsg_align(n: usize) -> usize {
    (n + 7) & !7
}
/// CMSG_LEN(n) with overflow checking.
fn get_cmsg_len(n: usize) -> Option<usize> {
    let base = cmsg_align(CMSG_HDR);
    if n > SOCKLEN_T_LIMIT - base {
        return None;
    }
    Some(base + n)
}
fn get_cmsg_space(n: usize) -> Option<usize> {
    let base = cmsg_align(CMSG_HDR);
    if n > SOCKLEN_T_LIMIT - base {
        return None;
    }
    let r = base + cmsg_align(n);
    if r > SOCKLEN_T_LIMIT {
        return None;
    }
    Some(r)
}

fn f_cmsg_len(_m: &PyObject, a: &PyObject) -> PyResult {
    let n = ab::index_as_ssize(a, Some(crate::types::exceptions::OverflowError()))?;
    match if n < 0 { None } else { get_cmsg_len(n as usize) } {
        Some(r) => Ok(int::new_int(r as i64)),
        None => raise!(OverflowError, "CMSG_LEN() argument out of range"),
    }
}

fn f_cmsg_space(_m: &PyObject, a: &PyObject) -> PyResult {
    let n = ab::index_as_ssize(a, Some(crate::types::exceptions::OverflowError()))?;
    match if n < 0 { None } else { get_cmsg_space(n as usize) } {
        Some(r) => Ok(int::new_int(r as i64)),
        None => raise!(OverflowError, "CMSG_SPACE() argument out of range"),
    }
}

/// Parse the received control buffer into a list of (level, type, data).
fn parse_ancillary(ctrl: *const u8, controllen: usize) -> PyResult {
    let out = list::new_list_empty();
    if ctrl.is_null() || controllen < CMSG_HDR {
        return Ok(out);
    }
    let base_len = cmsg_align(CMSG_HDR);
    let mut off = 0usize;
    while off + CMSG_HDR <= controllen {
        let c = unsafe { std::ptr::read_unaligned(ctrl.add(off) as *const libc::cmsghdr) };
        let clen = c.cmsg_len as usize;
        if clen < base_len {
            crate::modules::warnings::warn(
                crate::types::exceptions::RuntimeWarning(),
                "received malformed or improperly-truncated ancillary data",
                1,
            )?;
            break;
        }
        let doff = off + base_len;
        let mut dlen = clen - base_len;
        if doff > controllen {
            dlen = 0;
        } else if doff + dlen > controllen {
            dlen = controllen - doff;
        }
        let data = if dlen == 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(ctrl.add(doff), dlen) } };
        list::append(
            &out,
            tuple::new_tuple(vec![int::new_int(c.cmsg_level as i64), int::new_int(c.cmsg_type as i64), bytes::new_bytes(data)]),
        );
        let next = off + cmsg_align(clen);
        if next <= off {
            break;
        }
        off = next;
    }
    Ok(out)
}

/// Common part of recvmsg()/recvmsg_into(): returns (nbytes, ancdata, flags, address).
fn recvmsg_guts(s: &SockObj, iov: &mut [libc::iovec], ancbufsize: usize, flags: c_int) -> PyResult<(isize, PyRef, i32, PyRef)> {
    let mut sa = SockAddr::new();
    let words = (ancbufsize + 7) / 8;
    let mut ctrl: Vec<u64> = vec![0; words.max(1)];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_name = sa.as_mut_ptr() as *mut libc::c_void;
    msg.msg_namelen = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
    msg.msg_iov = iov.as_mut_ptr();
    msg.msg_iovlen = iov.len();
    if ancbufsize > 0 {
        msg.msg_control = ctrl.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = ancbufsize;
    }
    let mp = &mut msg as *mut libc::msghdr;
    let n = sock_call(s, false, &mut |fd| unsafe {
        (*mp).msg_namelen = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        (*mp).msg_controllen = ancbufsize;
        libc::recvmsg(fd, mp, flags)
    })?;
    let anc = parse_ancillary(if ancbufsize > 0 { ctrl.as_ptr() as *const u8 } else { std::ptr::null() }, msg.msg_controllen.min(ancbufsize))?;
    sa.len = msg.msg_namelen;
    let a = addr::makesockaddr(&sa, s.proto)?;
    Ok((n, anc, msg.msg_flags, a))
}

fn anc_size_arg(o: Option<&PyObject>, fname: &str) -> PyResult<usize> {
    match o {
        None => Ok(0),
        Some(x) => {
            let v = bufsize_arg(x)?;
            if v < 0 {
                raise!(ValueError, "negative buffer size in {}()", fname);
            }
            Ok(v as usize)
        }
    }
}

fn m_recvmsg(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("recvmsg", args, 1, 3)?;
    let n = bufsize_arg(args[0])?;
    if n < 0 {
        raise!(ValueError, "negative buffer size in recvmsg()");
    }
    let anc = match args.get(1) {
        None => 0,
        Some(x) => {
            let v = bufsize_arg(x)?;
            if v < 0 {
                raise!(ValueError, "negative ancillary buffer size in recvmsg()");
            }
            v as usize
        }
    };
    let flags = flags_arg(args.get(2).copied())?;
    let mut buf = vec![0u8; n as usize];
    let mut iov = [libc::iovec { iov_base: buf.as_mut_ptr() as *mut libc::c_void, iov_len: n as usize }];
    let (got, ancdata, mflags, a) = recvmsg_guts(sk(o), &mut iov, anc, flags)?;
    Ok(tuple::new_tuple(vec![bytes::new_bytes(&buf[..got as usize]), ancdata, int::new_int(mflags as i64), a]))
}

fn m_recvmsg_into(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("recvmsg_into", args, 1, 3)?;
    let it = match ab::get_iter(args[0]) {
        Ok(it) => it,
        Err(_) => {
            clear_exc();
            raise!(TypeError, "recvmsg_into() argument 1 must be an iterable");
        }
    };
    let anc = match args.get(1) {
        None => 0,
        Some(x) => {
            let v = bufsize_arg(x)?;
            if v < 0 {
                raise!(ValueError, "negative ancillary buffer size in recvmsg_into()");
            }
            v as usize
        }
    };
    let _ = anc_size_arg;
    let flags = flags_arg(args.get(2).copied())?;
    let mut views = Vec::new();
    while let Some(item) = ab::iter_next(&it)? {
        match BufferView::get(&item, PyBUF_WRITABLE) {
            Ok(v) => views.push(v),
            Err(_) => {
                clear_exc();
                raise!(
                    TypeError,
                    "recvmsg_into() argument 1 must be an iterable of single-segment read-write buffers"
                );
            }
        }
    }
    if views.len() > libc::UIO_MAXIOV as usize {
        raise!(OSError, "recvmsg_into() argument 1 is too long");
    }
    let mut iov: Vec<libc::iovec> = views.iter().map(|v| libc::iovec { iov_base: v.view.buf, iov_len: v.len() }).collect();
    let (got, ancdata, mflags, a) = recvmsg_guts(sk(o), &mut iov, anc, flags)?;
    Ok(tuple::new_tuple(vec![int::new_int(got as i64), ancdata, int::new_int(mflags as i64), a]))
}

/// Collect buffers for sendmsg(); returns the views (kept alive) and iovecs.
fn sendmsg_iovec(o: &PyObject, fname: &str) -> PyResult<(Vec<BufferView>, Vec<libc::iovec>)> {
    let it = match ab::get_iter(o) {
        Ok(it) => it,
        Err(_) => {
            clear_exc();
            raise!(TypeError, "{}() argument 1 must be an iterable", fname);
        }
    };
    let mut views = Vec::new();
    while let Some(item) = ab::iter_next(&it)? {
        views.push(BufferView::get(&item, PyBUF_SIMPLE)?);
    }
    if views.len() > libc::UIO_MAXIOV as usize {
        raise!(OSError, "{}() argument 1 is too long", fname);
    }
    let iov = views.iter().map(|v| libc::iovec { iov_base: v.view.buf, iov_len: v.len() }).collect();
    Ok((views, iov))
}

/// Build a control buffer from (level, type, data) items.
fn build_control(items: &[(c_int, c_int, Vec<u8>)]) -> PyResult<Vec<u64>> {
    let mut total = 0usize;
    for (_, _, d) in items {
        let sp = match get_cmsg_space(d.len()) {
            Some(s) => s,
            None => raise!(OSError, "ancillary data item too large"),
        };
        total = match total.checked_add(sp) {
            Some(t) if t <= SOCKLEN_T_LIMIT => t,
            _ => raise!(OSError, "too much ancillary data"),
        };
    }
    let mut buf: Vec<u64> = vec![0; (total + 7) / 8];
    let p = buf.as_mut_ptr() as *mut u8;
    let mut off = 0usize;
    for (level, ty, d) in items {
        let hdr = libc::cmsghdr { cmsg_len: get_cmsg_len(d.len()).unwrap() as _, cmsg_level: *level, cmsg_type: *ty };
        unsafe {
            std::ptr::write_unaligned(p.add(off) as *mut libc::cmsghdr, hdr);
            std::ptr::copy_nonoverlapping(d.as_ptr(), p.add(off + cmsg_align(CMSG_HDR)), d.len());
        }
        off += get_cmsg_space(d.len()).unwrap();
    }
    buf.truncate((total + 7) / 8);
    unsafe { buf.set_len((total + 7) / 8) };
    // record the exact byte length in the first unused position is not possible; callers use `total`
    Ok(buf)
}

fn control_len(items: &[(c_int, c_int, Vec<u8>)]) -> usize {
    items.iter().map(|(_, _, d)| get_cmsg_space(d.len()).unwrap_or(0)).sum()
}

fn m_sendmsg(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("sendmsg", args, 1, 4)?;
    let s = sk(o);
    let addr_obj = args.get(3).copied().filter(|a| !none::is_none(a));
    let sa = match addr_obj {
        Some(a) => Some(addr::getsockaddrarg(s, a, "sendmsg")?),
        None => None,
    };
    let (_views, mut iov) = sendmsg_iovec(args[0], "sendmsg")?;
    let mut items: Vec<(c_int, c_int, Vec<u8>)> = Vec::new();
    if let Some(anc) = args.get(1) {
        let it = match ab::get_iter(anc) {
            Ok(it) => it,
            Err(_) => {
                clear_exc();
                raise!(TypeError, "sendmsg() argument 2 must be an iterable");
            }
        };
        while let Some(item) = ab::iter_next(&it)? {
            if !tuple::is_tuple(&item) {
                raise!(TypeError, "[sendmsg() ancillary data items]() argument must be tuple, not {}", tname(&item));
            }
            let t = tuple::tuple_items(&item);
            if t.len() != 3 {
                raise!(
                    TypeError,
                    "[sendmsg() ancillary data items]() takes exactly 3 arguments ({} given)",
                    t.len()
                );
            }
            let level = int_arg(t[0])?;
            let ty = int_arg(t[1])?;
            let data = BufferView::get(t[2], PyBUF_SIMPLE)?.to_vec();
            items.push((level, ty, data));
        }
    }
    let flags = flags_arg(args.get(2).copied())?;
    let mut ctrl = build_control(&items)?;
    let clen = control_len(&items);
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    if let Some(sa) = &sa {
        msg.msg_name = sa.as_ptr() as *mut libc::c_void;
        msg.msg_namelen = sa.len;
    }
    msg.msg_iov = iov.as_mut_ptr();
    msg.msg_iovlen = iov.len();
    if clen > 0 {
        msg.msg_control = ctrl.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = clen;
    }
    if let Some(a) = addr_obj {
        audit("socket.sendmsg", o, a)?;
    } else {
        audit("socket.sendmsg", o, none::none())?;
    }
    let mp = &msg as *const libc::msghdr as usize;
    let n = sock_call(s, true, &mut |fd| unsafe { libc::sendmsg(fd, mp as *const libc::msghdr, flags | libc::MSG_NOSIGNAL) })?;
    Ok(int::new_int(n as i64))
}

fn m_sendmsg_afalg(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let a = crate::modules::posix::kwparse::<5>("sendmsg_afalg", &["msg", "op", "iv", "assoclen", "flags"], 0, 0, 4, args, kw)?;
    let s = sk(o);
    let op = match a[1] {
        Some(x) => {
            let v = ab::index(x)?;
            match int::as_i64(&v) {
                Some(n) if n >= 0 && n <= u32::MAX as i64 => n as u32,
                _ => raise!(OverflowError, "Python int too large to convert to C unsigned int"),
            }
        }
        None => raise!(TypeError, "Invalid or missing argument 'op'"),
    };
    let iv = match a[2].filter(|x| !none::is_none(x)) {
        Some(x) => Some(BufferView::get(x, PyBUF_SIMPLE)?.to_vec()),
        None => None,
    };
    let assoclen = match a[3].filter(|x| !none::is_none(x)) {
        Some(x) => {
            let v = int_arg(x)?;
            if v < 0 {
                raise!(TypeError, "assoclen must be positive");
            }
            Some(v as u32)
        }
        None => None,
    };
    let flags = flags_arg(a[4])?;
    let (_views, mut iov) = match a[0] {
        Some(m) => sendmsg_iovec(m, "sendmsg_afalg")?,
        None => (Vec::new(), Vec::new()),
    };
    let mut items: Vec<(c_int, c_int, Vec<u8>)> = vec![(279, 3, op.to_ne_bytes().to_vec())];
    if let Some(iv) = iv {
        let mut d = (iv.len() as u32).to_ne_bytes().to_vec();
        d.extend_from_slice(&iv);
        items.push((279, 2, d));
    }
    if let Some(al) = assoclen {
        items.push((279, 4, al.to_ne_bytes().to_vec()));
    }
    let mut ctrl = build_control(&items)?;
    let clen = control_len(&items);
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = iov.as_mut_ptr();
    msg.msg_iovlen = iov.len();
    msg.msg_control = ctrl.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = clen;
    let mp = &msg as *const libc::msghdr as usize;
    let n = sock_call(s, true, &mut |fd| unsafe { libc::sendmsg(fd, mp as *const libc::msghdr, flags) })?;
    Ok(int::new_int(n as i64))
}

// ------------------------------------------------------------------ module functions

fn f_getdefaulttimeout(_m: &PyObject) -> PyResult {
    Ok(timeout_obj(DEFAULT_TIMEOUT.load(Ordering::Relaxed)))
}

fn f_setdefaulttimeout(_m: &PyObject, v: &PyObject) -> PyResult {
    let t = parse_timeout(v)?;
    DEFAULT_TIMEOUT.store(t, Ordering::Relaxed);
    Ok(none::none_ref())
}

fn f_close(_m: &PyObject, fdo: &PyObject) -> PyResult {
    let fd = int_arg(fdo)?;
    let (r, e) = crate::threads::blocking(|| {
        let r = unsafe { libc::close(fd) };
        (r, if r < 0 { errno() } else { 0 })
    });
    if r < 0 && e != libc::ECONNRESET {
        return Err(os_error(e));
    }
    Ok(none::none_ref())
}

fn f_dup(_m: &PyObject, fdo: &PyObject) -> PyResult {
    let fd = int_arg(fdo)?;
    let r = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if r < 0 {
        return Err(os_error(errno()));
    }
    Ok(int::new_int(r as i64))
}

fn f_socketpair(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("socketpair", args, 0, 3)?;
    let family = match args.first() { Some(o) => int_arg(o)?, None => libc::AF_UNIX };
    let ty = match args.get(1) { Some(o) => int_arg(o)?, None => libc::SOCK_STREAM };
    let proto = match args.get(2) { Some(o) => int_arg(o)?, None => 0 };
    let mut fds = [0 as c_int; 2];
    let r = unsafe { libc::socketpair(family, ty | libc::SOCK_CLOEXEC, proto, fds.as_mut_ptr()) };
    if r < 0 {
        return Err(os_error(errno()));
    }
    let a = match new_sockobject(fds[0], family, ty, proto) {
        Ok(a) => a,
        Err(e) => {
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            return Err(e);
        }
    };
    let b = match new_sockobject(fds[1], family, ty, proto) {
        Ok(b) => b,
        Err(e) => {
            unsafe { libc::close(fds[1]) };
            return Err(e);
        }
    };
    Ok(tuple::new_tuple(vec![a, b]))
}

fn u_arg(o: &PyObject, fname: &str, bits: u32) -> PyResult<u64> {
    if !int::is_int(o) {
        raise!(TypeError, "expected int, {} found", tname(o));
    }
    if int::sign(o) < 0 {
        raise!(OverflowError, "{}: can't convert negative Python int to C {}-bit unsigned integer", fname, bits);
    }
    match int::as_i128(o) {
        Some(v) if v <= ((1u128 << bits) - 1) as i128 => Ok(v as u64),
        _ => raise!(OverflowError, "{}: Python int too large to convert to C {}-bit unsigned integer", fname, bits),
    }
}

fn f_ntohs(_m: &PyObject, x: &PyObject) -> PyResult {
    let v = u_arg(x, "ntohs", 16)? as u16;
    Ok(int::new_int(u16::from_be(v) as i64))
}
fn f_htons(_m: &PyObject, x: &PyObject) -> PyResult {
    let v = u_arg(x, "htons", 16)? as u16;
    Ok(int::new_int(v.to_be() as i64))
}
fn f_ntohl(_m: &PyObject, x: &PyObject) -> PyResult {
    let v = u_arg(x, "ntohl", 32)? as u32;
    Ok(int::new_int(u32::from_be(v) as i64))
}
fn f_htonl(_m: &PyObject, x: &PyObject) -> PyResult {
    let v = u_arg(x, "htonl", 32)? as u32;
    Ok(int::new_int(v.to_be() as i64))
}

// ------------------------------------------------------------------ module init

static INIT: std::sync::Once = std::sync::Once::new();

fn new_exc(name: &str, base: &PyObject, d: &PyObject) -> PyResult {
    let ns = dict::new_dict();
    dict::set_item_str(&ns, "__module__", &pystr::new_str("socket"))?;
    let tt = type_obj(type_type());
    let bases = tuple::new_tuple(vec![base.to_ref()]);
    let t = ab::call(tt, &[&pystr::new_str(name), &bases, &ns])?;
    dict::set_item_str(d, name, &t)?;
    Ok(t)
}

pub fn init() -> PyResult {
    let mut res = Ok(());
    INIT.call_once(|| unsafe {
        let t = &mut *sock_type();
        t.tp_name = c"_socket.socket".as_ptr();
        t.tp_basicsize = std::mem::size_of::<SockObj>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE;
        t.tp_new = Some(sock_new);
        t.tp_init = Some(sock_init);
        t.tp_dealloc = Some(sock_dealloc);
        t.tp_finalize = Some(sock_finalize);
        t.tp_repr = Some(sock_repr);
        t.tp_alloc = Some(generic_alloc);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_doc = c"socket(family=AF_INET, type=SOCK_STREAM, proto=0) -> socket object\nsocket(family=-1, type=-1, proto=-1, fileno=None) -> socket object\n\nOpen a socket of the given type.".as_ptr();
        t.tp_methods = crate::mdefs![
            ("_accept", NOARGS, m_accept, "_accept() -> (integer, address info)"),
            ("bind", O, m_bind, "bind(address)"),
            ("close", NOARGS, m_close, "close()\n\nClose the socket.  It cannot be used after this call."),
            ("connect", O, m_connect, "connect(address)"),
            ("connect_ex", O, m_connect_ex, "connect_ex(address) -> errno"),
            ("detach", NOARGS, m_detach, "detach()\n\nClose the socket object without closing the underlying file descriptor."),
            ("fileno", NOARGS, m_fileno, "fileno() -> integer\n\nReturn the integer file descriptor of the socket."),
            ("getpeername", NOARGS, m_getpeername, "getpeername() -> address info"),
            ("getsockname", NOARGS, m_getsockname, "getsockname() -> address info"),
            ("getsockopt", FAST, m_getsockopt, "getsockopt(level, option[, buffersize]) -> value"),
            ("getblocking", NOARGS, m_getblocking, "getblocking()\n\nReturns True if socket is in blocking mode, or False if it\nis in non-blocking mode."),
            ("gettimeout", NOARGS, m_gettimeout, "gettimeout() -> timeout"),
            ("listen", FAST, m_listen, "listen([backlog])"),
            ("recv", FAST, m_recv, "recv(buffersize[, flags]) -> data"),
            ("recv_into", FASTKW, m_recv_into, "recv_into(buffer, [nbytes[, flags]]) -> nbytes_read"),
            ("recvfrom", FAST, m_recvfrom, "recvfrom(buffersize[, flags]) -> (data, address info)"),
            ("recvfrom_into", FASTKW, m_recvfrom_into, "recvfrom_into(buffer[, nbytes[, flags]]) -> (nbytes, address info)"),
            ("recvmsg", FAST, m_recvmsg, "recvmsg(bufsize[, ancbufsize[, flags]]) -> (data, ancdata, msg_flags, address)"),
            ("recvmsg_into", FAST, m_recvmsg_into, "recvmsg_into(buffers[, ancbufsize[, flags]]) -> (nbytes, ancdata, msg_flags, address)"),
            ("send", FAST, m_send, "send(data[, flags]) -> count"),
            ("sendall", FAST, m_sendall, "sendall(data[, flags])"),
            ("sendto", FAST, m_sendto, "sendto(data[, flags], address) -> count"),
            ("sendmsg", FAST, m_sendmsg, "sendmsg(buffers[, ancdata[, flags[, address]]]) -> count"),
            ("sendmsg_afalg", FASTKW, m_sendmsg_afalg, "sendmsg_afalg([msg], *, op[, iv[, assoclen[, flags=MSG_MORE]]])"),
            ("setblocking", O, m_setblocking, "setblocking(flag)"),
            ("settimeout", O, m_settimeout, "settimeout(timeout)"),
            ("setsockopt", FAST, m_setsockopt, "setsockopt(level, option, value: int)\nsetsockopt(level, option, value: buffer)\nsetsockopt(level, option, None, optlen: int)"),
            ("shutdown", O, m_shutdown, "shutdown(flag)"),
        ];
        t.tp_getset = crate::getset_table![
            ("family", get_family; "the socket family"),
            ("type", get_type; "the socket type"),
            ("proto", get_proto; "the socket protocol"),
            ("timeout", get_timeout; "the socket timeout"),
        ];
        res = type_ready(sock_type());
    });
    res?;
    let m = crate::types::module::new_module("_socket")?;
    let d = crate::types::module::module_dict(&m);
    dict::set_item_str(&d, "__doc__", &pystr::new_str("Implementation module for socket operations.\n\nSee the socket module for documentation."))?;
    let st = type_ref(sock_type());
    dict::set_item_str(&d, "socket", &st)?;
    dict::set_item_str(&d, "SocketType", &st)?;
    let oserr = type_ref(crate::types::exceptions::OSError());
    dict::set_item_str(&d, "error", &oserr)?;
    dict::set_item_str(&d, "timeout", &type_ref(crate::types::exceptions::TimeoutError()))?;
    let h = new_exc("herror", &oserr, &d)?;
    HERROR.store(h.into_raw(), Ordering::Release);
    let g = new_exc("gaierror", &oserr, &d)?;
    GAIERROR.store(g.into_raw(), Ordering::Release);
    dict::set_item_str(&d, "has_ipv6", &bool_::new_bool(true))?;
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![
            ("getdefaulttimeout", NOARGS, f_getdefaulttimeout, "getdefaulttimeout() -> timeout\n\nReturns the default timeout in seconds (float) for new socket objects.\nA value of None indicates that new socket objects have no timeout.\nWhen the socket module is first imported, the default is None."),
            ("setdefaulttimeout", O, f_setdefaulttimeout, "setdefaulttimeout(timeout)\n\nSet the default timeout in seconds (float) for new socket objects.\nA value of None indicates that new socket objects have no timeout.\nWhen the socket module is first imported, the default is None."),
            ("close", O, f_close, "close(integer) -> None\n\nClose an integer socket file descriptor."),
            ("dup", O, f_dup, "dup(integer) -> integer\n\nDuplicate an integer socket file descriptor."),
            ("socketpair", FAST, f_socketpair, "socketpair([family[, type [, proto]]]) -> (socket object, socket object)"),
            ("ntohs", O, f_ntohs, "ntohs($module, x, /)\n--\n\nConvert a 16-bit unsigned integer from network to host byte order."),
            ("htons", O, f_htons, "htons($module, x, /)\n--\n\nConvert a 16-bit unsigned integer from host to network byte order."),
            ("ntohl", O, f_ntohl, "ntohl($module, x, /)\n--\n\nConvert a 32-bit unsigned integer from network to host byte order."),
            ("htonl", O, f_htonl, "htonl($module, x, /)\n--\n\nConvert a 32-bit unsigned integer from host to network byte order."),
            ("CMSG_LEN", O, f_cmsg_len, "CMSG_LEN(length) -> control message length"),
            ("CMSG_SPACE", O, f_cmsg_space, "CMSG_SPACE(length) -> buffer size"),
        ],
        Some(&m),
    )?;
    netdb::add_functions(&d, &m)?;
    consts::add_constants(&d)?;
    Ok(m)
}
