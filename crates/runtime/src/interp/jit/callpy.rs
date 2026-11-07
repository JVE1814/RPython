//! (jit6) Compiled-to-compiled Python calls without Rust on the common path.
//!
//! `routine()` is one process-wide machine-code routine that compiled `Call` sites
//! (tier 1, and tier 2 through the embedded tier-1 template) use for a Python
//! function whose code has a published fast entry. It does what `h_call_py` +
//! `run_call_frame` + `frame::recycle_call` do in Rust, in about a third of the
//! instructions:
//!
//! * frame from the per-thread pool (frame.rs `POOL`, reached through the thread
//!   pointer in r15 like the TLAB caches), geometry from `CodeCaches::fast_*`;
//! * arguments moved from the caller's stack slots, locals cleared, func/code/
//!   globals/builtins borrowed (the callable stays in the caller's slot for the call);
//! * recursion depth, native stack and tracing checks; the tier-2 call countdown;
//! * enter the callee through the shared entry trampoline at `fast_entry`;
//! * on a plain return: unlink, release the slots, put the frame back in the pool,
//!   result into the caller's slot. Everything unusual (deopt/exception in the callee,
//!   a frame object or trace function created during the call, pool empty, argument
//!   count mismatch, ...) takes the Rust paths (`h_call_py`, `h_callpy_finish`,
//!   `h_callpy_recycle`), which keep the old semantics.
//!
//! Contract (like `h_call_py`): rdi = base of [callable, self_or_null, args..] in the
//! caller's stack slots, rsi = argument count without self, rdx = callee CodeData,
//! r13 = thread state, r15 = thread pointer (fs:0). Returns eax = 0 (result in
//! *base) or 1 (exception set). Clobbers the caller-saved registers only.

use super::tier1::ST_RETURN;
use super::x64::{Cc, M, R::*};
use crate::ffi::PyObject;
use crate::interp::frame::{Frame, FrameState};
use crate::threads::{CThreadState, ThreadState};
use crate::types::code::CodeData;
use std::mem::offset_of;

static ROUTINE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// Address of the routine, or 0 when it cannot be used (unexpected `Vec` layout).
pub fn routine() -> usize {
    *ROUTINE.get_or_init(|| build().unwrap_or(0))
}

/// Disabled with RPYTHON_JIT_CALLPY=0 (calls go through `h_call_py`).
fn enabled() -> bool {
    !matches!(std::env::var("RPYTHON_JIT_CALLPY").as_deref(), Ok("0"))
}

