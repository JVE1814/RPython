//! Process-related posix functions.
use super::kwparse;
use super::path::*;
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::{PyObject, PyTypeObject};
use crate::object::PyRef;
use crate::raise;
use crate::types::structseq::{self, Desc, Field};
use crate::types::typeobject::tname;
use crate::types::{bool_, bytes, dict, float, int, list, none, str as pystr, tuple};
use std::ffi::CString;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering};

pub static TIMES_RESULT: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());
pub static UNAME_RESULT: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());
pub static WAITID_RESULT: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());
pub static SCHED_PARAM: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());

pub fn init_types() -> PyResult<()> {
    if !TIMES_RESULT.load(Ordering::Acquire).is_null() {
        return Ok(());
    }
    const TIMES: &[Field] = &[
        Field::new("user", "user time"),
        Field::new("system", "system time"),
        Field::new("children_user", "user time of children"),
        Field::new("children_system", "system time of children"),
        Field::new("elapsed", "elapsed time since an arbitrary point in the past"),
    ];
    let t = structseq::new_type(&Desc {
        name: "posix.times_result",
        doc: "times_result: Result from os.times().\n\nThis object may be accessed either as a tuple of\n  (user, system, children_user, children_system, elapsed),\nor via the attributes user, system, children_user, children_system,\nand elapsed.\n\nSee os.times for more information.",
        fields: TIMES,
        n_in_sequence: 5,
    })?;
    TIMES_RESULT.store(t, Ordering::Release);
    const UNAME: &[Field] = &[
        Field::new("sysname", "operating system name"),
        Field::new("nodename", "name of machine on network (implementation-defined)"),
        Field::new("release", "operating system release"),
        Field::new("version", "operating system version"),
        Field::new("machine", "hardware identifier"),
    ];
    let t = structseq::new_type(&Desc {
        name: "posix.uname_result",
        doc: "uname_result: Result from os.uname().\n\nThis object may be accessed either as a tuple of\n  (sysname, nodename, release, version, machine),\nor via the attributes sysname, nodename, release, version, and machine.\n\nSee os.uname for more information.",
        fields: UNAME,
        n_in_sequence: 5,
    })?;
    UNAME_RESULT.store(t, Ordering::Release);
    const WAITID: &[Field] = &[
        Field::new("si_pid", ""),
        Field::new("si_uid", ""),
        Field::new("si_signo", ""),
        Field::new("si_status", ""),
        Field::new("si_code", ""),
    ];
    let t = structseq::new_type(&Desc {
        name: "posix.waitid_result",
        doc: "waitid_result: Result from waitid.\n\nThis object may be accessed either as a tuple of\n  (si_pid, si_uid, si_signo, si_status, si_code),\nor via the attributes si_pid, si_uid, and so on.\n\nSee os.waitid for more information.",
        fields: WAITID,
        n_in_sequence: 5,
    })?;
    WAITID_RESULT.store(t, Ordering::Release);
    const SP: &[Field] = &[Field::new("sched_priority", "the scheduling priority")];
    let t = structseq::new_type(&Desc {
        name: "posix.sched_param",
        doc: "sched_param(sched_priority)\n--\n\nCurrently has only one field: sched_priority\n\n  sched_priority\n    A scheduling parameter.",
        fields: SP,
        n_in_sequence: 1,
    })?;
    unsafe {
        (*t).tp_new = Some(sched_param_new);
    }
    SCHED_PARAM.store(t, Ordering::Release);
    Ok(())
}

unsafe extern "C" fn sched_param_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let (vals, kws) = crate::args::kwdict_to_vectorcall(&*args, kw);
            let refs: Vec<&PyObject> = vals.iter().map(|x| &**x).collect();
            let [p] = kwparse::<1>("sched_param", &["sched_priority"], 0, 1, 0, &refs, kws.as_deref())?;
            Ok(structseq::new_instance(tp, vec![p.unwrap().to_ref()]))
        })();
        into_c(r)
    }
}

fn ok_none() -> PyResult {
    Ok(none::none_ref())
}
fn i(v: i64) -> PyRef {
    int::new_int(v)
}

macro_rules! simple_getter {
    ($fname:ident, $call:expr) => {
        pub fn $fname(_m: &PyObject) -> PyResult {
            Ok(int::new_int(unsafe { $call } as i64))
        }
    };
}
simple_getter!(os_getpid, libc::getpid());
simple_getter!(os_getppid, libc::getppid());
simple_getter!(os_getpgrp, libc::getpgrp());

pub fn os_getuid(_m: &PyObject) -> PyResult {
    Ok(id_result(unsafe { libc::getuid() }))
}
pub fn os_geteuid(_m: &PyObject) -> PyResult {
    Ok(id_result(unsafe { libc::geteuid() }))
}
pub fn os_getgid(_m: &PyObject) -> PyResult {
    Ok(id_result(unsafe { libc::getgid() }))
}
pub fn os_getegid(_m: &PyObject) -> PyResult {
    Ok(id_result(unsafe { libc::getegid() }))
}

pub fn os_getgroups(_m: &PyObject) -> PyResult {
    let n = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    if n < 0 {
        return Err(os_err());
    }
    let mut v = vec![0 as libc::gid_t; n as usize + 1];
    let n = unsafe { libc::getgroups(v.len() as i32, v.as_mut_ptr()) };
    if n < 0 {
        return Err(os_err());
    }
    Ok(list::new_list(v[..n as usize].iter().map(|g| id_result(*g)).collect()))
}

