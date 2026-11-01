//! Asynchronous exceptions (PyThreadState_SetAsyncExc): an exception is queued
//! on a thread state and raised by that thread at its next eval-breaker check.
use crate::errors::*;
use crate::ffi::PyObject;
use crate::threads::ThreadState;
use std::sync::atomic::Ordering::*;

/// Called from the eval breaker of the target thread.
pub fn raise_async_exc(ts: &ThreadState) -> PyResult<()> {
    ts.c.eval_breaker.fetch_and(!crate::threads::EVAL_ASYNC_EXC, AcqRel);
    let e = ts.async_exc.swap(std::ptr::null_mut(), AcqRel);
    if e.is_null() {
        return Ok(());
    }
    let e = unsafe { crate::object::PyRef::from_raw(e) };
    let inst = if crate::types::typeobject::is_type(&e) {
        crate::abstract_::call(&e, &[])?
    } else {
        e
    };
    set_exc(inst);
    Err(PyErr)
}

/// Queue `exc` (a type or instance; None clears) for the thread with pthread id
/// `thread_id`. Returns the number of thread states modified.
pub fn set_async_exc(thread_id: u64, exc: Option<&PyObject>) -> usize {
    let mut n = 0;
    for ts in crate::threads::live_threads() {
        if ts.c.thread_id as u64 != thread_id {
            continue;
        }
        let nv = exc.map_or(std::ptr::null_mut(), |e| e.to_ref().into_raw());
        let old = ts.async_exc.swap(nv, AcqRel);
        unsafe { crate::object::xdecref(old) };
        if !nv.is_null() {
            ts.c.eval_breaker.fetch_or(crate::threads::EVAL_ASYNC_EXC, Release);
        }
        n += 1;
    }
    n
}
