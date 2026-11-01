//! `_posixsubprocess.fork_exec` (the process launcher used by subprocess.Popen).
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::PyObject;
use crate::modules::posix::path::{c_int, fs_encode_str, fspath};
use crate::object::PyRef;
use crate::raise;
use crate::types::typeobject::tname;
use crate::types::{bytes, int, list, module, none, tuple};
use std::ffi::CString;

fn to_cstring(o: &PyObject) -> PyResult<CString> {
    let p = fspath(o)?;
    let raw = if bytes::is_bytes(&p) { bytes::bytes_data(&p).to_vec() } else { fs_encode_str(&p)? };
    if raw.contains(&0) {
        raise!(ValueError, "embedded null byte");
    }
    Ok(CString::new(raw).unwrap())
}

fn seq_cstrings(o: &PyObject, what: &str) -> PyResult<Vec<CString>> {
    if !(tuple::is_tuple(o) || list::is_list(o)) {
        if ab::get_iter(o).is_err() {
            clear_exc();
            raise!(TypeError, "{} must be a sequence, not {}", what, tname(o));
        }
    }
    let items = ab::collect_iterable(o)?;
    let mut v = Vec::with_capacity(items.len());
    for it in &items {
        v.push(to_cstring(it)?);
    }
    Ok(v)
}

fn ptrs(v: &[CString]) -> Vec<*const libc::c_char> {
    let mut p: Vec<*const libc::c_char> = v.iter().map(|s| s.as_ptr()).collect();
    p.push(std::ptr::null());
    p
}

fn fds_to_keep(o: &PyObject) -> PyResult<Vec<i32>> {
    if !tuple::is_tuple(o) {
        raise!(TypeError, "fds_to_keep must be a tuple, not {}", tname(o));
    }
    let mut v = Vec::new();
    let mut prev = -1i64;
    for it in tuple::tuple_items(o) {
        if !int::is_int(it) {
            raise!(ValueError, "bad value(s) in fds_to_keep");
        }
        let fd = int::as_i64(it).unwrap_or(-1);
        if fd < 0 || fd <= prev || fd > i32::MAX as i64 {
            raise!(ValueError, "bad value(s) in fds_to_keep");
        }
        prev = fd;
        v.push(fd as i32);
    }
    Ok(v)
}

/// Write "<exc>:<hex errno>:<msg>" to the error pipe (async-signal-safe).
unsafe fn report(fd: i32, exc: &[u8], err: i32, msg: &[u8]) {
    unsafe {
        let mut buf = [0u8; 128];
        let mut n = 0;
        let mut push = |b: &[u8]| {
            for &c in b {
                if n < buf.len() {
                    buf[n] = c;
                    n += 1;
                }
            }
        };
        push(exc);
        push(b":");
        let mut hex = [0u8; 16];
        let mut h = 0;
        let mut e = err as u32;
        if e == 0 {
            hex[0] = b'0';
            h = 1;
        }
        while e > 0 {
            let d = (e & 0xf) as u8;
            hex[h] = if d < 10 { b'0' + d } else { b'a' + d - 10 };
            h += 1;
            e >>= 4;
        }
        hex[..h].reverse();
        push(&hex[..h]);
        push(b":");
        push(msg);
        libc::write(fd, buf.as_ptr() as *const libc::c_void, n);
    }
}

unsafe fn close_fds_except(keep: &[i32], from: i32) {
    unsafe {
        // close every fd >= from not in keep, using close_range between kept fds
        let mut start = from as u32;
        for &k in keep {
            if (k as u32) < start {
                continue;
            }
            if k as u32 > start {
                if libc::syscall(libc::SYS_close_range, start, (k - 1) as u32, 0) != 0 {
                    for fd in start..k as u32 {
                        libc::close(fd as i32);
                    }
                }
            }
            start = k as u32 + 1;
        }
        if libc::syscall(libc::SYS_close_range, start, u32::MAX, 0) != 0 {
            let max = libc::sysconf(libc::_SC_OPEN_MAX).max(256) as u32;
            for fd in start..max {
                libc::close(fd as i32);
            }
        }
    }
}

