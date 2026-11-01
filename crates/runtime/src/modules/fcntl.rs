//! The `fcntl` module (Linux): fcntl, ioctl, flock, lockf.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::posix::path::{as_file_descriptor, check_signals, errno, os_err};
use crate::object::*;
use crate::types::typeobject::tname;
use crate::types::{bytes, dict, int, none, str as pystr};
use crate::{mdefs, raise};

/// Guard bytes around the copied argument buffer (detects kernel overruns).
const GUARD: usize = 64;

/// Call `f` retrying on EINTR (checking signals in between); thread state released.
fn retry(mut f: impl FnMut() -> libc::c_int) -> PyResult<libc::c_int> {
    loop {
        let r = crate::threads::blocking(&mut f);
        if r == -1 && errno() == libc::EINTR {
            check_signals()?;
            continue;
        }
        if r == -1 {
            return Err(os_err());
        }
        return Ok(r);
    }
}

/// An argument that is either an int or a buffer (str is encoded as UTF-8).
enum Arg {
    Int(libc::c_long),
    Buf(Vec<u8>),
}

fn int_arg(o: &PyObject, unsigned_ok: bool) -> PyResult<libc::c_long> {
    if crate::types::float::is_float(o) {
        raise!(TypeError, "'float' object cannot be interpreted as an integer");
    }
    let i = ab::index(o)?;
    match crate::modules::devtools::int_as_i128(&i) {
        Some(v) if v >= i32::MIN as i128 && v <= i32::MAX as i128 => Ok(v as libc::c_long),
        Some(v) if unsigned_ok && v >= 0 && v <= u32::MAX as i128 => Ok(v as u32 as i32 as libc::c_long),
        Some(v) if v > 0 => raise!(OverflowError, "signed integer is greater than maximum"),
        _ => {
            if int::sign(&i) < 0 {
                raise!(OverflowError, "signed integer is less than minimum")
            }
            raise!(OverflowError, "signed integer is greater than maximum")
        }
    }
}

fn parse_arg(o: Option<&PyObject>, fname: &str) -> PyResult<Arg> {
    let o = match o {
        None => return Ok(Arg::Int(0)),
        Some(o) => o,
    };
    if pystr::is_str(o) {
        return Ok(Arg::Buf(pystr::as_utf8(o)?.as_bytes().to_vec()));
    }
    if crate::buffer::check_buffer(o) {
        return Ok(Arg::Buf(crate::buffer::with_buffer(o, |b| b.to_vec())?));
    }
    if ab::has_index(o) && !crate::types::float::is_float(o) {
        return Ok(Arg::Int(int_arg(o, true)?));
    }
    raise!(TypeError, "{}() argument 3 must be an integer, a bytes-like object, or a string, not {}", fname, tname(o))
}

fn guarded(data: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; data.len() + GUARD];
    buf[..data.len()].copy_from_slice(data);
    for b in &mut buf[data.len()..] {
        *b = 0;
    }
    buf
}

fn fcntl_fcntl(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("fcntl", args, 2, 3)?;
    let fd = as_file_descriptor(args[0])?;
    let cmd = int_arg(args[1], false)? as libc::c_int;
    crate::modules::sys_full::audit("fcntl.fcntl", &[&int::new_int(fd as i64), &int::new_int(cmd as i64)])?;
    match parse_arg(args.get(2).copied(), "fcntl")? {
        Arg::Int(v) => {
            let r = retry(|| unsafe { libc::fcntl(fd, cmd, v) })?;
            Ok(int::new_int(r as i64))
        }
        Arg::Buf(data) => {
            let mut buf = guarded(&data);
            retry(|| unsafe { libc::fcntl(fd, cmd, buf.as_mut_ptr()) })?;
            Ok(bytes::new_bytes(&buf[..data.len()]))
        }
    }
}

