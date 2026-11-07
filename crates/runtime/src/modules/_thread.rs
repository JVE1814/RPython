//! The `_thread` module: real OS threads (no GIL), futex-based locks with timeouts,
//! RLock, thread handles, thread-local objects (`_local`), and the default thread
//! exception hook.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::{tname, type_obj, type_ref};
use crate::types::{bool_, dict, float, int, none, str as pystr, tuple};
use std::collections::HashMap;
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering::*};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Largest timeout accepted by lock.acquire(), in seconds.
pub const TIMEOUT_MAX: f64 = 9223372036.854776;

// ===================================================================== futex lock

fn futex_wait(addr: &AtomicU32, val: u32, timeout: Option<Duration>) {
    unsafe {
        let ts;
        let tp = match timeout {
            Some(d) => {
                ts = libc::timespec { tv_sec: d.as_secs().min(i64::MAX as u64) as libc::time_t, tv_nsec: d.subsec_nanos() as libc::c_long };
                &ts as *const libc::timespec
            }
            None => std::ptr::null(),
        };
        libc::syscall(libc::SYS_futex, addr.as_ptr(), libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG, val, tp);
    }
}

pub fn futex_wake(addr: *const AtomicU32, n: i32) {
    unsafe {
        libc::syscall(libc::SYS_futex, addr as *const u32, libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG, n);
    }
}

/// Address of the futex word the main thread is currently blocked on (so that a
/// signal handler can wake it to run Python signal handlers).
pub static MAIN_WAIT_ADDR: AtomicPtr<AtomicU32> = AtomicPtr::new(std::ptr::null_mut());

/// A non-owned binary lock (0 = unlocked, 1 = locked, 2 = locked with waiters).
pub struct RawLock {
    pub state: AtomicU32,
}

pub enum AcquireResult {
    Acquired,
    TimedOut,
}

impl RawLock {
    pub const fn new() -> Self {
        RawLock { state: AtomicU32::new(0) }
    }
    #[inline]
    pub fn try_acquire(&self) -> bool {
        self.state.compare_exchange(0, 1, Acquire, Relaxed).is_ok()
    }
    pub fn is_locked(&self) -> bool {
        self.state.load(Relaxed) != 0
    }
    pub fn release(&self) {
        if self.state.swap(0, Release) == 2 {
            futex_wake(&self.state, 1);
        }
    }
    /// Blocking acquire. `timeout` None = forever. When called on the main thread,
    /// pending Python signal handlers are run while waiting (errors propagate).
    pub fn acquire_timed(&self, timeout: Option<Duration>) -> PyResult<AcquireResult> {
        if self.try_acquire() {
            return Ok(AcquireResult::Acquired);
        }
        if timeout == Some(Duration::ZERO) {
            return Ok(AcquireResult::TimedOut);
        }
        if crate::gc::others_parked() {
            // every other thread is stopped for good: nobody can release it
            raise!(PythonFinalizationError, "cannot acquire lock at interpreter finalization");
        }
        let deadline = timeout.map(|t| Instant::now().checked_add(t).unwrap_or_else(|| Instant::now() + Duration::from_secs(1 << 40)));
        let main = crate::threads::is_main_thread();
        let ts = crate::threads::current();
        loop {
            if self.state.swap(2, Acquire) == 0 {
                return Ok(AcquireResult::Acquired);
            }
            let mut wait = match deadline {
                Some(d) => {
                    let now = Instant::now();
                    if now >= d {
                        return Ok(AcquireResult::TimedOut);
                    }
                    Some(d - now)
                }
                None => None,
            };
            if main {
                if crate::modules::signal::signals_pending() {
                    crate::modules::signal::check_signals()?;
                    continue;
                }
                // bounded wait so a lost wakeup cannot delay signal handling for long
                let cap = Duration::from_millis(50);
                wait = Some(wait.map_or(cap, |w| w.min(cap)));
                MAIN_WAIT_ADDR.store(&self.state as *const AtomicU32 as *mut AtomicU32, Release);
            }
            crate::threads::blocking(|| futex_wait(&self.state, 2, wait));
            if main {
                MAIN_WAIT_ADDR.store(std::ptr::null_mut(), Release);
            }
        }
    }
}

/// Wait on a condition with periodic signal checks on the main thread.
fn wait_interruptible<T>(m: &Mutex<T>, cv: &Condvar, mut done: impl FnMut(&T) -> bool, timeout: Option<Duration>) -> PyResult<bool> {
    let deadline = timeout.map(|t| Instant::now().checked_add(t).unwrap_or_else(|| Instant::now() + Duration::from_secs(1 << 40)));
    let main = crate::threads::is_main_thread();
    let ts = crate::threads::current();
    let mut g = m.lock().unwrap();
    loop {
        if done(&g) {
            return Ok(true);
        }
        let mut wait = match deadline {
            Some(d) => {
                let now = Instant::now();
                if now >= d {
                    return Ok(false);
                }
                d - now
            }
            None => Duration::from_secs(3600),
        };
        if main {
            if crate::modules::signal::signals_pending() {
                drop(g);
                crate::modules::signal::check_signals()?;
                g = m.lock().unwrap();
                continue;
            }
            wait = wait.min(Duration::from_millis(20));
        }
        g = crate::threads::blocking(|| cv.wait_timeout(g, wait).unwrap().0);
    }
}

/// Parse (blocking, timeout) arguments of acquire(); returns None for "wait forever".
fn parse_acquire_args(fname: &str, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult<Option<Duration>> {
    let p = crate::args::Params::new(fname, &["blocking", "timeout"], 0, 0);
    let [b, t] = crate::args::parse::<2>(&p, args, kw)?;
    let blocking = match b {
        Some(b) => ab::is_true(b)?,
        None => true,
    };
    let timeout = match t {
        Some(t) => {
            let v = if int::is_int(t) {
                match int::as_i64(t) {
                    Some(i) => i as f64,
                    None => raise!(OverflowError, "timeout value is too large"),
                }
            } else {
                ab::float_value(t)?
            };
            v
        }
        None => -1.0,
    };
    if !blocking && timeout != -1.0 {
        raise!(ValueError, "can't specify a timeout for a non-blocking call");
    }
    if timeout < 0.0 && timeout != -1.0 {
        raise!(ValueError, "timeout value must be a non-negative number");
    }
    if !blocking {
        return Ok(Some(Duration::ZERO));
    }
    if timeout == -1.0 {
        return Ok(None);
    }
    if timeout.is_nan() {
        raise!(ValueError, "Invalid value NaN (not a number)");
    }
    if timeout > TIMEOUT_MAX {
        raise!(OverflowError, "timeout value is too large");
    }
    Ok(Some(Duration::from_secs_f64(timeout)))
}

// ===================================================================== lock objects

#[repr(C)]
pub struct LockObject {
    ob_base: PyObject,
    weakreflist: *mut PyObject,
    lock: RawLock,
}

static mut LOCK_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub fn lock_type() -> *mut PyTypeObject {
    &raw mut LOCK_TYPE
}

fn lk(o: &PyObject) -> &RawLock {
    unsafe { &(*(o.as_ptr() as *const LockObject)).lock }
}

pub fn new_lock() -> PyRef {
    unsafe {
        let op = crate::alloc::alloc_object(lock_type(), std::mem::size_of::<LockObject>());
        std::ptr::write(&raw mut (*(op as *mut LockObject)).lock, RawLock::new());
        PyRef::from_raw(op)
    }
}

unsafe extern "C" fn lock_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        if tuple::tuple_len(&*args) != 0 || (!kw.is_null() && dict::dict_len(&*kw) != 0) {
            return into_c(Err(err!(TypeError, "lock() takes no arguments")));
        }
        let op = crate::alloc::alloc_object(tp, std::mem::size_of::<LockObject>());
        std::ptr::write(&raw mut (*(op as *mut LockObject)).lock, RawLock::new());
        op
    }
}

