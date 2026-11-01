//! The `syslog` and `_posixshmem` modules.

use crate::abstract_ as ab;
use crate::args::Params;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::posix::path::{c_int, fs_encode_str};
use crate::object::*;
use crate::types::typeobject::tname;
use crate::types::{dict, int, none, str as pystr};
use crate::{mdefs, raise};
use std::ffi::CString;
use std::sync::Mutex;

/// The ident string passed to openlog() must stay alive while the log is open.
static STATE: Mutex<Option<CString>> = Mutex::new(None);

fn utf8_arg(o: &PyObject) -> PyResult<CString> {
    let s = pystr::as_utf8(o)?;
    match CString::new(s) {
        Ok(c) => Ok(c),
        Err(_) => raise!(ValueError, "embedded null character"),
    }
}

/// Default ident: basename of sys.argv[0].
fn default_ident() -> Option<CString> {
    let r = (|| -> PyResult<Option<CString>> {
        let sys = crate::import::import_absolute("sys")?;
        let argv = match ab::getattr_str(&sys, "argv") {
            Ok(a) => a,
            Err(_) => {
                clear_exc();
                return Ok(None);
            }
        };
        if ab::length(&argv)? == 0 {
            return Ok(None);
        }
        let a0 = ab::getitem(&argv, &int::new_int(0))?;
        if !pystr::is_str(&a0) {
            return Ok(None);
        }
        let s = pystr::to_string_lossy(&a0);
        let base = s.rsplit('/').next().unwrap_or("").to_string();
        Ok(CString::new(base).ok())
    })();
    r.unwrap_or_else(|_| {
        clear_exc();
        None
    })
}

fn do_openlog(ident: Option<CString>, opt: i32, facility: i32) {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let p = ident.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null());
    unsafe { libc::openlog(p, opt, facility) };
    *g = ident;
}

fn s_openlog(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("openlog", &["ident", "logoption", "facility"], 0, 0);
    let [ident, opt, fac] = crate::args::parse::<3>(&P, args, kw)?;
    let ident = match ident {
        Some(i) if !i.is(none::none()) => {
            if !pystr::is_str(i) {
                raise!(TypeError, "openlog() argument 'ident' must be str or None, not {}", tname(i));
            }
            Some(utf8_arg(i)?)
        }
        _ => default_ident(),
    };
    let opt = match opt {
        Some(o) => c_int(o)?,
        None => 0,
    };
    let fac = match fac {
        Some(f) => c_int(f)?,
        None => libc::LOG_USER,
    };
    let ident_obj = match &ident {
        Some(c) => pystr::new_str(&c.to_string_lossy()),
        None => none::none_ref(),
    };
    crate::modules::sys_full::audit("syslog.openlog", &[&ident_obj, &int::new_int(opt as i64), &int::new_int(fac as i64)])?;
    do_openlog(ident, opt, fac);
    Ok(none::none_ref())
}

fn s_syslog(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("syslog", args, 1, 2)?;
    let (prio, msg) = if args.len() == 2 { (c_int(args[0])?, args[1]) } else { (libc::LOG_INFO, args[0]) };
    if !pystr::is_str(msg) {
        raise!(TypeError, "syslog() argument {} must be str, not {}", args.len(), tname(msg));
    }
    let cmsg = utf8_arg(msg)?;
    crate::modules::sys_full::audit("syslog.syslog", &[&int::new_int(prio as i64), msg])?;
    {
        let opened = STATE.lock().unwrap_or_else(|e| e.into_inner()).is_some();
        if !opened {
            do_openlog(default_ident(), 0, libc::LOG_USER);
        }
    }
    let _g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    crate::threads::blocking(|| unsafe { libc::syslog(prio, c"%s".as_ptr(), cmsg.as_ptr()) });
    Ok(none::none_ref())
}

fn s_closelog(_m: &PyObject) -> PyResult {
    crate::modules::sys_full::audit("syslog.closelog", &[])?;
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { libc::closelog() };
    *g = None;
    Ok(none::none_ref())
}

fn s_setlogmask(_m: &PyObject, mask: &PyObject) -> PyResult {
    let m = c_int(mask)?;
    crate::modules::sys_full::audit("syslog.setlogmask", &[mask])?;
    Ok(int::new_int(unsafe { libc::setlogmask(m) } as i64))
}

fn s_log_mask(_m: &PyObject, pri: &PyObject) -> PyResult {
    let p = c_int(pri)?;
    Ok(int::new_int(1i64 << p))
}

fn s_log_upto(_m: &PyObject, pri: &PyObject) -> PyResult {
    let p = c_int(pri)?;
    Ok(int::new_int((1i64 << (p + 1)) - 1))
}

