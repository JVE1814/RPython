//! (refs1) Borrowed value-stack references.
//!
//! A frame's value stack normally holds owned references: pushing a local increfs it and
//! the consumer decrefs it. For a local that stays bound until the pushed copy has been
//! consumed, that pair is pure overhead (and with threads it writes the refcount line of
//! shared objects from every core). So a stack slot may instead hold a *borrowed*
//! reference, marked by bit 0 of the pointer (objects are at least 8-aligned):
//!
//! * only value-stack slots (`Frame::slots[nlocalsplus..stack_top]`) are ever tagged;
//!   locals, cells and everything outside a frame hold plain owned pointers;
//! * a tagged slot keeps nothing alive. Popping it does nothing; a consumer that keeps
//!   the value (stores it into a local, a container, an attribute, returns or yields
//!   it, moves it into a callee frame) converts it to an owned reference first (`own`);
//! * the producer is `LoadFastBorrow`, emitted by `mark` below (run on the internal
//!   code after translation) only where the copy cannot outlive its local: every copy
//!   of the value (including pass-through copies such as LoadMethod's `self`) is
//!   consumed inside the same basic block, before any instruction of the frame rebinds
//!   or deletes the local, before the block ends (jump, branch, return, raise, yield,
//!   or the next instruction being a jump target / handler entry) and while no
//!   exception handler would keep it on the stack;
//! * writes to a local from outside the frame's own code (the `f_locals` proxy,
//!   C API) do not release the old value at once while the frame is running: it is
//!   parked in the frame's keep-alive list (`keep_alive`) until the frame is released,
//!   because a borrowed copy may still be on the stack (also protects tier 2's
//!   register-borrowed values);
//! * a debugger jump (f_lineno) converts the stack entries it keeps to owned ones.
//!
//! Compiled code is entered only at block starts (function entry, loop headers,
//! handlers, generator resumption), where no borrowed entries exist; leaving it at any
//! point (deopt, errors) is fine since the interpreter handles tags everywhere.
//!
//! `RPYTHON_BORROW=0` disables the translation step (no tagged slot is ever created).

use crate::ffi::PyObject;
use crate::object::*;


pub const TAG: usize = 1;

#[inline(always)]
pub fn is_borrowed(p: *mut PyObject) -> bool {
    (p as usize) & TAG != 0
}

#[inline(always)]
pub fn untag(p: *mut PyObject) -> *mut PyObject {
    ((p as usize) & !TAG) as *mut PyObject
}

#[inline(always)]
pub fn tag(p: *mut PyObject) -> *mut PyObject {
    ((p as usize) | TAG) as *mut PyObject
}

/// A value-stack slot's content as an owned reference (NULL stays NULL).
#[inline(always)]
pub unsafe fn own(p: *mut PyObject) -> *mut PyObject {
    if is_borrowed(p) {
        let q = untag(p);
        unsafe { incref(q) };
        q
    } else {
        p
    }
}

/// Drop a value-stack slot's content (NULL allowed).
#[inline(always)]
pub unsafe fn release(p: *mut PyObject) {
    if !is_borrowed(p) {
        unsafe { xdecref(p) };
    }
}

/// Does any of the n entries at `p` hold a borrowed reference? (Call arguments: short
/// runs, so the loop is kept scalar; a vectorised one costs more for 2-5 entries.)
#[inline(always)]
pub unsafe fn any_borrowed(p: *const *mut PyObject, n: usize) -> bool {
    let mut acc = 0usize;
    let mut i = 0;
    while i < n {
        acc |= unsafe { std::ptr::read_volatile(p.add(i)) } as usize;
        i += 1;
    }
    acc & TAG != 0
}

/// Copy n entries with their tags removed. Call blocks are short: up to 8 entries are
/// copied by straight-line code (one jump), longer ones by a scalar loop.
#[inline(always)]
pub unsafe fn copy_untagged(dst: *mut *mut PyObject, src: *const *mut PyObject, n: usize) {
    macro_rules! cp {
        ($($i:expr),*) => {{ $( unsafe { *dst.add($i) = untag(*src.add($i)) }; )* }};
    }
    match n {
        0 => {}
        1 => cp!(0),
        2 => cp!(0, 1),
        3 => cp!(0, 1, 2),
        4 => cp!(0, 1, 2, 3),
        5 => cp!(0, 1, 2, 3, 4),
        6 => cp!(0, 1, 2, 3, 4, 5),
        7 => cp!(0, 1, 2, 3, 4, 5, 6),
        8 => cp!(0, 1, 2, 3, 4, 5, 6, 7),
        _ => {
            let mut i = 0;
            while i < n {
                unsafe { *dst.add(i) = untag(std::ptr::read_volatile(src.add(i))) };
                i += 1;
            }
        }
    }
}

