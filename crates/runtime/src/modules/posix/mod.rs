//! The `posix` module (Linux).
pub mod fs;
pub mod path;
pub mod proc;
pub mod scandir;
pub mod stat;

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::{PyObject, PyTypeObject};
use crate::object::PyRef;
use crate::raise;
use crate::types::structseq::{self, Desc, Field};
use crate::types::typeobject::tname;
use crate::types::{bytes, dict, int, list, module, none, str as pystr, tuple};
use path::*;
use std::sync::atomic::{AtomicPtr, Ordering};

/// Keyword-aware argument parsing with a runtime-built `Params`.
pub fn kwparse<'b, const N: usize>(
    fname: &str,
    names: &[&str],
    posonly: usize,
    required: usize,
    kwonly: usize,
    args: &[&'b PyObject],
    kw: Option<&'b PyObject>,
) -> PyResult<[Option<&'b PyObject>; N]> {
    let p = crate::args::Params::new(fname, names, posonly, required).kwonly(kwonly);
    crate::args::parse::<N>(&p, args, kw)
}

#[inline]
pub fn typeptr(a: &AtomicPtr<PyTypeObject>) -> *mut PyTypeObject {
    a.load(Ordering::Acquire)
}

/// Called in the child after fork(); resets interpreter state that cannot survive fork.
pub fn after_fork_child_hook() {
    // JIT code memory must not stay shared with the parent.
    crate::interp::jit::mem::after_fork_child();
    // locks owned by threads that do not exist in the child, then those threads'
    // thread-local values (their finalizers may warn, which takes the warnings lock)
    crate::modules::_warnings::after_fork_child();
    crate::modules::_thread::clear_foreign_thread_locals(crate::threads::current().c.id);
}

pub static TERMINAL_SIZE: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());

fn os_get_terminal_size(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("get_terminal_size", args, 0, 1)?;
    let fd = if args.is_empty() { 1 } else { c_int(args[0])? };
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } != 0 {
        return Err(os_err());
    }
    Ok(structseq::new_instance(
        typeptr(&TERMINAL_SIZE),
        vec![int::new_int(ws.ws_col as i64), int::new_int(ws.ws_row as i64)],
    ))
}

// ------------------------------------------------------------------ environment

fn env_bytes_arg(fname: &str, o: &PyObject, what: &str) -> PyResult<Vec<u8>> {
    let p = path_converter(&PathSpec { func: fname, arg: what, allow_fd: false, nullable: false }, o)?;
    Ok(p.bytes().to_vec())
}

fn os_putenv(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("putenv", args, 2, 2)?;
    let k = env_bytes_arg("putenv", args[0], "name")?;
    let v = env_bytes_arg("putenv", args[1], "value")?;
    if k.is_empty() || k.contains(&b'=') {
        raise!(ValueError, "illegal environment variable name");
    }
    let kc = std::ffi::CString::new(k).unwrap();
    let vc = std::ffi::CString::new(v).unwrap();
    if unsafe { libc::setenv(kc.as_ptr(), vc.as_ptr(), 1) } != 0 {
        return Err(os_err());
    }
    Ok(none::none_ref())
}

fn os_unsetenv(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("unsetenv", args, 1, 1)?;
    let k = env_bytes_arg("unsetenv", args[0], "name")?;
    if k.is_empty() || k.contains(&b'=') {
        raise!(ValueError, "illegal environment variable name");
    }
    let kc = std::ffi::CString::new(k).unwrap();
    if unsafe { libc::unsetenv(kc.as_ptr()) } != 0 {
        return Err(os_err());
    }
    Ok(none::none_ref())
}

fn build_environ() -> PyResult {
    let d = dict::new_dict();
    unsafe {
        unsafe extern "C" {
            static environ: *const *const libc::c_char;
        }
        let mut p = environ;
        if p.is_null() {
            return Ok(d);
        }
        while !(*p).is_null() {
            let s = std::ffi::CStr::from_ptr(*p).to_bytes();
            if let Some(eq) = s.iter().skip(1).position(|&c| c == b'=').map(|x| x + 1) {
                let k = bytes::new_bytes(&s[..eq]);
                if dict::get_item(&d, &k)?.is_none() {
                    dict::set_item(&d, &k, &bytes::new_bytes(&s[eq + 1..]))?;
                }
            }
            p = p.add(1);
        }
    }
    Ok(d)
}

fn os_create_environ(_m: &PyObject) -> PyResult {
    build_environ()
}

// ------------------------------------------------------------------ sysconf & co.

