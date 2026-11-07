//! The `_signal` module and the runtime's signal machinery. The C-level handler only
//! records the signal (async-signal-safe); Python handlers run on the main thread from
//! the eval breaker (`run_pending_handlers`) or from blocking calls (`check_signals`).

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::threads::ThreadState;
use crate::types::{bool_, dict, float, int, none, set, str as pystr, tuple};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, Ordering::*};

pub const NSIG: usize = 65;

#[allow(clippy::declare_interior_mutable_const)]
const AB: AtomicBool = AtomicBool::new(false);
#[allow(clippy::declare_interior_mutable_const)]
const AP: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
static TRIPPED: [AtomicBool; NSIG] = [AB; NSIG];
static IS_TRIPPED: AtomicBool = AtomicBool::new(false);
/// Python-level handler per signal (owned; NULL = None/unknown).
static HANDLERS: [AtomicPtr<PyObject>; NSIG] = [AP; NSIG];
static WAKEUP_FD: AtomicI32 = AtomicI32::new(-1);
static WAKEUP_WARN: AtomicBool = AtomicBool::new(true);
static WAKEUP_ERRNO: AtomicI32 = AtomicI32::new(0);
static DEFAULT_INT_HANDLER: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
static ITIMER_ERROR: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
static SIGINFO_TYPE: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// Record that `sig` arrived (async-signal-safe).
fn trip(sig: i32) {
    if sig <= 0 || sig as usize >= NSIG {
        return;
    }
    TRIPPED[sig as usize].store(true, Release);
    IS_TRIPPED.store(true, Release);
    let p = crate::threads::MAIN_THREAD_STATE.load(Acquire);
    if !p.is_null() {
        unsafe { (*p).c.eval_breaker.fetch_or(crate::threads::EVAL_SIGNALS, Release) };
    }
    let w = crate::modules::_thread::MAIN_WAIT_ADDR.load(Acquire);
    if !w.is_null() {
        crate::modules::_thread::futex_wake(w, i32::MAX);
    }
}

extern "C" fn c_handler(sig: libc::c_int) {
    let saved = unsafe { *libc::__errno_location() };
    trip(sig);
    let fd = WAKEUP_FD.load(Relaxed);
    if fd >= 0 {
        let b = sig as u8;
        let r = unsafe { libc::write(fd, &b as *const u8 as *const libc::c_void, 1) };
        if r < 0 {
            let e = unsafe { *libc::__errno_location() };
            if e != libc::EAGAIN || WAKEUP_WARN.load(Relaxed) {
                WAKEUP_ERRNO.store(e, Relaxed);
                IS_TRIPPED.store(true, Release);
            }
        }
    }
    unsafe { *libc::__errno_location() = saved };
}

pub fn signals_pending() -> bool {
    IS_TRIPPED.load(Acquire)
}

/// PyErr_CheckSignals: run pending handlers if on the main thread.
pub fn check_signals() -> PyResult<()> {
    if !IS_TRIPPED.load(Acquire) || !crate::threads::is_main_thread() {
        return Ok(());
    }
    run_handlers()
}

/// Called from the eval breaker.
pub fn run_pending_handlers(ts: &ThreadState) -> PyResult<()> {
    ts.c.eval_breaker.fetch_and(!crate::threads::EVAL_SIGNALS, AcqRel);
    if !crate::threads::is_main_thread() {
        return Ok(());
    }
    if !IS_TRIPPED.load(Acquire) {
        return Ok(());
    }
    run_handlers()
}

