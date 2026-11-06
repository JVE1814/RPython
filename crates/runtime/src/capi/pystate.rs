//! pystate.h / ceval.h thread-state parts: thread states, the (emulated) GIL API
//! mapped to HotSpot-style thread modes (detached = InNative), interpreter states,
//! PEP 788 guards/views/tokens, pending calls.
//!
//! Model: the current (attached) thread state lives in `threads::CURRENT`; each OS
//! thread also remembers its "GILState" thread state. There is one real interpreter;
//! `Py_NewInterpreter` creates additional interpreter *identities* (own id, dict and
//! atexit list) that share the runtime — enough for the C-level thread-state API, not
//! for isolation (sub-interpreter isolation is not supported).

use super::*;
use crate::threads::{self, ThreadMode, ThreadState};
use crate::types::dict;
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::sync::Mutex;

pub struct Interp {
    pub id: i64,
    pub alive: std::sync::atomic::AtomicBool,
    pub dict: Mutex<usize>,
    pub atexit: Mutex<Vec<(usize, usize)>>,
}
static MAIN_INTERP: Interp = Interp {
    id: 0,
    alive: std::sync::atomic::AtomicBool::new(true),
    dict: Mutex::new(0),
    atexit: Mutex::new(Vec::new()),
};
static NEXT_INTERP_ID: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);

pub fn interp_ptr() -> *mut c_void {
    &MAIN_INTERP as *const Interp as *mut c_void
}
unsafe fn interp<'a>(p: *mut c_void) -> &'a Interp {
    unsafe { if p.is_null() { &MAIN_INTERP } else { &*(p as *const Interp) } }
}
unsafe fn interp_of(ts: *mut ThreadState) -> *mut c_void {
    unsafe {
        if (*ts).c.interp.is_null() {
            (*ts).c.interp = interp_ptr();
        }
        (*ts).c.interp
    }
}

thread_local! {
    /// This OS thread's GILState thread state.
    static GILSTATE: Cell<*mut ThreadState> = const { Cell::new(std::ptr::null_mut()) };
    /// Thread states created by PyThreadState_Ensure: (tstate, ensure depth, previous gilstate).
    static ENSURED: RefCell<Vec<(usize, usize, usize)>> = const { RefCell::new(Vec::new()) };
    /// Ensure depth of thread states that were not created by Ensure.
    static DEPTH: RefCell<Vec<(usize, usize)>> = const { RefCell::new(Vec::new()) };
}

fn gilstate() -> *mut ThreadState {
    let g = GILSTATE.with(|c| c.get());
    if g.is_null() {
        let cur = threads::current_ptr();
        if !cur.is_null() {
            GILSTATE.with(|c| c.set(cur));
        }
        return cur;
    }
    g
}
fn attach(ts: *mut ThreadState) {
    threads::set_current(ts);
    if !ts.is_null() {
        unsafe { (*ts).mode.set(ThreadMode::InVm) };
    }
}

#[inline]
fn ts_ptr() -> *mut ThreadState {
    let ts = threads::current_mut();
    unsafe {
        interp_of(ts);
    }
    if GILSTATE.with(|c| c.get()).is_null() {
        GILSTATE.with(|c| c.set(ts));
    }
    ts
}