/// A popped value-stack entry: dereferences to the object; dropping it releases the
/// reference only if it was owned. For consumers that only look at their operands.
pub struct SRef(*mut PyObject);

impl SRef {
    /// Safety: `p` is a non-NULL stack slot value whose ownership moves to the SRef.
    #[inline(always)]
    pub unsafe fn from_slot(p: *mut PyObject) -> SRef {
        SRef(p)
    }
    #[inline(always)]
    pub fn as_ptr(&self) -> *mut PyObject {
        untag(self.0)
    }
    /// Give the entry back as a stack slot value (ownership unchanged).
    #[inline(always)]
    pub fn into_slot(self) -> *mut PyObject {
        let p = self.0;
        std::mem::forget(self);
        p
    }
    /// (wheels3) The container operand of STORE_SUBSCR. CPython keeps a counted reference
    /// to it on the caller's stack while a class's `__setitem__` runs (measured with
    /// sys.getrefcount: a named object is seen 3 times there, a temporary twice), and
    /// pandas' chained-assignment check depends on that count. A borrowed entry is
    /// therefore owned when the container is an instance of a heap type (where Python
    /// code can run); builtin containers stay borrowed.
    #[inline(always)]
    pub fn owned_for_setitem(self) -> SRef {
        if is_borrowed(self.0) {
            let q = untag(self.0);
            if unsafe { (*(*q).type_ptr()).tp_flags } & crate::ffi::Py_TPFLAGS_HEAPTYPE != 0 {
                std::mem::forget(self);
                unsafe { incref(q) };
                return SRef(q);
            }
        }
        self
    }
    /// An owned reference to the object.
    #[inline(always)]
    pub fn into_ref(self) -> PyRef {
        let p = self.0;
        std::mem::forget(self);
        unsafe { PyRef::from_raw(own(p)) }
    }
}

impl std::ops::Deref for SRef {
    type Target = PyObject;
    #[inline(always)]
    fn deref(&self) -> &PyObject {
        unsafe { &*untag(self.0) }
    }
}

impl Drop for SRef {
    #[inline(always)]
    fn drop(&mut self) {
        if !is_borrowed(self.0) {
            unsafe { decref(self.0) };
        }
    }
}

/// Is the translation step on (`RPYTHON_BORROW`, default 1)?
pub fn enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering::Relaxed};
    static ON: AtomicU8 = AtomicU8::new(0);
    match ON.load(Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = !matches!(std::env::var("RPYTHON_BORROW").as_deref(), Ok("0"));
            ON.store(if on { 1 } else { 2 }, Relaxed);
            on
        }
    }
}

/// Convert every borrowed entry of the frame's value stack (`stack_top` must be
/// current) into an owned one.
pub fn own_stack(f: &mut super::frame::Frame) {
    let lo = f.nlocalsplus.min(f.slots.len());
    let hi = f.stack_top.min(f.slots.len());
    for i in lo..hi {
        let p = f.slots[i];
        if is_borrowed(p) {
            f.slots[i] = unsafe { own(p) };
        }
    }
}

/// A local of a running (or suspended) frame was rebound or deleted from outside its
/// code: `old` (owned, may be NULL) is released when the frame is, since borrowed copies
/// of it may still be on the frame's value stack.
pub fn keep_alive(f: &mut super::frame::Frame, old: *mut PyObject) {
    if old.is_null() {
        return;
    }
    if f.state == super::frame::FrameState::Executing || f.state == super::frame::FrameState::Suspended {
        if f.keep.is_null() {
            f.keep = Box::into_raw(Box::new(Vec::new()));
        }
        unsafe { (*f.keep).push(old) };
    } else {
        unsafe { decref(old) };
    }
}


// ------------------------------------------------------------------ translation step

pub use super::borrow_mark::{mark, pops};