pub fn os_setgroups(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("setgroups", args, 1, 1)?;
    if ab::get_iter(args[0]).is_err() {
        clear_exc();
        raise!(TypeError, "setgroups argument must be a sequence");
    }
    let items = ab::collect_iterable(args[0])?;
    let mut v = Vec::with_capacity(items.len());
    for it in &items {
        if !int::is_int(it) {
            raise!(TypeError, "groups must be integers");
        }
        v.push(id_arg(it, "gid")?);
    }
    if unsafe { libc::setgroups(v.len(), v.as_ptr()) } < 0 {
        return Err(os_err());
    }
    ok_none()
}

macro_rules! set_id {
    ($fname:ident, $pyname:literal, $what:literal, $call:path) => {
        pub fn $fname(_m: &PyObject, args: &[&PyObject]) -> PyResult {
            crate::args::check_pos($pyname, args, 1, 1)?;
            let v = id_arg(args[0], $what)?;
            if unsafe { $call(v) } < 0 {
                return Err(os_err());
            }
            ok_none()
        }
    };
}
set_id!(os_setuid, "setuid", "uid", libc::setuid);
set_id!(os_seteuid, "seteuid", "uid", libc::seteuid);
set_id!(os_setgid, "setgid", "gid", libc::setgid);
set_id!(os_setegid, "setegid", "gid", libc::setegid);

pub fn os_setreuid(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("setreuid", args, 2, 2)?;
    let a = id_arg(args[0], "uid")?;
    let b = id_arg(args[1], "uid")?;
    if unsafe { libc::setreuid(a, b) } < 0 {
        return Err(os_err());
    }
    ok_none()
}
pub fn os_setregid(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("setregid", args, 2, 2)?;
    let a = id_arg(args[0], "gid")?;
    let b = id_arg(args[1], "gid")?;
    if unsafe { libc::setregid(a, b) } < 0 {
        return Err(os_err());
    }
    ok_none()
}
pub fn os_getresuid(_m: &PyObject) -> PyResult {
    let (mut a, mut b, mut c) = (0, 0, 0);
    if unsafe { libc::getresuid(&mut a, &mut b, &mut c) } < 0 {
        return Err(os_err());
    }
    Ok(tuple::new_tuple(vec![id_result(a), id_result(b), id_result(c)]))
}
pub fn os_getresgid(_m: &PyObject) -> PyResult {
    let (mut a, mut b, mut c) = (0, 0, 0);
    if unsafe { libc::getresgid(&mut a, &mut b, &mut c) } < 0 {
        return Err(os_err());
    }
    Ok(tuple::new_tuple(vec![id_result(a), id_result(b), id_result(c)]))
}
pub fn os_setresuid(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("setresuid", args, 3, 3)?;
    let (a, b, c) = (id_arg(args[0], "uid")?, id_arg(args[1], "uid")?, id_arg(args[2], "uid")?);
    if unsafe { libc::setresuid(a, b, c) } < 0 {
        return Err(os_err());
    }
    ok_none()
}
pub fn os_setresgid(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("setresgid", args, 3, 3)?;
    let (a, b, c) = (id_arg(args[0], "gid")?, id_arg(args[1], "gid")?, id_arg(args[2], "gid")?);
    if unsafe { libc::setresgid(a, b, c) } < 0 {
        return Err(os_err());
    }
    ok_none()
}

pub fn os_getpgid(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("getpgid", args, 1, 1)?;
    let pid = c_int(args[0])?;
    let r = unsafe { libc::getpgid(pid) };
    if r < 0 {
        return Err(os_err());
    }
    Ok(i(r as i64))
}
pub fn os_setpgid(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("setpgid", args, 2, 2)?;
    let (a, b) = (c_int(args[0])?, c_int(args[1])?);
    if unsafe { libc::setpgid(a, b) } < 0 {
        return Err(os_err());
    }
    ok_none()
}
pub fn os_setpgrp(_m: &PyObject) -> PyResult {
    if unsafe { libc::setpgid(0, 0) } < 0 {
        return Err(os_err());
    }
    ok_none()
}
pub fn os_getsid(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("getsid", args, 1, 1)?;
    let pid = c_int(args[0])?;
    let r = unsafe { libc::getsid(pid) };
    if r < 0 {
        return Err(os_err());
    }
    Ok(i(r as i64))
}
pub fn os_setsid(_m: &PyObject) -> PyResult {
    if unsafe { libc::setsid() } < 0 {
        return Err(os_err());
    }
    ok_none()
}
pub fn os_tcgetpgrp(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("tcgetpgrp", args, 1, 1)?;
    let fd = c_int(args[0])?;
    let r = unsafe { libc::tcgetpgrp(fd) };
    if r < 0 {
        return Err(os_err());
    }
    Ok(i(r as i64))
}
pub fn os_tcsetpgrp(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("tcsetpgrp", args, 2, 2)?;
    let fd = c_int(args[0])?;
    let pg = c_int(args[1])?;
    if unsafe { libc::tcsetpgrp(fd, pg) } < 0 {
        return Err(os_err());
    }
    ok_none()
}

