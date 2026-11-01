//! Thread states. The struct starts with the public `PyThreadState` prefix (C ABI),
//! followed by runtime-private fields. Thread coordination follows HotSpot:
//! threads are `InVm` (running Python/runtime code, must poll safepoints),
//! `InNative` (running C extension code or blocking syscalls: counts as safe).

use crate::ffi::*;
use std::cell::Cell;
use std::ffi::{c_int, c_ulong, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering::*};

#[repr(C)]
pub struct ErrStackItem {
    pub exc_value: *mut PyObject,
    pub previous_item: *mut ErrStackItem,
}

#[repr(C)]
pub struct CThreadState {
    pub prev: *mut CThreadState,
    pub next: *mut CThreadState,
    pub interp: *mut c_void,
    pub eval_breaker: AtomicUsize,
    pub _status: u32,
    pub holds_gil: c_int,
    pub gil_requested: c_int,
    pub _whence: c_int,
    pub state: c_int,
    pub py_recursion_remaining: c_int,
    pub py_recursion_limit: c_int,
    pub recursion_headroom: c_int,
    pub tracing: c_int,
    pub what_event: c_int,
    pub current_frame: *mut c_void,
    pub base_frame: *mut c_void,
    pub last_profiled_frame: *mut c_void,
    pub last_profiled_frame_seq: usize,
    pub c_profilefunc: *mut c_void,
    pub c_tracefunc: *mut c_void,
    pub c_profileobj: *mut PyObject,
    pub c_traceobj: *mut PyObject,
    pub current_exception: *mut PyObject,
    pub exc_info: *mut ErrStackItem,
    pub dict: *mut PyObject,
    pub gilstate_counter: c_int,
    pub async_exc: *mut PyObject,
    pub thread_id: c_ulong,
    pub native_thread_id: c_ulong,
    pub delete_later: *mut PyObject,
    pub critical_section: usize,
    pub coroutine_origin_tracking_depth: c_int,
    pub async_gen_firstiter: *mut PyObject,
    pub async_gen_finalizer: *mut PyObject,
    pub context: *mut PyObject,
    pub context_ver: u64,
    pub id: u64,
    pub datastack_chunk: *mut c_void,
    pub datastack_top: *mut *mut PyObject,
    pub datastack_limit: *mut *mut PyObject,
    pub datastack_cached_chunk: *mut c_void,
    pub exc_state: ErrStackItem,
    /// Fields after this point are not accessed by extensions; keep generous padding
    /// so the C prefix size is never smaller than CPython's.
    pub _reserved: [usize; 32],
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ThreadMode {
    InVm = 0,
    InNative = 1,
    Blocked = 2,
}

#[repr(C)]
pub struct ThreadState {
    pub c: CThreadState,
    pub mode: Cell<ThreadMode>,
    /// Objects whose shared refcount went negative and need the owner to merge.
    pub merge_queue: Mutex<Vec<usize>>,
    /// Rust-side top interpreter frame (see interp::frame).
    pub frame: Cell<*mut crate::interp::frame::Frame>,
    pub recursion_depth: Cell<i32>,
    pub owner_tid: usize,
    pub reraise_flag: Cell<bool>,
    pub headroom: Cell<bool>,
    pub stack_low: Cell<usize>,
    /// Set when the OS thread has finished (its state may linger in ALL_THREADS).
    pub exited: std::sync::atomic::AtomicBool,
}

unsafe impl Sync for ThreadState {}

thread_local! {
    static CURRENT: Cell<*mut ThreadState> = const { Cell::new(std::ptr::null_mut()) };
}

pub static ALL_THREADS: Mutex<Vec<usize>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicUsize = AtomicUsize::new(1);

#[inline(always)]
pub fn current() -> &'static ThreadState {
    let p = CURRENT.with(|c| c.get());
    if p.is_null() {
        return attach_new();
    }
    unsafe { &*p }
}

#[inline(always)]
pub fn current_ptr() -> *mut ThreadState {
    CURRENT.with(|c| c.get())
}

#[cold]
fn attach_new() -> &'static ThreadState {
    let ts = new_thread_state();
    CURRENT.with(|c| c.set(ts));
    unsafe { &*ts }
}

pub fn new_thread_state() -> *mut ThreadState {
    unsafe {
        let mut c: CThreadState = std::mem::zeroed();
        c.py_recursion_limit = 1000;
        c.py_recursion_remaining = 1000;
        c.id = NEXT_ID.fetch_add(1, Relaxed) as u64;
        c.thread_id = libc::pthread_self() as c_ulong;
        c.native_thread_id = libc::gettid() as c_ulong;
        c.gilstate_counter = 1;
        let ts = Box::into_raw(Box::new(ThreadState {
            c,
            mode: Cell::new(ThreadMode::InVm),
            merge_queue: Mutex::new(Vec::new()),
            frame: Cell::new(std::ptr::null_mut()),
            recursion_depth: Cell::new(0),
            owner_tid: crate::object::thread_id(),
            reraise_flag: Cell::new(false),
            headroom: Cell::new(false),
            stack_low: Cell::new(compute_stack_low()),
            exited: std::sync::atomic::AtomicBool::new(false),
        }));
        (*ts).c.exc_info = &mut (*ts).c.exc_state;
        ALL_THREADS.lock().unwrap().push(ts as usize);
        ts
    }
}

