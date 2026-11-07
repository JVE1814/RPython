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
    pub mode: ModeCell,
    /// Objects whose shared refcount went negative and need the owner to merge.
    pub merge_queue: Mutex<Vec<usize>>,
    /// Rust-side top interpreter frame (see interp::frame).
    pub frame: Cell<*mut crate::interp::frame::Frame>,
    pub recursion_depth: Cell<i32>,
    pub owner_tid: usize,
    pub reraise_flag: Cell<bool>,
    /// (exception, traceback) addresses of the exception that last left a frame
    /// unhandled; the receiving frame must not chain __context__ again.
    pub propagated_exc: Cell<(usize, usize)>,
    pub headroom: Cell<bool>,
    pub stack_low: Cell<usize>,
    /// Native stack kept free below the soft limit (scaled to the thread's stack size).
    pub stack_margin: Cell<usize>,
    /// True while an OS thread is attached to this state.
    pub alive: std::sync::atomic::AtomicBool,
    /// Pending asynchronous exception (PyThreadState_SetAsyncExc), owned.
    pub async_exc: std::sync::atomic::AtomicPtr<PyObject>,
    /// Per-thread tracing state (sys.settrace / sys.setprofile), owned callables.
    pub trace_func: Cell<*mut PyObject>,
    pub profile_func: Cell<*mut PyObject>,
    /// Re-entrancy guard while a trace/profile function runs.
    pub tracing_depth: Cell<i32>,
    /// Set when the OS thread has finished (its state may linger in ALL_THREADS).
    pub exited: std::sync::atomic::AtomicBool,
    /// Value of the QSBR sequence this state saw at its last quiescent point (qsbr.rs).
    pub qsbr_seen: std::sync::atomic::AtomicU64,
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

/// Retires a thread state that was created implicitly (an OS thread the runtime
/// did not start touched the VM) when that OS thread exits. Without it the state
/// would stay registered, possibly in `InVm` mode, and every stop-the-world
/// would wait for a thread that no longer exists.
struct AutoAttached(Cell<*mut ThreadState>);
impl Drop for AutoAttached {
    fn drop(&mut self) {
        let p = self.0.get();
        if p.is_null() {
            return;
        }
        let ts = unsafe { &*p };
        if ts.exited.load(SeqCst) {
            return; // retired properly (thread_exit)
        }
        crate::sharded::thread_exit();
        ts.mode.0.store(ThreadMode::Blocked as u8, SeqCst);
        {
            let mut all = ALL_THREADS.lock().unwrap_or_else(|e| e.into_inner());
            ts.alive.store(false, SeqCst);
            ts.exited.store(true, SeqCst);
            all.retain(|&t| t != p as usize);
            crate::qsbr::note_threads(&all);
        }
        // its memory is not reused: objects may still sit in its merge queue
    }
}
thread_local! {
    static AUTO_ATTACHED: AutoAttached = const { AutoAttached(Cell::new(std::ptr::null_mut())) };
}

/// Retire `ts` when the calling OS thread exits, unless it was retired before
/// (states created for foreign threads by PyGILState_Ensure).
pub fn retire_at_thread_exit(ts: *mut ThreadState) {
    let _ = AUTO_ATTACHED.try_with(|g| g.0.set(ts));
}

#[cold]
fn attach_new() -> &'static ThreadState {
    let ts = new_thread_state();
    CURRENT.with(|c| c.set(ts));
    let _ = AUTO_ATTACHED.try_with(|g| g.0.set(ts));
    // new states start in a safe mode; entering the VM honours a stopped world
    unsafe { (*ts).mode.set(ThreadMode::InVm) };
    unsafe { &*ts }
}

