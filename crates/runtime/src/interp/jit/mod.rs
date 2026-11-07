//! JIT tiers (design notes: notes/jit.md).
//!
//! Tier 0 is the interpreter (eval.rs) with inline caches and profiles (cache.rs,
//! specialize.rs). Tier 1 (tier1.rs) is a baseline template JIT emitting x86-64 code
//! per bytecode that shares the interpreter's frame layout, so control can move
//! between the tiers at instruction boundaries in both directions:
//! * tier 0 -> tier 1 at function entry, loop headers (OSR at backedges), exception
//!   handlers and generator resume points;
//! * tier 1 -> tier 0 ("deopt") at any instruction the JIT does not handle, and for
//!   exception unwinding.
//!
//! Tiering policy (HotSpot-like counters in CodeData): a code object is compiled when
//! its invocation counter reaches RPYTHON_JIT_THRESHOLD (default 100) or its backedge
//! counter reaches 10x that. Compilation happens synchronously on the thread that
//! trips the counter, under the per-code lock `CodeCaches::tier_state`
//! (INTERP -> COMPILING by CAS); other threads keep interpreting. The machine code is
//! immutable once installed and is published with a release store of
//! `CodeCaches::jit` (readers use acquire), so no GIL is needed.
//!
//! Environment: RPYTHON_JIT=0 disables the JIT (pure tier 0, the reference semantics
//! for differential testing); RPYTHON_JIT_THRESHOLD=n sets the hotness threshold
//! (0 = compile every code object on first call); RPYTHON_JIT_DEBUG=1 reports
//! compilations and failures on stderr.

pub mod analysis;
pub mod callpy;
pub mod helpers;
pub mod mem;
pub mod opt;
pub mod tier1;
pub mod x64;

use crate::errors::PyResult;
use crate::ffi::PyObject;
use crate::interp::cache::tier;
use crate::interp::frame::Frame;
use crate::object::*;
use crate::threads::ThreadState;
use crate::types::code::CodeData;
use std::sync::atomic::{AtomicU32, Ordering::*};

/// Machine code installed for a code object.
pub struct Compiled {
    pub addr: usize,
    pub len: usize,
    /// native offset per code index (u32::MAX: not an entry point)
    pub entries: Vec<u32>,
}

type EntryFn = unsafe extern "C" fn(*mut Frame, *mut *mut PyObject, *const ThreadState, usize, *mut *mut PyObject) -> u32;

/// Free compiled code of a dying code object (no frame can be executing it: frames
/// own a reference to their code object).
pub fn release_compiled(p: *mut Compiled) {
    unsafe {
        let c = Box::from_raw(p);
        mem::free(c.addr, c.len);
    }
}

// ------------------------------------------------------------------ configuration

const UNSET: u32 = u32::MAX;
const GEN_FLAGS: u32 = rpy_compiler::bytecode::co::GENERATOR
    | rpy_compiler::bytecode::co::COROUTINE
    | rpy_compiler::bytecode::co::ASYNC_GENERATOR
    | rpy_compiler::bytecode::co::ITERABLE_COROUTINE;
static ENABLED: AtomicU32 = AtomicU32::new(UNSET);
static THRESHOLD: AtomicU32 = AtomicU32::new(100);
static DEBUG: AtomicU32 = AtomicU32::new(0);

#[cold]
fn init_config() -> bool {
    let en = std::env::var("RPYTHON_JIT").map(|v| v != "0").unwrap_or(true);
    if let Ok(t) = std::env::var("RPYTHON_JIT_THRESHOLD") {
        if let Ok(n) = t.parse::<u32>() {
            THRESHOLD.store(n, Relaxed);
        }
    }
    if let Ok(v) = std::env::var("RPYTHON_JIT_DEBUG") {
        DEBUG.store(v.parse::<u32>().unwrap_or(1), Relaxed);
    }
    ENABLED.store(en as u32, Relaxed);
    en
}

#[inline(always)]
pub fn enabled() -> bool {
    let e = ENABLED.load(Relaxed);
    if e == UNSET {
        return init_config();
    }
    e != 0
}

// ------------------------------------------------------------------ entry from tier 0

pub enum Run {
    /// no compiled code for this pc: keep interpreting
    NotRun,
    Return(PyRef),
    /// exception set; f.lasti / f.stack_top describe the raising instruction
    Error,
    /// continue interpreting at this pc
    Deopt(usize),
    /// (perf6) exception set and already unwound out of this frame (traceback entry
    /// added, stack cleared): the frame returns the error without unwinding again
    Propagated,
    /// the generator frame yielded this value (frame suspended at f.next_instr)
    Yield(PyRef),
}

#[inline]
fn compilable(f: &Frame, cd: &CodeData) -> bool {
    let _ = cd;
    f.locals.is_null() && crate::types::code::code(f.code()).flags() & rpy_compiler::bytecode::co::OPTIMIZED != 0
}