fn run_handlers() -> PyResult<()> {
    if !IS_TRIPPED.swap(false, AcqRel) {
        return Ok(());
    }
    let we = WAKEUP_ERRNO.swap(0, Relaxed);
    if we != 0 {
        let _ = crate::types::exceptions::os_error_from_errno(we, None, None);
        crate::modules::sys_full::write_unraisable(Some("Exception ignored while trying to write to the signal wakeup fd"), None);
    }
    let frame = || -> PyRef {
        let f = crate::interp::current_frame_ptr();
        if f.is_null() {
            none::none_ref()
        } else {
            unsafe { PyRef::from_borrowed(crate::interp::eval::frame_object(&mut *f)) }
        }
    };
    for sig in 1..NSIG {
        if !TRIPPED[sig].swap(false, AcqRel) {
            continue;
        }
        let h = HANDLERS[sig].load(Acquire);
        // (hang1) once the interpreter is finalizing, Python-level handlers
        // are gone (as after CPython's signal teardown): a signal that arrives
        // then (e.g. _thread.interrupt_main() from a __del__ run by module
        // teardown) is reported instead of raising inside the teardown
        let gone = h.is_null() || crate::modules::sys_full::is_finalizing();
        if gone || int::is_int(unsafe { &*h }) || !ab::is_callable(unsafe { &*h }) {
            if gone {
                let _ = crate::err!(OSError, "Signal {} ignored due to race condition", sig);
                crate::modules::sys_full::write_unraisable(None, Some(none::none()));
            }
            continue;
        }
        let h = unsafe { PyRef::from_borrowed(h) };
        let fr = frame();
        // Handlers run between two instructions of whatever frame is current,
        // possibly one right at the recursion limit: give the call some headroom
        // so that the signal is not consumed by a RecursionError instead.
        let ts = crate::threads::current();
        let headroom = !ts.recursion_headroom_active();
        if headroom {
            ts.set_recursion_headroom(true);
        }
        let r = ab::call(&h, &[&int::new_int(sig as i64), &fr]);
        if headroom {
            ts.set_recursion_headroom(false);
        }
        if r.is_err() {
            IS_TRIPPED.store(true, Release);
            if let Some(m) = crate::threads::main_thread_state() {
                m.c.eval_breaker.fetch_or(crate::threads::EVAL_SIGNALS, Release);
            }
            return Err(PyErr);
        }
    }
    Ok(())
}

/// PyErr_SetInterruptEx.
pub fn set_interrupt(signum: i32) -> PyResult<()> {
    if signum < 1 || signum as usize >= NSIG {
        raise!(ValueError, "signal number out of range");
    }
    let h = HANDLERS[signum as usize].load(Acquire);
    if h.is_null() {
        return Ok(());
    }
    let h = unsafe { &*h };
    if int::is_int(h) {
        return Ok(()); // SIG_IGN / SIG_DFL
    }
    if crate::modules::sys_full::is_finalizing() {
        // the Python-level handlers are going away with the interpreter: CPython finds
        // the handler gone when it would run it and reports that
        let _ = crate::err!(OSError, "Signal {} ignored due to race condition", signum);
        crate::modules::sys_full::write_unraisable(None, None);
        return Ok(());
    }
    trip(signum);
    Ok(())
}

pub fn after_fork_child() {
    for t in TRIPPED.iter() {
        t.store(false, Relaxed);
    }
    IS_TRIPPED.store(false, Relaxed);
}

fn get_handler_obj(sig: usize) -> PyRef {
    let h = HANDLERS[sig].load(Acquire);
    if h.is_null() { none::none_ref() } else { unsafe { PyRef::from_borrowed(h) } }
}
fn set_handler_obj(sig: usize, v: Option<PyRef>) {
    let nv = v.map_or(std::ptr::null_mut(), |v| v.into_raw());
    let old = HANDLERS[sig].swap(nv, AcqRel);
    unsafe { xdecref(old) };
}

fn os_sigaction(sig: i32, handler: libc::sighandler_t) -> Result<(), i32> {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = handler;
        libc::sigemptyset(&mut sa.sa_mask);
        sa.sa_flags = libc::SA_ONSTACK;
        if libc::sigaction(sig, &sa, std::ptr::null_mut()) != 0 {
            return Err(errno());
        }
        Ok(())
    }
}

/// Initialise signal handling at startup (main thread).
pub fn init_signals() {
    unsafe {
        for sig in 1..NSIG as i32 {
            let mut old: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(sig, std::ptr::null(), &mut old) != 0 {
                continue;
            }
            let v = if old.sa_sigaction == libc::SIG_DFL {
                Some(int::new_int(0))
            } else if old.sa_sigaction == libc::SIG_IGN {
                Some(int::new_int(1))
            } else {
                None
            };
            set_handler_obj(sig as usize, v);
        }
        for sig in [libc::SIGPIPE, libc::SIGXFSZ] {
            libc::signal(sig, libc::SIG_IGN);
            set_handler_obj(sig as usize, Some(int::new_int(1)));
        }
        // SIGINT -> default_int_handler (unless it is ignored)
        let cur = get_handler_obj(libc::SIGINT as usize);
        if int::is_int(&cur) && int::as_i64(&cur) == Some(0) {
            if let Ok(h) = default_int_handler_obj() {
                if os_sigaction(libc::SIGINT, c_handler as *const () as libc::sighandler_t).is_ok() {
                    set_handler_obj(libc::SIGINT as usize, Some(h));
                }
            }
        }
    }
}