pub fn os_getlogin(_m: &PyObject) -> PyResult {
    let p = unsafe { libc::getlogin() };
    if p.is_null() {
        let e = errno();
        if e != 0 {
            return Err(crate::types::exceptions::os_error_from_errno(e, None, None));
        }
        raise!(OSError, "unable to determine login name");
    }
    fs_decode(unsafe { std::ffi::CStr::from_ptr(p) }.to_bytes())
}

pub fn os_kill(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("kill", args, 2, 2)?;
    let pid = c_int(args[0])?;
    let sig = c_int(args[1])?;
    if unsafe { libc::kill(pid, sig) } < 0 {
        return Err(os_err());
    }
    check_signals()?;
    ok_none()
}
pub fn os_killpg(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("killpg", args, 2, 2)?;
    let pg = c_int(args[0])?;
    let sig = c_int(args[1])?;
    if unsafe { libc::killpg(pg, sig) } < 0 {
        return Err(os_err());
    }
    ok_none()
}

pub fn os_abort(_m: &PyObject) -> PyResult {
    unsafe { libc::abort() }
}

pub fn os__exit(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [status] = kwparse::<1>("_exit", &["status"], 0, 1, 0, args, kw)?;
    let s = c_int(status.unwrap())?;
    unsafe { libc::_exit(s) }
}

pub fn os_nice(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("nice", args, 1, 1)?;
    let inc = c_int(args[0])?;
    unsafe { *libc::__errno_location() = 0 };
    let r = unsafe { libc::nice(inc) };
    if r == -1 && errno() != 0 {
        return Err(os_err());
    }
    Ok(i(r as i64))
}

pub fn os_getpriority(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("getpriority", args, 2, 2)?;
    let which = c_int(args[0])?;
    let who = c_int(args[1])?;
    unsafe { *libc::__errno_location() = 0 };
    let r = unsafe { libc::getpriority(which as u32, who as u32) };
    if r == -1 && errno() != 0 {
        return Err(os_err());
    }
    Ok(i(r as i64))
}
pub fn os_setpriority(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("setpriority", args, 3, 3)?;
    let which = c_int(args[0])?;
    let who = c_int(args[1])?;
    let prio = c_int(args[2])?;
    if unsafe { libc::setpriority(which as u32, who as u32, prio) } < 0 {
        return Err(os_err());
    }
    ok_none()
}

pub fn os_times(_m: &PyObject) -> PyResult {
    let mut t: libc::tms = unsafe { std::mem::zeroed() };
    let c = unsafe { libc::times(&mut t) };
    if c == !0 as libc::clock_t {
        return Err(os_err());
    }
    let tick = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    let vals = vec![
        float::new_float(t.tms_utime as f64 / tick),
        float::new_float(t.tms_stime as f64 / tick),
        float::new_float(t.tms_cutime as f64 / tick),
        float::new_float(t.tms_cstime as f64 / tick),
        float::new_float(c as f64 / tick),
    ];
    Ok(structseq::new_instance(super::typeptr(&TIMES_RESULT), vals))
}

fn cfield(b: &[libc::c_char]) -> PyResult {
    let bytes: Vec<u8> = b.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    fs_decode(&bytes)
}

pub fn os_uname(_m: &PyObject) -> PyResult {
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut u) } < 0 {
        return Err(os_err());
    }
    let vals = vec![cfield(&u.sysname)?, cfield(&u.nodename)?, cfield(&u.release)?, cfield(&u.version)?, cfield(&u.machine)?];
    Ok(structseq::new_instance(super::typeptr(&UNAME_RESULT), vals))
}

pub fn os_cpu_count(_m: &PyObject) -> PyResult {
    let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    if n < 1 { ok_none() } else { Ok(i(n as i64)) }
}

pub fn affinity_count() -> Option<usize> {
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) == 0 {
            return Some(libc::CPU_COUNT(&set) as usize);
        }
    }
    None
}

pub fn os_sched_getaffinity(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("sched_getaffinity", args, 1, 1)?;
    let pid = c_int(args[0])?;
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        if libc::sched_getaffinity(pid, std::mem::size_of::<libc::cpu_set_t>(), &mut set) != 0 {
            return Err(os_err());
        }
        let s = crate::abstract_::call(crate::types::typeobject::type_obj(crate::types::set::set_type()), &[])?;
        for c in 0..libc::CPU_SETSIZE as usize {
            if libc::CPU_ISSET(c, &set) {
                ab::call_method_str(&s, "add", &[&i(c as i64)])?;
            }
        }
        Ok(s)
    }
}

pub fn os_sched_setaffinity(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("sched_setaffinity", args, 2, 2)?;
    let pid = c_int(args[0])?;
    let items = ab::collect_iterable(args[1])?;
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        for it in &items {
            if !int::is_int(it) {
                raise!(TypeError, "expected an iterator of ints, but iterator yielded {}", tname(it));
            }
            let c = ab::as_i64(it)?;
            if c < 0 {
                raise!(ValueError, "negative CPU number");
            }
            if c as usize >= libc::CPU_SETSIZE as usize {
                return Err(crate::types::exceptions::os_error_from_errno(libc::EINVAL, None, None));
            }
            libc::CPU_SET(c as usize, &mut set);
        }
        if libc::sched_setaffinity(pid, std::mem::size_of::<libc::cpu_set_t>(), &set) != 0 {
            return Err(os_err());
        }
    }
    ok_none()
}

