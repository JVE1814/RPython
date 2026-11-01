//! `_multiprocessing`: SemLock (a lock/semaphore shared between processes,
//! backed by a POSIX named semaphore), sem_unlink() and the `flags` dict.
//!
//! A SemLock remembers which thread acquired it last and how many times
//! (the recursion count), which gives recursive-mutex semantics for the
//! RLock flavour and the `_is_mine()` / `_count()` introspection used by
//! multiprocessing.synchronize.

use crate::abstract_ as ab;
use crate::args::Params;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::fncommon as fc;
use crate::object::*;
use crate::raise;
use crate::types::{bool_, dict, int, none, str as pystr};
use std::ffi::{CString, c_int};
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering::*};

const RECURSIVE_MUTEX: i32 = 0;
const SEMAPHORE: i32 = 1;
/// Largest semaphore value on Linux (glibc: INT_MAX).
const SEM_VALUE_MAX: i32 = i32::MAX;

#[repr(C)]
struct SemLock {
    ob_base: PyObject,
    handle: *mut libc::sem_t,
    /// pthread id of the thread that last acquired the semaphore.
    last_tid: AtomicU64,
    /// Number of acquisitions held by `last_tid` (not shared between processes).
    count: AtomicI32,
    maxvalue: i32,
    kind: i32,
    /// Owned str or None.
    name: *mut PyObject,
}

static mut SEMLOCK_TYPE: PyTypeObject = PyTypeObject::ZERO;

fn sl(o: &PyObject) -> &SemLock {
    unsafe { &*(o.as_ptr() as *const SemLock) }
}

fn me() -> u64 {
    unsafe { libc::pthread_self() as u64 }
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn os_err(e: i32) -> PyErr {
    crate::types::exceptions::os_error_from_errno(e, None, None)
}

fn is_mine(s: &SemLock) -> bool {
    s.count.load(Acquire) > 0 && s.last_tid.load(Acquire) == me()
}

unsafe fn alloc(tp: *mut PyTypeObject, handle: *mut libc::sem_t, kind: i32, maxvalue: i32, name: Option<PyRef>) -> PyResult {
    unsafe {
        let op = ((*tp).tp_alloc.unwrap())(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        let s = op as *mut SemLock;
        (*s).handle = handle;
        std::ptr::write(&raw mut (*s).last_tid, AtomicU64::new(0));
        std::ptr::write(&raw mut (*s).count, AtomicI32::new(0));
        (*s).kind = kind;
        (*s).maxvalue = maxvalue;
        (*s).name = match name {
            Some(n) => n.into_raw(),
            None => std::ptr::null_mut(),
        };
        Ok(PyRef::from_raw(op))
    }
}

fn name_cstring(name: &PyObject) -> PyResult<CString> {
    let s = pystr::as_utf8(name)?;
    match CString::new(s) {
        Ok(c) => Ok(c),
        Err(_) => raise!(ValueError, "embedded null character"),
    }
}

unsafe extern "C" fn semlock_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    let a = unsafe { fc::VcArgs::from_c(args, kw) };
    into_c(semlock_new_impl(tp, &a))
}

fn semlock_new_impl(tp: *mut PyTypeObject, a: &fc::VcArgs) -> PyResult {
    static P: Params = Params::new("SemLock", &["kind", "value", "maxvalue", "name", "unlink"], 0, 5);
    let [kind, value, maxvalue, name, unlink] = a.parse::<5>(&P)?;
    let kind = crate::modules::posix::path::c_int(kind.as_deref().unwrap())?;
    let value = crate::modules::posix::path::c_int(value.as_deref().unwrap())?;
    let maxvalue = crate::modules::posix::path::c_int(maxvalue.as_deref().unwrap())?;
    let name = name.unwrap();
    let unlink = ab::is_true(&unlink.unwrap())?;
    if kind != RECURSIVE_MUTEX && kind != SEMAPHORE {
        raise!(ValueError, "unrecognized kind");
    }
    if !pystr::is_str(&name) {
        raise!(TypeError, "SemLock() argument 'name' must be str, not {}", crate::types::typeobject::tname(&name));
    }
    if value < 0 || value > SEM_VALUE_MAX {
        raise!(ValueError, "invalid value");
    }
    let cname = name_cstring(&name)?;
    let handle = unsafe { libc::sem_open(cname.as_ptr(), libc::O_CREAT | libc::O_EXCL, 0o600 as libc::c_uint, value as libc::c_uint) };
    if handle == libc::SEM_FAILED {
        return Err(os_err(errno()));
    }
    if unlink && unsafe { libc::sem_unlink(cname.as_ptr()) } < 0 {
        let e = errno();
        unsafe { libc::sem_close(handle) };
        return Err(os_err(e));
    }
    unsafe { alloc(tp, handle, kind, maxvalue, if unlink { None } else { Some(name.to_ref()) }) }
}

unsafe extern "C" fn semlock_dealloc(op: *mut PyObject) {
    unsafe {
        let s = op as *mut SemLock;
        if !(*s).handle.is_null() && (*s).handle != libc::SEM_FAILED {
            libc::sem_close((*s).handle);
        }
        crate::object::xdecref((*s).name);
        crate::types::typeobject::generic_free_and_type_decref(op, (*op).type_ptr());
    }
}

fn realtime_now() -> libc::timespec {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut ts) };
    ts
}

