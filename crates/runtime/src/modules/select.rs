//! The `select` module: select(), poll objects and epoll objects (Linux).
//! All waits retry on EINTR after running signal handlers (PEP 475).

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::{bool_, dict, int, list, none, tuple};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// PyObject_AsFileDescriptor.
pub fn as_fd(o: &PyObject) -> PyResult<i32> {
    let v = if int::is_int(o) {
        o.to_ref()
    } else {
        match ab::getattr_opt(o, &crate::types::str::new_str("fileno"))? {
            Some(f) => {
                let r = ab::call(&f, &[])?;
                if !int::is_int(&r) {
                    raise!(TypeError, "fileno() returned a non-integer");
                }
                r
            }
            None => raise!(TypeError, "argument must be an int, or have a fileno() method."),
        }
    };
    let n = match int::as_i64(&v) {
        Some(n) if n <= i32::MAX as i64 && n >= i32::MIN as i64 => n as i32,
        _ => raise!(OverflowError, "Python int too large to convert to C int"),
    };
    if n < 0 {
        raise!(ValueError, "file descriptor cannot be a negative integer ({})", n);
    }
    Ok(n)
}

/// Parse a timeout in seconds (None = block forever).
fn timeout_secs(o: Option<&PyObject>, what: &str) -> PyResult<Option<Duration>> {
    match o {
        None => Ok(None),
        Some(t) if t.is(none::none()) => Ok(None),
        Some(t) => {
            if !int::is_int(t) && !crate::types::float::is_float(t) && ab::lookup_special(t, crate::name!("__float__"))?.is_none() {
                raise!(TypeError, "{} must be an integer or None", what);
            }
            let v = ab::float_value(t)?;
            if v.is_nan() {
                raise!(ValueError, "Invalid value NaN (not a number)");
            }
            if v < 0.0 {
                raise!(ValueError, "{} must be non-negative", what);
            }
            if v > 1e15 {
                raise!(OverflowError, "timeout is too large");
            }
            Ok(Some(Duration::from_secs_f64(v)))
        }
    }
}

/// Run a blocking wait, retrying on EINTR with the remaining timeout.
fn retry_eintr<F: FnMut(Option<Duration>) -> i64>(timeout: Option<Duration>, mut f: F) -> PyResult<i64> {
    let deadline = timeout.map(|t| Instant::now() + t);
    let mut cur = timeout;
    loop {
        let r = crate::threads::blocking(|| f(cur));
        if r >= 0 {
            return Ok(r);
        }
        let e = errno();
        if e != libc::EINTR {
            return Err(crate::types::exceptions::os_error_from_errno(e, None, None));
        }
        crate::modules::signal::check_signals()?;
        if let Some(d) = deadline {
            cur = Some(d.saturating_duration_since(Instant::now()));
        }
    }
}

