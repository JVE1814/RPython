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
    if std::env::var("RPYTHON_JIT_DEBUG").map(|v| v != "0").unwrap_or(false) {
        DEBUG.store(1, Relaxed);
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
    let c = cd.caches.jit.load(Acquire);
    if c.is_null() {
        if pc != 0 || !enabled() {
            return Run::NotRun;
        }
        let n = cd.invocation_count.fetch_add(1, Relaxed) + 1;
        if n < THRESHOLD.load(Relaxed) || cd.caches.tier_state.load(Relaxed) != tier::INTERP {
            return Run::NotRun;
        }
        if !compile(f, cd) {
            return Run::NotRun;
        }
        return try_run(ts, f, cd, pc);
    }
    if pc == 0 {
        let cc = &cd.caches;
        let t = cc.t2_calls.load(Relaxed);
        cc.t2_calls.store(t.wrapping_sub(1), Relaxed);
        if t == 1 {
            opt::compile(f.code(), f.globals, f.builtins, 0);
        }
    }
    run(ts, f, unsafe { &*c }, pc)
}

/// Backedge to `target`: true if the interpreter should re-dispatch through
/// `try_run` (compiled code with an entry at the loop header exists).
#[inline]
pub fn osr_check(f: &mut Frame, cd: &CodeData, target: usize) -> bool {
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
    let (target, t2) = match opt_entry(cd, pc) {
        Some(t) => (t, true),
        None => match c.entries.get(pc) {
            Some(&o) if o != u32::MAX => (c.addr + o as usize, false),
            _ => return Run::NotRun,
        },
    };
    crate::tlab::ensure_registered();
    let entry: EntryFn = unsafe { std::mem::transmute(c.addr) };
    let mut out: *mut PyObject = std::ptr::null_mut();
    let st = unsafe { entry(f, f.slots.as_mut_ptr(), ts, target, &mut out) };
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
    let n = cd.invocation_count.load(Relaxed);
    cd.invocation_count.store(n.wrapping_add(1), Relaxed);
    let t = cd.caches.t2_calls.load(Relaxed);
    cd.caches.t2_calls.store(t.wrapping_sub(1), Relaxed);
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
fn finish_in_interp(ts: &ThreadState, mut frame: Box<Frame>, st: u32) -> PyResult {
    if st == tier1::ST_DEOPT {
        opt::note_deopt(&crate::types::code::code_data(frame.code()).caches, &frame);
    }
    // let the interpreter finish this activation (unwinding or deopt)
    let throw = if st == tier1::ST_ERROR {
        frame.next_instr = frame.lasti + 1;
        crate::errors::take_exc()
    } else {
        None
    };
    let r = match crate::interp::eval::eval_frame(ts, &mut frame, throw) {
        Ok(crate::interp::Exit::Return(v)) => Ok(v),
        Ok(crate::interp::Exit::Yield(_)) => Err(crate::errors::system_error("yield from a non-generator frame")),
        Err(e) => Err(e),
    };
    crate::interp::frame_finished(frame);
    r
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
        if target == 0
            || cc.fast_argc.load(Relaxed) as usize != nargs
            || crate::modules::sys::tracing_active()
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
    if c.is_null() || crate::modules::sys::tracing_active() {
        return Err(None);
    }
    let pc = f.next_instr as usize;
    if pc == 0 {
        cd.invocation_count.fetch_add(1, Relaxed);
    }
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
        mem::install(&a.buf, |_, _| {}).expect("jit: cannot install the entry trampoline")
    })
}

/// Machine-code entry of a newly created generator frame of `code` (0 if its code is
/// not compiled): the frame's first resumption enters it directly (`Frame::resume`).
/// Counts the creation towards tier-2 compilation of the generator's code.
#[inline]
pub fn generator_entry(f: &Frame) -> usize {
    let cd = crate::types::code::code_data(f.code());
    let c = cd.caches.jit.load(Acquire);
    if c.is_null() || crate::modules::sys::tracing_active() {
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
            }
            cd.caches.tier_state.store(tier::FAILED, Release);
            false
        }
    }
}
