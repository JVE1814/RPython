//! (hang1) Linux-specific posix functions: timer descriptors (timerfd_*),
//! vectored positional I/O (preadv/pwritev with RWF_* flags), splice, and the
//! supplementary-group helpers getgrouplist/initgroups.
use super::kwparse;
use super::path::*;
use crate::errors::*;
use crate::ffi::PyObject;
use crate::raise;
use crate::types::typeobject::tname;
use crate::types::{float, int, list, none, str as pystr, tuple};
use std::ffi::CString;

// ------------------------------------------------------------------ timerfd

pub const TFD_TIMER_ABSTIME: i32 = 1;
pub const TFD_TIMER_CANCEL_ON_SET: i32 = 2;

pub fn os_timerfd_create(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [clock, flags] = kwparse::<2>("timerfd_create", &["clockid", "flags"], 1, 1, 1, args, kw)?;
    let clock = c_int(clock.unwrap())?;
    let flags = match flags {
        Some(f) => c_int(f)?,
        None => 0,
    };
    // descriptors are created non-inheritable (PEP 446)
    let fd = retry_eintr(-1, || unsafe { libc::timerfd_create(clock, flags | libc::TFD_CLOEXEC) })?;
    if fd < 0 {
        return Err(os_err());
    }
    Ok(int::new_int(fd as i64))
}

/// Seconds (float or int) to a timespec, rounding towards minus infinity.
fn secs_to_timespec(o: &PyObject) -> PyResult<libc::timespec> {
    if int::is_int(o) {
        return Ok(libc::timespec { tv_sec: c_long(o)?, tv_nsec: 0 });
    }
    let f = if float::is_float(o) {
        float::float_value(o)
    } else {
        raise!(TypeError, "'{}' object cannot be interpreted as a timestamp", tname(o));
    };
    if !f.is_finite() {
        raise!(ValueError, "Invalid value {} (type float)", f);
    }
    let total = (f * 1e9).floor();
    if total.abs() >= 9.2e18 {
        raise!(OverflowError, "timestamp too large to convert to C _PyTime_t");
    }
    let total = total as i64;
    Ok(libc::timespec { tv_sec: total.div_euclid(1_000_000_000), tv_nsec: total.rem_euclid(1_000_000_000) })
}

fn ns_to_timespec(o: &PyObject) -> PyResult<libc::timespec> {
    let ns = c_long(o)?;
    Ok(libc::timespec { tv_sec: ns.div_euclid(1_000_000_000), tv_nsec: ns.rem_euclid(1_000_000_000) })
}

fn timespec_secs(t: &libc::timespec) -> PyRefResult {
    Ok(float::new_float(t.tv_sec as f64 + t.tv_nsec as f64 * 1e-9))
}
fn timespec_ns(t: &libc::timespec) -> PyRefResult {
    Ok(int::new_int_i128(t.tv_sec as i128 * 1_000_000_000 + t.tv_nsec as i128))
}
type PyRefResult = PyResult<crate::object::PyRef>;

fn settime(fname: &str, args: &[&PyObject], kw: Option<&PyObject>, ns: bool) -> PyResult {
    let [fd, flags, initial, interval] = kwparse::<4>(fname, &["fd", "flags", "initial", "interval"], 1, 1, 3, args, kw)?;
    let fd = c_int(fd.unwrap())?;
    let flags = match flags {
        Some(f) => c_int(f)?,
        None => 0,
    };
    let conv = if ns { ns_to_timespec } else { secs_to_timespec };
    let zero = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    let new = libc::itimerspec {
        it_value: match initial {
            Some(v) => conv(v)?,
            None => zero,
        },
        it_interval: match interval {
            Some(v) => conv(v)?,
            None => zero,
        },
    };
    let mut old: libc::itimerspec = unsafe { std::mem::zeroed() };
    let r = retry_eintr(-1, || unsafe { libc::timerfd_settime(fd, flags, &new, &mut old) })?;
    if r < 0 {
        return Err(os_err());
    }
    pair(&old, ns)
}

fn pair(t: &libc::itimerspec, ns: bool) -> PyResult {
    let f = if ns { timespec_ns } else { timespec_secs };
    Ok(tuple::new_tuple(vec![f(&t.it_value)?, f(&t.it_interval)?]))
}

pub fn os_timerfd_settime(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    settime("timerfd_settime", args, kw, false)
}
pub fn os_timerfd_settime_ns(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    settime("timerfd_settime_ns", args, kw, true)
}

fn gettime(fname: &str, args: &[&PyObject], ns: bool) -> PyResult {
    crate::args::check_pos(fname, args, 1, 1)?;
    let fd = c_int(args[0])?;
    let mut cur: libc::itimerspec = unsafe { std::mem::zeroed() };
    let r = retry_eintr(-1, || unsafe { libc::timerfd_gettime(fd, &mut cur) })?;
    if r < 0 {
        return Err(os_err());
    }
    pair(&cur, ns)
}
pub fn os_timerfd_gettime(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    gettime("timerfd_gettime", args, false)
}
pub fn os_timerfd_gettime_ns(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    gettime("timerfd_gettime_ns", args, true)
}