pub fn init_syslog() -> PyResult {
    let methods = mdefs![
        ("openlog", FASTKW, s_openlog, "openlog($module, /, ident=<unrepresentable>, logoption=0,\n        facility=LOG_USER)\n--\n\nSet logging options of subsequent syslog() calls."),
        ("closelog", NOARGS, s_closelog, "closelog($module, /)\n--\n\nReset the syslog module values and call the system library closelog()."),
        ("syslog", FAST, s_syslog, "syslog([priority=LOG_INFO,] message)\nSend the string message to the system logger."),
        ("setlogmask", O, s_setlogmask, "setlogmask($module, maskpri, /)\n--\n\nSet the priority mask to maskpri and return the previous mask value."),
        ("LOG_MASK", O, s_log_mask, "LOG_MASK($module, pri, /)\n--\n\nCalculates the mask for the individual priority pri."),
        ("LOG_UPTO", O, s_log_upto, "LOG_UPTO($module, pri, /)\n--\n\nCalculates the mask for all priorities up to and including pri."),
    ];
    let (m, d) = crate::modules::fncommon::new_module("syslog", methods, None)?;
    let consts: &[(&str, i64)] = &[
        ("LOG_EMERG", 0), ("LOG_ALERT", 1), ("LOG_CRIT", 2), ("LOG_ERR", 3), ("LOG_WARNING", 4),
        ("LOG_NOTICE", 5), ("LOG_INFO", 6), ("LOG_DEBUG", 7),
        ("LOG_PID", 1), ("LOG_CONS", 2), ("LOG_ODELAY", 4), ("LOG_NDELAY", 8), ("LOG_NOWAIT", 16),
        ("LOG_PERROR", 32),
        ("LOG_KERN", 0), ("LOG_USER", 8), ("LOG_MAIL", 16), ("LOG_DAEMON", 24), ("LOG_AUTH", 32),
        ("LOG_SYSLOG", 40), ("LOG_LPR", 48), ("LOG_NEWS", 56), ("LOG_UUCP", 64), ("LOG_CRON", 72),
        ("LOG_AUTHPRIV", 80), ("LOG_FTP", 88),
        ("LOG_LOCAL0", 128), ("LOG_LOCAL1", 136), ("LOG_LOCAL2", 144), ("LOG_LOCAL3", 152),
        ("LOG_LOCAL4", 160), ("LOG_LOCAL5", 168), ("LOG_LOCAL6", 176), ("LOG_LOCAL7", 184),
    ];
    for (n, v) in consts {
        dict::set_item_str(&d, n, &int::new_int(*v))?;
    }
    Ok(m)
}

// ------------------------------------------------------------------ _posixshmem

fn path_arg(o: &PyObject) -> PyResult<CString> {
    if !pystr::is_str(o) {
        raise!(TypeError, "argument 'path' must be str, not {}", tname(o));
    }
    let b = fs_encode_str(o)?;
    match CString::new(b) {
        Ok(c) => Ok(c),
        Err(_) => raise!(ValueError, "embedded null character"),
    }
}

fn shm_open(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("shm_open", &["path", "flags", "mode"], 0, 2);
    let [path, flags, mode] = crate::args::parse::<3>(&P, args, kw)?;
    let path = path.unwrap();
    let c = path_arg(path)?;
    let flags = c_int(flags.unwrap())?;
    let mode = match mode {
        Some(m) => c_int(m)?,
        None => 0o777,
    };
    loop {
        let fd = crate::threads::blocking(|| unsafe { libc::shm_open(c.as_ptr(), flags, mode as libc::mode_t) });
        if fd < 0 {
            let e = crate::modules::posix::path::errno();
            if e == libc::EINTR {
                crate::modules::posix::path::check_signals()?;
                continue;
            }
            return Err(crate::types::exceptions::os_error_from_errno(e, Some(path), None));
        }
        return Ok(int::new_int(fd as i64));
    }
}

fn shm_unlink(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("shm_unlink", &["path"], 0, 1);
    let [path] = crate::args::parse::<1>(&P, args, kw)?;
    let path = path.unwrap();
    let c = path_arg(path)?;
    let r = crate::threads::blocking(|| unsafe { libc::shm_unlink(c.as_ptr()) });
    if r < 0 {
        let e = crate::modules::posix::path::errno();
        return Err(crate::types::exceptions::os_error_from_errno(e, Some(path), None));
    }
    Ok(none::none_ref())
}

pub fn init_posixshmem() -> PyResult {
    let methods = mdefs![
        ("shm_open", FASTKW, shm_open, "shm_open($module, /, path, flags, mode=511)\n--\n\nOpen a shared memory object.  Returns a file descriptor (integer)."),
        ("shm_unlink", FASTKW, shm_unlink, "shm_unlink($module, /, path)\n--\n\nRemove a shared memory object (similar to unlink()).\n\nRemove a shared memory object name, and, once all processes  have  unmapped\nthe object, de-allocates and destroys the contents of the associated memory\nregion."),
    ];
    let (m, _d) = crate::modules::fncommon::new_module("_posixshmem", methods, None)?;
    Ok(m)
}