/// Called by the interpreter when it (re)starts dispatching at `pc`.
#[inline]
pub fn try_run(ts: &ThreadState, f: &mut Frame, cd: &CodeData, pc: usize) -> Run {
    if crate::interp::pep523::forces_interpreter() {
        return Run::NotRun; // (capi2) every frame entry must reach eval_frame
    }
    let c = cd.caches.jit.load(Acquire);
    if c.is_null() {
        if pc != 0 || !enabled() {
            return Run::NotRun;
        }
        // (scale2) count only while it can still trigger tier 1 (plain update, a lost
        // one only delays tiering): code that failed to compile is called by many
        // threads, and an atomic add per call contends on the code object
        if cd.caches.tier_state.load(Relaxed) != tier::INTERP {
            return Run::NotRun;
        }
        let n = cd.invocation_count.load(Relaxed).wrapping_add(1);
        cd.invocation_count.store(n, Relaxed);
        if n < THRESHOLD.load(Relaxed) {
            return Run::NotRun;
        }
        if !compile(f, cd) {
            return Run::NotRun;
        }
        return try_run(ts, f, cd, pc);
    }
    if pc == 0 {
        let cc = &cd.caches;
        if t2_countdown(cc) {
            opt::compile(f.code(), f.globals, f.builtins, 0);
        }
    }
    run(ts, f, unsafe { &*c }, pc)
}

/// Backedge to `target`: true if the interpreter should re-dispatch through
/// `try_run` (compiled code with an entry at the loop header exists).
#[inline]
pub fn osr_check(f: &mut Frame, cd: &CodeData, target: usize) -> bool {
    if crate::interp::pep523::forces_interpreter() {
        return false;
    }
    let c = cd.caches.jit.load(Acquire);
    if !c.is_null() {
        let c = unsafe { &*c };
        return c.entries.get(target).is_some_and(|&e| e != u32::MAX);
    }
    if !enabled() || cd.caches.tier_state.load(Relaxed) != tier::INTERP {
        return false;
    }
    let n = cd.backedge_count.load(Relaxed);
    if n < THRESHOLD.load(Relaxed).saturating_mul(10) {
        return false;
    }
    compile(f, cd) && osr_check(f, cd, target)
}

/// Tier-2 entry for `pc`, if any (standard unit at 0, OSR unit at its header).
/// (Generator units also have entries after each yield in the standard unit.)
#[inline]
fn opt_entry(cd: &CodeData, pc: usize) -> Option<usize> {
    let o = cd.caches.opt.load(Acquire);
    if pc == 0 {
        return if o.is_null() { None } else { unsafe { (*o).entry(0) } };
    }
    let p = cd.caches.osr.load(Acquire);
    if !p.is_null() {
        if let Some(e) = unsafe { (*p).entry(pc) } {
            return Some(e);
        }
    }
    if o.is_null() { None } else { unsafe { (*o).entry(pc) } }
}

#[inline]
fn run(ts: &ThreadState, f: &mut Frame, c: &Compiled, pc: usize) -> Run {
    let cd: &CodeData = unsafe { &*(crate::types::code::code_data(f.code()) as *const CodeData) };
    // (perf7) pc 0 of generator code is its eager prologue (a genexpr's `iter()` run at
    // call time): tier 2 deoptimises at RETURN_GENERATOR, so every prologue run there
    // was a deopt, and after DEOPT_LIMIT of them the unit (with the resumption entries
    // the generator body needs) was discarded and recompiled, over and over
    let gen_prologue = pc == 0 && crate::types::code::code(f.code()).flags() & GEN_FLAGS != 0;
    let (target, t2) = match if gen_prologue { None } else { opt_entry(cd, pc) } {
        Some(t) => (t, true),
        None => match c.entries.get(pc) {
            Some(&o) if o != u32::MAX => (c.addr + o as usize, false),
            _ => return Run::NotRun,
        },
    };
    crate::tlab::ensure_registered();
    let entry: EntryFn = unsafe { std::mem::transmute(c.addr) };
    let mut out: *mut PyObject = std::ptr::null_mut();
    let mut st = unsafe { entry(f, f.slots.as_mut_ptr(), ts, target, &mut out) };
    let mut t2 = t2;
    // (perf6) an exception in an activation the interpreter entered compiled code for
    // (OSR, generator resumption): unwound in Rust as for call frames (finish_in_interp),
    // and continued at the handler in compiled code, instead of the interpreter running the
    // handler up to the next back edge for every caught exception
    while st == tier1::ST_ERROR {
        // `unwind_compiled` links the frame under ts.frame itself: this frame already
        // is the current one, so present its caller and restore both links afterwards
        let (saved_prev, cur) = (f.prev, ts.frame.get());
        ts.frame.set(saved_prev);
        let u = crate::interp::eval::unwind_compiled(ts, f);
        ts.frame.set(cur);
        f.prev = saved_prev;
        match u {
            crate::interp::eval::Unwound::Propagate => return Run::Propagated,
            crate::interp::eval::Unwound::Interp => return Run::Error,
            crate::interp::eval::Unwound::Handler(hpc) => {
                let t = match opt_entry(cd, hpc) {
                    Some(t) => {
                        t2 = true;
                        t
                    }
                    None => match c.entries.get(hpc) {
                        Some(&o) if o != u32::MAX => {
                            t2 = false;
                            c.addr + o as usize
                        }
                        _ => return Run::Deopt(hpc),
                    },
                };
                out = std::ptr::null_mut();
                st = unsafe { entry(f, f.slots.as_mut_ptr(), ts, t, &mut out) };
            }
        }
    }
    match st {
        tier1::ST_RETURN => Run::Return(unsafe { PyRef::from_raw(out) }),
        tier1::ST_ERROR => Run::Error,
        tier1::ST_YIELD => Run::Yield(unsafe { PyRef::from_raw(out) }),
        _ => {
            if t2 {
                opt::note_deopt(&cd.caches, f);
            }
            Run::Deopt(f.next_instr as usize)
        }
    }
}

