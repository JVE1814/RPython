//! The `resource` module (Linux).

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::structseq::{self, Desc, Field};
use crate::types::typeobject::{tname, type_ref};
use crate::types::{dict, exceptions, float, int, tuple};
use crate::{mdefs, raise};
use std::sync::atomic::{AtomicPtr, Ordering};

static RUSAGE_TYPE: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());

const RLIM_NLIMITS: i64 = 16;

fn os_err() -> PyErr {
    crate::modules::posix::path::os_err()
}

fn c_int_arg(o: &PyObject) -> PyResult<i32> {
    if crate::types::float::is_float(o) {
        raise!(TypeError, "'float' object cannot be interpreted as an integer");
    }
    if !ab::has_index(o) {
        raise!(TypeError, "'{}' object cannot be interpreted as an integer", tname(o));
    }
    let i = ab::index(o)?;
    match int::as_i64(&i) {
        Some(v) if v >= i32::MIN as i64 && v <= i32::MAX as i64 => Ok(v as i32),
        Some(v) if v > 0 => raise!(OverflowError, "signed integer is greater than maximum"),
        Some(_) => raise!(OverflowError, "signed integer is less than minimum"),
        None => raise!(OverflowError, "Python int too large to convert to C long"),
    }
}

fn resource_arg(o: &PyObject) -> PyResult<i32> {
    let r = c_int_arg(o)?;
    if r < 0 || r as i64 >= RLIM_NLIMITS {
        raise!(ValueError, "invalid resource specified");
    }
    Ok(r)
}

fn rlim_to_py(v: libc::rlim_t) -> PyRef {
    int::new_int_u128(v as u128)
}

fn py_to_rlim(o: &PyObject) -> PyResult<libc::rlim_t> {
    if !int::is_int(o) {
        if crate::types::float::is_float(o) || !ab::has_index(o) {
            raise!(TypeError, "'{}' object cannot be interpreted as an integer", tname(o));
        }
    }
    let i = ab::index(o)?;
    match crate::modules::devtools::int_as_i128(&i) {
        Some(v) if v >= 0 && v <= u64::MAX as i128 => Ok(v as libc::rlim_t),
        Some(v) if v < 0 => {
            let inf = libc::RLIM_INFINITY as i128;
            if v == (inf & 0xffff) - 0x10000 {
                crate::modules::warnings::warn(
                    exceptions::DeprecationWarning(),
                    "Use RLIM_INFINITY instead of negative limit value.",
                    1,
                )?;
                return Ok(libc::RLIM_INFINITY);
            }
            raise!(ValueError, "Cannot convert negative int")
        }
        Some(_) => raise!(OverflowError, "Python int too large to convert to C unsigned long long"),
        None => {
            if int::sign(&i) < 0 {
                raise!(ValueError, "Cannot convert negative int")
            }
            raise!(OverflowError, "Python int too large to convert to C unsigned long long")
        }
    }
}

fn limits_arg(o: &PyObject) -> PyResult<libc::rlimit> {
    let t = ab::sequence_tuple(o)?;
    if tuple::tuple_len(&t) != 2 {
        raise!(ValueError, "expected a tuple of 2 integers");
    }
    let cur = py_to_rlim(tuple::tuple_get(&t, 0))?;
    let max = py_to_rlim(tuple::tuple_get(&t, 1))?;
    Ok(libc::rlimit { rlim_cur: cur, rlim_max: max })
}

fn limits_obj(r: &libc::rlimit) -> PyRef {
    tuple::new_tuple(vec![rlim_to_py(r.rlim_cur), rlim_to_py(r.rlim_max)])
}

fn r_getrlimit(_m: &PyObject, arg: &PyObject) -> PyResult {
    let res = resource_arg(arg)?;
    let mut rl: libc::rlimit = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrlimit(res as _, &mut rl) } == -1 {
        return Err(os_err());
    }
    Ok(limits_obj(&rl))
}

fn setrlimit_error() -> PyErr {
    let e = crate::modules::posix::path::errno();
    if e == libc::EINVAL {
        value_error("current limit exceeds maximum limit")
    } else if e == libc::EPERM {
        value_error("not allowed to raise maximum limit")
    } else {
        crate::types::exceptions::os_error_from_errno(e, None, None)
    }
}

fn r_setrlimit(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("setrlimit", args, 2, 2)?;
    let res = resource_arg(args[0])?;
    let rl = limits_arg(args[1])?;
    if unsafe { libc::setrlimit(res as _, &rl) } == -1 {
        return Err(setrlimit_error());
    }
    Ok(crate::types::none::none_ref())
}

fn r_prlimit(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("prlimit", args, 2, 3)?;
    let pid = c_int_arg(args[0])?;
    let res = resource_arg(args[1])?;
    let new = match args.get(2) {
        Some(l) if !l.is(crate::types::none::none()) => Some(limits_arg(l)?),
        _ => None,
    };
    let mut old: libc::rlimit = unsafe { std::mem::zeroed() };
    let np = match &new {
        Some(r) => r as *const libc::rlimit,
        None => std::ptr::null(),
    };
    if unsafe { libc::prlimit(pid, res as _, np, &mut old) } == -1 {
        let e = crate::modules::posix::path::errno();
        if e == libc::EINVAL {
            raise!(ValueError, "current limit exceeds maximum limit");
        }
        return Err(crate::types::exceptions::os_error_from_errno(e, None, None));
    }
    Ok(limits_obj(&old))
}