fn add_secs(ts: libc::timespec, secs: f64) -> libc::timespec {
    let whole = secs.floor();
    let mut sec = ts.tv_sec + whole as libc::time_t;
    let mut nsec = ts.tv_nsec + ((secs - whole) * 1e9) as libc::c_long;
    if nsec >= 1_000_000_000 {
        sec += 1;
        nsec -= 1_000_000_000;
    }
    libc::timespec { tv_sec: sec, tv_nsec: nsec }
}

/// Wait for the semaphore.  Returns Ok(false) on timeout.
fn wait_sem(handle: *mut libc::sem_t, timeout: Option<f64>) -> PyResult<bool> {
    // fast path
    loop {
        if unsafe { libc::sem_trywait(handle) } == 0 {
            return Ok(true);
        }
        match errno() {
            libc::EINTR => continue,
            libc::EAGAIN => break,
            e => return Err(os_err(e)),
        }
    }
    if timeout == Some(0.0) {
        return Ok(false);
    }
    let main = crate::threads::is_main_thread();
    let start = std::time::Instant::now();
    loop {
        // The main thread waits in slices so that signal handlers run.
        let remaining = match timeout {
            Some(t) => {
                let left = t - start.elapsed().as_secs_f64();
                if left <= 0.0 {
                    return Ok(false);
                }
                Some(left)
            }
            None => None,
        };
        let slice = match (remaining, main) {
            (Some(r), true) => Some(r.min(0.02)),
            (None, true) => Some(0.02),
            (r, false) => r,
        };
        let (rc, e) = crate::threads::blocking(|| {
            let rc = match slice {
                Some(s) => {
                    let dl = add_secs(realtime_now(), s);
                    unsafe { libc::sem_timedwait(handle, &dl) }
                }
                None => unsafe { libc::sem_wait(handle) },
            };
            (rc, if rc < 0 { errno() } else { 0 })
        });
        if rc == 0 {
            return Ok(true);
        }
        match e {
            libc::EINTR | libc::ETIMEDOUT => {
                crate::modules::signal::check_signals()?;
            }
            e => return Err(os_err(e)),
        }
    }
}

