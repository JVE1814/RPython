//! stat / lstat / fstat / statvfs and their result types.
use super::path::*;
use super::{kwparse, typeptr};
use crate::errors::*;
use crate::ffi::{PyObject, PyTypeObject};
use crate::object::PyRef;
use crate::raise;
use crate::types::structseq::{self, Desc, Field};
use crate::types::{float, int, none};
use std::sync::atomic::{AtomicPtr, Ordering};

pub static STAT_RESULT: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());
pub static STATVFS_RESULT: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());

const STAT_FIELDS: &[Field] = &[
    Field::new("st_mode", "protection bits"),
    Field::new("st_ino", "inode"),
    Field::new("st_dev", "device"),
    Field::new("st_nlink", "number of hard links"),
    Field::new("st_uid", "user ID of owner"),
    Field::new("st_gid", "group ID of owner"),
    Field::new("st_size", "total size, in bytes"),
    Field::unnamed("integer time of last access"),
    Field::unnamed("integer time of last modification"),
    Field::unnamed("integer time of last change"),
    Field::new("st_atime", "time of last access"),
    Field::new("st_mtime", "time of last modification"),
    Field::new("st_ctime", "time of last change"),
    Field::new("st_atime_ns", "time of last access in nanoseconds"),
    Field::new("st_mtime_ns", "time of last modification in nanoseconds"),
    Field::new("st_ctime_ns", "time of last change in nanoseconds"),
    Field::new("st_blksize", "blocksize for filesystem I/O"),
    Field::new("st_blocks", "number of blocks allocated"),
    Field::new("st_rdev", "device type (if inode device)"),
];

const STATVFS_FIELDS: &[Field] = &[
    Field::new("f_bsize", ""),
    Field::new("f_frsize", ""),
    Field::new("f_blocks", ""),
    Field::new("f_bfree", ""),
    Field::new("f_bavail", ""),
    Field::new("f_files", ""),
    Field::new("f_ffree", ""),
    Field::new("f_favail", ""),
    Field::new("f_flag", ""),
    Field::new("f_namemax", ""),
    Field::new("f_fsid", ""),
];

pub fn init_types() -> PyResult<()> {
    if STAT_RESULT.load(Ordering::Acquire).is_null() {
        let t = structseq::new_type(&Desc {
            name: "os.stat_result",
            doc: "stat_result: Result from stat, fstat, or lstat.\n\nThis object may be accessed either as a tuple of\n  (mode, ino, dev, nlink, uid, gid, size, atime, mtime, ctime)\nor via the attributes st_mode, st_ino, st_dev, st_nlink, st_uid, and so on.\n\nPosix/windows: If your platform supports st_blksize, st_blocks, st_rdev,\nor st_flags, they are available as attributes only.\n\nSee os.stat for more information.",
            fields: STAT_FIELDS,
            n_in_sequence: 10,
        })?;
        STAT_RESULT.store(t, Ordering::Release);
        let t = structseq::new_type(&Desc {
            name: "os.statvfs_result",
            doc: "statvfs_result: Result from statvfs or fstatvfs.\n\nThis object may be accessed either as a tuple of\n  (bsize, frsize, blocks, bfree, bavail, files, ffree, favail, flag, namemax),\nor via the attributes f_bsize, f_frsize, f_blocks, f_bfree, and so on.\n\nSee os.statvfs for more information.",
            fields: STATVFS_FIELDS,
            n_in_sequence: 10,
        })?;
        STATVFS_RESULT.store(t, Ordering::Release);
    }
    Ok(())
}

fn time_parts(sec: i64, nsec: i64) -> (PyRef, PyRef, PyRef) {
    let i = int::new_int(sec);
    let f = float::new_float(sec as f64 + nsec as f64 * 1e-9);
    let ns = int::new_int_i128(sec as i128 * 1_000_000_000 + nsec as i128);
    (i, f, ns)
}

pub fn stat_result_from(st: &libc::stat) -> PyRef {
    let (ai, af, ans) = time_parts(st.st_atime, st.st_atime_nsec);
    let (mi, mf, mns) = time_parts(st.st_mtime, st.st_mtime_nsec);
    let (ci, cf, cns) = time_parts(st.st_ctime, st.st_ctime_nsec);
    let vals = vec![
        int::new_int(st.st_mode as i64),
        int::new_int_u128(st.st_ino as u128),
        int::new_int_u128(st.st_dev as u128),
        int::new_int(st.st_nlink as i64),
        int::new_int(st.st_uid as i64),
        int::new_int(st.st_gid as i64),
        int::new_int(st.st_size),
        ai,
        mi,
        ci,
        af,
        mf,
        cf,
        ans,
        mns,
        cns,
        int::new_int(st.st_blksize),
        int::new_int(st.st_blocks),
        int::new_int_u128(st.st_rdev as u128),
    ];
    structseq::new_instance(typeptr(&STAT_RESULT), vals)
}

