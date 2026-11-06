//! File and directory operations of the posix module.
use super::kwparse;
use super::path::*;
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::PyObject;
use crate::modules::bufview::BufView;
use crate::object::PyRef;
use crate::raise;
use crate::types::typeobject::tname;
use crate::types::{bool_, bytes, float, int, list, none, tuple};

fn ok_none() -> PyResult {
    Ok(none::none_ref())
}

// ------------------------------------------------------------------ descriptors

pub fn os_open(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [path, flags, mode, dfd] = kwparse::<4>("open", &["path", "flags", "mode", "dir_fd"], 0, 2, 1, args, kw)?;
    let p = super::path::path("open", path.unwrap())?;
    let flags = c_int(flags.unwrap())? | libc::O_CLOEXEC;
    let mode = match mode {
        Some(m) => c_int(m)?,
        None => 0o777,
    };
    let dfd = dir_fd(dfd)?;
    let fd = retry_eintr(-1, || unsafe {
        if dfd != libc::AT_FDCWD {
            libc::openat(dfd, p.ptr(), flags, mode as libc::c_uint)
        } else {
            libc::open(p.ptr(), flags, mode as libc::c_uint)
        }
    })?;
    if fd < 0 {
        return Err(os_err_with(&p));
    }
    Ok(int::new_int(fd as i64))
}

pub fn os_close(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [fd] = kwparse::<1>("close", &["fd"], 0, 1, 0, args, kw)?;
    let fd = c_int(fd.unwrap())?;
    let r = unsafe { libc::close(fd) };
    if r < 0 && errno() != libc::EINTR {
        return Err(os_err());
    }
    ok_none()
}

pub fn os_closerange(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("closerange", args, 2, 2)?;
    let lo = c_int(args[0])?.max(0);
    let hi = c_int(args[1])?;
    let mut fd = lo;
    while fd < hi {
        unsafe { libc::close(fd) };
        fd += 1;
    }
    ok_none()
}

pub fn os_dup(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("dup", args, 1, 1)?;
    let fd = c_int(args[0])?;
    let r = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if r < 0 {
        return Err(os_err());
    }
    Ok(int::new_int(r as i64))
}

pub fn os_dup2(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [fd, fd2, inh] = kwparse::<3>("dup2", &["fd", "fd2", "inheritable"], 0, 2, 0, args, kw)?;
    let fd = c_int(fd.unwrap())?;
    let fd2 = c_int(fd2.unwrap())?;
    let inheritable = truthy(inh, true)?;
    let r = if inheritable {
        unsafe { libc::dup2(fd, fd2) }
    } else if fd == fd2 {
        // dup3 fails with EINVAL when both are equal; emulate dup2 + set cloexec
        let r = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if r < 0 {
            -1
        } else {
            fd2
        }
    } else {
        unsafe { libc::dup3(fd, fd2, libc::O_CLOEXEC) }
    };
    if r < 0 {
        return Err(os_err());
    }
    Ok(int::new_int(r as i64))
}

pub fn os_read(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("read", args, 2, 2)?;
    let fd = c_int(args[0])?;
    let n = ssize(args[1])?;
    if n < 0 {
        return Err(crate::types::exceptions::os_error_from_errno(libc::EINVAL, None, None));
    }
    read_fd(fd, n as usize)
}

/// Read up to n bytes from fd (EINTR-retrying), returns bytes.
pub fn read_fd(fd: i32, n: usize) -> PyResult {
    let mut buf = crate::errors::try_zeroed_vec(n)?;
    let r = retry_eintr(-1, || unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, n) })?;
    if r < 0 {
        return Err(os_err());
    }
    Ok(bytes::new_bytes(&buf[..r as usize]))
}

pub fn os_readinto(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("readinto", args, 2, 2)?;
    let fd = c_int(args[0])?;
    let mut b = BufView::get(args[1], true)?;
    let s = b.as_mut_slice();
    let r = retry_eintr(-1, || unsafe { libc::read(fd, s.as_mut_ptr() as *mut libc::c_void, s.len()) })?;
    if r < 0 {
        return Err(os_err());
    }
    Ok(int::new_int(r as i64))
}

pub fn os_pread(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("pread", args, 3, 3)?;
    let fd = c_int(args[0])?;
    let n = ssize(args[1])?;
    let off = c_long(args[2])?;
    if n < 0 {
        return Err(crate::types::exceptions::os_error_from_errno(libc::EINVAL, None, None));
    }
    let mut buf = crate::errors::try_zeroed_vec(n as usize)?;
    let r = retry_eintr(-1, || unsafe { libc::pread(fd, buf.as_mut_ptr() as *mut libc::c_void, n as usize, off) })?;
    if r < 0 {
        return Err(os_err());
    }
    Ok(bytes::new_bytes(&buf[..r as usize]))
}