pub fn os_sched_yield(_m: &PyObject) -> PyResult {
    unsafe { libc::sched_yield() };
    ok_none()
}
pub fn os_sched_get_priority_max(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [p] = kwparse::<1>("sched_get_priority_max", &["policy"], 0, 1, 0, args, kw)?;
    let r = unsafe { libc::sched_get_priority_max(c_int(p.unwrap())?) };
    if r < 0 {
        return Err(os_err());
    }
    Ok(i(r as i64))
}
pub fn os_sched_get_priority_min(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [p] = kwparse::<1>("sched_get_priority_min", &["policy"], 0, 1, 0, args, kw)?;
    let r = unsafe { libc::sched_get_priority_min(c_int(p.unwrap())?) };
    if r < 0 {
        return Err(os_err());
    }
    Ok(i(r as i64))
}
pub fn os_sched_getscheduler(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("sched_getscheduler", args, 1, 1)?;
    let r = unsafe { libc::sched_getscheduler(c_int(args[0])?) };
    if r < 0 {
        return Err(os_err());
    }
    Ok(i(r as i64))
}
fn sched_param_arg(o: &PyObject) -> PyResult<libc::sched_param> {
    if !o.is_type(super::typeptr(&SCHED_PARAM)) {
        raise!(TypeError, "must have a sched_param object");
    }
    let p = structseq::get_item(o, 0);
    let v = c_long(p)?;
    if v > i32::MAX as i64 || v < i32::MIN as i64 {
        raise!(OverflowError, "sched_priority out of range");
    }
    Ok(libc::sched_param { sched_priority: v as i32 })
}
pub fn os_sched_getparam(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("sched_getparam", args, 1, 1)?;
    let mut p: libc::sched_param = unsafe { std::mem::zeroed() };
    if unsafe { libc::sched_getparam(c_int(args[0])?, &mut p) } < 0 {
        return Err(os_err());
    }
    Ok(structseq::new_instance(super::typeptr(&SCHED_PARAM), vec![i(p.sched_priority as i64)]))
}
pub fn os_sched_setparam(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("sched_setparam", args, 2, 2)?;
    let pid = c_int(args[0])?;
    let p = sched_param_arg(args[1])?;
    if unsafe { libc::sched_setparam(pid, &p) } < 0 {
        return Err(os_err());
    }
    ok_none()
}
pub fn os_sched_setscheduler(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("sched_setscheduler", args, 3, 3)?;
    let pid = c_int(args[0])?;
    let pol = c_int(args[1])?;
    let p = sched_param_arg(args[2])?;
    if unsafe { libc::sched_setscheduler(pid, pol, &p) } < 0 {
        return Err(os_err());
    }
    ok_none()
}
pub fn os_sched_rr_get_interval(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("sched_rr_get_interval", args, 1, 1)?;
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    if unsafe { libc::sched_rr_get_interval(c_int(args[0])?, &mut ts) } < 0 {
        return Err(os_err());
    }
    Ok(float::new_float(ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9))
}

pub fn os_getloadavg(_m: &PyObject) -> PyResult {
    let mut l = [0f64; 3];
    if unsafe { libc::getloadavg(l.as_mut_ptr(), 3) } != 3 {
        raise!(OSError, "Load averages are unobtainable");
    }
    Ok(tuple::new_tuple(l.iter().map(|x| float::new_float(*x)).collect()))
}

pub fn os_strerror(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("strerror", args, 1, 1)?;
    let code = c_int(args[0])?;
    let s = unsafe { std::ffi::CStr::from_ptr(libc::strerror(code)) };
    Ok(pystr::new_str(&s.to_string_lossy()))
}

pub fn os_ctermid(_m: &PyObject) -> PyResult {
    let mut buf = [0 as libc::c_char; 64];
    let p = unsafe { libc::ctermid(buf.as_mut_ptr()) };
    if p.is_null() {
        return Err(os_err());
    }
    fs_decode(unsafe { std::ffi::CStr::from_ptr(p) }.to_bytes())
}

pub fn os_urandom(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("urandom", args, 1, 1)?;
    let n = ssize(args[0])?;
    if n < 0 {
        raise!(ValueError, "negative argument not allowed");
    }
    let mut buf = crate::errors::try_zeroed_vec(n as usize)?;
    fill_random(&mut buf)?;
    Ok(bytes::new_bytes(&buf))
}

pub fn fill_random(buf: &mut [u8]) -> PyResult<()> {
    let mut off = 0;
    while off < buf.len() {
        let r = unsafe { libc::getrandom(buf[off..].as_mut_ptr() as *mut libc::c_void, buf.len() - off, 0) };
        if r < 0 {
            if errno() == libc::EINTR {
                check_signals()?;
                continue;
            }
            return Err(os_err());
        }
        off += r as usize;
    }
    Ok(())
}

pub fn os_getrandom(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [size, flags] = kwparse::<2>("getrandom", &["size", "flags"], 0, 1, 0, args, kw)?;
    let n = ssize(size.unwrap())?;
    if n < 0 {
        return Err(crate::types::exceptions::os_error_from_errno(libc::EINVAL, None, None));
    }
    let flags = match flags {
        Some(f) => c_int(f)? as u32,
        None => 0,
    };
    let mut buf = crate::errors::try_zeroed_vec(n as usize)?;
    let r = retry_eintr(-1, || unsafe { libc::getrandom(buf.as_mut_ptr() as *mut libc::c_void, n as usize, flags) })?;
    if r < 0 {
        return Err(os_err());
    }
    Ok(bytes::new_bytes(&buf[..r as usize]))
}

