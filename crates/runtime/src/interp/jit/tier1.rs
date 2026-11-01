//! Tier 1: baseline ("template") JIT. Each bytecode instruction becomes a fixed
//! x86-64 sequence: stack/local traffic and reference counting inline, everything
//! else through runtime helpers (helpers.rs), plus inline fast paths for the common
//! exact-type cases (compact int arithmetic/compares, bool branches).
//!
//! Frame layout is the interpreter's: values live in `Frame::slots` (localsplus then
//! value stack) at statically known depths, so tier 1 can enter at any loop header /
//! handler / resume point and leave to the interpreter at any instruction boundary
//! (deoptimisation is trivial: store `next_instr` and `stack_top`).
//!
//! Register assignment (callee-saved, valid in all generated code):
//!   rbx = *mut Frame, r12 = slots base, r13 = *const ThreadState,
//!   r15 = current thread id (fs:[0], for the biased-refcount fast path),
//!   r14 = scratch preserved across helper calls.
//! Native frame: [rsp] = out pointer for the return value.
//!
//! Entry: fn(frame, slots, ts, target_address, out) -> status
//!   status 0 = returned (*out = value), 1 = exception raised (f.lasti, f.stack_top
//!   set; the interpreter unwinds), 2 = deopt (continue interpreting at f.next_instr
//!   with f.stack_top).

use super::analysis::{Analysis, Instr};
use super::helpers as h;
use super::x64::*;
use crate::ffi::*;
use crate::interp::frame::Frame;
use crate::object::*;
use crate::threads::{CThreadState, ThreadState};
use crate::types::code::CodeData;
use rpy_compiler::bytecode::{BinOp, Op};
use std::mem::offset_of;

pub const ST_RETURN: u32 = 0;
pub const ST_ERROR: u32 = 1;
pub const ST_DEOPT: u32 = 2;

const LASTI: i32 = offset_of!(Frame, lasti) as i32;
const NEXT_INSTR: i32 = offset_of!(Frame, next_instr) as i32;
const STACK_TOP: i32 = offset_of!(Frame, stack_top) as i32;
const TS_EVAL_BREAKER: i32 = (offset_of!(ThreadState, c) + offset_of!(CThreadState, eval_breaker)) as i32;
const OB_TID: i32 = offset_of!(PyObject, ob_tid) as i32;
const OB_REF_LOCAL: i32 = offset_of!(PyObject, ob_ref_local) as i32;
const OB_REF_SHARED: i32 = offset_of!(PyObject, ob_ref_shared) as i32;
const OB_TYPE: i32 = offset_of!(PyObject, ob_type) as i32;
const LV_TAG: i32 = offset_of!(PyLongObject, lv_tag) as i32;
const LV_DIGIT: i32 = offset_of!(PyLongObject, ob_digit) as i32;
const FVAL: i32 = offset_of!(PyFloatObject, ob_fval) as i32;

type Stub = Box<dyn FnOnce(&mut Asm, Label, i32)>;

pub struct Output {
    pub asm: Asm,
    /// native offset per code index (u32::MAX: not an entry point)
    pub entries: Vec<u32>,
}

struct Gen<'a> {
    a: Asm,
    nlp: i32,
    an: &'a Analysis,
    cd: &'a CodeData,
    tables: &'a crate::interp::cache::Tables,
    ilabel: Vec<Label>,
    exit: Label,
    stubs: Vec<Stub>,
}

fn fnaddr<T>(f: T) -> usize
where
    T: Copy,
{
    debug_assert!(std::mem::size_of::<T>() == 8);
    unsafe { std::mem::transmute_copy(&f) }
}

macro_rules! addr {
    ($f:expr) => {
        fnaddr($f as usize)
    };
}

impl<'a> Gen<'a> {
    fn slot(&self, d: i32) -> M {
        M(R12, 8 * (self.nlp + d))
    }
    fn local(&self, i: u32) -> M {
        M(R12, 8 * i as i32)
    }
    fn call(&mut self, addr: usize) {
        self.a.call_abs(addr);
    }
    fn set_lasti(&mut self, pc: u32) {
        self.a.store32_i(M(Rbx, LASTI), pc as i32);
    }
    fn lea_sp(&mut self, r: R, depth: i32) {
        let m = self.slot(depth);
        self.a.lea(r, m);
    }