pub fn os_write(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("write", args, 2, 2)?;
    let fd = c_int(args[0])?;
    let b = BufView::get(args[1], false)?;
    let s = b.as_slice();
    let r = retry_eintr(-1, || unsafe { libc::write(fd, s.as_ptr() as *const libc::c_void, s.len()) })?;
    if r < 0 {
        return Err(os_err());
    }
    if (r as usize) < s.len() {
        // a write cut short by a signal: let its handler run (and raise) now
        check_signals()?;
    }
    Ok(int::new_int(r as i64))
}

pub fn os_pwrite(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("pwrite", args, 3, 3)?;
    let fd = c_int(args[0])?;
    let b = BufView::get(args[1], false)?;
    let off = c_long(args[2])?;
    let s = b.as_slice();
    let r = retry_eintr(-1, || unsafe { libc::pwrite(fd, s.as_ptr() as *const libc::c_void, s.len(), off) })?;
    if r < 0 {
        return Err(os_err());
    }
    Ok(int::new_int(r as i64))
}

fn collect_buffers(o: &PyObject, writable: bool) -> PyResult<Vec<BufView>> {
    if ab::get_iter(o).is_err() {
        clear_exc();
        raise!(TypeError, "buffers must be a sequence");
    }
    let items = ab::collect_iterable(o)?;
    let mut v = Vec::with_capacity(items.len());
    for it in &items {
        v.push(BufView::get(it, writable)?);
    }
    Ok(v)
}

pub fn os_writev(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("writev", args, 2, 2)?;
    let fd = c_int(args[0])?;
    let bufs = collect_buffers(args[1], false)?;
    let iov: Vec<libc::iovec> =
        bufs.iter().map(|b| libc::iovec { iov_base: b.as_slice().as_ptr() as *mut _, iov_len: b.len() }).collect();
    let r = retry_eintr(-1, || unsafe { libc::writev(fd, iov.as_ptr(), iov.len() as i32) })?;
    if r < 0 {
        return Err(os_err());
    }
    Ok(int::new_int(r as i64))
}

pub fn os_readv(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("readv", args, 2, 2)?;
    let fd = c_int(args[0])?;
    let mut bufs = collect_buffers(args[1], true)?;
    let iov: Vec<libc::iovec> =
        bufs.iter_mut().map(|b| libc::iovec { iov_base: b.as_mut_slice().as_mut_ptr() as *mut _, iov_len: b.len() }).collect();
    let r = retry_eintr(-1, || unsafe { libc::readv(fd, iov.as_ptr(), iov.len() as i32) })?;
    if r < 0 {
        return Err(os_err());
    }
    Ok(int::new_int(r as i64))
}

pub fn os_lseek(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("lseek", args, 3, 3)?;
    let fd = c_int(args[0])?;
    let pos = c_long(args[1])?;
    let how = c_int(args[2])?;
    let r = unsafe { libc::lseek(fd, pos, how) };
    if r < 0 {
        return Err(os_err());
    }
    Ok(int::new_int(r))
}

pub fn os_fsync(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [fd] = kwparse::<1>("fsync", &["fd"], 0, 1, 0, args, kw)?;
    let fd = as_file_descriptor(fd.unwrap())?;
    let r = retry_eintr(-1, || unsafe { libc::fsync(fd) })?;
    if r < 0 {
        return Err(os_err());
    }
    ok_none()
}

pub fn os_fdatasync(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [fd] = kwparse::<1>("fdatasync", &["fd"], 0, 1, 0, args, kw)?;
    let fd = as_file_descriptor(fd.unwrap())?;
    let r = retry_eintr(-1, || unsafe { libc::fdatasync(fd) })?;
    if r < 0 {
        return Err(os_err());
    }
    ok_none()
}

pub fn os_ftruncate(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("ftruncate", args, 2, 2)?;
    let fd = c_int(args[0])?;
    let len = c_long(args[1])?;
    let r = retry_eintr(-1, || unsafe { libc::ftruncate(fd, len) })?;
    if r < 0 {
        return Err(os_err());
    }
    ok_none()
}

pub fn os_truncate(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [path, length] = kwparse::<2>("truncate", &["path", "length"], 0, 2, 0, args, kw)?;
    let p = path_fd("truncate", path.unwrap())?;
    let len = c_long(length.unwrap())?;
    let r = match p.fd {
        Some(fd) => retry_eintr(-1, || unsafe { libc::ftruncate(fd, len) })?,
        None => unsafe { libc::truncate(p.ptr(), len) },
    };
    if r < 0 {
        return Err(os_err_with(&p));
    }
    ok_none()
}

pub fn os_posix_fallocate(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("posix_fallocate", args, 3, 3)?;
    let fd = c_int(args[0])?;
    let off = c_long(args[1])?;
    let len = c_long(args[2])?;
    loop {
        let r = unsafe { libc::posix_fallocate(fd, off, len) };
        if r == libc::EINTR {
            check_signals()?;
            continue;
        }
        if r != 0 {
            return Err(crate::types::exceptions::os_error_from_errno(r, None, None));
        }
        return ok_none();
    }
}

