//! `_posixsubprocess`: fork_exec() used by subprocess.Popen on POSIX.
//!
//! Everything the child needs (argv, envp, executable candidates, fd lists)
//! is converted before fork(); between fork() and exec() the child only uses
//! async-signal-safe libc calls, unless a preexec_fn must run Python code.
//! Errors in the child are reported to the parent through `errpipe_write`
//! as "ExceptionName:hex_errno:message".

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::tname;
use crate::types::{bytes, dict, int, list, none, str as pystr, tuple};
use std::ffi::{c_int, CString};

fn errno() -> i32 {
    unsafe { *libc::__errno_location() }
}

/// PyUnicode_FSConverter: str/bytes/PathLike -> bytes without NUL.
fn fs_convert(o: &PyObject) -> PyResult<CString> {
    let p = crate::modules::posix::path::fspath(o)?;
    let raw = if bytes::is_bytes(&p) {
        bytes::bytes_data(&p).to_vec()
    } else {
        crate::modules::posix::path::fs_encode_str(&p)?
    };
    match CString::new(raw) {
        Ok(c) => Ok(c),
        Err(_) => raise!(ValueError, "embedded null byte"),
    }
}

fn seq_items(o: &PyObject, what: &str) -> PyResult<Vec<PyRef>> {
    if list::is_list(o) || tuple::is_tuple(o) {
        return ab::collect_iterable(o);
    }
    match ab::sequence_list(o) {
        Ok(l) => Ok(list::list_to_vec(&l)),
        Err(e) => {
            if exc_matches(crate::types::exceptions::TypeError()) {
                clear_exc();
                raise!(TypeError, "{}", what);
            }
            Err(e)
        }
    }
}

fn int_of(o: &PyObject) -> PyResult<i64> {
    let v = ab::index(o)?;
    match int::as_i64(&v) {
        Some(x) => Ok(x),
        None => raise!(OverflowError, "Python int too large to convert to C long"),
    }
}

fn c_int_of(o: &PyObject) -> PyResult<c_int> {
    crate::modules::posix::path::c_int(o)
}

fn id_of(o: &PyObject, what: &str) -> PyResult<u32> {
    crate::modules::posix::path::id_arg(o, what)
}

/// Write the error report to the parent (async-signal-safe).
unsafe fn report(fd: c_int, err: i32, msg: &[u8], exc_name: &[u8]) {
    unsafe {
        if err != 0 {
            let _ = libc::write(fd, b"OSError:".as_ptr() as *const _, 8);
            let mut buf = [0u8; 16];
            let mut i = buf.len();
            let mut e = err as u32;
            while e != 0 && i > 0 {
                i -= 1;
                buf[i] = b"0123456789abcdef"[(e % 16) as usize];
                e /= 16;
            }
            let _ = libc::write(fd, buf[i..].as_ptr() as *const _, buf.len() - i);
            let _ = libc::write(fd, b":".as_ptr() as *const _, 1);
            let _ = libc::write(fd, msg.as_ptr() as *const _, msg.len());
        } else {
            let _ = libc::write(fd, exc_name.as_ptr() as *const _, exc_name.len());
            let _ = libc::write(fd, b":0:".as_ptr() as *const _, 3);
            let _ = libc::write(fd, msg.as_ptr() as *const _, msg.len());
        }
    }
}

unsafe fn set_inheritable(fd: c_int, inheritable: bool) -> c_int {
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFD);
        if fl < 0 {
            return -1;
        }
        let nfl = if inheritable { fl & !libc::FD_CLOEXEC } else { fl | libc::FD_CLOEXEC };
        if nfl != fl {
            return libc::fcntl(fd, libc::F_SETFD, nfl);
        }
        0
    }
}