    /// Out-of-line error exit with the given remaining stack depth.
    fn error_label(&mut self, pc: u32, depth: i32) -> Label {
        let l = self.a.label();
        self.stubs.push(Box::new(move |a: &mut Asm, exit, nlp| {
            a.bind(l);
            a.store32_i(M(Rbx, LASTI), pc as i32);
            a.store_i(M(Rbx, STACK_TOP), nlp + depth);
            a.mov_ri(Rax, ST_ERROR as i64);
            a.jmp(exit);
        }));
        l
    }
    /// Branch to an error exit if eax != 0.
    fn check(&mut self, pc: u32, depth: i32) {
        let l = self.error_label(pc, depth);
        self.a.test_rr(Rax, Rax);
        self.a.jcc(Cc::NE, l);
    }
    fn deopt(&mut self, start: u32, depth: i32) {
        self.a.store32_i(M(Rbx, NEXT_INSTR), start as i32);
        self.a.store_i(M(Rbx, STACK_TOP), self.nlp + depth);
        self.a.mov_ri(Rax, ST_DEOPT as i64);
        self.a.jmp(self.exit);
    }

    /// incref of the object in `r` (non-null). Clobbers r11.
    fn incref(&mut self, r: R) {
        let done = self.a.label();
        let slow = self.a.label();
        self.a.load32(R11, M(r, OB_REF_LOCAL));
        self.a.add32_ri(R11, 1);
        self.a.jcc(Cc::E, done);
        self.a.cmp_rm(R15, M(r, OB_TID));
        self.a.jcc(Cc::NE, slow);
        self.a.store32(M(r, OB_REF_LOCAL), R11);
        self.a.bind(done);
        self.stubs.push(Box::new(move |a: &mut Asm, _exit, _nlp| {
            a.bind(slow);
            a.lock_add_mi(M(r, OB_REF_SHARED), 1 << REF_SHARED_SHIFT);
            a.jmp(done);
        }));
    }
    fn xincref(&mut self, r: R) {
        let skip = self.a.label();
        self.a.test_rr(r, r);
        self.a.jcc(Cc::E, skip);
        self.incref(r);
        self.a.bind(skip);
    }
    /// decref of the object in `r` (non-null). Clobbers r11 and, on the slow path,
    /// all caller-saved registers.
    fn decref(&mut self, r: R) {
        let done = self.a.label();
        let shared = self.a.label();
        let zero = self.a.label();
        self.a.load32(R11, M(r, OB_REF_LOCAL));
        self.a.cmp32_ri(R11, -1);
        self.a.jcc(Cc::E, done);
        self.a.cmp_rm(R15, M(r, OB_TID));
        self.a.jcc(Cc::NE, shared);
        self.a.sub32_ri(R11, 1);
        self.a.store32(M(r, OB_REF_LOCAL), R11);
        self.a.jcc(Cc::E, zero);
        self.a.bind(done);
        self.stubs.push(Box::new(move |a: &mut Asm, _exit, _nlp| {
            a.bind(shared);
            a.mov_rr(Rdi, r);
            a.call_abs(addr!(h::h_decref_shared));
            a.jmp(done);
            a.bind(zero);
            a.mov_rr(Rdi, r);
            a.call_abs(addr!(h::h_merge_zero));
            a.jmp(done);
        }));
    }
    fn xdecref(&mut self, r: R) {
        let skip = self.a.label();
        self.a.test_rr(r, r);
        self.a.jcc(Cc::E, skip);
        self.decref(r);
        self.a.bind(skip);
    }

    /// Eval-breaker poll (safepoints, signals, GC) at depth `depth`.
    fn poll(&mut self, pc: u32, depth: i32) {
        let slow = self.a.label();
        let back = self.a.label();
        self.a.load(Rax, M(R13, TS_EVAL_BREAKER));
        self.a.test_rr(Rax, Rax);
        self.a.jcc(Cc::NE, slow);
        self.a.bind(back);
        let err = self.error_label(pc, depth);
        self.stubs.push(Box::new(move |a: &mut Asm, _exit, nlp| {
            a.bind(slow);
            a.store32_i(M(Rbx, LASTI), pc as i32);
            a.store_i(M(Rbx, STACK_TOP), nlp + depth);
            a.mov_rr(Rdi, R13);
            a.call_abs(addr!(h::h_eval_breaker));
            a.test_rr(Rax, Rax);
            a.jcc(Cc::NE, err);
            a.jmp(back);
        }));
    }

    /// Call `helper(sp, a1, a2)` with sp = &slot[depth]; on failure exit with err_depth.
    fn helper_sp(&mut self, ins: &Instr, depth: i32, helper: usize, a1: Option<i64>, a2: Option<i64>, err_depth: i32) {
        self.set_lasti(ins.pc);
        self.lea_sp(Rdi, depth);
        if let Some(v) = a1 {
            self.a.mov_ri(Rsi, v);
        }
        if let Some(v) = a2 {
            self.a.mov_ri(Rdx, v);
        }
        self.call(helper);
        self.check(ins.pc, err_depth);
    }

