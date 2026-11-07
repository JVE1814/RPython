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
use rpy_compiler::bytecode::{self as bc, Op};

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

/// Rewrite the `LoadFast` instructions of internal `code` whose pushed reference can be
/// borrowed into `LoadFastBorrow` (see the module comment for the rule). Returns the
/// number rewritten.
///
/// One forward pass over the code with every load still in question tracked at once
/// (usually none or one or two): each candidate records the stack positions holding
/// copies of its value; it succeeds when the last copy is consumed and fails at a
/// block boundary, at a store to its local while a copy is live, under a handler that
/// would keep a copy, or at an instruction this model does not know. Stack depths come
/// from the same pass (forward jump targets record theirs; code reached only by a
/// backward jump, which compiled code does not have, is skipped).
pub fn mark(code: &mut [u32], handlers: &[bc::ExceptionEntry]) -> usize {
    let n = code.len();
    // jump targets and handler entries: basic-block starts
    let mut target = vec![0u64; n / 64 + 1];
    let set = |t: &mut [u64], i: usize| {
        if i < n {
            t[i / 64] |= 1 << (i % 64);
        }
    };
    let mut any_load = false;
    let mut ext = 0u32;
    for &w in code.iter() {
        let (op, a) = bc::decode(w);
        let arg = a | ext;
        if op == Op::ExtendedArg {
            ext = arg << 24;
            continue;
        }
        ext = 0;
        if op.is_jump() {
            set(&mut target, arg as usize);
        }
        any_load |= op == Op::LoadFast;
    }
    if !any_load {
        return 0;
    }
    for h in handlers {
        set(&mut target, h.target as usize);
    }
    let is_target = |i: usize| i < n && target[i / 64] & (1 << (i % 64)) != 0;
    // depth recorded for forward targets (-1: none yet)
    let mut entry = vec![-1i32; n + 1];
    for h in handlers {
        if (h.target as usize) < n {
            entry[h.target as usize] = h.depth as i32 + h.lasti as i32 + 1;
        }
    }
    let mut cands: Vec<Cand> = Vec::new();
    let mut count = 0;
    let mut cur: i32 = 0; // -1: unknown (unreachable by fall-through)
    let mut hix = 0usize; // handler entries are sorted and do not overlap
    let mut start = 0usize;
    let mut ext = 0u32;
    for pc in 0..n {
        let (op, a) = bc::decode(code[pc]);
        let arg = a | ext;
        if op == Op::ExtendedArg {
            ext = arg << 24;
            continue;
        }
        ext = 0;
        let first = std::mem::replace(&mut start, pc + 1);
        if (first..=pc).any(is_target) {
            cands.clear();
            let e = (first..=pc).map(|k| entry[k]).max().unwrap_or(-1);
            if cur < 0 {
                cur = e;
            }
        }
        if cur < 0 {
            continue;
        }
        // deepest stack level an exception handler keeps here
        while hix < handlers.len() && handlers[hix].end as usize <= pc {
            hix += 1;
        }
        let keep = match handlers.get(hix) {
            Some(h) if (h.start as usize) <= pc => h.depth as i32,
            _ => -1,
        };
        let io = io(op, arg);
        let mut k = 0;
        while k < cands.len() {
            match cands[k].step(&io, cur, keep) {
                Step::Live => k += 1,
                Step::Done => {
                    let p = cands[k].pc;
                    code[p] = (code[p] & !0xff) | Op::LoadFastBorrow as u32;
                    count += 1;
                    cands.swap_remove(k);
                }
                Step::Fail => {
                    cands.swap_remove(k);
                }
            }
        }
        if op == Op::LoadFast && cands.len() < 16 {
            cands.push(Cand { pc, local: arg, al: [cur, 0, 0, 0], na: 1 });
        }
        let (fall, jump) = super::jit::analysis::effect(op, arg);
        if op.is_jump() {
            let t = arg as usize;
            if t > pc && t <= n && entry[t] < 0 {
                entry[t] = cur + jump;
            }
        }
        if super::jit::analysis::is_terminator(op) {
            cur = -1;
            cands.clear();
        } else {
            cur += fall;
            if cur < 0 {
                cur = -1;
                cands.clear();
            }
        }
    }
    count
}

/// A `LoadFast` whose value may still be borrowed (`mark`).
struct Cand {
    pc: usize,
    local: u32,
    /// stack positions (0 = bottom of the value stack) holding copies of the value
    al: [i32; 4],
    na: usize,
}

enum Step {
    Live,
    Done,
    Fail,
}

/// How an instruction treats the stack, for `borrowable`.
enum Io {
    /// pops `0`, then pushes `1` new (owned) values
    Plain(u32, u32),
    /// like Plain, and the value popped from the top reappears as the topmost push
    /// (LoadMethod's `self`)
    Through(u32, u32),
    /// rebinds or deletes local `1` after popping `0`
    Store(u32, u32),
    /// pops `0` and ends the basic block (control leaves or the frame suspends)
    End(u32),
    Swap(u32),
    /// anything not modelled: give up
    Unknown,
}