pub fn os_posix_fadvise(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("posix_fadvise", args, 4, 4)?;
    let fd = c_int(args[0])?;
    let off = c_long(args[1])?;
    let len = c_long(args[2])?;
    let adv = c_int(args[3])?;
    loop {
        let r = unsafe { libc::posix_fadvise(fd, off, len, adv) };
        if r == libc::EINTR {
            check_signals()?;
            continue;
        }
        if r != 0 {
            return Err(crate::types::exceptions::os_error_from_errno(r, None, None));
        }
        return ok_none();
    }
}

pub fn os_pipe(_m: &PyObject) -> PyResult {
    let mut fds = [0i32; 2];
    let r = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    if r < 0 {
        return Err(os_err());
    }
    Ok(tuple::new_tuple(vec![int::new_int(fds[0] as i64), int::new_int(fds[1] as i64)]))
}

pub fn os_pipe2(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("pipe2", args, 1, 1)?;
    let flags = c_int(args[0])?;
    let mut fds = [0i32; 2];
    let r = unsafe { libc::pipe2(fds.as_mut_ptr(), flags) };
    if r < 0 {
        return Err(os_err());
    }
    Ok(tuple::new_tuple(vec![int::new_int(fds[0] as i64), int::new_int(fds[1] as i64)]))
}

pub fn os_isatty(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("isatty", args, 1, 1)?;
    let fd = c_int(args[0])?;
    Ok(bool_::new_bool(unsafe { libc::isatty(fd) } == 1))
}

pub fn os_ttyname(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("ttyname", args, 1, 1)?;
    let fd = c_int(args[0])?;
    let mut buf = [0u8; 256];
    let r = unsafe { libc::ttyname_r(fd, buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if r != 0 {
        return Err(crate::types::exceptions::os_error_from_errno(r, None, None));
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    fs_decode(&buf[..n])
}

pub fn os_openpty(_m: &PyObject) -> PyResult {
    let mut master = 0;
    let mut slave = 0;
    let r = unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(), std::ptr::null()) };
    if r < 0 {
        return Err(os_err());
    }
    unsafe {
        set_inheritable_fd(master, false)?;
        set_inheritable_fd(slave, false)?;
    }
    Ok(tuple::new_tuple(vec![int::new_int(master as i64), int::new_int(slave as i64)]))
}

/// os.login_tty(fd): make `fd` the controlling terminal and the standard
/// streams of the calling process (in a new session), then close it.
pub fn os_login_tty(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("login_tty", args, 1, 1)?;
    let fd = c_int(args[0])?;
    unsafe {
        // become a session leader (fails harmlessly if we already are one)
        libc::setsid();
        if libc::ioctl(fd, libc::TIOCSCTTY as _, 0) < 0 {
            return Err(os_err());
        }
        for target in 0..3 {
            if fd != target && libc::dup2(fd, target) < 0 {
                return Err(os_err());
            }
        }
        if fd > 2 {
            libc::close(fd);
        }
    }
    ok_none()
}

/// posix._is_inputhook_installed(): whether PyOS_InputHook is set (e.g. by Tk).
pub fn os_is_inputhook_installed(_m: &PyObject) -> PyResult {
    Ok(bool_::new_bool(unsafe { (*(&raw const crate::capi::misc::PyOS_InputHook)).is_some() }))
}

/// posix._inputhook(): run PyOS_InputHook once (no-op when unset).
pub fn os_inputhook(_m: &PyObject) -> PyResult {
    let hook = unsafe { *(&raw const crate::capi::misc::PyOS_InputHook) };
    let r = match hook {
        Some(h) => unsafe { h() },
        None => 0,
    };
    Ok(int::new_int(r as i64))
}

pub unsafe fn set_inheritable_fd(fd: i32, inheritable: bool) -> PyResult<()> {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags < 0 {
            return Err(os_err());
        }
        let nf = if inheritable { flags & !libc::FD_CLOEXEC } else { flags | libc::FD_CLOEXEC };
        if nf != flags && libc::fcntl(fd, libc::F_SETFD, nf) < 0 {
            return Err(os_err());
        }
    }
    Ok(())
}

pub fn os_get_inheritable(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("get_inheritable", args, 1, 1)?;
    let fd = c_int(args[0])?;
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(os_err());
    }
    Ok(bool_::new_bool(flags & libc::FD_CLOEXEC == 0))
}

pub fn os_set_inheritable(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("set_inheritable", args, 2, 2)?;
    let fd = c_int(args[0])?;
    let inh = ab::is_true(args[1])?;
    unsafe { set_inheritable_fd(fd, inh)? };
    ok_none()
}

pub fn os_get_blocking(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("get_blocking", args, 1, 1)?;
    let fd = c_int(args[0])?;
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(os_err());
    }
    Ok(bool_::new_bool(flags & libc::O_NONBLOCK == 0))
}

pub fn set_blocking_fd(fd: i32, blocking: bool) -> PyResult<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(os_err());
    }
    let nf = if blocking { flags & !libc::O_NONBLOCK } else { flags | libc::O_NONBLOCK };
    if nf != flags && unsafe { libc::fcntl(fd, libc::F_SETFL, nf) } < 0 {
        return Err(os_err());
    }
    Ok(())
}