/// Core stat used by stat/lstat and DirEntry.
pub fn do_stat(func: &str, p: &PathArg, dir_fd: i32, follow: bool) -> PyResult<libc::stat> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if let Some(fd) = p.fd {
        if !follow {
            raise!(ValueError, "{}: cannot use fd and follow_symlinks together", func);
        }
        if dir_fd != libc::AT_FDCWD {
            raise!(ValueError, "{}: can't specify both dir_fd and fd", func);
        }
        let r = retry_eintr(-1, || unsafe { libc::fstat(fd, &mut st) })?;
        if r != 0 {
            return Err(os_err_with(p));
        }
        return Ok(st);
    }
    let r = if dir_fd != libc::AT_FDCWD || !follow {
        let flags = if follow { 0 } else { libc::AT_SYMLINK_NOFOLLOW };
        unsafe { libc::fstatat(dir_fd, p.ptr(), &mut st, flags) }
    } else {
        unsafe { libc::stat(p.ptr(), &mut st) }
    };
    if r != 0 {
        return Err(os_err_with(p));
    }
    Ok(st)
}

pub fn os_stat(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [path, dfd, follow] = kwparse::<3>("stat", &["path", "dir_fd", "follow_symlinks"], 0, 1, 2, args, kw)?;
    let p = path_fd("stat", path.unwrap())?;
    let dfd = dir_fd(dfd)?;
    let follow = truthy(follow, true)?;
    let st = do_stat("stat", &p, dfd, follow)?;
    Ok(stat_result_from(&st))
}

pub fn os_lstat(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [pth, dfd] = kwparse::<2>("lstat", &["path", "dir_fd"], 0, 1, 1, args, kw)?;
    let p = path("lstat", pth.unwrap())?;
    let dfd = dir_fd(dfd)?;
    let st = do_stat("lstat", &p, dfd, false)?;
    Ok(stat_result_from(&st))
}

pub fn fstat_fd(fd: i32) -> PyResult<libc::stat> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let r = retry_eintr(-1, || unsafe { libc::fstat(fd, &mut st) })?;
    if r != 0 {
        return Err(os_err());
    }
    Ok(st)
}

pub fn os_fstat(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [fd] = kwparse::<1>("fstat", &["fd"], 0, 1, 0, args, kw)?;
    let fd = c_int(fd.unwrap())?;
    Ok(stat_result_from(&fstat_fd(fd)?))
}

fn statvfs_result_from(st: &libc::statvfs) -> PyRef {
    let vals = vec![
        int::new_int(st.f_bsize as i64),
        int::new_int(st.f_frsize as i64),
        int::new_int_u128(st.f_blocks as u128),
        int::new_int_u128(st.f_bfree as u128),
        int::new_int_u128(st.f_bavail as u128),
        int::new_int_u128(st.f_files as u128),
        int::new_int_u128(st.f_ffree as u128),
        int::new_int_u128(st.f_favail as u128),
        int::new_int(st.f_flag as i64),
        int::new_int(st.f_namemax as i64),
        int::new_int_u128(st.f_fsid as u128),
    ];
    structseq::new_instance(typeptr(&STATVFS_RESULT), vals)
}

pub fn os_statvfs(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [path] = kwparse::<1>("statvfs", &["path"], 0, 1, 0, args, kw)?;
    let p = path_fd("statvfs", path.unwrap())?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let r = match p.fd {
        Some(fd) => retry_eintr(-1, || unsafe { libc::fstatvfs(fd, &mut st) })?,
        None => unsafe { libc::statvfs(p.ptr(), &mut st) },
    };
    if r != 0 {
        return Err(os_err_with(&p));
    }
    Ok(statvfs_result_from(&st))
}

pub fn os_fstatvfs(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("fstatvfs", args, 1, 1)?;
    let fd = c_int(args[0])?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let r = retry_eintr(-1, || unsafe { libc::fstatvfs(fd, &mut st) })?;
    if r != 0 {
        return Err(os_err());
    }
    Ok(statvfs_result_from(&st))
}

pub fn _unused() -> PyRef {
    none::none_ref()
}