/// Fast call of a Python function whose code is compiled: exact positional
/// arguments, no keywords, not a generator. Sets up the frame and enters the machine
/// code directly (no interpreter prelude). Falls back to the interpreter for the rest
/// of the activation after a deopt or an exception (the interpreter unwinds).
/// Returns None if the fast path does not apply.
#[inline]
pub fn call_fast(func: &PyObject, args: &[*mut PyObject], nargs: usize, kwnames: Option<&PyObject>) -> Option<PyResult> {
    if kwnames.is_some() {
        return None;
    }
    let f = crate::types::funcobject::func(func);
    if f.func_code.is_null() {
        return None;
    }
    let code = unsafe { &*f.func_code };
    let cd = crate::types::code::code_data(code);
    let c = cd.caches.jit.load(Acquire);
    if c.is_null() {
        return None;
    }
    let c = unsafe { &*c };
    let co = crate::types::code::code(code).obj;
    if co.co_argcount as usize != nargs
        || co.co_flags as u32
            & (rpy_compiler::bytecode::co::VARARGS
                | rpy_compiler::bytecode::co::VARKEYWORDS
                | rpy_compiler::bytecode::co::GENERATOR
                | rpy_compiler::bytecode::co::COROUTINE
                | rpy_compiler::bytecode::co::ASYNC_GENERATOR)
            != 0
        // (non-optimized code runs with its globals as locals: interpreter path)
        || co.co_flags as u32 & rpy_compiler::bytecode::co::OPTIMIZED == 0
        || co.co_kwonlyargcount != 0
        || crate::modules::sys::tracing_active()
        || crate::interp::pep523::forces_interpreter()
    {
        return None;
    }
    let off = match c.entries.first() {
        Some(&o) if o != u32::MAX => o,
        _ => return None,
    };
    Some(call_compiled(func, code, cd, c, off, &args[..nargs]))
}

fn call_compiled(func: &PyObject, code: &PyObject, cd: &CodeData, c: &Compiled, off: u32, args: &[*mut PyObject]) -> PyResult {
    let f = crate::types::funcobject::func(func);
    let ts = crate::threads::current();
    crate::interp::enter_recursive_call(ts, "")?;
    // borrowed code/func (frame.rs `new_call`): the caller keeps `func` alive
    let mut frame = Frame::new_call(code, func, unsafe { &*f.func_globals }, unsafe { &*f.func_builtins });
    for (i, &a) in args.iter().enumerate() {
        unsafe { incref(a) };
        frame.slots[i] = a;
    }
    enter_bound(ts, f, code, cd, c, off, frame)
}

/// (perf4) Call of a compiled, optimized, non-generator Python function with any
/// argument shape (keywords, defaults, *args/**kwargs parameters): the arguments are
/// bound into a pooled frame by `bind_arguments` and the activation enters machine
/// code directly (no `run_frame`/`eval_frame` prelude, cheap frame recycling).
/// None: not compiled (or tracing); the caller takes the interpreter path.
#[inline]
pub fn call_bound(func: &PyObject, args: &[*mut PyObject], nargs: usize, kwnames: Option<&PyObject>) -> Option<PyResult> {
    let f = crate::types::funcobject::func(func);
    let code = unsafe { &*f.func_code };
    let cd = crate::types::code::code_data(code);
    let c = cd.caches.jit.load(Acquire);
    if c.is_null() {
        return None;
    }
    let c = unsafe { &*c };
    let off = match c.entries.first() {
        Some(&o) if o != u32::MAX => o,
        _ => return None,
    };
    if crate::modules::sys::tracing_active() {
        return None;
    }
    let ts = crate::threads::current();
    if let Err(e) = crate::interp::enter_recursive_call(ts, "") {
        return Some(Err(e));
    }
    let mut frame = Frame::new_call(code, func, unsafe { &*f.func_globals }, unsafe { &*f.func_builtins });
    if let Err(e) = crate::interp::bind_arguments(func, &mut frame, args, nargs, kwnames) {
        crate::interp::leave_recursive_call(ts);
        crate::interp::frame::recycle(frame);
        return Some(Err(e));
    }
    Some(enter_bound(ts, f, code, cd, c, off, frame))
}