pub fn os_set_blocking(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("set_blocking", args, 2, 2)?;
    let fd = c_int(args[0])?;
    let b = ab::is_true(args[1])?;
    set_blocking_fd(fd, b)?;
    ok_none()
}

pub fn os_device_encoding(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [fd] = kwparse::<1>("device_encoding", &["fd"], 0, 1, 0, args, kw)?;
    let fd = c_int(fd.unwrap())?;
    if unsafe { libc::isatty(fd) } == 1 {
        return Ok(crate::types::str::new_str("UTF-8"));
    }
    ok_none()
}

pub fn os_lockf(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("lockf", args, 3, 3)?;
    let fd = c_int(args[0])?;
    let cmd = c_int(args[1])?;
    let len = c_long(args[2])?;
    let r = retry_eintr(-1, || unsafe { libc::lockf(fd, cmd, len) })?;
    if r < 0 {
        return Err(os_err());
    }
    ok_none()
}

pub fn os_sendfile(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [out_fd, in_fd, offset, count] = kwparse::<4>("sendfile", &["out_fd", "in_fd", "offset", "count"], 0, 4, 0, args, kw)?;
    let out_fd = c_int(out_fd.unwrap())?;
    let in_fd = c_int(in_fd.unwrap())?;
    let count = ssize(count.unwrap())?;
    let off = offset.unwrap();
    let r = if off.is(none::none()) {
        retry_eintr(-1, || unsafe { libc::sendfile(out_fd, in_fd, std::ptr::null_mut(), count as usize) })?
    } else {
        let mut o = c_long(off)?;
        retry_eintr(-1, || unsafe { libc::sendfile(out_fd, in_fd, &mut o, count as usize) })?
    };
    if r < 0 {
        return Err(os_err());
    }
    Ok(int::new_int(r as i64))
}

pub fn os_copy_file_range(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [src, dst, count, off_src, off_dst] =
        kwparse::<5>("copy_file_range", &["src", "dst", "count", "offset_src", "offset_dst"], 0, 3, 0, args, kw)?;
    let src = c_int(src.unwrap())?;
    let dst = c_int(dst.unwrap())?;
    let count = ssize(count.unwrap())?;
    if count < 0 {
        raise!(ValueError, "negative value not allowed");
    }
    let mut os_ = match off_src {
        Some(o) if !o.is(none::none()) => Some(c_long(o)?),
        _ => None,
    };
    let mut od = match off_dst {
        Some(o) if !o.is(none::none()) => Some(c_long(o)?),
        _ => None,
    };
    let r = retry_eintr(-1, || unsafe {
        libc::copy_file_range(
            src,
            os_.as_mut().map_or(std::ptr::null_mut(), |x| x as *mut i64),
            dst,
            od.as_mut().map_or(std::ptr::null_mut(), |x| x as *mut i64),
            count as usize,
            0,
        )
    })?;
    if r < 0 {
        return Err(os_err());
    }
    Ok(int::new_int(r as i64))
}

// ------------------------------------------------------------------ paths

pub fn os_mkdir(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [path, mode, dfd] = kwparse::<3>("mkdir", &["path", "mode", "dir_fd"], 0, 1, 1, args, kw)?;
    let p = super::path::path("mkdir", path.unwrap())?;
    let mode = match mode {
        Some(m) => c_int(m)?,
        None => 0o777,
    };
    let dfd = dir_fd(dfd)?;
    let r = unsafe {
        if dfd != libc::AT_FDCWD { libc::mkdirat(dfd, p.ptr(), mode as u32) } else { libc::mkdir(p.ptr(), mode as u32) }
    };
    if r < 0 {
        return Err(os_err_with(&p));
    }
    ok_none()
}

pub fn os_rmdir(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [path, dfd] = kwparse::<2>("rmdir", &["path", "dir_fd"], 0, 1, 1, args, kw)?;
    let p = super::path::path("rmdir", path.unwrap())?;
    let dfd = dir_fd(dfd)?;
    let r = unsafe {
        if dfd != libc::AT_FDCWD { libc::unlinkat(dfd, p.ptr(), libc::AT_REMOVEDIR) } else { libc::rmdir(p.ptr()) }
    };
    if r < 0 {
        return Err(os_err_with(&p));
    }
    ok_none()
}

fn do_unlink(fname: &str, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [path, dfd] = kwparse::<2>(fname, &["path", "dir_fd"], 0, 1, 1, args, kw)?;
    let p = super::path::path(fname, path.unwrap())?;
    let dfd = dir_fd(dfd)?;
    let r = unsafe { if dfd != libc::AT_FDCWD { libc::unlinkat(dfd, p.ptr(), 0) } else { libc::unlink(p.ptr()) } };
    if r < 0 {
        return Err(os_err_with(&p));
    }
    ok_none()
}
pub fn os_unlink(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    do_unlink("unlink", args, kw)
}
pub fn os_remove(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    do_unlink("remove", args, kw)
}