fn default_int_handler(_m: &PyObject, _args: &[&PyObject]) -> PyResult {
    Err(set_err_noargs(crate::types::exceptions::KeyboardInterrupt()))
}

fn default_int_handler_obj() -> PyResult {
    let p = DEFAULT_INT_HANDLER.load(Acquire);
    if !p.is_null() {
        return Ok(unsafe { PyRef::from_borrowed(p) });
    }
    let defs = crate::mdefs![(
        "default_int_handler",
        FAST,
        default_int_handler,
        "default_int_handler($module, signalnum, frame, /)\n--\n\nThe default handler for SIGINT installed by Python.\n\nIt raises KeyboardInterrupt."
    )];
    let f = crate::types::cfunction::new_cfunction(defs, None, Some(pystr::new_str("_signal")));
    match DEFAULT_INT_HANDLER.compare_exchange(std::ptr::null_mut(), f.clone().into_raw(), AcqRel, Acquire) {
        Ok(_) => Ok(f),
        Err(o) => Ok(unsafe { PyRef::from_borrowed(o) }),
    }
}

fn signum_arg(o: &PyObject) -> PyResult<i32> {
    let n = ab::index(o)?;
    match int::as_i64(&n) {
        Some(v) if v >= 1 && (v as usize) < NSIG => Ok(v as i32),
        Some(v) if v > i32::MIN as i64 && v < i32::MAX as i64 => raise!(ValueError, "signal number out of range"),
        _ => raise!(OverflowError, "Python int too large to convert to C int"),
    }
}

fn require_main(what: &str) -> PyResult<()> {
    if !crate::threads::is_main_thread() {
        raise!(ValueError, "{} only works in main thread of the main interpreter", what);
    }
    Ok(())
}

fn sig_signal(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("signal", args, 2, 2)?;
    let sig = signum_arg(args[0])?;
    let handler = args[1];
    require_main("signal")?;
    let is_int_handler = int::is_int(handler);
    let func: libc::sighandler_t = if is_int_handler {
        match int::as_i64(handler) {
            Some(1) => libc::SIG_IGN,
            Some(0) => libc::SIG_DFL,
            _ => raise!(TypeError, "signal handler must be signal.SIG_IGN, signal.SIG_DFL, or a callable object"),
        }
    } else if ab::is_callable(handler) {
        c_handler as *const () as libc::sighandler_t
    } else {
        raise!(TypeError, "signal handler must be signal.SIG_IGN, signal.SIG_DFL, or a callable object");
    };
    check_signals()?;
    if let Err(e) = os_sigaction(sig, func) {
        return Err(crate::types::exceptions::os_error_from_errno(e, None, None));
    }
    let old = get_handler_obj(sig as usize);
    let stored = if is_int_handler { int::new_int(int::as_i64(handler).unwrap()) } else { handler.to_ref() };
    set_handler_obj(sig as usize, Some(stored));
    Ok(old)
}

fn sig_getsignal(_m: &PyObject, s: &PyObject) -> PyResult {
    let sig = signum_arg(s)?;
    Ok(get_handler_obj(sig as usize))
}

fn sig_strsignal(_m: &PyObject, s: &PyObject) -> PyResult {
    let sig = signum_arg(s)?;
    unsafe {
        let p = libc::strsignal(sig);
        if p.is_null() {
            return Ok(none::none_ref());
        }
        let st = std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned();
        if st.contains("Unknown signal") {
            return Ok(none::none_ref());
        }
        Ok(pystr::new_str(&st))
    }
}

fn sigset_from(o: &PyObject) -> PyResult<libc::sigset_t> {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        let it = ab::get_iter(o)?;
        while let Some(x) = ab::iter_next(&it)? {
            let n = ab::index(&x)?;
            let v = int::as_i64(&n).unwrap_or(i64::MAX);
            if v < 1 || v as usize >= NSIG {
                raise!(ValueError, "signal number {} out of range [1; {}]", v, NSIG - 1);
            }
            libc::sigaddset(&mut set, v as i32);
        }
        Ok(set)
    }
}
fn sigset_to_pyset(s: &libc::sigset_t) -> PyResult {
    let mut v = Vec::new();
    for sig in 1..NSIG as i32 {
        if unsafe { libc::sigismember(s, sig) } == 1 {
            v.push(int::new_int(sig as i64));
        }
    }
    set::new_set_from_vec(v)
}