/// (perf4) Run a `Frame::new_call` frame of optimized, non-generator function `func`
/// whose arguments are bound: in compiled code when there is a unit, else in the
/// interpreter (`run_frame`).
pub fn run_bound(func: &PyObject, frame: Box<Frame>) -> PyResult {
    let f = crate::types::funcobject::func(func);
    let code = unsafe { &*f.func_code };
    let cd = crate::types::code::code_data(code);
    let c = cd.caches.jit.load(Acquire);
    if !c.is_null() && !crate::modules::sys::tracing_active() {
        let c = unsafe { &*c };
        if let Some(&off) = c.entries.first() {
            if off != u32::MAX {
                let ts = crate::threads::current();
                if let Err(e) = crate::interp::enter_recursive_call(ts, "") {
                    crate::interp::frame::recycle(frame);
                    return Err(e);
                }
                return enter_bound(ts, f, code, cd, c, off, frame);
            }
        }
    }
    crate::interp::run_frame(frame)
}

/// Run a bound call frame in compiled code (recursion depth already entered; left here).
#[inline]
fn enter_bound(ts: &ThreadState, f: &crate::types::funcobject::PyFunctionObject, code: &PyObject, cd: &CodeData, c: &Compiled, off: u32, frame: Box<Frame>) -> PyResult {
    let mut target = c.addr + off as usize;
    if bump_invocations(cd) {
        if let Some(t) = opt::compile(code, f.func_globals, f.func_builtins, 0) {
            target = t;
        }
    } else if let Some(t) = opt_entry(cd, 0) {
        target = t;
    }
    crate::tlab::ensure_registered();
    let r = run_call_frame(ts, frame, c.addr, target);
    crate::interp::leave_recursive_call(ts);
    r
}

/// Non-atomic hotness counters (a lost update only delays tiering). True when the
/// tier-2 call countdown expires.
#[inline(always)]
fn bump_invocations(cd: &CodeData) -> bool {
    // (scale2) only compiled code gets here: the invocation count (tier-1 trigger) is
    // no longer needed, and the tier-2 countdown is written only while it runs, so
    // that threads calling the same function do not fight over the counters' line
    t2_countdown(&cd.caches)
}

/// (scale2) Tier-2 call countdown: decrement while positive; true when it expires.
#[inline(always)]
fn t2_countdown(cc: &crate::interp::cache::CodeCaches) -> bool {
    let t = cc.t2_calls.load(Relaxed);
    if t <= 0 {
        return false;
    }
    cc.t2_calls.store(t - 1, Relaxed);
    t == 1
}

/// Run a freshly set up call frame in compiled code (arguments stored) and release
/// it. Deopt or an exception hands the rest of the activation to the interpreter.
#[inline]
fn run_call_frame(ts: &ThreadState, mut frame: Box<Frame>, tramp: usize, target: usize) -> PyResult {
    use crate::interp::frame::FrameState;
    let prev = ts.frame.get();
    frame.prev = prev;
    let fp: *mut Frame = &mut *frame;
    ts.frame.set(fp);
    frame.state = FrameState::Executing;
    let entry: EntryFn = unsafe { std::mem::transmute(tramp) };
    let mut out: *mut PyObject = std::ptr::null_mut();
    let st = unsafe { entry(fp, frame.slots.as_mut_ptr(), ts, target, &mut out) };
    ts.frame.set(prev);
    frame.prev = std::ptr::null_mut();
    if st == tier1::ST_RETURN {
        crate::interp::frame::recycle_call(frame);
        return Ok(unsafe { PyRef::from_raw(out) });
    }
    finish_in_interp(ts, frame, st)
}

#[cold]
#[inline(never)]
pub(crate) fn finish_in_interp(ts: &ThreadState, mut frame: Box<Frame>, mut st: u32) -> PyResult {
    use crate::interp::eval::Unwound;
    let mut throw = None;
    loop {
        if st == tier1::ST_DEOPT {
            opt::note_deopt(&crate::types::code::code_data(frame.code()).caches, &frame);
            break;
        }
        if st != tier1::ST_ERROR {
            break;
        }
        match crate::interp::eval::unwind_compiled(ts, &mut frame) {
            Unwound::Propagate => {
                // (perf4) no handler in this frame: propagate without the interpreter
                crate::interp::frame_finished(frame);
                return Err(crate::errors::PyErr);
            }
            Unwound::Handler(pc) => {
                // (perf5) continue at the handler in compiled code when it is an entry
                let mut out: *mut PyObject = std::ptr::null_mut();
                match reenter(ts, &mut frame, pc, &mut out) {
                    Some(tier1::ST_RETURN) => {
                        crate::interp::frame::recycle_call(frame);
                        return Ok(unsafe { PyRef::from_raw(out) });
                    }
                    Some(s) => st = s,
                    None => {
                        frame.next_instr = pc as u32;
                        break;
                    }
                }
            }
            Unwound::Interp => {
                frame.next_instr = frame.lasti + 1;
                throw = crate::errors::take_exc();
                break;
            }
        }
    }
    // let the interpreter finish this activation (unwinding or deopt)
    let r = match crate::interp::eval::eval_frame(ts, &mut frame, throw) {
        Ok(crate::interp::Exit::Return(v)) => Ok(v),
        Ok(crate::interp::Exit::Yield(_)) => Err(crate::errors::system_error("yield from a non-generator frame")),
        Err(e) => Err(e),
    };
    crate::interp::frame_finished(frame);
    r
}