/// Close every fd >= 3 not in `keep` (sorted).
unsafe fn close_open_fds(keep: &[c_int], maxfd: c_int) {
    unsafe {
        let mut start: c_int = 3;
        let mut ranges: Vec<(c_int, c_int)> = Vec::new();
        let _ = &mut ranges;
        for &k in keep.iter().chain(std::iter::once(&c_int::MAX)) {
            if k < start {
                continue;
            }
            if k > start {
                let end = if k == c_int::MAX { u32::MAX } else { (k - 1) as u32 };
                let r = libc::syscall(libc::SYS_close_range, start as u32, end, 0u32);
                if r != 0 {
                    let stop = if k == c_int::MAX { maxfd } else { k };
                    let mut fd = start;
                    while fd < stop {
                        libc::close(fd);
                        fd += 1;
                    }
                }
            }
            if k == c_int::MAX {
                break;
            }
            start = k + 1;
        }
    }
}

struct ChildArgs {
    argv: Vec<CString>,
    exec_list: Vec<CString>,
    envp: Option<Vec<CString>>,
    cwd: Option<CString>,
    close_fds: bool,
    keep: Vec<c_int>,
    p2cread: c_int,
    p2cwrite: c_int,
    c2pread: c_int,
    c2pwrite: c_int,
    errread: c_int,
    errwrite: c_int,
    errpipe_read: c_int,
    errpipe_write: c_int,
    restore_signals: bool,
    call_setsid: bool,
    pgid_to_set: libc::pid_t,
    gid: Option<libc::gid_t>,
    extra_groups: Option<Vec<libc::gid_t>>,
    uid: Option<libc::uid_t>,
    child_umask: c_int,
    preexec_fn: Option<PyRef>,
    maxfd: c_int,
}

/// Runs in the child; never returns.
unsafe fn child_exec(a: &ChildArgs, argv: &[*const libc::c_char], envp: Option<&[*const libc::c_char]>) -> ! {
    unsafe {
        let ew = a.errpipe_write;
        macro_rules! fail {
            ($msg:expr) => {{
                let e = errno();
                report(ew, e, $msg, b"OSError");
                libc::_exit(255);
            }};
        }
        for &fd in &a.keep {
            if fd == ew {
                continue;
            }
            if set_inheritable(fd, true) < 0 {
                fail!(b"noexec");
            }
        }
        for fd in [a.p2cwrite, a.c2pread, a.errread] {
            if fd != -1 {
                libc::close(fd);
            }
        }
        libc::close(a.errpipe_read);

        let mut c2pwrite = a.c2pwrite;
        let mut errwrite = a.errwrite;
        // make sure stdout/stderr targets are not overwritten by earlier dup2s
        if c2pwrite == 0 {
            c2pwrite = libc::dup(c2pwrite);
            if c2pwrite < 0 || set_inheritable(c2pwrite, false) < 0 {
                fail!(b"noexec");
            }
        }
        while errwrite == 0 || errwrite == 1 {
            errwrite = libc::dup(errwrite);
            if errwrite < 0 || set_inheritable(errwrite, false) < 0 {
                fail!(b"noexec");
            }
        }
        for (src, dst) in [(a.p2cread, 0), (c2pwrite, 1), (errwrite, 2)] {
            if src == dst {
                if set_inheritable(src, true) < 0 {
                    fail!(b"noexec");
                }
            } else if src != -1 {
                if libc::dup2(src, dst) < 0 {
                    fail!(b"noexec");
                }
            }
        }
        if let Some(cwd) = &a.cwd {
            if libc::chdir(cwd.as_ptr()) < 0 {
                fail!(b"noexec:chdir");
            }
        }
        if a.child_umask >= 0 {
            libc::umask(a.child_umask as libc::mode_t);
        }
        if a.restore_signals {
            for sig in [libc::SIGPIPE, libc::SIGXFSZ] {
                libc::signal(sig, libc::SIG_DFL);
            }
        }
        if a.call_setsid && libc::setsid() < 0 {
            fail!(b"noexec");
        }
        if a.pgid_to_set >= 0 && libc::setpgid(0, a.pgid_to_set) < 0 {
            fail!(b"noexec");
        }
        if let Some(g) = &a.extra_groups {
            if libc::setgroups(g.len(), g.as_ptr()) < 0 {
                fail!(b"noexec");
            }
        }
        if let Some(gid) = a.gid {
            if libc::setregid(gid, gid) < 0 {
                fail!(b"noexec");
            }
        }
        if let Some(uid) = a.uid {
            if libc::setreuid(uid, uid) < 0 {
                fail!(b"noexec");
            }
        }
        if let Some(f) = &a.preexec_fn {
            match ab::call(f, &[]) {
                Ok(_) => {}
                Err(_) => {
                    clear_exc();
                    report(ew, 0, b"Exception occurred in preexec_fn.", b"SubprocessError");
                    libc::_exit(255);
                }
            }
        }
        if a.close_fds {
            close_open_fds(&a.keep, a.maxfd);
        }
        let mut saved = 0;
        for exe in &a.exec_list {
            match envp {
                Some(e) => libc::execve(exe.as_ptr(), argv.as_ptr(), e.as_ptr()),
                None => libc::execv(exe.as_ptr(), argv.as_ptr()),
            };
            let e = errno();
            if e != libc::ENOENT && e != libc::ENOTDIR && saved == 0 {
                saved = e;
            }
        }
        let e = if saved != 0 { saved } else { errno() };
        report(ew, e, b"", b"OSError");
        libc::_exit(255);
    }
}