    /// Load compact-int value of the object in `obj` into `out` or jump to `slow`.
    /// Clobbers `tmp`.
    fn compact_int(&mut self, obj: R, out: R, tmp: R, slow: Label) {
        let int_t = crate::types::int::int_type() as i64;
        self.a.mov_ri(tmp, int_t);
        self.a.cmp_rm(tmp, M(obj, OB_TYPE));
        self.a.jcc(Cc::NE, slow);
        self.a.load(tmp, M(obj, LV_TAG));
        self.a.cmp_ri(tmp, 16);
        self.a.jcc(Cc::AE, slow);
        self.a.load32(out, M(obj, LV_DIGIT));
        // sign = 1 - (tag & 3): 0 -> +, 1 -> 0 (digit is 0 anyway), 2 -> -
        self.a.and_ri(tmp, 2);
        let pos = self.a.label();
        self.a.jcc(Cc::E, pos);
        self.a.neg(out);
        self.a.bind(pos);
    }

    fn emit(&mut self, i: usize) -> Result<(), String> {
        let ins = self.an.instrs[i];
        let d = self.an.depth[i];
        let pc = ins.pc;
        let arg = ins.arg;
        match ins.op {
            Op::Nop | Op::ExtendedArg => {}
            Op::Resume => self.poll(pc, d),
            Op::PopTop => {
                let m = self.slot(d - 1);
                self.a.load(Rax, m);
                self.xdecref(Rax);
            }
            Op::PushNull => {
                let m = self.slot(d);
                self.a.store_i(m, 0);
            }
            Op::Copy => {
                let m = self.slot(d - arg as i32);
                self.a.load(Rax, m);
                self.xincref(Rax);
                let m = self.slot(d);
                self.a.store(m, Rax);
            }
            Op::Swap => {
                let (m1, m2) = (self.slot(d - 1), self.slot(d - arg as i32));
                self.a.load(Rax, m1);
                self.a.load(Rcx, m2);
                self.a.store(m1, Rcx);
                self.a.store(m2, Rax);
            }
            Op::LoadConst => {
                let c = &self.cd.consts[arg as usize];
                self.a.mov_ri(Rax, c.as_ptr() as i64);
                if !c.is_immortal() {
                    self.incref(Rax);
                }
                let m = self.slot(d);
                self.a.store(m, Rax);
            }
            Op::LoadFast | Op::LoadFastCheck => {
                let l = self.local(arg);
                self.a.load(Rax, l);
                let unbound = self.a.label();
                self.a.test_rr(Rax, Rax);
                self.a.jcc(Cc::E, unbound);
                self.incref(Rax);
                let m = self.slot(d);
                self.a.store(m, Rax);
                let err = self.error_label(pc, d);
                self.stubs.push(Box::new(move |a: &mut Asm, _exit, _nlp| {
                    a.bind(unbound);
                    a.store32_i(M(Rbx, LASTI), pc as i32);
                    a.mov_rr(Rdi, Rbx);
                    a.mov_ri(Rsi, arg as i64);
                    a.call_abs(addr!(h::h_unbound_local));
                    a.jmp(err);
                }));
            }
            Op::LoadFastAndClear => {
                let l = self.local(arg);
                self.a.load(Rax, l);
                self.a.store_i(l, 0);
                let m = self.slot(d);
                self.a.store(m, Rax);
            }
            Op::StoreFast => {
                let m = self.slot(d - 1);
                let l = self.local(arg);
                self.a.load(Rax, m);
                self.a.load(Rcx, l);
                self.a.store(l, Rax);
                self.xdecref(Rcx);
            }
            Op::DeleteFast => {
                let l = self.local(arg);
                self.a.load(Rax, l);
                let unbound = self.a.label();
                self.a.test_rr(Rax, Rax);
                self.a.jcc(Cc::E, unbound);
                self.a.store_i(l, 0);
                self.decref(Rax);
                let err = self.error_label(pc, d);
                self.stubs.push(Box::new(move |a: &mut Asm, _exit, _nlp| {
                    a.bind(unbound);
                    a.mov_rr(Rdi, Rbx);
                    a.mov_ri(Rsi, arg as i64);
                    a.call_abs(addr!(h::h_unbound_local));
                    a.jmp(err);
                }));
            }
            Op::ReturnValue => {
                let m = self.slot(d - 1);
                self.a.load(Rax, m);
                self.a.load(Rcx, M(Rsp, 0));
                self.a.store(M(Rcx, 0), Rax);
                self.a.store_i(M(Rbx, STACK_TOP), self.nlp + d - 1);
                self.a.mov_ri(Rax, ST_RETURN as i64);
                self.a.jmp(self.exit);
            }
            Op::Jump => {
                let t = self.an.index_of[arg as usize] as usize;
                if arg <= ins.start {
                    self.poll(pc, d);
                }
                let l = self.ilabel[t];
                self.a.jmp(l);
            }
            Op::PopJumpIfFalse | Op::PopJumpIfTrue => {
                let t = self.ilabel[self.an.index_of[arg as usize] as usize];
                let fall = self.a.label();
                let slow = self.a.label();
                let jump_if_true = ins.op == Op::PopJumpIfTrue;
                let m = self.slot(d - 1);
                self.a.load(Rax, m);
                self.a.mov_ri(Rcx, crate::types::bool_::true_obj().as_ptr() as i64);
                self.a.cmp_rr(Rax, Rcx);
                if jump_if_true {
                    self.a.jcc(Cc::E, t);
                } else {
                    self.a.jcc(Cc::E, fall);
                }
                self.a.mov_ri(Rcx, crate::types::bool_::false_obj().as_ptr() as i64);
                self.a.cmp_rr(Rax, Rcx);
                if jump_if_true {
                    self.a.jcc(Cc::E, fall);
                } else {
                    self.a.jcc(Cc::E, t);
                }
                self.a.jmp(slow);
                self.a.bind(fall);
                let err = self.error_label(pc, d - 1);
                let nlp = self.nlp;
                self.stubs.push(Box::new(move |a: &mut Asm, _exit, _nlp| {
                    a.bind(slow);
                    a.store32_i(M(Rbx, LASTI), pc as i32);
                    a.lea(Rdi, M(R12, 8 * (nlp + d)));
                    a.call_abs(addr!(h::h_truth_pop));
                    a.cmp_ri(Rax, 2);
                    a.jcc(Cc::E, err);
                    a.test_rr(Rax, Rax);
                    if jump_if_true {
                        a.jcc(Cc::NE, t);
                    } else {
                        a.jcc(Cc::E, t);
                    }
                    a.jmp(fall);
                }));
            }
            Op::PopJumpIfNone | Op::PopJumpIfNotNone => {
                let t = self.ilabel[self.an.index_of[arg as usize] as usize];
                let m = self.slot(d - 1);
                self.a.load(Rax, m);
                self.a.mov_ri(Rcx, crate::types::none::none().as_ptr() as i64);
                self.a.cmp_rr(Rax, Rcx);
                if ins.op == Op::PopJumpIfNone {
                    self.a.jcc(Cc::E, t);
                    self.decref(Rax);
                } else {
                    let fall = self.a.label();
                    self.a.jcc(Cc::E, fall);
                    self.decref(Rax);
                    self.a.jmp(t);
                    self.a.bind(fall);
                }
            }
            Op::JumpIfFalseOrPop | Op::JumpIfTrueOrPop => {
                let t = self.ilabel[self.an.index_of[arg as usize] as usize];
                let jump_if_true = ins.op == Op::JumpIfTrueOrPop;
                self.set_lasti(pc);
                self.lea_sp(Rdi, d);
                self.call(addr!(h::h_truth_peek));
                let err = self.error_label(pc, d);
                self.a.cmp_ri(Rax, 2);
                self.a.jcc(Cc::E, err);
                self.a.test_rr(Rax, Rax);
                self.a.jcc(if jump_if_true { Cc::NE } else { Cc::E }, t);
                let m = self.slot(d - 1);
                self.a.load(Rax, m);
                self.decref(Rax);
            }
            Op::LoadGlobal => {
                let name = self.cd.names[arg as usize].as_ptr() as i64;
                let e = self.tables.entry(pc as usize) as *const _ as i64;
                self.set_lasti(pc);
                self.a.mov_rr(Rdi, Rbx);
                self.lea_sp(Rsi, d);
                self.a.mov_ri(Rdx, name);
                self.a.mov_ri(Rcx, e);
                self.call(addr!(h::h_load_global));
                self.check(pc, d);
            }
            Op::LoadAttr | Op::LoadMethod | Op::StoreAttr => {
                let name = self.cd.names[arg as usize].as_ptr() as i64;
                let e = self.tables.entry(pc as usize) as *const _ as i64;
                let (hf, errd) = match ins.op {
                    Op::LoadAttr => (addr!(h::h_load_attr), d - 1),
                    Op::LoadMethod => (addr!(h::h_load_method), d - 1),
                    _ => (addr!(h::h_store_attr), d - 2),
                };
                self.helper_sp(&ins, d, hf, Some(name), Some(e), errd);
            }
            Op::DeleteAttr => {
                let name = self.cd.names[arg as usize].as_ptr() as i64;
                self.helper_sp(&ins, d, addr!(h::h_delete_attr), Some(name), None, d - 1);
            }
            Op::StoreGlobal => {
                let name = self.cd.names[arg as usize].as_ptr() as i64;
                self.set_lasti(pc);
                self.a.mov_rr(Rdi, Rbx);
                self.lea_sp(Rsi, d);
                self.a.mov_ri(Rdx, name);
                self.call(addr!(h::h_store_global));
                self.check(pc, d - 1);
            }
            Op::LoadDeref | Op::StoreDeref => {
                self.set_lasti(pc);
                self.a.mov_rr(Rdi, Rbx);
                self.lea_sp(Rsi, d);
                self.a.mov_ri(Rdx, arg as i64);
                let (hf, errd) =
                    if ins.op == Op::LoadDeref { (addr!(h::h_load_deref), d) } else { (addr!(h::h_store_deref), d - 1) };
                self.call(hf);
                self.check(pc, errd);
            }
            Op::MakeCell | Op::CopyFreeVars => {
                self.a.mov_rr(Rdi, Rbx);
                self.a.mov_ri(Rsi, arg as i64);
                let hf = if ins.op == Op::MakeCell { addr!(h::h_make_cell) } else { addr!(h::h_copy_free_vars) };
                self.set_lasti(pc);
                self.call(hf);
                self.check(pc, d);
            }
            Op::BinaryOp => self.binary_op(&ins, d)?,
            Op::CompareOp => {
                let next = self.an.instrs.get(i + 1).copied();
                let fuse = arg & 16 != 0
                    && matches!(next.map(|n| n.op), Some(Op::PopJumpIfFalse | Op::PopJumpIfTrue))
                    && !self.an.is_target[i + 1];
                if fuse {
                    self.compare_jump(&ins, &next.unwrap(), d)?;
                    return Ok(());
                }
                self.helper_sp(&ins, d, addr!(h::h_compare), Some(arg as i64), None, d - 2);
            }
            Op::IsOp => {
                // inline: pointer compare, produce bool, decref both
                let (ma, mb) = (self.slot(d - 2), self.slot(d - 1));
                self.a.load(Rax, ma);
                self.a.cmp_rm(Rax, mb);
                self.a.setcc(if arg == 1 { Cc::NE } else { Cc::E }, R14);
                self.decref(Rax);
                self.a.load(Rax, mb);
                self.decref(Rax);
                self.a.mov_ri(Rax, crate::types::bool_::false_obj().as_ptr() as i64);
                self.a.mov_ri(Rcx, crate::types::bool_::true_obj().as_ptr() as i64);
                self.a.test_rr(R14, R14);
                self.a.cmov(Cc::NE, Rax, Rcx);
                self.a.store(ma, Rax);
            }
            Op::ContainsOp => self.helper_sp(&ins, d, addr!(h::h_contains_op), Some(arg as i64), None, d - 2),
            Op::UnaryNegative => self.helper_sp(&ins, d, addr!(h::h_unary), Some(0), None, d - 1),
            Op::UnaryPositive => self.helper_sp(&ins, d, addr!(h::h_unary), Some(1), None, d - 1),
            Op::UnaryInvert => self.helper_sp(&ins, d, addr!(h::h_unary), Some(2), None, d - 1),
            Op::UnaryNot | Op::ToBool => {
                // inline for exact bools
                let slow = self.a.label();
                let done = self.a.label();
                let m = self.slot(d - 1);
                let t = crate::types::bool_::true_obj().as_ptr() as i64;
                let f = crate::types::bool_::false_obj().as_ptr() as i64;
                self.a.load(Rax, m);
                self.a.mov_ri(Rcx, t);
                self.a.mov_ri(Rdx, f);
                self.a.cmp_rr(Rax, Rcx);
                let is_t = self.a.label();
                self.a.jcc(Cc::E, is_t);
                self.a.cmp_rr(Rax, Rdx);
                self.a.jcc(Cc::NE, slow);
                // false
                if ins.op == Op::UnaryNot {
                    self.a.store(m, Rcx);
                }
                self.a.jmp(done);
                self.a.bind(is_t);
                if ins.op == Op::UnaryNot {
                    self.a.store(m, Rdx);
                }
                self.a.jmp(done);
                self.a.bind(slow);
                let which = if ins.op == Op::UnaryNot { 3 } else { 4 };
                self.helper_sp(&ins, d, addr!(h::h_unary), Some(which), None, d - 1);
                self.a.bind(done);
            }
            Op::BinarySubscr => self.helper_sp(&ins, d, addr!(h::h_binary_subscr), None, None, d - 2),
            Op::StoreSubscr => self.helper_sp(&ins, d, addr!(h::h_store_subscr), None, None, d - 3),
            Op::DeleteSubscr => self.helper_sp(&ins, d, addr!(h::h_delete_subscr), None, None, d - 2),
            Op::BinarySlice => self.helper_sp(&ins, d, addr!(h::h_binary_slice), None, None, d - 3),
            Op::StoreSlice => self.helper_sp(&ins, d, addr!(h::h_store_slice), None, None, d - 4),
            Op::BuildSlice => self.helper_sp(&ins, d, addr!(h::h_build_slice), Some(arg as i64), None, d - arg as i32),
            Op::BuildTuple => self.helper_sp(&ins, d, addr!(h::h_build_tuple), Some(arg as i64), None, d - arg as i32),
            Op::BuildList => self.helper_sp(&ins, d, addr!(h::h_build_list), Some(arg as i64), None, d - arg as i32),
            Op::BuildSet => self.helper_sp(&ins, d, addr!(h::h_build_set), Some(arg as i64), None, d - arg as i32),
            Op::BuildMap => self.helper_sp(&ins, d, addr!(h::h_build_map), Some(arg as i64), None, d - 2 * arg as i32),
            Op::BuildString => self.helper_sp(&ins, d, addr!(h::h_build_string), Some(arg as i64), None, d - arg as i32),
            Op::ListAppend | Op::SetAdd | Op::ListExtend | Op::SetUpdate | Op::DictUpdate | Op::DictMerge => {
                let which = match ins.op {
                    Op::ListAppend => 0,
                    Op::SetAdd => 1,
                    Op::ListExtend => 3,
                    Op::SetUpdate => 4,
                    Op::DictUpdate => 5,
                    _ => 6,
                };
                self.helper_sp(&ins, d, addr!(h::h_container_op), Some(which), Some(arg as i64), d - 1);
            }
            Op::MapAdd => self.helper_sp(&ins, d, addr!(h::h_container_op), Some(2), Some(arg as i64), d - 2),
            Op::UnpackSequence => self.helper_sp(&ins, d, addr!(h::h_unpack_sequence), Some(arg as i64), None, d - 1),
            Op::UnpackEx => self.helper_sp(&ins, d, addr!(h::h_unpack_ex), Some(arg as i64), None, d - 1),
            Op::GetIter => self.helper_sp(&ins, d, addr!(h::h_get_iter), None, None, d - 1),
            Op::ForIter => {
                let t = self.ilabel[self.an.index_of[arg as usize] as usize];
                self.set_lasti(pc);
                self.lea_sp(Rdi, d);
                self.call(addr!(h::h_for_iter));
                let err = self.error_label(pc, d);
                self.a.cmp_ri(Rax, 1);
                self.a.jcc(Cc::E, t);
                self.a.jcc(Cc::A, err);
            }
            Op::Call => self.helper_sp(&ins, d, addr!(h::h_call), Some(arg as i64), Some(0), d - arg as i32 - 2),
            Op::CallKw => self.helper_sp(&ins, d, addr!(h::h_call), Some(arg as i64), Some(1), d - arg as i32 - 3),
            Op::CallFunctionEx => self.helper_sp(&ins, d, addr!(h::h_call_ex), None, None, d - 4),
            Op::CallIntrinsic1 => {
                self.set_lasti(pc);
                self.a.mov_rr(Rdi, R13);
                self.a.mov_rr(Rsi, Rbx);
                self.lea_sp(Rdx, d);
                self.a.mov_ri(Rcx, arg as i64);
                self.call(addr!(h::h_intrinsic1));
                self.check(pc, d - 1);
            }
            Op::CallIntrinsic2 => {
                self.set_lasti(pc);
                self.a.mov_rr(Rdi, Rbx);
                self.lea_sp(Rsi, d);
                self.a.mov_ri(Rdx, arg as i64);
                self.call(addr!(h::h_intrinsic2));
                self.check(pc, d - 2);
            }
            Op::MakeFunction => {
                self.set_lasti(pc);
                self.a.mov_rr(Rdi, Rbx);
                self.lea_sp(Rsi, d);
                self.call(addr!(h::h_make_function));
                self.check(pc, d - 1);
            }
            Op::SetFunctionAttribute => {
                self.helper_sp(&ins, d, addr!(h::h_set_function_attribute), Some(arg as i64), None, d - 2)
            }
            Op::FormatSimple => self.helper_sp(&ins, d, addr!(h::h_format), Some(0), None, d - 1),
            Op::FormatWithSpec => self.helper_sp(&ins, d, addr!(h::h_format), Some(1), None, d - 2),
            Op::ConvertValue => self.helper_sp(&ins, d, addr!(h::h_format), Some(arg as i64 + 1), None, d - 1),
            Op::LoadSuperAttr => {
                let name = self.cd.names[(arg >> 2) as usize].as_ptr() as i64;
                self.helper_sp(&ins, d, addr!(h::h_load_super_attr), Some(arg as i64), Some(name), d - 3);
            }
            // everything else runs in the interpreter
            _ => self.deopt(ins.start, d),
        }
        Ok(())
    }