fn do_rename(fname: &str, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [src, dst, sfd, dfd] = kwparse::<4>(fname, &["src", "dst", "src_dir_fd", "dst_dir_fd"], 0, 2, 2, args, kw)?;
    let s = path_named(fname, "src", src.unwrap())?;
    let d = path_named(fname, "dst", dst.unwrap())?;
    if s.narrow != d.narrow {
        raise!(TypeError, "{}: src and dst must be the same type", fname);
    }
    let sfd = dir_fd(sfd)?;
    let dfd = dir_fd(dfd)?;
    let r = unsafe { libc::renameat(sfd, s.ptr(), dfd, d.ptr()) };
    if r < 0 {
        return Err(os_err_with2(&s, &d));
    }
    ok_none()
}
pub fn os_rename(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    do_rename("rename", args, kw)
}
pub fn os_replace(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    do_rename("replace", args, kw)
}

pub fn os_link(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [src, dst, sfd, dfd, follow] =
        kwparse::<5>("link", &["src", "dst", "src_dir_fd", "dst_dir_fd", "follow_symlinks"], 0, 2, 3, args, kw)?;
    let s = path_named("link", "src", src.unwrap())?;
    let d = path_named("link", "dst", dst.unwrap())?;
    if s.narrow != d.narrow {
        raise!(TypeError, "link: src and dst must be the same type");
    }
    let sfd = dir_fd(sfd)?;
    let dfd = dir_fd(dfd)?;
    let follow = truthy(follow, true)?;
    let r = unsafe { libc::linkat(sfd, s.ptr(), dfd, d.ptr(), if follow { libc::AT_SYMLINK_FOLLOW } else { 0 }) };
    if r < 0 {
        return Err(os_err_with2(&s, &d));
    }
    ok_none()
}

pub fn os_symlink(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [src, dst, _tid, dfd] =
        kwparse::<4>("symlink", &["src", "dst", "target_is_directory", "dir_fd"], 0, 2, 1, args, kw)?;
    let s = path_named("symlink", "src", src.unwrap())?;
    let d = path_named("symlink", "dst", dst.unwrap())?;
    if s.narrow != d.narrow {
        raise!(TypeError, "symlink: src and dst must be the same type");
    }
    let dfd = dir_fd(dfd)?;
    let r = unsafe { libc::symlinkat(s.ptr(), dfd, d.ptr()) };
    if r < 0 {
        return Err(os_err_with2(&s, &d));
    }
    ok_none()
}

pub fn readlink_bytes(p: &PathArg, dfd: i32) -> PyResult<Vec<u8>> {
    let mut cap = 256usize;
    loop {
        let mut buf = vec![0u8; cap];
        let n = unsafe {
            if dfd != libc::AT_FDCWD {
                libc::readlinkat(dfd, p.ptr(), buf.as_mut_ptr() as *mut libc::c_char, cap)
            } else {
                libc::readlink(p.ptr(), buf.as_mut_ptr() as *mut libc::c_char, cap)
            }
        };
        if n < 0 {
            return Err(os_err_with(p));
        }
        if (n as usize) < cap {
            buf.truncate(n as usize);
            return Ok(buf);
        }
        cap *= 4;
    }
}

pub fn os_readlink(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [path, dfd] = kwparse::<2>("readlink", &["path", "dir_fd"], 0, 1, 1, args, kw)?;
    let p = super::path::path("readlink", path.unwrap())?;
    let dfd = dir_fd(dfd)?;
    let b = readlink_bytes(&p, dfd)?;
    p.wrap(&b)
}

pub fn os_chmod(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [path, mode, dfd, follow] = kwparse::<4>("chmod", &["path", "mode", "dir_fd", "follow_symlinks"], 0, 2, 2, args, kw)?;
    let p = path_fd("chmod", path.unwrap())?;
    let mode = c_int(mode.unwrap())?;
    let dfd = dir_fd(dfd)?;
    let follow = truthy(follow, true)?;
    let r = if let Some(fd) = p.fd {
        if !follow {
            raise!(ValueError, "chmod: cannot use fd and follow_symlinks together");
        }
        retry_eintr(-1, || unsafe { libc::fchmod(fd, mode as u32) })?
    } else if dfd != libc::AT_FDCWD || !follow {
        let r = unsafe { libc::fchmodat(dfd, p.ptr(), mode as u32, if follow { 0 } else { libc::AT_SYMLINK_NOFOLLOW }) };
        if r < 0 && !follow && (errno() == libc::ENOTSUP || errno() == libc::EOPNOTSUPP) {
            raise!(NotImplementedError, "chmod: follow_symlinks unavailable on this platform");
        }
        r
    } else {
        unsafe { libc::chmod(p.ptr(), mode as u32) }
    };
    if r < 0 {
        return Err(os_err_with(&p));
    }
    ok_none()
}