const SYSCONF_NAMES: &[(&str, i32)] = &[
    ("SC_ARG_MAX", libc::_SC_ARG_MAX),
    ("SC_CHILD_MAX", libc::_SC_CHILD_MAX),
    ("SC_CLK_TCK", libc::_SC_CLK_TCK),
    ("SC_NGROUPS_MAX", libc::_SC_NGROUPS_MAX),
    ("SC_OPEN_MAX", libc::_SC_OPEN_MAX),
    ("SC_STREAM_MAX", libc::_SC_STREAM_MAX),
    ("SC_TZNAME_MAX", libc::_SC_TZNAME_MAX),
    ("SC_JOB_CONTROL", libc::_SC_JOB_CONTROL),
    ("SC_SAVED_IDS", libc::_SC_SAVED_IDS),
    ("SC_REALTIME_SIGNALS", libc::_SC_REALTIME_SIGNALS),
    ("SC_PRIORITY_SCHEDULING", libc::_SC_PRIORITY_SCHEDULING),
    ("SC_TIMERS", libc::_SC_TIMERS),
    ("SC_ASYNCHRONOUS_IO", libc::_SC_ASYNCHRONOUS_IO),
    ("SC_PRIORITIZED_IO", libc::_SC_PRIORITIZED_IO),
    ("SC_SYNCHRONIZED_IO", libc::_SC_SYNCHRONIZED_IO),
    ("SC_FSYNC", libc::_SC_FSYNC),
    ("SC_MAPPED_FILES", libc::_SC_MAPPED_FILES),
    ("SC_MEMLOCK", libc::_SC_MEMLOCK),
    ("SC_MEMLOCK_RANGE", libc::_SC_MEMLOCK_RANGE),
    ("SC_MEMORY_PROTECTION", libc::_SC_MEMORY_PROTECTION),
    ("SC_MESSAGE_PASSING", libc::_SC_MESSAGE_PASSING),
    ("SC_SEMAPHORES", libc::_SC_SEMAPHORES),
    ("SC_SHARED_MEMORY_OBJECTS", libc::_SC_SHARED_MEMORY_OBJECTS),
    ("SC_AIO_LISTIO_MAX", libc::_SC_AIO_LISTIO_MAX),
    ("SC_AIO_MAX", libc::_SC_AIO_MAX),
    ("SC_AIO_PRIO_DELTA_MAX", libc::_SC_AIO_PRIO_DELTA_MAX),
    ("SC_DELAYTIMER_MAX", libc::_SC_DELAYTIMER_MAX),
    ("SC_MQ_OPEN_MAX", libc::_SC_MQ_OPEN_MAX),
    ("SC_MQ_PRIO_MAX", libc::_SC_MQ_PRIO_MAX),
    ("SC_VERSION", libc::_SC_VERSION),
    ("SC_PAGESIZE", libc::_SC_PAGESIZE),
    ("SC_PAGE_SIZE", libc::_SC_PAGE_SIZE),
    ("SC_RTSIG_MAX", libc::_SC_RTSIG_MAX),
    ("SC_SEM_NSEMS_MAX", libc::_SC_SEM_NSEMS_MAX),
    ("SC_SEM_VALUE_MAX", libc::_SC_SEM_VALUE_MAX),
    ("SC_SIGQUEUE_MAX", libc::_SC_SIGQUEUE_MAX),
    ("SC_TIMER_MAX", libc::_SC_TIMER_MAX),
    ("SC_BC_BASE_MAX", libc::_SC_BC_BASE_MAX),
    ("SC_BC_DIM_MAX", libc::_SC_BC_DIM_MAX),
    ("SC_BC_SCALE_MAX", libc::_SC_BC_SCALE_MAX),
    ("SC_BC_STRING_MAX", libc::_SC_BC_STRING_MAX),
    ("SC_COLL_WEIGHTS_MAX", libc::_SC_COLL_WEIGHTS_MAX),
    ("SC_EXPR_NEST_MAX", libc::_SC_EXPR_NEST_MAX),
    ("SC_LINE_MAX", libc::_SC_LINE_MAX),
    ("SC_RE_DUP_MAX", libc::_SC_RE_DUP_MAX),
    ("SC_2_VERSION", libc::_SC_2_VERSION),
    ("SC_2_C_BIND", libc::_SC_2_C_BIND),
    ("SC_2_C_DEV", libc::_SC_2_C_DEV),
    ("SC_2_FORT_DEV", libc::_SC_2_FORT_DEV),
    ("SC_2_FORT_RUN", libc::_SC_2_FORT_RUN),
    ("SC_2_SW_DEV", libc::_SC_2_SW_DEV),
    ("SC_2_LOCALEDEF", libc::_SC_2_LOCALEDEF),
    ("SC_UIO_MAXIOV", libc::_SC_UIO_MAXIOV),
    ("SC_IOV_MAX", libc::_SC_IOV_MAX),
    ("SC_THREADS", libc::_SC_THREADS),
    ("SC_THREAD_SAFE_FUNCTIONS", libc::_SC_THREAD_SAFE_FUNCTIONS),
    ("SC_GETGR_R_SIZE_MAX", libc::_SC_GETGR_R_SIZE_MAX),
    ("SC_GETPW_R_SIZE_MAX", libc::_SC_GETPW_R_SIZE_MAX),
    ("SC_LOGIN_NAME_MAX", libc::_SC_LOGIN_NAME_MAX),
    ("SC_TTY_NAME_MAX", libc::_SC_TTY_NAME_MAX),
    ("SC_THREAD_DESTRUCTOR_ITERATIONS", libc::_SC_THREAD_DESTRUCTOR_ITERATIONS),
    ("SC_THREAD_KEYS_MAX", libc::_SC_THREAD_KEYS_MAX),
    ("SC_THREAD_STACK_MIN", libc::_SC_THREAD_STACK_MIN),
    ("SC_THREAD_THREADS_MAX", libc::_SC_THREAD_THREADS_MAX),
    ("SC_THREAD_ATTR_STACKADDR", libc::_SC_THREAD_ATTR_STACKADDR),
    ("SC_THREAD_ATTR_STACKSIZE", libc::_SC_THREAD_ATTR_STACKSIZE),
    ("SC_THREAD_PRIORITY_SCHEDULING", libc::_SC_THREAD_PRIORITY_SCHEDULING),
    ("SC_THREAD_PRIO_INHERIT", libc::_SC_THREAD_PRIO_INHERIT),
    ("SC_THREAD_PRIO_PROTECT", libc::_SC_THREAD_PRIO_PROTECT),
    ("SC_THREAD_PROCESS_SHARED", libc::_SC_THREAD_PROCESS_SHARED),
    ("SC_NPROCESSORS_CONF", libc::_SC_NPROCESSORS_CONF),
    ("SC_NPROCESSORS_ONLN", libc::_SC_NPROCESSORS_ONLN),
    ("SC_PHYS_PAGES", libc::_SC_PHYS_PAGES),
    ("SC_AVPHYS_PAGES", libc::_SC_AVPHYS_PAGES),
    ("SC_ATEXIT_MAX", libc::_SC_ATEXIT_MAX),
    ("SC_PASS_MAX", libc::_SC_PASS_MAX),
    ("SC_XOPEN_VERSION", libc::_SC_XOPEN_VERSION),
    ("SC_XOPEN_XCU_VERSION", libc::_SC_XOPEN_XCU_VERSION),
    ("SC_XOPEN_UNIX", libc::_SC_XOPEN_UNIX),
    ("SC_XOPEN_CRYPT", libc::_SC_XOPEN_CRYPT),
    ("SC_XOPEN_ENH_I18N", libc::_SC_XOPEN_ENH_I18N),
    ("SC_XOPEN_SHM", libc::_SC_XOPEN_SHM),
    ("SC_2_CHAR_TERM", libc::_SC_2_CHAR_TERM),
    ("SC_2_UPE", libc::_SC_2_UPE),
    ("SC_XOPEN_XPG2", libc::_SC_XOPEN_XPG2),
    ("SC_XOPEN_XPG3", libc::_SC_XOPEN_XPG3),
    ("SC_XOPEN_XPG4", libc::_SC_XOPEN_XPG4),
    ("SC_NZERO", libc::_SC_NZERO),
    ("SC_XBS5_ILP32_OFF32", libc::_SC_XBS5_ILP32_OFF32),
    ("SC_XBS5_ILP32_OFFBIG", libc::_SC_XBS5_ILP32_OFFBIG),
    ("SC_XBS5_LP64_OFF64", libc::_SC_XBS5_LP64_OFF64),
    ("SC_XBS5_LPBIG_OFFBIG", libc::_SC_XBS5_LPBIG_OFFBIG),
    ("SC_XOPEN_LEGACY", libc::_SC_XOPEN_LEGACY),
    ("SC_XOPEN_REALTIME", libc::_SC_XOPEN_REALTIME),
    ("SC_XOPEN_REALTIME_THREADS", libc::_SC_XOPEN_REALTIME_THREADS),
    ("SC_HOST_NAME_MAX", libc::_SC_HOST_NAME_MAX),
    ("SC_MINSIGSTKSZ", 249),
];

const CONFSTR_NAMES: &[(&str, i32)] = &[
    ("CS_PATH", 0),
    ("CS_GNU_LIBC_VERSION", 2),
    ("CS_GNU_LIBPTHREAD_VERSION", 3),
];