unsafe extern "C" fn lock_dealloc(op: *mut PyObject) {
    unsafe {
        crate::types::weakref::clear_weakrefs(op);
        crate::alloc::free_object(op);
    }
}

unsafe extern "C" fn lock_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        let s = format!("<{} {} object at {:#x}>", if lk(o).is_locked() { "locked" } else { "unlocked" }, crate::types::typeobject::type_fqname(o.type_ptr()), op as usize);
        pystr::new_str(&s).into_raw()
    }
}

fn lock_acquire(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let t = parse_acquire_args("acquire", args, kw)?;
    match lk(o).acquire_timed(t)? {
        AcquireResult::Acquired => Ok(bool_::new_bool(true)),
        AcquireResult::TimedOut => Ok(bool_::new_bool(false)),
    }
}
fn lock_enter(o: &PyObject) -> PyResult {
    lk(o).acquire_timed(None)?;
    Ok(bool_::new_bool(true))
}
fn lock_release(o: &PyObject) -> PyResult {
    let l = lk(o);
    if !l.is_locked() {
        raise!(RuntimeError, "release unlocked lock");
    }
    l.release();
    Ok(none::none_ref())
}
fn lock_exit(o: &PyObject, _args: &[&PyObject]) -> PyResult {
    lock_release(o)?;
    Ok(none::none_ref())
}
fn lock_locked(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(lk(o).is_locked()))
}
fn lock_at_fork_reinit(o: &PyObject) -> PyResult {
    lk(o).state.store(0, Release);
    Ok(none::none_ref())
}

// ===================================================================== RLock

#[repr(C)]
pub struct RLockObject {
    ob_base: PyObject,
    weakreflist: *mut PyObject,
    lock: RawLock,
    owner: AtomicU64,
    count: AtomicU64,
}

static mut RLOCK_TYPE: PyTypeObject = PyTypeObject::ZERO;

fn rl(o: &PyObject) -> &RLockObject {
    unsafe { &*(o.as_ptr() as *const RLockObject) }
}

fn my_ident() -> u64 {
    unsafe { libc::pthread_self() as u64 }
}

unsafe extern "C" fn rlock_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        // Arguments are only accepted when a subclass defines its own __init__.
        let has_args = (!args.is_null() && tuple::tuple_len(&*args) != 0) || (!kw.is_null() && dict::dict_len(&*kw) != 0);
        if has_args && (tp == &raw mut RLOCK_TYPE || (*tp).tp_init == RLOCK_TYPE.tp_init) {
            return into_c(Err(err!(TypeError, "RLock() takes no arguments")));
        }
        let op = crate::alloc::alloc_object(tp, std::mem::size_of::<RLockObject>().max((*tp).tp_basicsize as usize));
        let r = op as *mut RLockObject;
        std::ptr::write(&raw mut (*r).lock, RawLock::new());
        (*r).owner = AtomicU64::new(0);
        (*r).count = AtomicU64::new(0);
        op
    }
}
unsafe extern "C" fn rlock_dealloc(op: *mut PyObject) {
    unsafe {
        crate::types::weakref::clear_weakrefs(op);
        let tp = (*op).type_ptr();
        crate::types::typeobject::generic_free_and_type_decref(op, tp);
    }
}
unsafe extern "C" fn rlock_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        let r = rl(o);
        let owner = r.owner.load(Relaxed);
        let count = r.count.load(Relaxed);
        let s = format!(
            "<{} {} object owner={} count={} at {:#x}>",
            if r.lock.is_locked() { "locked" } else { "unlocked" },
            crate::types::typeobject::type_fqname(o.type_ptr()),
            owner,
            count,
            op as usize
        );
        pystr::new_str(&s).into_raw()
    }
}
fn rlock_acquire(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let t = parse_acquire_args("acquire", args, kw)?;
    rlock_acquire_impl(o, t)
}
fn rlock_acquire_impl(o: &PyObject, t: Option<Duration>) -> PyResult {
    let r = rl(o);
    let me = my_ident();
    if r.count.load(Relaxed) > 0 && r.owner.load(Relaxed) == me {
        let c = r.count.load(Relaxed);
        if c == u64::MAX {
            raise!(OverflowError, "Internal lock count overflowed");
        }
        r.count.store(c + 1, Relaxed);
        return Ok(bool_::new_bool(true));
    }
    match r.lock.acquire_timed(t)? {
        AcquireResult::Acquired => {
            r.owner.store(me, Relaxed);
            r.count.store(1, Relaxed);
            Ok(bool_::new_bool(true))
        }
        AcquireResult::TimedOut => Ok(bool_::new_bool(false)),
    }
}
fn rlock_enter(o: &PyObject) -> PyResult {
    rlock_acquire_impl(o, None)
}
fn rlock_release(o: &PyObject) -> PyResult {
    let r = rl(o);
    let me = my_ident();
    let c = r.count.load(Relaxed);
    if c == 0 || r.owner.load(Relaxed) != me {
        raise!(RuntimeError, "cannot release un-acquired lock");
    }
    if c == 1 {
        r.count.store(0, Relaxed);
        r.owner.store(0, Relaxed);
        r.lock.release();
    } else {
        r.count.store(c - 1, Relaxed);
    }
    Ok(none::none_ref())
}
fn rlock_exit(o: &PyObject, _args: &[&PyObject]) -> PyResult {
    rlock_release(o)
}
fn rlock_locked(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(rl(o).lock.is_locked()))
}
fn rlock_is_owned(o: &PyObject) -> PyResult {
    let r = rl(o);
    Ok(bool_::new_bool(r.count.load(Relaxed) > 0 && r.owner.load(Relaxed) == my_ident()))
}
fn rlock_recursion_count(o: &PyObject) -> PyResult {
    let r = rl(o);
    let n = if r.owner.load(Relaxed) == my_ident() { r.count.load(Relaxed) } else { 0 };
    Ok(int::new_int_u128(n as u128))
}
fn rlock_release_save(o: &PyObject) -> PyResult {
    let r = rl(o);
    let c = r.count.load(Relaxed);
    if c == 0 || r.owner.load(Relaxed) != my_ident() {
        raise!(RuntimeError, "cannot release un-acquired lock");
    }
    let owner = r.owner.load(Relaxed);
    r.count.store(0, Relaxed);
    r.owner.store(0, Relaxed);
    r.lock.release();
    Ok(tuple::new_tuple(vec![int::new_int_u128(c as u128), int::new_int_u128(owner as u128)]))
}
fn rlock_acquire_restore(o: &PyObject, state: &PyObject) -> PyResult {
    if !tuple::is_tuple(state) || tuple::tuple_len(state) != 2 {
        raise!(TypeError, "_acquire_restore() argument 1 must be tuple");
    }
    let c = ab::as_i64(tuple::tuple_get(state, 0))? as u64;
    let owner = ab::as_i128_index(tuple::tuple_get(state, 1))? as u64;
    let r = rl(o);
    r.lock.acquire_timed(None)?;
    r.owner.store(owner, Relaxed);
    r.count.store(c, Relaxed);
    Ok(none::none_ref())
}
fn rlock_at_fork_reinit(o: &PyObject) -> PyResult {
    let r = rl(o);
    r.lock.state.store(0, Release);
    r.owner.store(0, Relaxed);
    r.count.store(0, Relaxed);
    Ok(none::none_ref())
}