fn tv(t: &libc::timeval) -> PyRef {
    float::new_float(t.tv_sec as f64 + t.tv_usec as f64 * 1e-6)
}

fn r_getrusage(_m: &PyObject, arg: &PyObject) -> PyResult {
    let who = c_int_arg(arg)?;
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(who, &mut ru) } == -1 {
        if crate::modules::posix::path::errno() == libc::EINVAL {
            raise!(ValueError, "invalid who parameter");
        }
        return Err(os_err());
    }
    let l = |v: libc::c_long| int::new_int(v as i64);
    let vals = vec![
        tv(&ru.ru_utime),
        tv(&ru.ru_stime),
        l(ru.ru_maxrss),
        l(ru.ru_ixrss),
        l(ru.ru_idrss),
        l(ru.ru_isrss),
        l(ru.ru_minflt),
        l(ru.ru_majflt),
        l(ru.ru_nswap),
        l(ru.ru_inblock),
        l(ru.ru_oublock),
        l(ru.ru_msgsnd),
        l(ru.ru_msgrcv),
        l(ru.ru_nsignals),
        l(ru.ru_nvcsw),
        l(ru.ru_nivcsw),
    ];
    Ok(structseq::new_instance(RUSAGE_TYPE.load(Ordering::Acquire), vals))
}

fn r_getpagesize(_m: &PyObject) -> PyResult {
    Ok(int::new_int(unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as i64))
}

static RUSAGE_FIELDS: &[Field] = &[
    Field::new("ru_utime", "user time used"),
    Field::new("ru_stime", "system time used"),
    Field::new("ru_maxrss", "max. resident set size"),
    Field::new("ru_ixrss", "shared memory size"),
    Field::new("ru_idrss", "unshared data size"),
    Field::new("ru_isrss", "unshared stack size"),
    Field::new("ru_minflt", "page faults not requiring I/O"),
    Field::new("ru_majflt", "page faults requiring I/O"),
    Field::new("ru_nswap", "number of swap outs"),
    Field::new("ru_inblock", "block input operations"),
    Field::new("ru_oublock", "block output operations"),
    Field::new("ru_msgsnd", "IPC messages sent"),
    Field::new("ru_msgrcv", "IPC messages received"),
    Field::new("ru_nsignals", "signals received"),
    Field::new("ru_nvcsw", "voluntary context switches"),
    Field::new("ru_nivcsw", "involuntary context switches"),
];

pub fn init() -> PyResult {
    if RUSAGE_TYPE.load(Ordering::Acquire).is_null() {
        let t = structseq::new_type(&Desc {
            name: "resource.struct_rusage",
            doc: "struct_rusage: Result from getrusage.\n\nThis object may be accessed either as a tuple of\n    (utime,stime,maxrss,ixrss,idrss,isrss,minflt,majflt,\n    nswap,inblock,oublock,msgsnd,msgrcv,nsignals,nvcsw,nivcsw)\nor via the attributes ru_utime, ru_stime, ru_maxrss, and so on.",
            fields: RUSAGE_FIELDS,
            n_in_sequence: 16,
        })?;
        RUSAGE_TYPE.store(t, Ordering::Release);
    }
    let methods = mdefs![
        ("getrusage", O, r_getrusage, "getrusage($module, who, /)\n--\n\n"),
        ("getrlimit", O, r_getrlimit, "getrlimit($module, resource, /)\n--\n\n"),
        ("prlimit", FAST, r_prlimit, "prlimit($module, pid, resource, limits=None, /)\n--\n\n"),
        ("setrlimit", FAST, r_setrlimit, "setrlimit($module, resource, limits, /)\n--\n\n"),
        ("getpagesize", NOARGS, r_getpagesize, "getpagesize($module, /)\n--\n\n"),
    ];
    let (m, d) = crate::modules::fncommon::new_module("resource", methods, None)?;
    dict::set_item_str(&d, "error", &type_ref(exceptions::OSError()))?;
    dict::set_item_str(&d, "struct_rusage", &type_ref(RUSAGE_TYPE.load(Ordering::Acquire)))?;
    let consts: &[(&str, i64)] = &[
        ("RLIMIT_CPU", 0),
        ("RLIMIT_FSIZE", 1),
        ("RLIMIT_DATA", 2),
        ("RLIMIT_STACK", 3),
        ("RLIMIT_CORE", 4),
        ("RLIMIT_RSS", 5),
        ("RLIMIT_NPROC", 6),
        ("RLIMIT_NOFILE", 7),
        ("RLIMIT_OFILE", 7),
        ("RLIMIT_MEMLOCK", 8),
        ("RLIMIT_AS", 9),
        ("RLIMIT_LOCKS", 10),
        ("RLIMIT_SIGPENDING", 11),
        ("RLIMIT_MSGQUEUE", 12),
        ("RLIMIT_NICE", 13),
        ("RLIMIT_RTPRIO", 14),
        ("RLIMIT_RTTIME", 15),
        ("RUSAGE_SELF", 0),
        ("RUSAGE_CHILDREN", -1),
        ("RUSAGE_THREAD", 1),
    ];
    for (n, v) in consts {
        dict::set_item_str(&d, n, &int::new_int(*v))?;
    }
    dict::set_item_str(&d, "RLIM_INFINITY", &rlim_to_py(libc::RLIM_INFINITY))?;
    Ok(m)
}