fn sig_pthread_sigmask(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("pthread_sigmask", args, 2, 2)?;
    let how = ab::as_i64(args[0])? as i32;
    let mask = sigset_from(args[1])?;
    unsafe {
        let mut old: libc::sigset_t = std::mem::zeroed();
        let r = libc::pthread_sigmask(how, &mask, &mut old);
        if r != 0 {
            return Err(crate::types::exceptions::os_error_from_errno(r, None, None));
        }
        check_signals()?;
        sigset_to_pyset(&old)
    }
}
fn sig_sigpending(_m: &PyObject) -> PyResult {
    unsafe {
        let mut s: libc::sigset_t = std::mem::zeroed();
        if libc::sigpending(&mut s) != 0 {
            return Err(crate::types::exceptions::last_os_error());
        }
        sigset_to_pyset(&s)
    }
}
fn sig_sigwait(_m: &PyObject, s: &PyObject) -> PyResult {
    let set = sigset_from(s)?;
    let mut sig: libc::c_int = 0;
    let r = crate::threads::blocking(|| unsafe { libc::sigwait(&set, &mut sig) });
    if r != 0 {
        return Err(crate::types::exceptions::os_error_from_errno(r, None, None));
    }
    Ok(int::new_int(sig as i64))
}
fn siginfo_obj(si: &libc::siginfo_t) -> PyResult {
    let tp = SIGINFO_TYPE.load(Acquire);
    let i = |v: i64| int::new_int(v);
    unsafe {
        Ok(crate::modules::sys_structseq::new_instance(
            tp,
            vec![
                i(si.si_signo as i64),
                i(si.si_code as i64),
                i(si.si_errno as i64),
                i(si.si_pid() as i64),
                i(si.si_uid() as i64),
                i(si.si_status() as i64),
                i(0),
            ],
        ))
    }
}
fn sig_sigwaitinfo(_m: &PyObject, s: &PyObject) -> PyResult {
    let set = sigset_from(s)?;
    loop {
        let mut si: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let r = crate::threads::blocking(|| unsafe { libc::sigwaitinfo(&set, &mut si) });
        if r < 0 {
            let e = errno();
            if e == libc::EINTR {
                check_signals()?;
                continue;
            }
            return Err(crate::types::exceptions::os_error_from_errno(e, None, None));
        }
        return siginfo_obj(&si);
    }
}
fn sig_sigtimedwait(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("sigtimedwait", args, 2, 2)?;
    let set = sigset_from(args[0])?;
    let t = ab::float_value(args[1])?;
    if t < 0.0 {
        raise!(ValueError, "timeout must be non-negative");
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(t);
    loop {
        let rem = deadline.saturating_duration_since(std::time::Instant::now());
        let ts = libc::timespec { tv_sec: rem.as_secs() as libc::time_t, tv_nsec: rem.subsec_nanos() as libc::c_long };
        let mut si: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let r = crate::threads::blocking(|| unsafe { libc::sigtimedwait(&set, &mut si, &ts) });
        if r < 0 {
            let e = errno();
            if e == libc::EAGAIN {
                return Ok(none::none_ref());
            }
            if e == libc::EINTR {
                check_signals()?;
                continue;
            }
            return Err(crate::types::exceptions::os_error_from_errno(e, None, None));
        }
        return siginfo_obj(&si);
    }
}
fn sig_valid_signals(_m: &PyObject) -> PyResult {
    unsafe {
        let mut s: libc::sigset_t = std::mem::zeroed();
        libc::sigfillset(&mut s);
        sigset_to_pyset(&s)
    }
}
fn sig_raise_signal(_m: &PyObject, s: &PyObject) -> PyResult {
    let sig = signum_arg(s)?;
    if unsafe { libc::raise(sig) } != 0 {
        return Err(crate::types::exceptions::last_os_error());
    }
    check_signals()?;
    Ok(none::none_ref())
}
fn sig_pthread_kill(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("pthread_kill", args, 2, 2)?;
    let tid = ab::as_i128_index(args[0])?;
    if tid < 0 || tid > u64::MAX as i128 {
        raise!(OverflowError, "thread id out of range");
    }
    let n = ab::index(args[1])?;
    let sig = int::as_i64(&n).unwrap_or(-1) as i32;
    crate::modules::sys_full::audit("signal.pthread_kill", &[args[0], args[1]])?;
    let r = unsafe { libc::pthread_kill(tid as u64 as libc::pthread_t, sig) };
    if r != 0 {
        return Err(crate::types::exceptions::os_error_from_errno(r, None, None));
    }
    check_signals()?;
    Ok(none::none_ref())
}
fn sig_alarm(_m: &PyObject, s: &PyObject) -> PyResult {
    let n = ab::as_i64(s)?;
    Ok(int::new_int(unsafe { libc::alarm(n as u32) } as i64))
}
fn sig_pause(_m: &PyObject) -> PyResult {
    crate::threads::blocking(|| unsafe { libc::pause() });
    check_signals()?;
    Ok(none::none_ref())
}
fn tv_from(x: f64) -> libc::timeval {
    let secs = x.trunc();
    let mut usec = ((x - secs) * 1e6).round() as i64;
    let mut s = secs as i64;
    if usec >= 1_000_000 {
        s += 1;
        usec -= 1_000_000;
    }
    // round tiny positive values up so the timer is not disabled
    if s == 0 && usec == 0 && x > 0.0 {
        usec = 1;
    }
    libc::timeval { tv_sec: s as libc::time_t, tv_usec: usec as libc::suseconds_t }
}
fn tv_to(t: &libc::timeval) -> f64 {
    t.tv_sec as f64 + t.tv_usec as f64 * 1e-6
}
fn itimer_err(e: i32) -> PyErr {
    let msg = unsafe { std::ffi::CStr::from_ptr(libc::strerror(e)).to_string_lossy().into_owned() };
    let tp = ITIMER_ERROR.load(Acquire);
    match ab::call(unsafe { &*tp }, &[&int::new_int(e as i64), &pystr::new_str(&msg)]) {
        Ok(ex) => set_exc(ex),
        Err(_) => {}
    }
    PyErr
}
fn sig_setitimer(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("setitimer", args, 2, 3)?;
    let which = ab::as_i64(args[0])? as i32;
    let secs = ab::float_value(args[1])?;
    let interval = match args.get(2) {
        Some(i) => ab::float_value(i)?,
        None => 0.0,
    };
    let new = libc::itimerval { it_value: tv_from(secs), it_interval: tv_from(interval) };
    let mut old: libc::itimerval = unsafe { std::mem::zeroed() };
    if unsafe { libc::setitimer(which, &new, &mut old) } != 0 {
        return Err(itimer_err(errno()));
    }
    Ok(tuple::new_tuple(vec![float::new_float(tv_to(&old.it_value)), float::new_float(tv_to(&old.it_interval))]))
}
fn sig_getitimer(_m: &PyObject, w: &PyObject) -> PyResult {
    let which = ab::as_i64(w)? as i32;
    let mut old: libc::itimerval = unsafe { std::mem::zeroed() };
    if unsafe { libc::getitimer(which, &mut old) } != 0 {
        return Err(itimer_err(errno()));
    }
    Ok(tuple::new_tuple(vec![float::new_float(tv_to(&old.it_value)), float::new_float(tv_to(&old.it_interval))]))
}
fn sig_siginterrupt(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("siginterrupt", args, 2, 2)?;
    let sig = signum_arg(args[0])?;
    let flag = ab::is_true(args[1])?;
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(sig, std::ptr::null(), &mut sa) != 0 {
            return Err(crate::types::exceptions::last_os_error());
        }
        if flag {
            sa.sa_flags &= !libc::SA_RESTART;
        } else {
            sa.sa_flags |= libc::SA_RESTART;
        }
        if libc::sigaction(sig, &sa, std::ptr::null_mut()) != 0 {
            return Err(crate::types::exceptions::last_os_error());
        }
    }
    Ok(none::none_ref())
}
fn sig_set_wakeup_fd(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("set_wakeup_fd", &["fd", "warn_on_full_buffer"], 1, 1).kwonly(1);
    let [fd, warn] = crate::args::parse::<2>(&P, args, kw)?;
    let fd = ab::as_i64(fd.unwrap())?;
    let warn = match warn {
        Some(w) => ab::is_true(w)?,
        None => true,
    };
    require_main("set_wakeup_fd")?;
    if fd != -1 {
        if fd < 0 || fd > i32::MAX as i64 {
            raise!(ValueError, "invalid fd");
        }
        let fl = unsafe { libc::fcntl(fd as i32, libc::F_GETFL) };
        if fl < 0 {
            return Err(crate::types::exceptions::last_os_error());
        }
        if fl & libc::O_NONBLOCK == 0 {
            raise!(ValueError, "the fd {} must be in non-blocking mode", fd);
        }
    }
    WAKEUP_WARN.store(warn, Relaxed);
    let old = WAKEUP_FD.swap(fd as i32, AcqRel);
    Ok(int::new_int(old as i64))
}
fn sig_pidfd_send_signal(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("pidfd_send_signal", args, 2, 4)?;
    let pidfd = ab::as_i64(args[0])? as i32;
    let sig = ab::as_i64(args[1])? as i32;
    if let Some(si) = args.get(2) {
        if !si.is(none::none()) {
            raise!(TypeError, "siginfo must be None");
        }
    }
    let flags = match args.get(3) {
        Some(f) => ab::as_i64(f)? as i32,
        None => 0,
    };
    let r = unsafe { libc::syscall(libc::SYS_pidfd_send_signal, pidfd, sig, std::ptr::null::<libc::c_void>(), flags) };
    if r < 0 {
        return Err(crate::types::exceptions::last_os_error());
    }
    Ok(none::none_ref())
}