fn io(op: Op, arg: u32) -> Io {
    use Io::*;
    use Op::*;
    match op {
        Nop | Resume | SetupAnnotations | DeleteDeref | DeleteName | DeleteGlobal => Plain(0, 0),
        PopTop | StoreDeref | StoreName | StoreGlobal | DeleteAttr | ListAppend | SetAdd | ListExtend | SetUpdate
        | DictUpdate | DictMerge => Plain(1, 0),
        PushNull | Copy | LoadConst | LoadFast | LoadFastBorrow | LoadFastCheck | LoadDeref | LoadLocals | LoadName
        | LoadGlobal | LoadBuildClass | LoadCommonConstant | GetAnext | ImportFrom | GetLen | MatchMapping
        | MatchSequence | MatchKeys => Plain(0, 1),
        LoadFromDictOrDeref | LoadFromDictOrGlobals | LoadAttr | UnaryNegative | UnaryPositive | UnaryInvert
        | UnaryNot | ToBool | GetIter | GetAiter | GetAwaitable | GetYieldFromIter | CallIntrinsic1 | MakeFunction
        | FormatSimple | ConvertValue => Plain(1, 1),
        BinaryOp | CompareOp | IsOp | ContainsOp | BinarySubscr | CallIntrinsic2 | SetFunctionAttribute
        | FormatWithSpec | ImportName | BuildTemplate => Plain(2, 1),
        StoreAttr | MapAdd | DeleteSubscr => Plain(2, 0),
        StoreSubscr => Plain(3, 0),
        StoreSlice => Plain(4, 0),
        BinarySlice | MatchClass => Plain(3, 1),
        BuildSlice => Plain(arg, 1),
        BuildTuple | BuildList | BuildSet | BuildString => Plain(arg, 1),
        BuildMap => Plain(2 * arg, 1),
        BuildInterpolation => Plain(2 + (arg & 1), 1),
        UnpackSequence => Plain(1, arg),
        UnpackEx => Plain(1, (arg & 0xff) + (arg >> 8) + 1),
        Call => Plain(arg + 2, 1),
        CallKw => Plain(arg + 3, 1),
        CallFunctionEx => Plain(4, 1),
        BeforeWith | BeforeAsyncWith => Plain(1, 3),
        LoadMethod | LoadSpecial => Through(1, 2),
        LoadSuperAttr => {
            if arg & 1 != 0 {
                Through(3, 2)
            } else {
                Plain(3, 1)
            }
        }
        StoreFast => Store(1, arg),
        DeleteFast => Store(0, arg),
        LoadFastAndClear => Store(0, arg),
        Op::Swap => Io::Swap(arg),
        Jump | ForIter | Send | JumpIfFalseOrPop | JumpIfTrueOrPop | RaiseVarargs | Reraise => End(0),
        PopJumpIfFalse | PopJumpIfTrue | PopJumpIfNone | PopJumpIfNotNone | ReturnValue | YieldValue => End(1),
        _ => Unknown,
    }
}

impl Cand {
    /// Apply one instruction (stack depth `cur` before it; `keep`: the deepest stack
    /// level an exception handler keeps here, -1 if none).
    fn step(&mut self, io: &Io, cur: i32, keep: i32) -> Step {
        let al = &mut self.al;
        // an exception raised here would leave the copy on the stack for the handler
        if al[..self.na].iter().any(|&p| p < keep) {
            return Step::Fail;
        }
        let (pops, pushes, through, store, end) = match *io {
            Io::Plain(p, q) => (p, q, false, None, false),
            Io::Through(p, q) => (p, q, true, None, false),
            Io::Store(p, l) => (p, 0, false, Some(l), false),
            Io::End(p) => (p, 0, false, None, true),
            Io::Swap(k) => {
                let (a, b) = (cur - 1, cur - k as i32);
                for x in al[..self.na].iter_mut() {
                    if *x == a {
                        *x = b;
                    } else if *x == b {
                        *x = a;
                    }
                }
                return Step::Live;
            }
            Io::Unknown => return Step::Fail,
        };
        let base = cur - pops as i32;
        if base < 0 {
            return Step::Fail;
        }
        let mut passed = false;
        let mut k = 0;
        while k < self.na {
            if al[k] >= base {
                if through && al[k] == cur - 1 {
                    passed = true;
                }
                al[k] = al[self.na - 1];
                self.na -= 1;
            } else {
                k += 1;
            }
        }
        if passed {
            if self.na == al.len() {
                return Step::Fail;
            }
            al[self.na] = base + pushes as i32 - 1;
            self.na += 1;
        }
        if self.na == 0 {
            return Step::Done;
        }
        if end || store == Some(self.local) {
            return Step::Fail;
        }
        Step::Live
    }
}

/// Values an instruction pops in the model of `mark` (None: not modelled).
pub fn pops(op: Op, arg: u32) -> Option<u32> {
    match io(op, arg) {
        Io::Plain(p, _) | Io::Through(p, _) | Io::Store(p, _) | Io::End(p) => Some(p),
        Io::Swap(_) => Some(0),
        Io::Unknown => None,
    }
}
