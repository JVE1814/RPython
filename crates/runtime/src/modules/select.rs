//! `select`: select(), poll objects and epoll objects (Linux).

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::*;
use crate::types::{bool_, dict, float, int, list, none, str as pystr, tuple};
use std::ffi::c_int;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;

fn errno() -> i32 {
    unsafe { *libc::__errno_location() }
}
fn os_error(e: i32) -> PyErr {
    crate::types::exceptions::os_error_from_errno(e, None, None)
}
fn as_fd(o: &PyObject) -> PyResult<i32> {
    crate::modules::posix::path::as_file_descriptor(o)
}
fn mono_ns() -> i64 {
    crate::modules::_socket::mono_ns()
}

// ------------------------------------------------------------------ select()

/// Collect (fd, object) pairs from a sequence; re-reads the length each step.
fn seq2set(seq: &PyObject, set: &mut libc::fd_set, maxfd: &mut i32) -> PyResult<Vec<(i32, PyRef)>> {
    let fast: PyRef = if list::is_list(seq) || tuple::is_tuple(seq) {
        seq.to_ref()
    } else {
        match ab::sequence_list(seq) {
            Ok(l) => l,
            Err(e) => {
                if exc_matches(crate::types::exceptions::TypeError()) {
                    clear_exc();
                    raise!(TypeError, "arguments 1-3 must be sequences");
                }
                return Err(e);
            }
        }
    };
    let mut out = Vec::new();
    let mut i = 0;
    loop {
        let item = if list::is_list(&fast) {
            match list::get(&fast, i) {
                Some(x) => x,
                None => break,
            }
        } else {
            if i >= tuple::tuple_len(&fast) {
                break;
            }
            tuple::tuple_get(&fast, i).to_ref()
        };
        let fd = as_fd(&item)?;
        if fd >= libc::FD_SETSIZE as i32 {
            raise!(ValueError, "filedescriptor out of range in select()");
        }
        unsafe { libc::FD_SET(fd, set) };
        if fd > *maxfd {
            *maxfd = fd;
        }
        out.push((fd, item));
        i += 1;
    }
    Ok(out)
}

fn set2list(set: &libc::fd_set, items: &[(i32, PyRef)]) -> PyRef {
    let l = list::new_list_empty();
    for (fd, o) in items {
        if unsafe { libc::FD_ISSET(*fd, set) } {
            list::append(&l, o.clone());
        }
    }
    l
}

fn f_select(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("select", args, 3, 4)?;
    let timeout: i64 = match args.get(3) {
        None => -1,
        Some(t) if none::is_none(t) => -1,
        Some(t) => {
            let ns = match crate::modules::_socket::seconds_to_ns(t) {
                Ok(n) => n,
                Err(e) => {
                    if exc_matches(crate::types::exceptions::TypeError()) {
                        clear_exc();
                        raise!(TypeError, "timeout must be a float or None");
                    }
                    return Err(e);
                }
            };
            if ns < 0 {
                raise!(ValueError, "timeout must be non-negative");
            }
            ns
        }
    };
    let mut rset: libc::fd_set = unsafe { std::mem::zeroed() };
    let mut wset: libc::fd_set = unsafe { std::mem::zeroed() };
    let mut xset: libc::fd_set = unsafe { std::mem::zeroed() };
    let mut maxfd = -1;
    let r0 = seq2set(args[0], &mut rset, &mut maxfd)?;
    let w0 = seq2set(args[1], &mut wset, &mut maxfd)?;
    let x0 = seq2set(args[2], &mut xset, &mut maxfd)?;
    let deadline = if timeout >= 0 { mono_ns() + timeout } else { 0 };
    let mut remaining = timeout;
    loop {
        let (mut r, mut w, mut x) = (rset, wset, xset);
        let mut tv = libc::timeval { tv_sec: 0, tv_usec: 0 };
        let tvp: *mut libc::timeval = if remaining >= 0 {
            // round up to microseconds
            let us = (remaining + 999) / 1000;
            tv.tv_sec = (us / 1_000_000) as _;
            tv.tv_usec = (us % 1_000_000) as _;
            &mut tv
        } else {
            std::ptr::null_mut()
        };
        let (n, e) = crate::threads::blocking_pure(false, || { // (stability3)
            let n = unsafe { libc::select(maxfd + 1, &mut r, &mut w, &mut x, tvp) };
            (n, if n < 0 { errno() } else { 0 })
        });
        if n < 0 {
            if e == libc::EINTR {
                crate::modules::signal::check_signals()?;
                if timeout >= 0 {
                    remaining = (deadline - mono_ns()).max(0);
                }
                continue;
            }
            return Err(os_error(e));
        }
        if n == 0 {
            return Ok(tuple::new_tuple(vec![list::new_list_empty(), list::new_list_empty(), list::new_list_empty()]));
        }
        return Ok(tuple::new_tuple(vec![set2list(&r, &r0), set2list(&w, &w0), set2list(&x, &x0)]));
    }
}