const PATHCONF_NAMES: &[(&str, i32)] = &[
    ("PC_LINK_MAX", 0),
    ("PC_MAX_CANON", 1),
    ("PC_MAX_INPUT", 2),
    ("PC_NAME_MAX", 3),
    ("PC_PATH_MAX", 4),
    ("PC_PIPE_BUF", 5),
    ("PC_CHOWN_RESTRICTED", 6),
    ("PC_NO_TRUNC", 7),
    ("PC_VDISABLE", 8),
    ("PC_SYNC_IO", 9),
    ("PC_ASYNC_IO", 10),
    ("PC_PRIO_IO", 11),
    ("PC_SOCK_MAXBUF", 12),
    ("PC_FILESIZEBITS", 13),
    ("PC_REC_INCR_XFER_SIZE", 14),
    ("PC_REC_MAX_XFER_SIZE", 15),
    ("PC_REC_MIN_XFER_SIZE", 16),
    ("PC_REC_XFER_ALIGN", 17),
    ("PC_ALLOC_SIZE_MIN", 18),
    ("PC_SYMLINK_MAX", 19),
    ("PC_2_SYMLINKS", 20),
];

fn conf_name(o: &PyObject, table: &[(&str, i32)]) -> PyResult<i32> {
    if int::is_int(o) {
        return c_int(o);
    }
    if pystr::is_str(o) {
        let s = pystr::to_string_lossy(o);
        return match table.iter().find(|(n, _)| *n == s) {
            Some((_, v)) => Ok(*v),
            None => raise!(ValueError, "unrecognized configuration name"),
        };
    }
    raise!(TypeError, "configuration names must be strings or integers");
}

fn os_sysconf(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("sysconf", args, 1, 1)?;
    let n = conf_name(args[0], SYSCONF_NAMES)?;
    unsafe { *libc::__errno_location() = 0 };
    let r = unsafe { libc::sysconf(n) };
    if r == -1 && errno() != 0 {
        return Err(os_err());
    }
    Ok(int::new_int(r as i64))
}

fn os_confstr(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("confstr", args, 1, 1)?;
    let n = conf_name(args[0], CONFSTR_NAMES)?;
    unsafe { *libc::__errno_location() = 0 };
    let len = unsafe { libc::confstr(n, std::ptr::null_mut(), 0) };
    if len == 0 {
        if errno() != 0 {
            return Err(os_err());
        }
        return Ok(none::none_ref());
    }
    let mut buf = vec![0u8; len];
    unsafe { libc::confstr(n, buf.as_mut_ptr() as *mut libc::c_char, len) };
    buf.truncate(len - 1);
    fs_decode(&buf)
}

fn os_fpathconf(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [fd, name] = kwparse::<2>("fpathconf", &["fd", "name"], 0, 2, 0, args, kw)?;
    let fd = as_file_descriptor(fd.unwrap())?;
    let n = conf_name(name.unwrap(), PATHCONF_NAMES)?;
    unsafe { *libc::__errno_location() = 0 };
    let r = unsafe { libc::fpathconf(fd, n) };
    if r == -1 && errno() != 0 {
        return Err(os_err());
    }
    Ok(int::new_int(r as i64))
}

fn os_pathconf(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [path, name] = kwparse::<2>("pathconf", &["path", "name"], 0, 2, 0, args, kw)?;
    let p = path_fd("pathconf", path.unwrap())?;
    let n = conf_name(name.unwrap(), PATHCONF_NAMES)?;
    unsafe { *libc::__errno_location() = 0 };
    let r = unsafe {
        match p.fd {
            Some(fd) => libc::fpathconf(fd, n),
            None => libc::pathconf(p.ptr(), n),
        }
    };
    if r == -1 && errno() != 0 {
        return Err(os_err_with(&p));
    }
    Ok(int::new_int(r as i64))
}

fn names_dict(table: &[(&str, i32)]) -> PyResult {
    let d = dict::new_dict();
    for (n, v) in table {
        dict::set_item_str(&d, n, &int::new_int(*v as i64))?;
    }
    Ok(d)
}

// ------------------------------------------------------------------ path helpers in C

/// posix._path_normpath(path): fast normpath for str/bytes.
fn os_path_normpath(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [p] = kwparse::<1>("_path_normpath", &["path"], 0, 1, 0, args, kw)?;
    let p = p.unwrap();
    let (data, narrow): (Vec<u32>, bool) = if pystr::is_str(p) {
        (pystr::str_data(p).iter().collect(), false)
    } else if bytes::is_bytes(p) {
        (bytes::bytes_data(p).iter().map(|&b| b as u32).collect(), true)
    } else {
        let f = fspath(p)?;
        return os_path_normpath(_m, &[&f], None);
    };
    let out = normpath_cps(&data);
    if narrow {
        let b: Vec<u8> = out.iter().map(|&c| c as u8).collect();
        Ok(bytes::new_bytes(&b))
    } else {
        Ok(pystr::from_codepoints(&out))
    }
}

/// posix._path_splitroot_ex(p) -> (drive, root, tail)
fn os_path_splitroot_ex(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let [p] = kwparse::<1>("_path_splitroot_ex", &["p"], 0, 1, 0, args, kw)?;
    let p = p.unwrap();
    let p = if pystr::is_str(p) || bytes::is_bytes(p) { p.to_ref() } else { fspath(p)? };
    let narrow = bytes::is_bytes(&p);
    let n = if narrow { bytes::bytes_data(&p).len() } else { pystr::str_len(&p) };
    let at = |i: usize| -> u32 {
        if narrow { bytes::bytes_data(&p)[i] as u32 } else { pystr::str_data(&p).get(i) }
    };
    let slash = '/' as u32;
    let rootlen = if n > 0 && at(0) == slash {
        if n > 1 && at(1) == slash && !(n > 2 && at(2) == slash) { 2 } else { 1 }
    } else {
        0
    };
    let mk = |a: usize, b: usize| -> PyRef {
        if narrow {
            bytes::new_bytes(&bytes::bytes_data(&p)[a..b])
        } else {
            let cps: Vec<u32> = (a..b).map(|i| pystr::str_data(&p).get(i)).collect();
            pystr::from_codepoints(&cps)
        }
    };
    Ok(tuple::new_tuple(vec![mk(0, 0), mk(0, rootlen), mk(rootlen, n)]))
}