struct ChildSpec {
    argv: Vec<CString>,
    exes: Vec<CString>,
    envp: Option<Vec<CString>>,
    cwd: Option<CString>,
    close_fds: bool,
    keep: Vec<i32>,
    fds: [i32; 6],
    errpipe_read: i32,
    errpipe_write: i32,
    restore_signals: bool,
    call_setsid: bool,
    pgid: i64,
    gid: Option<u32>,
    groups: Option<Vec<libc::gid_t>>,
    uid: Option<u32>,
    umask: i32,
    preexec: Option<PyRef>,
}

unsafe fn child_exec(s: &ChildSpec) -> ! {
    unsafe {
        let [p2cread, p2cwrite, c2pread, mut c2pwrite, errread, mut errwrite] = s.fds;
        let ew = s.errpipe_write;
        let fail = |stage: &[u8]| -> ! {
            let e = *libc::__errno_location();
            report(ew, b"OSError", e, stage);
            libc::_exit(255);
        };
        for fd in [p2cwrite, c2pread, errread] {
            if fd != -1 {
                libc::close(fd);
            }
        }
        if s.errpipe_read != -1 {
            libc::close(s.errpipe_read);
        }
        // make sure the targets don't clobber each other
        if c2pwrite == 0 {
            c2pwrite = libc::dup(c2pwrite);
            if c2pwrite < 0 {
                fail(b"noexec");
            }
        }
        while errwrite == 0 || errwrite == 1 {
            errwrite = libc::dup(errwrite);
            if errwrite < 0 {
                fail(b"noexec");
            }
        }
        for (src, dst) in [(p2cread, 0), (c2pwrite, 1), (errwrite, 2)] {
            if src == dst {
                // clear close-on-exec
                let fl = libc::fcntl(src, libc::F_GETFD);
                if fl >= 0 {
                    libc::fcntl(src, libc::F_SETFD, fl & !libc::FD_CLOEXEC);
                }
            } else if src != -1 && libc::dup2(src, dst) < 0 {
                fail(b"noexec");
            }
        }
        if let Some(cwd) = &s.cwd {
            if libc::chdir(cwd.as_ptr()) < 0 {
                fail(b"noexec:chdir");
            }
        }
        if s.umask >= 0 {
            libc::umask(s.umask as libc::mode_t);
        }
        if s.restore_signals {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            libc::signal(libc::SIGXFSZ, libc::SIG_DFL);
        }
        if s.call_setsid && libc::setsid() < 0 {
            fail(b"noexec");
        }
        if s.pgid >= 0 && libc::setpgid(0, s.pgid as libc::pid_t) < 0 {
            fail(b"noexec");
        }
        if let Some(g) = &s.groups {
            if libc::setgroups(g.len(), g.as_ptr()) < 0 {
                fail(b"noexec");
            }
        }
        if let Some(gid) = s.gid {
            if libc::setregid(gid, gid) < 0 {
                fail(b"noexec");
            }
        }
        if let Some(uid) = s.uid {
            if libc::setreuid(uid, uid) < 0 {
                fail(b"noexec");
            }
        }
        if let Some(f) = &s.preexec {
            crate::modules::posix::proc::run_after_fork_child();
            if ab::call(f, &[]).is_err() {
                clear_exc();
                report(ew, b"SubprocessError", 0, b"Exception occurred in preexec_fn.");
                libc::_exit(255);
            }
        }
        if s.close_fds {
            close_fds_except(&s.keep, 3);
        }
        let argv = ptrs(&s.argv);
        let envp = s.envp.as_ref().map(|e| ptrs(e));
        let mut saved_errno = 0;
        for exe in &s.exes {
            match &envp {
                Some(e) => libc::execve(exe.as_ptr(), argv.as_ptr(), e.as_ptr()),
                None => libc::execv(exe.as_ptr(), argv.as_ptr()),
            };
            let e = *libc::__errno_location();
            if e != libc::ENOENT && e != libc::ENOTDIR && saved_errno == 0 {
                saved_errno = e;
            }
        }
        if saved_errno != 0 {
            *libc::__errno_location() = saved_errno;
        }
        let e = *libc::__errno_location();
        report(ew, b"OSError", e, b"");
        libc::_exit(255);
    }
}