// ------------------------------------------------------------------ poll objects

#[repr(C)]
pub struct PollObj {
    ob_base: PyObject,
    fds: *mut Mutex<Vec<(i32, i16)>>,
    running: AtomicBool,
}

static mut POLL_TYPE: PyTypeObject = PyTypeObject::ZERO;
fn poll_type() -> *mut PyTypeObject {
    &raw mut POLL_TYPE
}

fn pobj(o: &PyObject) -> &PollObj {
    unsafe { &*(o.as_ptr() as *const PollObj) }
}
fn pfds(o: &PyObject) -> std::sync::MutexGuard<'static, Vec<(i32, i16)>> {
    unsafe { (*pobj(o).fds).lock().unwrap_or_else(|e| e.into_inner()) }
}

/// unsigned short converter (eventmask).
fn ushort_arg(o: &PyObject) -> PyResult<i16> {
    let v = ab::index(o)?;
    if int::sign(&v) < 0 {
        raise!(ValueError, "value must be positive");
    }
    match int::as_i64(&v) {
        Some(x) if x <= u16::MAX as i64 => Ok(x as u16 as i16),
        _ => raise!(OverflowError, "Python int too large for C unsigned short"),
    }
}

fn poll_register(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("register", args, 1, 2)?;
    let fd = as_fd(args[0])?;
    let ev = match args.get(1) {
        Some(e) => ushort_arg(e)?,
        None => (libc::POLLIN | libc::POLLPRI | libc::POLLOUT) as i16,
    };
    let mut v = pfds(o);
    if let Some(e) = v.iter_mut().find(|e| e.0 == fd) {
        e.1 = ev;
    } else {
        v.push((fd, ev));
    }
    Ok(none::none_ref())
}

fn poll_modify(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("modify", args, 2, 2)?;
    let fd = as_fd(args[0])?;
    let ev = ushort_arg(args[1])?;
    let mut v = pfds(o);
    match v.iter_mut().find(|e| e.0 == fd) {
        Some(e) => e.1 = ev,
        None => return Err(os_error(libc::ENOENT)),
    }
    Ok(none::none_ref())
}

fn poll_unregister(o: &PyObject, a: &PyObject) -> PyResult {
    let fd = as_fd(a)?;
    let mut v = pfds(o);
    match v.iter().position(|e| e.0 == fd) {
        Some(i) => {
            v.remove(i);
        }
        None => {
            drop(v);
            return Err(set_err_obj(crate::types::exceptions::KeyError(), int::new_int(fd as i64)));
        }
    }
    Ok(none::none_ref())
}

/// poll() timeout in milliseconds: None/negative -> -1.
fn poll_ms_arg(o: Option<&PyObject>) -> PyResult<i32> {
    let o = match o {
        None => return Ok(-1),
        Some(x) if none::is_none(x) => return Ok(-1),
        Some(x) => x,
    };
    let ms: f64 = if int::is_int(o) {
        match int::as_i64(o) {
            Some(v) => v as f64,
            None => raise!(OverflowError, "timeout is too large"),
        }
    } else if float::is_float(o) {
        let f = float::float_value(o);
        if f.is_nan() {
            raise!(ValueError, "Invalid value NaN (not a number)");
        }
        if f >= 0.0 { f.ceil() } else { f.floor() }
    } else if ab::has_index(o) {
        let v = ab::index(o)?;
        match int::as_i64(&v) {
            Some(v) => v as f64,
            None => raise!(OverflowError, "timeout is too large"),
        }
    } else {
        raise!(TypeError, "timeout must be an integer or None");
    };
    if ms < 0.0 {
        return Ok(-1);
    }
    if ms > i32::MAX as f64 {
        raise!(OverflowError, "timeout is too large");
    }
    Ok(ms as i32)
}

