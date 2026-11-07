//! pythread.h / cpython/pylock.h / critical_section.h: native threads, locks, TSS,
//! PyMutex (parking slow path) and critical sections (own lock table, see below).

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
//
// Design (ours): the runtime uses `ob_mutex` as a short leaf lock around single
// container operations and never holds it while running other code. Extension
// critical sections, however, stay open across arbitrary C-API calls, which take
// that same leaf lock or run Python code. So `PyCriticalSection_Begin(op)` does
// not touch `ob_mutex`; it takes an entry for `op` in a separate lock table
// (keyed by object address). Sections over an explicit PyMutex
// (`*_BeginMutex`) lock that mutex itself.
//
// Rules: re-entering a lock this thread already holds through an active section
// is a no-op (the record is not pushed). Before blocking on a contended lock, and
// when the thread detaches (PyEval_SaveThread), every section this thread holds
// is suspended (its locks dropped); a suspended section takes its locks back when
// it is the innermost one again (on the inner section's End, or on reattach).
// That keeps the lock graph acyclic, so sections cannot deadlock each other.

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
/// Tag on a chain link: the record is a PyCriticalSection2.
const CS_TWO: usize = 1;
/// Flag in a record's own `_cs_prev`: its locks are currently dropped.
const CS_SUSPENDED: usize = 2;
const LINK_MASK: usize = !(CS_TWO | CS_SUSPENDED);
/// Tag on a lock key: an object address in the lock table (else a PyMutex address).
const KEY_OBJECT: usize = 1;

struct LockShard {
    held: Mutex<Vec<usize>>,
    freed: Condvar,
}
const NSHARDS: usize = 64;
static LOCK_TABLE: [LockShard; NSHARDS] =
    [const { LockShard { held: Mutex::new(Vec::new()), freed: Condvar::new() } }; NSHARDS];
fn shard(key: usize) -> &'static LockShard {
    &LOCK_TABLE[(key >> 4).wrapping_mul(0x9E37_79B9) % NSHARDS]
}

fn key_try_lock(key: usize) -> bool {
    if key & KEY_OBJECT == 0 {
        let m = unsafe { AtomicU8::from_ptr(key as *mut u8) };
        let v = m.load(Relaxed);
        return v & LOCKED == 0 && m.compare_exchange(v, v | LOCKED, Acquire, Relaxed).is_ok();
    }
    let mut held = shard(key).held.lock().unwrap();
    if held.contains(&key) {
        return false;
    }
    held.push(key);
    true
}
fn key_lock(key: usize) {
    if key & KEY_OBJECT == 0 {
        return mutex_lock(unsafe { AtomicU8::from_ptr(key as *mut u8) });
    }
    let s = shard(key);
    let mut held = s.held.lock().unwrap();
    while held.contains(&key) {
        held = s.freed.wait(held).unwrap();
    }
    held.push(key);
}
fn key_unlock(key: usize) {
    if key == 0 {
        return;
    }
    if key & KEY_OBJECT == 0 {
        return mutex_unlock(unsafe { AtomicU8::from_ptr(key as *mut u8) });
    }
    let s = shard(key);
    let mut held = s.held.lock().unwrap();
    if let Some(i) = held.iter().position(|&k| k == key) {
        held.swap_remove(i);
    }
    drop(held);
    s.freed.notify_all();
}
fn object_key(op: *mut PyObject) -> *mut u8 {
    (op as usize | KEY_OBJECT) as *mut u8
}

/// The two lock keys of the record a chain link points at (0 = none).
unsafe fn record_keys(link: usize) -> (usize, usize) {
    unsafe {
        let cs = (link & LINK_MASK) as *mut PyCriticalSection;
        let k2 = if link & CS_TWO != 0 { (*(cs as *mut PyCriticalSection2))._cs_mutex2 as usize } else { 0 };
        ((*cs)._cs_mutex as usize, k2)
    }
}
unsafe fn record(link: usize) -> *mut PyCriticalSection {
    (link & LINK_MASK) as *mut PyCriticalSection
}
unsafe fn chain_head(ts: *mut crate::threads::ThreadState) -> *mut usize {
    unsafe { &raw mut (*ts).c.critical_section }
}

/// Does an active (not suspended) section of this thread hold `key`?
unsafe fn held_by_me(ts: *mut crate::threads::ThreadState, key: usize) -> bool {
    unsafe {
        let mut link = *chain_head(ts);
        while link != 0 {
            let cs = record(link);
            if (*cs)._cs_prev & CS_SUSPENDED == 0 {
                let (a, b) = record_keys(link);
                if a == key || (b != 0 && b == key) {
                    return true;
                }
            }
            link = (*cs)._cs_prev & !CS_SUSPENDED;
        }
        false
    }
}

/// Drop the locks of every section of thread `ts` (they are re-taken lazily).
pub(crate) unsafe fn suspend_all(ts: *mut crate::threads::ThreadState) {
    unsafe {
        if ts.is_null() {
            return;
        }
        let mut link = *chain_head(ts);
        while link != 0 {
            let cs = record(link);
            if (*cs)._cs_prev & CS_SUSPENDED == 0 {
                let (a, b) = record_keys(link);
                key_unlock(b);
                key_unlock(a);
                (*cs)._cs_prev |= CS_SUSPENDED;
            }
            link = (*cs)._cs_prev & !CS_SUSPENDED;
        }
    }
}