pub fn new_thread_state() -> *mut ThreadState {
    if let Some(path) = std::env::var_os("RPY_ATTACH_DEBUG") {
        // debugging aid: append a backtrace of every thread-state creation to the file
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "new thread state (pid {}):\n{}", std::process::id(), std::backtrace::Backtrace::force_capture());
        }
    }
    unsafe {
        let mut c: CThreadState = std::mem::zeroed();
        c.py_recursion_limit = 1000;
        c.py_recursion_remaining = 1000;
        c.id = NEXT_ID.fetch_add(1, Relaxed) as u64;
        c.thread_id = libc::pthread_self() as c_ulong;
        c.native_thread_id = libc::gettid() as c_ulong;
        c.gilstate_counter = 1;
        let limit = RECURSION_LIMIT.load(Relaxed) as i32;
        c.py_recursion_limit = limit;
        c.py_recursion_remaining = limit;
        let st = ThreadState {
            c,
            mode: ModeCell(std::sync::atomic::AtomicU8::new(ThreadMode::Blocked as u8)),
            merge_queue: Mutex::new(Vec::new()),
            frame: Cell::new(std::ptr::null_mut()),
            recursion_depth: Cell::new(0),
            owner_tid: crate::object::thread_id(),
            reraise_flag: Cell::new(false),
            propagated_exc: Cell::new((0, 0)),
            headroom: Cell::new(false),
            stack_low: Cell::new(compute_stack_low()),
            stack_margin: Cell::new(compute_stack_margin()),
            alive: std::sync::atomic::AtomicBool::new(true),
            async_exc: std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()),
            trace_func: Cell::new(std::ptr::null_mut()),
            profile_func: Cell::new(std::ptr::null_mut()),
            tracing_depth: Cell::new(0),
            exited: std::sync::atomic::AtomicBool::new(false),
            qsbr_seen: std::sync::atomic::AtomicU64::new(crate::qsbr::initial_seen()),
        };
        // Reuse the memory of a dead thread state (pointers to thread states stay valid).
        let recycled = FREE_STATES.lock().unwrap().pop();
        let ts = match recycled {
            Some(p) => {
                let p = p as *mut ThreadState;
                std::ptr::drop_in_place(p);
                std::ptr::write(p, st);
                p
            }
            None => Box::into_raw(Box::new(st)),
        };
        (*ts).c.exc_info = &mut (*ts).c.exc_state;
        {
            let mut all = ALL_THREADS.lock().unwrap_or_else(|e| e.into_inner());
            // (jit6) the flag flipped here, not by a parent before starting the
            // thread (_thread, PyThread_start_new_thread): the other threads may be
            // inside a single-threaded fast path right now; the first attach of a
            // thread state handshakes with them before running Python code
            // (MT_HANDSHAKE, capi/pystate.rs `attach`)
            if !all.is_empty() && MULTI_THREADED.swap(1, SeqCst) == 0 {
                MT_HANDSHAKE.store(true, SeqCst);
            }
            all.push(ts as usize);
            if crate::scalestats::on() {
                crate::scalestats::MAX_THREADS.fetch_max(all.len() as u64, Relaxed);
            }
            crate::qsbr::note_threads(&all);
        };
        ts
    }
}

pub(crate) static FREE_STATES: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// 1 once a second thread state was ever created (never reset). Container fast
/// paths that read without the object lock (interpreter, JIT inline code) are
/// only taken while this is 0: with a single thread no writer can race them.
/// (A u64 so JIT code can test it with one qword compare.)
pub static MULTI_THREADED: AtomicUsize = AtomicUsize::new(0);
/// MULTI_THREADED was set by a thread registering itself (e.g. a foreign thread in
/// PyGILState_Ensure) rather than before the second thread started: the next thread
/// to attach waits until every other thread has passed a safepoint (single-threaded
/// fast paths never span one), so none of them still relies on the old value.
pub static MT_HANDSHAKE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[inline(always)]
pub fn multi_threaded() -> bool {
    MULTI_THREADED.load(Relaxed) != 0
}
static RECURSION_LIMIT: AtomicUsize = AtomicUsize::new(1000);
/// pthread id of the main Python thread (0 = not yet known).
pub static MAIN_THREAD_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static MAIN_THREAD_STATE: std::sync::atomic::AtomicPtr<ThreadState> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// Is the thread state attached to a running OS thread?
pub fn is_live(ts: &ThreadState) -> bool {
    ts.alive.load(Acquire)
}

/// Record the calling thread as the Python main thread.
pub fn set_main_thread() {
    let ts = current();
    MAIN_THREAD_ID.store(ts.c.thread_id as u64, Release);
    MAIN_THREAD_STATE.store(ts as *const ThreadState as *mut ThreadState, Release);
}

pub fn is_main_thread() -> bool {
    let m = MAIN_THREAD_ID.load(Acquire);
    m == 0 || m == unsafe { libc::pthread_self() } as u64
}

pub fn main_thread_state() -> Option<&'static ThreadState> {
    let p = MAIN_THREAD_STATE.load(Acquire);
    if p.is_null() { None } else { Some(unsafe { &*p }) }
}

