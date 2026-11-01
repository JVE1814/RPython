//! The `faulthandler` module: dump Python tracebacks on fatal signals, on a
//! timeout (watchdog thread), on user signals, or on demand. Output is written
//! with raw write(2) calls and no heap allocation so it is usable from signal
//! handlers.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::*;
use crate::raise;
use crate::threads::ThreadState;
use crate::types::{bool_, int, none};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicUsize, Ordering::*};
use std::sync::{Condvar, Mutex};

const MAX_FRAME_DEPTH: usize = 100;
const MAX_STRING_LENGTH: usize = 500;
const DEFAULT_MAX_THREADS: usize = 100;

// ------------------------------------------------------------------ async-signal-safe writer

pub fn wr(fd: i32, s: &[u8]) {
    let mut b = s;
    while !b.is_empty() {
        let n = unsafe { libc::write(fd, b.as_ptr() as *const libc::c_void, b.len()) };
        if n <= 0 {
            if n < 0 && unsafe { *libc::__errno_location() } == libc::EINTR {
                continue;
            }
            return;
        }
        b = &b[n as usize..];
    }
}
fn wr_dec(fd: i32, mut v: u64) {
    let mut buf = [0u8; 24];
    let mut i = buf.len();
    if v == 0 {
        i -= 1;
        buf[i] = b'0';
    }
    while v > 0 {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    wr(fd, &buf[i..]);
}
fn wr_hex(fd: i32, v: u64, width: usize) {
    let hex = b"0123456789abcdef";
    let mut buf = [0u8; 16];
    let mut i = 16;
    let mut x = v;
    while x > 0 || 16 - i < width {
        i -= 1;
        buf[i] = hex[(x & 0xf) as usize];
        x >>= 4;
        if i == 0 {
            break;
        }
    }
    wr(fd, &buf[i..]);
}
/// Write a str object as ASCII with backslash escapes (truncated).
fn wr_pystr(fd: i32, o: &PyObject) {
    if !crate::types::str::is_str(o) {
        wr(fd, b"???");
        return;
    }
    let d = crate::types::str::str_data(o);
    let n = d.len();
    let mut buf = [0u8; 64];
    let mut k = 0;
    let flush = |buf: &[u8], k: &mut usize| {
        wr(fd, &buf[..*k]);
        *k = 0;
    };
    for i in 0..n.min(MAX_STRING_LENGTH) {
        let c = d.get(i);
        if k + 12 > buf.len() {
            flush(&buf, &mut k);
        }
        if (0x20..0x7f).contains(&c) {
            buf[k] = c as u8;
            k += 1;
        } else {
            let (pre, w): (&[u8], usize) = if c < 0x100 { (b"\\x", 2) } else if c < 0x10000 { (b"\\u", 4) } else { (b"\\U", 8) };
            for &b in pre {
                buf[k] = b;
                k += 1;
            }
            let hex = b"0123456789abcdef";
            for j in (0..w).rev() {
                buf[k] = hex[((c >> (4 * j)) & 0xf) as usize];
                k += 1;
            }
        }
    }
    flush(&buf, &mut k);
    if n > MAX_STRING_LENGTH {
        wr(fd, b"...");
    }
}

fn dump_frames(fd: i32, ts: &ThreadState) {
    let mut f = ts.frame.get();
    if f.is_null() {
        wr(fd, b"  <no Python frame>\n");
        return;
    }
    let mut depth = 0;
    while !f.is_null() {
        if depth >= MAX_FRAME_DEPTH {
            wr(fd, b"  ...\n");
            break;
        }
        let fr = unsafe { &*f };
        let cd = crate::types::code::code_data(fr.code());
        wr(fd, b"  File \"");
        wr_pystr(fd, &cd.filename);
        wr(fd, b"\", line ");
        let ln = fr.lineno();
        if ln >= 0 {
            wr_dec(fd, ln as u64);
        } else {
            wr(fd, b"???");
        }
        wr(fd, b" in ");
        wr_pystr(fd, &cd.co_name);
        wr(fd, b"\n");
        f = fr.prev;
        depth += 1;
    }
}

fn thread_name(tid: u64, buf: &mut [u8; 32]) -> usize {
    let r = unsafe { libc::pthread_getname_np(tid as libc::pthread_t, buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if r != 0 {
        return 0;
    }
    buf.iter().position(|&c| c == 0).unwrap_or(0)
}

fn write_thread_header(fd: i32, ts: &ThreadState, current: bool) {
    if current {
        wr(fd, b"Current thread 0x");
    } else {
        wr(fd, b"Thread 0x");
    }
    wr_hex(fd, ts.c.thread_id as u64, 16);
    let mut nb = [0u8; 32];
    let n = thread_name(ts.c.thread_id as u64, &mut nb);
    if n > 0 {
        wr(fd, b" [");
        wr(fd, &nb[..n]);
        wr(fd, b"]");
    }
    wr(fd, b" (most recent call first):\n");
}

/// Dump the traceback of all Python threads (or only the current one).
pub fn dump_traceback_fd(fd: i32, all_threads: bool, max_threads: usize) {
    let cur = crate::threads::current_ptr();
    if !all_threads {
        wr(fd, b"Stack (most recent call first):\n");
        if !cur.is_null() {
            dump_frames(fd, unsafe { &*cur });
        }
        return;
    }
    // try_lock: never block (we may run in a signal handler)
    let threads: Vec<usize> = match crate::threads::ALL_THREADS.try_lock() {
        Ok(g) => g.clone(),
        Err(_) => {
            wr(fd, b"<Cannot show all threads>\n");
            if !cur.is_null() {
                write_thread_header(fd, unsafe { &*cur }, true);
                dump_frames(fd, unsafe { &*cur });
            }
            return;
        }
    };
    let mut count = 0;
    let mut first = true;
    // most recently created first, like CPython's thread list
    for &t in threads.iter().rev() {
        let ts = unsafe { &*(t as *const ThreadState) };
        if !crate::threads::is_live(ts) {
            continue;
        }
        if count >= max_threads {
            wr(fd, b"...\n");
            break;
        }
        if !first {
            wr(fd, b"\n");
        }
        first = false;
        write_thread_header(fd, ts, t == cur as usize);
        dump_frames(fd, ts);
        count += 1;
    }
}

fn dump_c_stack_fd(fd: i32) {
    wr(fd, b"Current thread's C stack trace (most recent call first):\n");
    wr(fd, b"  <cannot get C stack on this system>\n");
}

// ------------------------------------------------------------------ file argument

/// Resolve the `file` argument to a file descriptor, keeping the object alive.
fn get_fileno(file: Option<&PyObject>) -> PyResult<(i32, Option<PyRef>)> {
    let f = match file {
        Some(f) if !f.is(none::none()) => f.to_ref(),
        _ => match crate::modules::sys::get_sys_attr("stderr") {
            Some(e) if !e.is(none::none()) => e,
            _ => raise!(RuntimeError, "sys.stderr is None"),
        },
    };
    if int::is_int(&f) {
        let fd = ab::as_i64(&f)?;
        if fd < 0 {
            raise!(ValueError, "file is not a valid file descriptor");
        }
        return Ok((fd as i32, None));
    }
    let r = ab::call_method_str(&f, "fileno", &[])?;
    if !int::is_int(&r) {
        raise!(RuntimeError, "file.fileno() is not a valid file descriptor");
    }
    let fd = ab::as_i64(&r)?;
    if fd < 0 {
        raise!(RuntimeError, "file.fileno() is not a valid file descriptor");
    }
    if ab::call_method_str(&f, "flush", &[]).is_err() {
        clear_exc();
    }
    Ok((fd as i32, Some(f)))
}

fn parse_file_threads(fname: &str, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult<(i32, Option<PyRef>, bool, usize)> {
    let p = crate::args::Params::new(fname, &["file", "all_threads", "max_threads"], 0, 0);
    let [f, a, m] = crate::args::parse::<3>(&p, args, kw)?;
    let (fd, keep) = get_fileno(f)?;
    let all = match a {
        Some(a) => ab::is_true(a)?,
        None => true,
    };
    let maxt = match m {
        Some(m) => {
            let n = ab::as_i64(m)?;
            if n < 1 {
                raise!(ValueError, "max_threads must be >= 1");
            }
            n as usize
        }
        None => DEFAULT_MAX_THREADS,
    };
    Ok((fd, keep, all, maxt))
}

fn fh_dump_traceback(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let (fd, _keep, all, maxt) = parse_file_threads("dump_traceback", args, kw)?;
    dump_traceback_fd(fd, all, maxt);
    Ok(none::none_ref())
}
fn fh_dump_c_stack(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("dump_c_stack", &["file"], 0, 0);
    let [f] = crate::args::parse::<1>(&P, args, kw)?;
    let (fd, _keep) = get_fileno(f)?;
    dump_c_stack_fd(fd);
    Ok(none::none_ref())
}

// ------------------------------------------------------------------ fatal signals

struct FatalSig {
    signum: i32,
    name: &'static [u8],
}
const FATAL: [FatalSig; 5] = [
    FatalSig { signum: libc::SIGBUS, name: b"Bus error" },
    FatalSig { signum: libc::SIGILL, name: b"Illegal instruction" },
    FatalSig { signum: libc::SIGFPE, name: b"Floating-point exception" },
    FatalSig { signum: libc::SIGABRT, name: b"Aborted" },
    FatalSig { signum: libc::SIGSEGV, name: b"Segmentation fault" },
];

static ENABLED: AtomicBool = AtomicBool::new(false);
static FATAL_FD: AtomicI32 = AtomicI32::new(2);
static FATAL_ALL_THREADS: AtomicBool = AtomicBool::new(true);
static FATAL_C_STACK: AtomicBool = AtomicBool::new(true);
static FATAL_MAX_THREADS: AtomicUsize = AtomicUsize::new(DEFAULT_MAX_THREADS);
static FATAL_FILE: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
static mut PREV_ACTIONS: [std::mem::MaybeUninit<libc::sigaction>; 5] = [const { std::mem::MaybeUninit::uninit() }; 5];
static ALT_STACK: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut());

extern "C" fn fatal_handler(sig: libc::c_int) {
    let saved = unsafe { *libc::__errno_location() };
    let idx = match FATAL.iter().position(|f| f.signum == sig) {
        Some(i) => i,
        None => return,
    };
    if !ENABLED.load(Acquire) {
        unsafe { libc::signal(sig, libc::SIG_DFL) };
        unsafe { libc::raise(sig) };
        return;
    }
    // restore the previous handler first so that a second fault is fatal
    ENABLED.store(false, Release);
    unsafe {
        for (i, f) in FATAL.iter().enumerate() {
            libc::sigaction(f.signum, (*(&raw const PREV_ACTIONS))[i].as_ptr(), std::ptr::null_mut());
        }
    }
    let fd = FATAL_FD.load(Relaxed);
    wr(fd, b"Fatal Python error: ");
    wr(fd, FATAL[idx].name);
    wr(fd, b"\n\n");
    if FATAL_ALL_THREADS.load(Relaxed) {
        // without a GIL other threads cannot be inspected safely
        wr(fd, b"<Cannot show all threads while the GIL is disabled>\n");
    }
    dump_traceback_fd(fd, false, 1);
    if FATAL_C_STACK.load(Relaxed) {
        dump_c_stack_fd(fd);
    }
    unsafe {
        *libc::__errno_location() = saved;
        // re-deliver to the previous handler (default: terminate)
        libc::raise(sig);
    }
}

fn install_alt_stack() {
    unsafe {
        let size = (libc::SIGSTKSZ as usize).max(64 * 1024) * 2;
        let mut p = ALT_STACK.load(Acquire);
        if p.is_null() {
            p = libc::malloc(size) as *mut u8;
            ALT_STACK.store(p, Release);
        }
        let ss = libc::stack_t { ss_sp: p as *mut libc::c_void, ss_flags: 0, ss_size: size };
        libc::sigaltstack(&ss, std::ptr::null_mut());
    }
}

fn fh_enable(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("enable", &["file", "all_threads", "c_stack", "max_threads"], 0, 0);
    let [f, a, c, mx] = crate::args::parse::<4>(&P, args, kw)?;
    let (fd, keep) = get_fileno(f)?;
    let all = match a {
        Some(a) => ab::is_true(a)?,
        None => true,
    };
    let cs = match c {
        Some(c) => ab::is_true(c)?,
        None => true,
    };
    if let Some(m) = mx {
        let n = ab::as_i64(m)?;
        if n < 1 {
            raise!(ValueError, "max_threads must be >= 1");
        }
        FATAL_MAX_THREADS.store(n as usize, Relaxed);
    }
    enable_impl(fd, all, cs);
    let old = FATAL_FILE.swap(keep.map_or(std::ptr::null_mut(), |k| k.into_raw()), AcqRel);
    unsafe { xdecref(old) };
    Ok(none::none_ref())
}

pub fn enable_impl(fd: i32, all: bool, c_stack: bool) {
    FATAL_FD.store(fd, Relaxed);
    FATAL_ALL_THREADS.store(all, Relaxed);
    FATAL_C_STACK.store(c_stack, Relaxed);
    if ENABLED.swap(true, AcqRel) {
        return;
    }
    install_alt_stack();
    unsafe {
        for (i, f) in FATAL.iter().enumerate() {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = fatal_handler as *const () as libc::sighandler_t;
            libc::sigemptyset(&mut sa.sa_mask);
            sa.sa_flags = libc::SA_NODEFER | libc::SA_ONSTACK;
            libc::sigaction(f.signum, &sa, (*(&raw mut PREV_ACTIONS))[i].as_mut_ptr());
        }
    }
}

fn fh_disable(_m: &PyObject) -> PyResult {
    if !ENABLED.swap(false, AcqRel) {
        return Ok(bool_::new_bool(false));
    }
    unsafe {
        for (i, f) in FATAL.iter().enumerate() {
            libc::sigaction(f.signum, (*(&raw const PREV_ACTIONS))[i].as_ptr(), std::ptr::null_mut());
        }
    }
    let old = FATAL_FILE.swap(std::ptr::null_mut(), AcqRel);
    unsafe { xdecref(old) };
    Ok(bool_::new_bool(true))
}
fn fh_is_enabled(_m: &PyObject) -> PyResult {
    Ok(bool_::new_bool(ENABLED.load(Acquire)))
}

// ------------------------------------------------------------------ dump_traceback_later

struct Watchdog {
    cancel: bool,
    generation: u64,
}
static WATCHDOG: Mutex<Watchdog> = Mutex::new(Watchdog { cancel: false, generation: 0 });
static WATCHDOG_CV: Condvar = Condvar::new();
static WATCHDOG_FILE: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
static WATCHDOG_THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

fn format_timeout(us: u64) -> String {
    let secs = us / 1_000_000;
    let frac = us % 1_000_000;
    let (h, m, s) = (secs / 3600, (secs / 60) % 60, secs % 60);
    if frac != 0 {
        format!("Timeout ({}:{:02}:{:02}.{:06})!\n", h, m, s, frac)
    } else {
        format!("Timeout ({}:{:02}:{:02})!\n", h, m, s)
    }
}

fn cancel_watchdog() {
    {
        let mut g = WATCHDOG.lock().unwrap();
        g.cancel = true;
        g.generation += 1;
        WATCHDOG_CV.notify_all();
    }
    let h = WATCHDOG_THREAD.lock().unwrap().take();
    if let Some(h) = h {
        let _ = crate::threads::blocking(|| h.join());
    }
    let old = WATCHDOG_FILE.swap(std::ptr::null_mut(), AcqRel);
    unsafe { xdecref(old) };
}

fn fh_dump_traceback_later(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("dump_traceback_later", &["timeout", "repeat", "file", "exit"], 0, 1);
    let [t, r, f, e] = crate::args::parse::<4>(&P, args, kw)?;
    let secs = ab::float_value(t.unwrap())?;
    if secs <= 0.0 {
        raise!(ValueError, "timeout must be greater than 0");
    }
    if secs > 1e12 {
        raise!(OverflowError, "timeout value is too large");
    }
    let us = (secs * 1e6).round() as u64;
    let repeat = match r {
        Some(r) => ab::is_true(r)?,
        None => false,
    };
    let exit = match e {
        Some(e) => ab::is_true(e)?,
        None => false,
    };
    let (fd, keep) = get_fileno(f)?;
    cancel_watchdog();
    let header = format_timeout(us);
    let wgen = {
        let mut g = WATCHDOG.lock().unwrap();
        g.cancel = false;
        g.generation
    };
    let old = WATCHDOG_FILE.swap(keep.map_or(std::ptr::null_mut(), |k| k.into_raw()), AcqRel);
    unsafe { xdecref(old) };
    let h = std::thread::Builder::new()
        .name("faulthandler".into())
        .spawn(move || loop {
            let deadline = std::time::Instant::now() + std::time::Duration::from_micros(us);
            let mut g = WATCHDOG.lock().unwrap();
            loop {
                if g.cancel || g.generation != wgen {
                    return;
                }
                let now = std::time::Instant::now();
                if now >= deadline {
                    break;
                }
                g = WATCHDOG_CV.wait_timeout(g, deadline - now).unwrap().0;
            }
            drop(g);
            wr(fd, header.as_bytes());
            dump_traceback_fd(fd, true, DEFAULT_MAX_THREADS);
            if exit {
                unsafe { libc::_exit(1) };
            }
            if !repeat {
                return;
            }
        })
        .map_err(|_| err!(RuntimeError, "unable to start watchdog thread"))?;
    *WATCHDOG_THREAD.lock().unwrap() = Some(h);
    Ok(none::none_ref())
}
fn fh_cancel_dump_traceback_later(_m: &PyObject) -> PyResult {
    cancel_watchdog();
    Ok(none::none_ref())
}

// ------------------------------------------------------------------ register (user signals)

struct UserSig {
    fd: i32,
    all_threads: bool,
    chain: bool,
    prev: libc::sigaction,
    file: usize,
}
static USER: Mutex<Vec<(i32, UserSig)>> = Mutex::new(Vec::new());
#[allow(clippy::declare_interior_mutable_const)]
const AU: AtomicI32 = AtomicI32::new(-1);
static USER_FD: [AtomicI32; 65] = [AU; 65];
static USER_FLAGS: [AtomicI32; 65] = [AU; 65];

extern "C" fn user_handler(sig: libc::c_int) {
    let saved = unsafe { *libc::__errno_location() };
    if !(0..65).contains(&sig) {
        return;
    }
    let fd = USER_FD[sig as usize].load(Acquire);
    let flags = USER_FLAGS[sig as usize].load(Acquire);
    if fd < 0 {
        return;
    }
    let all = flags & 1 != 0;
    dump_traceback_fd(fd, all, DEFAULT_MAX_THREADS);
    if flags & 2 != 0 {
        // chain: call the previous handler
        if let Ok(g) = USER.try_lock() {
            if let Some((_, u)) = g.iter().find(|(s, _)| *s == sig) {
                let h = u.prev.sa_sigaction;
                if h != libc::SIG_DFL && h != libc::SIG_IGN && h != user_handler as *const () as libc::sighandler_t {
                    let f: extern "C" fn(libc::c_int) = unsafe { std::mem::transmute(h) };
                    drop(g);
                    f(sig);
                }
            }
        }
    }
    unsafe { *libc::__errno_location() = saved };
}

fn fh_register(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("register", &["signum", "file", "all_threads", "chain"], 0, 1);
    let [s, f, a, c] = crate::args::parse::<4>(&P, args, kw)?;
    let sig = ab::as_i64(s.unwrap())? as i32;
    if FATAL.iter().any(|x| x.signum == sig) {
        raise!(RuntimeError, "signal {} cannot be registered, use enable() instead", sig);
    }
    if !(1..65).contains(&sig) {
        raise!(ValueError, "signal number out of range");
    }
    let (fd, keep) = get_fileno(f)?;
    let all = match a {
        Some(a) => ab::is_true(a)?,
        None => true,
    };
    let chain = match c {
        Some(c) => ab::is_true(c)?,
        None => false,
    };
    let mut g = USER.lock().unwrap();
    let keep_p = keep.map_or(0, |k| k.into_raw() as usize);
    USER_FD[sig as usize].store(fd, Release);
    USER_FLAGS[sig as usize].store(all as i32 | ((chain as i32) << 1), Release);
    if let Some((_, u)) = g.iter_mut().find(|(s, _)| *s == sig) {
        u.fd = fd;
        u.all_threads = all;
        u.chain = chain;
        let old = std::mem::replace(&mut u.file, keep_p);
        unsafe { xdecref(old as *mut PyObject) };
        return Ok(none::none_ref());
    }
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = user_handler as *const () as libc::sighandler_t;
        libc::sigemptyset(&mut sa.sa_mask);
        sa.sa_flags = libc::SA_ONSTACK | if chain { libc::SA_NODEFER } else { 0 };
        let mut prev: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(sig, &sa, &mut prev) != 0 {
            return Err(crate::types::exceptions::last_os_error());
        }
        g.push((sig, UserSig { fd, all_threads: all, chain, prev, file: keep_p }));
    }
    Ok(none::none_ref())
}
fn fh_unregister(_m: &PyObject, s: &PyObject) -> PyResult {
    let sig = ab::as_i64(s)? as i32;
    if !(1..65).contains(&sig) {
        raise!(ValueError, "signal number out of range");
    }
    let mut g = USER.lock().unwrap();
    match g.iter().position(|(x, _)| *x == sig) {
        Some(i) => {
            let (_, u) = g.remove(i);
            USER_FD[sig as usize].store(-1, Release);
            unsafe {
                libc::sigaction(sig, &u.prev, std::ptr::null_mut());
                xdecref(u.file as *mut PyObject);
            }
            let _ = (u.fd, u.all_threads, u.chain);
            Ok(bool_::new_bool(true))
        }
        None => Ok(bool_::new_bool(false)),
    }
}

// ------------------------------------------------------------------ fatal error / test helpers

/// Py_FatalError: print the message and traceback, then abort.
pub fn fatal_error(func: Option<&str>, msg: &str) -> ! {
    let fd = 2;
    wr(fd, b"Fatal Python error: ");
    if let Some(f) = func {
        wr(fd, f.as_bytes());
        wr(fd, b": ");
    }
    wr(fd, msg.as_bytes());
    wr(fd, b"\nPython runtime state: initialized\n\n");
    dump_traceback_fd(fd, false, 1);
    if FATAL_C_STACK.load(Relaxed) {
        dump_c_stack_fd(fd);
    }
    ENABLED.store(false, Release);
    unsafe {
        libc::signal(libc::SIGABRT, libc::SIG_DFL);
        libc::abort();
    }
}

fn fh_read_null(_m: &PyObject) -> PyResult {
    unsafe {
        let p: *const i32 = std::ptr::null();
        let v = std::ptr::read_volatile(p);
        Ok(int::new_int(v as i64))
    }
}
fn fh_sigsegv(_m: &PyObject, _args: &[&PyObject]) -> PyResult {
    if !ENABLED.load(Acquire) {
        // Rust's own SIGSEGV handler (stack overflow detection) swallows a
        // raised signal that is not a guard-page fault: crash for real.
        unsafe { libc::signal(libc::SIGSEGV, libc::SIG_DFL) };
    }
    unsafe { libc::raise(libc::SIGSEGV) };
    Ok(none::none_ref())
}
fn fh_sigabrt(_m: &PyObject) -> PyResult {
    unsafe { libc::abort() };
}
fn fh_sigfpe(_m: &PyObject) -> PyResult {
    unsafe { libc::raise(libc::SIGFPE) };
    Ok(none::none_ref())
}
fn fh_fatal_error(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("_fatal_error", args, 1, 2)?;
    let msg = crate::types::str::to_string_lossy(&*ab::str(args[0])?);
    fatal_error(Some("_fatal_error"), &msg)
}
fn fh_fatal_error_c_thread(_m: &PyObject) -> PyResult {
    let h = std::thread::spawn(|| fatal_error(Some("faulthandler_fatal_error_thread"), "in new thread"));
    let _ = crate::threads::blocking(|| h.join());
    Ok(none::none_ref())
}
#[inline(never)]
fn recurse(depth: u64, sink: &mut [u8; 4096]) -> u64 {
    let mut buf = [0u8; 4096];
    buf[(depth % 4096) as usize] = depth as u8;
    sink[0] ^= buf[(depth * 7 % 4096) as usize];
    std::hint::black_box(&mut buf);
    recurse(depth + 1, sink) + buf[0] as u64
}
fn fh_stack_overflow(_m: &PyObject) -> PyResult {
    let mut sink = [0u8; 4096];
    let r = recurse(0, &mut sink);
    Ok(int::new_int(r as i64))
}

pub fn init_module() -> PyResult {
    let m = crate::types::module::new_module("faulthandler")?;
    let d = crate::types::module::module_dict(&m);
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![
            ("enable", FASTKW, fh_enable, "enable($module, /, file=sys.stderr, all_threads=True, c_stack=True)\n--\n\nInstall handlers that dump the Python traceback on a fatal signal."),
            ("disable", NOARGS, fh_disable),
            ("is_enabled", NOARGS, fh_is_enabled),
            ("dump_traceback", FASTKW, fh_dump_traceback, "dump_traceback($module, /, file=sys.stderr, all_threads=True)\n--\n\nWrite the traceback of the current thread (or of all threads) to file."),
            ("dump_c_stack", FASTKW, fh_dump_c_stack, "dump_c_stack($module, /, file=sys.stderr)\n--\n\nWrite the C stack of the current thread to file."),
            ("dump_traceback_later", FASTKW, fh_dump_traceback_later, "dump_traceback_later($module, /, timeout, repeat=False, file=sys.stderr, exit=False)\n--\n\nDump the tracebacks of all threads after timeout seconds (again every\ntimeout seconds if repeat), exiting the process afterwards if exit."),
            ("cancel_dump_traceback_later", NOARGS, fh_cancel_dump_traceback_later),
            ("register", FASTKW, fh_register, "register($module, /, signum, file=sys.stderr, all_threads=True, chain=False)\n--\n\nDump the traceback when the signal signum arrives."),
            ("unregister", O, fh_unregister, "unregister($module, signum, /)\n--\n\nUndo register() for a signal; return True if a handler was installed."),
            ("_read_null", NOARGS, fh_read_null),
            ("_sigsegv", FAST, fh_sigsegv),
            ("_sigabrt", NOARGS, fh_sigabrt),
            ("_sigfpe", NOARGS, fh_sigfpe),
            ("_fatal_error", FAST, fh_fatal_error),
            ("_fatal_error_c_thread", NOARGS, fh_fatal_error_c_thread),
            ("_stack_overflow", NOARGS, fh_stack_overflow),
        ],
        Some(&m),
    )?;
    Ok(m)
}

/// -X faulthandler / PYTHONFAULTHANDLER at startup.
pub fn enable_at_startup() {
    enable_impl(2, true, true);
}