    /// BinaryOp with inline compact-int add/sub/mul/and/or/xor and float add/sub/mul.
    fn binary_op(&mut self, ins: &Instr, d: i32) -> Result<(), String> {
        let op: BinOp = unsafe { std::mem::transmute(ins.arg as u8) };
        use BinOp::*;
        let int_ok = matches!(
            op,
            Add | InplaceAdd | Subtract | InplaceSubtract | Multiply | InplaceMultiply | And | InplaceAnd | Or | InplaceOr | Xor | InplaceXor
        );
        let float_ok = matches!(op, Add | InplaceAdd | Subtract | InplaceSubtract | Multiply | InplaceMultiply);
        if !int_ok {
            self.helper_sp(ins, d, addr!(h::h_binary_op), Some(ins.arg as i64), None, d - 2);
            return Ok(());
        }
        let (ma, mb) = (self.slot(d - 2), self.slot(d - 1));
        let slow = self.a.label();
        let not_int = self.a.label();
        let have_result = self.a.label();
        let done = self.a.label();
        self.a.load(Rax, ma);
        self.a.load(Rcx, mb);
        self.compact_int(Rax, Rdi, Rdx, not_int);
        self.compact_int(Rcx, Rsi, Rdx, slow);
        match op {
            Add | InplaceAdd => self.a.add_rr(Rdi, Rsi),
            Subtract | InplaceSubtract => self.a.sub_rr(Rdi, Rsi),
            Multiply | InplaceMultiply => self.a.imul_rr(Rdi, Rsi),
            And | InplaceAnd => self.a.and_rr(Rdi, Rsi),
            Or | InplaceOr => self.a.or_rr(Rdi, Rsi),
            _ => self.a.xor_rr(Rdi, Rsi),
        }
        self.call(addr!(h::h_new_int));
        self.a.jmp(have_result);
        self.a.bind(not_int);
        if float_ok {
            let ft = crate::types::float::float_type() as i64;
            self.a.mov_ri(Rdx, ft);
            self.a.cmp_rm(Rdx, M(Rax, OB_TYPE));
            self.a.jcc(Cc::NE, slow);
            self.a.cmp_rm(Rdx, M(Rcx, OB_TYPE));
            self.a.jcc(Cc::NE, slow);
            self.a.movsd_load(XMM0, M(Rax, FVAL));
            self.a.movsd_load(XMM1, M(Rcx, FVAL));
            match op {
                Add | InplaceAdd => self.a.addsd(XMM0, XMM1),
                Subtract | InplaceSubtract => self.a.subsd(XMM0, XMM1),
                _ => self.a.mulsd(XMM0, XMM1),
            }
            self.call(addr!(h::h_new_float));
            self.a.jmp(have_result);
        } else {
            self.a.jmp(slow);
        }
        self.a.bind(have_result);
        // result in rax: keep it in r14 while releasing the operands
        self.a.mov_rr(R14, Rax);
        self.a.load(Rax, ma);
        self.decref(Rax);
        self.a.load(Rax, mb);
        self.decref(Rax);
        self.a.store(ma, R14);
        self.a.jmp(done);
        self.a.bind(slow);
        self.helper_sp(ins, d, addr!(h::h_binary_op), Some(ins.arg as i64), None, d - 2);
        self.a.bind(done);
        Ok(())
    }