// ===================================================================== thread handles

#[derive(Clone, Copy, PartialEq, Eq)]
enum HState {
    NotStarted,
    Starting,
    Running,
    Done,
}

pub struct HandleInner {
    state: Mutex<HState>,
    cv: Condvar,
    ident: AtomicU64,
    generation: AtomicU64,
    daemon: AtomicBool,
}

impl HandleInner {
    fn new() -> Arc<HandleInner> {
        Arc::new(HandleInner {
            state: Mutex::new(HState::NotStarted),
            cv: Condvar::new(),
            ident: AtomicU64::new(0),
            generation: AtomicU64::new(FORK_GENERATION.load(Relaxed)),
            daemon: AtomicBool::new(true),
        })
    }
    fn effective_state(&self) -> HState {
        let s = *self.state.lock().unwrap();
        if s == HState::Running && self.lost_by_fork(self.generation.load(Relaxed)) {
            // the OS thread does not exist in this (forked) process
            return HState::Done;
        }
        s
    }
    /// True if this handle's thread did not survive a fork() since the
    /// handle was created (only the forking thread survives).
    fn lost_by_fork(&self, created_generation: u64) -> bool {
        created_generation != FORK_GENERATION.load(Relaxed) && self.ident.load(Relaxed) != FORK_SURVIVOR.load(Relaxed)
    }
    fn set_done(&self) {
        *self.state.lock().unwrap() = HState::Done;
        self.cv.notify_all();
    }
}

static FORK_GENERATION: AtomicU64 = AtomicU64::new(0);
/// Ident of the thread that called fork() most recently (in a forked child).
static FORK_SURVIVOR: AtomicU64 = AtomicU64::new(0);
static NON_DAEMON: Mutex<Vec<usize>> = Mutex::new(Vec::new());
static THREAD_COUNT: AtomicUsize = AtomicUsize::new(0);
static STACK_SIZE: AtomicUsize = AtomicUsize::new(0);
const DEFAULT_STACK: usize = 64 << 20;

#[repr(C)]
pub struct HandleObject {
    ob_base: PyObject,
    weakreflist: *mut PyObject,
    inner: *const HandleInner,
}
static mut HANDLE_TYPE: PyTypeObject = PyTypeObject::ZERO;

fn hinner(o: &PyObject) -> &HandleInner {
    unsafe { &*(*(o.as_ptr() as *const HandleObject)).inner }
}

fn new_handle_object(inner: Arc<HandleInner>) -> PyRef {
    unsafe {
        let op = crate::alloc::alloc_object(&raw mut HANDLE_TYPE, std::mem::size_of::<HandleObject>());
        (*(op as *mut HandleObject)).inner = Arc::into_raw(inner);
        PyRef::from_raw(op)
    }
}
fn handle_arc(o: &PyObject) -> Arc<HandleInner> {
    unsafe {
        let p = (*(o.as_ptr() as *const HandleObject)).inner;
        Arc::increment_strong_count(p);
        Arc::from_raw(p)
    }
}
unsafe extern "C" fn handle_new(_tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        if tuple::tuple_len(&*args) != 0 || (!kw.is_null() && dict::dict_len(&*kw) != 0) {
            return into_c(Err(err!(TypeError, "_ThreadHandle() takes no arguments")));
        }
        new_handle_object(HandleInner::new()).into_raw()
    }
}
unsafe extern "C" fn handle_dealloc(op: *mut PyObject) {
    unsafe {
        crate::types::weakref::clear_weakrefs(op);
        drop(Arc::from_raw((*(op as *mut HandleObject)).inner));
        crate::alloc::free_object(op);
    }
}
unsafe extern "C" fn handle_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let h = hinner(&*op);
        pystr::new_str(&format!("<_thread._ThreadHandle object: ident={}>", h.ident.load(Relaxed))).into_raw()
    }
}
fn handle_ident(o: &PyObject) -> PyResult {
    Ok(int::new_int_u128(hinner(o).ident.load(Relaxed) as u128))
}
fn handle_join(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("join", &["timeout"], 0, 0);
    let [t] = crate::args::parse::<1>(&P, args, kw)?;
    let timeout = match t {
        None => None,
        Some(t) if t.is(none::none()) => None,
        Some(t) => {
            let v = ab::float_value(t)?;
            if v < 0.0 { None } else { Some(Duration::from_secs_f64(v.min(TIMEOUT_MAX))) }
        }
    };
    let h = handle_arc(o);
    match h.effective_state() {
        HState::NotStarted => raise!(RuntimeError, "thread not started"),
        HState::Done => return Ok(none::none_ref()),
        _ => {}
    }
    if h.ident.load(Relaxed) == my_ident() {
        raise!(RuntimeError, "Cannot join current thread");
    }
    if crate::gc::others_parked() {
        // the thread is stopped for good and will never finish
        raise!(PythonFinalizationError, "cannot join thread at interpreter shutdown");
    }
    let fgen = h.generation.load(Relaxed);
    wait_interruptible(
        &h.state,
        &h.cv,
        |s| *s == HState::Done || h.lost_by_fork(fgen),
        timeout,
    )?;
    // references the finished thread dropped on objects this thread owns were queued for
    // a refcount merge: apply them now, so the caller sees the objects freed
    let ts = crate::threads::current();
    if ts.c.eval_breaker.load(Relaxed) & crate::threads::EVAL_MERGE_REFCOUNTS != 0 {
        crate::threads::process_merge_queue(ts);
    }
    Ok(none::none_ref())
}
fn handle_is_done(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(hinner(o).effective_state() == HState::Done))
}
fn handle_set_done(o: &PyObject) -> PyResult {
    let h = hinner(o);
    if *h.state.lock().unwrap() == HState::NotStarted {
        raise!(RuntimeError, "thread not started");
    }
    h.set_done();
    Ok(none::none_ref())
}

