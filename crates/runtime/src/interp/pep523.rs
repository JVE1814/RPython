//! Frame-evaluation hooks (PEP 523, as `_testinternalcapi.set_eval_frame_*` exposes them).
//!
//! A hook observes Python frame entries (calls, generator creation and every generator
//! resumption) by appending the code name to a list. Two modes:
//! * `ALL` (specialisation off): every entry is observed. Compiled code and its direct
//!   frame-to-frame calls are switched off while the hook is installed, so each frame
//!   goes through `eval::eval_frame`.
//! * `OUTER` (specialisation allowed): only entries that do not happen inside a frame
//!   already run by the hook are observed (calls between Python frames under the hook
//!   stay on the fast paths).
//!
//! Cost while no hook is installed: one relaxed load per frame entry.

use crate::ffi::PyObject;
use crate::object::*;
use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering::*};

pub const NONE: u8 = 0;
pub const ALL: u8 = 1;
pub const OUTER: u8 = 2;

static MODE: AtomicU8 = AtomicU8::new(NONE);
/// The list receiving code names (a strong reference while installed).
static SINK: Mutex<usize> = Mutex::new(0);

thread_local! {
    /// How many frames run by the hook are active on this thread (OUTER mode).
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// Install `sink` (a list) in `mode`, or uninstall with `None`.
pub fn install(mode: u8, sink: Option<&PyObject>) {
    let new = sink.map_or(0, |s| s.to_ref().into_raw() as usize);
    let old = {
        let mut g = SINK.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::replace(&mut *g, new)
    };
    MODE.store(if new == 0 { NONE } else { mode }, SeqCst);
    // compiled code (including machine-code call paths) stands down while tracing is on
    crate::modules::sys_trace::recompute_tracing();
    unsafe { xdecref(old as *mut PyObject) };
}

/// Whether a hook is installed.
pub fn installed() -> bool {
    MODE.load(Relaxed) != NONE
}

/// Whether compiled code must stay off (every frame entry has to be observed).
#[inline(always)]
pub fn forces_interpreter() -> bool {
    MODE.load(Relaxed) == ALL
}

/// Whether the interpreter may specialise (false while an ALL hook is installed).
pub fn specialization_enabled() -> bool {
    MODE.load(Relaxed) != ALL
}

/// A frame running `code` is entered; returns whether `leave` must be called after it.
#[inline(always)]
pub fn enter(code: &PyObject) -> bool {
    let mode = MODE.load(Relaxed);
    if mode == NONE {
        return false;
    }
    enter_slow(mode, code)
}

#[cold]
#[inline(never)]
fn enter_slow(mode: u8, code: &PyObject) -> bool {
    let observe = mode == ALL || DEPTH.with(|d| d.get()) == 0;
    if observe {
        let sink = {
            let g = SINK.lock().unwrap_or_else(|e| e.into_inner());
            if *g == 0 { None } else { Some(unsafe { PyRef::from_borrowed(*g as *mut PyObject) }) }
        };
        if let Some(sink) = sink {
            let name = crate::types::code::code(code).co_name.clone();
            crate::types::list::append(&sink, name);
        }
    }
    DEPTH.with(|d| d.set(d.get() + 1));
    true
}

/// A frame entry that completes at once (a generator created without running its
/// prologue through `eval_frame`).
#[inline(always)]
pub fn note_entry(code: &PyObject) {
    if enter(code) {
        leave();
    }
}

#[inline]
pub fn leave() {
    DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
}