fn select_select(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("select", args, 3, 4)?;
    let timeout = timeout_secs(args.get(3).copied(), "timeout")?;
    let mut sets: Vec<Vec<(i32, PyRef)>> = Vec::new();
    for a in &args[..3] {
        let seq = crate::abstract_::collect_iterable(a).map_err(|e| {
            if exc_matches(crate::types::exceptions::TypeError()) {
                clear_exc();
                err!(TypeError, "arguments 1-3 must be sequences")
            } else {
                e
            }
        })?;
        let mut v = Vec::new();
        for o in seq {
            let fd = as_fd(&o)?;
            if fd >= libc::FD_SETSIZE as i32 {
                raise!(ValueError, "filedescriptor out of range in select()");
            }
            v.push((fd, o));
        }
        sets.push(v);
    }
    let mut result: [Vec<PyRef>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    let n = retry_eintr(timeout, |t| unsafe {
        let mut fs: [libc::fd_set; 3] = std::mem::zeroed();
        let mut maxfd = -1;
        for (k, s) in sets.iter().enumerate() {
            libc::FD_ZERO(&mut fs[k]);
            for (fd, _) in s {
                libc::FD_SET(*fd, &mut fs[k]);
                maxfd = maxfd.max(*fd);
            }
        }
        let mut tv;
        let tvp = match t {
            Some(d) => {
                tv = libc::timeval { tv_sec: d.as_secs() as libc::time_t, tv_usec: d.subsec_micros() as libc::suseconds_t };
                &mut tv as *mut libc::timeval
            }
            None => std::ptr::null_mut(),
        };
        let r = libc::select(maxfd + 1, &mut fs[0], &mut fs[1], &mut fs[2], tvp);
        if r > 0 {
            for (k, s) in sets.iter().enumerate() {
                for (fd, o) in s {
                    if libc::FD_ISSET(*fd, &fs[k]) {
                        result[k].push(o.clone());
                    }
                }
            }
        }
        r as i64
    })?;
    let _ = n;
    let [a, b, c] = result;
    Ok(tuple::new_tuple(vec![list::new_list(a), list::new_list(b), list::new_list(c)]))
}

// ------------------------------------------------------------------ poll

#[repr(C)]
struct PollObject {
    ob_base: PyObject,
    fds: Mutex<BTreeMap<i32, i16>>,
    poll_running: std::sync::atomic::AtomicBool,
}
static mut POLL_TYPE: PyTypeObject = PyTypeObject::ZERO;

fn po(o: &PyObject) -> &PollObject {
    unsafe { &*(o.as_ptr() as *const PollObject) }
}
unsafe extern "C" fn poll_dealloc(op: *mut PyObject) {
    unsafe {
        std::ptr::drop_in_place(&raw mut (*(op as *mut PollObject)).fds);
        crate::alloc::free_object(op);
    }
}
fn eventmask(o: Option<&PyObject>, default: i64) -> PyResult<i16> {
    match o {
        None => Ok(default as i16),
        Some(m) => {
            let v = ab::index(m)?;
            match int::as_i64(&v) {
                Some(x) if (0..=0xffff).contains(&x) => Ok(x as u16 as i16),
                Some(x) if x < 0 => raise!(OverflowError, "Python int too large for C unsigned short"),
                _ => raise!(OverflowError, "Python int too large for C unsigned short"),
            }
        }
    }
}
fn poll_register(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("register", args, 1, 2)?;
    let fd = as_fd(args[0])?;
    let m = eventmask(args.get(1).copied(), (libc::POLLIN | libc::POLLPRI | libc::POLLOUT) as i64)?;
    po(o).fds.lock().unwrap().insert(fd, m);
    Ok(none::none_ref())
}
fn poll_modify(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("modify", args, 2, 2)?;
    let fd = as_fd(args[0])?;
    let m = eventmask(Some(args[1]), 0)?;
    let mut g = po(o).fds.lock().unwrap();
    if !g.contains_key(&fd) {
        drop(g);
        return Err(crate::types::exceptions::os_error_from_errno(libc::ENOENT, None, None));
    }
    g.insert(fd, m);
    Ok(none::none_ref())
}
fn poll_unregister(o: &PyObject, f: &PyObject) -> PyResult {
    let fd = as_fd(f)?;
    if po(o).fds.lock().unwrap().remove(&fd).is_none() {
        return Err(set_err_obj(crate::types::exceptions::KeyError(), int::new_int(fd as i64)));
    }
    Ok(none::none_ref())
}
fn ms_timeout(t: Option<&PyObject>) -> PyResult<Option<Duration>> {
    match t {
        None => Ok(None),
        Some(t) if t.is(none::none()) => Ok(None),
        Some(t) => {
            let v = if int::is_int(t) {
                match int::as_i64(t) {
                    Some(v) => v as f64,
                    None => raise!(OverflowError, "Python int too large to convert to C int"),
                }
            } else if crate::types::float::is_float(t) {
                ab::float_value(t)?
            } else {
                match ab::lookup_special(t, crate::name!("__index__"))? {
                    Some(_) => ab::as_i64(t)? as f64,
                    None => raise!(TypeError, "timeout must be an integer or None"),
                }
            };
            if v < 0.0 {
                return Ok(None);
            }
            if v > i32::MAX as f64 {
                raise!(OverflowError, "Python int too large to convert to C int");
            }
            Ok(Some(Duration::from_secs_f64(v.ceil() / 1000.0)))
        }
    }
}
fn poll_poll(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("poll", args, 0, 1)?;
    let timeout = ms_timeout(args.first().copied())?;
    let p = po(o);
    if p.poll_running.swap(true, std::sync::atomic::Ordering::AcqRel) {
        raise!(RuntimeError, "concurrent poll() invocation");
    }
    let mut pfds: Vec<libc::pollfd> = p.fds.lock().unwrap().iter().map(|(&fd, &ev)| libc::pollfd { fd, events: ev, revents: 0 }).collect();
    let r = retry_eintr(timeout, |t| unsafe {
        let ms = match t {
            Some(d) => (d.as_micros().div_ceil(1000)).min(i32::MAX as u128) as i32,
            None => -1,
        };
        libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, ms) as i64
    });
    p.poll_running.store(false, std::sync::atomic::Ordering::Release);
    r?;
    let out: Vec<PyRef> = pfds
        .iter()
        .filter(|p| p.revents != 0)
        .map(|p| tuple::new_tuple(vec![int::new_int(p.fd as i64), int::new_int(p.revents as u16 as i64)]))
        .collect();
    Ok(list::new_list(out))
}
fn select_poll(_m: &PyObject) -> PyResult {
    unsafe {
        let op = crate::alloc::alloc_object(&raw mut POLL_TYPE, std::mem::size_of::<PollObject>());
        let p = op as *mut PollObject;
        std::ptr::write(&raw mut (*p).fds, Mutex::new(BTreeMap::new()));
        (*p).poll_running = std::sync::atomic::AtomicBool::new(false);
        Ok(PyRef::from_raw(op))
    }
}