fn f_fork_exec(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.len() != 22 && args.len() != 23 {
        raise!(TypeError, "fork_exec expected 22 arguments, got {}", args.len());
    }
    let argv: Vec<CString> = if none::is_none(args[0]) {
        Vec::new()
    } else {
        if !(list::is_list(args[0]) || tuple::is_tuple(args[0])) {
            raise!(TypeError, "argv must be a tuple or list, not {}", tname(args[0]));
        }
        let mut v = Vec::new();
        for it in ab::collect_iterable(args[0])? {
            v.push(fs_convert(&it)?);
        }
        v
    };
    let mut exec_list = Vec::new();
    for it in seq_items(args[1], "executable_list must be a sequence")? {
        exec_list.push(fs_convert(&it)?);
    }
    let close_fds = ab::is_true(args[2])?;
    if !tuple::is_tuple(args[3]) {
        raise!(TypeError, "fork_exec() argument 4 must be tuple, not {}", tname(args[3]));
    }
    let mut keep: Vec<c_int> = Vec::new();
    let mut prev: i64 = -1;
    for it in tuple::tuple_items(args[3]) {
        let ok = int::is_int(it) && match int::as_i64(it) {
            Some(v) => v >= 0 && v <= c_int::MAX as i64 && v > prev,
            None => false,
        };
        if !ok {
            raise!(ValueError, "bad value(s) in fds_to_keep");
        }
        prev = int::as_i64(it).unwrap();
        keep.push(prev as c_int);
    }
    let cwd = if none::is_none(args[4]) { None } else { Some(fs_convert(args[4])?) };
    let envp: Option<Vec<CString>> = if none::is_none(args[5]) {
        None
    } else {
        let mut v = Vec::new();
        for it in seq_items(args[5], "env must be a sequence")? {
            v.push(fs_convert(&it)?);
        }
        Some(v)
    };
    let p2cread = c_int_of(args[6])?;
    let p2cwrite = c_int_of(args[7])?;
    let c2pread = c_int_of(args[8])?;
    let c2pwrite = c_int_of(args[9])?;
    let errread = c_int_of(args[10])?;
    let errwrite = c_int_of(args[11])?;
    let errpipe_read = c_int_of(args[12])?;
    let errpipe_write = c_int_of(args[13])?;
    let restore_signals = ab::is_true(args[14])?;
    let call_setsid = ab::is_true(args[15])?;
    let pgid_to_set = int_of(args[16])? as libc::pid_t;
    let gid = if none::is_none(args[17]) { None } else { Some(id_of(args[17], "gid")?) };
    let extra_groups = if none::is_none(args[18]) {
        None
    } else {
        if !list::is_list(args[18]) {
            raise!(TypeError, "extra_groups must be a list");
        }
        let mut v = Vec::new();
        for g in ab::collect_iterable(args[18])? {
            v.push(id_of(&g, "gid")?);
        }
        Some(v)
    };
    let uid = if none::is_none(args[19]) { None } else { Some(id_of(args[19], "uid")?) };
    let child_umask = c_int_of(args[20])?;
    let preexec_fn = if none::is_none(args[21]) { None } else { Some(args[21].to_ref()) };
    // (hang1) running Python code in a child forked from a finalizer is not
    // possible (the runtime is half torn down)
    if preexec_fn.is_some() && crate::modules::sys_full::is_finalizing() {
        raise!(PythonFinalizationError, "preexec_fn not supported at interpreter shutdown");
    }

    crate::modules::sys_full::audit(
        "_posixsubprocess.fork_exec",
        // (executable_list, args, env)
        &[args[1], args[0], args[5]],
    )?;

    let mut rl = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    let maxfd = if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) } == 0 && rl.rlim_cur != libc::RLIM_INFINITY {
        (rl.rlim_cur.min(1 << 20)) as c_int
    } else {
        256
    };
    let ca = ChildArgs {
        argv,
        exec_list,
        envp,
        cwd,
        close_fds,
        keep,
        p2cread,
        p2cwrite,
        c2pread,
        c2pwrite,
        errread,
        errwrite,
        errpipe_read,
        errpipe_write,
        restore_signals,
        call_setsid,
        pgid_to_set,
        gid,
        extra_groups,
        uid,
        child_umask,
        preexec_fn,
        maxfd,
    };
    let mut argv_p: Vec<*const libc::c_char> = ca.argv.iter().map(|c| c.as_ptr()).collect();
    argv_p.push(std::ptr::null());
    let envp_p: Option<Vec<*const libc::c_char>> = ca.envp.as_ref().map(|v| {
        let mut p: Vec<*const libc::c_char> = v.iter().map(|c| c.as_ptr()).collect();
        p.push(std::ptr::null());
        p
    });

    let has_preexec = ca.preexec_fn.is_some();
    if has_preexec {
        crate::modules::posix::proc::run_before_fork();
    }
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        if has_preexec {
            crate::modules::posix::after_fork_child_hook();
            crate::modules::posix::proc::run_after_fork_child();
        }
        unsafe { child_exec(&ca, &argv_p, envp_p.as_deref()) };
    }
    let e = errno();
    if has_preexec {
        crate::modules::posix::proc::run_after_fork_parent();
    }
    if pid < 0 {
        return Err(crate::types::exceptions::os_error_from_errno(e, None, None));
    }
    Ok(int::new_int(pid as i64))
}