/// Continue the call frame `frame` (not executing) in compiled code at `pc`: the
/// status, or None when `pc` is not an entry point.
fn reenter(ts: &ThreadState, frame: &mut Frame, pc: usize, out: &mut *mut PyObject) -> Option<u32> {
    use crate::interp::frame::FrameState;
    let cd: &CodeData = unsafe { &*(crate::types::code::code_data(frame.code()) as *const CodeData) };
    let c = cd.caches.jit.load(Acquire);
    if c.is_null() {
        return None;
    }
    let c = unsafe { &*c };
    let target = match opt_entry(cd, pc) {
        Some(t) => t,
        None => match c.entries.get(pc) {
            Some(&o) if o != u32::MAX => c.addr + o as usize,
            _ => return None,
        },
    };
    let prev = ts.frame.get();
    frame.prev = prev;
    let fp: *mut Frame = frame;
    ts.frame.set(fp);
    frame.state = FrameState::Executing;
    let entry: EntryFn = unsafe { std::mem::transmute(c.addr) };
    let st = unsafe { entry(fp, frame.slots.as_mut_ptr(), ts, target, out) };
    ts.frame.set(prev);
    frame.prev = std::ptr::null_mut();
    Some(st)
}

/// Deoptimisation inside a callee inlined by tier 2 (opt/build.rs `try_inline`):
/// build its frame from `buf` (owned references: `nloc` locals, then `nstack` stack
/// entries; NULL = unbound), resume it in the interpreter at `resume` (or raise the
/// pending exception at `lasti` when `error`), and return its result (new reference)
/// or NULL with an exception set.
pub unsafe extern "C" fn h2_run_inlined(func: *mut PyObject, buf: *mut *mut PyObject, counts: u64, resume: u64, lasti: u64, error: u64) -> *mut PyObject {
    unsafe {
        let (nloc, nstack) = ((counts & 0xffff_ffff) as usize, (counts >> 32) as usize);
        let f = crate::types::funcobject::func(&*func);
        let code = f.func_code;
        let co = crate::types::code::code(&*code).obj;
        let nlp = co.co_nlocalsplus as usize;
        let total = nlp + co.co_stacksize as usize + 1;
        let mut frame = Frame::new_call_raw(code, func, f.func_globals, f.func_builtins, nlp, total);
        // the function object is only borrowed from a constant in compiled code
        frame.make_owned();
        let s = frame.slots.as_mut_ptr();
        for k in 0..nloc.min(nlp) {
            *s.add(k) = *buf.add(k);
        }
        for k in 0..nstack {
            *s.add(nlp + k) = *buf.add(nloc + k);
        }
        frame.stack_top = nlp + nstack;
        frame.lasti = lasti as u32;
        frame.next_instr = resume as u32;
        let throw = if error != 0 {
            frame.next_instr = lasti as u32 + 1;
            crate::errors::take_exc()
        } else {
            None
        };
        let ts = crate::threads::current();
        let r = match crate::interp::enter_recursive_call(ts, "") {
            Ok(()) => {
                let r = crate::interp::eval::eval_frame(ts, &mut frame, throw);
                crate::interp::leave_recursive_call(ts);
                match r {
                    Ok(crate::interp::Exit::Return(v)) => v.into_raw(),
                    Ok(crate::interp::Exit::Yield(_)) => {
                        let _ = crate::errors::system_error("yield from an inlined frame");
                        std::ptr::null_mut()
                    }
                    Err(_) => std::ptr::null_mut(),
                }
            }
            Err(_) => std::ptr::null_mut(),
        };
        crate::interp::frame_finished(frame);
        r
    }
}