fn new_tstate(interp: *mut c_void) -> *mut ThreadState {
    let ts = threads::new_thread_state();
    unsafe {
        (*ts).c.interp = interp;
        (*ts).mode.set(ThreadMode::InNative);
    }
    ts
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThreadState_Get() -> *mut c_void {
    ts_ptr() as *mut c_void
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThreadState_GetUnchecked() -> *mut c_void {
    threads::current_ptr() as *mut c_void
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThreadState_Swap(new: *mut c_void) -> *mut c_void {
    let old = threads::current_ptr();
    unsafe {
        if !old.is_null() {
            gilstate();
            (*old).mode.set(ThreadMode::InNative);
        }
    }
    attach(new as *mut ThreadState);
    old as *mut c_void
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThreadState_New(interp: *mut c_void) -> *mut c_void {
    new_tstate(if interp.is_null() { interp_ptr() } else { interp }) as *mut c_void
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThreadState_Clear(ts: *mut c_void) {
    unsafe {
        let t = &mut *(ts as *mut ThreadState);
        for p in [&mut t.c.dict, &mut t.c.async_exc, &mut t.c.current_exception, &mut t.c.context] {
            let o = std::mem::replace(p, std::ptr::null_mut());
            xdecref(o);
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThreadState_Delete(ts: *mut c_void) {
    unsafe {
        PyThreadState_Clear(ts);
        {
            let mut all = threads::ALL_THREADS.lock().unwrap();
            all.retain(|&t| t != ts as usize);
            crate::qsbr::note_threads(all.len());
        }
        // the memory is intentionally not freed: other threads may still hold pointers
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThreadState_DeleteCurrent() {
    unsafe {
        let ts = threads::current_ptr();
        if !ts.is_null() {
            threads::set_current(std::ptr::null_mut());
            PyThreadState_Delete(ts as *mut c_void);
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThreadState_GetDict() -> *mut PyObject {
    unsafe {
        let ts = threads::current_ptr();
        if ts.is_null() {
            return std::ptr::null_mut();
        }
        if (*ts).c.dict.is_null() {
            (*ts).c.dict = dict::new_dict().into_raw();
        }
        (*ts).c.dict
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThreadState_GetFrame(ts: *mut c_void) -> *mut PyObject {
    unsafe {
        let t = &*(ts as *mut ThreadState);
        let f = t.frame.get();
        if f.is_null() {
            return std::ptr::null_mut();
        }
        let fo = crate::interp::eval::frame_object(&mut *f);
        (*fo).to_ref().into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThreadState_GetID(ts: *mut c_void) -> u64 {
    unsafe { (*(ts as *mut ThreadState)).c.id }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThreadState_GetInterpreter(ts: *mut c_void) -> *mut c_void {
    if !ts.is_null() {
        return unsafe { interp_of(ts as *mut ThreadState) };
    }
    interp_ptr()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThreadState_Next(ts: *mut c_void) -> *mut c_void {
    let g = threads::ALL_THREADS.lock().unwrap();
    match g.iter().position(|&t| t == ts as usize) {
        Some(i) if i + 1 < g.len() => g[i + 1] as *mut c_void,
        _ => std::ptr::null_mut(),
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThreadState_SetAsyncExc(id: std::ffi::c_ulong, exc: *mut PyObject) -> c_int {
    // the pending exception lives in ThreadState::async_exc, where the eval
    // breaker looks for it
    let exc = if exc.is_null() { None } else { Some(unsafe { &*exc }) };
    crate::modules::thread::set_async_exc(id as u64, exc) as c_int
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThreadState_EnterTracing(ts: *mut c_void) {
    unsafe { (*(ts as *mut ThreadState)).c.tracing += 1 }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThreadState_LeaveTracing(ts: *mut c_void) {
    unsafe { (*(ts as *mut ThreadState)).c.tracing -= 1 }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_ThreadState_SetStackProtection(_ts: *mut c_void, _base: *mut c_void, _size: usize) -> c_int {
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_ThreadState_ResetStackProtection(_ts: *mut c_void) {}


// ---------------------------------------------------------------- GIL API (no GIL)

/// PyGILState_LOCKED = 0, PyGILState_UNLOCKED = 1
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyGILState_Ensure() -> c_int {
    unsafe {
        let cur = threads::current_ptr();
        if !cur.is_null() {
            (*cur).c.gilstate_counter += 1;
            return 0;
        }
        let mut g = gilstate();
        if g.is_null() {
            g = new_tstate(interp_ptr());
            GILSTATE.with(|c| c.set(g));
        }
        attach(g);
        (*g).c.gilstate_counter += 1;
        1
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyGILState_Release(state: c_int) {
    unsafe {
        let ts = threads::current_ptr();
        if ts.is_null() {
            return;
        }
        (*ts).c.gilstate_counter -= 1;
        if state == 1 {
            (*ts).mode.set(ThreadMode::InNative);
            threads::set_current(std::ptr::null_mut());
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyGILState_GetThisThreadState() -> *mut c_void {
    gilstate() as *mut c_void
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyGILState_Check() -> c_int {
    let ts = threads::current_ptr();
    (!ts.is_null() && ts == gilstate()) as c_int
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_SaveThread() -> *mut c_void {
    unsafe {
        let ts = ts_ptr();
        super::pythread::suspend_all(ts);
        (*ts).mode.set(ThreadMode::InNative);
        threads::set_current(std::ptr::null_mut());
        ts as *mut c_void
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_RestoreThread(ts: *mut c_void) {
    unsafe {
        if ts.is_null() {
            super::refcount::fatal("PyEval_RestoreThread: NULL thread state");
        }
        // re-take suspended critical sections while still detached (may block)
        super::pythread::resume_top(ts as *mut ThreadState);
        attach(ts as *mut ThreadState);
        // safepoint/handshake poll on re-entering the VM
        crate::gc::safepoint_poll(&*(ts as *mut ThreadState));
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_AcquireThread(ts: *mut c_void) {
    unsafe { PyEval_RestoreThread(ts) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_ReleaseThread(ts: *mut c_void) {
    unsafe {
        if !ts.is_null() {
            super::pythread::suspend_all(ts as *mut ThreadState);
            (*(ts as *mut ThreadState)).mode.set(ThreadMode::InNative);
        }
        threads::set_current(std::ptr::null_mut());
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_InitThreads() {}

// ---------------------------------------------------------------- interpreter state

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterState_Get() -> *mut c_void {
    unsafe { interp_of(ts_ptr()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterState_Main() -> *mut c_void {
    interp_ptr()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterState_Head() -> *mut c_void {
    interp_ptr()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterState_Next(_i: *mut c_void) -> *mut c_void {
    std::ptr::null_mut()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterState_ThreadHead(i: *mut c_void) -> *mut c_void {
    let g = threads::ALL_THREADS.lock().unwrap();
    g.iter()
        .find(|&&t| unsafe { interp_of(t as *mut ThreadState) } == if i.is_null() { interp_ptr() } else { i })
        .map_or(std::ptr::null_mut(), |&t| t as *mut c_void)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterState_GetID(i: *mut c_void) -> i64 {
    unsafe { interp(i).id }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterState_GetDict(i: *mut c_void) -> *mut PyObject {
    unsafe {
        let mut g = interp(i).dict.lock().unwrap();
        if *g == 0 {
            *g = dict::new_dict().into_raw() as usize;
        }
        *g as *mut PyObject
    }
}
fn new_interp() -> *mut c_void {
    let i = Box::leak(Box::new(Interp {
        id: NEXT_INTERP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        alive: std::sync::atomic::AtomicBool::new(true),
        dict: Mutex::new(0),
        atexit: Mutex::new(Vec::new()),
    }));
    i as *mut Interp as *mut c_void
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterState_New() -> *mut c_void {
    new_interp()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterState_Clear(_i: *mut c_void) {}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterState_Delete(i: *mut c_void) {
    unsafe {
        if !i.is_null() && i != interp_ptr() {
            interp(i).alive.store(false, std::sync::atomic::Ordering::Release);
        }
    }
}
/// Run (and clear) the PyUnstable_AtExit callbacks of an interpreter.
pub fn run_interp_atexit(i: *mut c_void) {
    let v: Vec<(usize, usize)> = std::mem::take(&mut *unsafe { interp(i) }.atexit.lock().unwrap());
    for (f, d) in v.into_iter().rev() {
        let f: unsafe extern "C" fn(*mut c_void) = unsafe { std::mem::transmute(f) };
        unsafe { f(d as *mut c_void) };
    }
}
pub fn add_interp_atexit(i: *mut c_void, f: usize, data: usize) {
    unsafe { interp(i) }.atexit.lock().unwrap().push((f, data));
}
/// Py_NewInterpreter: a new interpreter identity with an attached thread state.
pub fn new_interpreter() -> *mut c_void {
    let i = new_interp();
    let ts = new_tstate(i);
    attach(ts);
    GILSTATE.with(|c| c.set(ts));
    ts as *mut c_void
}
pub unsafe fn end_interpreter(ts: *mut c_void) {
    unsafe {
        let ts = ts as *mut ThreadState;
        let i = interp_of(ts);
        run_interp_atexit(i);
        interp(i).alive.store(false, std::sync::atomic::Ordering::Release);
        if threads::current_ptr() == ts {
            threads::set_current(std::ptr::null_mut());
        }
        if GILSTATE.with(|c| c.get()) == ts {
            GILSTATE.with(|c| c.set(std::ptr::null_mut()));
        }
        PyThreadState_Delete(ts as *mut c_void);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyInterpreterState_RequiresIDRef(_i: *mut c_void) -> c_int {
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyInterpreterState_RequireIDRef(_i: *mut c_void, _req: c_int) {}
static EVAL_FRAME_FUNC: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyInterpreterState_GetEvalFrameFunc(_i: *mut c_void) -> *mut c_void {
    let f = EVAL_FRAME_FUNC.load(std::sync::atomic::Ordering::Relaxed);
    if f == 0 { super::ceval::_PyEval_EvalFrameDefault as *mut c_void } else { f as *mut c_void }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyInterpreterState_SetEvalFrameFunc(_i: *mut c_void, f: *mut c_void) {
    EVAL_FRAME_FUNC.store(f as usize, std::sync::atomic::Ordering::Relaxed);
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyInterpreterState_SetEvalFrameAllowSpecialization(_i: *mut c_void, _allow: c_int) {}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyInterpreterState_IsSpecializationEnabled(_i: *mut c_void) -> c_int {
    0
}

// ---------------------------------------------------------------- PEP 788

/// Guards and views are interpreter pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterGuard_FromCurrent() -> *mut c_void {
    unsafe { PyInterpreterState_Get() }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterGuard_FromView(v: *mut c_void) -> *mut c_void {
    unsafe {
        if interp(v).alive.load(std::sync::atomic::Ordering::Acquire) { v } else { std::ptr::null_mut() }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterGuard_Close(_g: *mut c_void) {}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterView_FromCurrent() -> *mut c_void {
    unsafe { PyInterpreterState_Get() }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterView_FromMain() -> *mut c_void {
    interp_ptr()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterView_Close(_v: *mut c_void) {}

static DETACHED_TOKEN: u8 = 0;

fn depth_inc(ts: *mut ThreadState) {
    let created = ENSURED.with(|e| {
        let mut e = e.borrow_mut();
        if let Some(x) = e.iter_mut().find(|x| x.0 == ts as usize) {
            x.1 += 1;
            true
        } else {
            false
        }
    });
    if !created {
        DEPTH.with(|d| {
            let mut d = d.borrow_mut();
            match d.iter_mut().find(|x| x.0 == ts as usize) {
                Some(x) => x.1 += 1,
                None => d.push((ts as usize, 1)),
            }
        });
    }
}
unsafe fn ensure(i: *mut c_void) -> *mut c_void {
    unsafe {
        let i = if i.is_null() { interp_ptr() } else { i };
        let cur = threads::current_ptr();
        if !cur.is_null() && interp_of(cur) == i {
            depth_inc(cur);
            return cur as *mut c_void;
        }
        let g = gilstate();
        let ts = if !g.is_null() && g != cur && interp_of(g) == i {
            depth_inc(g);
            g
        } else {
            let t = new_tstate(i);
            let prev = GILSTATE.with(|c| c.replace(t));
            ENSURED.with(|e| e.borrow_mut().push((t as usize, 1, prev as usize)));
            t
        };
        attach(ts);
        if cur.is_null() { &DETACHED_TOKEN as *const u8 as *mut c_void } else { cur as *mut c_void }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThreadState_Ensure(g: *mut c_void) -> *mut c_void {
    unsafe { ensure(g) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThreadState_EnsureFromView(v: *mut c_void) -> *mut c_void {
    unsafe {
        if !interp(v).alive.load(std::sync::atomic::Ordering::Acquire) {
            return std::ptr::null_mut();
        }
        ensure(v)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThreadState_Release(tok: *mut c_void) {
    unsafe {
        let ts = threads::current_ptr();
        if ts.is_null() {
            return;
        }
        // decrement the ensure depth of the current thread state
        let created = ENSURED.with(|e| {
            let mut e = e.borrow_mut();
            if let Some(pos) = e.iter().position(|x| x.0 == ts as usize) {
                e[pos].1 -= 1;
                if e[pos].1 == 0 {
                    let x = e.remove(pos);
                    return Some(x.2);
                }
                return Some(usize::MAX);
            }
            None
        });
        if created.is_none() {
            DEPTH.with(|d| {
                let mut d = d.borrow_mut();
                if let Some(pos) = d.iter().position(|x| x.0 == ts as usize) {
                    d[pos].1 -= 1;
                    if d[pos].1 == 0 {
                        d.remove(pos);
                    }
                }
            });
        }
        if tok == ts as *mut c_void {
            return; // nested fast path
        }
        let prev = if tok == &DETACHED_TOKEN as *const u8 as *mut c_void { std::ptr::null_mut() } else { tok as *mut ThreadState };
        if let Some(prev_gil) = created {
            if prev_gil != usize::MAX {
                // the thread state was created by Ensure: destroy it
                if GILSTATE.with(|c| c.get()) == ts {
                    GILSTATE.with(|c| c.set(prev_gil as *mut ThreadState));
                }
                (*ts).mode.set(ThreadMode::InNative);
                threads::set_current(std::ptr::null_mut());
                PyThreadState_Delete(ts as *mut c_void);
            }
        }
        (*ts).mode.set(ThreadMode::InNative);
        attach(prev);
    }
}
// ---------------------------------------------------------------- pending calls

// Two queues as in CPython: calls for the main thread (Py_AddPendingCall, at most 32 queued)
// and calls any thread of the interpreter may run (at most 300). Queuing sets EVAL_PENDING
// in the eval breaker of the thread(s) that may run them; the interpreter then calls
// `run_pending_calls` between instructions.
static PENDING_MAIN: Mutex<std::collections::VecDeque<(usize, usize)>> = Mutex::new(std::collections::VecDeque::new());
static PENDING_ANY: Mutex<std::collections::VecDeque<(usize, usize)>> = Mutex::new(std::collections::VecDeque::new());
const MAX_PENDING_MAIN: usize = 32;
const MAX_PENDING_ANY: usize = 300;

fn request_pending(ts: *const ThreadState) {
    if !ts.is_null() {
        unsafe { (*ts).c.eval_breaker.fetch_or(threads::EVAL_PENDING, std::sync::atomic::Ordering::Release) };
    }
}
/// Queue `f(arg)`; `main_only`: only the main thread runs it. -1 if the queue is full.
pub(crate) fn add_pending_call(f: unsafe extern "C" fn(*mut c_void) -> c_int, arg: *mut c_void, main_only: bool) -> c_int {
    let (q, max) = if main_only { (&PENDING_MAIN, MAX_PENDING_MAIN) } else { (&PENDING_ANY, MAX_PENDING_ANY) };
    {
        let mut g = q.lock().unwrap();
        if g.len() >= max {
            return -1;
        }
        g.push_back((f as usize, arg as usize));
    }
    request_pending(threads::MAIN_THREAD_STATE.load(std::sync::atomic::Ordering::Acquire));
    if !main_only {
        // any thread may serve it: wake them all (the first to look takes it)
        for &t in threads::ALL_THREADS.lock().unwrap().iter() {
            request_pending(t as *const ThreadState);
        }
    }
    0
}
/// Run queued calls this thread may run (eval breaker, Py_MakePendingCalls).
pub(crate) fn run_pending_calls() -> PyResult<()> {
    let ts = threads::current_ptr();
    if !ts.is_null() {
        unsafe { (*ts).c.eval_breaker.fetch_and(!threads::EVAL_PENDING, std::sync::atomic::Ordering::AcqRel) };
    }
    let main = threads::is_main_thread();
    loop {
        let item = {
            let a = if main { PENDING_MAIN.lock().unwrap().pop_front() } else { None };
            match a {
                Some(x) => Some(x),
                None => PENDING_ANY.lock().unwrap().pop_front(),
            }
        };
        let Some((f, arg)) = item else { return Ok(()) };
        let f: unsafe extern "C" fn(*mut c_void) -> c_int = unsafe { std::mem::transmute(f) };
        if unsafe { f(arg as *mut c_void) } < 0 {
            // leave the rest for the next check
            if !PENDING_ANY.lock().unwrap().is_empty() || (main && !PENDING_MAIN.lock().unwrap().is_empty()) {
                request_pending(ts);
            }
            if !err_occurred() {
                let _ = system_error("pending call failed without setting an exception");
            }
            return Err(PyErr);
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_AddPendingCall(func: Option<unsafe extern "C" fn(*mut c_void) -> c_int>, arg: *mut c_void) -> c_int {
    let Some(f) = func else { return -1 };
    add_pending_call(f, arg, true)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_MakePendingCalls() -> c_int {
    if !threads::is_main_thread() {
        return 0;
    }
    ret_i(run_pending_calls())
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyThread_CurrentFrames() -> *mut PyObject {
    unsafe {
        ret((|| -> PyResult {
            let s = crate::modules::sys::sys_module();
            let f = crate::abstract_::getattr_str(&s, "_current_frames")?;
            crate::abstract_::call(&f, &[])
        })())
    }
}

// ---------------------------------------------------------------- trashcan (no-ops: no deep dealloc recursion limit)

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyTrash_thread_deposit_object(_ts: *mut c_void, o: *mut PyObject) {
    unsafe { crate::object::dealloc(o) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyTrash_thread_destroy_chain(_ts: *mut c_void) {}

// ---------------------------------------------------------------- ref tracer

static REF_TRACER: Mutex<(usize, usize)> = Mutex::new((0, 0));
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyRefTracer_SetTracer(f: *mut c_void, data: *mut c_void) -> c_int {
    *REF_TRACER.lock().unwrap() = (f as usize, data as usize);
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyRefTracer_GetTracer(data: *mut *mut c_void) -> *mut c_void {
    let g = REF_TRACER.lock().unwrap();
    unsafe {
        if !data.is_null() {
            *data = g.1 as *mut c_void;
        }
    }
    g.0 as *mut c_void
}