fn poll_poll(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("poll", args, 0, 1)?;
    let mut ms = poll_ms_arg(args.first().copied())?;
    let p = pobj(o);
    if p.running.swap(true, Ordering::AcqRel) {
        raise!(RuntimeError, "concurrent poll() invocation");
    }
    struct Reset<'a>(&'a AtomicBool);
    impl Drop for Reset<'_> {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    let _reset = Reset(&p.running);
    let mut pf: Vec<libc::pollfd> = pfds(o).iter().map(|&(fd, ev)| libc::pollfd { fd, events: ev, revents: 0 }).collect();
    let deadline = if ms >= 0 { mono_ns() + ms as i64 * 1_000_000 } else { 0 };
    loop {
        let (n, e) = crate::threads::blocking_pure(false, || { // (stability3)
            let n = unsafe { libc::poll(pf.as_mut_ptr(), pf.len() as libc::nfds_t, ms) };
            (n, if n < 0 { errno() } else { 0 })
        });
        if n < 0 {
            if e == libc::EINTR {
                crate::modules::signal::check_signals()?;
                if ms >= 0 {
                    let rem = deadline - mono_ns();
                    ms = if rem <= 0 { 0 } else { ((rem + 999_999) / 1_000_000) as i32 };
                }
                continue;
            }
            return Err(os_error(e));
        }
        break;
    }
    let out = list::new_list_empty();
    for p in &pf {
        if p.revents != 0 {
            list::append(&out, tuple::new_tuple(vec![int::new_int(p.fd as i64), int::new_int(p.revents as u16 as i64)]));
        }
    }
    Ok(out)
}

unsafe extern "C" fn poll_dealloc(op: *mut PyObject) {
    unsafe {
        let p = &mut *(op as *mut PollObj);
        if !p.fds.is_null() {
            drop(Box::from_raw(p.fds));
            p.fds = std::ptr::null_mut();
        }
        generic_free_and_type_decref(op, (*op).type_ptr());
    }
}

fn f_poll(_m: &PyObject) -> PyResult {
    unsafe {
        let op = crate::alloc::alloc_object(poll_type(), std::mem::size_of::<PollObj>());
        if op.is_null() {
            return Err(memory_error());
        }
        let p = &mut *(op as *mut PollObj);
        p.fds = Box::into_raw(Box::new(Mutex::new(Vec::new())));
        std::ptr::write(&mut p.running, AtomicBool::new(false));
        Ok(PyRef::from_raw(op))
    }
}

// ------------------------------------------------------------------ epoll objects

#[repr(C)]
pub struct EpollObj {
    ob_base: PyObject,
    epfd: AtomicI32,
}

static mut EPOLL_TYPE: PyTypeObject = PyTypeObject::ZERO;
fn epoll_type() -> *mut PyTypeObject {
    &raw mut EPOLL_TYPE
}
fn ep(o: &PyObject) -> &EpollObj {
    unsafe { &*(o.as_ptr() as *const EpollObj) }
}

fn closed_err() -> PyErr {
    crate::err!(ValueError, "I/O operation on closed epoll object")
}
fn ep_fd(o: &PyObject) -> PyResult<i32> {
    let fd = ep(o).epfd.load(Ordering::Relaxed);
    if fd < 0 {
        return Err(closed_err());
    }
    Ok(fd)
}

fn new_epoll(tp: *mut PyTypeObject, fd: i32) -> PyResult {
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
        let op = alloc(tp, 0);
        if op.is_null() {
            if fd >= 0 {
                libc::close(fd);
            }
            return Err(PyErr);
        }
        (*(op as *mut EpollObj)).epfd = AtomicI32::new(fd);
        Ok(PyRef::from_raw(op))
    }
}

unsafe extern "C" fn epoll_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let (v, kwn) = crate::args::kwdict_to_vectorcall(&*args, kw);
            let refs: Vec<&PyObject> = v.iter().map(|x| &**x).collect();
            let a = crate::modules::posix::kwparse::<2>("epoll", &["sizehint", "flags"], 0, 0, 0, &refs, kwn.as_deref())?;
            let sizehint = match a[0] {
                Some(o) => crate::modules::posix::path::c_int(o)?,
                None => -1,
            };
            let flags = match a[1] {
                Some(o) => crate::modules::posix::path::c_int(o)?,
                None => 0,
            };
            if sizehint == -1 {
                // default
            } else if sizehint <= 0 {
                raise!(ValueError, "negative sizehint");
            }
            if flags != 0 && flags != libc::EPOLL_CLOEXEC {
                return Err(os_error(libc::EINVAL));
            }
            let fd = libc::epoll_create1(libc::EPOLL_CLOEXEC);
            if fd < 0 {
                return Err(os_error(errno()));
            }
            new_epoll(tp, fd)
        })())
    }
}