fn opt_id(o: &PyObject, what: &str) -> PyResult<Option<u32>> {
    if o.is(none::none()) || crate::types::bool_::is_bool(o) && !ab::is_true(o)? {
        return Ok(None);
    }
    let v = ab::as_i64(o)?;
    if v < 0 {
        return Ok(None);
    }
    if v > u32::MAX as i64 {
        raise!(OverflowError, "{} too large", what);
    }
    Ok(Some(v as u32))
}

fn fork_exec(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.len() != 23 && args.len() != 22 {
        raise!(TypeError, "fork_exec() takes 22 or 23 positional arguments ({} given)", args.len());
    }
    let process_args = args[0];
    let argv = if process_args.is(none::none()) { Vec::new() } else { seq_cstrings(process_args, "args")? };
    let exes = seq_cstrings(args[1], "executable_list")?;
    let close_fds = ab::is_true(args[2])?;
    let keep = fds_to_keep(args[3])?;
    let cwd = if args[4].is(none::none()) { None } else { Some(to_cstring(args[4])?) };
    let envp = if args[5].is(none::none()) { None } else { Some(seq_cstrings(args[5], "env")?) };
    let mut fds = [0i32; 6];
    for i in 0..6 {
        fds[i] = c_int(args[6 + i])?;
    }
    let errpipe_read = c_int(args[12])?;
    let errpipe_write = c_int(args[13])?;
    let restore_signals = ab::is_true(args[14])?;
    let call_setsid = ab::is_true(args[15])?;
    let pgid = ab::as_i64(args[16])?;
    let gid = opt_id(args[17], "gid")?;
    let groups = if args[18].is(none::none()) {
        None
    } else {
        let items = ab::collect_iterable(args[18])?;
        let mut g = Vec::with_capacity(items.len());
        for it in &items {
            g.push(ab::as_i64(it)? as libc::gid_t);
        }
        Some(g)
    };
    let uid = opt_id(args[19], "uid")?;
    let umask = c_int(args[20])?;
    let preexec = if args[21].is(none::none()) { None } else { Some(args[21].to_ref()) };
    if close_fds && errpipe_write < 3 {
        raise!(ValueError, "errpipe_write must be >= 3");
    }
    let spec = ChildSpec {
        argv,
        exes,
        envp,
        cwd,
        close_fds,
        keep,
        fds,
        errpipe_read,
        errpipe_write,
        restore_signals,
        call_setsid,
        pgid,
        gid,
        groups,
        uid,
        umask,
        preexec,
    };
    if spec.preexec.is_some() {
        crate::modules::posix::proc::run_before_fork();
    }
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        unsafe { child_exec(&spec) }
    }
    let e = crate::modules::posix::path::errno();
    if spec.preexec.is_some() {
        crate::modules::posix::proc::run_after_fork_parent();
    }
    if pid < 0 {
        return Err(crate::types::exceptions::os_error_from_errno(e, None, None));
    }
    Ok(int::new_int(pid as i64))
}

pub fn init() -> PyResult {
    let m = module::new_module("_posixsubprocess")?;
    let d = module::module_dict(&m);
    crate::builtins::add_methods_to_dict(&d, crate::mdefs![("fork_exec", FAST, fork_exec)], Some(&m))?;
    Ok(m)
}
