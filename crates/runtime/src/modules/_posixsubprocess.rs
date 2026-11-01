//! `_posixsubprocess.fork_exec`: fork a child, set up its file descriptors,
//! process attributes and exec the program. Everything the child needs is
//! prepared before fork(); the child only makes async-signal-safe calls
//! (except for the optional preexec_fn).

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::*;
use crate::raise;
use crate::types::{bytes, int, list, none, tuple};
use std::ffi::CString;

fn to_cstring_list(o: &PyObject, what: &str) -> PyResult<Vec<CString>> {
    let items = ab::collect_iterable(o).map_err(|e| {
        if exc_matches(crate::types::exceptions::TypeError()) {
            clear_exc();
            err!(TypeError, "expected a sequence for {}", what)
        } else {
            e
        }
    })?;
    let mut out = Vec::with_capacity(items.len());
    for it in items {
        let b = fs_encode(&it)?;
        if b.contains(&0) {
            raise!(ValueError, "embedded null byte");
        }
        out.push(CString::new(b).unwrap());
    }
    Ok(out)
}

/// os.fsencode for str/bytes/PathLike.
fn fs_encode(o: &PyObject) -> PyResult<Vec<u8>> {
    if bytes::is_bytes(o) {
        return Ok(bytes::bytes_data(o).to_vec());
    }
    if crate::types::str::is_str(o) {
        // UTF-8 with surrogateescape
        let d = crate::types::str::str_data(o);
        let mut out = Vec::new();
        for i in 0..d.len() {
            let c = d.get(i);
            if (0xdc80..0xdd00).contains(&c) {
                out.push((c - 0xdc00) as u8);
            } else if let Some(ch) = char::from_u32(c) {
                let mut b = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
            } else {
                return Err(crate::types::exceptions::unicode_encode_error("utf-8", o, i, i + 1, "surrogates not allowed"));
            }
        }
        return Ok(out);
    }
    if let Some(f) = ab::lookup_special(o, crate::name!("__fspath__"))? {
        let r = ab::call(&f, &[])?;
        return fs_encode(&r);
    }
    raise!(TypeError, "expected str, bytes or os.PathLike object, not {}", crate::types::typeobject::tname(o))
}

fn int_arg(o: &PyObject) -> PyResult<i64> {
    ab::as_i64(o)
}

fn optional_id(o: &PyObject) -> PyResult<Option<u32>> {
    if o.is(none::none()) {
        return Ok(None);
    }
    let v = ab::as_i64(o)?;
    if v < 0 {
        return Ok(None);
    }
    Ok(Some(v as u32))
}

fn write_all(fd: i32, mut b: &[u8]) {
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

/// Format "OSError:<hex errno>:<msg>" into a fixed buffer without allocating.
fn report_errno(fd: i32, e: i32, msg: &[u8]) {
    let mut buf = [0u8; 64];
    let mut n = 0;
    for &c in b"OSError:" {
        buf[n] = c;
        n += 1;
    }
    let hex = b"0123456789abcdef";
    let mut digits = [0u8; 16];
    let mut k = 0;
    let mut v = e as u32;
    if v == 0 {
        digits[0] = b'0';
        k = 1;
    }
    while v > 0 {
        digits[k] = hex[(v & 0xf) as usize];
        v >>= 4;
        k += 1;
    }
    while k > 0 {
        k -= 1;
        buf[n] = digits[k];
        n += 1;
    }
    buf[n] = b':';
    n += 1;
    for &c in msg {
        if n < buf.len() {
            buf[n] = c;
            n += 1;
        }
    }
    write_all(fd, &buf[..n]);
}

fn errno() -> i32 {
    unsafe { *libc::__errno_location() }
}

fn set_inheritable(fd: i32) -> bool {
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFD);
        if fl < 0 {
            return false;
        }
        if fl & libc::FD_CLOEXEC != 0 {
            return libc::fcntl(fd, libc::F_SETFD, fl & !libc::FD_CLOEXEC) == 0;
        }
        true
    }
}