/// Snapshot of the live thread states.
pub fn live_threads() -> Vec<&'static ThreadState> {
    ALL_THREADS.lock().unwrap().iter().map(|&t| unsafe { &*(t as *const ThreadState) }).filter(|t| is_live(t)).collect()
}

/// Detach and retire the calling thread's state (OS thread is about to exit).
pub fn thread_exit() {
    let p = current_ptr();
    if p.is_null() {
        return;
    }
    unsafe {
        let ts = &*p;
        process_merge_queue(ts);
        // release owned per-thread objects
        let e = std::mem::replace(&mut (*p).c.current_exception, std::ptr::null_mut());
        crate::object::xdecref(e);
        let a = ts.async_exc.swap(std::ptr::null_mut(), AcqRel);
        crate::object::xdecref(a);
        for cellp in [&ts.trace_func, &ts.profile_func] {
            let f = cellp.replace(std::ptr::null_mut());
            crate::object::xdecref(f);
        }
        for slot in [&raw mut (*p).c.async_gen_firstiter, &raw mut (*p).c.async_gen_finalizer, &raw mut (*p).c.context, &raw mut (*p).c.dict] {
            let o = std::mem::replace(&mut *slot, std::ptr::null_mut());
            crate::object::xdecref(o);
        }
        process_merge_queue(ts);
        crate::qsbr::thread_exit();
        crate::object::thread_exit_type_counts();
        crate::interp::frame::drain_pool();
        crate::gc::flush_thread_counts();
        // Nobody queues objects for this owner once `alive` is false (checked under
        // the ALL_THREADS lock). Drain the queue while this thread still counts as
        // running for stop-the-world purposes.
        {
            let _all = ALL_THREADS.lock().unwrap_or_else(|e| e.into_inner());
            ts.alive.store(false, SeqCst);
        }
        loop {
            let rest: Vec<usize> = std::mem::take(&mut *ts.merge_queue.lock().unwrap_or_else(|e| e.into_inner()));
            if rest.is_empty() {
                break;
            }
            for op in rest {
                crate::object::merge_queued(op as *mut PyObject);
            }
        }
        ts.c.eval_breaker.fetch_and(!EVAL_MERGE_REFCOUNTS, AcqRel);
        // pending sharded-count deltas (sharded.rs) are folded in while this thread
        // still counts as running
        crate::sharded::thread_exit();
        // from here on this thread touches no Python object
        ts.mode.set(ThreadMode::Blocked);
        ts.exited.store(true, SeqCst);
        {
            let mut all = ALL_THREADS.lock().unwrap_or_else(|e| e.into_inner());
            all.retain(|&t| t != p as usize);
            crate::qsbr::note_threads(&all);
        }
        // retired here: the exit-time guard must not touch the (reusable) memory
        let _ = AUTO_ATTACHED.try_with(|g| g.0.set(std::ptr::null_mut()));
        FREE_STATES.lock().unwrap_or_else(|e| e.into_inner()).push(p as usize);
        set_current(std::ptr::null_mut());
    }
}

pub fn set_current(ts: *mut ThreadState) {
    CURRENT.with(|c| c.set(ts));
}

/// Called from `decref_shared` when a non-owner dropped a reference that is part
/// of the owner's local count. The queue entry owns that reference.
pub unsafe fn queue_object_for_merge(op: *mut PyObject) {
    unsafe {
        if crate::scalestats::on() {
            crate::scalestats::MERGE_QUEUED.fetch_add(1, Relaxed);
        }
        let tid = (*op).ob_tid.load(Relaxed);
        let threads = ALL_THREADS.lock().unwrap_or_else(|e| e.into_inner());
        for &t in threads.iter() {
            let ts = &*(t as *const ThreadState);
            if ts.owner_tid == tid && ts.alive.load(SeqCst) {
                ts.merge_queue.lock().unwrap_or_else(|e| e.into_inner()).push(op as usize);
                ts.c.eval_breaker.fetch_or(EVAL_MERGE_REFCOUNTS, Release);
                return;
            }
        }
        drop(threads);
        // Owner is gone: merge right here.
        crate::object::merge_queued(op);
    }
}