pub fn os_fchmod(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [fd, mode] = kwparse::<2>("fchmod", &["fd", "mode"], 0, 2, 0, args, kw)?;
    let fd = c_int(fd.unwrap())?;
    let mode = c_int(mode.unwrap())?;
    let r = retry_eintr(-1, || unsafe { libc::fchmod(fd, mode as u32) })?;
    if r < 0 {
        return Err(os_err());
    }
    ok_none()
}

pub fn os_chown(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [path, uid, gid, dfd, follow] =
        kwparse::<5>("chown", &["path", "uid", "gid", "dir_fd", "follow_symlinks"], 0, 3, 2, args, kw)?;
    let p = path_fd("chown", path.unwrap())?;
    let uid = id_arg(uid.unwrap(), "uid")?;
    let gid = id_arg(gid.unwrap(), "gid")?;
    let dfd = dir_fd(dfd)?;
    let follow = truthy(follow, true)?;
    let r = if let Some(fd) = p.fd {
        retry_eintr(-1, || unsafe { libc::fchown(fd, uid, gid) })?
    } else if dfd != libc::AT_FDCWD || !follow {
        unsafe { libc::fchownat(dfd, p.ptr(), uid, gid, if follow { 0 } else { libc::AT_SYMLINK_NOFOLLOW }) }
    } else {
        unsafe { libc::chown(p.ptr(), uid, gid) }
    };
    if r < 0 {
        return Err(os_err_with(&p));
    }
    ok_none()
}

pub fn os_lchown(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [path, uid, gid] = kwparse::<3>("lchown", &["path", "uid", "gid"], 0, 3, 0, args, kw)?;
    let p = super::path::path("lchown", path.unwrap())?;
    let uid = id_arg(uid.unwrap(), "uid")?;
    let gid = id_arg(gid.unwrap(), "gid")?;
    if unsafe { libc::lchown(p.ptr(), uid, gid) } < 0 {
        return Err(os_err_with(&p));
    }
    ok_none()
}

pub fn os_fchown(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [fd, uid, gid] = kwparse::<3>("fchown", &["fd", "uid", "gid"], 0, 3, 0, args, kw)?;
    let fd = c_int(fd.unwrap())?;
    let uid = id_arg(uid.unwrap(), "uid")?;
    let gid = id_arg(gid.unwrap(), "gid")?;
    if retry_eintr(-1, || unsafe { libc::fchown(fd, uid, gid) })? < 0 {
        return Err(os_err());
    }
    ok_none()
}

fn time_to_timespec(o: &PyObject) -> PyResult<libc::timespec> {
    if float::is_float(o) {
        let f = float::float_value(o);
        if !f.is_finite() {
            raise!(ValueError, "Invalid value {} (type float)", f);
        }
        let sec = f.floor();
        let mut nsec = ((f - sec) * 1e9).round();
        let mut s = sec as i64;
        if nsec >= 1e9 {
            nsec -= 1e9;
            s += 1;
        }
        return Ok(libc::timespec { tv_sec: s, tv_nsec: nsec as i64 });
    }
    if int::is_int(o) {
        return Ok(libc::timespec { tv_sec: c_long(o)?, tv_nsec: 0 });
    }
    raise!(TypeError, "unsupported type for timestamp {}", tname(o).replace('\'', ""));
}

pub fn os_utime(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [path, times, ns, dfd, follow] =
        kwparse::<5>("utime", &["path", "times", "ns", "dir_fd", "follow_symlinks"], 0, 1, 3, args, kw)?;
    let p = path_fd("utime", path.unwrap())?;
    let times = times.filter(|t| !t.is(none::none()));
    let dfd = dir_fd(dfd)?;
    let follow = truthy(follow, true)?;
    if times.is_some() && ns.is_some() {
        raise!(ValueError, "utime: you may specify either 'times' or 'ns' but not both");
    }
    let ts: Option<[libc::timespec; 2]> = if let Some(t) = times {
        if !tuple::is_tuple(t) || tuple::tuple_len(t) != 2 {
            raise!(TypeError, "utime: 'times' must be either a tuple of two ints or None");
        }
        Some([time_to_timespec(tuple::tuple_get(t, 0))?, time_to_timespec(tuple::tuple_get(t, 1))?])
    } else if let Some(n) = ns {
        if !tuple::is_tuple(n) || tuple::tuple_len(n) != 2 {
            raise!(TypeError, "utime: 'ns' must be a tuple of two ints");
        }
        let mut out = [libc::timespec { tv_sec: 0, tv_nsec: 0 }; 2];
        for i in 0..2 {
            let v = tuple::tuple_get(n, i);
            if !int::is_int(v) {
                raise!(TypeError, "utime: 'ns' must be a tuple of two ints");
            }
            let total = ab::as_i128_index(v)?;
            out[i] = libc::timespec { tv_sec: total.div_euclid(1_000_000_000) as i64, tv_nsec: total.rem_euclid(1_000_000_000) as i64 };
        }
        Some(out)
    } else {
        None
    };
    let tsp = ts.as_ref().map_or(std::ptr::null(), |t| t.as_ptr());
    let r = if let Some(fd) = p.fd {
        if !follow {
            raise!(ValueError, "utime: cannot use fd and follow_symlinks together");
        }
        unsafe { libc::futimens(fd, tsp) }
    } else {
        unsafe { libc::utimensat(dfd, p.ptr(), tsp, if follow { 0 } else { libc::AT_SYMLINK_NOFOLLOW }) }
    };
    if r < 0 {
        return Err(os_err_with(&p));
    }
    ok_none()
}