// ------------------------------------------------------------------ fork / exec / wait

struct AtFork {
    before: Vec<PyRef>,
    parent: Vec<PyRef>,
    child: Vec<PyRef>,
}
unsafe impl Send for AtFork {}
static AT_FORK: Mutex<AtFork> = Mutex::new(AtFork { before: Vec::new(), parent: Vec::new(), child: Vec::new() });

pub fn os_register_at_fork(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [before, child, parent] = kwparse::<3>("register_at_fork", &["before", "after_in_child", "after_in_parent"], 0, 0, 3, args, kw)?;
    if before.is_none() && child.is_none() && parent.is_none() {
        raise!(TypeError, "At least one argument is required.");
    }
    for (o, n) in [(before, "before"), (child, "after_in_child"), (parent, "after_in_parent")] {
        if let Some(o) = o {
            if !ab::is_callable(o) {
                raise!(TypeError, "'{}' must be callable, not {}", n, tname(o));
            }
        }
    }
    let mut a = AT_FORK.lock().unwrap();
    if let Some(o) = before {
        a.before.push(o.to_ref());
    }
    if let Some(o) = child {
        a.child.push(o.to_ref());
    }
    if let Some(o) = parent {
        a.parent.push(o.to_ref());
    }
    ok_none()
}

fn run_hooks(hooks: Vec<PyRef>, reverse: bool) {
    let iter: Box<dyn Iterator<Item = &PyRef>> = if reverse { Box::new(hooks.iter().rev()) } else { Box::new(hooks.iter()) };
    for h in iter {
        if ab::call(h, &[]).is_err() {
            crate::modules::sys::write_unraisable_with_obj("Exception ignored in fork hook", Some(h));
        }
    }
}

pub fn run_before_fork() {
    let hooks = AT_FORK.lock().unwrap().before.clone();
    run_hooks(hooks, true);
}
pub fn run_after_fork_parent() {
    let hooks = AT_FORK.lock().unwrap().parent.clone();
    run_hooks(hooks, false);
}
pub fn run_after_fork_child() {
    // the lock may have been held by another thread at fork time
    let hooks = match AT_FORK.try_lock() {
        Ok(a) => a.child.clone(),
        Err(_) => Vec::new(),
    };
    run_hooks(hooks, false);
}

pub fn os_fork(_m: &PyObject) -> PyResult {
    let (pid, _) = fork_with("fork", || {
        let pid = unsafe { libc::fork() };
        (pid as i64, -1)
    })?;
    Ok(i(pid))
}

/// os.forkpty(): fork with the child attached to a new pseudo-terminal.
/// Returns (pid, master_fd).
pub fn os_forkpty(_m: &PyObject) -> PyResult {
    let (pid, fd) = fork_with("forkpty", || {
        let mut master: libc::c_int = -1;
        let pid = unsafe { libc::forkpty(&mut master, std::ptr::null_mut(), std::ptr::null(), std::ptr::null()) };
        (pid as i64, master)
    })?;
    Ok(tuple::new_tuple(vec![i(pid), i(fd as i64)]))
}

/// The fork protocol shared by fork() and forkpty(): Python before-hooks,
/// runtime preparation (see crate::fork), the fork itself, runtime fix-up,
/// Python after-hooks, and in the parent the multi-threaded fork warning.
fn fork_with(name: &str, f: impl FnOnce() -> (i64, libc::c_int)) -> PyResult<(i64, libc::c_int)> {
    run_before_fork();
    let st = crate::fork::prepare();
    let others = st.other_threads;
    let (pid, fd) = f();
    if pid < 0 {
        let e = errno();
        crate::fork::parent(st);
        run_after_fork_parent();
        return Err(crate::types::exceptions::os_error_from_errno(e, None, None));
    }
    if pid == 0 {
        crate::fork::child(st);
        crate::modules::posix::after_fork_child_hook();
        run_after_fork_child();
    } else {
        crate::fork::parent(st);
        run_after_fork_parent();
        crate::fork::warn_about_threads(others, name)?;
    }
    Ok((pid, fd))
}

/// Encode a sequence of str/bytes/PathLike into C strings.
fn argv_list(fname: &str, o: &PyObject, argn: &str) -> PyResult<Vec<CString>> {
    if !(tuple::is_tuple(o) || list::is_list(o)) {
        raise!(TypeError, "{} {} must be a tuple or list", fname, argn);
    }
    let items = ab::collect_iterable(o)?;
    let mut v = Vec::with_capacity(items.len());
    for it in &items {
        let b = fspath(it).map_err(|_| {
            clear_exc();
            err!(TypeError, "expected str, bytes or os.PathLike object, not {}", tname(it))
        })?;
        let raw = if bytes::is_bytes(&b) { bytes::bytes_data(&b).to_vec() } else { fs_encode_str(&b)? };
        if raw.contains(&0) {
            raise!(ValueError, "embedded null byte");
        }
        v.push(CString::new(raw).unwrap());
    }
    Ok(v)
}

