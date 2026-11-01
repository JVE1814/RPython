//! pythread.h / cpython/pylock.h / critical_section.h: native threads, locks, TSS,
//! PyMutex (parking-lot slow path) and per-object critical sections (ob_mutex).

use super::*;
use std::ffi::{c_uint, c_ulong, c_void};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering::*};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- PyMutex

const LOCKED: u8 = 1;
const HAS_PARKED: u8 = 2;
const NBUCKETS: usize = 64;

struct Bucket {
    m: Mutex<()>,
    cv: Condvar,
}
static BUCKETS: [Bucket; NBUCKETS] = [const { Bucket { m: Mutex::new(()), cv: Condvar::new() } }; NBUCKETS];
fn bucket(addr: usize) -> &'static Bucket {
    &BUCKETS[(addr >> 3).wrapping_mul(0x9E37_79B9) % NBUCKETS]
}

/// Lock a 1-byte mutex with CPython's bit protocol (bit 0 locked, bit 1 has parked waiters).
pub(crate) fn mutex_lock(m: &AtomicU8) {
    let mut spins = 0;
    loop {
        let v = m.load(Relaxed);
        if v & LOCKED == 0 {
            if m.compare_exchange_weak(v, v | LOCKED, Acquire, Relaxed).is_ok() {
                return;
            }
            continue;
        }
        if spins < 40 {
            spins += 1;
            std::hint::spin_loop();
            if spins > 20 {
                std::thread::yield_now();
            }
            continue;
        }
        // park
        let b = bucket(m as *const AtomicU8 as usize);
        let g = b.m.lock().unwrap();
        let v = m.load(Relaxed);
        if v & LOCKED == 0 {
            continue;
        }
        if v & HAS_PARKED == 0 && m.compare_exchange(v, v | HAS_PARKED, Relaxed, Relaxed).is_err() {
            continue;
        }
        let _ = b.cv.wait_timeout(g, Duration::from_millis(5)).unwrap();
    }
}
pub(crate) fn mutex_unlock(m: &AtomicU8) {
    let v = m.swap(0, Release);
    if v & HAS_PARKED != 0 {
        let b = bucket(m as *const AtomicU8 as usize);
        let _g = b.m.lock().unwrap();
        b.cv.notify_all();
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMutex_Lock(m: *mut u8) {
    unsafe { mutex_lock(AtomicU8::from_ptr(m)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMutex_Unlock(m: *mut u8) {
    unsafe {
        let a = AtomicU8::from_ptr(m);
        if a.load(Relaxed) & LOCKED == 0 {
            super::refcount::fatal("unlocking mutex that is not locked");
        }
        mutex_unlock(a)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMutex_IsLocked(m: *mut u8) -> c_int {
    unsafe { (AtomicU8::from_ptr(m).load(Acquire) & LOCKED != 0) as c_int }
}

// ---------------------------------------------------------------- critical sections

#[repr(C)]
pub struct PyCriticalSection {
    pub _cs_prev: usize,
    pub _cs_mutex: *mut u8,
}
#[repr(C)]
pub struct PyCriticalSection2 {
    pub _cs_base: PyCriticalSection,
    pub _cs_mutex2: *mut u8,
}
const CS_TWO: usize = 1;

/// Is mutex `m` held by an active critical section of this thread?
unsafe fn held_by_me(m: *mut u8) -> bool {
    unsafe {
        let mut p = (*crate::threads::current_mut()).c.critical_section;
        let mut n = 0;
        while p != 0 && n < 10_000 {
            let cs = (p & !CS_TWO) as *mut PyCriticalSection;
            if (*cs)._cs_mutex == m {
                return true;
            }
            if p & CS_TWO != 0 && (*(cs as *mut PyCriticalSection2))._cs_mutex2 == m {
                return true;
            }
            p = (*cs)._cs_prev;
            n += 1;
        }
        false
    }
}
unsafe fn push(c: *mut PyCriticalSection, tag: usize) {
    unsafe {
        let ts = crate::threads::current_mut();
        (*c)._cs_prev = (*ts).c.critical_section;
        (*ts).c.critical_section = c as usize | tag;
    }
}
unsafe fn pop(c: *mut PyCriticalSection) {
    unsafe {
        let ts = crate::threads::current_mut();
        (*ts).c.critical_section = (*c)._cs_prev;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCriticalSection_BeginMutex(c: *mut PyCriticalSection, m: *mut u8) {
    unsafe {
        if held_by_me(m) {
            (*c)._cs_mutex = std::ptr::null_mut();
            (*c)._cs_prev = 0;
            return;
        }
        mutex_lock(AtomicU8::from_ptr(m));
        (*c)._cs_mutex = m;
        push(c, 0);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCriticalSection_Begin(c: *mut PyCriticalSection, op: *mut PyObject) {
    unsafe { PyCriticalSection_BeginMutex(c, (*op).ob_mutex.as_ptr()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCriticalSection_End(c: *mut PyCriticalSection) {
    unsafe {
        if (*c)._cs_mutex.is_null() {
            return;
        }
        mutex_unlock(AtomicU8::from_ptr((*c)._cs_mutex));
        pop(c);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCriticalSection2_BeginMutex(c: *mut PyCriticalSection2, m1: *mut u8, m2: *mut u8) {
    unsafe {
        if m1 == m2 {
            PyCriticalSection_BeginMutex(&mut (*c)._cs_base, m1);
            (*c)._cs_mutex2 = std::ptr::null_mut();
            return;
        }
        // lock in address order to avoid deadlocks
        let (a, b) = if (m1 as usize) < (m2 as usize) { (m1, m2) } else { (m2, m1) };
        let ha = held_by_me(a);
        let hb = held_by_me(b);
        if !ha {
            mutex_lock(AtomicU8::from_ptr(a));
        }
        if !hb {
            mutex_lock(AtomicU8::from_ptr(b));
        }
        (*c)._cs_base._cs_mutex = if ha { std::ptr::null_mut() } else { a };
        (*c)._cs_mutex2 = if hb { std::ptr::null_mut() } else { b };
        push(&mut (*c)._cs_base, CS_TWO);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCriticalSection2_Begin(c: *mut PyCriticalSection2, a: *mut PyObject, b: *mut PyObject) {
    unsafe { PyCriticalSection2_BeginMutex(c, (*a).ob_mutex.as_ptr(), (*b).ob_mutex.as_ptr()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCriticalSection2_End(c: *mut PyCriticalSection2) {
    unsafe {
        let m1 = (*c)._cs_base._cs_mutex;
        let m2 = (*c)._cs_mutex2;
        if !m2.is_null() {
            mutex_unlock(AtomicU8::from_ptr(m2));
        }
        if !m1.is_null() {
            mutex_unlock(AtomicU8::from_ptr(m1));
        }
        let ts = crate::threads::current_mut();
        if (*ts).c.critical_section & !CS_TWO == c as usize {
            pop(&mut (*c)._cs_base);
        }
    }
}

// ---------------------------------------------------------------- PyThread locks

struct RawLock {
    locked: Mutex<bool>,
    cv: Condvar,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_allocate_lock() -> *mut c_void {
    Box::into_raw(Box::new(RawLock { locked: Mutex::new(false), cv: Condvar::new() })) as *mut c_void
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_free_lock(l: *mut c_void) {
    if !l.is_null() {
        drop(unsafe { Box::from_raw(l as *mut RawLock) });
    }
}
/// PY_LOCK_FAILURE = 0, PY_LOCK_ACQUIRED = 1, PY_LOCK_INTR = 2
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_acquire_lock_timed(l: *mut c_void, microseconds: i64, intr_flag: c_int) -> c_int {
    unsafe {
        let lk = &*(l as *const RawLock);
        let mut g = lk.locked.lock().unwrap();
        if !*g {
            *g = true;
            return 1;
        }
        if microseconds == 0 {
            return 0;
        }
        let deadline = if microseconds > 0 { Some(Instant::now() + Duration::from_micros(microseconds as u64)) } else { None };
        let ts = crate::threads::current();
        let old = ts.mode.replace(crate::threads::ThreadMode::Blocked);
        let r = loop {
            if !*g {
                *g = true;
                break 1;
            }
            let wait = match deadline {
                Some(d) => {
                    let now = Instant::now();
                    if now >= d {
                        break 0;
                    }
                    (d - now).min(Duration::from_millis(50))
                }
                None => Duration::from_millis(50),
            };
            g = lk.cv.wait_timeout(g, wait).unwrap().0;
            if intr_flag != 0 && ts.c.eval_breaker.load(Relaxed) & crate::threads::EVAL_SIGNALS != 0 {
                break 2;
            }
        };
        ts.mode.set(old);
        r
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_acquire_lock(l: *mut c_void, waitflag: c_int) -> c_int {
    unsafe { (PyThread_acquire_lock_timed(l, if waitflag != 0 { -1 } else { 0 }, 0) == 1) as c_int }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_release_lock(l: *mut c_void) {
    unsafe {
        let lk = &*(l as *const RawLock);
        *lk.locked.lock().unwrap() = false;
        lk.cv.notify_one();
    }
}

// ---------------------------------------------------------------- threads

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_init_thread() {}
static STACK_SIZE: AtomicUsize = AtomicUsize::new(0);
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_start_new_thread(func: Option<unsafe extern "C" fn(*mut c_void)>, arg: *mut c_void) -> c_ulong {
    let Some(f) = func else { return c_ulong::MAX };
    let argp = arg as usize;
    let mut b = std::thread::Builder::new();
    let ss = STACK_SIZE.load(Relaxed);
    b = b.stack_size(if ss > 0 { ss } else { 16 << 20 });
    let (tx, rx) = std::sync::mpsc::channel::<c_ulong>();
    match b.spawn(move || {
        let _ = tx.send(unsafe { libc::pthread_self() } as c_ulong);
        unsafe { f(argp as *mut c_void) };
    }) {
        Ok(_) => rx.recv().unwrap_or(c_ulong::MAX),
        Err(_) => c_ulong::MAX,
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_exit_thread() -> ! {
    unsafe {
        libc::pthread_exit(std::ptr::null_mut());
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_get_thread_ident() -> c_ulong {
    unsafe { libc::pthread_self() as c_ulong }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_get_thread_native_id() -> c_ulong {
    unsafe { libc::gettid() as c_ulong }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_get_stacksize() -> usize {
    STACK_SIZE.load(Relaxed)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_set_stacksize(size: usize) -> c_int {
    if size != 0 && size < 32768 {
        return -1;
    }
    STACK_SIZE.store(size, Relaxed);
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_GetInfo() -> *mut PyObject {
    ret(crate::modules::sys::get_sys_attr("thread_info").ok_or_else(|| system_error("sys.thread_info missing")))
}
#[unsafe(no_mangle)]
pub static PY_TIMEOUT_MAX: i64 = i64::MAX / 1000;

// ---------------------------------------------------------------- TSS

#[repr(C)]
pub struct PyTss {
    pub _is_initialized: c_int,
    pub _key: c_uint,
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_tss_alloc() -> *mut PyTss {
    Box::into_raw(Box::new(PyTss { _is_initialized: 0, _key: 0 }))
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_tss_free(k: *mut PyTss) {
    unsafe {
        if !k.is_null() {
            PyThread_tss_delete(k);
            drop(Box::from_raw(k));
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_tss_is_created(k: *mut PyTss) -> c_int {
    unsafe { (*k)._is_initialized }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_tss_create(k: *mut PyTss) -> c_int {
    unsafe {
        if (*k)._is_initialized != 0 {
            return 0;
        }
        let mut key: libc::pthread_key_t = 0;
        if libc::pthread_key_create(&mut key, None) != 0 {
            return -1;
        }
        (*k)._key = key;
        (*k)._is_initialized = 1;
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_tss_delete(k: *mut PyTss) {
    unsafe {
        if (*k)._is_initialized == 0 {
            return;
        }
        libc::pthread_key_delete((*k)._key);
        (*k)._is_initialized = 0;
        (*k)._key = 0;
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_tss_set(k: *mut PyTss, v: *mut c_void) -> c_int {
    unsafe { if libc::pthread_setspecific((*k)._key, v) == 0 { 0 } else { -1 } }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_tss_get(k: *mut PyTss) -> *mut c_void {
    unsafe { libc::pthread_getspecific((*k)._key) }
}
// deprecated TLS key API
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_create_key() -> c_int {
    unsafe {
        let mut key: libc::pthread_key_t = 0;
        if libc::pthread_key_create(&mut key, None) != 0 {
            return -1;
        }
        key as c_int
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_delete_key(k: c_int) {
    unsafe {
        libc::pthread_key_delete(k as libc::pthread_key_t);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_set_key_value(k: c_int, v: *mut c_void) -> c_int {
    unsafe { if libc::pthread_setspecific(k as libc::pthread_key_t, v) == 0 { 0 } else { -1 } }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_get_key_value(k: c_int) -> *mut c_void {
    unsafe { libc::pthread_getspecific(k as libc::pthread_key_t) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_delete_key_value(k: c_int) {
    unsafe {
        libc::pthread_setspecific(k as libc::pthread_key_t, std::ptr::null());
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThread_ReInitTLS() {}