pub fn normpath_cps(p: &[u32]) -> Vec<u32> {
    const SLASH: u32 = '/' as u32;
    const DOT: u32 = '.' as u32;
    if p.is_empty() {
        return vec![DOT];
    }
    let initial = if p.starts_with(&[SLASH]) {
        if p.starts_with(&[SLASH, SLASH]) && !p.starts_with(&[SLASH, SLASH, SLASH]) { 2 } else { 1 }
    } else {
        0
    };
    let mut comps: Vec<&[u32]> = Vec::new();
    for c in p.split(|&c| c == SLASH) {
        if c.is_empty() || c == [DOT] {
            continue;
        }
        if c == [DOT, DOT] {
            if (initial == 0 && comps.is_empty()) || comps.last().map_or(false, |l| *l == [DOT, DOT]) {
                comps.push(c);
            } else if !comps.is_empty() {
                comps.pop();
            }
            continue;
        }
        comps.push(c);
    }
    let mut out: Vec<u32> = vec![SLASH; initial];
    for (i, c) in comps.iter().enumerate() {
        if i > 0 {
            out.push(SLASH);
        }
        out.extend_from_slice(c);
    }
    if out.is_empty() {
        out.push(DOT);
    }
    out
}

// ------------------------------------------------------------------ module

fn have_functions() -> PyRef {
    let names = [
        "HAVE_FACCESSAT",
        "HAVE_FCHDIR",
        "HAVE_FCHMOD",
        "HAVE_FCHMODAT",
        "HAVE_FCHOWN",
        "HAVE_FCHOWNAT",
        "HAVE_FDOPENDIR",
        "HAVE_FEXECVE",
        "HAVE_FPATHCONF",
        "HAVE_FSTATAT",
        "HAVE_FSTATVFS",
        "HAVE_FTRUNCATE",
        "HAVE_FUTIMENS",
        "HAVE_FUTIMES",
        "HAVE_FUTIMESAT",
        "HAVE_LINKAT",
        "HAVE_LCHOWN",
        "HAVE_LSTAT",
        "HAVE_LUTIMES",
        "HAVE_MKDIRAT",
        "HAVE_MKFIFOAT",
        "HAVE_MKNODAT",
        "HAVE_OPENAT",
        "HAVE_READLINKAT",
        "HAVE_RENAMEAT",
        "HAVE_SYMLINKAT",
        "HAVE_UNLINKAT",
        "HAVE_UTIMENSAT",
    ];
    list::new_list(names.iter().map(|n| pystr::new_str(n)).collect())
}

fn int_constants() -> Vec<(&'static str, i64)> {
    use libc::*;
    vec![
        ("F_OK", F_OK as i64),
        ("R_OK", R_OK as i64),
        ("W_OK", W_OK as i64),
        ("X_OK", X_OK as i64),
        ("NGROUPS_MAX", 65536),
        ("TMP_MAX", 238328),
        ("WCONTINUED", WCONTINUED as i64),
        ("WNOHANG", WNOHANG as i64),
        ("WUNTRACED", WUNTRACED as i64),
        ("WEXITED", WEXITED as i64),
        ("WSTOPPED", WSTOPPED as i64),
        ("WNOWAIT", WNOWAIT as i64),
        ("P_PID", P_PID as i64),
        ("P_PGID", P_PGID as i64),
        ("P_ALL", P_ALL as i64),
        ("P_PIDFD", 3),
        ("CLD_EXITED", CLD_EXITED as i64),
        ("CLD_KILLED", CLD_KILLED as i64),
        ("CLD_DUMPED", CLD_DUMPED as i64),
        ("CLD_TRAPPED", CLD_TRAPPED as i64),
        ("CLD_STOPPED", CLD_STOPPED as i64),
        ("CLD_CONTINUED", CLD_CONTINUED as i64),
        ("O_RDONLY", O_RDONLY as i64),
        ("O_WRONLY", O_WRONLY as i64),
        ("O_RDWR", O_RDWR as i64),
        ("O_NDELAY", O_NDELAY as i64),
        ("O_NONBLOCK", O_NONBLOCK as i64),
        ("O_APPEND", O_APPEND as i64),
        ("O_DSYNC", O_DSYNC as i64),
        ("O_RSYNC", O_RSYNC as i64),
        ("O_SYNC", O_SYNC as i64),
        ("O_NOCTTY", O_NOCTTY as i64),
        ("O_CREAT", O_CREAT as i64),
        ("O_EXCL", O_EXCL as i64),
        ("O_TRUNC", O_TRUNC as i64),
        ("O_LARGEFILE", 0),
        ("O_PATH", O_PATH as i64),
        ("O_TMPFILE", O_TMPFILE as i64),
        ("O_ASYNC", O_ASYNC as i64),
        ("O_DIRECT", O_DIRECT as i64),
        ("O_DIRECTORY", O_DIRECTORY as i64),
        ("O_NOFOLLOW", O_NOFOLLOW as i64),
        ("O_NOATIME", O_NOATIME as i64),
        ("O_CLOEXEC", O_CLOEXEC as i64),
        ("O_ACCMODE", O_ACCMODE as i64),
        ("O_FSYNC", O_FSYNC as i64),
        ("PRIO_PROCESS", PRIO_PROCESS as i64),
        ("PRIO_PGRP", PRIO_PGRP as i64),
        ("PRIO_USER", PRIO_USER as i64),
        ("EX_OK", 0),
        ("EX_USAGE", 64),
        ("EX_DATAERR", 65),
        ("EX_NOINPUT", 66),
        ("EX_NOUSER", 67),
        ("EX_NOHOST", 68),
        ("EX_UNAVAILABLE", 69),
        ("EX_SOFTWARE", 70),
        ("EX_OSERR", 71),
        ("EX_OSFILE", 72),
        ("EX_CANTCREAT", 73),
        ("EX_IOERR", 74),
        ("EX_TEMPFAIL", 75),
        ("EX_PROTOCOL", 76),
        ("EX_NOPERM", 77),
        ("EX_CONFIG", 78),
        ("ST_RDONLY", ST_RDONLY as i64),
        ("ST_NOSUID", ST_NOSUID as i64),
        ("ST_NODEV", ST_NODEV as i64),
        ("ST_NOEXEC", ST_NOEXEC as i64),
        ("ST_SYNCHRONOUS", ST_SYNCHRONOUS as i64),
        ("ST_MANDLOCK", ST_MANDLOCK as i64),
        ("ST_WRITE", ST_WRITE as i64),
        ("ST_APPEND", ST_APPEND as i64),
        ("ST_NOATIME", ST_NOATIME as i64),
        ("ST_NODIRATIME", ST_NODIRATIME as i64),
        ("ST_RELATIME", ST_RELATIME as i64),
        ("POSIX_FADV_NORMAL", POSIX_FADV_NORMAL as i64),
        ("POSIX_FADV_SEQUENTIAL", POSIX_FADV_SEQUENTIAL as i64),
        ("POSIX_FADV_RANDOM", POSIX_FADV_RANDOM as i64),
        ("POSIX_FADV_NOREUSE", POSIX_FADV_NOREUSE as i64),
        ("POSIX_FADV_WILLNEED", POSIX_FADV_WILLNEED as i64),
        ("POSIX_FADV_DONTNEED", POSIX_FADV_DONTNEED as i64),
        ("F_LOCK", F_LOCK as i64),
        ("F_TLOCK", F_TLOCK as i64),
        ("F_ULOCK", F_ULOCK as i64),
        ("F_TEST", F_TEST as i64),
        ("POSIX_SPAWN_OPEN", 0),
        ("POSIX_SPAWN_CLOSE", 1),
        ("POSIX_SPAWN_DUP2", 2),
        ("SCHED_OTHER", SCHED_OTHER as i64),
        ("SCHED_FIFO", SCHED_FIFO as i64),
        ("SCHED_RR", SCHED_RR as i64),
        ("SCHED_BATCH", SCHED_BATCH as i64),
        ("SCHED_IDLE", SCHED_IDLE as i64),
        ("SCHED_DEADLINE", 6), // (hang1) Linux value; libc does not export it
        ("NODEV", -1),         // (hang1) (dev_t)-1
        ("SCHED_RESET_ON_FORK", SCHED_RESET_ON_FORK as i64),
        ("RTLD_LAZY", RTLD_LAZY as i64),
        ("RTLD_NOW", RTLD_NOW as i64),
        ("RTLD_GLOBAL", RTLD_GLOBAL as i64),
        ("RTLD_LOCAL", RTLD_LOCAL as i64),
        ("RTLD_NODELETE", RTLD_NODELETE as i64),
        ("RTLD_NOLOAD", RTLD_NOLOAD as i64),
        ("RTLD_DEEPBIND", RTLD_DEEPBIND as i64),
        ("GRND_NONBLOCK", GRND_NONBLOCK as i64),
        ("GRND_RANDOM", GRND_RANDOM as i64),
        ("MFD_CLOEXEC", MFD_CLOEXEC as i64),
        ("MFD_ALLOW_SEALING", MFD_ALLOW_SEALING as i64),
        ("MFD_HUGETLB", MFD_HUGETLB as i64),
        ("EFD_CLOEXEC", EFD_CLOEXEC as i64),
        ("EFD_NONBLOCK", EFD_NONBLOCK as i64),
        ("EFD_SEMAPHORE", EFD_SEMAPHORE as i64),
        ("SEEK_SET", 0),
        ("SEEK_CUR", 1),
        ("SEEK_END", 2),
        ("SEEK_DATA", SEEK_DATA as i64),
        ("SEEK_HOLE", SEEK_HOLE as i64),
        ("PIDFD_NONBLOCK", O_NONBLOCK as i64),
        ("SPLICE_F_MOVE", 1),
        ("SPLICE_F_NONBLOCK", 2),
        ("SPLICE_F_MORE", 4),
    ]
}