pub fn init() -> PyResult {
    let m = crate::types::module::new_module("_posixsubprocess")?;
    let d = crate::types::module::module_dict(&m);
    dict::set_item_str(&d, "__doc__", &pystr::new_str("A POSIX helper for the subprocess module."))?;
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![(
            "fork_exec",
            FAST,
            f_fork_exec,
            "fork_exec($module, args, executable_list, close_fds, pass_fds, cwd,\n          env, p2cread, p2cwrite, c2pread, c2pwrite, errread,\n          errwrite, errpipe_read, errpipe_write, restore_signals,\n          call_setsid, pgid_to_set, gid, extra_groups, uid,\n          child_umask, preexec_fn, /)\n--\n\nSpawn a fresh new child process.\n\nFork a child process, close parent file descriptors as appropriate in the\nchild and duplicate the few that are needed before calling exec() in the\nchild process.\n\nIf close_fds is True, close file descriptors 3 and higher, except those listed\nin the sorted tuple pass_fds.\n\nThe preexec_fn, if supplied, will be called immediately before closing file\ndescriptors and exec.\n\nWARNING: preexec_fn is NOT SAFE if your application uses threads.\n         It may trigger infrequent, difficult to debug deadlocks.\n\nIf an error occurs in the child process before the exec, it is\nserialized and written to the errpipe_write fd per subprocess.py.\n\nReturns: the child process's PID.\n\nRaises: Only on an error in the parent process."
        )],
        Some(&m),
    )?;
    Ok(m)
}