unsafe extern "C" fn epoll_dealloc(op: *mut PyObject) {
    unsafe {
        let fd = (*(op as *mut EpollObj)).epfd.swap(-1, Ordering::Relaxed);
        if fd >= 0 {
            libc::close(fd);
        }
        generic_free_and_type_decref(op, (*op).type_ptr());
    }
}

fn epoll_close(o: &PyObject) -> PyResult {
    let fd = ep(o).epfd.swap(-1, Ordering::Relaxed);
    if fd >= 0 {
        let r = crate::threads::blocking(|| unsafe { libc::close(fd) });
        if r < 0 {
            return Err(os_error(errno()));
        }
    }
    Ok(none::none_ref())
}

fn epoll_closed(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(ep(o).epfd.load(Ordering::Relaxed) < 0))
}

fn epoll_fileno(o: &PyObject) -> PyResult {
    Ok(int::new_int(ep_fd(o)? as i64))
}

fn epoll_fromfd(cls: &PyObject, a: &PyObject) -> PyResult {
    let fd = crate::modules::posix::path::c_int(a)?;
    new_epoll(cls.as_ptr() as *mut PyTypeObject, fd)
}

fn uint_mask(o: &PyObject) -> PyResult<u32> {
    let v = ab::index(o)?;
    match int::as_i128(&v) {
        Some(x) if x >= i32::MIN as i128 && x <= u32::MAX as i128 => Ok(x as u32),
        _ => raise!(OverflowError, "Python int too large to convert to C unsigned int"),
    }
}

fn epoll_ctl(o: &PyObject, op: c_int, fdo: &PyObject, mask: u32) -> PyResult {
    let epfd = ep_fd(o)?;
    let fd = as_fd(fdo)?;
    let mut ev = libc::epoll_event { events: mask, u64: fd as u32 as u64 };
    let r = crate::threads::blocking(|| unsafe { libc::epoll_ctl(epfd, op, fd, &mut ev) });
    if r < 0 {
        return Err(os_error(errno()));
    }
    Ok(none::none_ref())
}

fn epoll_register(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let a = crate::modules::posix::kwparse::<2>("register", &["fd", "eventmask"], 0, 1, 0, args, kw)?;
    let mask = match a[1] {
        Some(m) => uint_mask(m)?,
        None => (libc::EPOLLIN | libc::EPOLLPRI | libc::EPOLLOUT) as u32,
    };
    epoll_ctl(o, libc::EPOLL_CTL_ADD, a[0].unwrap(), mask)
}

fn epoll_modify(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let a = crate::modules::posix::kwparse::<2>("modify", &["fd", "eventmask"], 0, 2, 0, args, kw)?;
    let mask = uint_mask(a[1].unwrap())?;
    epoll_ctl(o, libc::EPOLL_CTL_MOD, a[0].unwrap(), mask)
}

fn epoll_unregister(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let a = crate::modules::posix::kwparse::<1>("unregister", &["fd"], 0, 1, 0, args, kw)?;
    epoll_ctl(o, libc::EPOLL_CTL_DEL, a[0].unwrap(), 0)
}