/// Re-take the locks of the innermost section of `ts` if it was suspended.
pub(crate) unsafe fn resume_top(ts: *mut crate::threads::ThreadState) {
    unsafe {
        if ts.is_null() {
            return;
        }
        let link = *chain_head(ts);
        if link == 0 {
            return;
        }
        let cs = record(link);
        if (*cs)._cs_prev & CS_SUSPENDED == 0 {
            return;
        }
        let (a, b) = record_keys(link);
        // keys of a two-lock record are stored in address order
        if a != 0 {
            key_lock(a);
        }
        if b != 0 {
            key_lock(b);
        }
        (*cs)._cs_prev &= !CS_SUSPENDED;
    }
}

/// Acquire `keys` (in the given order, zeros skipped) for a new section, suspending
/// this thread's other sections first if any of them is contended.
unsafe fn acquire_for_new(ts: *mut crate::threads::ThreadState, keys: [usize; 2]) {
    unsafe {
        let mut got = [false; 2];
        let mut contended = false;
        for (i, &k) in keys.iter().enumerate() {
            if k == 0 {
                continue;
            }
            if key_try_lock(k) {
                got[i] = true;
            } else {
                contended = true;
                break;
            }
        }
        if !contended {
            return;
        }
        for i in (0..2).rev() {
            if got[i] {
                key_unlock(keys[i]);
            }
        }
        suspend_all(ts);
        for &k in &keys {
            if k != 0 {
                key_lock(k);
            }
        }
    }
}

unsafe fn begin_one(c: *mut PyCriticalSection, key: *mut u8) {
    unsafe {
        let ts = crate::threads::current_mut();
        if held_by_me(ts, key as usize) {
            (*c)._cs_mutex = std::ptr::null_mut();
            (*c)._cs_prev = 0;
            return;
        }
        acquire_for_new(ts, [key as usize, 0]);
        (*c)._cs_mutex = key;
        (*c)._cs_prev = *chain_head(ts);
        *chain_head(ts) = c as usize;
    }
}

unsafe fn end_record(c: *mut PyCriticalSection, two: bool) {
    unsafe {
        let ts = crate::threads::current_mut();
        let link = c as usize | if two { CS_TWO } else { 0 };
        if *chain_head(ts) != link {
            // not pushed (fully re-entrant) or ended out of order: nothing to release
            return;
        }
        if (*c)._cs_prev & CS_SUSPENDED == 0 {
            let (a, b) = record_keys(link);
            key_unlock(b);
            key_unlock(a);
        }
        *chain_head(ts) = (*c)._cs_prev & !CS_SUSPENDED;
        resume_top(ts);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCriticalSection_BeginMutex(c: *mut PyCriticalSection, m: *mut u8) {
    unsafe { begin_one(c, m) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCriticalSection_Begin(c: *mut PyCriticalSection, op: *mut PyObject) {
    unsafe { begin_one(c, object_key(op)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCriticalSection_End(c: *mut PyCriticalSection) {
    unsafe { end_record(c, false) }
}

unsafe fn begin_two(c: *mut PyCriticalSection2, k1: *mut u8, k2: *mut u8) {
    unsafe {
        let ts = crate::threads::current_mut();
        // the same object twice is one lock; the record is still a two-lock record (the
        // matching End pops it as such), with an empty second key
        let (lo, hi) = if k1 == k2 {
            (k1, std::ptr::null_mut())
        } else if (k1 as usize) < (k2 as usize) {
            (k1, k2)
        } else {
            (k2, k1)
        };
        let lo = if held_by_me(ts, lo as usize) { 0 } else { lo as usize };
        let hi = if held_by_me(ts, hi as usize) { 0 } else { hi as usize };
        if lo == 0 && hi == 0 {
            (*c)._cs_base._cs_mutex = std::ptr::null_mut();
            (*c)._cs_mutex2 = std::ptr::null_mut();
            (*c)._cs_base._cs_prev = 0;
            return;
        }
        acquire_for_new(ts, [lo, hi]);
        (*c)._cs_base._cs_mutex = lo as *mut u8;
        (*c)._cs_mutex2 = hi as *mut u8;
        (*c)._cs_base._cs_prev = *chain_head(ts);
        *chain_head(ts) = c as usize | CS_TWO;
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCriticalSection2_BeginMutex(c: *mut PyCriticalSection2, m1: *mut u8, m2: *mut u8) {
    unsafe { begin_two(c, m1, m2) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCriticalSection2_Begin(c: *mut PyCriticalSection2, a: *mut PyObject, b: *mut PyObject) {
    unsafe { begin_two(c, object_key(a), object_key(b)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCriticalSection2_End(c: *mut PyCriticalSection2) {
    unsafe { end_record(&mut (*c)._cs_base, true) }
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
    // (jit5) single-threaded fast paths stop before the second thread exists
    crate::threads::MULTI_THREADED.store(1, std::sync::atomic::Ordering::SeqCst);
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