    /// CompareOp|16 immediately followed by PopJumpIfFalse/True.
    fn compare_jump(&mut self, ins: &Instr, jmp: &Instr, d: i32) -> Result<(), String> {
        let t = self.ilabel[self.an.index_of[jmp.arg as usize] as usize];
        let jump_if_true = jmp.op == Op::PopJumpIfTrue;
        let cop = (ins.arg & 15) as i32;
        let cc = match cop {
            Py_LT => Cc::L,
            Py_LE => Cc::LE,
            Py_EQ => Cc::E,
            Py_NE => Cc::NE,
            Py_GT => Cc::G,
            _ => Cc::GE,
        };
        let (ma, mb) = (self.slot(d - 2), self.slot(d - 1));
        let slow = self.a.label();
        let decide = self.a.label();
        self.a.load(Rax, ma);
        self.a.load(Rcx, mb);
        self.compact_int(Rax, Rdi, Rdx, slow);
        self.compact_int(Rcx, Rsi, Rdx, slow);
        self.a.cmp_rr(Rdi, Rsi);
        self.a.setcc(cc, R14);
        // operands: release (compact ints are mostly immortal small ints)
        self.a.load(Rax, ma);
        self.decref(Rax);
        self.a.load(Rax, mb);
        self.decref(Rax);
        self.a.bind(decide);
        self.a.test_rr(R14, R14);
        self.a.jcc(if jump_if_true { Cc::NE } else { Cc::E }, t);
        let fall = self.a.label();
        self.a.jmp(fall);
        // slow path: helper returns 0/1/2
        self.a.bind(slow);
        self.set_lasti(ins.pc);
        self.lea_sp(Rdi, d);
        self.a.mov_ri(Rsi, ins.arg as i64);
        self.call(addr!(h::h_compare_truth));
        let err = self.error_label(ins.pc, d - 2);
        self.a.cmp_ri(Rax, 2);
        self.a.jcc(Cc::E, err);
        self.a.mov_rr(R14, Rax);
        self.a.jmp(decide);
        self.a.bind(fall);
        Ok(())
    }
}