fn epoll_poll(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let a = crate::modules::posix::kwparse::<2>("poll", &["timeout", "maxevents"], 0, 0, 0, args, kw)?;
    let epfd = ep_fd(o)?;
    let mut timeout_ns: i64 = match a[0] {
        None => -1,
        Some(t) if none::is_none(t) => -1,
        Some(t) => match crate::modules::_socket::seconds_to_ns(t) {
            Ok(n) => n,
            Err(e) => {
                if exc_matches(crate::types::exceptions::TypeError()) {
                    clear_exc();
                    raise!(TypeError, "timeout must be an integer or None");
                }
                return Err(e);
            }
        },
    };
    if timeout_ns < 0 {
        timeout_ns = -1;
    } else if (timeout_ns + 999_999) / 1_000_000 > i32::MAX as i64 {
        raise!(OverflowError, "timeout is too large");
    }
    let mut maxevents = match a[1] {
        Some(m) => crate::modules::posix::path::c_int(m)?,
        None => -1,
    };
    if maxevents == -1 {
        maxevents = libc::FD_SETSIZE as i32 - 1;
    } else if maxevents < 1 {
        raise!(ValueError, "maxevents must be greater than 0, got {}", maxevents);
    }
    let mut evs: Vec<libc::epoll_event> = vec![libc::epoll_event { events: 0, u64: 0 }; maxevents as usize];
    let deadline = if timeout_ns >= 0 { mono_ns() + timeout_ns } else { 0 };
    let mut ms: c_int = if timeout_ns < 0 { -1 } else { ((timeout_ns + 999_999) / 1_000_000) as c_int };
    let n = loop {
        let (n, e) = crate::threads::blocking_pure(false, || { // (stability3)
            let n = unsafe { libc::epoll_wait(epfd, evs.as_mut_ptr(), maxevents, ms) };
            (n, if n < 0 { errno() } else { 0 })
        });
        if n < 0 {
            if e == libc::EINTR {
                crate::modules::signal::check_signals()?;
                if timeout_ns >= 0 {
                    let rem = deadline - mono_ns();
                    ms = if rem <= 0 { 0 } else { ((rem + 999_999) / 1_000_000) as c_int };
                }
                continue;
            }
            return Err(os_error(e));
        }
        break n;
    };
    let out = list::new_list_empty();
    for e in &evs[..n as usize] {
        let fd = e.u64 as u32 as i32;
        let evv = e.events;
        list::append(&out, tuple::new_tuple(vec![int::new_int(fd as i64), int::new_int(evv as i64)]));
    }
    Ok(out)
}

fn epoll_enter(o: &PyObject) -> PyResult {
    ep_fd(o)?;
    Ok(o.to_ref())
}

fn epoll_exit(o: &PyObject, _args: &[&PyObject]) -> PyResult {
    epoll_close(o)?;
    Ok(none::none_ref())
}

// ------------------------------------------------------------------ module

static INIT: std::sync::Once = std::sync::Once::new();

const CONSTANTS: &[(&str, i64)] = &[
    ("POLLIN", 0x1),
    ("POLLPRI", 0x2),
    ("POLLOUT", 0x4),
    ("POLLERR", 0x8),
    ("POLLHUP", 0x10),
    ("POLLNVAL", 0x20),
    ("POLLRDNORM", 0x40),
    ("POLLRDBAND", 0x80),
    ("POLLWRNORM", 0x100),
    ("POLLWRBAND", 0x200),
    ("POLLMSG", 0x400),
    ("POLLRDHUP", 0x2000),
    ("EPOLLIN", 0x1),
    ("EPOLLPRI", 0x2),
    ("EPOLLOUT", 0x4),
    ("EPOLLERR", 0x8),
    ("EPOLLHUP", 0x10),
    ("EPOLLRDNORM", 0x40),
    ("EPOLLRDBAND", 0x80),
    ("EPOLLWRNORM", 0x100),
    ("EPOLLWRBAND", 0x200),
    ("EPOLLMSG", 0x400),
    ("EPOLLRDHUP", 0x2000),
    ("EPOLLEXCLUSIVE", 1 << 28),
    ("EPOLLWAKEUP", 1 << 29),
    ("EPOLLONESHOT", 1 << 30),
    ("EPOLLET", 1 << 31),
    ("EPOLL_CLOEXEC", 0o2000000),
    ("PIPE_BUF", 4096),
];