const SIGNAL_NAMES: &[(&str, i32)] = &[
    ("SIGHUP", libc::SIGHUP),
    ("SIGINT", libc::SIGINT),
    ("SIGQUIT", libc::SIGQUIT),
    ("SIGILL", libc::SIGILL),
    ("SIGTRAP", libc::SIGTRAP),
    ("SIGABRT", libc::SIGABRT),
    ("SIGIOT", libc::SIGIOT),
    ("SIGBUS", libc::SIGBUS),
    ("SIGFPE", libc::SIGFPE),
    ("SIGKILL", libc::SIGKILL),
    ("SIGUSR1", libc::SIGUSR1),
    ("SIGSEGV", libc::SIGSEGV),
    ("SIGUSR2", libc::SIGUSR2),
    ("SIGPIPE", libc::SIGPIPE),
    ("SIGALRM", libc::SIGALRM),
    ("SIGTERM", libc::SIGTERM),
    ("SIGSTKFLT", libc::SIGSTKFLT),
    ("SIGCHLD", libc::SIGCHLD),
    ("SIGCONT", libc::SIGCONT),
    ("SIGSTOP", libc::SIGSTOP),
    ("SIGTSTP", libc::SIGTSTP),
    ("SIGTTIN", libc::SIGTTIN),
    ("SIGTTOU", libc::SIGTTOU),
    ("SIGURG", libc::SIGURG),
    ("SIGXCPU", libc::SIGXCPU),
    ("SIGXFSZ", libc::SIGXFSZ),
    ("SIGVTALRM", libc::SIGVTALRM),
    ("SIGPROF", libc::SIGPROF),
    ("SIGWINCH", libc::SIGWINCH),
    ("SIGIO", libc::SIGIO),
    ("SIGPOLL", libc::SIGPOLL),
    ("SIGPWR", libc::SIGPWR),
    ("SIGSYS", libc::SIGSYS),
];