// ===================================================================== starting threads

#[derive(Clone, Copy)]
struct SendPtr(*mut PyObject);
unsafe impl Send for SendPtr {}

fn spawn_thread(func: PyRef, args: PyRef, kwargs: Option<PyRef>, handle: Arc<HandleInner>, report_unraisable: bool) -> PyResult<u64> {
    {
        let mut st = handle.state.lock().unwrap();
        if *st != HState::NotStarted {
            raise!(RuntimeError, "thread already started");
        }
        if crate::modules::sys_full::is_finalizing() {
            raise!(PythonFinalizationError, "can't create new thread at interpreter shutdown");
        }
        *st = HState::Starting;
    }
    let f = SendPtr(func.into_raw());
    let a = SendPtr(args.into_raw());
    let k = SendPtr(kwargs.map_or(std::ptr::null_mut(), |k| k.into_raw()));
    let h2 = handle.clone();
    let started = Arc::new((Mutex::new(false), Condvar::new()));
    let started2 = started.clone();
    let ss = STACK_SIZE.load(Relaxed);
    let builder = std::thread::Builder::new().stack_size(if ss == 0 { DEFAULT_STACK } else { ss });
    THREAD_COUNT.fetch_add(1, Relaxed);
    let res = builder.spawn(move || {
        let (f, a, k) = (f, a, k);
        let ts = crate::threads::current();
        h2.ident.store(ts.c.thread_id as u64, Release);
        h2.generation.store(FORK_GENERATION.load(Relaxed), Relaxed);
        *h2.state.lock().unwrap() = HState::Running;
        {
            let (m, cv) = &*started2;
            *m.lock().unwrap() = true;
            cv.notify_all();
        }
        drop(started2);
        unsafe {
            let func = PyRef::from_raw(f.0);
            let args = PyRef::from_raw(a.0);
            let kw = PyRef::from_raw_opt(k.0);
            let items: Vec<&PyObject> = tuple::tuple_items(&args).to_vec();
            let r = ab::call_with_kwargs_dict(&func, &items, kw.as_deref());
            match r {
                Ok(v) => drop(v),
                Err(_) => {
                    if exc_matches(crate::types::exceptions::SystemExit()) {
                        clear_exc();
                    } else if report_unraisable {
                        let msg = format!(
                            "Exception ignored in thread started by {}",
                            ab::repr_string(&func).unwrap_or_else(|_| {
                                clear_exc();
                                "<object repr() failed>".into()
                            })
                        );
                        // the hook must see the exception
                        crate::modules::sys_full::write_unraisable(Some(&msg), None);
                    } else {
                        crate::modules::sys_full::write_unraisable(Some("Exception ignored in thread started by"), Some(&func));
                    }
                }
            }
            drop(items);
            drop(args);
            drop(kw);
            drop(func);
        }
        clear_thread_locals(ts.c.id);
        THREAD_COUNT.fetch_sub(1, Relaxed);
        crate::threads::thread_exit();
        h2.set_done();
        {
            let mut nd = NON_DAEMON.lock().unwrap();
            let me = Arc::as_ptr(&h2) as usize;
            if let Some(i) = nd.iter().position(|&p| p == me) {
                nd.swap_remove(i);
                unsafe { drop(Arc::from_raw(me as *const HandleInner)) };
            }
        }
    });
    match res {
        Ok(jh) => {
            drop(jh); // detach
            let (m, cv) = &*started;
            crate::threads::blocking(|| {
                let mut g = m.lock().unwrap();
                while !*g {
                    g = cv.wait(g).unwrap();
                }
            });
            Ok(handle.ident.load(Acquire))
        }
        Err(_) => {
            THREAD_COUNT.fetch_sub(1, Relaxed);
            *handle.state.lock().unwrap() = HState::NotStarted;
            unsafe {
                xdecref(f.0);
                xdecref(a.0);
                xdecref(k.0);
            }
            raise!(RuntimeError, "can't start new thread");
        }
    }
}

fn start_new_thread(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.len() < 2 || args.len() > 3 {
        raise!(TypeError, "start_new_thread expected at least 2 arguments, got {}", args.len());
    }
    let func = args[0];
    if !ab::is_callable(func) {
        raise!(TypeError, "first arg must be callable");
    }
    if !tuple::is_tuple(args[1]) {
        raise!(TypeError, "2nd arg must be a tuple");
    }
    let kw = match args.get(2) {
        Some(k) if !dict::is_dict(k) => raise!(TypeError, "optional 3rd arg must be a dictionary"),
        Some(k) => Some(k.to_ref()),
        None => None,
    };
    crate::modules::sys_full::audit("_thread.start_new_thread", &[func, args[1], kw.as_deref().unwrap_or(none::none())])?;
    let h = HandleInner::new();
    let ident = spawn_thread(func.to_ref(), args[1].to_ref(), kw, h, true)?;
    Ok(int::new_int_u128(ident as u128))
}

fn start_joinable_thread(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("start_joinable_thread", &["function", "handle", "daemon"], 0, 1);
    let [f, h, d] = crate::args::parse::<3>(&P, args, kw)?;
    let f = f.unwrap();
    if !ab::is_callable(f) {
        raise!(TypeError, "thread function must be callable");
    }
    let daemon = match d {
        Some(d) => ab::is_true(d)?,
        None => true,
    };
    // audited with the handle as passed (None when the call creates one)
    crate::modules::sys_full::audit(
        "_thread.start_joinable_thread",
        &[f, &int::new_int(daemon as i64), h.unwrap_or(none::none())],
    )?;
    let hobj = match h {
        Some(h) if !h.is(none::none()) => {
            if !h.is_type(&raw mut HANDLE_TYPE) {
                raise!(TypeError, "'handle' must be a _ThreadHandle");
            }
            h.to_ref()
        }
        _ => new_handle_object(HandleInner::new()),
    };
    let inner = handle_arc(&hobj);
    inner.daemon.store(daemon, Relaxed);
    if !daemon {
        NON_DAEMON.lock().unwrap().push(Arc::into_raw(inner.clone()) as usize);
    }
    match spawn_thread(f.to_ref(), tuple::empty_tuple(), None, inner.clone(), false) {
        Ok(_) => Ok(hobj),
        Err(e) => {
            if !daemon {
                let mut nd = NON_DAEMON.lock().unwrap();
                let me = Arc::as_ptr(&inner) as usize;
                if let Some(i) = nd.iter().position(|&p| p == me) {
                    nd.swap_remove(i);
                    unsafe { drop(Arc::from_raw(me as *const HandleInner)) };
                }
            }
            Err(e)
        }
    }
}