// ------------------------------------------------------------------ epoll

#[repr(C)]
struct EpollObject {
    ob_base: PyObject,
    fd: std::sync::atomic::AtomicI32,
}
static mut EPOLL_TYPE: PyTypeObject = PyTypeObject::ZERO;

fn ep(o: &PyObject) -> &EpollObject {
    unsafe { &*(o.as_ptr() as *const EpollObject) }
}
fn ep_fd(o: &PyObject) -> PyResult<i32> {
    let fd = ep(o).fd.load(std::sync::atomic::Ordering::Acquire);
    if fd < 0 {
        raise!(ValueError, "I/O operation on closed epoll object");
    }
    Ok(fd)
}
unsafe extern "C" fn epoll_dealloc(op: *mut PyObject) {
    unsafe {
        let fd = (*(op as *mut EpollObject)).fd.swap(-1, std::sync::atomic::Ordering::AcqRel);
        if fd >= 0 {
            libc::close(fd);
        }
        crate::types::typeobject::generic_free_and_type_decref(op, (*op).type_ptr());
    }
}
fn new_epoll(tp: *mut PyTypeObject, fd: i32) -> PyResult {
    unsafe {
        let op = crate::alloc::alloc_object(tp, std::mem::size_of::<EpollObject>().max((*tp).tp_basicsize as usize));
        (*(op as *mut EpollObject)).fd = std::sync::atomic::AtomicI32::new(fd);
        Ok(PyRef::from_raw(op))
    }
}
unsafe extern "C" fn epoll_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let (v, kwn) = crate::args::kwdict_to_vectorcall(&*args, kw);
            let refs: Vec<&PyObject> = v.iter().map(|x| &**x).collect();
            static P: crate::args::Params = crate::args::Params::new("epoll", &["sizehint", "flags"], 0, 0);
            let [sh, fl] = crate::args::parse::<2>(&P, &refs, kwn.as_deref())?;
            if let Some(s) = sh {
                let n = ab::as_i64(s)?;
                if n == 0 || n < -1 {
                    raise!(ValueError, "negative sizehint");
                }
            }
            if let Some(f) = fl {
                let n = ab::as_i64(f)?;
                if n != 0 && n != libc::EPOLL_CLOEXEC as i64 {
                    raise!(OSError, "Invalid flags");
                }
            }
            let fd = libc::epoll_create1(libc::EPOLL_CLOEXEC);
            if fd < 0 {
                return Err(crate::types::exceptions::last_os_error());
            }
            new_epoll(tp, fd)
        })();
        into_c(r)
    }
}
fn epoll_close(o: &PyObject) -> PyResult {
    let fd = ep(o).fd.swap(-1, std::sync::atomic::Ordering::AcqRel);
    if fd >= 0 && unsafe { libc::close(fd) } != 0 {
        return Err(crate::types::exceptions::last_os_error());
    }
    Ok(none::none_ref())
}
fn epoll_closed(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(ep(o).fd.load(std::sync::atomic::Ordering::Acquire) < 0))
}
fn epoll_fileno(o: &PyObject) -> PyResult {
    Ok(int::new_int(ep_fd(o)? as i64))
}
fn epoll_fromfd(cls: &PyObject, f: &PyObject) -> PyResult {
    let fd = ab::as_i64(f)? as i32;
    new_epoll(crate::types::typeobject::as_type(cls), fd)
}
fn epoll_ctl(o: &PyObject, op: i32, fdo: &PyObject, mask: u32) -> PyResult {
    let epfd = ep_fd(o)?;
    let fd = as_fd(fdo)?;
    let mut ev = libc::epoll_event { events: mask, u64: fd as u64 };
    let r = unsafe { libc::epoll_ctl(epfd, op, fd, &mut ev) };
    if r < 0 {
        return Err(crate::types::exceptions::last_os_error());
    }
    Ok(none::none_ref())
}
fn epmask(o: Option<&PyObject>) -> PyResult<u32> {
    match o {
        None => Ok((libc::EPOLLIN | libc::EPOLLPRI | libc::EPOLLOUT) as u32),
        Some(m) => {
            let v = ab::index(m)?;
            match int::as_i64(&v) {
                Some(x) if (0..=u32::MAX as i64).contains(&x) => Ok(x as u32),
                Some(x) if x < 0 && x >= i32::MIN as i64 => Ok(x as i32 as u32),
                _ => raise!(OverflowError, "Python int too large to convert to C unsigned int"),
            }
        }
    }
}
fn epoll_register(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("register", &["fd", "eventmask"], 0, 1);
    let [f, m] = crate::args::parse::<2>(&P, args, kw)?;
    epoll_ctl(o, libc::EPOLL_CTL_ADD, f.unwrap(), epmask(m)?)
}
fn epoll_modify(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("modify", &["fd", "eventmask"], 0, 2);
    let [f, m] = crate::args::parse::<2>(&P, args, kw)?;
    epoll_ctl(o, libc::EPOLL_CTL_MOD, f.unwrap(), epmask(m)?)
}
fn epoll_unregister(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("unregister", &["fd"], 0, 1);
    let [f] = crate::args::parse::<1>(&P, args, kw)?;
    epoll_ctl(o, libc::EPOLL_CTL_DEL, f.unwrap(), 0)
}
fn epoll_poll(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("poll", &["timeout", "maxevents"], 0, 0);
    let [t, mx] = crate::args::parse::<2>(&P, args, kw)?;
    let epfd = ep_fd(o)?;
    let timeout = match t {
        Some(t) if !t.is(none::none()) => {
            let v = ab::float_value(t)?;
            if v < 0.0 { None } else { Some(Duration::from_secs_f64(v.min(1e9))) }
        }
        _ => None,
    };
    let maxevents = match mx {
        Some(m) => {
            let n = ab::as_i64(m)?;
            if n == -1 {
                libc::FD_SETSIZE as i64 - 1
            } else if n < 1 {
                raise!(ValueError, "maxevents must be greater than 0, got {}", n);
            } else {
                n
            }
        }
        None => libc::FD_SETSIZE as i64 - 1,
    } as usize;
    let mut evs: Vec<libc::epoll_event> = vec![libc::epoll_event { events: 0, u64: 0 }; maxevents];
    let n = retry_eintr(timeout, |t| unsafe {
        let ms = match t {
            Some(d) => (d.as_micros().div_ceil(1000)).min(i32::MAX as u128) as i32,
            None => -1,
        };
        libc::epoll_wait(epfd, evs.as_mut_ptr(), maxevents as i32, ms) as i64
    })? as usize;
    let out: Vec<PyRef> = evs[..n]
        .iter()
        .map(|e| {
            let ev = e.events;
            let data = e.u64;
            tuple::new_tuple(vec![int::new_int(data as i32 as i64), int::new_int(ev as i64)])
        })
        .collect();
    Ok(list::new_list(out))
}
fn epoll_enter(o: &PyObject) -> PyResult {
    ep_fd(o)?;
    Ok(o.to_ref())
}
fn epoll_exit(o: &PyObject, _args: &[&PyObject]) -> PyResult {
    epoll_close(o)
}