fn env_list(fname: &str, env: &PyObject) -> PyResult<Vec<CString>> {
    let keys = match ab::call_method_str(env, "keys", &[]) {
        Ok(k) => k,
        Err(e) => {
            if exc_matches(crate::types::exceptions::AttributeError()) {
                clear_exc();
                raise!(TypeError, "{}: environment must be a mapping object", fname);
            }
            return Err(e);
        }
    };
    let keys = ab::collect_iterable(&keys)?;
    let mut v = Vec::with_capacity(keys.len());
    for k in &keys {
        let val = ab::getitem(env, k)?;
        let kb = fspath(k)?;
        let vb = fspath(&val)?;
        let mut kraw = if bytes::is_bytes(&kb) { bytes::bytes_data(&kb).to_vec() } else { fs_encode_str(&kb)? };
        let vraw = if bytes::is_bytes(&vb) { bytes::bytes_data(&vb).to_vec() } else { fs_encode_str(&vb)? };
        if kraw.is_empty() || kraw[1..].contains(&b'=') {
            raise!(ValueError, "illegal environment variable name");
        }
        if kraw.contains(&0) || vraw.contains(&0) {
            raise!(ValueError, "embedded null byte");
        }
        kraw.push(b'=');
        kraw.extend_from_slice(&vraw);
        v.push(CString::new(kraw).unwrap());
    }
    Ok(v)
}

fn ptr_array(v: &[CString]) -> Vec<*const libc::c_char> {
    let mut p: Vec<*const libc::c_char> = v.iter().map(|s| s.as_ptr()).collect();
    p.push(std::ptr::null());
    p
}

pub fn os_execv(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("execv", args, 2, 2)?;
    let p = super::path::path("execv", args[0])?;
    let argv = argv_list("execv()", args[1], "arg 2")?;
    if argv.is_empty() {
        raise!(ValueError, "execv() arg 2 must not be empty");
    }
    if argv[0].as_bytes().is_empty() {
        raise!(ValueError, "execv() arg 2 first element cannot be empty");
    }
    let av = ptr_array(&argv);
    unsafe { libc::execv(p.ptr(), av.as_ptr()) };
    Err(os_err())
}

pub fn os_execve(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [path, argv, env] = kwparse::<3>("execve", &["path", "argv", "env"], 0, 3, 0, args, kw)?;
    let p = path_fd("execve", path.unwrap())?;
    let argv = argv_list("execve:", argv.unwrap(), "argv")?;
    if argv.is_empty() {
        raise!(ValueError, "execve: argv must not be empty");
    }
    if argv[0].as_bytes().is_empty() {
        raise!(ValueError, "execve: argv first element cannot be empty");
    }
    let envv = env_list("execve", env.unwrap())?;
    let av = ptr_array(&argv);
    let ev = ptr_array(&envv);
    unsafe {
        match p.fd {
            Some(fd) => libc::fexecve(fd, av.as_ptr(), ev.as_ptr()),
            None => libc::execve(p.ptr(), av.as_ptr(), ev.as_ptr()),
        }
    };
    Err(os_err_with(&p))
}

pub fn os_waitpid(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("waitpid", args, 2, 2)?;
    let pid = c_int(args[0])?;
    let opts = c_int(args[1])?;
    let mut status = 0;
    let r = retry_eintr(-1, || unsafe { libc::waitpid(pid, &mut status, opts) })?;
    if r < 0 {
        return Err(os_err());
    }
    Ok(tuple::new_tuple(vec![i(r as i64), i(status as i64)]))
}

pub fn os_wait(_m: &PyObject) -> PyResult {
    let mut status = 0;
    let r = retry_eintr(-1, || unsafe { libc::wait(&mut status) })?;
    if r < 0 {
        return Err(os_err());
    }
    Ok(tuple::new_tuple(vec![i(r as i64), i(status as i64)]))
}

fn rusage_obj(ru: &libc::rusage) -> PyResult {
    let resource = crate::import::import_absolute("resource")?;
    let st = ab::getattr_str(&resource, "struct_rusage")?;
    let tv = |t: &libc::timeval| float::new_float(t.tv_sec as f64 + t.tv_usec as f64 * 1e-6);
    let vals = tuple::new_tuple(vec![
        tv(&ru.ru_utime),
        tv(&ru.ru_stime),
        i(ru.ru_maxrss),
        i(ru.ru_ixrss),
        i(ru.ru_idrss),
        i(ru.ru_isrss),
        i(ru.ru_minflt),
        i(ru.ru_majflt),
        i(ru.ru_nswap),
        i(ru.ru_inblock),
        i(ru.ru_oublock),
        i(ru.ru_msgsnd),
        i(ru.ru_msgrcv),
        i(ru.ru_nsignals),
        i(ru.ru_nvcsw),
        i(ru.ru_nivcsw),
    ]);
    ab::call(&st, &[&vals])
}

pub fn os_wait3(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [opts] = kwparse::<1>("wait3", &["options"], 0, 1, 0, args, kw)?;
    let opts = c_int(opts.unwrap())?;
    let mut status = 0;
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    let r = retry_eintr(-1, || unsafe { libc::wait4(-1, &mut status, opts, &mut ru) })?;
    if r < 0 {
        return Err(os_err());
    }
    Ok(tuple::new_tuple(vec![i(r as i64), i(status as i64), rusage_obj(&ru)?]))
}