/// Compile a code object. Err: not compilable (stays in the interpreter).
pub fn compile(cd: &CodeData, nlocalsplus: usize, tables: &crate::interp::cache::Tables) -> Result<Output, String> {
    let an = super::analysis::analyze(&cd.code, &cd.exception_table)?;
    let mut a = Asm::new();
    let ilabel: Vec<Label> = (0..an.instrs.len()).map(|_| a.label()).collect();
    let exit = a.label();
    let mut g = Gen { a, nlp: nlocalsplus as i32, an: &an, cd, tables, ilabel, exit, stubs: Vec::new() };

    // prologue
    g.a.push(Rbp);
    g.a.mov_rr(Rbp, Rsp);
    g.a.push(Rbx);
    g.a.push(R12);
    g.a.push(R13);
    g.a.push(R14);
    g.a.push(R15);
    g.a.sub_ri(Rsp, 8);
    g.a.store(M(Rsp, 0), R8);
    g.a.mov_rr(Rbx, Rdi);
    g.a.mov_rr(R12, Rsi);
    g.a.mov_rr(R13, Rdx);
    g.a.load_fs0(R15);
    g.a.jmp_r(Rcx);
    // exit
    g.a.bind(exit);
    g.a.add_ri(Rsp, 8);
    g.a.pop(R15);
    g.a.pop(R14);
    g.a.pop(R13);
    g.a.pop(R12);
    g.a.pop(Rbx);
    g.a.pop(Rbp);
    g.a.ret();

    let mut entries = vec![u32::MAX; cd.code.len() + 1];
    let mut prev_yield = false;
    let mut skip_next = false;
    for i in 0..an.instrs.len() {
        let ins = an.instrs[i];
        let l = g.ilabel[i];
        g.a.bind(l);
        let reachable = an.depth[i] >= 0;
        if reachable && (i == 0 || an.is_target[i] || prev_yield) {
            entries[ins.start as usize] = g.a.pos() as u32;
        }
        prev_yield = ins.op == Op::YieldValue;
        if !reachable {
            continue;
        }
        if skip_next {
            skip_next = false;
            continue;
        }
        if ins.op == Op::CompareOp && ins.arg & 16 != 0 {
            let next = an.instrs.get(i + 1).map(|n| n.op);
            if matches!(next, Some(Op::PopJumpIfFalse | Op::PopJumpIfTrue)) && !an.is_target[i + 1] {
                skip_next = true;
            }
        }
        g.emit(i)?;
        // fall off the end of a non-terminating instruction into the next one is implicit
    }
    // a trailing reachable fallthrough should not exist (code ends with a terminator);
    // guard anyway
    g.a.ud2();
    let nlp = g.nlp;
    while let Some(s) = g.stubs.pop() {
        s(&mut g.a, exit, nlp);
    }
    g.a.resolve()?;
    Ok(Output { asm: g.a, entries })
}