pub fn os_access(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [path, mode, dfd, eff, follow] =
        kwparse::<5>("access", &["path", "mode", "dir_fd", "effective_ids", "follow_symlinks"], 0, 2, 3, args, kw)?;
    let p = super::path::path("access", path.unwrap())?;
    let mode = c_int(mode.unwrap())?;
    let dfd = dir_fd(dfd)?;
    let eff = truthy(eff, false)?;
    let follow = truthy(follow, true)?;
    let r = if dfd != libc::AT_FDCWD || eff || !follow {
        let mut flags = 0;
        if eff {
            flags |= libc::AT_EACCESS;
        }
        if !follow {
            flags |= libc::AT_SYMLINK_NOFOLLOW;
        }
        unsafe { libc::faccessat(dfd, p.ptr(), mode, flags) }
    } else {
        unsafe { libc::access(p.ptr(), mode) }
    };
    Ok(bool_::new_bool(r == 0))
}

pub fn os_chdir(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [path] = kwparse::<1>("chdir", &["path"], 0, 1, 0, args, kw)?;
    let p = path_fd("chdir", path.unwrap())?;
    let r = match p.fd {
        Some(fd) => unsafe { libc::fchdir(fd) },
        None => unsafe { libc::chdir(p.ptr()) },
    };
    if r < 0 {
        return Err(os_err_with(&p));
    }
    ok_none()
}

pub fn os_fchdir(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [fd] = kwparse::<1>("fchdir", &["fd"], 0, 1, 0, args, kw)?;
    let fd = as_file_descriptor(fd.unwrap())?;
    if retry_eintr(-1, || unsafe { libc::fchdir(fd) })? < 0 {
        return Err(os_err());
    }
    ok_none()
}

pub fn os_chroot(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [path] = kwparse::<1>("chroot", &["path"], 0, 1, 0, args, kw)?;
    let p = super::path::path("chroot", path.unwrap())?;
    if unsafe { libc::chroot(p.ptr()) } < 0 {
        return Err(os_err_with(&p));
    }
    ok_none()
}

pub fn getcwd_bytes() -> PyResult<Vec<u8>> {
    let mut cap = 1024usize;
    loop {
        let mut buf = vec![0u8; cap];
        let r = unsafe { libc::getcwd(buf.as_mut_ptr() as *mut libc::c_char, cap) };
        if r.is_null() {
            if errno() == libc::ERANGE {
                cap *= 4;
                continue;
            }
            return Err(os_err());
        }
        let n = buf.iter().position(|&c| c == 0).unwrap_or(cap);
        buf.truncate(n);
        return Ok(buf);
    }
}

pub fn os_getcwd(_m: &PyObject) -> PyResult {
    fs_decode(&getcwd_bytes()?)
}
pub fn os_getcwdb(_m: &PyObject) -> PyResult {
    Ok(bytes::new_bytes(&getcwd_bytes()?))
}

pub fn os_mkfifo(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [path, mode, dfd] = kwparse::<3>("mkfifo", &["path", "mode", "dir_fd"], 0, 1, 1, args, kw)?;
    let p = super::path::path("mkfifo", path.unwrap())?;
    let mode = match mode {
        Some(m) => c_int(m)?,
        None => 0o666,
    };
    let dfd = dir_fd(dfd)?;
    let r = retry_eintr(-1, || unsafe { libc::mkfifoat(dfd, p.ptr(), mode as u32) })?;
    if r < 0 {
        return Err(os_err_with(&p));
    }
    ok_none()
}

pub fn os_mknod(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [path, mode, device, dfd] = kwparse::<4>("mknod", &["path", "mode", "device", "dir_fd"], 0, 1, 1, args, kw)?;
    let p = super::path::path("mknod", path.unwrap())?;
    let mode = match mode {
        Some(m) => c_int(m)?,
        None => 0o600,
    };
    let dev = match device {
        Some(d) => c_u64(d)?,
        None => 0,
    };
    let dfd = dir_fd(dfd)?;
    let r = retry_eintr(-1, || unsafe { libc::mknodat(dfd, p.ptr(), mode as u32, dev) })?;
    if r < 0 {
        return Err(os_err_with(&p));
    }
    ok_none()
}