/// Tier-1 `Call` of a Python function: [callable, self_or_null, args..] at `base`
/// (n = argument count without self). The arguments (and self) are moved into the
/// callee frame, the callable reference becomes the frame's `func`. Result in
/// `*base`; status like the other helpers. Falls back to the generic call.
pub unsafe extern "C" fn h_call_py(base: *mut *mut PyObject, n: u64) -> u64 {
    unsafe {
        let callable = *base;
        let self_ = *base.add(1);
        let (argp, nargs) = if self_.is_null() { (base.add(2), n as usize) } else { (base.add(1), n as usize + 1) };
        let f = callable as *mut crate::types::funcobject::PyFunctionObject;
        let code = (*f).func_code;
        let cd = crate::types::code::code_data(&*code);
        let cc = &cd.caches;
        let target = cc.fast_entry.load(Acquire);
        let ts = crate::threads::current();
        let depth = ts.recursion_depth.get() + 1;
        let argc = cc.fast_argc.load(Relaxed) as usize;
        // (jit5) trailing parameters with defaults: filled from __defaults__ below
        let ndef = if argc == nargs {
            0
        } else {
            let dt = (*f).func_defaults;
            if nargs > argc || dt.is_null() || crate::types::tuple::tuple_len(&*dt) < argc - nargs {
                usize::MAX
            } else {
                argc - nargs
            }
        };
        if target == 0
            || ndef == usize::MAX
            || crate::modules::sys::tracing_active()
        || crate::interp::pep523::forces_interpreter()
            || depth > ts.c.py_recursion_limit
            || crate::interp::stack_pointer() < ts.stack_soft_limit()
        {
            return helpers::h_call(base.add(n as usize + 2), n, 0);
        }
        let c = &*cd.caches.jit.load(Acquire);
        ts.recursion_depth.set(depth);
        let (nlp, total) = (cc.fast_nlp.load(Relaxed) as usize, cc.fast_total.load(Relaxed) as usize);
        // the callable stays in the caller's stack slot for the duration of the call:
        // that reference keeps func/code alive for the borrowed frame (frame.rs `owns_fc`)
        let mut frame = Frame::new_call_raw(code, callable, (*f).func_globals, (*f).func_builtins, nlp, total);
        let s = frame.slots.as_mut_ptr();
        for i in 0..nargs {
            *s.add(i) = *argp.add(i);
            *argp.add(i) = std::ptr::null_mut();
        }
        if ndef != 0 {
            let dt = &*(*f).func_defaults;
            let items = crate::types::tuple::tuple_items(dt);
            let first = items.len() - ndef;
            for k in 0..ndef {
                let v = items[first + k].as_ptr();
                incref(v);
                *s.add(nargs + k) = v;
            }
        }
        let mut target = target;
        if bump_invocations(cd) {
            if let Some(t) = opt::compile(&*code, (*f).func_globals, (*f).func_builtins, 0) {
                target = t;
            }
        }
        let r = run_call_frame(ts, frame, c.addr, target);
        ts.recursion_depth.set(depth - 1);
        *base = std::ptr::null_mut();
        decref(callable);
        match r {
            Ok(v) => {
                *base = v.into_raw();
                helpers::OK
            }
            Err(_) => helpers::ERR,
        }
    }
}

/// Resume/start a frame directly in compiled code, bypassing the interpreter prelude
/// (used by eval_frame, e.g. for every generator resumption). Ok(result) if the
/// activation finished (return or yield) in compiled code; Err(throw) if the
/// interpreter must take over, with the exception to raise at f.next_instr - 1.
#[inline]
pub fn resume(ts: &ThreadState, f: &mut Frame, throw: Option<PyRef>) -> Result<PyResult<crate::interp::Exit>, Option<PyRef>> {
    if throw.is_some() {
        return Err(throw);
    }
    let cd = crate::types::code::code_data(f.code());
    let c = cd.caches.jit.load(Acquire);
    if c.is_null() || crate::modules::sys::tracing_active() || crate::interp::pep523::forces_interpreter() {
        return Err(None);
    }
    let pc = f.next_instr as usize;
    // (scale2) no invocation count here: the code is compiled already (the count only
    // triggers tier 1), and an atomic add per generator start on the shared code object
    // was a contended cache line for threads creating generators of the same function
    f.yield_from_send = -1;
    match run(ts, f, unsafe { &*c }, pc) {
        Run::NotRun => Err(None),
        Run::Return(v) => {
            f.clear_stack();
            Ok(Ok(crate::interp::Exit::Return(v)))
        }
        Run::Yield(v) => Ok(Ok(crate::interp::Exit::Yield(v))),
        Run::Deopt(npc) => {
            f.next_instr = npc as u32;
            Err(None)
        }
        // (perf6) already unwound out of the frame: the activation finished with the error
        Run::Propagated => Ok(Err(crate::errors::PyErr)),
        Run::Error => {
            f.next_instr = f.lasti + 1;
            match crate::errors::take_exc() {
                Some(e) => Err(Some(e)),
                None => Ok(Err(crate::errors::system_error("error return without exception set"))),
            }
        }
    }
}

fn env_flag(cell: &AtomicU32, var: &str) -> bool {
    match cell.load(Relaxed) {
        UNSET => {
            let en = std::env::var(var).map(|v| v != "0").unwrap_or(true);
            cell.store(en as u32, Relaxed);
            en
        }
        x => x != 0,
    }
}

/// Tier 1 compiles the instructions shared with the interpreter through
/// `eval::exec_misc` (RPYTHON_JIT1_MISC=0: they deoptimise instead).
/// RaiseVarargs / Reraise compiled in tier 1 (RPYTHON_JIT1_RAISE=0: they deoptimise).
pub fn raise_enabled() -> bool {
    static F: AtomicU32 = AtomicU32::new(UNSET);
    env_flag(&F, "RPYTHON_JIT1_RAISE")
}

pub fn misc_enabled() -> bool {
    static F: AtomicU32 = AtomicU32::new(UNSET);
    env_flag(&F, "RPYTHON_JIT1_MISC")
}

/// Generator frames are resumed directly in compiled code (RPYTHON_JIT_GENFAST=0:
/// always through the interpreter's entry).
pub fn genfast_enabled() -> bool {
    static F: AtomicU32 = AtomicU32::new(UNSET);
    env_flag(&F, "RPYTHON_JIT_GENFAST")
}

