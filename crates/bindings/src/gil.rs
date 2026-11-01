//! Detaching the thread state around blocking C calls, and re-attaching inside
//! callbacks invoked by those C libraries.
//!
//! A callback that runs on a thread which detached itself with `save()` must
//! reuse that thread's own state (frames, JIT context); only callbacks on
//! threads unknown to the runtime get a fresh state through PyGILState_Ensure.

use crate::capi;
use std::cell::Cell;
use std::ffi::{c_int, c_void};

thread_local! {
    static SAVED: Cell<*mut c_void> = const { Cell::new(std::ptr::null_mut()) };
}

pub struct Saved {
    ts: *mut c_void,
    prev: *mut c_void,
}

/// Detach the current thread state (like Py_BEGIN_ALLOW_THREADS).
pub fn save() -> Saved {
    let ts = unsafe { capi::PyEval_SaveThread() };
    let prev = SAVED.with(|c| c.replace(ts));
    Saved { ts, prev }
}

/// Re-attach the state detached by `save()`.
pub fn restore(s: Saved) {
    SAVED.with(|c| c.set(s.prev));
    unsafe { capi::PyEval_RestoreThread(s.ts) };
}

pub enum Token {
    Restored(*mut c_void),
    GilState(c_int),
}

/// Attach a thread state inside a callback.
pub fn ensure() -> Token {
    let s = SAVED.with(|c| c.replace(std::ptr::null_mut()));
    if !s.is_null() {
        unsafe { capi::PyEval_RestoreThread(s) };
        Token::Restored(s)
    } else {
        Token::GilState(unsafe { capi::PyGILState_Ensure() })
    }
}

/// Undo `ensure()`.
pub fn release(t: Token) {
    match t {
        Token::Restored(s) => {
            let ts = unsafe { capi::PyEval_SaveThread() };
            SAVED.with(|c| c.set(if ts.is_null() { s } else { ts }));
        }
        Token::GilState(g) => unsafe { capi::PyGILState_Release(g) },
    }
}