// ------------------------------------------------------------------ preadv / pwritev

pub fn os_preadv(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("preadv", args, 3, 4)?;
    let fd = c_int(args[0])?;
    let mut bufs = super::fs::collect_buffers(args[1], true)?;
    let off = c_long(args[2])?;
    let flags = if args.len() > 3 { c_int(args[3])? } else { 0 };
    let iov: Vec<libc::iovec> =
        bufs.iter_mut().map(|b| libc::iovec { iov_base: b.as_mut_slice().as_mut_ptr() as *mut _, iov_len: b.len() }).collect();
    let r = retry_eintr(-1, || unsafe {
        if flags == 0 {
            libc::preadv(fd, iov.as_ptr(), iov.len() as i32, off)
        } else {
            libc::preadv2(fd, iov.as_ptr(), iov.len() as i32, off, flags)
        }
    })?;
    if r < 0 {
        return Err(os_err());
    }
    Ok(int::new_int(r as i64))
}

pub fn os_pwritev(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("pwritev", args, 3, 4)?;
    let fd = c_int(args[0])?;
    let bufs = super::fs::collect_buffers(args[1], false)?;
    let off = c_long(args[2])?;
    let flags = if args.len() > 3 { c_int(args[3])? } else { 0 };
    let iov: Vec<libc::iovec> =
        bufs.iter().map(|b| libc::iovec { iov_base: b.as_slice().as_ptr() as *mut _, iov_len: b.len() }).collect();
    let r = retry_eintr(-1, || unsafe {
        if flags == 0 {
            libc::pwritev(fd, iov.as_ptr(), iov.len() as i32, off)
        } else {
            libc::pwritev2(fd, iov.as_ptr(), iov.len() as i32, off, flags)
        }
    })?;
    if r < 0 {
        return Err(os_err());
    }
    Ok(int::new_int(r as i64))
}

// ------------------------------------------------------------------ splice

pub fn os_splice(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [src, dst, count, off_src, off_dst, flags] =
        kwparse::<6>("splice", &["src", "dst", "count", "offset_src", "offset_dst", "flags"], 0, 3, 0, args, kw)?;
    let src = c_int(src.unwrap())?;
    let dst = c_int(dst.unwrap())?;
    let count = ssize(count.unwrap())?;
    if count < 0 {
        raise!(ValueError, "negative value not allowed");
    }
    let opt_off = |o: Option<&PyObject>| -> PyResult<Option<i64>> {
        match o {
            Some(o) if !o.is(none::none()) => Ok(Some(c_long(o)?)),
            _ => Ok(None),
        }
    };
    let mut os_ = opt_off(off_src)?;
    let mut od = opt_off(off_dst)?;
    let flags = match flags {
        Some(f) => c_int(f)? as u32,
        None => 0,
    };
    let r = retry_eintr(-1, || unsafe {
        libc::splice(
            src,
            os_.as_mut().map_or(std::ptr::null_mut(), |x| x as *mut i64),
            dst,
            od.as_mut().map_or(std::ptr::null_mut(), |x| x as *mut i64),
            count as usize,
            flags,
        )
    })?;
    if r < 0 {
        return Err(os_err());
    }
    Ok(int::new_int(r as i64))
}

// ------------------------------------------------------------------ groups

fn user_cstring(fname: &str, o: &PyObject) -> PyResult<CString> {
    if !pystr::is_str(o) {
        raise!(TypeError, "{}() argument 1 must be str, not {}", fname, tname(o));
    }
    let b = fs_encode_str(o)?;
    match CString::new(b) {
        Ok(c) => Ok(c),
        Err(_) => raise!(ValueError, "embedded null byte"),
    }
}

pub fn os_getgrouplist(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("getgrouplist", args, 2, 2)?;
    let user = user_cstring("getgrouplist", args[0])?;
    let base = id_arg(args[1], "gid")?;
    let mut n: libc::c_int = 64;
    loop {
        let mut groups: Vec<libc::gid_t> = vec![0; n as usize];
        let mut got = n;
        let r = unsafe { libc::getgrouplist(user.as_ptr(), base, groups.as_mut_ptr(), &mut got) };
        if r >= 0 {
            groups.truncate(got as usize);
            let items = groups.into_iter().map(|g| int::new_int(g as i64)).collect();
            return Ok(list::new_list(items));
        }
        // the list did not fit: `got` holds the size needed (or grow)
        n = if got > n { got } else { n * 2 };
        if n > 1 << 20 {
            return Err(os_err());
        }
    }
}

pub fn os_initgroups(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("initgroups", args, 2, 2)?;
    let user = user_cstring("initgroups", args[0])?;
    let base = id_arg(args[1], "gid")?;
    if unsafe { libc::initgroups(user.as_ptr(), base) } < 0 {
        return Err(os_err());
    }
    Ok(none::none_ref())
}