pub fn thread_shutdown(_m: &PyObject) -> PyResult {
    loop {
        let next: Option<Arc<HandleInner>> = {
            let nd = NON_DAEMON.lock().unwrap();
            let me = my_ident();
            nd.iter()
                .map(|&p| unsafe {
                    Arc::increment_strong_count(p as *const HandleInner);
                    Arc::from_raw(p as *const HandleInner)
                })
                .find(|h| h.ident.load(Relaxed) != me && h.effective_state() != HState::Done)
        };
        match next {
            Some(h) => {
                let fgen = h.generation.load(Relaxed);
                wait_interruptible(&h.state, &h.cv, |s| *s == HState::Done || h.lost_by_fork(fgen), None)?;
                let mut nd = NON_DAEMON.lock().unwrap();
                let me = Arc::as_ptr(&h) as usize;
                if let Some(i) = nd.iter().position(|&p| p == me) {
                    nd.swap_remove(i);
                    unsafe { drop(Arc::from_raw(me as *const HandleInner)) };
                }
            }
            None => break,
        }
    }
    Ok(none::none_ref())
}

static MAIN_HANDLE: Mutex<usize> = Mutex::new(0);

fn make_thread_handle(_m: &PyObject, ident: &PyObject) -> PyResult {
    let id = ab::as_i128_index(ident)? as u64;
    let h = HandleInner::new();
    h.ident.store(id, Relaxed);
    *h.state.lock().unwrap() = HState::Running;
    Ok(new_handle_object(h))
}
fn get_main_thread_ident(_m: &PyObject) -> PyResult {
    let id = crate::threads::MAIN_THREAD_ID.load(Acquire);
    Ok(int::new_int_u128(if id == 0 { my_ident() } else { id } as u128))
}

fn allocate_lock(_m: &PyObject) -> PyResult {
    Ok(new_lock())
}
fn get_ident(_m: &PyObject) -> PyResult {
    Ok(int::new_int_u128(my_ident() as u128))
}
fn get_native_id(_m: &PyObject) -> PyResult {
    Ok(int::new_int(unsafe { libc::gettid() } as i64))
}
fn thread_count(_m: &PyObject) -> PyResult {
    Ok(int::new_int(THREAD_COUNT.load(Relaxed) as i64))
}
fn thread_exit_fn(_m: &PyObject) -> PyResult {
    Err(set_err_noargs(crate::types::exceptions::SystemExit()))
}
fn stack_size(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("stack_size", args, 0, 1)?;
    let old = STACK_SIZE.load(Relaxed);
    if let Some(a) = args.first() {
        let n = ab::index(a)?;
        let n = match int::as_i64(&n) {
            Some(n) => n,
            None => raise!(OverflowError, "Python int too large to convert to C ssize_t"),
        };
        if n < 0 {
            raise!(ValueError, "size must be 0 or a positive value");
        }
        if n != 0 && n < 32768 {
            raise!(ValueError, "size not valid: {} bytes", n);
        }
        STACK_SIZE.store(n as usize, Relaxed);
    }
    Ok(int::new_int(old as i64))
}
fn interrupt_main(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("interrupt_main", &["signum"], 0, 0);
    let [s] = crate::args::parse::<1>(&P, args, kw)?;
    let signum = match s {
        Some(s) => ab::as_i64(s)? as i32,
        None => libc::SIGINT,
    };
    crate::modules::signal::set_interrupt(signum)?;
    // Called from the main thread: the handler runs right away (before the
    // caller's next statement), as it would after the call returns.
    crate::modules::signal::check_signals()?;
    Ok(none::none_ref())
}
fn daemon_threads_allowed(_m: &PyObject) -> PyResult {
    Ok(bool_::new_bool(true))
}
fn is_main_interpreter(_m: &PyObject) -> PyResult {
    Ok(bool_::new_bool(true))
}

pub const NAME_MAXLEN: usize = 15;

/// Encode a str as UTF-8, replacing unencodable code points (surrogates) with '?'.
pub fn encode_replace(s: &PyObject) -> Vec<u8> {
    let d = pystr::str_data(s);
    let mut out = Vec::new();
    for i in 0..d.len() {
        let c = d.get(i);
        match char::from_u32(c) {
            Some(ch) => {
                let mut b = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
            }
            None => out.push(b'?'),
        }
    }
    out
}
/// Decode bytes as UTF-8 with surrogateescape.
pub fn decode_surrogateescape(b: &[u8]) -> PyRef {
    let mut cps: Vec<u32> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match std::str::from_utf8(&b[i..]) {
            Ok(s) => {
                cps.extend(s.chars().map(|c| c as u32));
                break;
            }
            Err(e) => {
                let good = e.valid_up_to();
                let s = unsafe { std::str::from_utf8_unchecked(&b[i..i + good]) };
                cps.extend(s.chars().map(|c| c as u32));
                i += good;
                let bad = e.error_len().unwrap_or(b.len() - i);
                for k in 0..bad {
                    cps.push(0xDC00 + b[i + k] as u32);
                }
                i += bad;
            }
        }
    }
    pystr::from_codepoints(&cps)
}

