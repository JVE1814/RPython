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
//! (refs1) The arguments must be owned references: callers own borrowed (tagged) stack
//! entries first (tier1.rs, Call and the property getter path).

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
    a.call_abs(tramp as usize);
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
    super::mem::install_asm(&a)
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

// ---------------------------------------------------------------- shared call sites (perf8)

/// (perf8) Shared machine code of a `Call`/`CallKw` site, like HotSpot's call stubs:
/// the per-site code is `lasti = pc; rdi = &slot[base]; esi = argc; call dispatch;
/// test eax, eax; jne error`, and everything else (the checks for a Python function
/// with a fast entry, bound-method unpacking, `type(x)`, the generic call with its
/// stack_top bookkeeping and the eval-breaker poll after it) is one routine per
/// variant instead of ~250 bytes at every site.
///
/// Contract: rdi = base of [callable, self_or_null, args..(, kwnames)] in the
/// caller's stack slots (the callable owned and untagged), rsi = argument count
/// without self (and without kwnames), rbx/r12/r13/r15 as in compiled code, f.lasti
/// set, rsp 16-byte aligned at the call. Returns eax = 0 (result in slot[base]) or 1
/// (exception set; nothing left above base: the entries were consumed, a result the
/// poll would have kept is released). Clobbers the caller-saved registers.
/// The tagged variants accept borrowed (refs1) self/argument entries.
pub fn dispatch(kind: Dispatch) -> usize {
    static D: [std::sync::OnceLock<usize>; 4] = [const { std::sync::OnceLock::new() }; 4];
    *D[kind as usize].get_or_init(|| build_dispatch(kind).unwrap_or(0))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Dispatch {
    Call = 0,
    CallTagged = 1,
    CallKw = 2,
    CallKwTagged = 3,
}

/// Release the call result left at `*base` (the eval-breaker poll after a C call
/// raised: the error exit's depth does not include it).
unsafe extern "C" fn h_drop_result(base: *mut *mut PyObject) {
    unsafe {
        let r = std::ptr::replace(base, std::ptr::null_mut());
        crate::object::xdecref(r);
    }
}

fn build_dispatch(kind: Dispatch) -> Option<usize> {
    use super::helpers as h;
    use super::x64::{Asm, Label, R};
    use crate::ffi::{PyObject as O, METH_FASTCALL, METH_KEYWORDS, METH_NOARGS, METH_O};
    let tagged = matches!(kind, Dispatch::CallTagged | Dispatch::CallKwTagged);
    let kw = matches!(kind, Dispatch::CallKw | Dispatch::CallKwTagged);
    let ob_type = offset_of!(O, ob_type) as i32;
    let ob_tid = offset_of!(O, ob_tid) as i32;
    let ob_ref_local = offset_of!(O, ob_ref_local) as i32;
    use crate::types::funcobject::PyFunctionObject as F;
    let fn_code = offset_of!(F, func_code) as i32;
    let fn_vc = offset_of!(F, vectorcall) as i32;
    let code_data = offset_of!(crate::types::code::PyCodeObject, data) as i32;
    let cd_entry = (offset_of!(CodeData, caches) + offset_of!(crate::interp::cache::CodeCaches, fast_entry)) as i32;
    let stack_top = offset_of!(Frame, stack_top) as i32;
    let eval_breaker = (offset_of!(ThreadState, c) + offset_of!(CThreadState, eval_breaker)) as i32;
    let func_t = crate::types::funcobject::function_type() as i64;
    let meth_t = crate::types::funcobject::method_type() as i64;
    let type_t = crate::types::typeobject::type_type() as i64;
    let cfunc_t = (&raw mut crate::types::cfunction::PyCFunction_Type) as i64;
    let mdescr_t = (&raw mut crate::types::descr::PyMethodDescr_Type) as i64;
    let m_ml = offset_of!(crate::ffi::PyCFunctionObject, m_ml) as i32;
    let m_self = offset_of!(crate::ffi::PyCFunctionObject, m_self) as i32;
    let d_type = (offset_of!(crate::ffi::PyMethodDescrObject, d_common) + offset_of!(crate::ffi::PyDescrObject, d_type)) as i32;
    let d_method = offset_of!(crate::ffi::PyMethodDescrObject, d_method) as i32;
    let ml_meth = offset_of!(crate::ffi::PyMethodDef, ml_meth) as i32;
    let ml_flags = offset_of!(crate::ffi::PyMethodDef, ml_flags) as i32;
    let callpy = routine();

    // inline owner incref of the object in r (not r11), slow path out of line
    fn incref(a: &mut Asm, r: R, stubs: &mut Vec<(Label, Label, R)>) {
        let done = a.label();
        let slow = a.label();
        super::tier1::emit_incref_fast(a, r, done, slow);
        a.bind(done);
        stubs.push((slow, done, r));
    }
    // (tagged variants) own the borrowed entries among self and the arguments: untag
    // and incref in place (self may be NULL). Preserves rdi/rsi; clobbers rax, r8, r9,
    // r11.
    fn own_entries(a: &mut Asm, stubs: &mut Vec<(Label, Label, R)>) {
        let lp = a.label();
        let next = a.label();
        let done = a.label();
        a.lea(R8, M(Rdi, 8));
        a.lea_idx(R9, Rdi, Rsi, 8, 16);
        a.bind(lp);
        a.cmp_rr(R8, R9);
        a.jcc(Cc::AE, done);
        a.load(Rax, M(R8, 0));
        a.test_ri(Rax, 1);
        a.jcc(Cc::E, next);
        a.and_ri(Rax, -2);
        a.store(M(R8, 0), Rax);
        incref(a, Rax, stubs);
        a.bind(next);
        a.add_ri(R8, 8);
        a.jmp(lp);
        a.bind(done);
    }

    let mut a = Asm::new();
    let mut stubs: Vec<(Label, Label, R)> = Vec::new();
    let generic = a.label();
    // after a successful C call (result in slot[base], frame pushed): poll, return 0
    let after_ok = a.label();
    let out = a.label();
    // frame of the calling paths: [rsp] = argc, [rsp+8] = base, [rsp+16] = saved r14;
    // r14 = the caller's stack_top while stack_top is the call's top
    let extra = if kw { 3 } else { 2 };
    let enter_frame = |a: &mut Asm| {
        a.push(R14);
        a.push(Rdi);
        a.push(Rsi);
        a.load(R14, M(Rbx, stack_top));
        a.mov_rr(Rax, Rdi);
        a.sub_rr(Rax, R12);
        a.sar_ri(Rax, 3);
        a.add_rr(Rax, Rsi);
        a.add_ri(Rax, extra);
        a.store(M(Rbx, stack_top), Rax);
    };
    if !kw {
        let is_func = a.label();
        let not_type = a.label();
        a.load(Rax, M(Rdi, 0));
        a.mov_ri(Rcx, func_t);
        a.cmp_rm(Rcx, M(Rax, ob_type));
        a.jcc(Cc::E, is_func);
        // type(x): the argument's type (CPython's CALL_TYPE_1)
        a.cmp_ri(Rsi, 1);
        a.jcc(Cc::NE, not_type);
        a.mov_ri(Rcx, type_t);
        a.cmp_rr(Rax, Rcx);
        a.jcc(Cc::NE, not_type);
        a.cmp_mi(M(Rdi, 8), 0);
        a.jcc(Cc::NE, not_type);
        a.load(Rdx, M(Rdi, 16));
        a.mov_rr(Rcx, Rdx);
        if tagged {
            a.and_ri(Rcx, -2);
        }
        a.load(Rax, M(Rcx, ob_type));
        incref(&mut a, Rax, &mut stubs);
        a.store(M(Rdi, 0), Rax);
        a.store_i(M(Rdi, 16), 0);
        // release the argument (unless borrowed); the `type` callable is immortal
        if !unsafe { (*(type_t as *mut O)).is_immortal() } {
            return None;
        }
        let rel_done = a.label();
        let rel_slow = a.label();
        if tagged {
            a.test_ri(Rdx, 1);
            a.jcc(Cc::NE, rel_done);
        }
        a.cmp32_mi(M(Rcx, ob_ref_local), -1);
        a.jcc_short(Cc::E, rel_done);
        a.cmp_rm(R15, M(Rcx, ob_tid));
        a.jcc(Cc::NE, rel_slow);
        a.cmp32_mi(M(Rcx, ob_ref_local), 1);
        a.jcc(Cc::E, rel_slow);
        a.sub32_mi(M(Rcx, ob_ref_local), 1);
        a.bind(rel_done);
        a.xor_rr(Rax, Rax);
        a.ret();
        a.bind(rel_slow);
        // (rsp is 8 off alignment at entry: one push aligns it)
        a.push(Rdi);
        a.mov_rr(Rdi, Rcx);
        a.call_abs(h::h_decref as usize);
        a.pop(Rdi);
        a.xor_rr(Rax, Rax);
        a.ret();
        a.bind(not_type);
        // ---- (perf8) builtin functions and method descriptors with the common C
        // calling conventions: their C function directly (h_call's call_builtin_fast)
        let cfn = a.label();
        let not_c = a.label();
        let ccall = a.label();
        a.mov_ri(Rcx, cfunc_t);
        a.cmp_rm(Rcx, M(Rax, ob_type));
        a.jcc(Cc::E, cfn);
        a.mov_ri(Rcx, mdescr_t);
        a.cmp_rm(Rcx, M(Rax, ob_type));
        a.jcc(Cc::NE, not_c);
        // method descriptor: self present, of the descriptor's exact type. (Borrowed
        // entries are passed untagged, not owned: the callee sees the true reference
        // counts, as through h_call_t; the release below skips them.)
        a.load(R8, M(Rdi, 8));
        a.test_rr(R8, R8);
        a.jcc(Cc::E, generic);
        if tagged {
            a.and_ri(R8, -2);
        }
        a.load(Rcx, M(Rax, d_type));
        a.cmp_rm(Rcx, M(R8, ob_type));
        a.jcc(Cc::NE, generic);
        a.load(Rdx, M(Rax, d_method));
        a.lea(R9, M(Rdi, 16));
        a.mov_rr(R10, Rsi);
        a.jmp(ccall);
        a.bind(cfn);
        a.load(Rdx, M(Rax, m_ml));
        a.load(R8, M(Rax, m_self));
        a.lea(R9, M(Rdi, 16));
        a.mov_rr(R10, Rsi);
        a.cmp_mi(M(Rdi, 8), 0);
        a.jcc(Cc::E, ccall);
        a.sub_ri(R9, 8);
        a.add_ri(R10, 1);
        // rdx = method def, r8 = self, r9 = args, r10 = nargs
        a.bind(ccall);
        a.load(R11, M(Rdx, ml_meth));
        a.test_rr(R11, R11);
        a.jcc(Cc::E, generic);
        a.load32(Rcx, M(Rdx, ml_flags));
        let (c_o, c_noargs, c_fast, c_call, c_done) = (a.label(), a.label(), a.label(), a.label(), a.label());
        let c_after = a.label();
        a.cmp32_ri(Rcx, METH_O);
        a.jcc(Cc::E, c_o);
        a.cmp32_ri(Rcx, METH_NOARGS);
        a.jcc(Cc::E, c_noargs);
        a.cmp32_ri(Rcx, METH_FASTCALL | METH_KEYWORDS);
        a.jcc(Cc::E, c_fast);
        a.cmp32_ri(Rcx, METH_FASTCALL);
        a.jcc(Cc::NE, generic);
        // fastcall (with keywords: kwnames = NULL; ecx still tells which)
        a.bind(c_fast);
        if tagged {
            // borrowed entries: an untagged copy of the (at most 4) arguments on the
            // native stack
            let (cl, cdone) = (a.label(), a.label());
            a.cmp_ri(R10, 4);
            a.jcc(Cc::A, generic);
            enter_frame(&mut a);
            a.sub_ri(Rsp, 32);
            a.mov_rr(Rdx, Rcx);
            a.xor_rr(Rcx, Rcx);
            a.bind(cl);
            a.cmp_rr(Rcx, R10);
            a.jcc(Cc::AE, cdone);
            a.load_idx(Rax, R9, Rcx, 8, 0);
            a.and_ri(Rax, -2);
            a.store_idx(Rsp, Rcx, 8, 0, Rax);
            a.add_ri(Rcx, 1);
            a.jmp(cl);
            a.bind(cdone);
            a.mov_rr(Rcx, Rdx);
            a.mov_rr(Rsi, Rsp);
        } else {
            enter_frame(&mut a);
            a.mov_rr(Rsi, R9);
        }
        a.mov_rr(Rdi, R8);
        a.mov_rr(Rdx, R10);
        let nokw = a.label();
        a.cmp32_ri(Rcx, METH_FASTCALL);
        a.jcc_short(Cc::E, nokw);
        a.xor_rr(Rcx, Rcx);
        a.bind(nokw);
        if tagged {
            a.call_r(R11);
            a.add_ri(Rsp, 32);
            a.jmp(c_after);
        } else {
            a.jmp(c_call);
        }
        a.bind(c_o);
        a.cmp_ri(R10, 1);
        a.jcc(Cc::NE, generic);
        enter_frame(&mut a);
        a.mov_rr(Rdi, R8);
        a.load(Rsi, M(R9, 0));
        if tagged {
            a.and_ri(Rsi, -2);
        }
        a.jmp(c_call);
        a.bind(c_noargs);
        a.test_rr(R10, R10);
        a.jcc(Cc::NE, generic);
        enter_frame(&mut a);
        a.mov_rr(Rdi, R8);
        a.xor_rr(Rsi, Rsi);
        a.bind(c_call);
        a.call_r(R11);
        a.bind(c_after);
        a.store(M(Rbx, stack_top), R14);
        a.test_rr(Rax, Rax);
        let c_err = a.label();
        a.jcc(Cc::E, c_err);
        // release callable, self/NULL and the arguments, result in place
        a.bind(c_done);
        let (lp, lslow, ldone) = (a.label(), a.label(), a.label());
        a.load(Rcx, M(Rsp, 0));
        a.load(R14, M(Rsp, 8));
        a.lea_idx(R14, R14, Rcx, 8, 16);
        a.store(M(Rsp, 0), Rax);
        a.bind(lp);
        a.cmp_rm(R14, M(Rsp, 8));
        a.jcc(Cc::BE, ldone);
        a.sub_ri(R14, 8);
        a.load(Rdi, M(R14, 0));
        a.store_i(M(R14, 0), 0);
        a.test_rr(Rdi, Rdi);
        a.jcc(Cc::E, lp);
        if tagged {
            // borrowed entries are not released
            a.test_ri(Rdi, 1);
            a.jcc(Cc::NE, lp);
        }
        a.cmp32_mi(M(Rdi, ob_ref_local), -1);
        a.jcc(Cc::E, lp);
        a.cmp_rm(R15, M(Rdi, ob_tid));
        a.jcc(Cc::NE, lslow);
        a.cmp32_mi(M(Rdi, ob_ref_local), 1);
        a.jcc(Cc::E, lslow);
        a.sub32_mi(M(Rdi, ob_ref_local), 1);
        a.jmp(lp);
        a.bind(lslow);
        a.call_abs(h::h_decref as usize);
        a.jmp(lp);
        a.bind(ldone);
        a.load(Rax, M(Rsp, 0));
        a.load(Rcx, M(Rsp, 8));
        a.store(M(Rcx, 0), Rax);
        a.jmp(after_ok);
        // NULL result: SystemError unless an exception is set; entries released
        a.bind(c_err);
        a.load(Rdi, M(Rsp, 8));
        a.load(Rsi, M(Rsp, 0));
        a.call_abs(h_cfunc_null as usize);
        a.mov_ri(Rax, 1);
        a.jmp(out);
        a.bind(not_c);
        // bound method with a free self slot: unpack it in the stack, retry
        a.mov_ri(Rcx, meth_t);
        a.cmp_rm(Rcx, M(Rax, ob_type));
        a.jcc(Cc::NE, generic);
        a.cmp_mi(M(Rdi, 8), 0);
        a.jcc(Cc::NE, generic);
        a.push(Rdi);
        a.push(Rsi);
        a.sub_ri(Rsp, 8);
        a.call_abs(h::h_unpack_method as usize);
        a.add_ri(Rsp, 8);
        a.pop(Rsi);
        a.pop(Rdi);
        a.load(Rax, M(Rdi, 0));
        a.mov_ri(Rcx, func_t);
        a.cmp_rm(Rcx, M(Rax, ob_type));
        a.jcc(Cc::NE, generic);
        a.bind(is_func);
        // a replaced vectorcall (C API) must be honoured; the code body may still be
        // lazy (perf2); no published fast entry: generic path
        a.mov_ri(Rcx, crate::interp::function_vectorcall as usize as i64);
        a.cmp_rm(Rcx, M(Rax, fn_vc));
        a.jcc(Cc::NE, generic);
        a.load(Rcx, M(Rax, fn_code));
        a.test_rr(Rcx, Rcx);
        a.jcc(Cc::E, generic);
        a.load(Rcx, M(Rcx, code_data));
        a.test_rr(Rcx, Rcx);
        a.jcc(Cc::E, generic);
        a.cmp_mi(M(Rcx, cd_entry), 0);
        a.jcc(Cc::E, generic);
        if tagged {
            // the callee frame owns its arguments (rcx survives own_entries)
            own_entries(&mut a, &mut stubs);
        }
        a.mov_rr(Rdx, Rcx);
        if callpy != 0 {
            a.jmp_abs_via(callpy, R11);
        } else {
            a.jmp_abs_via(super::h_call_py as usize, R11);
        }
    }
    // ---- generic: h_call(&slot[top], argc, kw) with stack_top = the call's top
    a.bind(generic);
    let poll = a.label();
    enter_frame(&mut a);
    a.lea_idx(Rdi, Rdi, Rsi, 8, 8 * extra);
    a.mov_ri(Rdx, kw as i64);
    a.call_abs(if tagged { h::h_call_t as usize } else { h::h_call as usize });
    a.store(M(Rbx, stack_top), R14);
    a.test_rr(Rax, Rax);
    a.jcc(Cc::NE, out);
    // C calls: observe signals, pending calls and safepoints right after (Python
    // callees poll at their own Resume)
    a.bind(after_ok);
    a.load(Rax, M(R13, eval_breaker));
    a.test_rr(Rax, Rax);
    a.jcc(Cc::NE, poll);
    a.bind(out);
    a.pop(Rsi);
    a.pop(Rdi);
    a.pop(R14);
    a.ret();
    a.bind(poll);
    a.load(Rax, M(Rsp, 8));
    a.sub_rr(Rax, R12);
    a.sar_ri(Rax, 3);
    a.add_ri(Rax, 1);
    a.store(M(Rbx, stack_top), Rax);
    a.mov_rr(Rdi, R13);
    a.call_abs(h::h_eval_breaker as usize);
    a.store(M(Rbx, stack_top), R14);
    a.test_rr(Rax, Rax);
    a.jcc(Cc::E, out);
    a.load(Rdi, M(Rsp, 8));
    a.call_abs(h_drop_result as usize);
    a.mov_ri(Rax, 1);
    a.jmp(out);
    for (slow, done, r) in stubs {
        a.bind(slow);
        super::tier1::emit_nonowner_incref(&mut a, r, done);
    }
    a.resolve().ok()?;
    super::mem::install_asm(&a)
}

/// (perf8) A C function called by `dispatch` returned NULL: SystemError unless an
/// exception is set (as `call_builtin_fast`), then release the call's entries
/// [callable, self_or_null, args..] (borrowed ones are not released).
unsafe extern "C" fn h_cfunc_null(base: *mut *mut PyObject, n: u64) {
    unsafe {
        let callable = *base;
        if !crate::errors::err_occurred() {
            let _ = crate::err!(SystemError, "{} returned NULL without setting an exception", crate::types::typeobject::type_name((*callable).ty()));
        }
        for k in (0..n as usize + 2).rev() {
            let v = std::ptr::replace(base.add(k), std::ptr::null_mut());
            crate::interp::borrow::release(v);
        }
    }
}