pub fn init_module() -> PyResult {
    unsafe {
        if POLL_TYPE.tp_flags & Py_TPFLAGS_READY == 0 {
            let t = &mut *(&raw mut POLL_TYPE);
            t.tp_name = c"select.poll".as_ptr();
            t.tp_basicsize = std::mem::size_of::<PollObject>() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_DISALLOW_INSTANTIATION;
            t.tp_dealloc = Some(poll_dealloc);
            t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
            t.tp_methods = crate::mdefs![
                ("register", FAST, poll_register),
                ("modify", FAST, poll_modify),
                ("unregister", O, poll_unregister),
                ("poll", FAST, poll_poll),
            ];
            crate::types::typeobject::type_ready(&raw mut POLL_TYPE)?;
            (*(&raw mut POLL_TYPE)).tp_new = None;

            let t = &mut *(&raw mut EPOLL_TYPE);
            t.tp_name = c"select.epoll".as_ptr();
            t.tp_basicsize = std::mem::size_of::<EpollObject>() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE;
            t.tp_new = Some(epoll_new);
            t.tp_dealloc = Some(epoll_dealloc);
            t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
            t.tp_doc = c"select.epoll(sizehint=-1, flags=0)\n\nReturns an epolling object.".as_ptr();
            t.tp_methods = crate::mdefs![
                ("close", NOARGS, epoll_close),
                ("fileno", NOARGS, epoll_fileno),
                ("fromfd", O, epoll_fromfd; METH_CLASS),
                ("register", FASTKW, epoll_register),
                ("modify", FASTKW, epoll_modify),
                ("unregister", FASTKW, epoll_unregister),
                ("poll", FASTKW, epoll_poll),
                ("__enter__", NOARGS, epoll_enter),
                ("__exit__", FAST, epoll_exit),
            ];
            t.tp_getset = crate::getset_table![("closed", epoll_closed)];
            crate::types::typeobject::type_ready(&raw mut EPOLL_TYPE)?;
        }
    }
    let m = crate::types::module::new_module("select")?;
    let d = crate::types::module::module_dict(&m);
    crate::builtins::add_methods_to_dict(&d, crate::mdefs![("select", FAST, select_select), ("poll", NOARGS, select_poll)], Some(&m))?;
    dict::set_item_str(&d, "error", crate::types::typeobject::type_obj(crate::types::exceptions::OSError()))?;
    dict::set_item_str(&d, "epoll", crate::types::typeobject::type_obj(&raw mut EPOLL_TYPE))?;
    let consts: &[(&str, i64)] = &[
        ("POLLIN", libc::POLLIN as i64),
        ("POLLPRI", libc::POLLPRI as i64),
        ("POLLOUT", libc::POLLOUT as i64),
        ("POLLERR", libc::POLLERR as i64),
        ("POLLHUP", libc::POLLHUP as i64),
        ("POLLNVAL", libc::POLLNVAL as i64),
        ("POLLRDNORM", libc::POLLRDNORM as i64),
        ("POLLRDBAND", libc::POLLRDBAND as i64),
        ("POLLWRNORM", libc::POLLWRNORM as i64),
        ("POLLWRBAND", libc::POLLWRBAND as i64),
        ("POLLMSG", 0x400),
        ("POLLRDHUP", libc::POLLRDHUP as i64),
        ("EPOLLIN", libc::EPOLLIN as i64),
        ("EPOLLOUT", libc::EPOLLOUT as i64),
        ("EPOLLPRI", libc::EPOLLPRI as i64),
        ("EPOLLERR", libc::EPOLLERR as i64),
        ("EPOLLHUP", libc::EPOLLHUP as i64),
        ("EPOLLET", libc::EPOLLET as u32 as i64),
        ("EPOLLONESHOT", libc::EPOLLONESHOT as i64),
        ("EPOLLEXCLUSIVE", libc::EPOLLEXCLUSIVE as i64),
        ("EPOLLRDHUP", libc::EPOLLRDHUP as i64),
        ("EPOLLRDNORM", libc::EPOLLRDNORM as i64),
        ("EPOLLRDBAND", libc::EPOLLRDBAND as i64),
        ("EPOLLWRNORM", libc::EPOLLWRNORM as i64),
        ("EPOLLWRBAND", libc::EPOLLWRBAND as i64),
        ("EPOLLMSG", libc::EPOLLMSG as i64),
        ("EPOLL_CLOEXEC", libc::EPOLL_CLOEXEC as i64),
        ("PIPE_BUF", libc::PIPE_BUF as i64),
    ];
    for (k, v) in consts {
        dict::set_item_str(&d, k, &int::new_int(*v))?;
    }
    Ok(m)
}