fn set_name(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("set_name", &["name"], 0, 1);
    let [n] = crate::args::parse::<1>(&P, args, kw)?;
    let n = n.unwrap();
    if !pystr::is_str(n) {
        raise!(TypeError, "set_name() argument 'name' must be str, not {}", tname(n));
    }
    let mut b = encode_replace(n);
    if let Some(i) = b.iter().position(|&c| c == 0) {
        b.truncate(i);
    }
    b.truncate(NAME_MAXLEN);
    b.push(0);
    let r = unsafe { libc::pthread_setname_np(libc::pthread_self(), b.as_ptr() as *const libc::c_char) };
    if r != 0 {
        return Err(crate::types::exceptions::os_error_from_errno(r, None, None));
    }
    Ok(none::none_ref())
}
fn get_name(_m: &PyObject) -> PyResult {
    let mut buf = [0u8; 64];
    let r = unsafe { libc::pthread_getname_np(libc::pthread_self(), buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if r != 0 {
        return Err(crate::types::exceptions::os_error_from_errno(r, None, None));
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Ok(decode_surrogateescape(&buf[..n]))
}

// ===================================================================== _local

#[repr(C)]
pub struct LocalObject {
    ob_base: PyObject,
    weakreflist: *mut PyObject,
    id: u64,
    args: *mut PyObject,
    kw: *mut PyObject,
    /// thread serial -> owned dict pointer
    dicts: Mutex<HashMap<u64, usize>>,
}

static mut LOCAL_TYPE: PyTypeObject = PyTypeObject::ZERO;
static NEXT_LOCAL_ID: AtomicU64 = AtomicU64::new(1);
/// local id -> local object address (borrowed), for thread-exit cleanup.
static LIVE_LOCALS: Mutex<Option<HashMap<u64, usize>>> = Mutex::new(None);

fn loc(o: &PyObject) -> &LocalObject {
    unsafe { &*(o.as_ptr() as *const LocalObject) }
}

unsafe extern "C" fn local_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let has_args = tuple::tuple_len(&*args) > 0 || (!kw.is_null() && dict::dict_len(&*kw) > 0);
        if has_args {
            let init = crate::types::typeobject::lookup_str(tp, "__init__");
            let base_init = crate::types::typeobject::lookup_str(crate::types::typeobject::object_type(), "__init__");
            if let (Some(a), Some(b)) = (init, base_init) {
                if a.is(&b) {
                    return into_c(Err(err!(TypeError, "Initialization arguments are not supported")));
                }
            }
        }
        let op = ((*tp).tp_alloc.unwrap())(tp, 0);
        if op.is_null() {
            return op;
        }
        let l = op as *mut LocalObject;
        let id = NEXT_LOCAL_ID.fetch_add(1, Relaxed);
        (*l).id = id;
        (*l).args = (*args).to_ref().into_raw();
        (*l).kw = if kw.is_null() { std::ptr::null_mut() } else { (*kw).to_ref().into_raw() };
        std::ptr::write(&raw mut (*l).dicts, Mutex::new(HashMap::new()));
        LIVE_LOCALS.lock().unwrap().get_or_insert_with(HashMap::new).insert(id, op as usize);
        // the creating thread gets its dict now (without calling __init__ again)
        let ts = crate::threads::current();
        (*l).dicts.lock().unwrap().insert(ts.c.id, dict::new_dict().into_raw() as usize);
        op
    }
}

unsafe extern "C" fn local_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        crate::types::weakref::clear_weakrefs(op);
        let l = op as *mut LocalObject;
        if let Some(m) = LIVE_LOCALS.lock().unwrap().as_mut() {
            m.remove(&(*l).id);
        }
        let dicts: Vec<usize> = (*l).dicts.lock().unwrap().drain().map(|(_, d)| d).collect();
        for d in dicts {
            decref(d as *mut PyObject);
        }
        std::ptr::drop_in_place(&raw mut (*l).dicts);
        xdecref((*l).args);
        xdecref((*l).kw);
        let tp = (*op).type_ptr();
        if crate::types::typeobject::is_heap_type(tp) {
            // clear subclass slots / managed dict
            if let Some(clear) = (*tp).tp_clear {
                if clear as usize != local_clear as usize {
                    clear(op);
                }
            }
        }
        crate::types::typeobject::generic_free_and_type_decref(op, tp);
    }
}
unsafe extern "C" fn local_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let l = &*(op as *const LocalObject);
        for p in [l.args, l.kw] {
            if !p.is_null() {
                let r = visit.unwrap()(p, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        if let Ok(g) = l.dicts.try_lock() {
            for &d in g.values() {
                let r = visit.unwrap()(d as *mut PyObject, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        if crate::types::typeobject::is_heap_type((*op).type_ptr()) {
            return visit.unwrap()((*op).type_ptr() as *mut PyObject, arg);
        }
        0
    }
}
unsafe extern "C" fn local_clear(op: *mut PyObject) -> c_int {
    unsafe {
        let l = op as *mut LocalObject;
        let dicts: Vec<usize> = (*l).dicts.lock().unwrap().drain().map(|(_, d)| d).collect();
        for d in dicts {
            decref(d as *mut PyObject);
        }
        0
    }
}

/// Get (creating and initialising if needed) this thread's dict for the local.
fn local_dict(o: &PyObject) -> PyResult {
    let l = loc(o);
    let key = crate::threads::current().c.id;
    if let Some(&d) = l.dicts.lock().unwrap().get(&key) {
        return Ok(unsafe { PyRef::from_borrowed(d as *mut PyObject) });
    }
    let d = dict::new_dict();
    l.dicts.lock().unwrap().insert(key, d.clone().into_raw() as usize);
    // call __init__ with the original arguments if overridden
    let tp = o.type_ptr();
    let init = crate::types::typeobject::lookup_str(tp, "__init__");
    let base_init = crate::types::typeobject::lookup_str(crate::types::typeobject::object_type(), "__init__");
    let overridden = match (&init, &base_init) {
        (Some(a), Some(b)) => !a.is(b),
        _ => false,
    };
    if overridden {
        let args = unsafe { &*l.args };
        let items: Vec<&PyObject> = tuple::tuple_items(args).to_vec();
        let kw = if l.kw.is_null() { None } else { Some(unsafe { &*l.kw }) };
        let r = ab::call_method_str(o, "__init__", &[]).ok().map(|_| ());
        let _ = r;
        // call_method_str above cannot pass kwargs; redo properly
        clear_exc();
        let bound = ab::getattr_str(o, "__init__");
        let r = match bound {
            Ok(b) => ab::call_with_kwargs_dict(&b, &items, kw).map(|_| ()),
            Err(e) => Err(e),
        };
        if r.is_err() {
            if let Some(dp) = l.dicts.lock().unwrap().remove(&key) {
                unsafe { decref(dp as *mut PyObject) };
            }
            return Err(PyErr);
        }
    }
    Ok(d)
}

unsafe extern "C" fn local_getattro(op: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        let n = &*name;
        let r = (|| -> PyResult {
            let d = local_dict(o)?;
            let is_dict_name = if pystr::is_str(n) {
                pystr::str_eq_ascii(n, "__dict__")
            } else {
                // compared like any object (the name check comes first)
                ab::rich_compare_bool(n, &pystr::new_str("__dict__"), crate::ffi::Py_EQ)?
            };
            if is_dict_name {
                return Ok(d);
            }
            match crate::types::typeobject::generic_getattr_opt(o, n, Some(&d))? {
                Some(v) => Ok(v),
                None => Err(crate::types::typeobject::attribute_error_for(o, n)),
            }
        })();
        into_c(r)
    }
}
unsafe extern "C" fn local_setattro(op: *mut PyObject, name: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let o = &*op;
        let n = &*name;
        let r = (|| -> PyResult<()> {
            let is_dict_name = if pystr::is_str(n) {
                pystr::str_eq_ascii(n, "__dict__")
            } else {
                // compared like any object (the name check comes first)
                ab::rich_compare_bool(n, &pystr::new_str("__dict__"), crate::ffi::Py_EQ)?
            };
            if is_dict_name {
                raise!(AttributeError, "'{}' object attribute '__dict__' is read-only", tname(o));
            }
            let d = local_dict(o)?;
            crate::types::typeobject::generic_setattr_with_dict(o, n, if v.is_null() { None } else { Some(&*v) }, Some(&d))
        })();
        int_into_c(r)
    }
}

/// Drop all `_local` dicts belonging to the thread with serial `key`.
pub fn clear_thread_locals(key: u64) {
    let mut drop_list: Vec<usize> = Vec::new();
    {
        let g = LIVE_LOCALS.lock().unwrap();
        if let Some(m) = g.as_ref() {
            for &lp in m.values() {
                let l = unsafe { &*(lp as *const LocalObject) };
                if let Some(d) = l.dicts.lock().unwrap().remove(&key) {
                    drop_list.push(d);
                }
            }
        }
    }
    for d in drop_list {
        unsafe { decref(d as *mut PyObject) };
    }
}

// ===================================================================== excepthook

static EXCEPTHOOK_ARGS_TYPE: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());

fn thread_excepthook(_m: &PyObject, a: &PyObject) -> PyResult {
    let tp = EXCEPTHOOK_ARGS_TYPE.load(Acquire);
    if !a.is_type(tp) {
        raise!(TypeError, "_thread._excepthook argument type must be ExceptHookArgs");
    }
    use crate::modules::sys_structseq::get_field;
    let et = get_field(a, 0);
    let ev = get_field(a, 1);
    let tb = get_field(a, 2);
    let thread = get_field(a, 3);
    if et.is(type_obj(crate::types::exceptions::SystemExit())) {
        return Ok(none::none_ref());
    }
    let mut file = crate::modules::sys::get_sys_attr("stderr").filter(|f| !f.is(none::none()));
    if file.is_none() {
        if !thread.is(none::none()) {
            file = ab::getattr_opt(&thread, &pystr::new_str("_stderr"))?.filter(|f| !f.is(none::none()));
        }
        if file.is_none() {
            return Ok(none::none_ref());
        }
    }
    let file = file.unwrap();
    let name = if !thread.is(none::none()) {
        match ab::getattr_opt(&thread, &pystr::new_str("name"))? {
            Some(n) if !n.is(none::none()) => ab::str_string(&n)?,
            _ => my_ident().to_string(),
        }
    } else {
        my_ident().to_string()
    };
    ab::call_method_str(&file, "write", &[&pystr::new_str(&format!("Exception in thread {}:\n", name))])?;
    let _ = ab::call_method_str(&file, "flush", &[]);
    clear_exc();
    let tbm = crate::import::import_absolute("traceback");
    match tbm {
        Ok(m) => {
            let f = ab::getattr_str(&m, "print_exception")?;
            let kwn = tuple::new_tuple(vec![pystr::new_str("file")]);
            ab::call_kw(&f, &[&et, &ev, &tb, &file], Some(&kwn))?;
        }
        Err(_) => {
            clear_exc();
            if crate::types::exceptions::is_exception_instance(&ev) {
                let s = crate::modules::traceback_print::format_exception(&ev);
                ab::call_method_str(&file, "write", &[&pystr::new_str(&s)])?;
            }
        }
    }
    let _ = ab::call_method_str(&file, "flush", &[]);
    clear_exc();
    Ok(none::none_ref())
}

// ===================================================================== fork support

extern "C" fn atfork_child() {
    FORK_GENERATION.fetch_add(1, Relaxed);
    FORK_SURVIVOR.store(my_ident(), Relaxed);
    let me = my_ident();
    crate::threads::MAIN_THREAD_ID.store(me, Release);
    let ts = crate::threads::current_ptr();
    if !ts.is_null() {
        crate::threads::MAIN_THREAD_STATE.store(ts, Release);
    }
    crate::threads::after_fork_child();
    THREAD_COUNT.store(0, Relaxed);
    if let Ok(mut nd) = NON_DAEMON.try_lock() {
        nd.clear();
    }
}

// ===================================================================== module

fn init_types() -> PyResult<()> {
    unsafe {
        if LOCK_TYPE.tp_flags & Py_TPFLAGS_READY != 0 {
            return Ok(());
        }
        let t = &mut *(&raw mut LOCK_TYPE);
        t.tp_name = c"_thread.lock".as_ptr();
        t.tp_basicsize = std::mem::size_of::<LockObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_IMMUTABLETYPE;
        t.tp_weaklistoffset = std::mem::offset_of!(LockObject, weakreflist) as isize;
        t.tp_new = Some(lock_new);
        t.tp_dealloc = Some(lock_dealloc);
        t.tp_repr = Some(lock_repr);
        t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
        t.tp_doc = c"lock()\n--\n\nA primitive mutual-exclusion lock, initially unlocked.".as_ptr();
        t.tp_methods = crate::mdefs![
            ("acquire", FASTKW, lock_acquire, "acquire($self, /, blocking=True, timeout=-1)\n--\n\nAcquire the lock, waiting up to timeout seconds (-1: forever) if blocking.\n\nReturn True if the lock was acquired."),
            ("acquire_lock", FASTKW, lock_acquire, "acquire_lock($self, /, blocking=True, timeout=-1)\n--\n\nDeprecated alias of acquire()."),
            ("release", NOARGS, lock_release),
            ("release_lock", NOARGS, lock_release),
            ("locked", NOARGS, lock_locked),
            ("locked_lock", NOARGS, lock_locked),
            ("__enter__", NOARGS, lock_enter),
            ("__exit__", FAST, lock_exit, "__exit__($self, /, *exc_info)\n--\n\nRelease the lock when leaving a with block."),
            ("_at_fork_reinit", NOARGS, lock_at_fork_reinit),
        ];
        crate::types::typeobject::type_ready(&raw mut LOCK_TYPE)?;

        let t = &mut *(&raw mut RLOCK_TYPE);
        t.tp_name = c"_thread.RLock".as_ptr();
        t.tp_basicsize = std::mem::size_of::<RLockObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE;
        t.tp_weaklistoffset = std::mem::offset_of!(RLockObject, weakreflist) as isize;
        t.tp_new = Some(rlock_new);
        t.tp_alloc = Some(crate::types::typeobject::generic_alloc);
        t.tp_dealloc = Some(rlock_dealloc);
        t.tp_repr = Some(rlock_repr);
        t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
        t.tp_methods = crate::mdefs![
            ("acquire", FASTKW, rlock_acquire, "acquire($self, /, blocking=True, timeout=-1)\n--\n\nAcquire the lock; the owning thread may acquire it again.\n\nReturn True if the lock was acquired."),
            ("release", NOARGS, rlock_release),
            ("locked", NOARGS, rlock_locked),
            ("__enter__", NOARGS, rlock_enter),
            ("__exit__", FAST, rlock_exit, "__exit__($self, /, *exc_info)\n--\n\nRelease the lock once when leaving a with block."),
            ("_is_owned", NOARGS, rlock_is_owned),
            ("_recursion_count", NOARGS, rlock_recursion_count),
            ("_release_save", NOARGS, rlock_release_save),
            ("_acquire_restore", O, rlock_acquire_restore),
            ("_at_fork_reinit", NOARGS, rlock_at_fork_reinit),
        ];
        crate::types::typeobject::type_ready(&raw mut RLOCK_TYPE)?;

        let t = &mut *(&raw mut HANDLE_TYPE);
        t.tp_name = c"_thread._ThreadHandle".as_ptr();
        t.tp_basicsize = std::mem::size_of::<HandleObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_IMMUTABLETYPE;
        t.tp_weaklistoffset = std::mem::offset_of!(HandleObject, weakreflist) as isize;
        t.tp_new = Some(handle_new);
        t.tp_dealloc = Some(handle_dealloc);
        t.tp_repr = Some(handle_repr);
        t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
        t.tp_methods = crate::mdefs![
            ("join", FASTKW, handle_join, "join($self, /, timeout=None)\n--\n\nWait for the thread to finish."),
            ("is_done", NOARGS, handle_is_done),
            ("_set_done", NOARGS, handle_set_done),
        ];
        t.tp_getset = crate::getset_table![("ident", handle_ident)];
        crate::types::typeobject::type_ready(&raw mut HANDLE_TYPE)?;

        let t = &mut *(&raw mut LOCAL_TYPE);
        t.tp_name = c"_thread._local".as_ptr();
        t.tp_basicsize = std::mem::size_of::<LocalObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_GC;
        t.tp_weaklistoffset = std::mem::offset_of!(LocalObject, weakreflist) as isize;
        t.tp_new = Some(local_new);
        t.tp_alloc = Some(crate::types::typeobject::generic_alloc);
        t.tp_dealloc = Some(local_dealloc);
        t.tp_traverse = Some(local_traverse);
        t.tp_clear = Some(local_clear);
        t.tp_getattro = Some(local_getattro);
        t.tp_setattro = Some(local_setattro);
        t.tp_doc = c"_local()\n--\n\nAn object whose attributes are separate for every thread.".as_ptr();
        crate::types::typeobject::type_ready(&raw mut LOCAL_TYPE)?;

        let eh = crate::modules::sys_structseq::make_type(
            "_thread._ExceptHookArgs",
            &["exc_type", "exc_value", "exc_traceback", "thread"],
            4,
            true,
        )?;
        EXCEPTHOOK_ARGS_TYPE.store(eh, Release);
        libc::pthread_atfork(None, None, Some(atfork_child));
        Ok(())
    }
}

pub fn init_module() -> PyResult {
    init_types()?;
    let m = crate::types::module::new_module("_thread")?;
    let d = crate::types::module::module_dict(&m);
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![
            ("start_new_thread", FAST, start_new_thread, "start_new_thread($module, function, args, kwargs=None, /)\n--\n\nStart a thread running function(*args, **kwargs); return its identifier."),
            ("start_new", FAST, start_new_thread, "start_new($module, function, args, kwargs=None, /)\n--\n\nDeprecated alias of start_new_thread()."),
            ("start_joinable_thread", FASTKW, start_joinable_thread, "start_joinable_thread($module, /, function, handle=None, daemon=True)\n--\n\nStart a thread running function() and return a handle that can be joined."),
            ("allocate_lock", NOARGS, allocate_lock),
            ("allocate", NOARGS, allocate_lock),
            ("get_ident", NOARGS, get_ident),
            ("get_native_id", NOARGS, get_native_id),
            ("_count", NOARGS, thread_count),
            ("exit", NOARGS, thread_exit_fn),
            ("exit_thread", NOARGS, thread_exit_fn),
            ("stack_size", FAST, stack_size, "stack_size($module, size=0, /)\n--\n\nReturn the stack size for new threads, setting a new one when size is given."),
            ("interrupt_main", FASTKW, interrupt_main, "interrupt_main($module, /, signum=2)\n--\n\nSimulate the arrival of a signal (SIGINT by default) in the main thread."),
            ("daemon_threads_allowed", NOARGS, daemon_threads_allowed),
            ("_shutdown", NOARGS, thread_shutdown),
            ("_make_thread_handle", O, make_thread_handle),
            ("_get_main_thread_ident", NOARGS, get_main_thread_ident),
            ("_is_main_interpreter", NOARGS, is_main_interpreter),
            ("_excepthook", O, thread_excepthook),
            ("set_name", FASTKW, set_name, "set_name($module, /, name)\n--\n\nGive the current thread a name at the operating system level."),
            ("_get_name", NOARGS, get_name),
        ],
        Some(&m),
    )?;
    let set = |k: &str, v: &PyObject| dict::set_item_str(&d, k, v);
    set("LockType", type_obj(&raw mut LOCK_TYPE))?;
    set("lock", type_obj(&raw mut LOCK_TYPE))?;
    set("RLock", type_obj(&raw mut RLOCK_TYPE))?;
    set("_ThreadHandle", type_obj(&raw mut HANDLE_TYPE))?;
    set("_local", type_obj(&raw mut LOCAL_TYPE))?;
    set("_ExceptHookArgs", type_obj(EXCEPTHOOK_ARGS_TYPE.load(Acquire)))?;
    set("error", type_obj(crate::types::exceptions::RuntimeError()))?;
    set("TIMEOUT_MAX", &float::new_float(TIMEOUT_MAX))?;
    set("_NAME_MAXLEN", &int::new_int(NAME_MAXLEN as i64))?;
    let _ = (&MAIN_HANDLE, AtomicI64::new(0), type_ref(&raw mut LOCK_TYPE));
    Ok(m)
}

/// In a forked child: drop the thread-local values of every thread except `keep` (those
/// threads do not exist in the child; their values' finalizers run now).
pub fn clear_foreign_thread_locals(keep: u64) {
    let mut drop_list: Vec<usize> = Vec::new();
    {
        let g = LIVE_LOCALS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = g.as_ref() {
            for &lp in m.values() {
                let l = unsafe { &*(lp as *const LocalObject) };
                let mut dicts = l.dicts.lock().unwrap_or_else(|e| e.into_inner());
                let gone: Vec<u64> = dicts.keys().copied().filter(|&k| k != keep).collect();
                for k in gone {
                    if let Some(d) = dicts.remove(&k) {
                        drop_list.push(d);
                    }
                }
            }
        }
    }
    for d in drop_list {
        unsafe { decref(d as *mut PyObject) };
    }
}