pub fn os_wait4(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [pid, opts] = kwparse::<2>("wait4", &["pid", "options"], 0, 2, 0, args, kw)?;
    let pid = c_int(pid.unwrap())?;
    let opts = c_int(opts.unwrap())?;
    let mut status = 0;
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    let r = retry_eintr(-1, || unsafe { libc::wait4(pid, &mut status, opts, &mut ru) })?;
    if r < 0 {
        return Err(os_err());
    }
    Ok(tuple::new_tuple(vec![i(r as i64), i(status as i64), rusage_obj(&ru)?]))
}

pub fn os_waitid(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("waitid", args, 3, 3)?;
    let idtype = c_int(args[0])?;
    let id = c_int(args[1])?;
    let opts = c_int(args[2])?;
    let mut si: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let r = retry_eintr(-1, || unsafe { libc::waitid(idtype as libc::idtype_t, id as libc::id_t, &mut si, opts) })?;
    if r < 0 {
        return Err(os_err());
    }
    let pid = unsafe { si.si_pid() };
    if pid == 0 {
        return ok_none();
    }
    let vals = vec![i(pid as i64), id_result(unsafe { si.si_uid() }), i(si.si_signo as i64), i(unsafe { si.si_status() } as i64), i(si.si_code as i64)];
    Ok(structseq::new_instance(super::typeptr(&WAITID_RESULT), vals))
}

pub fn os_pidfd_open(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [pid, flags] = kwparse::<2>("pidfd_open", &["pid", "flags"], 0, 1, 0, args, kw)?;
    let pid = c_int(pid.unwrap())?;
    let flags = match flags {
        Some(f) => c_int(f)?,
        None => 0,
    };
    let r = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, flags) };
    if r < 0 {
        return Err(os_err());
    }
    Ok(i(r))
}

pub fn os_waitstatus_to_exitcode(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [status] = kwparse::<1>("waitstatus_to_exitcode", &["status"], 0, 1, 0, args, kw)?;
    let s = c_int(status.unwrap())?;
    if libc::WIFEXITED(s) {
        return Ok(i(libc::WEXITSTATUS(s) as i64));
    }
    if libc::WIFSIGNALED(s) {
        return Ok(i(-(libc::WTERMSIG(s) as i64)));
    }
    raise!(ValueError, "invalid wait status: {}", s);
}

macro_rules! wfunc {
    ($fname:ident, $pyname:literal, $f:path, bool) => {
        pub fn $fname(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
            let [status] = kwparse::<1>($pyname, &["status"], 0, 1, 0, args, kw)?;
            let s = c_int(status.unwrap())?;
            Ok(bool_::new_bool($f(s)))
        }
    };
    ($fname:ident, $pyname:literal, $f:path, int) => {
        pub fn $fname(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
            let [status] = kwparse::<1>($pyname, &["status"], 0, 1, 0, args, kw)?;
            let s = c_int(status.unwrap())?;
            Ok(i($f(s) as i64))
        }
    };
}
wfunc!(os_wcoredump, "WCOREDUMP", libc::WCOREDUMP, bool);
wfunc!(os_wifcontinued, "WIFCONTINUED", libc::WIFCONTINUED, bool);
wfunc!(os_wifstopped, "WIFSTOPPED", libc::WIFSTOPPED, bool);
wfunc!(os_wifsignaled, "WIFSIGNALED", libc::WIFSIGNALED, bool);
wfunc!(os_wifexited, "WIFEXITED", libc::WIFEXITED, bool);
wfunc!(os_wexitstatus, "WEXITSTATUS", libc::WEXITSTATUS, int);
wfunc!(os_wtermsig, "WTERMSIG", libc::WTERMSIG, int);
wfunc!(os_wstopsig, "WSTOPSIG", libc::WSTOPSIG, int);

pub fn os_system(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [cmd] = kwparse::<1>("system", &["command"], 0, 1, 0, args, kw)?;
    let c = super::path::path_named("system", "command", cmd.unwrap())?;
    let r = crate::threads::blocking(|| unsafe { libc::system(c.ptr()) });
    Ok(i(r as i64))
}

// ------------------------------------------------------------------ posix_spawn