fn build() -> Option<usize> {
    if !enabled() {
        return None;
    }
    let (vptr, vcap, vlen) = crate::interp::frame::vec_layout()?;
    let slots = offset_of!(Frame, slots) as i32;
    let (f_ptr, f_cap, f_len) = (slots + vptr, slots + vcap, slots + vlen);
    let f_prev = offset_of!(Frame, prev) as i32;
    let f_code = offset_of!(Frame, code) as i32;
    let f_func = offset_of!(Frame, func) as i32;
    let f_globals = offset_of!(Frame, globals) as i32;
    let f_builtins = offset_of!(Frame, builtins) as i32;
    let f_locals = offset_of!(Frame, locals) as i32;
    let f_nlp = offset_of!(Frame, nlocalsplus) as i32;
    let f_stack_top = offset_of!(Frame, stack_top) as i32;
    let f_owns = offset_of!(Frame, owns_fc) as i32;
    let f_bgb = offset_of!(Frame, borrowed_gb) as i32;
    let f_state = offset_of!(Frame, state) as i32;
    let f_obj = offset_of!(Frame, frame_obj) as i32;
    let f_keep = offset_of!(Frame, keep) as i32;
    let f_trace = offset_of!(Frame, f_trace) as i32;
    let f_lasti = offset_of!(Frame, lasti) as i32;
    let f_next = offset_of!(Frame, next_instr) as i32;
    let f_t2site = offset_of!(Frame, t2_site) as i32;
    let ts_frame = offset_of!(ThreadState, frame) as i32;
    let ts_depth = offset_of!(ThreadState, recursion_depth) as i32;
    let ts_limit = (offset_of!(ThreadState, c) + offset_of!(CThreadState, py_recursion_limit)) as i32;
    let ts_low = offset_of!(ThreadState, stack_low) as i32;
    let ts_margin = offset_of!(ThreadState, stack_margin) as i32;
    let cc = offset_of!(CodeData, caches);
    use crate::interp::cache::CodeCaches as C;
    let cd_argc = (cc + offset_of!(C, fast_argc)) as i32;
    let cd_nlp = (cc + offset_of!(C, fast_nlp)) as i32;
    let cd_total = (cc + offset_of!(C, fast_total)) as i32;
    let cd_entry = (cc + offset_of!(C, fast_entry)) as i32;
    let cd_t2 = (cc + offset_of!(C, t2_calls)) as i32;
    let cd_invoc = offset_of!(CodeData, invocation_count) as i32;
    use crate::types::funcobject::PyFunctionObject as F;
    let fn_code = offset_of!(F, func_code) as i32;
    let fn_globals = offset_of!(F, func_globals) as i32;
    let fn_builtins = offset_of!(F, func_builtins) as i32;
    let fn_defaults = offset_of!(F, func_defaults) as i32;
    let tup_size = offset_of!(crate::ffi::PyVarObject, ob_size) as i32;
    let tup_items = offset_of!(crate::ffi::PyTupleObject, ob_item) as i32;
    use crate::ffi::{PyObject as O, REF_SHARED_SHIFT};
    let _ = REF_SHARED_SHIFT;
    let ob_tid = offset_of!(O, ob_tid) as i32;
    let ob_ref_local = offset_of!(O, ob_ref_local) as i32;
    let ob_ref_shared = offset_of!(O, ob_ref_shared) as i32;
    let pool = crate::interp::frame::pool_tls_offset();
    if pool < i32::MIN as isize || pool > i32::MAX as isize - 16 {
        return None;
    }
    let pool = pool as i32;
    let (p_head, p_count) = (pool + offset_of!(crate::interp::frame::FramePool, head) as i32, pool + offset_of!(crate::interp::frame::FramePool, count) as i32);
    let tracing = (&raw const crate::modules::sys::TRACING) as i64;
    let tramp = super::trampoline() as i64;
    let addr = |f: usize| f;

    let mut a = super::x64::Asm::new();
    let slow = a.label();
    let not_ret = a.label();
    let slow_rec = a.label();
    let result = a.label();
    let epi = a.label();
    let free_num = a.label();

    // ---- argument shape: r8 = first argument, r9 = count (self included)
    a.lea(R8, M(Rdi, 16));
    a.mov_rr(R9, Rsi);
    let noself = a.label();
    a.cmp_mi(M(Rdi, 8), 0);
    a.jcc(Cc::E, noself);
    a.sub_ri(R8, 8);
    a.add_ri(R9, 1);
    a.bind(noself);
    let shape_ok = a.label();
    a.cmp32_rm(R9, M(Rdx, cd_argc));
    a.jcc(Cc::E, shape_ok);
    a.jcc(Cc::A, slow);
    // fewer arguments: the rest must come from __defaults__ (a tuple)
    a.load(Rax, M(Rdi, 0));
    a.load(Rax, M(Rax, fn_defaults));
    a.test_rr(Rax, Rax);
    a.jcc(Cc::E, slow);
    a.load32(Rcx, M(Rdx, cd_argc));
    a.sub_rr(Rcx, R9);
    a.cmp_rm(Rcx, M(Rax, tup_size));
    a.jcc(Cc::A, slow);
    a.bind(shape_ok);
    // ---- checks: tracing, recursion depth, native stack
    a.mov_ri(Rax, tracing);
    a.cmp8_mi(M(Rax, 0), 0);
    a.jcc(Cc::NE, slow);
    a.load32(Rax, M(R13, ts_depth));
    a.add32_ri(Rax, 1);
    a.cmp32_rm(Rax, M(R13, ts_limit));
    a.jcc(Cc::G, slow);
    a.load(R10, M(R13, ts_low));
    a.load(R11, M(R13, ts_margin));
    a.add_rr(R10, R11);
    a.cmp_rr(Rsp, R10);
    a.jcc(Cc::B, slow);
    // ---- frame from the pool (slot vector large enough)
    a.load(R11, M(R15, p_head));
    a.test_rr(R11, R11);
    a.jcc(Cc::E, slow);
    a.load32(Rcx, M(Rdx, cd_total));
    a.cmp_rm(Rcx, M(R11, f_cap));
    a.jcc(Cc::A, slow);
    // nothing changed so far; from here on the call is committed
    a.load(Rsi, M(R11, f_prev));
    a.store(M(R15, p_head), Rsi);
    a.add_mi(M(R15, p_count), -1);
    a.store32(M(R13, ts_depth), Rax);
    a.store(M(R11, f_len), Rcx);
    a.load(R10, M(R11, f_ptr));
    a.load32(Rcx, M(Rdx, cd_nlp));
    a.store(M(R11, f_nlp), Rcx);
    a.store(M(R11, f_stack_top), Rcx);
    // move the arguments (rsi = index)
    a.xor_rr(Rsi, Rsi);
    let args_done = a.label();
    let args = a.label();
    a.test_rr(R9, R9);
    a.jcc(Cc::E, args_done);
    a.bind(args);
    a.load_idx(Rax, R8, Rsi, 8, 0);
    a.store_idx(R10, Rsi, 8, 0, Rax);
    a.xor_rr(Rax, Rax);
    a.store_idx(R8, Rsi, 8, 0, Rax);
    a.add_ri(Rsi, 1);
    a.cmp_rr(Rsi, R9);
    a.jcc(Cc::B, args);
    a.bind(args_done);
    // trailing parameters from __defaults__ (new references): rdx = argc,
    // r8 = len(defaults) - argc, rcx = scratch; the code data is back in rdx after
    let defs_done = a.label();
    a.push(Rdx);
    a.load32(Rdx, M(Rdx, cd_argc));
    a.cmp_rr(Rsi, Rdx);
    a.jcc(Cc::AE, defs_done);
    a.load(Rax, M(Rdi, 0));
    a.load(Rax, M(Rax, fn_defaults));
    a.load(R8, M(Rax, tup_size));
    a.sub_rr(R8, Rdx);
    let dl = a.label();
    let dstore = a.label();
    let dshared = a.label();
    a.bind(dl);
    a.lea_idx(R9, R8, Rsi, 1, 0);
    a.load_idx(R9, Rax, R9, 8, tup_items);
    a.load32(Rcx, M(R9, ob_ref_local));
    a.add32_ri(Rcx, 1);
    a.jcc(Cc::E, dstore);
    a.cmp_rm(R15, M(R9, ob_tid));
    a.jcc(Cc::NE, dshared);
    a.store32(M(R9, ob_ref_local), Rcx);
    a.jmp(dstore);
    a.bind(dshared);
    a.lock_add_mi(M(R9, ob_ref_shared), 1 << REF_SHARED_SHIFT);
    a.bind(dstore);
    a.store_idx(R10, Rsi, 8, 0, R9);
    a.add_ri(Rsi, 1);
    a.cmp_rr(Rsi, Rdx);
    a.jcc(Cc::B, dl);
    a.bind(defs_done);
    a.pop(Rdx);
    a.load32(Rcx, M(Rdx, cd_nlp));
    // clear the other locals
    let zdone = a.label();
    let zloop = a.label();
    a.cmp_rr(Rsi, Rcx);
    a.jcc(Cc::AE, zdone);
    a.xor_rr(Rax, Rax);
    a.bind(zloop);
    a.store_idx(R10, Rsi, 8, 0, Rax);
    a.add_ri(Rsi, 1);
    a.cmp_rr(Rsi, Rcx);
    a.jcc(Cc::B, zloop);
    a.bind(zdone);
    // borrowed func/code/globals/builtins
    a.load(Rax, M(Rdi, 0));
    a.store(M(R11, f_func), Rax);
    a.load(Rsi, M(Rax, fn_code));
    a.store(M(R11, f_code), Rsi);
    a.load(Rsi, M(Rax, fn_globals));
    a.store(M(R11, f_globals), Rsi);
    a.load(Rsi, M(Rax, fn_builtins));
    a.store(M(R11, f_builtins), Rsi);
    a.store8_i(M(R11, f_owns), 0);
    a.store8_i(M(R11, f_bgb), 1);
    // ---- save what the call must not clobber; 5 words keep rsp 16-aligned
    a.push(Rbx);
    a.push(R12);
    a.push(R14);
    a.push(Rbp);
    a.sub_ri(Rsp, 8); // [rsp] = the callee's result (`out`)
    a.mov_rr(Rbx, R11);
    a.mov_rr(R12, Rdi);
    a.load(R14, M(Rdx, cd_entry));
    // tier-2 countdown (bump_invocations)
    let nocompile = a.label();
    let docall = a.label();
    // (scale2) the countdown is written only while it runs (> 0): a shared counter
    // stored at every call makes all threads calling the function fight over its
    // cache line. (The invocation count only matters before tier 1, so callees
    // reached here, which are compiled, no longer bump it.)
    let _ = cd_invoc;
    a.cmp32_mi(M(Rdx, cd_t2), 0);
    a.jcc(Cc::LE, nocompile);
    a.sub32_mi(M(Rdx, cd_t2), 1);
    a.jcc(Cc::NE, nocompile);
    a.mov_rr(Rdi, Rbx);
    a.call_abs(addr(h_callpy_compile as usize));
    a.test_rr(Rax, Rax);
    a.jcc(Cc::E, nocompile);
    a.mov_rr(R14, Rax);
    a.bind(nocompile);
    a.bind(docall);
    // link
    a.load(Rsi, M(R13, ts_frame));
    a.store(M(Rbx, f_prev), Rsi);
    a.store(M(R13, ts_frame), Rbx);
    a.store8_i(M(Rbx, f_state), FrameState::Executing as u8);
    // enter: trampoline(frame, slots, ts, target, &out)
    a.mov_rr(Rdi, Rbx);
    a.load(Rsi, M(Rbx, f_ptr));
    a.mov_rr(Rdx, R13);
    a.mov_rr(Rcx, R14);
    a.mov_rr(R8, Rsp);
    a.mov_ri(Rax, tramp);
    a.call_r(Rax);
    // unlink
    a.load(Rcx, M(Rbx, f_prev));
    a.store(M(R13, ts_frame), Rcx);
    a.store_i(M(Rbx, f_prev), 0);
    a.cmp32_ri(Rax, ST_RETURN as i32);
    a.jcc(Cc::NE, not_ret);
    // ---- plain return: release the frame (recycle_call)
    a.cmp_mi(M(Rbx, f_obj), 0);
    a.jcc(Cc::NE, slow_rec);
    // (refs1) values parked by borrow::keep_alive: the general release
    a.cmp_mi(M(Rbx, f_keep), 0);
    a.jcc(Cc::NE, slow_rec);
    a.cmp_mi(M(Rbx, f_locals), 0);
    a.jcc(Cc::NE, slow_rec);
    a.cmp_mi(M(Rbx, f_trace), 0);
    a.jcc(Cc::NE, slow_rec);
    a.cmp8_mi(M(Rbx, f_owns), 0);
    a.jcc(Cc::NE, slow_rec);
    // r14 = cursor, rbp = end of max(stack_top, nlp) slots
    a.load(Rbp, M(Rbx, f_stack_top));
    a.load(Rax, M(Rbx, f_nlp));
    a.cmp_rr(Rbp, Rax);
    a.cmov(Cc::B, Rbp, Rax);
    a.load(R14, M(Rbx, f_ptr));
    a.lea_idx(Rbp, R14, Rbp, 8, 0);
    let rl = a.label();
    let rnext = a.label();
    let rdone = a.label();
    let rshared = a.label();
    a.bind(rl);
    a.cmp_rr(R14, Rbp);
    a.jcc(Cc::AE, rdone);
    a.load(Rdi, M(R14, 0));
    a.test_rr(Rdi, Rdi);
    a.jcc(Cc::E, rnext);
    a.store_i(M(R14, 0), 0);
    a.load32(R11, M(Rdi, ob_ref_local));
    a.cmp32_ri(R11, -1);
    a.jcc(Cc::E, rnext);
    a.cmp_rm(R15, M(Rdi, ob_tid));
    a.jcc(Cc::NE, rshared);
    a.sub32_ri(R11, 1);
    a.store32(M(Rdi, ob_ref_local), R11);
    a.jcc(Cc::NE, rnext);
    a.call_label(free_num);
    a.jmp(rnext);
    a.bind(rshared);
    a.call_abs(addr(super::helpers::h_decref_shared as usize));
    a.bind(rnext);
    a.add_ri(R14, 8);
    a.jmp(rl);
    a.bind(rdone);
    a.load(Rax, M(Rbx, f_nlp));
    a.store(M(Rbx, f_stack_top), Rax);
    a.store32_i(M(Rbx, f_lasti), 0);
    a.store32_i(M(Rbx, f_next), 0);
    a.store8_i(M(Rbx, f_state), FrameState::Created as u8);
    a.store_i(M(Rbx, f_t2site), 0);
    // back into the pool (or dropped when it is full)
    let drop = a.label();
    a.cmp_mi(M(R15, p_count), crate::interp::frame::POOL_MAX as i32);
    a.jcc(Cc::AE, drop);
    a.load(Rax, M(R15, p_head));
    a.store(M(Rbx, f_prev), Rax);
    a.store(M(R15, p_head), Rbx);
    a.add_mi(M(R15, p_count), 1);
    a.jmp(result);
    a.bind(drop);
    a.mov_rr(Rdi, Rbx);
    a.call_abs(addr(h_callpy_drop as usize));
    // ---- result into the caller's slot; the callable reference is released
    a.bind(result);
    a.add32_mi(M(R13, ts_depth), -1);
    a.load(Rdi, M(R12, 0));
    a.load(Rax, M(Rsp, 0));
    a.store(M(R12, 0), Rax);
    let cdone = a.label();
    let cshared = a.label();
    a.load32(R11, M(Rdi, ob_ref_local));
    a.cmp32_ri(R11, -1);
    a.jcc(Cc::E, cdone);
    a.cmp_rm(R15, M(Rdi, ob_tid));
    a.jcc(Cc::NE, cshared);
    a.sub32_ri(R11, 1);
    a.store32(M(Rdi, ob_ref_local), R11);
    a.jcc(Cc::NE, cdone);
    a.call_label(free_num);
    a.jmp(cdone);
    a.bind(cshared);
    a.call_abs(addr(super::helpers::h_decref_shared as usize));
    a.bind(cdone);
    a.xor_rr(Rax, Rax);
    a.bind(epi);
    a.add_ri(Rsp, 8);
    a.pop(Rbp);
    a.pop(R14);
    a.pop(R12);
    a.pop(Rbx);
    a.ret();
    // ---- deopt or exception in the callee: the interpreter finishes the activation
    a.bind(not_ret);
    a.mov_rr(Rdi, Rbx);
    a.mov_rr(Rsi, Rax);
    a.mov_rr(Rdx, R12);
    a.call_abs(addr(h_callpy_finish as usize));
    a.add32_mi(M(R13, ts_depth), -1);
    a.jmp(epi);
    // ---- returned, but the frame needs the general release (frame object, ...)
    a.bind(slow_rec);
    a.mov_rr(Rdi, Rbx);
    a.call_abs(addr(h_callpy_recycle as usize));
    a.jmp(result);
    // ---- not applicable: the Rust path (nothing was changed)
    a.bind(slow);
    a.jmp_abs(addr(super::h_call_py as usize));
    let (bi, bf) = (a.label(), a.label());
    super::tier1::emit_routines(&mut a, bi, bf, free_num);

    a.resolve().ok()?;
    let _ = (f_func, epi);
    super::mem::install(&a.buf, |_, _| {})
}

