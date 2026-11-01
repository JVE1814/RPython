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
pub mod tier1;
pub mod x64;

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

#[inline]
fn run(ts: &ThreadState, f: &mut Frame, c: &Compiled, pc: usize) -> Run {
    let off = match c.entries.get(pc) {
        Some(&o) if o != u32::MAX => o,
        _ => return Run::NotRun,
    };
    let entry: EntryFn = unsafe { std::mem::transmute(c.addr) };
    let mut out: *mut PyObject = std::ptr::null_mut();
    let st = unsafe { entry(f, f.slots.as_mut_ptr(), ts, c.addr + off as usize, &mut out) };
    match st {
        tier1::ST_RETURN => Run::Return(unsafe { PyRef::from_raw(out) }),
        tier1::ST_ERROR => Run::Error,
        _ => Run::Deopt(f.next_instr as usize),
    }
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
    match tier1::compile(cd, nlp, tables) {
        Ok(out) => {
            let code = &out.asm.buf;
            let asm = &out.asm;
            let addr = mem::install(code, |buf, base| {
                let _ = asm.patch_abs(buf, base);
            });
            match addr {
                Some(addr) => {
                    if dbg {
                        eprintln!(
                            "[jit] tier1 {} ({} instrs -> {} bytes)",
                            crate::types::str::to_string_lossy(&cd.co_qualname),
                            cd.code.len(),
                            code.len()
                        );
                    }
                    let c = Box::into_raw(Box::new(Compiled { addr, len: code.len(), entries: out.entries }));
                    cd.caches.jit.store(c, Release);
                    cd.caches.tier_state.store(tier::COMPILED, Release);
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