fn semlock_acquire(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("acquire", &["block", "timeout"], 0, 0);
    let [block, timeout] = crate::args::parse::<2>(&P, args, kw)?;
    let s = sl(o);
    let block = match block {
        Some(b) => ab::is_true(b)?,
        None => true,
    };
    if s.kind == RECURSIVE_MUTEX && is_mine(s) {
        s.count.fetch_add(1, AcqRel);
        return Ok(bool_::new_bool(true));
    }
    let timeout = if !block {
        Some(0.0)
    } else {
        match timeout {
            Some(t) if !t.is(none::none()) => {
                let v = ab::float_value(t)?;
                Some(if v < 0.0 { 0.0 } else { v })
            }
            _ => None,
        }
    };
    if !wait_sem(s.handle, timeout)? {
        return Ok(bool_::new_bool(false));
    }
    // owner first: another thread must never see count > 0 with a stale owner
    s.last_tid.store(me(), Release);
    s.count.fetch_add(1, AcqRel);
    Ok(bool_::new_bool(true))
}

fn semlock_release(o: &PyObject) -> PyResult {
    let s = sl(o);
    if s.kind == RECURSIVE_MUTEX {
        if !is_mine(s) {
            raise!(AssertionError, "attempt to release recursive lock not owned by thread");
        }
        if s.count.load(Acquire) > 1 {
            s.count.fetch_sub(1, AcqRel);
            return Ok(none::none_ref());
        }
    } else {
        let mut v: c_int = 0;
        if unsafe { libc::sem_getvalue(s.handle, &mut v) } < 0 {
            return Err(os_err(errno()));
        }
        if v >= s.maxvalue {
            raise!(ValueError, "semaphore or lock released too many times");
        }
    }
    // Drop our acquisition before posting: once posted, another thread of
    // this process may acquire (and count) immediately.
    let dropped = s.count.fetch_update(AcqRel, Acquire, |c| if c > 0 { Some(c - 1) } else { None }).is_ok();
    if unsafe { libc::sem_post(s.handle) } < 0 {
        let e = errno();
        if dropped {
            s.count.fetch_add(1, AcqRel);
        }
        return Err(os_err(e));
    }
    Ok(none::none_ref())
}

fn semlock_enter(o: &PyObject) -> PyResult {
    semlock_acquire(o, &[], None)
}
fn semlock_exit(o: &PyObject, _args: &[&PyObject]) -> PyResult {
    semlock_release(o)
}
fn semlock_count(o: &PyObject) -> PyResult {
    Ok(int::new_int(sl(o).count.load(Acquire) as i64))
}
fn semlock_is_mine(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(is_mine(sl(o))))
}
fn get_value(s: &SemLock) -> PyResult<i32> {
    let mut v: c_int = 0;
    if unsafe { libc::sem_getvalue(s.handle, &mut v) } < 0 {
        return Err(os_err(errno()));
    }
    // some platforms report waiters as a negative value
    Ok(v.max(0))
}
fn semlock_get_value(o: &PyObject) -> PyResult {
    Ok(int::new_int(get_value(sl(o))? as i64))
}
fn semlock_is_zero(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(get_value(sl(o))? == 0))
}
fn semlock_after_fork(o: &PyObject) -> PyResult {
    sl(o).count.store(0, Release);
    Ok(none::none_ref())
}

/// SemLock._rebuild(handle, kind, maxvalue, name): recreate a SemLock in
/// another process (by name when it has one, else from the inherited handle).
fn semlock_rebuild(cls: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("_rebuild", args, 4, 4)?;
    let mut handle = ab::as_i64(args[0])? as usize as *mut libc::sem_t;
    let kind = crate::modules::posix::path::c_int(args[1])?;
    let maxvalue = crate::modules::posix::path::c_int(args[2])?;
    let name = args[3];
    let keep_name = if name.is(none::none()) {
        None
    } else {
        let cname = name_cstring(name)?;
        handle = unsafe { libc::sem_open(cname.as_ptr(), 0) };
        if handle == libc::SEM_FAILED {
            return Err(os_err(errno()));
        }
        Some(name.to_ref())
    };
    unsafe { alloc(cls.as_ptr() as *mut PyTypeObject, handle, kind, maxvalue, keep_name) }
}