fn methods() -> *mut crate::ffi::PyMethodDef {
    use fs::*;
    use proc::*;
    use scandir::*;
    use stat::*;
    crate::mdefs![
        ("login_tty", FAST, os_login_tty),
        ("_is_inputhook_installed", NOARGS, os_is_inputhook_installed, "_is_inputhook_installed($module, /)\n--\n\n"),
        ("_inputhook", NOARGS, os_inputhook, "_inputhook($module, /)\n--\n\n"),
        ("stat", FASTKW, os_stat, "stat($module, /, path, *, dir_fd=None, follow_symlinks=True)\n--\n\nReturn a stat_result for a path or file descriptor."),
        ("lstat", FASTKW, os_lstat, "lstat($module, /, path, *, dir_fd=None)\n--\n\nLike stat(), but do not follow a final symbolic link."),
        ("fstat", FASTKW, os_fstat, "fstat($module, /, fd)\n--\n\nReturn a stat_result for an open file descriptor."),
        ("statvfs", FASTKW, os_statvfs, "statvfs($module, /, path)\n--\n\nReturn a statvfs_result describing the file system holding path."),
        ("fstatvfs", FAST, os_fstatvfs, "fstatvfs($module, fd, /)\n--\n\nReturn a statvfs_result for the file system of an open descriptor."),
        ("open", FASTKW, os_open, "open($module, /, path, flags, mode=511, *, dir_fd=None)\n--\n\nOpen a file at the OS level and return its file descriptor."),
        ("close", FASTKW, os_close, "close($module, /, fd)\n--\n\nClose a file descriptor."),
        ("closerange", FAST, os_closerange, "closerange($module, fd_low, fd_high, /)\n--\n\nClose every descriptor from fd_low (inclusive) to fd_high (exclusive), ignoring errors."),
        ("dup", FAST, os_dup, "dup($module, fd, /)\n--\n\nReturn a non-inheritable duplicate of a file descriptor."),
        ("dup2", FASTKW, os_dup2, "dup2($module, /, fd, fd2, inheritable=True)\n--\n\nMake fd2 a copy of fd and return fd2."),
        ("read", FAST, os_read, "read($module, fd, length, /)\n--\n\nRead at most length bytes from a file descriptor."),
        ("readinto", FAST, os_readinto, "readinto($module, fd, buffer, /)\n--\n\nRead from a descriptor into a writable buffer; return the byte count."),
        ("pread", FAST, os_pread, "pread($module, fd, length, offset, /)\n--\n\nRead at most length bytes at offset without moving the file position."),
        ("write", FAST, os_write, "write($module, fd, data, /)\n--\n\nWrite a bytes-like object to a descriptor; return the byte count."),
        ("pwrite", FAST, os_pwrite, "pwrite($module, fd, str, offset, /)\n--\n\nWrite bytes at offset without moving the file position."),
        ("readv", FAST, os_readv, "readv($module, fd, buffers, /)\n--\n\nRead from a descriptor into a sequence of writable buffers."),
        ("writev", FAST, os_writev, "writev($module, fd, buffers, /)\n--\n\nWrite a sequence of bytes-like objects to a descriptor."),
        ("lseek", FAST, os_lseek, "lseek($module, fd, position, whence, /)\n--\n\nMove the position of a descriptor and return the new offset."),
        ("fsync", FASTKW, os_fsync, "fsync($module, /, fd)\n--\n\nFlush a file's data and metadata to disk."),
        ("fdatasync", FASTKW, os_fdatasync, "fdatasync($module, /, fd)\n--\n\nFlush a file's data (but not all metadata) to disk."),
        ("ftruncate", FAST, os_ftruncate, "ftruncate($module, fd, length, /)\n--\n\nResize an open file to length bytes."),
        ("truncate", FASTKW, os_truncate, "truncate($module, /, path, length)\n--\n\nResize the file at path to length bytes."),
        ("posix_fallocate", FAST, os_posix_fallocate, "posix_fallocate($module, fd, offset, len, /)\n--\n\nReserve disk space for the given byte range of a file."),
        ("posix_fadvise", FAST, os_posix_fadvise, "posix_fadvise($module, fd, offset, len, advice, /)\n--\n\nTell the kernel how a byte range of a file will be accessed."),
        ("pipe", NOARGS, os_pipe),
        ("pipe2", FAST, os_pipe2, "pipe2($module, flags, /)\n--\n\nCreate a pipe with the given O_* flags; return (read_fd, write_fd)."),
        ("isatty", FAST, os_isatty, "isatty($module, fd, /)\n--\n\nReturn True if the descriptor refers to a terminal."),
        ("ttyname", FAST, os_ttyname, "ttyname($module, fd, /)\n--\n\nReturn the name of the terminal a descriptor refers to."),
        ("openpty", NOARGS, os_openpty),
        ("get_inheritable", FAST, os_get_inheritable, "get_inheritable($module, fd, /)\n--\n\nReturn whether a descriptor is inherited by child processes."),
        ("set_inheritable", FAST, os_set_inheritable, "set_inheritable($module, fd, inheritable, /)\n--\n\nChoose whether a descriptor is inherited by child processes."),
        ("get_blocking", FAST, os_get_blocking, "get_blocking($module, fd, /)\n--\n\nReturn False if the descriptor is in non-blocking mode."),
        ("set_blocking", FAST, os_set_blocking, "set_blocking($module, fd, blocking, /)\n--\n\nSwitch a descriptor between blocking and non-blocking mode."),
        ("device_encoding", FASTKW, os_device_encoding, "device_encoding($module, /, fd)\n--\n\nReturn the encoding of the terminal behind fd, or None."),
        ("lockf", FAST, os_lockf, "lockf($module, fd, command, length, /)\n--\n\nApply, test or remove a POSIX lock on an open file."),
        ("sendfile", FASTKW, os_sendfile, "sendfile($module, /, out_fd, in_fd, offset, count)\n--\n\nCopy count bytes from in_fd to out_fd inside the kernel."),
        ("copy_file_range", FASTKW, os_copy_file_range, "copy_file_range($module, /, src, dst, count, offset_src=None, offset_dst=None)\n--\n\nCopy count bytes between two descriptors inside the kernel."),
        ("mkdir", FASTKW, os_mkdir, "mkdir($module, /, path, mode=511, *, dir_fd=None)\n--\n\nCreate a directory."),
        ("rmdir", FASTKW, os_rmdir, "rmdir($module, /, path, *, dir_fd=None)\n--\n\nRemove an empty directory."),
        ("unlink", FASTKW, os_unlink, "unlink($module, /, path, *, dir_fd=None)\n--\n\nRemove a file (not a directory)."),
        ("remove", FASTKW, os_remove, "remove($module, /, path, *, dir_fd=None)\n--\n\nRemove a file (same as unlink())."),
        ("rename", FASTKW, os_rename, "rename($module, /, src, dst, *, src_dir_fd=None, dst_dir_fd=None)\n--\n\nRename a file or directory."),
        ("replace", FASTKW, os_replace, "replace($module, /, src, dst, *, src_dir_fd=None, dst_dir_fd=None)\n--\n\nRename a file or directory, overwriting an existing destination."),
        ("link", FASTKW, os_link, "link($module, /, src, dst, *, src_dir_fd=None, dst_dir_fd=None,\n     follow_symlinks=(os.name != 'nt'))\n--\n\nCreate a hard link named dst pointing to src."),
        ("symlink", FASTKW, os_symlink, "symlink($module, /, src, dst, target_is_directory=False, *, dir_fd=None)\n--\n\nCreate a symbolic link named dst pointing to src."),
        ("readlink", FASTKW, os_readlink, "readlink($module, /, path, *, dir_fd=None)\n--\n\nReturn the target of a symbolic link."),
        ("chmod", FASTKW, os_chmod, "chmod($module, /, path, mode, *, dir_fd=None,\n      follow_symlinks=(os.name != 'nt'))\n--\n\nChange the permission bits of a path."),
        ("fchmod", FASTKW, os_fchmod, "fchmod($module, /, fd, mode)\n--\n\nChange the permission bits of an open file."),
        ("chown", FASTKW, os_chown, "chown($module, /, path, uid, gid, *, dir_fd=None, follow_symlinks=True)\n--\n\nChange the owner and group of a path (-1 leaves a value unchanged)."),
        ("lchown", FASTKW, os_lchown, "lchown($module, /, path, uid, gid)\n--\n\nLike chown(), but do not follow a final symbolic link."),
        ("fchown", FASTKW, os_fchown, "fchown($module, /, fd, uid, gid)\n--\n\nChange the owner and group of an open file."),
        ("utime", FASTKW, os_utime, "utime($module, /, path, times=None, *, ns=<unrepresentable>, dir_fd=None,\n      follow_symlinks=True)\n--\n\nSet the access and modification times of a path."),
        ("access", FASTKW, os_access, "access($module, /, path, mode, *, dir_fd=None, effective_ids=False,\n       follow_symlinks=True)\n--\n\nTest whether the caller may access a path with the given mode."),
        ("chdir", FASTKW, os_chdir, "chdir($module, /, path)\n--\n\nChange the current working directory."),
        ("fchdir", FASTKW, os_fchdir, "fchdir($module, /, fd)\n--\n\nChange the current working directory to an open directory."),
        ("chroot", FASTKW, os_chroot, "chroot($module, /, path)\n--\n\nChange the root directory of the process."),
        ("getcwd", NOARGS, os_getcwd),
        ("getcwdb", NOARGS, os_getcwdb),
        ("mkfifo", FASTKW, os_mkfifo, "mkfifo($module, /, path, mode=438, *, dir_fd=None)\n--\n\nCreate a named pipe."),
        ("mknod", FASTKW, os_mknod, "mknod($module, /, path, mode=384, device=0, *, dir_fd=None)\n--\n\nCreate a file system node (file, device file or named pipe)."),
        ("major", FAST, os_major, "major($module, device, /)\n--\n\nReturn the major number of a raw device number."),
        ("minor", FAST, os_minor, "minor($module, device, /)\n--\n\nReturn the minor number of a raw device number."),
        ("makedev", FAST, os_makedev, "makedev($module, major, minor, /)\n--\n\nCombine major and minor numbers into a raw device number."),
        ("sync", NOARGS, os_sync),
        ("umask", FAST, os_umask, "umask($module, mask, /)\n--\n\nSet the file creation mask and return the previous one."),
        ("listdir", FASTKW, os_listdir, "listdir($module, /, path=None)\n--\n\nReturn the names of the entries in a directory (default: '.')."),
        ("scandir", FASTKW, os_scandir, "scandir($module, /, path=None)\n--\n\nReturn an iterator of DirEntry objects for a directory."),
        ("fspath", FASTKW, os_fspath, "fspath($module, /, path)\n--\n\nReturn the file system representation of a path-like object."),
        ("memfd_create", FASTKW, os_memfd_create, "memfd_create($module, /, name, flags=MFD_CLOEXEC)\n--\n\nCreate an anonymous in-memory file and return its descriptor."),
        ("eventfd", FASTKW, os_eventfd, "eventfd($module, /, initval, flags=EFD_CLOEXEC)\n--\n\nCreate an event notification descriptor."),
        ("eventfd_read", FASTKW, os_eventfd_read, "eventfd_read($module, /, fd)\n--\n\nRead the counter of an eventfd."),
        ("eventfd_write", FASTKW, os_eventfd_write, "eventfd_write($module, /, fd, value)\n--\n\nAdd value to the counter of an eventfd."),
        ("getpid", NOARGS, os_getpid),
        ("getppid", NOARGS, os_getppid),
        ("getpgrp", NOARGS, os_getpgrp),
        ("getuid", NOARGS, os_getuid),
        ("geteuid", NOARGS, os_geteuid),
        ("getgid", NOARGS, os_getgid),
        ("getegid", NOARGS, os_getegid),
        ("getgroups", NOARGS, os_getgroups),
        ("setgroups", FAST, os_setgroups, "setgroups($module, groups, /)\n--\n\nSet the supplementary group ids of the process."),
        ("setuid", FAST, os_setuid, "setuid($module, uid, /)\n--\n\nSet the user id of the process."),
        ("seteuid", FAST, os_seteuid, "seteuid($module, euid, /)\n--\n\nSet the effective user id of the process."),
        ("setgid", FAST, os_setgid, "setgid($module, gid, /)\n--\n\nSet the group id of the process."),
        ("setegid", FAST, os_setegid, "setegid($module, egid, /)\n--\n\nSet the effective group id of the process."),
        ("setreuid", FAST, os_setreuid, "setreuid($module, ruid, euid, /)\n--\n\nSet the real and effective user ids of the process."),
        ("setregid", FAST, os_setregid, "setregid($module, rgid, egid, /)\n--\n\nSet the real and effective group ids of the process."),
        ("getresuid", NOARGS, os_getresuid),
        ("getresgid", NOARGS, os_getresgid),
        ("setresuid", FAST, os_setresuid, "setresuid($module, ruid, euid, suid, /)\n--\n\nSet the real, effective and saved user ids of the process."),
        ("setresgid", FAST, os_setresgid, "setresgid($module, rgid, egid, sgid, /)\n--\n\nSet the real, effective and saved group ids of the process."),
        ("getpgid", FAST, os_getpgid, "getpgid($module, /, pid)\n--\n\nReturn the process group id of a process."),
        ("setpgid", FAST, os_setpgid, "setpgid($module, pid, pgrp, /)\n--\n\nMove a process into a process group."),
        ("setpgrp", NOARGS, os_setpgrp),
        ("getsid", FAST, os_getsid, "getsid($module, pid, /)\n--\n\nReturn the session id of a process."),
        ("setsid", NOARGS, os_setsid),
        ("tcgetpgrp", FAST, os_tcgetpgrp, "tcgetpgrp($module, fd, /)\n--\n\nReturn the foreground process group of the terminal behind fd."),
        ("tcsetpgrp", FAST, os_tcsetpgrp, "tcsetpgrp($module, fd, pgid, /)\n--\n\nSet the foreground process group of the terminal behind fd."),
        ("getlogin", NOARGS, os_getlogin),
        ("kill", FAST, os_kill, "kill($module, pid, signal, /)\n--\n\nSend a signal to a process."),
        ("killpg", FAST, os_killpg, "killpg($module, pgid, signal, /)\n--\n\nSend a signal to a process group."),
        ("abort", NOARGS, os_abort),
        ("_exit", FASTKW, os__exit, "_exit($module, /, status)\n--\n\nExit the process immediately, without cleanup."),
        ("nice", FAST, os_nice, "nice($module, increment, /)\n--\n\nAdd increment to the niceness of the process and return the new value."),
        ("getpriority", FAST, os_getpriority, "getpriority($module, /, which, who)\n--\n\nReturn the scheduling priority of a process, group or user."),
        ("setpriority", FAST, os_setpriority, "setpriority($module, /, which, who, priority)\n--\n\nSet the scheduling priority of a process, group or user."),
        ("times", NOARGS, os_times),
        ("uname", NOARGS, os_uname),
        ("cpu_count", NOARGS, os_cpu_count),
        ("sched_getaffinity", FAST, os_sched_getaffinity, "sched_getaffinity($module, pid, /)\n--\n\nReturn the set of CPUs a process may run on."),
        ("sched_setaffinity", FAST, os_sched_setaffinity, "sched_setaffinity($module, pid, mask, /)\n--\n\nRestrict a process to the given set of CPUs."),
        ("sched_yield", NOARGS, os_sched_yield),
        ("sched_get_priority_max", FASTKW, os_sched_get_priority_max, "sched_get_priority_max($module, /, policy)\n--\n\nReturn the highest priority of a scheduling policy."),
        ("sched_get_priority_min", FASTKW, os_sched_get_priority_min, "sched_get_priority_min($module, /, policy)\n--\n\nReturn the lowest priority of a scheduling policy."),
        ("sched_getscheduler", FAST, os_sched_getscheduler, "sched_getscheduler($module, pid, /)\n--\n\nReturn the scheduling policy of a process."),
        ("sched_getparam", FAST, os_sched_getparam, "sched_getparam($module, pid, /)\n--\n\nReturn the scheduling parameters of a process."),
        ("sched_setparam", FAST, os_sched_setparam, "sched_setparam($module, pid, param, /)\n--\n\nSet the scheduling parameters of a process."),
        ("sched_setscheduler", FAST, os_sched_setscheduler, "sched_setscheduler($module, pid, policy, param, /)\n--\n\nSet the scheduling policy and parameters of a process."),
        ("sched_rr_get_interval", FAST, os_sched_rr_get_interval, "sched_rr_get_interval($module, pid, /)\n--\n\nReturn the round-robin time slice of a process, in seconds."),
        ("getloadavg", NOARGS, os_getloadavg),
        ("strerror", FAST, os_strerror, "strerror($module, code, /)\n--\n\nReturn the message for an errno value."),
        ("ctermid", NOARGS, os_ctermid),
        ("urandom", FAST, os_urandom, "urandom($module, size, /)\n--\n\nReturn size random bytes suitable for cryptographic use."),
        ("getrandom", FASTKW, os_getrandom, "getrandom($module, /, size, flags=0)\n--\n\nReturn up to size random bytes from the kernel."),
        ("register_at_fork", FASTKW, os_register_at_fork, "register_at_fork($module, /, *, before=<unrepresentable>,\n                 after_in_child=<unrepresentable>,\n                 after_in_parent=<unrepresentable>)\n--\n\nRegister callables to run around fork()."),
        ("fork", NOARGS, os_fork),
        ("forkpty", NOARGS, os_forkpty),
        ("execv", FAST, os_execv, "execv($module, path, argv, /)\n--\n\nReplace the current process with a new program."),
        ("execve", FASTKW, os_execve, "execve($module, /, path, argv, env)\n--\n\nReplace the current process with a new program and environment."),
        ("waitpid", FAST, os_waitpid, "waitpid($module, pid, options, /)\n--\n\nWait for a child process; return (pid, status)."),
        ("wait", NOARGS, os_wait),
        ("wait3", FASTKW, os_wait3, "wait3($module, /, options)\n--\n\nWait for any child; return (pid, status, rusage)."),
        ("wait4", FASTKW, os_wait4, "wait4($module, /, pid, options)\n--\n\nWait for a given child; return (pid, status, rusage)."),
        ("waitid", FAST, os_waitid, "waitid($module, idtype, id, options, /)\n--\n\nWait for child state changes; return a waitid_result or None."),
        ("pidfd_open", FASTKW, os_pidfd_open, "pidfd_open($module, /, pid, flags=0)\n--\n\nReturn a file descriptor referring to a process."),
        ("waitstatus_to_exitcode", FASTKW, os_waitstatus_to_exitcode, "waitstatus_to_exitcode($module, /, status)\n--\n\nTurn a wait status into an exit code (negative for a signal)."),
        ("WCOREDUMP", FASTKW, os_wcoredump, "WCOREDUMP($module, status, /)\n--\n\nReturn True if the process produced a core dump."),
        ("WIFCONTINUED", FASTKW, os_wifcontinued, "WIFCONTINUED($module, /, status)\n--\n\nReturn True if the process was resumed by SIGCONT."),
        ("WIFSTOPPED", FASTKW, os_wifstopped, "WIFSTOPPED($module, /, status)\n--\n\nReturn True if the process is stopped."),
        ("WIFSIGNALED", FASTKW, os_wifsignaled, "WIFSIGNALED($module, /, status)\n--\n\nReturn True if the process was ended by a signal."),
        ("WIFEXITED", FASTKW, os_wifexited, "WIFEXITED($module, /, status)\n--\n\nReturn True if the process exited normally."),
        ("WEXITSTATUS", FASTKW, os_wexitstatus, "WEXITSTATUS($module, /, status)\n--\n\nReturn the exit code of a process that exited normally."),
        ("WTERMSIG", FASTKW, os_wtermsig, "WTERMSIG($module, /, status)\n--\n\nReturn the signal that ended the process."),
        ("WSTOPSIG", FASTKW, os_wstopsig, "WSTOPSIG($module, /, status)\n--\n\nReturn the signal that stopped the process."),
        ("system", FASTKW, os_system, "system($module, /, command)\n--\n\nRun a shell command and return its wait status."),
        ("posix_spawn", FASTKW, os_posix_spawn, "posix_spawn($module, path, argv, env, /, *, file_actions=(), setpgroup=None,\n            resetids=False, setsid=False, setsigmask=(), setsigdef=(),\n            scheduler=None)\n--\n\nStart a new process running the executable at path."),
        ("posix_spawnp", FASTKW, os_posix_spawnp, "posix_spawnp($module, path, argv, env, /, *, file_actions=(), setpgroup=None,\n             resetids=False, setsid=False, setsigmask=(), setsigdef=(),\n             scheduler=None)\n--\n\nLike posix_spawn(), but search PATH for the executable."),
        ("get_terminal_size", FAST, os_get_terminal_size, "get_terminal_size($module, fd=<unrepresentable>, /)\n--\n\nReturn the size of the terminal as (columns, lines)."),
        ("putenv", FAST, os_putenv, "putenv($module, name, value, /)\n--\n\nSet an environment variable for this process and its children."),
        ("unsetenv", FAST, os_unsetenv, "unsetenv($module, name, /)\n--\n\nRemove an environment variable."),
        ("sysconf", FAST, os_sysconf, "sysconf($module, name, /)\n--\n\nReturn a system configuration value."),
        ("confstr", FAST, os_confstr, "confstr($module, name, /)\n--\n\nReturn a string-valued system configuration value."),
        ("fpathconf", FASTKW, os_fpathconf, "fpathconf($module, /, fd, name)\n--\n\nReturn a configuration limit for an open file."),
        ("pathconf", FASTKW, os_pathconf, "pathconf($module, /, path, name)\n--\n\nReturn a configuration limit for a path."),
        ("_path_normpath", FASTKW, os_path_normpath, "_path_normpath($module, /, path)\n--\n\nNormalize a path (helper for os.path.normpath)."),
        ("_path_splitroot_ex", FASTKW, os_path_splitroot_ex, "_path_splitroot_ex($module, /, p)\n--\n\nSplit a path into (drive, root, tail)."),
        ("_create_environ", NOARGS, os_create_environ),
    ]
}