pub fn init_module() -> PyResult {
    let m = crate::types::module::new_module("_signal")?;
    let d = crate::types::module::module_dict(&m);
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![
            ("signal", FAST, sig_signal, "signal($module, signalnum, handler, /)\n--\n\nInstall handler for signalnum and return the previous handler.\n\nhandler is a callable taking (signum, frame), SIG_IGN or SIG_DFL."),
            ("getsignal", O, sig_getsignal, "getsignal($module, signalnum, /)\n--\n\nReturn the current Python-level handler of a signal."),
            ("strsignal", O, sig_strsignal, "strsignal($module, signalnum, /)\n--\n\nReturn the system description of a signal, or None."),
            ("pthread_sigmask", FAST, sig_pthread_sigmask, "pthread_sigmask($module, how, mask, /)\n--\n\nChange the signal mask of the calling thread and return the old mask."),
            ("sigpending", NOARGS, sig_sigpending),
            ("sigwait", O, sig_sigwait, "sigwait($module, sigset, /)\n--\n\nWait until one of the signals in sigset is pending and return it."),
            ("sigwaitinfo", O, sig_sigwaitinfo, "sigwaitinfo($module, sigset, /)\n--\n\nLike sigwait(), but return a struct_siginfo."),
            ("sigtimedwait", FAST, sig_sigtimedwait, "sigtimedwait($module, sigset, timeout, /)\n--\n\nLike sigwaitinfo(), but give up (returning None) after timeout seconds."),
            ("valid_signals", NOARGS, sig_valid_signals),
            ("raise_signal", O, sig_raise_signal, "raise_signal($module, signalnum, /)\n--\n\nSend a signal to the current process."),
            ("pthread_kill", FAST, sig_pthread_kill, "pthread_kill($module, thread_id, signalnum, /)\n--\n\nSend a signal to a thread of this process."),
            ("alarm", O, sig_alarm, "alarm($module, seconds, /)\n--\n\nSchedule SIGALRM after the given number of seconds."),
            ("pause", NOARGS, sig_pause),
            ("setitimer", FAST, sig_setitimer, "setitimer($module, which, seconds, interval=0.0, /)\n--\n\nArm an interval timer; return the old (delay, interval)."),
            ("getitimer", O, sig_getitimer, "getitimer($module, which, /)\n--\n\nReturn the current (delay, interval) of an interval timer."),
            ("siginterrupt", FAST, sig_siginterrupt, "siginterrupt($module, signalnum, flag, /)\n--\n\nChoose whether signalnum interrupts system calls."),
            ("set_wakeup_fd", FASTKW, sig_set_wakeup_fd, "set_wakeup_fd($module, fd, /, *, warn_on_full_buffer=True)\n--\n\nWrite the number of each arriving signal as a byte to fd (-1 disables).\n\nReturn the previous descriptor."),
            ("pidfd_send_signal", FAST, sig_pidfd_send_signal, "pidfd_send_signal($module, pidfd, sig, siginfo=None, flags=0, /)\n--\n\nSend a signal to the process referred to by a pidfd."),
        ],
        Some(&m),
    )?;
    let set_i = |k: &str, v: i64| dict::set_item_str(&d, k, &int::new_int(v));
    set_i("SIG_DFL", 0)?;
    set_i("SIG_IGN", 1)?;
    set_i("NSIG", NSIG as i64)?;
    set_i("SIG_BLOCK", libc::SIG_BLOCK as i64)?;
    set_i("SIG_UNBLOCK", libc::SIG_UNBLOCK as i64)?;
    set_i("SIG_SETMASK", libc::SIG_SETMASK as i64)?;
    set_i("ITIMER_REAL", libc::ITIMER_REAL as i64)?;
    set_i("ITIMER_VIRTUAL", libc::ITIMER_VIRTUAL as i64)?;
    set_i("ITIMER_PROF", libc::ITIMER_PROF as i64)?;
    set_i("SIGRTMIN", libc::SIGRTMIN() as i64)?;
    set_i("SIGRTMAX", libc::SIGRTMAX() as i64)?;
    for (n, v) in SIGNAL_NAMES {
        set_i(n, *v as i64)?;
    }
    dict::set_item_str(&d, "default_int_handler", &*default_int_handler_obj()?)?;
    if ITIMER_ERROR.load(Acquire).is_null() {
        let ns = dict::new_dict();
        dict::set_item_str(&ns, "__module__", &pystr::new_str("signal"))?;
        let bases = tuple::new_tuple(vec![crate::types::typeobject::type_ref(crate::types::exceptions::OSError())]);
        let cls = ab::call(
            crate::types::typeobject::type_obj(crate::types::typeobject::type_type()),
            &[&pystr::new_str("itimer_error"), &bases, &ns],
        )?;
        ITIMER_ERROR.store(cls.into_raw(), Release);
        let si = crate::modules::sys_structseq::make_type(
            "signal.struct_siginfo",
            &["si_signo", "si_code", "si_errno", "si_pid", "si_uid", "si_status", "si_band"],
            7,
            true,
        )?;
        SIGINFO_TYPE.store(si, Release);
    }
    dict::set_item_str(&d, "ItimerError", unsafe { &*ITIMER_ERROR.load(Acquire) })?;
    dict::set_item_str(&d, "struct_siginfo", crate::types::typeobject::type_obj(SIGINFO_TYPE.load(Acquire)))?;
    let _ = bool_::new_bool(true);
    Ok(m)
}