fn get_handle(o: &PyObject) -> PyResult {
    Ok(int::new_int_usize(sl(o).handle as usize))
}
fn get_kind(o: &PyObject) -> PyResult {
    Ok(int::new_int(sl(o).kind as i64))
}
fn get_maxvalue(o: &PyObject) -> PyResult {
    Ok(int::new_int(sl(o).maxvalue as i64))
}
fn get_name(o: &PyObject) -> PyResult {
    let p = sl(o).name;
    Ok(if p.is_null() { none::none_ref() } else { unsafe { &*p }.to_ref() })
}

fn sem_unlink(_m: &PyObject, name: &PyObject) -> PyResult {
    let cname = name_cstring(name)?;
    if unsafe { libc::sem_unlink(cname.as_ptr()) } < 0 {
        let e = errno();
        return Err(crate::types::exceptions::os_error_from_errno(e, Some(name), None));
    }
    Ok(none::none_ref())
}

pub fn init() -> PyResult {
    unsafe {
        if SEMLOCK_TYPE.tp_flags & Py_TPFLAGS_READY == 0 {
            let t = &mut *(&raw mut SEMLOCK_TYPE);
            t.tp_name = c"_multiprocessing.SemLock".as_ptr();
            t.tp_basicsize = std::mem::size_of::<SemLock>() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE;
            t.tp_new = Some(semlock_new);
            t.tp_alloc = Some(crate::types::typeobject::generic_alloc);
            t.tp_dealloc = Some(semlock_dealloc);
            t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
            t.tp_doc = c"Semaphore/Mutex type".as_ptr();
            t.tp_methods = crate::mdefs![
                ("acquire", FASTKW, semlock_acquire, "Acquire the semaphore/lock."),
                ("release", NOARGS, semlock_release, "Release the semaphore/lock."),
                ("__enter__", NOARGS, semlock_enter, "Enter the semaphore/lock."),
                ("__exit__", FAST, semlock_exit, "Exit the semaphore/lock."),
                ("_count", NOARGS, semlock_count, "Num of `acquire()`s minus num of `release()`s for this process."),
                ("_is_mine", NOARGS, semlock_is_mine, "Whether the lock is owned by this thread."),
                ("_get_value", NOARGS, semlock_get_value, "Get the value of the semaphore."),
                ("_is_zero", NOARGS, semlock_is_zero, "Return whether semaphore has value zero."),
                ("_rebuild", FAST, semlock_rebuild; METH_CLASS),
                ("_after_fork", NOARGS, semlock_after_fork, "Rezero the net acquisition count after fork()."),
            ];
            t.tp_getset = crate::getset_table![
                ("handle", get_handle),
                ("kind", get_kind),
                ("maxvalue", get_maxvalue),
                ("name", get_name),
            ];
            crate::types::typeobject::type_ready(&raw mut SEMLOCK_TYPE)?;
            let td = crate::types::typeobject::type_dict(&raw mut SEMLOCK_TYPE);
            dict::set_item_str(&td, "SEM_VALUE_MAX", &int::new_int(SEM_VALUE_MAX as i64))?;
        }
    }
    let m = crate::types::module::new_module("_multiprocessing")?;
    let d = crate::types::module::module_dict(&m);
    dict::set_item_str(&d, "SemLock", crate::types::typeobject::type_obj(&raw mut SEMLOCK_TYPE))?;
    crate::builtins::add_methods_to_dict(&d, crate::mdefs![("sem_unlink", O, sem_unlink)], Some(&m))?;
    let flags = dict::new_dict();
    dict::set_item_str(&flags, "HAVE_SEM_OPEN", &int::new_int(1))?;
    dict::set_item_str(&flags, "HAVE_SEM_TIMEDWAIT", &int::new_int(1))?;
    dict::set_item_str(&d, "flags", &flags)?;
    Ok(m)
}