pub fn init() -> PyResult {
    stat::init_types()?;
    proc::init_types()?;
    scandir::init_types()?;
    if TERMINAL_SIZE.load(Ordering::Acquire).is_null() {
        const TS: &[Field] = &[Field::new("columns", "width of the terminal window in characters"), Field::new("lines", "height of the terminal window in characters")];
        let t = structseq::new_type(&Desc {
            name: "os.terminal_size",
            doc: "A tuple of (columns, lines) for holding terminal window size",
            fields: TS,
            n_in_sequence: 2,
        })?;
        TERMINAL_SIZE.store(t, Ordering::Release);
    }
    let m = module::new_module("posix")?;
    let d = module::module_dict(&m);
    crate::builtins::add_methods_to_dict(&d, methods(), Some(&m))?;
    for (n, v) in int_constants() {
        dict::set_item_str(&d, n, &int::new_int(v))?;
    }
    let ty = |p: *mut PyTypeObject| crate::types::typeobject::type_ref(p);
    dict::set_item_str(&d, "stat_result", &ty(typeptr(&stat::STAT_RESULT)))?;
    dict::set_item_str(&d, "statvfs_result", &ty(typeptr(&stat::STATVFS_RESULT)))?;
    dict::set_item_str(&d, "times_result", &ty(typeptr(&proc::TIMES_RESULT)))?;
    dict::set_item_str(&d, "uname_result", &ty(typeptr(&proc::UNAME_RESULT)))?;
    dict::set_item_str(&d, "waitid_result", &ty(typeptr(&proc::WAITID_RESULT)))?;
    dict::set_item_str(&d, "sched_param", &ty(typeptr(&proc::SCHED_PARAM)))?;
    dict::set_item_str(&d, "terminal_size", &ty(typeptr(&TERMINAL_SIZE)))?;
    dict::set_item_str(&d, "DirEntry", &ty(&raw mut scandir::DIRENTRY_TYPE))?;
    dict::set_item_str(&d, "error", &ty(crate::types::exceptions::OSError()))?;
    dict::set_item_str(&d, "environ", &*build_environ()?)?;
    dict::set_item_str(&d, "_have_functions", &have_functions())?;
    dict::set_item_str(&d, "sysconf_names", &*names_dict(SYSCONF_NAMES)?)?;
    dict::set_item_str(&d, "confstr_names", &*names_dict(CONFSTR_NAMES)?)?;
    dict::set_item_str(&d, "pathconf_names", &*names_dict(PATHCONF_NAMES)?)?;
    let _ = (ab::is_callable, tname, tuple::empty_tuple);
    Ok(m)
}