fn spawn_impl(fname: &str, use_path: bool, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [path, argv, env, file_actions, setpgroup, resetids, setsid, setsigmask, setsigdef, scheduler] = kwparse::<10>(
        fname,
        &[
            "path",
            "argv",
            "env",
            "file_actions",
            "setpgroup",
            "resetids",
            "setsid",
            "setsigmask",
            "setsigdef",
            "scheduler",
        ],
        3,
        3,
        7,
        args,
        kw,
    )?;
    let p = super::path::path(fname, path.unwrap())?;
    let argv = argv_list(fname, argv.unwrap(), "argv")?;
    if argv.is_empty() {
        raise!(ValueError, "{}: argv must not be empty", fname);
    }
    let envv = env_list(fname, env.unwrap())?;
    let av = ptr_array(&argv);
    let ev = ptr_array(&envv);
    unsafe {
        let mut fa: libc::posix_spawn_file_actions_t = std::mem::zeroed();
        let mut attr: libc::posix_spawnattr_t = std::mem::zeroed();
        libc::posix_spawn_file_actions_init(&mut fa);
        libc::posix_spawnattr_init(&mut attr);
        struct Cleanup(*mut libc::posix_spawn_file_actions_t, *mut libc::posix_spawnattr_t);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                unsafe {
                    libc::posix_spawn_file_actions_destroy(self.0);
                    libc::posix_spawnattr_destroy(self.1);
                }
            }
        }
        let _c = Cleanup(&mut fa, &mut attr);
        let mut keep: Vec<CString> = Vec::new();
        let mut flags: i32 = 0;
        if let Some(fas) = file_actions.filter(|x| !x.is(none::none())) {
            let seq = ab::collect_iterable(fas)?;
            for a in &seq {
                if !tuple::is_tuple(a) || tuple::tuple_len(a) == 0 {
                    raise!(TypeError, "Each file_actions element must be a non-empty tuple");
                }
                let items = tuple::tuple_items(a);
                let tag = c_int(items[0])?;
                let r = match tag {
                    0 => {
                        // POSIX_SPAWN_OPEN: (tag, fd, path, flags, mode)
                        if items.len() != 5 {
                            raise!(TypeError, "A open file_action tuple must have 5 elements");
                        }
                        let fd = c_int(items[1])?;
                        let pth = super::path::path(fname, items[2])?;
                        let fl = c_int(items[3])?;
                        let md = c_int(items[4])?;
                        let r = libc::posix_spawn_file_actions_addopen(&mut fa, fd, pth.ptr(), fl, md as u32);
                        keep.push(pth.c);
                        r
                    }
                    1 => {
                        if items.len() != 2 {
                            raise!(TypeError, "A close file_action tuple must have 2 elements");
                        }
                        libc::posix_spawn_file_actions_addclose(&mut fa, c_int(items[1])?)
                    }
                    2 => {
                        if items.len() != 3 {
                            raise!(TypeError, "A dup2 file_action tuple must have 3 elements");
                        }
                        libc::posix_spawn_file_actions_adddup2(&mut fa, c_int(items[1])?, c_int(items[2])?)
                    }
                    _ => raise!(TypeError, "Unknown file_actions identifier"),
                };
                if r != 0 {
                    return Err(crate::types::exceptions::os_error_from_errno(r, None, None));
                }
            }
        }
        if let Some(pg) = setpgroup.filter(|x| !x.is(none::none())) {
            libc::posix_spawnattr_setpgroup(&mut attr, c_int(pg)?);
            flags |= libc::POSIX_SPAWN_SETPGROUP;
        }
        if truthy(resetids, false)? {
            flags |= libc::POSIX_SPAWN_RESETIDS;
        }
        if truthy(setsid, false)? {
            flags |= 0x80; // POSIX_SPAWN_SETSID
        }
        for (o, is_mask) in [(setsigmask, true), (setsigdef, false)] {
            if let Some(o) = o {
                let sigs = ab::collect_iterable(o)?;
                if sigs.is_empty() && is_mask {
                    // an empty mask still means "set the mask" only if given explicitly
                }
                let mut set: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut set);
                for s in &sigs {
                    let n = c_int(s)?;
                    if n <= 0 || n >= 65 {
                        raise!(ValueError, "signal number {} out of range [1; 64]", n);
                    }
                    libc::sigaddset(&mut set, n);
                }
                if is_mask {
                    libc::posix_spawnattr_setsigmask(&mut attr, &set);
                    flags |= libc::POSIX_SPAWN_SETSIGMASK;
                } else {
                    libc::posix_spawnattr_setsigdefault(&mut attr, &set);
                    flags |= libc::POSIX_SPAWN_SETSIGDEF;
                }
            }
        }
        if let Some(sch) = scheduler.filter(|x| !x.is(none::none())) {
            if !tuple::is_tuple(sch) || tuple::tuple_len(sch) != 2 {
                raise!(TypeError, "{}: scheduler must be a tuple or None", fname);
            }
            let pol = tuple::tuple_get(sch, 0);
            let par = sched_param_arg(tuple::tuple_get(sch, 1))?;
            if !pol.is(none::none()) {
                libc::posix_spawnattr_setschedpolicy(&mut attr, c_int(pol)?);
                flags |= libc::POSIX_SPAWN_SETSCHEDULER;
            }
            libc::posix_spawnattr_setschedparam(&mut attr, &par);
            flags |= libc::POSIX_SPAWN_SETSCHEDPARAM;
        }
        libc::posix_spawnattr_setflags(&mut attr, flags as i16);
        let mut pid: libc::pid_t = 0;
        let r = if use_path {
            libc::posix_spawnp(&mut pid, p.ptr(), &fa, &attr, av.as_ptr() as *const *mut _, ev.as_ptr() as *const *mut _)
        } else {
            libc::posix_spawn(&mut pid, p.ptr(), &fa, &attr, av.as_ptr() as *const *mut _, ev.as_ptr() as *const *mut _)
        };
        drop(keep);
        if r != 0 {
            return Err(p.err(r));
        }
        Ok(i(pid as i64))
    }
}

pub fn os_posix_spawn(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    spawn_impl("posix_spawn", false, args, kw)
}
pub fn os_posix_spawnp(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    spawn_impl("posix_spawnp", true, args, kw)
}

pub fn _unused(_d: &PyObject) -> PyResult<()> {
    let _ = dict::new_dict();
    Ok(())
}