fn close_fds_except(keep: &[i32]) {
    unsafe {
        // close every fd >= 3 not in keep (sorted)
        let mut start = 3;
        let mut kept: Vec<i32> = keep.iter().copied().filter(|&f| f >= 3).collect();
        kept.push(i32::MAX);
        for &k in &kept {
            if start < k {
                let end = if k == i32::MAX { u32::MAX } else { (k - 1) as u32 };
                let r = libc::syscall(libc::SYS_close_range, start as u32, end, 0u32);
                if r != 0 {
                    // fallback: close individually up to the soft limit
                    let max = libc::sysconf(libc::_SC_OPEN_MAX).max(256) as i64;
                    let hi = if k == i32::MAX { max } else { k as i64 };
                    for fd in start as i64..hi {
                        libc::close(fd as i32);
                    }
                }
            }
            if k == i32::MAX {
                break;
            }
            start = k + 1;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn fork_exec(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.len() < 22 || args.len() > 23 {
        raise!(TypeError, "fork_exec() expected 22 arguments, got {}", args.len());
    }
    let argv_o = args[0];
    let exe_o = args[1];
    let close_fds = ab::is_true(args[2])?;
    let keep_o = args[3];
    let cwd_o = args[4];
    let env_o = args[5];
    let p2cread = int_arg(args[6])? as i32;
    let p2cwrite = int_arg(args[7])? as i32;
    let c2pread = int_arg(args[8])? as i32;
    let mut c2pwrite = int_arg(args[9])? as i32;
    let errread = int_arg(args[10])? as i32;
    let mut errwrite = int_arg(args[11])? as i32;
    let errpipe_read = int_arg(args[12])? as i32;
    let errpipe_write = int_arg(args[13])? as i32;
    let restore_signals = ab::is_true(args[14])?;
    let call_setsid = ab::is_true(args[15])?;
    let pgid_to_set = int_arg(args[16])?;
    let gid = optional_id(args[17])?;
    let extra_groups_o = args[18];
    let uid = optional_id(args[19])?;
    let child_umask = int_arg(args[20])?;
    let preexec_fn = args[21];

    if !tuple::is_tuple(keep_o) {
        raise!(TypeError, "fds_to_keep must be a tuple");
    }
    let mut keep: Vec<i32> = Vec::new();
    let mut prev = -1i64;
    for it in tuple::tuple_items(keep_o) {
        let ok = int::is_int(it) && int::as_i64(it).map_or(false, |v| v >= 0 && v <= i32::MAX as i64 && v > prev);
        if !ok {
            raise!(ValueError, "bad value(s) in fds_to_keep");
        }
        prev = int::as_i64(it).unwrap();
        keep.push(prev as i32);
    }
    let argv = if argv_o.is(none::none()) { None } else { Some(to_cstring_list(argv_o, "args")?) };
    let exes = to_cstring_list(exe_o, "executable_list")?;
    let cwd = if cwd_o.is(none::none()) {
        None
    } else {
        let b = fs_encode(cwd_o)?;
        if b.contains(&0) {
            raise!(ValueError, "embedded null byte");
        }
        Some(CString::new(b).unwrap())
    };
    let env = if env_o.is(none::none()) { None } else { Some(to_cstring_list(env_o, "env")?) };
    let extra_groups: Option<Vec<libc::gid_t>> = if extra_groups_o.is(none::none()) {
        None
    } else {
        if !list::is_list(extra_groups_o) {
            raise!(TypeError, "setgroups argument must be a list");
        }
        let mut v = Vec::new();
        for g in list::list_items(extra_groups_o) {
            v.push(ab::as_i64(g)? as libc::gid_t);
        }
        Some(v)
    };
    let has_preexec = !preexec_fn.is(none::none());
    if has_preexec && !ab::is_callable(preexec_fn) {
        raise!(TypeError, "preexec_fn must be callable");
    }

    // NULL-terminated pointer arrays (built before fork)
    let argv_ptrs: Option<Vec<*const libc::c_char>> =
        argv.as_ref().map(|a| a.iter().map(|s| s.as_ptr()).chain(std::iter::once(std::ptr::null())).collect());
    let env_ptrs: Option<Vec<*const libc::c_char>> =
        env.as_ref().map(|a| a.iter().map(|s| s.as_ptr()).chain(std::iter::once(std::ptr::null())).collect());

    if has_preexec {
        crate::modules::posix::proc::run_before_fork();
    }
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        // ---------------- child
        unsafe {
            if has_preexec {
                crate::modules::posix::after_fork_child_hook();
                crate::modules::posix::proc::run_after_fork_child();
            }
            for fd in [p2cwrite, c2pread, errread] {
                if fd != -1 {
                    libc::close(fd);
                }
            }
            if errpipe_read != -1 {
                libc::close(errpipe_read);
            }
            // make sure the std fds are not clobbered by the dup2 calls below
            if c2pwrite == 0 {
                c2pwrite = libc::dup(c2pwrite);
                if c2pwrite < 0 || !set_inheritable(c2pwrite) {
                    report_errno(errpipe_write, errno(), b"noexec");
                    libc::_exit(255);
                }
            }
            while errwrite == 0 || errwrite == 1 {
                errwrite = libc::dup(errwrite);
                if errwrite < 0 || !set_inheritable(errwrite) {
                    report_errno(errpipe_write, errno(), b"noexec");
                    libc::_exit(255);
                }
            }
            for (fd, target) in [(p2cread, 0), (c2pwrite, 1), (errwrite, 2)] {
                if fd == target {
                    if !set_inheritable(fd) {
                        report_errno(errpipe_write, errno(), b"noexec");
                        libc::_exit(255);
                    }
                } else if fd != -1 && libc::dup2(fd, target) < 0 {
                    report_errno(errpipe_write, errno(), b"noexec");
                    libc::_exit(255);
                }
            }
            if let Some(c) = &cwd {
                if libc::chdir(c.as_ptr()) < 0 {
                    report_errno(errpipe_write, errno(), b"noexec:chdir");
                    libc::_exit(255);
                }
            }
            if child_umask >= 0 {
                libc::umask(child_umask as libc::mode_t);
            }
            if restore_signals {
                libc::signal(libc::SIGPIPE, libc::SIG_DFL);
                libc::signal(libc::SIGXFSZ, libc::SIG_DFL);
            }
            if call_setsid && libc::setsid() < 0 {
                report_errno(errpipe_write, errno(), b"noexec");
                libc::_exit(255);
            }
            if pgid_to_set >= 0 && libc::setpgid(0, pgid_to_set as libc::pid_t) < 0 {
                report_errno(errpipe_write, errno(), b"noexec");
                libc::_exit(255);
            }
            if let Some(g) = &extra_groups {
                if libc::setgroups(g.len(), g.as_ptr()) < 0 {
                    report_errno(errpipe_write, errno(), b"noexec");
                    libc::_exit(255);
                }
            }
            if let Some(g) = gid {
                if libc::setregid(g, g) < 0 {
                    report_errno(errpipe_write, errno(), b"noexec");
                    libc::_exit(255);
                }
            }
            if let Some(u) = uid {
                if libc::setreuid(u, u) < 0 {
                    report_errno(errpipe_write, errno(), b"noexec");
                    libc::_exit(255);
                }
            }
            if has_preexec {
                if ab::call(preexec_fn, &[]).is_err() {
                    clear_exc();
                    write_all(errpipe_write, b"SubprocessError:0:Exception occurred in preexec_fn.");
                    libc::_exit(255);
                }
            }
            if close_fds {
                let mut k = keep.clone();
                k.push(errpipe_write);
                k.sort();
                close_fds_except(&k);
            }
            let mut saved = 0;
            if let Some(av) = &argv_ptrs {
                for exe in &exes {
                    match &env_ptrs {
                        Some(ep) => libc::execve(exe.as_ptr(), av.as_ptr(), ep.as_ptr()),
                        None => libc::execv(exe.as_ptr(), av.as_ptr()),
                    };
                    let e = errno();
                    if e != libc::ENOENT && e != libc::ENOTDIR && saved == 0 {
                        saved = e;
                    }
                }
            }
            let e = if saved != 0 { saved } else { errno() };
            report_errno(errpipe_write, e, b"");
            libc::_exit(255);
        }
    }
    // ---------------- parent
    let e = errno();
    if has_preexec {
        crate::modules::posix::proc::run_after_fork_parent();
    }
    drop(argv_ptrs);
    drop(env_ptrs);
    if pid < 0 {
        return Err(crate::types::exceptions::os_error_from_errno(e, None, None));
    }
    Ok(int::new_int(pid as i64))
}

pub fn init_module() -> PyResult {
    let m = crate::types::module::new_module("_posixsubprocess")?;
    let d = crate::types::module::module_dict(&m);
    crate::builtins::add_methods_to_dict(&d, crate::mdefs![("fork_exec", FAST, fork_exec)], Some(&m))?;
    Ok(m)
}