/// The tier-2 call countdown of the callee expired: compile it; returns the new
/// entry address (0: keep the current one).
unsafe extern "C" fn h_callpy_compile(frame: *mut Frame) -> usize {
    unsafe {
        let f = &*frame;
        super::opt::compile(&*f.code, f.globals, f.builtins, 0).unwrap_or(0)
    }
}

/// The callee deoptimised or raised (status `st`): the interpreter finishes it, the
/// result (or error) goes to the caller's slot like `h_call_py`.
unsafe extern "C" fn h_callpy_finish(frame: *mut Frame, st: u32, base: *mut *mut PyObject) -> u64 {
    unsafe {
        let ts = crate::threads::current();
        let r = super::finish_in_interp(ts, Box::from_raw(frame), st);
        let callable = std::mem::replace(&mut *base, std::ptr::null_mut());
        crate::object::decref(callable);
        match r {
            Ok(v) => {
                *base = v.into_raw();
                super::helpers::OK
            }
            Err(_) => super::helpers::ERR,
        }
    }
}

/// General release of a returned call frame (`frame::recycle_call`).
unsafe extern "C" fn h_callpy_recycle(frame: *mut Frame) {
    unsafe { crate::interp::frame::recycle_call(Box::from_raw(frame)) }
}

/// The pool is full: free the frame.
unsafe extern "C" fn h_callpy_drop(frame: *mut Frame) {
    unsafe {
        let mut f = Box::from_raw(frame);
        f.code = std::ptr::null_mut();
        f.func = std::ptr::null_mut();
        f.globals = std::ptr::null_mut();
        f.builtins = std::ptr::null_mut();
        drop(f);
    }
}
