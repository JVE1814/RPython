//! pystate.h / ceval.h thread-state parts: thread states, the (emulated) GIL API
//! mapped to HotSpot-style thread modes (detached = InNative), interpreter state,
//! pending calls, PEP 788 guards/views.

use super::*;
use crate::threads::{self, ThreadMode, ThreadState};
use crate::types::dict;
use std::ffi::c_void;
use std::sync::Mutex;

/// The single interpreter: an opaque, zeroed block (C code only passes pointers around;
/// `tstate->interp` must be non-NULL and stable).
#[repr(C, align(64))]
struct Interp([u8; 4096]);
static mut INTERP: Interp = Interp([0; 4096]);
static INTERP_DICT: Mutex<usize> = Mutex::new(0);

pub fn interp_ptr() -> *mut c_void {
    &raw mut INTERP as *mut c_void
}

#[inline]
fn ts_ptr() -> *mut ThreadState {
    let ts = threads::current_mut();
    unsafe {
        if (*ts).c.interp.is_null() {
            (*ts).c.interp = interp_ptr();
        }
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
            (*old).mode.set(ThreadMode::InNative);
        }
        threads::set_current(new as *mut ThreadState);
        if !new.is_null() {
            (*(new as *mut ThreadState)).mode.set(ThreadMode::InVm);
        }
    }
    old as *mut c_void
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThreadState_New(_interp: *mut c_void) -> *mut c_void {
    let ts = threads::new_thread_state();
    unsafe {
        (*ts).c.interp = interp_ptr();
        (*ts).mode.set(ThreadMode::InNative);
    }
    ts as *mut c_void
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
        threads::ALL_THREADS.lock().unwrap().retain(|&t| t != ts as usize);
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
pub unsafe extern "C" fn PyThreadState_GetInterpreter(_ts: *mut c_void) -> *mut c_void {
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
    unsafe {
        let g = threads::ALL_THREADS.lock().unwrap();
        let mut n = 0;
        for &t in g.iter() {
            let ts = &mut *(t as *mut ThreadState);
            if ts.c.thread_id == id {
                let old = std::mem::replace(&mut ts.c.async_exc, if exc.is_null() { std::ptr::null_mut() } else { (*exc).to_ref().into_raw() });
                xdecref(old);
                if !exc.is_null() {
                    ts.c.eval_breaker.fetch_or(threads::EVAL_ASYNC_EXC, std::sync::atomic::Ordering::Release);
                }
                n += 1;
            }
        }
        n
    }
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
        let had = !threads::current_ptr().is_null();
        let ts = ts_ptr();
        if had && (*ts).mode.get() == ThreadMode::InVm {
            (*ts).c.gilstate_counter += 1;
            return 0;
        }
        (*ts).mode.set(ThreadMode::InVm);
        (*ts).c.gilstate_counter += 1;
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
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyGILState_GetThisThreadState() -> *mut c_void {
    threads::current_ptr() as *mut c_void
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyGILState_Check() -> c_int {
    unsafe {
        let ts = threads::current_ptr();
        (!ts.is_null() && (*ts).mode.get() == ThreadMode::InVm) as c_int
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_SaveThread() -> *mut c_void {
    unsafe {
        let ts = ts_ptr();
        (*ts).mode.set(ThreadMode::InNative);
        ts as *mut c_void
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_RestoreThread(ts: *mut c_void) {
    unsafe {
        if ts.is_null() {
            super::refcount::fatal("PyEval_RestoreThread: NULL thread state");
        }
        threads::set_current(ts as *mut ThreadState);
        (*(ts as *mut ThreadState)).mode.set(ThreadMode::InVm);
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
            (*(ts as *mut ThreadState)).mode.set(ThreadMode::InNative);
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_InitThreads() {}

// ---------------------------------------------------------------- interpreter state

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterState_Get() -> *mut c_void {
    interp_ptr()
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
pub unsafe extern "C" fn PyInterpreterState_ThreadHead(_i: *mut c_void) -> *mut c_void {
    threads::ALL_THREADS.lock().unwrap().first().map_or(std::ptr::null_mut(), |&t| t as *mut c_void)
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterState_GetID(_i: *mut c_void) -> i64 {
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterState_GetDict(_i: *mut c_void) -> *mut PyObject {
    let mut g = INTERP_DICT.lock().unwrap();
    if *g == 0 {
        *g = dict::new_dict().into_raw() as usize;
    }
    *g as *mut PyObject
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterState_New() -> *mut c_void {
    let _ = crate::err!(RuntimeError, "sub-interpreters are not supported");
    std::ptr::null_mut()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterState_Clear(_i: *mut c_void) {}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterState_Delete(_i: *mut c_void) {}
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

static GUARD_TOKEN: u8 = 0;
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterGuard_FromCurrent() -> *mut c_void {
    &GUARD_TOKEN as *const u8 as *mut c_void
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterGuard_FromView(_v: *mut c_void) -> *mut c_void {
    &GUARD_TOKEN as *const u8 as *mut c_void
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterGuard_Close(_g: *mut c_void) {}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterView_FromCurrent() -> *mut c_void {
    &GUARD_TOKEN as *const u8 as *mut c_void
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterView_FromMain() -> *mut c_void {
    &GUARD_TOKEN as *const u8 as *mut c_void
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInterpreterView_Close(_v: *mut c_void) {}
/// Token: 1 + PyGILState_STATE (never NULL on success).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThreadState_Ensure(_g: *mut c_void) -> *mut c_void {
    unsafe { (1 + PyGILState_Ensure() as usize) as *mut c_void }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThreadState_EnsureFromView(_v: *mut c_void) -> *mut c_void {
    unsafe { (1 + PyGILState_Ensure() as usize) as *mut c_void }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyThreadState_Release(tok: *mut c_void) {
    unsafe { PyGILState_Release(tok as usize as c_int - 1) }
}

// ---------------------------------------------------------------- pending calls

static PENDING: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_AddPendingCall(func: Option<unsafe extern "C" fn(*mut c_void) -> c_int>, arg: *mut c_void) -> c_int {
    let Some(f) = func else { return -1 };
    let mut g = PENDING.lock().unwrap();
    if g.len() >= 300 {
        return -1;
    }
    g.push((f as usize, arg as usize));
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_MakePendingCalls() -> c_int {
    loop {
        let item = {
            let mut g = PENDING.lock().unwrap();
            if g.is_empty() {
                return 0;
            }
            g.remove(0)
        };
        let f: unsafe extern "C" fn(*mut c_void) -> c_int = unsafe { std::mem::transmute(item.0) };
        if unsafe { f(item.1 as *mut c_void) } < 0 {
            return -1;
        }
    }
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