pub fn set_current(ts: *mut ThreadState) {
    CURRENT.with(|c| c.set(ts));
}

/// Called from `decref_shared` when a non-owner drove the shared count negative.
pub unsafe fn queue_object_for_merge(op: *mut PyObject) {
    unsafe {
        let tid = (*op).ob_tid.load(Relaxed);
        let threads = ALL_THREADS.lock().unwrap();
        for &t in threads.iter() {
            let ts = &*(t as *const ThreadState);
            if ts.owner_tid == tid {
                ts.merge_queue.lock().unwrap().push(op as usize);
                ts.c.eval_breaker.fetch_or(EVAL_MERGE_REFCOUNTS, Release);
                return;
            }
        }
        drop(threads);
        // Owner is gone: merge right here.
        crate::object::explicit_merge(op);
    }
}

pub const EVAL_MERGE_REFCOUNTS: usize = 1 << 0;
pub const EVAL_SAFEPOINT: usize = 1 << 1;
pub const EVAL_SIGNALS: usize = 1 << 2;
pub const EVAL_ASYNC_EXC: usize = 1 << 3;
pub const EVAL_GC: usize = 1 << 4;

/// Process pending merges for the current thread.
pub fn process_merge_queue(ts: &ThreadState) {
    let items: Vec<usize> = std::mem::take(&mut *ts.merge_queue.lock().unwrap());
    ts.c.eval_breaker.fetch_and(!EVAL_MERGE_REFCOUNTS, AcqRel);
    for op in items {
        unsafe { crate::object::explicit_merge(op as *mut PyObject) };
    }
}

impl ThreadState {
    #[inline]
    pub fn recursion_headroom_active(&self) -> bool {
        self.headroom.get()
    }
    #[inline]
    pub fn set_recursion_headroom(&self, v: bool) {
        self.headroom.set(v)
    }
    /// Lowest usable native stack address (+ margin for raising RecursionError).
    #[inline]
    pub fn stack_soft_limit(&self) -> usize {
        self.stack_low.get() + 256 * 1024
    }
    #[inline]
    pub fn stack_hard_limit(&self) -> usize {
        self.stack_low.get() + 64 * 1024
    }
}

/// Determine the native stack bounds of the calling thread.
pub fn compute_stack_low() -> usize {
    unsafe {
        let mut attr: libc::pthread_attr_t = std::mem::zeroed();
        if libc::pthread_getattr_np(libc::pthread_self(), &mut attr) != 0 {
            return 0;
        }
        let mut addr: *mut libc::c_void = std::ptr::null_mut();
        let mut size: libc::size_t = 0;
        libc::pthread_attr_getstack(&attr, &mut addr, &mut size);
        libc::pthread_attr_destroy(&mut attr);
        // the main thread's reported size may be the rlimit; clamp to what is mapped
        let low = addr as usize;
        let here = &attr as *const _ as usize;
        if here.saturating_sub(low) > 1 << 30 {
            return here.saturating_sub(8 << 20);
        }
        low
    }
}

pub fn set_recursion_limit_all(n: i32) {
    let threads = ALL_THREADS.lock().unwrap();
    for &t in threads.iter() {
        let ts = unsafe { &mut *(t as *mut ThreadState) };
        ts.c.py_recursion_limit = n;
        ts.c.py_recursion_remaining = n;
    }
}

/// Raw pointer to the current thread state (attaching one if needed).
#[inline(always)]
pub fn current_mut() -> *mut ThreadState {
    let p = current_ptr();
    if p.is_null() {
        return current() as *const ThreadState as *mut ThreadState;
    }
    p
}

impl ThreadState {
    pub fn alive(&self) -> bool {
        !self.exited.load(std::sync::atomic::Ordering::Acquire)
    }
}

/// Run a potentially blocking system call with the thread marked `Blocked` so a
/// stop-the-world collection does not wait for it; parks at a safepoint on return
/// if a collection is in progress.
pub fn blocking<R>(f: impl FnOnce() -> R) -> R {
    let ts = current();
    let prev = ts.mode.get();
    ts.mode.set(ThreadMode::Blocked);
    let r = f();
    ts.mode.set(prev);
    if prev == ThreadMode::InVm && ts.c.eval_breaker.load(std::sync::atomic::Ordering::Acquire) & EVAL_SAFEPOINT != 0 {
        crate::gc::safepoint_poll(ts);
    }
    r
}