fn fcntl_ioctl(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("ioctl", args, 2, 4)?;
    let fd = as_file_descriptor(args[0])?;
    let req = {
        let i = ab::index(args[1])?;
        match crate::modules::devtools::int_as_i128(&i) {
            Some(v) if v >= i64::MIN as i128 && v <= u64::MAX as i128 => v as u64 as libc::c_ulong,
            _ => raise!(OverflowError, "Python int too large to convert to C unsigned long"),
        }
    };
    let mutate = match args.get(3) {
        Some(m) => ab::is_true(m)?,
        None => true,
    };
    crate::modules::sys_full::audit("fcntl.ioctl", &[&int::new_int(fd as i64), &int::new_int(req as i64)])?;
    if let Some(&a) = args.get(2) {
        // writable buffer: operate in place when mutate_flag is true
        if crate::buffer::check_buffer(a) && !pystr::is_str(a) {
            let mut view = crate::buffer::empty_view();
            let writable = crate::buffer::get_buffer(a, &mut view, crate::buffer::PyBUF_WRITABLE).is_ok();
            if !writable {
                clear_exc();
            }
            if writable {
                let len = view.len as usize;
                let ptr = view.buf as *mut u8;
                let data = unsafe { std::slice::from_raw_parts(ptr, len).to_vec() };
                let mut buf = guarded(&data);
                let r = retry(|| unsafe { libc::ioctl(fd, req as _, buf.as_mut_ptr()) });
                let result = match r {
                    Ok(rv) => {
                        if mutate {
                            unsafe { std::ptr::copy_nonoverlapping(buf.as_ptr(), ptr, len) };
                            Ok(int::new_int(rv as i64))
                        } else {
                            Ok(bytes::new_bytes(&buf[..len]))
                        }
                    }
                    Err(e) => Err(e),
                };
                unsafe { crate::buffer::release_buffer(&mut view) };
                return result;
            }
        }
    }
    match parse_arg(args.get(2).copied(), "ioctl")? {
        Arg::Int(v) => {
            let r = retry(|| unsafe { libc::ioctl(fd, req as _, v) })?;
            Ok(int::new_int(r as i64))
        }
        Arg::Buf(data) => {
            let mut buf = guarded(&data);
            retry(|| unsafe { libc::ioctl(fd, req as _, buf.as_mut_ptr()) })?;
            Ok(bytes::new_bytes(&buf[..data.len()]))
        }
    }
}

fn fcntl_flock(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("flock", args, 2, 2)?;
    let fd = as_file_descriptor(args[0])?;
    let op = int_arg(args[1], false)? as libc::c_int;
    crate::modules::sys_full::audit("fcntl.flock", &[&int::new_int(fd as i64), &int::new_int(op as i64)])?;
    retry(|| unsafe { libc::flock(fd, op) })?;
    Ok(none::none_ref())
}

fn off_arg(o: Option<&PyObject>) -> PyResult<i64> {
    match o {
        None => Ok(0),
        Some(v) => {
            if crate::types::float::is_float(v) {
                raise!(TypeError, "'float' object cannot be interpreted as an integer");
            }
            ab::as_i64(v)
        }
    }
}

fn fcntl_lockf(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("lockf", args, 2, 5)?;
    let fd = as_file_descriptor(args[0])?;
    let code = int_arg(args[1], false)? as libc::c_int;
    let len = off_arg(args.get(2).copied())?;
    let start = off_arg(args.get(3).copied())?;
    let whence = match args.get(4) {
        Some(w) => int_arg(w, false)? as libc::c_int,
        None => 0,
    };
    crate::modules::sys_full::audit(
        "fcntl.lockf",
        &[&int::new_int(fd as i64), &int::new_int(code as i64), &int::new_int(len), &int::new_int(start), &int::new_int(whence as i64)],
    )?;
    let ltype = if code == libc::LOCK_UN {
        libc::F_UNLCK
    } else if code & libc::LOCK_SH != 0 {
        libc::F_RDLCK
    } else if code & libc::LOCK_EX != 0 {
        libc::F_WRLCK
    } else {
        raise!(ValueError, "unrecognized lockf argument");
    };
    let mut l: libc::flock = unsafe { std::mem::zeroed() };
    l.l_type = ltype as _;
    l.l_whence = whence as _;
    l.l_start = start as _;
    l.l_len = len as _;
    let cmd = if code & libc::LOCK_NB != 0 { libc::F_SETLK } else { libc::F_SETLKW };
    retry(|| unsafe { libc::fcntl(fd, cmd, &mut l as *mut libc::flock) })?;
    Ok(none::none_ref())
}