pub fn init() -> PyResult {
    let mut res = Ok(());
    INIT.call_once(|| unsafe {
        let t = &mut *poll_type();
        t.tp_name = c"select.poll".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PollObj>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_DISALLOW_INSTANTIATION | Py_TPFLAGS_IMMUTABLETYPE;
        t.tp_dealloc = Some(poll_dealloc);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_methods = crate::mdefs![
            ("register", FAST, poll_register, "register($self, fd, eventmask=select.POLLIN | select.POLLPRI | select.POLLOUT, /)\n--\n\nRegister a file descriptor with the polling object."),
            ("modify", FAST, poll_modify, "modify($self, fd, eventmask, /)\n--\n\nModify an already registered file descriptor."),
            ("unregister", O, poll_unregister, "unregister($self, fd, /)\n--\n\nRemove a file descriptor being tracked by the polling object."),
            ("poll", FAST, poll_poll, "poll($self, timeout=None, /)\n--\n\nPolls the set of registered file descriptors."),
        ];
        res = type_ready(poll_type());
        if res.is_err() {
            return;
        }
        let t = &mut *epoll_type();
        t.tp_name = c"select.epoll".as_ptr();
        t.tp_basicsize = std::mem::size_of::<EpollObj>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE;
        t.tp_new = Some(epoll_new);
        t.tp_alloc = Some(generic_alloc);
        t.tp_dealloc = Some(epoll_dealloc);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_doc = c"epoll(sizehint=-1, flags=0)\n--\n\nAn edge- and level-triggered I/O event notification object.\n\nsizehint is only a hint for old kernels (positive, or -1 for the default);\nflags is ignored but must be 0 or EPOLL_CLOEXEC.".as_ptr();
        t.tp_methods = crate::mdefs![
            ("close", NOARGS, epoll_close, "close($self, /)\n--\n\nClose the epoll control file descriptor.\n\nFurther operations on the epoll object will raise an exception."),
            ("fileno", NOARGS, epoll_fileno, "fileno($self, /)\n--\n\nReturn the epoll control file descriptor."),
            ("fromfd", O, epoll_fromfd, "fromfd($type, fd, /)\n--\n\nCreate an epoll object from a given control fd."; METH_CLASS),
            ("register", FASTKW, epoll_register, "register($self, /, fd, eventmask=select.EPOLLIN | select.EPOLLPRI | select.EPOLLOUT)\n--\n\nRegisters a new fd or raises an OSError if the fd is already registered."),
            ("modify", FASTKW, epoll_modify, "modify($self, /, fd, eventmask)\n--\n\nModify event mask for a registered file descriptor."),
            ("unregister", FASTKW, epoll_unregister, "unregister($self, /, fd)\n--\n\nRemove a registered file descriptor from the epoll object."),
            ("poll", FASTKW, epoll_poll, "poll($self, /, timeout=None, maxevents=-1)\n--\n\nWait for events on the epoll file descriptor."),
            ("__enter__", NOARGS, epoll_enter),
            ("__exit__", FAST, epoll_exit, "__exit__($self, exc_type=None, exc_value=None, exc_tb=None, /)\n--\n\nClose the epoll object when leaving a with block."),
        ];
        t.tp_getset = crate::getset_table![("closed", epoll_closed; "True if the epoll handler is closed")];
        res = type_ready(epoll_type());
    });
    res?;
    let m = crate::types::module::new_module("select")?;
    let d = crate::types::module::module_dict(&m);
    dict::set_item_str(&d, "__doc__", &pystr::new_str("This module supports asynchronous I/O on multiple file descriptors.\n\n*** IMPORTANT NOTICE ***\nOn Windows, only sockets are supported; on Unix, all file descriptors."))?;
    dict::set_item_str(&d, "error", &type_ref(crate::types::exceptions::OSError()))?;
    dict::set_item_str(&d, "epoll", &type_ref(epoll_type()))?;
    for (n, v) in CONSTANTS {
        dict::set_item_str(&d, n, &int::new_int(*v))?;
    }
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![
            ("select", FAST, f_select, "select($module, rlist, wlist, xlist, timeout=None, /)\n--\n\nWait until one or more file descriptors are ready for some kind of I/O.\n\nThe first three arguments are iterables of file descriptors to be waited for:\nrlist -- wait until ready for reading\nwlist -- wait until ready for writing\nxlist -- wait for an \"exceptional condition\"\nIf only one kind of condition is required, pass [] for the other lists.\n\nA file descriptor is either a socket or file object, or a small integer\ngotten from a fileno() method call on one of those.\n\nThe optional 4th argument specifies a timeout in seconds; it may be\na floating-point number to specify fractions of seconds.  If it is absent\nor None, the call will never time out.\n\nThe return value is a tuple of three lists corresponding to the first three\narguments; each contains the subset of the corresponding file descriptors\nthat are ready."),
            ("poll", NOARGS, f_poll, "poll($module, /)\n--\n\nReturns a polling object.\n\nThis object supports registering and unregistering file descriptors, and then\npolling them for I/O events."),
        ],
        Some(&m),
    )?;
    Ok(m)
}

/// Entry point used by the import registration in sys_full.
pub fn init_module() -> PyResult {
    init()
}