pub fn os_major(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("major", args, 1, 1)?;
    let d = c_u64(args[0])?;
    Ok(int::new_int(libc::major(d) as i64))
}
pub fn os_minor(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("minor", args, 1, 1)?;
    let d = c_u64(args[0])?;
    Ok(int::new_int(libc::minor(d) as i64))
}
pub fn os_makedev(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("makedev", args, 2, 2)?;
    let a = c_u64(args[0])?;
    let b = c_u64(args[1])?;
    if a > u32::MAX as u64 || b > u32::MAX as u64 {
        raise!(OverflowError, "Python int too large to convert to C unsigned int");
    }
    Ok(int::new_int_u128(libc::makedev(a as u32, b as u32) as u128))
}

pub fn os_sync(_m: &PyObject) -> PyResult {
    unsafe { libc::sync() };
    ok_none()
}

pub fn os_umask(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("umask", args, 1, 1)?;
    let m = c_int(args[0])?;
    let old = unsafe { libc::umask(m as u32) };
    Ok(int::new_int(old as i64))
}

/// Read all entry names of a directory (excluding . and ..).
pub fn list_dir_names(p: &PathArg) -> PyResult<Vec<Vec<u8>>> {
    unsafe {
        let dir = match p.fd {
            Some(fd) => {
                let fd2 = libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0);
                if fd2 < 0 {
                    return Err(os_err_with(p));
                }
                let d = libc::fdopendir(fd2);
                if d.is_null() {
                    let e = errno();
                    libc::close(fd2);
                    return Err(p.err(e));
                }
                libc::rewinddir(d);
                d
            }
            None => libc::opendir(p.ptr()),
        };
        if dir.is_null() {
            return Err(os_err_with(p));
        }
        let mut out = Vec::new();
        loop {
            *libc::__errno_location() = 0;
            let ent = libc::readdir64(dir);
            if ent.is_null() {
                let e = errno();
                if e != 0 {
                    if p.fd.is_some() {
                        libc::rewinddir(dir);
                    }
                    libc::closedir(dir);
                    return Err(p.err(e));
                }
                break;
            }
            let name = std::ffi::CStr::from_ptr((*ent).d_name.as_ptr()).to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            out.push(name.to_vec());
        }
        if p.fd.is_some() {
            libc::rewinddir(dir);
        }
        libc::closedir(dir);
        Ok(out)
    }
}

pub fn os_listdir(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [path] = kwparse::<1>("listdir", &["path"], 0, 0, 0, args, kw)?;
    let p = match path {
        None => PathArg::none(),
        Some(o) => path_converter(&PathSpec { func: "listdir", arg: "path", allow_fd: true, nullable: true }, o)?,
    };
    let names = list_dir_names(&p)?;
    let mut v: Vec<PyRef> = Vec::with_capacity(names.len());
    for n in names {
        v.push(if p.narrow { bytes::new_bytes(&n) } else { fs_decode(&n)? });
    }
    Ok(list::new_list(v))
}

pub fn os_fspath(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [path] = kwparse::<1>("fspath", &["path"], 0, 1, 0, args, kw)?;
    fspath(path.unwrap())
}

pub fn os_memfd_create(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [name, flags] = kwparse::<2>("memfd_create", &["name", "flags"], 0, 1, 0, args, kw)?;
    let n = super::path::path_named("memfd_create", "name", name.unwrap())?;
    let flags = match flags {
        Some(f) => c_int(f)? as u32,
        None => libc::MFD_CLOEXEC,
    };
    let fd = unsafe { libc::memfd_create(n.ptr(), flags) };
    if fd < 0 {
        return Err(os_err());
    }
    Ok(int::new_int(fd as i64))
}

pub fn os_eventfd(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [initval, flags] = kwparse::<2>("eventfd", &["initval", "flags"], 0, 1, 0, args, kw)?;
    let iv = c_u64(initval.unwrap())?;
    if iv > u32::MAX as u64 {
        raise!(OverflowError, "Python int too large to convert to C unsigned int");
    }
    let flags = match flags {
        Some(f) => c_int(f)?,
        None => libc::EFD_CLOEXEC,
    };
    let fd = unsafe { libc::eventfd(iv as u32, flags) };
    if fd < 0 {
        return Err(os_err());
    }
    Ok(int::new_int(fd as i64))
}

pub fn os_eventfd_read(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [fd] = kwparse::<1>("eventfd_read", &["fd"], 0, 1, 0, args, kw)?;
    let fd = as_file_descriptor(fd.unwrap())?;
    let mut v: u64 = 0;
    let r = retry_eintr(-1, || unsafe { libc::read(fd, &mut v as *mut u64 as *mut libc::c_void, 8) })?;
    if r < 0 {
        return Err(os_err());
    }
    Ok(int::new_int_u128(v as u128))
}

pub fn os_eventfd_write(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [fd, value] = kwparse::<2>("eventfd_write", &["fd", "value"], 0, 2, 0, args, kw)?;
    let fd = as_file_descriptor(fd.unwrap())?;
    let v = c_u64(value.unwrap())?;
    let r = retry_eintr(-1, || unsafe { libc::write(fd, &v as *const u64 as *const libc::c_void, 8) })?;
    if r < 0 {
        return Err(os_err());
    }
    ok_none()
}