const CONSTANTS: &[(&str, i64)] = &[
    ("LOCK_SH", 1),
    ("LOCK_EX", 2),
    ("LOCK_NB", 4),
    ("LOCK_UN", 8),
    ("LOCK_MAND", 32),
    ("LOCK_READ", 64),
    ("LOCK_WRITE", 128),
    ("LOCK_RW", 192),
    ("F_DUPFD", 0),
    ("F_DUPFD_CLOEXEC", 1030),
    ("F_GETFD", 1),
    ("F_SETFD", 2),
    ("F_GETFL", 3),
    ("F_SETFL", 4),
    ("F_GETLK", 5),
    ("F_SETLK", 6),
    ("F_SETLKW", 7),
    ("F_SETOWN", 8),
    ("F_GETOWN", 9),
    ("F_SETSIG", 10),
    ("F_GETSIG", 11),
    ("F_SETOWN_EX", 15),
    ("F_GETOWN_EX", 16),
    ("F_OWNER_TID", 0),
    ("F_OWNER_PID", 1),
    ("F_OWNER_PGRP", 2),
    ("F_GETLK64", 5),
    ("F_SETLK64", 6),
    ("F_SETLKW64", 7),
    ("F_OFD_GETLK", 36),
    ("F_OFD_SETLK", 37),
    ("F_OFD_SETLKW", 38),
    ("FD_CLOEXEC", 1),
    ("F_RDLCK", 0),
    ("F_WRLCK", 1),
    ("F_UNLCK", 2),
    ("F_EXLCK", 4),
    ("F_SHLCK", 8),
    ("F_SETLEASE", 1024),
    ("F_GETLEASE", 1025),
    ("F_NOTIFY", 1026),
    ("F_CANCELLK", 1029),
    ("F_SETPIPE_SZ", 1031),
    ("F_GETPIPE_SZ", 1032),
    ("F_ADD_SEALS", 1033),
    ("F_GET_SEALS", 1034),
    ("F_SEAL_SEAL", 1),
    ("F_SEAL_SHRINK", 2),
    ("F_SEAL_GROW", 4),
    ("F_SEAL_WRITE", 8),
    ("F_SEAL_FUTURE_WRITE", 16),
    ("F_GET_RW_HINT", 1035),
    ("F_SET_RW_HINT", 1036),
    ("F_GET_FILE_RW_HINT", 1037),
    ("F_SET_FILE_RW_HINT", 1038),
    ("RWH_WRITE_LIFE_NOT_SET", 0),
    ("RWH_WRITE_LIFE_NONE", 1),
    ("RWH_WRITE_LIFE_SHORT", 2),
    ("RWH_WRITE_LIFE_MEDIUM", 3),
    ("RWH_WRITE_LIFE_LONG", 4),
    ("RWH_WRITE_LIFE_EXTREME", 5),
    ("DN_ACCESS", 1),
    ("DN_MODIFY", 2),
    ("DN_CREATE", 4),
    ("DN_DELETE", 8),
    ("DN_RENAME", 16),
    ("DN_ATTRIB", 32),
    ("DN_MULTISHOT", 0x8000_0000),
    ("FASYNC", 0o20000),
    ("FICLONE", 0x4004_9409),
    ("FICLONERANGE", 0x4020_940d),
];

pub fn init() -> PyResult {
    let methods = mdefs![
        ("fcntl", FAST, fcntl_fcntl, "fcntl($module, fd, cmd, arg=0, /)\n--\n\nApply the fcntl() command cmd to the file descriptor.\n\narg may be an int (the result is the int returned by the call) or a\nbytes-like object or str, which is copied into a buffer passed to the call;\nthe possibly modified buffer is then returned as bytes."),
        ("ioctl", FAST, fcntl_ioctl, "ioctl($module, fd, request, arg=0, mutate_flag=True, /)\n--\n\nApply the ioctl() request to the file descriptor.\n\narg may be an int or a buffer. A writable buffer is updated in place when\nmutate_flag is true (the int result is returned); otherwise a copy is passed\nand returned as bytes."),
        ("flock", FAST, fcntl_flock, "flock($module, fd, operation, /)\n--\n\nApply the flock() operation (LOCK_SH, LOCK_EX, LOCK_UN, optionally | LOCK_NB)."),
        ("lockf", FAST, fcntl_lockf, "lockf($module, fd, cmd, len=0, start=0, whence=0, /)\n--\n\nLock or unlock a byte range of the file using fcntl() record locks.\n\ncmd is LOCK_UN, LOCK_SH or LOCK_EX, optionally | LOCK_NB."),
    ];
    let (m, d) = crate::modules::fncommon::new_module("fcntl", methods, Some("The fcntl() and ioctl() system calls and file locking (flock, lockf).\n\nFile descriptors may be given as ints or as objects with a fileno() method."))?;
    for (n, v) in CONSTANTS {
        dict::set_item_str(&d, n, &int::new_int(*v))?;
    }
    Ok(m)
}