pub const EVAL_MERGE_REFCOUNTS: usize = 1 << 0;
pub const EVAL_SAFEPOINT: usize = 1 << 1;
pub const EVAL_SIGNALS: usize = 1 << 2;
pub const EVAL_ASYNC_EXC: usize = 1 << 3;
pub const EVAL_GC: usize = 1 << 4;
/// Pending calls (Py_AddPendingCall) are queued for this thread (capi/pystate.rs).
pub const EVAL_PENDING: usize = 1 << 6;

/// Process pending merges for the current thread.
pub fn process_merge_queue(ts: &ThreadState) {
    ts.c.eval_breaker.fetch_and(!EVAL_MERGE_REFCOUNTS, AcqRel);
    let items: Vec<usize> = std::mem::take(&mut *ts.merge_queue.lock().unwrap_or_else(|e| e.into_inner()));
    for op in items {
        unsafe { crate::object::merge_queued(op as *mut PyObject) };
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
        self.stack_low.get() + self.stack_margin.get()
    }
    #[inline]
    pub fn stack_hard_limit(&self) -> usize {
        self.stack_low.get() + self.stack_margin.get() / 4
    }
}

/// Native stack reserved below the RecursionError limit: 256 KiB on ordinary
/// stacks, a quarter of the stack for small thread stacks (threading.stack_size()).
pub fn compute_stack_margin() -> usize {
    let low = compute_stack_low();
    let x = 0u8;
    let here = &x as *const u8 as usize;
    let size = here.saturating_sub(low);
    if size == 0 { 256 * 1024 } else { (size / 4).clamp(16 * 1024, 256 * 1024) }
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
    RECURSION_LIMIT.store(n as usize, Relaxed);
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

/// In a forked child only the calling thread survives: retire all other states.
/// (Called from a pthread_atfork child handler; must not block.)
pub fn after_fork_child() {
    let me = current_ptr() as usize;
    if let Ok(mut all) = ALL_THREADS.try_lock() {
        for &t in all.iter() {
            if t != me {
                let ts = unsafe { &*(t as *const ThreadState) };
                ts.alive.store(false, Relaxed);
                ts.exited.store(true, Relaxed);
            }
        }
        all.retain(|&t| t == me);
        crate::qsbr::note_threads(&all);
    }
    crate::sharded::after_fork_child();
    crate::modules::signal::after_fork_child();
}

/// Run `f` (a blocking native operation) with the thread in `Blocked` mode so that
/// stop-the-world operations do not wait for it; polls the safepoint on return.
#[inline]
pub fn blocking<R>(f: impl FnOnce() -> R) -> R {
    // A thread without an attached state (inside Py_BEGIN_ALLOW_THREADS, or a C thread
    // that never entered Python) is already safe for stop-the-world: do not attach a
    // new state here. (Attaching made a second, never-detached InVm state for a
    // thread blocking on a PyThread lock while detached; it stalled QSBR and every
    // stop-the-world for the rest of the process.)
    let p = current_ptr();
    if p.is_null() {
        return f();
    }
    let ts = unsafe { &*p };
    let prev = ts.mode.replace(ThreadMode::Blocked);
    let r = f();
    ts.mode.set(prev);
    if prev == ThreadMode::InVm && ts.c.eval_breaker.load(Acquire) & EVAL_SAFEPOINT != 0 {
        crate::gc::safepoint_poll(ts);
    }
    r
}

impl ThreadState {
    pub fn alive(&self) -> bool {
        !self.exited.load(std::sync::atomic::Ordering::Acquire)
    }
}


/// Thread mode readable from other threads (stop-the-world coordination).
pub struct ModeCell(pub std::sync::atomic::AtomicU8);
impl ModeCell {
    #[inline]
    pub fn get(&self) -> ThreadMode {
        match self.0.load(std::sync::atomic::Ordering::Acquire) {
            0 => ThreadMode::InVm,
            1 => ThreadMode::InNative,
            _ => ThreadMode::Blocked,
        }
    }
    /// Switching to `InVm` waits while another thread has stopped the world
    /// (see the handshake in gc.rs).
    #[inline]
    pub fn set(&self, m: ThreadMode) {
        if m == ThreadMode::InVm {
            let cur = self.get();
            crate::gc::enter_vm(self, cur);
        } else {
            self.0.store(m as u8, std::sync::atomic::Ordering::SeqCst)
        }
    }
    #[inline]
    pub fn replace(&self, m: ThreadMode) -> ThreadMode {
        let old = self.get();
        self.set(m);
        old
    }
}