// ------------------------------------------------------------------ generator resumption

static TRAMPOLINE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// A process-wide copy of the entry trampoline every compiled unit starts with
/// (fn(frame, slots, ts, target, out) -> status; tier1.rs `compile`). Units share the
/// stack layout of the trampoline, so code entered through it may exit through the
/// epilogue of any unit.
pub fn trampoline() -> usize {
    *TRAMPOLINE.get_or_init(|| {
        use x64::R::*;
        let mut a = x64::Asm::new();
        a.push(Rbp);
        a.mov_rr(Rbp, Rsp);
        a.push(Rbx);
        a.push(R12);
        a.push(R13);
        a.push(R14);
        a.push(R15);
        a.sub_ri(Rsp, 8);
        a.store(x64::M(Rsp, 0), R8);
        a.mov_rr(Rbx, Rdi);
        a.mov_rr(R12, Rsi);
        a.mov_rr(R13, Rdx);
        a.load_fs0(R15);
        a.jmp_r(Rcx);
        let t = mem::install(&a.buf, |_, _| {}).expect("jit: cannot install the entry trampoline");
        TRAMP_ADDR.store(t, Relaxed);
        t
    })
}

/// Machine-code entry of a newly created generator frame of `code` (0 if its code is
/// not compiled): the frame's first resumption enters it directly (`Frame::resume`).
/// Counts the creation towards tier-2 compilation of the generator's code.
#[inline]
pub fn generator_entry(f: &Frame) -> usize {
    let cd = crate::types::code::code_data(f.code());
    let c = cd.caches.jit.load(Acquire);
    if c.is_null() || crate::modules::sys::tracing_active() || crate::interp::pep523::forces_interpreter() {
        return 0;
    }
    if bump_invocations(cd) {
        opt::compile(f.code(), f.globals, f.builtins, 0);
    }
    // pc 0, or the instruction after an eager prologue's ReturnGenerator
    let pc = f.next_instr as usize;
    match opt_entry(cd, pc) {
        Some(t) => t,
        None => match unsafe { (&(*c).entries).get(pc) } {
            Some(&o) if o != u32::MAX => unsafe { (*c).addr + o as usize },
            _ => 0,
        },
    }
}

/// Enter compiled code at `target` for the linked, executing frame `f`.
#[inline(always)]
pub fn enter(ts: &ThreadState, f: &mut Frame, target: usize, out: &mut *mut PyObject) -> u32 {
    crate::tlab::ensure_registered();
    let entry: EntryFn = unsafe { std::mem::transmute(trampoline()) };
    unsafe { entry(f, f.slots.as_mut_ptr(), ts, target, out) }
}

/// `enter` for callers running inside compiled code (jit5: generator resumption from
/// Send/ForIter): the thread's allocator is registered and the trampoline exists.
#[inline(always)]
pub fn enter_registered(ts: &ThreadState, f: &mut Frame, target: usize, out: &mut *mut PyObject) -> u32 {
    let t = TRAMP_ADDR.load(Relaxed);
    let t = if t != 0 { t } else { trampoline() };
    let entry: EntryFn = unsafe { std::mem::transmute(t) };
    unsafe { entry(f, f.slots.as_mut_ptr(), ts, target, out) }
}
static TRAMP_ADDR: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Compiled code entered by `enter` ended with a deopt or an error (status `st`): the
/// interpreter finishes this activation (or runs it to the next yield).
#[cold]
#[inline(never)]
pub fn continue_in_interp(ts: &ThreadState, f: &mut Frame, st: u32) -> PyResult<crate::interp::Exit> {
    let throw = if st == tier1::ST_ERROR {
        f.next_instr = f.lasti + 1;
        match crate::errors::take_exc() {
            Some(e) => Some(e),
            None => return Err(crate::errors::system_error("error return without exception set")),
        }
    } else {
        let cc = &crate::types::code::code_data(f.code()).caches;
        opt::note_deopt(cc, f);
        None
    };
    crate::interp::eval::eval_inner(ts, f, throw)
}

/// (jit6) Drain-mode tier-1 code of a generator expression (tier1.rs YieldValue,
/// helpers.rs `h_drain`), built on first use; None if it cannot be (or is being)
/// built. RPYTHON_JIT_DRAIN=0 disables draining.
pub fn drain_code(cd: &CodeData, code: &PyObject) -> Option<*const Compiled> {
    use std::sync::atomic::AtomicPtr;
    let cell: &AtomicPtr<Compiled> = &cd.caches.drain;
    let p = cell.load(Acquire);
    if p as usize == 1 {
        return None;
    }
    if !p.is_null() {
        return Some(p);
    }
    if cell.compare_exchange(std::ptr::null_mut(), 1 as *mut Compiled, AcqRel, Acquire).is_err() {
        return None;
    }
    static DRAIN: AtomicU32 = AtomicU32::new(UNSET);
    if !env_flag(&DRAIN, "RPYTHON_JIT_DRAIN") {
        return None;
    }
    let co_ = crate::types::code::code(code);
    let nlp = co_.obj.co_nlocalsplus as usize;
    let tables = cd.caches.tables(&cd.code);
    let out = tier1::compile_mode(cd, nlp, co_.flags(), tables, true).ok()?;
    let asm = &out.asm;
    let addr = mem::install(&asm.buf, |buf, base| {
        let _ = asm.patch_abs(buf, base);
    })?;
    if DEBUG.load(Relaxed) != 0 {
        eprintln!("[jit] tier1 drain {} ({} bytes)", crate::types::str::to_string_lossy(&cd.co_qualname), asm.buf.len());
    }
    let c = Box::into_raw(Box::new(Compiled { addr, len: asm.buf.len(), entries: out.entries }));
    cell.store(c, Release);
    Some(c)
}

/// Publish the fast-call descriptor of a code object (see CodeCaches::fast_entry).
fn publish_fast_entry(code: &PyObject, cd: &CodeData, target: usize) {
    use rpy_compiler::bytecode::co;
    let co_ = crate::types::code::code(code).obj;
    let flags = co_.co_flags as u32;
    if target == 0
        || flags & (co::VARARGS | co::VARKEYWORDS | co::GENERATOR | co::COROUTINE | co::ASYNC_GENERATOR | co::ITERABLE_COROUTINE) != 0
        || flags & co::OPTIMIZED == 0
        || co_.co_kwonlyargcount != 0
    {
        return;
    }
    let cc = &cd.caches;
    let nlp = co_.co_nlocalsplus as u32;
    cc.fast_argc.store(co_.co_argcount as u32, Relaxed);
    cc.fast_nlp.store(nlp, Relaxed);
    cc.fast_total.store(nlp + co_.co_stacksize as u32 + 1, Relaxed);
    cc.fast_argc_ok.store(true, Relaxed);
    cc.fast_entry.store(target, Release);
}

/// Compile the frame's code object (tier 1). Returns true if code is installed.
#[cold]
fn compile(f: &Frame, cd: &CodeData) -> bool {
    if !compilable(f, cd) {
        cd.caches.tier_state.store(tier::FAILED, Relaxed);
        return false;
    }
    if cd.caches.tier_state.compare_exchange(tier::INTERP, tier::COMPILING, Acquire, Relaxed).is_err() {
        return false;
    }
    let nlp = crate::types::code::code(f.code()).obj.co_nlocalsplus as usize;
    let tables = cd.caches.tables(&cd.code);
    let dbg = DEBUG.load(Relaxed) != 0;
    let flags = crate::types::code::code(f.code()).flags();
    match tier1::compile(cd, nlp, flags, tables) {
        Ok(out) => {
            let code = &out.asm.buf;
            let asm = &out.asm;
            let addr = mem::install(code, |buf, base| {
                let _ = asm.patch_abs(buf, base);
            });
            match addr {
                Some(addr) => {
                    if let Ok(dir) = std::env::var("RPYTHON_JIT_DUMP") {
                        let nm = crate::types::str::to_string_lossy(&cd.co_qualname);
                        let _ = std::fs::write(format!("{}/{}.{:x}.bin", dir, nm, addr), code);
                    }
                    if dbg {
                        eprintln!(
                            "[jit] tier1 {} ({} instrs -> {} bytes)",
                            crate::types::str::to_string_lossy(&cd.co_qualname),
                            cd.code.len(),
                            code.len()
                        );
                        if DEBUG.load(Relaxed) >= 5 {
                            for ins in analysis::decode(&cd.code) {
                                eprintln!("    {:4} {:?} {}", ins.pc, ins.op, ins.arg);
                            }
                        }
                    }
                    let entry0 = out.entries.first().copied().unwrap_or(u32::MAX);
                    let c = Box::into_raw(Box::new(Compiled { addr, len: code.len(), entries: out.entries }));
                    cd.caches.jit.store(c, Release);
                    cd.caches.tier_state.store(tier::COMPILED, Release);
                    cd.caches.t2_calls.store(opt::call_threshold(), Relaxed);
                    cd.caches.t2_backedge.store(opt::backedge_threshold(), Relaxed);
                    publish_fast_entry(f.code(), cd, if entry0 == u32::MAX { 0 } else { addr + entry0 as usize });
                    true
                }
                None => {
                    cd.caches.tier_state.store(tier::FAILED, Release);
                    false
                }
            }
        }
        Err(e) => {
            if dbg {
                eprintln!("[jit] tier1 failed for {}: {}", crate::types::str::to_string_lossy(&cd.co_qualname), e);
                if DEBUG.load(Relaxed) >= 4 {
                    for ins in analysis::decode(&cd.code) {
                        eprintln!("    {:4} {:?} {}", ins.pc, ins.op, ins.arg);
                    }
                    for h in cd.exception_table.iter() {
                        eprintln!("    handler {}..{} -> {} depth {} lasti {}", h.start, h.end, h.target, h.depth, h.lasti);
                    }
                }
            }
            cd.caches.tier_state.store(tier::FAILED, Release);
            false
        }
    }
}
