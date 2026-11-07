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
use crate::interp::cache::prof;
use crate::interp::frame::Frame;
use crate::object::*;
use crate::threads::{CThreadState, ThreadState};
use crate::types::code::CodeData;
use rpy_compiler::bytecode::{BinOp, Op};
use std::mem::offset_of;

pub const ST_RETURN: u32 = 0;
pub const ST_ERROR: u32 = 1;
pub const ST_DEOPT: u32 = 2;
pub const ST_YIELD: u32 = 3;
const YIELD_FROM_SEND: i32 = offset_of!(Frame, yield_from_send) as i32;
const RESUME: i32 = offset_of!(Frame, resume) as i32;

const LASTI: i32 = offset_of!(Frame, lasti) as i32;
const NEXT_INSTR: i32 = offset_of!(Frame, next_instr) as i32;
const STACK_TOP: i32 = offset_of!(Frame, stack_top) as i32;
const TS_EVAL_BREAKER: i32 = (offset_of!(ThreadState, c) + offset_of!(CThreadState, eval_breaker)) as i32;
const OB_TID: i32 = offset_of!(PyObject, ob_tid) as i32;
const OB_REF_LOCAL: i32 = offset_of!(PyObject, ob_ref_local) as i32;
const OB_REF_SHARED: i32 = offset_of!(PyObject, ob_ref_shared) as i32;
const OB_TYPE: i32 = offset_of!(PyObject, ob_type) as i32;
const OB_MUTEX: i32 = offset_of!(PyObject, ob_mutex) as i32;
const LV_TAG: i32 = offset_of!(PyLongObject, lv_tag) as i32;
const LV_DIGIT: i32 = offset_of!(PyLongObject, ob_digit) as i32;
const FVAL: i32 = offset_of!(PyFloatObject, ob_fval) as i32;

pub(crate) type Stub = Box<dyn FnOnce(&mut Asm, Label, i32)>;

const TP_TAG: i32 = offset_of!(PyTypeObject, tp_version_tag) as i32;
const GC_BITS: i32 = offset_of!(PyObject, ob_gc_bits) as i32;
const MA_KEYS: i32 = offset_of!(PyDictObject, ma_keys) as i32;
// (scale2) dict table fields read by the inline LoadGlobal with threads
const WSEQ: i32 = offset_of!(crate::types::dict::DictTable, wseq) as i32;
const SHARED_READ: i32 = offset_of!(crate::types::dict::DictTable, shared_read) as i32;
const NENTRIES: i32 = offset_of!(crate::types::dict::DictTable, nentries) as i32;
const MA_USED: i32 = offset_of!(PyDictObject, ma_used) as i32;
const MANAGED_DICT: i32 = -(crate::alloc::MANAGED as i32);
const MD_DICT: i32 = offset_of!(crate::types::module::PyModuleObject, md_dict) as i32;
const FRAME_GLOBALS: i32 = offset_of!(Frame, globals) as i32;
const FRAME_BUILTINS: i32 = offset_of!(Frame, builtins) as i32;
const E_SEQ: i32 = offset_of!(crate::interp::cache::Entry, seq) as i32;
const E_A: i32 = offset_of!(crate::interp::cache::Entry, a) as i32;
const E_B: i32 = offset_of!(crate::interp::cache::Entry, b) as i32;
const E_C: i32 = offset_of!(crate::interp::cache::Entry, c) as i32;
const E_KIND: i32 = offset_of!(crate::interp::cache::Entry, kind) as i32;
const E_MEMO: i32 = offset_of!(crate::interp::cache::Entry, memo) as i32;

const FUNC_CODE: i32 = offset_of!(crate::types::funcobject::PyFunctionObject, func_code) as i32;
const FUNC_VECTORCALL: i32 = offset_of!(crate::types::funcobject::PyFunctionObject, vectorcall) as i32;
const CODE_DATA: i32 = offset_of!(crate::types::code::PyCodeObject, data) as i32;
const CD_FAST_ENTRY: i32 = (offset_of!(CodeData, caches) + offset_of!(crate::interp::cache::CodeCaches, fast_entry)) as i32;

const LIST_SIZE: i32 =offset_of!(PyVarObject, ob_size) as i32;
const LIST_ITEMS: i32 = offset_of!(PyListObject, ob_item) as i32;
const LIST_ALLOCATED: i32 = offset_of!(PyListObject, allocated) as i32;
const TUPLE_ITEMS: i32 = offset_of!(PyTupleObject, ob_item) as i32;

/// The range/list/tuple iterator structs are private to their modules; the inline
/// ForIter paths assume (index or start, seq or step, len) at offsets 32/40/48.
/// Verify that once on real objects and fall back to the helper otherwise.
pub(crate) fn iter_layout_ok() -> bool {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OK.get_or_init(|| unsafe {
        let rd = |o: &PyObject, off: usize| *((o.as_ptr() as *const u8).add(off) as *const i64);
        let r = match crate::types::range::new_range_i64(5, 100, 3) {
            Ok(r) => r,
            Err(_) => {
                crate::errors::clear_exc();
                return false;
            }
        };
        let ri = match crate::abstract_::get_iter(&r) {
            Ok(i) => i,
            Err(_) => {
                crate::errors::clear_exc();
                return false;
            }
        };
        let range_ok = ri.is_type(&raw mut crate::types::range::PyRangeIter_Type) && rd(&ri, 32) == 5 && rd(&ri, 40) == 3 && rd(&ri, 48) == 32;
        let l = crate::types::list::new_list(vec![crate::types::int::new_int(7)]);
        let li = crate::abstract_::get_iter(&l).ok();
        let list_ok = li.as_ref().is_some_and(|i| {
            i.is_type(&raw mut crate::types::list::PyListIter_Type) && rd(i, 32) == 0 && rd(i, 40) == l.as_ptr() as i64
        });
        let t = crate::types::tuple::new_tuple(vec![crate::types::int::new_int(7)]);
        let ti = crate::abstract_::get_iter(&t).ok();
        let tuple_ok = ti.as_ref().is_some_and(|i| {
            i.is_type(&raw mut crate::types::tuple::PyTupleIter_Type) && rd(i, 32) == 0 && rd(i, 40) == t.as_ptr() as i64
        });
        range_ok && list_ok && tuple_ok
    })
}

/// Layout facts about Rust-owned structures, probed at run time (Vec field order is
/// not specified by Rust).
struct Layout {
    /// DictTable: entries.ptr / entries.len / version
    entries_ptr: i32,
    entries_len: i32,
    version: i32,
    entry_size: i32,
    entry_key: i32,
    entry_value: i32,
}

/// (perf5) C getter and closure of an exact getset descriptor, if readable.
unsafe fn getset_getter(d: *mut PyObject) -> Option<(usize, usize)> {
    unsafe {
        if !(*d).is_type(crate::types::descr::getset_descr_type()) {
            return None;
        }
        let g = (*(d as *mut crate::ffi::PyGetSetDescrObject)).d_getset;
        if g.is_null() {
            return None;
        }
        (*g).get.map(|f| (f as usize, (*g).closure as usize))
    }
}

fn layout() -> &'static Layout {
    static L: std::sync::OnceLock<Layout> = std::sync::OnceLock::new();
    L.get_or_init(|| {
        Layout {
            entries_ptr: offset_of!(crate::types::dict::DictTable, entries_p) as i32,
            entries_len: offset_of!(crate::types::dict::DictTable, nentries) as i32,
            version: offset_of!(crate::types::dict::DictTable, version) as i32,
            entry_size: std::mem::size_of::<crate::types::dict::Entry>() as i32,
            entry_key: offset_of!(crate::types::dict::Entry, key) as i32,
            entry_value: offset_of!(crate::types::dict::Entry, value) as i32,
        }
    })
}

pub struct Output {
    pub asm: Asm,
    /// native offset per code index (u32::MAX: not an entry point)
    pub entries: Vec<u32>,
}

pub(crate) struct Gen<'a> {
    pub(crate) a: Asm,
    pub(crate) nlp: i32,
    pub(crate) an: &'a Analysis,
    pub(crate) cd: &'a CodeData,
    pub(crate) tables: &'a crate::interp::cache::Tables,
    pub(crate) ilabel: Vec<Label>,
    pub(crate) exit: Label,
    stubs: Vec<Stub>,
    pub(crate) rt_box_int: Label,
    pub(crate) rt_box_float: Label,
    pub(crate) rt_free_num: Label,
    flags: u32,
    /// do not fuse CompareOp with a following branch (embedded in tier 2)
    pub(crate) no_fuse: bool,
    /// (jit6) drain-mode code of a generator expression (see YieldValue)
    pub(crate) drain: bool,
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
        // the exit current at creation (tier 2 redirects it per embedded instruction)
        let exit = self.exit;
        self.stubs.push(Box::new(move |a: &mut Asm, _exit, nlp| {
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
            emit_nonowner_incref(a, r, done);
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
        let free_num = self.rt_free_num;
        self.stubs.push(Box::new(move |a: &mut Asm, _exit, _nlp| {
            a.bind(shared);
            a.mov_rr(Rdi, r);
            a.call_abs(addr!(h::h_decref_shared));
            a.jmp(done);
            a.bind(zero);
            a.mov_rr(Rdi, r);
            a.call_label(free_num);
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

    /// (scale2) Release the object's mutex taken inline (`lock cmpxchg` 0 -> 1): wake
    /// parked waiters through the runtime when the byte is not plain 1. Clobbers rax
    /// and r11; `obj` must not be rax or r11.
    fn ob_unlock(&mut self, obj: R) {
        let done = self.a.label();
        self.a.mov_ri(Rax, 1);
        self.a.mov_ri(R11, 0);
        self.a.lock_cmpxchg8(M(obj, OB_MUTEX), R11);
        self.a.jcc(Cc::E, done);
        let rt = unlock_slow_routine().expect("tier1: unlock routine");
        self.a.push(Rdi);
        if obj != Rdi {
            self.a.mov_rr(Rdi, obj);
        }
        self.a.mov_ri(R11, rt as i64);
        self.a.call_r(R11);
        self.a.pop(Rdi);
        self.a.bind(done);
    }

    /// (scale2) Inline `qsbr::release_attr(obj, old)` after a swap of attribute storage
    /// (the swap or the unlock that followed it was a locked instruction, i.e. a full
    /// barrier before the flags load): `old` (may be null) is released at once when this
    /// thread owns `obj` and no other thread read its attribute storage lock-free (XREAD
    /// clear), else through QSBR. `obj` must be callee-saved (r14); clobbers the rest.
    fn release_attr(&mut self, obj: R, old: R) {
        const OB_FLAGS: i32 = offset_of!(PyObject, ob_flags) as i32;
        let skip = self.a.label();
        let defer = self.a.label();
        self.a.test_rr(old, old);
        self.a.jcc(Cc::E, skip);
        self.a.cmp_rm(R15, M(obj, OB_TID));
        self.a.jcc(Cc::NE, defer);
        self.a.load8(R11, M(obj, OB_FLAGS));
        self.a.test_ri(R11, crate::qsbr::XREAD as i32);
        self.a.jcc(Cc::NE, defer);
        if old != Rax {
            self.a.mov_rr(Rax, old);
        }
        self.decref(Rax);
        self.a.jmp(skip);
        self.a.bind(defer);
        if old != Rdi {
            self.a.mov_rr(Rdi, old);
        }
        self.call(addr!(h::h_decref_deferred));
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

    fn prof_addr(&self, pc: u32) -> i64 {
        &self.tables.profile[pc as usize] as *const _ as i64
    }
    /// Record type-profile bits for the instruction at `pc` (fast paths). Clobbers r11.
    fn prof(&mut self, pc: u32, bits: u16) {
        // (scale2) write only when a bit is new: an unconditional `or` stores to the
        // shared profile line at every execution, and threads running the same code
        // then fight over that cache line
        let p = self.prof_addr(pc);
        let skip = self.a.label();
        let set = self.a.label();
        self.a.mov_ri(R11, p);
        for byte in 0..2 {
            let b = (bits >> (8 * byte)) as u8;
            for k in 0..8 {
                if b & (1 << k) != 0 {
                    self.a.test8_mi(M(R11, byte), 1 << k);
                    self.a.jcc(Cc::E, set);
                }
            }
        }
        self.a.bind(skip);
        self.stubs.push(Box::new(move |a: &mut Asm, _exit, _nlp| {
            a.bind(set);
            a.or16_mi(M(R11, 0), bits);
            a.jmp(skip);
        }));
    }
    /// Record the types of the stack operands at depths `da` and `db` (slow paths;
    /// clobbers caller-saved registers).
    fn prof_slow(&mut self, pc: u32, da: i32, db: Option<i32>) {
        let skip = self.a.label();
        self.prof_gate(skip);
        let p = self.prof_addr(pc);
        self.a.mov_ri(Rdi, p);
        let m = self.slot(da);
        self.a.load(Rsi, m);
        match db {
            Some(db) => {
                let m = self.slot(db);
                self.a.load(Rdx, m);
            }
            None => self.a.mov_ri(Rdx, 0),
        }
        self.call(addr!(h::h_prof));
        self.a.bind(skip);
    }

    /// Jump to `skip` once slow-path profiling is off for this code (tier-2 code
    /// installed; re-enabled when it is invalidated). Clobbers r11.
    fn prof_gate(&mut self, skip: Label) {
        let g = &self.cd.caches.t2_prof as *const _ as i64;
        self.a.mov_ri(R11, g);
        self.a.cmp8_mi(M(R11, 0), 0);
        self.a.jcc(Cc::E, skip);
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

    pub(crate) fn emit(&mut self, i: usize) -> Result<(), String> {
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
            Op::LoadFast | Op::LoadFastBorrow | Op::LoadFastCheck => {
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
            Op::Send => {
                let t = self.ilabel[self.an.index_of[arg as usize] as usize];
                self.set_lasti(pc);
                self.lea_sp(Rdi, d);
                self.a.mov_rr(Rsi, R13);
                self.call(addr!(h::h_send));
                let err = self.error_label(pc, d - 1);
                self.a.cmp_ri(Rax, 1);
                self.a.jcc(Cc::E, t);
                self.a.jcc(Cc::A, err);
            }
            Op::GetYieldFromIter => {
                let flags = self.flags as i64;
                self.helper_sp(&ins, d, addr!(h::h_get_yield_from_iter), Some(flags), None, d - 1);
            }
            Op::YieldValue if self.drain && i + 1 < self.ilabel.len() => {
                // (jit6) drain mode: the consumer takes the value directly
                // (`h_drain_add`); the yield's result is then None and execution goes
                // on. If the consumer must see the value itself, an ordinary
                // suspension follows whose continuation is left to the general
                // resumption path (resume = 0: normal code, not this drain code).
                let next = self.an.instrs[i + 1].start;
                let normal = self.a.label();
                let m = self.slot(d - 1);
                self.a.mov_rr(Rdi, Rbx);
                self.a.load(Rsi, m);
                self.call(addr!(crate::types::genobject::h_drain_add));
                self.a.test_rr(Rax, Rax);
                self.a.jcc(Cc::NE, normal);
                self.a.mov_ri(Rax, crate::types::none::none().as_ptr() as i64);
                self.a.store(m, Rax);
                self.a.jmp(self.ilabel[i + 1]);
                self.a.bind(normal);
                self.a.load(Rax, m);
                self.a.load(Rcx, M(Rsp, 0));
                self.a.store(M(Rcx, 0), Rax);
                self.a.store32_i(M(Rbx, LASTI), pc as i32);
                self.a.store32_i(M(Rbx, NEXT_INSTR), next as i32);
                self.a.store32_i(M(Rbx, YIELD_FROM_SEND), -1);
                self.a.store_i(M(Rbx, STACK_TOP), self.nlp + d - 1);
                self.a.store_i(M(Rbx, RESUME), 0);
                self.a.mov_ri(Rax, ST_YIELD as i64);
                self.a.jmp(self.exit);
            }
            Op::YieldValue => {
                // suspend: the interpreter resumes the generator at the next instruction
                // (an entry point of this code) and pushes the sent value there
                let next = self.an.instrs.get(i + 1).map_or(ins.pc + 1, |n| n.start);
                let send = if i > 0 && self.an.instrs[i - 1].op == Op::Send { self.an.instrs[i - 1].pc as i32 } else { -1 };
                let m = self.slot(d - 1);
                self.a.load(Rax, m);
                self.a.load(Rcx, M(Rsp, 0));
                self.a.store(M(Rcx, 0), Rax);
                self.a.store32_i(M(Rbx, LASTI), pc as i32);
                self.a.store32_i(M(Rbx, NEXT_INSTR), next as i32);
                self.a.store32_i(M(Rbx, YIELD_FROM_SEND), send);
                self.a.store_i(M(Rbx, STACK_TOP), self.nlp + d - 1);
                if !self.no_fuse && i + 1 < self.ilabel.len() {
                    // the next resumption enters the continuation directly
                    // (genobject.rs `gen_send_fast`)
                    // (unless tier-2 code is installed: the general resumption path
                    // moves the activation there)
                    let cont = self.ilabel[i + 1];
                    let t2 = self.a.label();
                    let done = self.a.label();
                    self.a.mov_ri(Rdx, &self.cd.caches.opt as *const _ as i64);
                    self.a.cmp_mi(M(Rdx, 0), 0);
                    self.a.jcc(Cc::NE, t2);
                    self.a.mov_r_label_addr(Rcx, cont);
                    self.a.store(M(Rbx, RESUME), Rcx);
                    self.a.jmp(done);
                    self.a.bind(t2);
                    self.a.store_i(M(Rbx, RESUME), 0);
                    self.a.bind(done);
                }
                self.a.mov_ri(Rax, ST_YIELD as i64);
                self.a.jmp(self.exit);
            }
            Op::Jump => {
                let t = self.an.index_of[arg as usize] as usize;
                if arg <= ins.start {
                    self.poll(pc, d);
                    if !self.no_fuse && !self.drain && super::opt::enabled() {
                        // tier-2 backedge countdown; on expiry compile and continue
                        // in the tier-2 OSR entry of this loop header if one exists
                        let stub = self.a.label();
                        let back = self.a.label();
                        let cnt = &self.cd.caches.t2_backedge as *const _ as i64;
                        self.a.mov_ri(R11, cnt);
                        // (scale2) written only while it runs (>= 0; h_backedge parks it
                        // at -1): threads running the same loop share this line
                        self.a.cmp32_mi(M(R11, 0), 0);
                        self.a.jcc(Cc::L, back);
                        // (weighted by the loop length, like the interpreter's counter)
                        self.a.sub32_mi(M(R11, 0), 1 + ((pc - arg) / 32) as i32);
                        self.a.jcc(Cc::L, stub);
                        self.a.bind(back);
                        self.stubs.push(Box::new(move |a: &mut Asm, _exit, _nlp| {
                            a.bind(stub);
                            a.store32_i(M(Rbx, LASTI), pc as i32);
                            a.mov_rr(Rdi, Rbx);
                            a.mov_ri(Rsi, arg as i64);
                            a.call_abs(addr!(super::opt::h_backedge));
                            a.test_rr(Rax, Rax);
                            a.jcc(Cc::E, back);
                            a.jmp_r(Rax);
                        }));
                    }
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
            Op::LoadGlobal => self.load_global(&ins, d),
            Op::LoadAttr | Op::LoadMethod | Op::StoreAttr => {
                let (hf, errd) = match ins.op {
                    Op::LoadAttr => (addr!(h::h_load_attr), d - 1),
                    Op::LoadMethod => (addr!(h::h_load_method), d - 1),
                    _ => (addr!(h::h_store_attr), d - 2),
                };
                self.attr_op(&ins, d, hf, errd);
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
            Op::LoadDeref => {
                // (jit5) cell contents inline; unbound (or no cell): helper raises
                let slow = self.a.label();
                let done = self.a.label();
                let l = self.local(arg);
                // (stability3) with threads the contents are read under the cell's
                // lock (funcobject::cell_get): helper
                self.a.mov_ri(Rdi, (&raw const crate::threads::MULTI_THREADED) as i64);
                self.a.cmp_mi(M(Rdi, 0), 0);
                self.a.jcc(Cc::NE, slow);
                self.a.load(Rax, l);
                self.a.test_rr(Rax, Rax);
                self.a.jcc(Cc::E, slow);
                self.a.load(Rsi, M(Rax, std::mem::offset_of!(crate::types::funcobject::PyCellObject, ob_ref) as i32));
                self.a.test_rr(Rsi, Rsi);
                self.a.jcc(Cc::E, slow);
                self.incref(Rsi);
                let m = self.slot(d);
                self.a.store(m, Rsi);
                self.a.jmp(done);
                self.a.bind(slow);
                self.set_lasti(pc);
                self.a.mov_rr(Rdi, Rbx);
                self.lea_sp(Rsi, d);
                self.a.mov_ri(Rdx, arg as i64);
                self.call(addr!(h::h_load_deref));
                self.check(pc, d);
                self.a.bind(done);
            }
            Op::StoreDeref => {
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
                let fuse = !self.no_fuse
                    && arg & 16 != 0
                    && matches!(next.map(|n| n.op), Some(Op::PopJumpIfFalse | Op::PopJumpIfTrue))
                    && !self.an.is_target[i + 1];
                if fuse {
                    self.compare_jump(&ins, &next.unwrap(), d)?;
                    return Ok(());
                }
                self.prof_slow(pc, d - 2, Some(d - 1));
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
            Op::BinarySubscr => self.binary_subscr(&ins, d),
            Op::StoreSubscr => {
                self.prof_slow(pc, d - 2, Some(d - 1));
                self.helper_sp(&ins, d, addr!(h::h_store_subscr), None, None, d - 3)
            }
            Op::DeleteSubscr => self.helper_sp(&ins, d, addr!(h::h_delete_subscr), None, None, d - 2),
            Op::BinarySlice => self.helper_sp(&ins, d, addr!(h::h_binary_slice), None, None, d - 3),
            Op::StoreSlice => self.helper_sp(&ins, d, addr!(h::h_store_slice), None, None, d - 4),
            Op::BuildSlice => self.helper_sp(&ins, d, addr!(h::h_build_slice), Some(arg as i64), None, d - arg as i32),
            Op::BuildTuple => self.helper_sp(&ins, d, addr!(h::h_build_tuple), Some(arg as i64), None, d - arg as i32),
            Op::BuildList => self.helper_sp(&ins, d, addr!(h::h_build_list), Some(arg as i64), None, d - arg as i32),
            Op::BuildSet => self.helper_sp(&ins, d, addr!(h::h_build_set), Some(arg as i64), None, d - arg as i32),
            Op::BuildMap => self.helper_sp(&ins, d, addr!(h::h_build_map), Some(arg as i64), None, d - 2 * arg as i32),
            Op::BuildString => self.helper_sp(&ins, d, addr!(h::h_build_string), Some(arg as i64), None, d - arg as i32),
            Op::ListAppend => {
                // (jit6) inline append to the comprehension's exact list while it has
                // room (single-threaded, as the other lock-free list paths)
                let generic = self.a.label();
                let done = self.a.label();
                let lm = self.slot(d - 1 - arg as i32);
                let vm = self.slot(d - 1);
                self.a.load(Rax, lm);
                self.a.mov_ri(Rcx, crate::types::list::list_type() as i64);
                self.a.cmp_rm(Rcx, M(Rax, OB_TYPE));
                self.a.jcc(Cc::NE, generic);
                // (scale1) with threads too, as long as the list is referenced only from
                // this frame (owned by this thread, count 1): nobody else can reach it
                self.emit_exclusive_check(Rax, generic);
                self.a.load(Rcx, M(Rax, LIST_SIZE));
                self.a.cmp_rm(Rcx, M(Rax, LIST_ALLOCATED));
                self.a.jcc(Cc::GE, generic);
                self.a.load(Rdx, M(Rax, LIST_ITEMS));
                self.a.load(Rsi, vm);
                self.a.store_idx(Rdx, Rcx, 8, 0, Rsi);
                self.a.add_ri(Rcx, 1);
                self.a.store(M(Rax, LIST_SIZE), Rcx);
                self.a.jmp(done);
                self.a.bind(generic);
                self.helper_sp(&ins, d, addr!(h::h_container_op), Some(0), Some(arg as i64), d - 1);
                self.a.bind(done);
            }
            Op::SetAdd | Op::ListExtend | Op::SetUpdate | Op::DictUpdate | Op::DictMerge => {
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
            Op::UnpackSequence => {
                self.prof_slow(pc, d - 1, None);
                self.helper_sp(&ins, d, addr!(h::h_unpack_sequence), Some(arg as i64), None, d - 1)
            }
            Op::UnpackEx => self.helper_sp(&ins, d, addr!(h::h_unpack_ex), Some(arg as i64), None, d - 1),
            Op::GetIter => self.helper_sp(&ins, d, addr!(h::h_get_iter), None, None, d - 1),
            Op::ForIter => self.for_iter(&ins, d),
            Op::Call => {
                // Python function with a fast entry: direct call (args moved into the
                // callee frame); anything else: generic call helper
                let base = d - arg as i32 - 2;
                let generic = self.a.label();
                let done = self.a.label();
                let m = self.slot(base);
                self.a.load(Rax, m);
                self.a.mov_ri(Rcx, crate::types::funcobject::function_type() as i64);
                self.a.cmp_rm(Rcx, M(Rax, OB_TYPE));
                let is_func = self.a.label();
                self.a.jcc(Cc::E, is_func);
                if arg == 1 {
                    // (perf5) type(x): the argument's type
                    let not_type = self.a.label();
                    self.a.mov_ri(Rcx, crate::types::typeobject::type_type() as i64);
                    self.a.cmp_rr(Rax, Rcx);
                    self.a.jcc(Cc::NE, not_type);
                    let sm = self.slot(base + 1);
                    self.a.cmp_mi(sm, 0);
                    self.a.jcc(Cc::NE, not_type);
                    let am = self.slot(base + 2);
                    self.a.load(Rcx, am);
                    self.a.load(Rdx, M(Rcx, OB_TYPE));
                    self.incref(Rdx);
                    self.a.store(m, Rdx);
                    self.a.mov_rr(Rax, Rcx);
                    self.decref(Rax);
                    self.a.mov_ri(Rax, crate::types::typeobject::type_type() as i64);
                    self.decref(Rax);
                    self.a.jmp(done);
                    self.a.bind(not_type);
                }
                // (perf5) bound method with a free self slot: unpack it in the stack and
                // retry with its function (CPython's bound-method call specialisation)
                self.a.mov_ri(Rcx, crate::types::funcobject::method_type() as i64);
                self.a.cmp_rm(Rcx, M(Rax, OB_TYPE));
                self.a.jcc(Cc::NE, generic);
                let ms = self.slot(base + 1);
                self.a.cmp_mi(ms, 0);
                self.a.jcc(Cc::NE, generic);
                self.lea_sp(Rdi, base);
                self.call(addr!(h::h_unpack_method));
                self.a.load(Rax, m);
                self.a.mov_ri(Rcx, crate::types::funcobject::function_type() as i64);
                self.a.cmp_rm(Rcx, M(Rax, OB_TYPE));
                self.a.jcc(Cc::NE, generic);
                self.a.bind(is_func);
                // a replaced vectorcall (C API) must be honoured
                self.a.mov_ri(Rcx, crate::interp::function_vectorcall as usize as i64);
                self.a.cmp_rm(Rcx, M(Rax, FUNC_VECTORCALL));
                self.a.jcc(Cc::NE, generic);
                self.a.load(Rcx, M(Rax, FUNC_CODE));
                self.a.test_rr(Rcx, Rcx);
                self.a.jcc(Cc::E, generic);
                self.a.load(Rcx, M(Rcx, CODE_DATA));
                // body not unmarshalled yet (perf2 lazy code): the generic path loads it
                self.a.test_rr(Rcx, Rcx);
                self.a.jcc(Cc::E, generic);
                self.a.cmp_mi(M(Rcx, CD_FAST_ENTRY), 0);
                self.a.jcc(Cc::E, generic);
                self.set_lasti(pc);
                self.lea_sp(Rdi, base);
                self.a.mov_ri(Rsi, arg as i64);
                // (jit6) the machine-code call routine (falls back to h_call_py)
                let r = super::callpy::routine();
                if r != 0 {
                    self.a.mov_rr(Rdx, Rcx);
                    self.call(r);
                } else {
                    self.call(addr!(super::h_call_py));
                }
                self.check(pc, base);
                self.a.jmp(done);
                self.a.bind(generic);
                self.helper_sp(&ins, d, addr!(h::h_call), Some(arg as i64), Some(0), base);
                // C calls: observe signals, pending calls and safepoints right after
                // (Python callees poll at their own Resume)
                self.poll(pc, base + 1);
                self.a.bind(done);
            }
            Op::CallKw => {
                let base = d - arg as i32 - 3;
                self.helper_sp(&ins, d, addr!(h::h_call), Some(arg as i64), Some(1), base);
                self.poll(pc, base + 1);
            }
            Op::CallFunctionEx => {
                self.helper_sp(&ins, d, addr!(h::h_call_ex), None, None, d - 4);
                self.poll(pc, d - 3);
            }
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
            Op::GetAwaitable => self.helper_sp(&ins, d, addr!(h::h_get_awaitable), Some(arg as i64), None, d - 1),
            // (perf5) raise / re-raise without leaving compiled code: the helper sets the
            // exception, lasti/unwind_pc and stack_top; the caller unwinds (and continues
            // at a handler of this frame in compiled code, jit::finish_in_interp)
            Op::RaiseVarargs | Op::Reraise if super::raise_enabled() => {
                self.set_lasti(pc);
                self.a.mov_rr(Rdi, R13);
                self.a.mov_rr(Rsi, Rbx);
                self.lea_sp(Rdx, d);
                self.a.mov_ri(Rcx, ins.op as i64 | (arg as i64) << 8);
                self.a.mov_ri(R8, pc as i64);
                self.call(addr!(h::h_raise));
                self.a.mov_ri(Rax, ST_ERROR as i64);
                self.a.jmp(self.exit);
            }
            // instructions shared with the interpreter (eval::exec_misc)
            op if crate::interp::eval::is_misc(op) && super::misc_enabled() => {
                let pops = match op {
                    Op::LoadFromDictOrDeref | Op::LoadFromDictOrGlobals | Op::GetAiter | Op::GetAwaitable | Op::LoadSpecial => 1,
                    Op::PushExcInfo | Op::PopExcept | Op::CheckExcMatch | Op::BeforeWith | Op::BeforeAsyncWith => 1,
                    Op::EndAsyncFor | Op::CheckEgMatch | Op::ImportName | Op::BuildTemplate => 2,
                    Op::BuildInterpolation => 2 + (arg & 1) as i32,
                    Op::CleanupThrow | Op::MatchClass => 3,
                    _ => 0,
                };
                self.set_lasti(pc);
                self.a.mov_rr(Rdi, R13);
                self.a.mov_rr(Rsi, Rbx);
                self.lea_sp(Rdx, d);
                self.a.mov_ri(Rcx, op as i64 | (arg as i64) << 8);
                self.a.mov_ri(R8, (self.nlp + d - pops) as i64);
                self.call(addr!(h::h_misc));
                self.check(pc, d - pops);
            }
            // everything else runs in the interpreter
            _ => self.deopt(ins.start, d),
        }
        Ok(())
    }

    /// BinarySubscr with inline list[int] / tuple[int] (compact int index).
    fn binary_subscr(&mut self, ins: &Instr, d: i32) {
        let (mc, mk) = (self.slot(d - 2), self.slot(d - 1));
        let slow = self.a.label();
        let done = self.a.label();
        let not_list = self.a.label();
        let got = self.a.label();
        self.a.load(Rax, mc);
        self.a.load(Rcx, mk);
        self.compact_int(Rcx, Rsi, Rdx, slow);
        for (tp, is_list) in [(crate::types::list::list_type() as i64, true), (crate::types::tuple::tuple_type() as i64, false)] {
            if !is_list {
                self.a.bind(not_list);
            }
            self.a.mov_ri(Rdx, tp);
            self.a.cmp_rm(Rdx, M(Rax, OB_TYPE));
            self.a.jcc(Cc::NE, if is_list { not_list } else { slow });
            self.a.load(Rdx, M(Rax, LIST_SIZE));
            let pos = self.a.label();
            self.a.test_rr(Rsi, Rsi);
            self.a.jcc(Cc::NS, pos);
            self.a.add_rr(Rsi, Rdx);
            self.a.bind(pos);
            self.a.cmp_rr(Rsi, Rdx);
            self.a.jcc(Cc::AE, slow);
            if is_list {
                // lock-free read: size (above), then array, then item; writers keep
                // this safe while threads share the list (perf2, types/list.rs docs).
                // A null array or item means a racing writer: take the helper.
                self.a.load(Rdx, M(Rax, LIST_ITEMS));
                self.a.test_rr(Rdx, Rdx);
                self.a.jcc(Cc::E, slow);
                self.a.load_idx(Rdi, Rdx, Rsi, 8, 0);
                self.a.test_rr(Rdi, Rdi);
                self.a.jcc(Cc::E, slow);
                self.prof(ins.pc, (prof::LIST as u16) | (prof::INT as u16) << 8);
                self.a.jmp(got);
            } else {
                self.a.load_idx(Rdi, Rax, Rsi, 8, TUPLE_ITEMS);
                self.prof(ins.pc, (prof::TUPLE as u16) | (prof::INT as u16) << 8);
            }
        }
        self.a.bind(got);
        self.incref(Rdi);
        self.a.mov_rr(R14, Rdi);
        self.a.load(Rax, mc);
        self.decref(Rax);
        self.a.load(Rax, mk);
        self.decref(Rax);
        self.a.store(mc, R14);
        self.a.jmp(done);
        self.a.bind(slow);
        self.prof_slow(ins.pc, d - 2, Some(d - 1));
        self.helper_sp(ins, d, addr!(h::h_binary_subscr), None, None, d - 2);
        self.a.bind(done);
    }

    // ------------------------------------------------------------ iteration

    /// ForIter with inline fast paths for range, list and tuple iterators; anything
    /// else (and exhaustion) goes through the helper.
    fn for_iter(&mut self, ins: &Instr, d: i32) {
        let t = self.ilabel[self.an.index_of[ins.arg as usize] as usize];
        let generic = self.a.label();
        let pushed = self.a.label();
        if iter_layout_ok() {
            let it = self.slot(d - 1);
            let out = self.slot(d);
            let not_range = self.a.label();
            let not_list = self.a.label();
            self.a.load(Rax, it);
            // range iterator: start/step/len are i64 at 32/40/48
            self.a.mov_ri(Rcx, (&raw mut crate::types::range::PyRangeIter_Type) as i64);
            self.a.cmp_rm(Rcx, M(Rax, OB_TYPE));
            self.a.jcc(Cc::NE, not_range);
            self.a.cmp_mi(M(Rax, 48), 0);
            self.a.jcc(Cc::LE, generic);
            self.a.add_mi(M(Rax, 48), -1);
            self.a.load(Rdi, M(Rax, 32));
            self.a.load(Rcx, M(Rax, 40));
            self.a.lea(Rdx, M(Rdi, 0));
            self.a.add_rr(Rdx, Rcx);
            self.a.store(M(Rax, 32), Rdx);
            self.a.call_label(self.rt_box_int);
            self.a.store(out, Rax);
            self.prof(ins.pc, prof::IT_RANGE as u16);
            self.a.jmp(pushed);
            // list iterator: index at 32, seq at 40; list size at 32, items at 40
            self.a.bind(not_range);
            self.a.mov_ri(Rcx, (&raw mut crate::types::list::PyListIter_Type) as i64);
            self.a.cmp_rm(Rcx, M(Rax, OB_TYPE));
            self.a.jcc(Cc::NE, not_list);
            // (scale1) an iterator stepped only by this frame (owned, count 1); the list
            // itself is read like the lock-free subscript: size, then array, then item,
            // a null array or slot (racing writer) takes the generic path
            self.emit_exclusive_check(Rax, generic);
            self.a.load(Rdx, M(Rax, 40));
            self.a.test_rr(Rdx, Rdx);
            self.a.jcc(Cc::E, generic);
            self.a.load(Rcx, M(Rax, 32));
            self.a.test_rr(Rcx, Rcx);
            self.a.jcc(Cc::S, generic);
            self.a.cmp_rm(Rcx, M(Rdx, LIST_SIZE));
            self.a.jcc(Cc::GE, generic);
            self.a.load(Rsi, M(Rdx, LIST_ITEMS));
            self.a.test_rr(Rsi, Rsi);
            self.a.jcc(Cc::E, generic);
            self.a.load_idx(Rsi, Rsi, Rcx, 8, 0);
            self.a.test_rr(Rsi, Rsi);
            self.a.jcc(Cc::E, generic);
            self.a.add_mi(M(Rax, 32), 1);
            self.incref(Rsi);
            self.a.store(out, Rsi);
            self.prof(ins.pc, prof::IT_LIST as u16);
            self.a.jmp(pushed);
            // tuple iterator: index at 32, seq at 40; tuple size at 32, items inline
            self.a.bind(not_list);
            self.a.mov_ri(Rcx, (&raw mut crate::types::tuple::PyTupleIter_Type) as i64);
            self.a.cmp_rm(Rcx, M(Rax, OB_TYPE));
            self.a.jcc(Cc::NE, generic);
            self.a.load(Rdx, M(Rax, 40));
            self.a.test_rr(Rdx, Rdx);
            self.a.jcc(Cc::E, generic);
            self.a.load(Rcx, M(Rax, 32));
            self.a.cmp_rm(Rcx, M(Rdx, LIST_SIZE));
            self.a.jcc(Cc::GE, generic);
            self.a.load_idx(Rsi, Rdx, Rcx, 8, TUPLE_ITEMS);
            self.a.add_mi(M(Rax, 32), 1);
            self.incref(Rsi);
            self.a.store(out, Rsi);
            self.prof(ins.pc, prof::IT_TUPLE as u16);
            self.a.jmp(pushed);
        }
        self.a.bind(generic);
        if let Some(kind_arg) = self.drain_pattern(ins) {
            // (jit6) a consumer loop that can drain a fresh generator: the generator
            // runs in drain mode and hands its values over itself (helpers.rs h_drain)
            let not_gen = self.a.label();
            let item = self.a.label();
            let it = self.slot(d - 1);
            self.a.load(Rax, it);
            self.a.mov_ri(Rcx, crate::types::genobject::gen_type() as i64);
            self.a.cmp_rm(Rcx, M(Rax, OB_TYPE));
            self.a.jcc(Cc::NE, not_gen);
            self.set_lasti(ins.pc);
            self.lea_sp(Rdi, d);
            self.a.mov_rr(Rsi, R13);
            self.a.mov_ri(Rdx, kind_arg as i64);
            self.call(addr!(h::h_drain));
            let err = self.error_label(ins.pc, d);
            self.a.cmp_ri(Rax, 1);
            self.a.jcc(Cc::E, t);
            self.a.jcc(Cc::B, item);
            self.a.cmp_ri(Rax, 2);
            self.a.jcc(Cc::E, err);
            self.a.jmp(not_gen);
            self.a.bind(item);
            self.a.jmp(pushed);
            self.a.bind(not_gen);
        }
        let noprof = self.a.label();
        self.prof_gate(noprof);
        let p = self.prof_addr(ins.pc);
        self.a.mov_ri(Rdi, p);
        let m = self.slot(d - 1);
        self.a.load(Rsi, m);
        self.call(addr!(h::h_prof_iter));
        self.a.bind(noprof);
        self.set_lasti(ins.pc);
        self.lea_sp(Rdi, d);
        self.a.mov_rr(Rsi, R13);
        self.call(addr!(h::h_for_iter));
        let err = self.error_label(ins.pc, d);
        self.a.cmp_ri(Rax, 1);
        self.a.jcc(Cc::E, t);
        self.a.jcc(Cc::A, err);
        self.a.bind(pushed);
    }

    /// (jit6) Is this ForIter the head of a loop that only collects into a container
    /// (`ListAppend`/`SetAdd`, then the backedge) or searches like any()/all()
    /// (`ToBool`, a conditional exit, the backedge)? Some(kind | container distance
    /// << 8) for `h_drain`.
    fn drain_pattern(&self, ins: &Instr) -> Option<u64> {
        use crate::types::genobject::{DRAIN_ALL, DRAIN_ANY, DRAIN_LIST, DRAIN_SET};
        let i = self.an.index_of[ins.start as usize] as usize;
        let at = |k: usize| self.an.instrs.get(i + k).copied();
        let back = |n: Option<Instr>| n.is_some_and(|n| n.op == Op::Jump && n.arg <= n.start && self.an.index_of[n.arg as usize] as usize == i);
        let n1 = at(1)?;
        match n1.op {
            Op::ListAppend | Op::SetAdd if back(at(2)) && n1.arg >= 1 => {
                let kind = if n1.op == Op::ListAppend { DRAIN_LIST } else { DRAIN_SET };
                Some(kind | (n1.arg as u64) << 8)
            }
            Op::ToBool => {
                let n2 = at(2)?;
                if !back(at(3)) {
                    return None;
                }
                match n2.op {
                    Op::PopJumpIfTrue => Some(DRAIN_ANY),
                    Op::PopJumpIfFalse => Some(DRAIN_ALL),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    // ------------------------------------------------------------ inline caches

    /// rcx = type of obj in `obj`; jump to slow unless its version tag is `tag`.
    fn guard_tag(&mut self, obj: R, tag: u32, slow: Label) {
        self.a.load(Rcx, M(obj, OB_TYPE));
        self.a.cmp32_mi(M(Rcx, TP_TAG), tag as i32);
        self.a.jcc(Cc::NE, slow);
    }
    /// rdx = managed instance dict of obj (exact dict, may be NULL -> `null` label).
    fn managed_dict(&mut self, obj: R, slow: Label, null: Label) {
        self.a.load8(Rdx, M(obj, GC_BITS));
        self.a.and_ri(Rdx, crate::alloc::GC_BIT_MANAGED as i32);
        self.a.jcc(Cc::E, slow);
        self.a.load(Rdx, M(obj, MANAGED_DICT));
        self.a.test_rr(Rdx, Rdx);
        self.a.jcc(Cc::E, null);
        // inline values (perf-runtime shape.rs): tagged pointer, not a dict
        self.a.mov_rr(Rsi, Rdx);
        self.a.and_ri(Rsi, 1);
        self.a.jcc(Cc::NE, slow);
        self.a.mov_ri(Rsi, crate::types::dict::dict_type() as i64);
        self.a.cmp_rm(Rsi, M(Rdx, OB_TYPE));
        self.a.jcc(Cc::NE, slow);
    }
    /// Inline values (perf-runtime `shape.rs`): rsi = borrowed value at index `hint` if
    /// obj keeps its attributes inline and its shape has `name` there; jumps to
    /// `notvalues` if obj is not in values mode, to `slow` otherwise. Clobbers rcx/rdx.
    fn inline_values(&mut self, obj: R, hint: u64, name: i64, slow: Label, notvalues: Label) {
        // managed word = shape | 1; value i lives at obj + shape.voff + 8*i (perf2)
        let (_id_off, len_off, keys_off, voff_off) = crate::shape::jit_layout();
        let h = hint as i32;
        self.a.load8(Rdx, M(obj, GC_BITS));
        self.a.and_ri(Rdx, crate::alloc::GC_BIT_MANAGED as i32);
        self.a.jcc(Cc::E, notvalues);
        self.a.load(Rdx, M(obj, MANAGED_DICT));
        self.a.mov_rr(Rsi, Rdx);
        self.a.and_ri(Rsi, 1);
        self.a.jcc(Cc::E, notvalues);
        self.a.cmp32_mi(M(Rdx, len_off - 1), h);
        self.a.jcc(Cc::BE, slow);
        self.a.load(Rcx, M(Rdx, keys_off - 1));
        self.a.mov_ri(Rsi, name);
        self.a.cmp_rm(Rsi, M(Rcx, h * 8));
        self.a.jcc(Cc::NE, slow);
        self.a.load32(Rcx, M(Rdx, voff_off - 1));
        self.a.load_idx(Rsi, obj, Rcx, 1, h * 8);
        self.a.test_rr(Rsi, Rsi);
        self.a.jcc(Cc::E, slow);
    }
    /// The instance in rax does not have `name` itself (so a class attribute is not
    /// shadowed): jumps or falls through to `ok` (bound right after by the caller),
    /// else jumps to `slow`. Inline values: the
    /// shape must lack the name (the entry memoises the id of a shape known to lack
    /// it); a materialised dict must not contain it. Clobbers rcx, rdx, rsi, rdi, r14
    /// and caller-saved registers on the helper paths; rax is preserved.
    fn lacks_name(&mut self, e: &crate::interp::cache::Entry, name: i64, slow: Label, ok: Label) {
        let not_values = self.a.label();
        let (id_off, _len, _keys, _voff) = crate::shape::jit_layout();
        self.a.load8(Rdx, M(Rax, GC_BITS));
        self.a.and_ri(Rdx, crate::alloc::GC_BIT_MANAGED as i32);
        self.a.jcc(Cc::E, not_values);
        self.a.load(Rdx, M(Rax, MANAGED_DICT));
        self.a.test_ri(Rdx, 1);
        self.a.jcc(Cc::E, not_values);
        // managed word = shape | 1 (perf2)
        self.a.lea(Rdx, M(Rdx, -1));
        self.a.load32(Rcx, M(Rdx, id_off));
        let memo = (e as *const crate::interp::cache::Entry as i64) + E_MEMO as i64;
        self.a.mov_ri(Rsi, memo);
        self.a.cmp32_rm(Rcx, M(Rsi, 0));
        self.a.jcc(Cc::E, ok);
        self.a.mov_rr(R14, Rax);
        self.a.mov_rr(Rdi, Rdx);
        self.a.mov_rr(Rdx, Rsi);
        self.a.mov_ri(Rsi, name);
        self.call(addr!(h::h_shape_lacks_memo));
        self.a.test_rr(Rax, Rax);
        self.a.mov_rr(Rax, R14);
        self.a.jcc(Cc::E, slow);
        self.a.jmp(ok);
        self.a.bind(not_values);
        self.managed_dict(Rax, slow, ok);
        self.a.cmp_mi(M(Rdx, MA_USED), 0);
        self.a.jcc(Cc::E, ok);
        self.a.mov_rr(R14, Rax);
        self.a.mov_rr(Rdi, Rdx);
        self.a.mov_ri(Rsi, name);
        self.call(addr!(h::h_dict_lacks));
        self.a.test_rr(Rax, Rax);
        self.a.mov_rr(Rax, R14);
        self.a.jcc(Cc::E, slow);
    }

    /// rsi = value at entry `hint` of the dict in rdx if its key is `name`, else slow.
    /// Clobbers rdx.
    fn dict_hinted(&mut self, hint: u64, name: i64, slow: Label) -> bool {
        let l = layout();
        let disp = hint as i64 * l.entry_size as i64;
        if hint >= (1 << 20) || disp + 64 > i32::MAX as i64 {
            return false;
        }
        self.a.load(Rdx, M(Rdx, MA_KEYS));
        self.a.cmp_mi(M(Rdx, l.entries_len), hint as i32);
        self.a.jcc(Cc::BE, slow);
        self.a.load(Rdx, M(Rdx, l.entries_ptr));
        self.a.mov_ri(Rsi, name);
        self.a.cmp_rm(Rsi, M(Rdx, disp as i32 + l.entry_key));
        self.a.jcc(Cc::NE, slow);
        self.a.load(Rsi, M(Rdx, disp as i32 + l.entry_value));
        true
    }

    /// LoadAttr / LoadMethod / StoreAttr with an inline fast path specialised on the
    /// cache state observed at compile time; the generic helper is the slow path.
    fn attr_op(&mut self, ins: &Instr, d: i32, hf: usize, errd: i32) {
        use crate::interp::cache::kind;
        let pc = ins.pc as usize;
        let es = self.tables.poly(pc);
        let ep = es.as_ptr() as i64;
        let name = self.cd.names[ins.arg as usize].as_ptr() as i64;
        let done = self.a.label();
        // (perf5) LoadMethod of a plain attribute: the attribute cases of LoadAttr, then
        // NULL in the self slot (at `adone`)
        let adone = if ins.op == Op::LoadMethod { self.a.label() } else { done };
        // one guarded case per filled entry (polymorphic inline cache), then the helper
        for e in es {
        let slow = self.a.label();
        let snap = e.read();
        let mut inline = false;
        let mut attr_kind = false;
        if let Some(s) = snap {
            let tag = s.a as u32;
            let obj = self.slot(d - 1);
            match (ins.op, s.kind) {
                (Op::LoadAttr | Op::LoadMethod, kind::INSTANCE_DICT) if s.b < (1 << 20) => {
                    attr_kind = true;
                    self.a.load(Rax, obj);
                    self.guard_tag(Rax, tag, slow);
                    let dict_path = self.a.label();
                    emit_note_attr_read(&mut self.a, Rax); // scale2
                    self.inline_values(Rax, s.b, name, slow, dict_path);
                    self.incref(Rsi);
                    self.a.store(obj, Rsi);
                    self.decref(Rax);
                    self.a.jmp(adone);
                    self.a.bind(dict_path);
                    self.managed_dict(Rax, slow, slow);
                    if self.dict_hinted(s.b, name, slow) {
                        self.incref(Rsi);
                        self.a.store(obj, Rsi);
                        self.decref(Rax);
                        inline = true;
                    }
                }
                (Op::LoadAttr | Op::LoadMethod, kind::SLOT) => {
                    attr_kind = true;
                    self.a.load(Rax, obj);
                    self.guard_tag(Rax, tag, slow);
                    emit_note_attr_read(&mut self.a, Rax); // scale2
                    self.a.load(Rsi, M(Rax, s.c as i32));
                    self.a.test_rr(Rsi, Rsi);
                    self.a.jcc(Cc::E, slow);
                    self.incref(Rsi);
                    self.a.store(obj, Rsi);
                    self.decref(Rax);
                    inline = true;
                }
                (Op::LoadAttr | Op::LoadMethod, kind::TYPE_ATTR) => {
                    attr_kind = true;
                    self.a.load(Rax, obj);
                    self.a.mov_ri(Rcx, s.b as i64);
                    self.a.cmp_rr(Rax, Rcx);
                    self.a.jcc(Cc::NE, slow);
                    self.a.cmp32_mi(M(Rcx, TP_TAG), tag as i32);
                    self.a.jcc(Cc::NE, slow);
                    self.a.mov_ri(Rsi, s.c as i64);
                    self.incref(Rsi);
                    self.a.store(obj, Rsi);
                    self.decref(Rax);
                    inline = true;
                }
                (Op::LoadAttr | Op::LoadMethod, kind::CLASS_ATTR)
                    if unsafe { (*(s.c as *mut PyObject)).ty().tp_descr_get.is_none() } =>
                {
                    attr_kind = true;
                    self.a.load(Rax, obj);
                    self.guard_tag(Rax, tag, slow);
                    self.a.mov_ri(Rsi, s.c as i64);
                    self.incref(Rsi);
                    self.a.store(obj, Rsi);
                    self.decref(Rax);
                    inline = true;
                }
                (Op::LoadAttr | Op::LoadMethod, kind::MODULE) => {
                    attr_kind = true;
                    // guards read from the entry at run time (module dicts change)
                    self.a.load(Rax, obj);
                    self.a.mov_ri(Rcx, crate::types::module::module_type() as i64);
                    self.a.cmp_rm(Rcx, M(Rax, OB_TYPE));
                    self.a.jcc(Cc::NE, slow);
                    self.a.mov_ri(Rcx, e as *const _ as i64);
                    self.a.load32(R8, M(Rcx, E_SEQ));
                    self.a.test_rr(R8, R8); // (low bit tested below)
                    self.a.mov_rr(R9, R8);
                    self.a.and_ri(R9, 1);
                    self.a.jcc(Cc::NE, slow);
                    self.a.cmp32_mi(M(Rcx, E_KIND), kind::MODULE as i32);
                    self.a.jcc(Cc::NE, slow);
                    self.a.load(Rdx, M(Rax, MD_DICT));
                    self.a.cmp_rm(Rdx, M(Rcx, E_B));
                    self.a.jcc(Cc::NE, slow);
                    self.a.test_rr(Rdx, Rdx);
                    self.a.jcc(Cc::E, slow);
                    self.a.load(Rdx, M(Rdx, MA_KEYS));
                    self.a.load(Rdx, M(Rdx, layout().version));
                    self.a.cmp_rm(Rdx, M(Rcx, E_A));
                    self.a.jcc(Cc::NE, slow);
                    self.a.load(Rsi, M(Rcx, E_C));
                    self.a.cmp32_rm(R8, M(Rcx, E_SEQ));
                    self.a.jcc(Cc::NE, slow);
                    self.incref(Rsi);
                    self.a.store(obj, Rsi);
                    self.decref(Rax);
                    inline = true;
                }
                (Op::LoadMethod, kind::METHOD) | (Op::LoadMethod, kind::METHOD_SHADOWABLE) => {
                    let push = self.a.label();
                    self.a.load(Rax, obj);
                    self.guard_tag(Rax, tag, slow);
                    if s.kind == kind::METHOD_SHADOWABLE {
                        self.lacks_name(e, name, slow, push);
                    }
                    self.a.bind(push);
                    self.a.mov_ri(Rsi, s.c as i64);
                    self.incref(Rsi);
                    self.a.store(obj, Rsi);
                    let m = self.slot(d);
                    self.a.store(m, Rax);
                    inline = true;
                }
                (Op::LoadAttr | Op::LoadMethod, kind::CLASS_ATTR_SHADOWABLE)
                    if unsafe { (*(s.c as *mut PyObject)).ty().tp_descr_get.is_none() } =>
                {
                    // (perf5) class attribute (no descriptor) the instance does not
                    // shadow
                    attr_kind = true;
                    let push = self.a.label();
                    self.a.load(Rax, obj);
                    self.guard_tag(Rax, tag, slow);
                    self.lacks_name(e, name, slow, push);
                    self.a.bind(push);
                    self.a.mov_ri(Rsi, s.c as i64);
                    self.incref(Rsi);
                    self.a.store(obj, Rsi);
                    self.decref(Rax);
                    inline = true;
                }
                (Op::LoadAttr | Op::LoadMethod, kind::DATA_DESCR)
                    if unsafe { (*(s.c as *mut PyObject)).is_type(crate::types::property::property_type()) }
                        && super::callpy::routine() != 0 =>
                {
                    attr_kind = true;
                    // (perf5) property whose getter is a Python function with a fast
                    // entry: call the getter through the machine-code call routine with
                    // the stack laid out as [fget, obj] (the spare slot above the stack
                    // holds obj as self)
                    self.a.load(Rax, obj);
                    self.guard_tag(Rax, tag, slow);
                    self.a.mov_ri(Rcx, s.c as i64);
                    self.a.load(Rcx, M(Rcx, crate::types::property::FGET_OFFSET as i32));
                    self.a.test_rr(Rcx, Rcx);
                    self.a.jcc(Cc::E, slow);
                    self.a.mov_ri(Rdx, crate::types::funcobject::function_type() as i64);
                    self.a.cmp_rm(Rdx, M(Rcx, OB_TYPE));
                    self.a.jcc(Cc::NE, slow);
                    self.a.mov_ri(Rdx, crate::interp::function_vectorcall as usize as i64);
                    self.a.cmp_rm(Rdx, M(Rcx, FUNC_VECTORCALL));
                    self.a.jcc(Cc::NE, slow);
                    self.a.load(Rdx, M(Rcx, FUNC_CODE));
                    self.a.test_rr(Rdx, Rdx);
                    self.a.jcc(Cc::E, slow);
                    self.a.load(Rdx, M(Rdx, CODE_DATA));
                    self.a.test_rr(Rdx, Rdx);
                    self.a.jcc(Cc::E, slow);
                    self.a.cmp_mi(M(Rdx, CD_FAST_ENTRY), 0);
                    self.a.jcc(Cc::E, slow);
                    let selfm = self.slot(d);
                    self.a.store(selfm, Rax);
                    self.incref(Rcx);
                    self.a.store(obj, Rcx);
                    self.set_lasti(pc as u32);
                    self.lea_sp(Rdi, d - 1);
                    self.a.mov_ri(Rsi, 0);
                    self.call(super::callpy::routine());
                    self.check(pc as u32, d - 1);
                    inline = true;
                }
                (Op::LoadAttr | Op::LoadMethod, kind::DATA_DESCR)
                    if unsafe { getset_getter(s.c as *mut PyObject).is_some() } =>
                {
                    // (perf5) getset descriptor (frame.f_back, code.co_filename, ...):
                    // its C getter directly (the type guard implies the descriptor's
                    // owner check)
                    attr_kind = true;
                    let (get, closure) = unsafe { getset_getter(s.c as *mut PyObject).unwrap() };
                    self.a.load(Rax, obj);
                    self.guard_tag(Rax, tag, slow);
                    self.set_lasti(pc as u32);
                    self.a.mov_rr(Rdi, Rax);
                    self.a.mov_ri(Rsi, closure as i64);
                    self.call(get);
                    self.a.test_rr(Rax, Rax);
                    let err = self.error_label(pc as u32, d);
                    self.a.jcc(Cc::E, err);
                    self.a.mov_rr(Rsi, Rax);
                    self.a.load(Rax, obj);
                    self.a.store(obj, Rsi);
                    self.decref(Rax);
                    inline = true;
                }
                (Op::LoadAttr | Op::LoadMethod, kind::DATA_DESCR)
                    if crate::modules::_collections::tuplegetter_index(s.c as *mut PyObject).is_some_and(|i| (0..1 << 20).contains(&i)) =>
                {
                    // (perf5) namedtuple field (`_tuplegetter`): the item itself
                    attr_kind = true;
                    let idx = crate::modules::_collections::tuplegetter_index(s.c as *mut PyObject).unwrap() as i32;
                    self.a.load(Rax, obj);
                    self.guard_tag(Rax, tag, slow);
                    self.a.load(Rcx, M(Rax, OB_TYPE));
                    self.a.load(Rdx, M(Rcx, offset_of!(PyTypeObject, tp_flags) as i32));
                    self.a.test_ri(Rdx, crate::ffi::Py_TPFLAGS_TUPLE_SUBCLASS as i32);
                    self.a.jcc(Cc::E, slow);
                    self.a.cmp_mi(M(Rax, LIST_SIZE), idx);
                    self.a.jcc(Cc::LE, slow);
                    self.a.load(Rsi, M(Rax, TUPLE_ITEMS + 8 * idx));
                    self.a.test_rr(Rsi, Rsi);
                    self.a.jcc(Cc::E, slow);
                    self.incref(Rsi);
                    self.a.store(obj, Rsi);
                    self.decref(Rax);
                    inline = true;
                }
                (Op::LoadAttr | Op::LoadMethod, kind::TYPE_ATTR_META)
                | (Op::LoadAttr | Op::LoadMethod, kind::TYPE_STATICMETHOD)
                | (Op::LoadMethod, kind::TYPE_CLASSMETHOD) => {
                    // (perf5) attribute of class b under a type-derived metatype:
                    // identity and both version tags
                    self.a.load(Rax, obj);
                    self.a.mov_ri(Rcx, s.b as i64);
                    self.a.cmp_rr(Rax, Rcx);
                    self.a.jcc(Cc::NE, slow);
                    self.a.cmp32_mi(M(Rax, TP_TAG), (s.a & 0xffff_ffff) as u32 as i32);
                    self.a.jcc(Cc::NE, slow);
                    self.a.load(Rcx, M(Rax, OB_TYPE));
                    self.a.cmp32_mi(M(Rcx, TP_TAG), (s.a >> 32) as u32 as i32);
                    self.a.jcc(Cc::NE, slow);
                    self.a.mov_ri(Rsi, s.c as i64);
                    if s.kind != kind::TYPE_ATTR_META {
                        let off = std::mem::offset_of!(crate::types::funcobject::StaticOrClassMethod, callable) as i32;
                        self.a.load(Rsi, M(Rsi, off));
                        self.a.test_rr(Rsi, Rsi);
                        self.a.jcc(Cc::E, slow);
                    }
                    self.incref(Rsi);
                    self.a.store(obj, Rsi);
                    if s.kind == kind::TYPE_CLASSMETHOD {
                        // the class moves into the self slot
                        let m = self.slot(d);
                        self.a.store(m, Rax);
                    } else {
                        attr_kind = true;
                        self.decref(Rax);
                    }
                    inline = true;
                }
                (Op::StoreAttr, kind::SLOT) => {
                    let val = self.slot(d - 2);
                    let mt = self.a.label();
                    self.a.load(Rax, obj);
                    self.guard_tag(Rax, tag, slow);
                    self.a.mov_ri(Rdi, (&raw const crate::threads::MULTI_THREADED) as i64);
                    self.a.cmp_mi(M(Rdi, 0), 0);
                    self.a.jcc(Cc::NE, mt);
                    self.a.load(Rsi, val);
                    self.a.load(Rdx, M(Rax, s.c as i32));
                    self.a.store(M(Rax, s.c as i32), Rsi);
                    self.a.mov_rr(R14, Rax);
                    self.xdecref(Rdx);
                    self.a.mov_rr(Rax, R14);
                    self.decref(Rax);
                    self.a.jmp(done);
                    // (scale2) with threads: swap atomically (slots are written without the
                    // object lock), then release the old value at once or through QSBR
                    self.a.bind(mt);
                    self.a.load(Rsi, val);
                    self.a.xchg_mr(M(Rax, s.c as i32), Rsi);
                    self.a.mov_rr(R14, Rax);
                    self.release_attr(R14, Rsi);
                    self.a.mov_rr(Rax, R14);
                    self.decref(Rax);
                    inline = true;
                }
                (Op::StoreAttr, kind::INSTANCE_DICT) if s.b < (1 << 20) => {
                    let val = self.slot(d - 2);
                    let mt = self.a.label();
                    self.a.load(Rax, obj);
                    self.guard_tag(Rax, tag, slow);
                    // (perf5) attribute-adding store (`__init__`): with the object in the
                    // one shape that lacks the name and room left, store the value and
                    // move to the child shape, while single-threaded (as tier 2 does)
                    let trans = if s.c != 0 && crate::interp::cache::type_alive_with_tag(s.c, tag) {
                        crate::shape::transitions_adding(s.c as *mut PyTypeObject, name as *mut PyObject, s.b as usize)
                    } else {
                        Vec::new()
                    };
                    if trans.len() == 1 {
                        let (parent, child) = trans[0];
                        let not_add = self.a.label();
                        self.a.load8(Rdx, M(Rax, GC_BITS));
                        self.a.and_ri(Rdx, crate::alloc::GC_BIT_MANAGED as i32);
                        self.a.jcc(Cc::E, not_add);
                        self.a.load(Rdx, M(Rax, MANAGED_DICT));
                        self.a.mov_ri(Rsi, parent as i64 | 1);
                        self.a.cmp_rr(Rdx, Rsi);
                        self.a.jcc(Cc::NE, not_add);
                        self.a.mov_ri(Rdi, (&raw const crate::threads::MULTI_THREADED) as i64);
                        self.a.cmp_mi(M(Rdi, 0), 0);
                        self.a.jcc(Cc::NE, slow);
                        self.a.load8(Rcx, M(Rax, crate::shape::capacity_offset()));
                        self.a.cmp_ri(Rcx, s.b as i32);
                        self.a.jcc(Cc::BE, slow);
                        let voff = unsafe { (*parent).voff } as i32;
                        self.a.load(Rsi, val);
                        self.a.store(M(Rax, voff + 8 * s.b as i32), Rsi);
                        self.a.mov_ri(Rsi, child as i64 | 1);
                        self.a.store(M(Rax, MANAGED_DICT), Rsi);
                        self.decref(Rax);
                        self.a.jmp(done);
                        self.a.bind(not_add);
                    }
                    // inline values (jit3): replace the existing attribute in place
                    // (rsi = old value, rdx = values word)
                    let dict_path = self.a.label();
                    self.a.mov_ri(Rdi, (&raw const crate::threads::MULTI_THREADED) as i64);
                    self.a.cmp_mi(M(Rdi, 0), 0);
                    self.a.jcc(Cc::NE, mt);
                    self.inline_values(Rax, s.b, name, slow, dict_path);
                    // rcx = shape.voff: the slot is at rax + rcx + 8*hint (perf2 layout)
                    self.a.load(Rdx, val);
                    self.a.store_idx(Rax, Rcx, 1, 8 * s.b as i32, Rdx);
                    self.a.mov_rr(R14, Rax);
                    self.a.mov_rr(Rax, Rsi);
                    self.decref(Rax);
                    self.a.mov_rr(Rax, R14);
                    self.decref(Rax);
                    self.a.jmp(done);
                    // (scale2) with threads: the same under the object's mutex (writers on
                    // other threads may change the shape), taken inline when free; the
                    // old value is released after the unlock (qsbr::release_attr)
                    {
                        let unlock_slow = self.a.label();
                        let unlock_dict = self.a.label();
                        self.a.bind(mt);
                        self.a.mov_rr(R14, Rax);
                        self.a.mov_ri(Rax, 0);
                        self.a.mov_ri(R11, 1);
                        self.a.lock_cmpxchg8(M(R14, OB_MUTEX), R11);
                        self.a.jcc(Cc::NE, slow);
                        self.inline_values(R14, s.b, name, unlock_slow, unlock_dict);
                        self.a.load(Rdx, val);
                        self.a.store_idx(R14, Rcx, 1, 8 * s.b as i32, Rdx);
                        self.ob_unlock(R14);
                        self.release_attr(R14, Rsi);
                        self.a.mov_rr(Rax, R14);
                        self.decref(Rax);
                        self.a.jmp(done);
                        self.a.bind(unlock_dict);
                        self.ob_unlock(R14);
                        self.a.mov_rr(Rax, R14);
                        self.a.jmp(dict_path);
                        self.a.bind(unlock_slow);
                        self.ob_unlock(R14);
                        self.a.jmp(slow);
                    }
                    self.a.bind(dict_path);
                    self.managed_dict(Rax, slow, slow);
                    self.a.mov_rr(R14, Rax);
                    self.a.mov_rr(Rdi, Rdx);
                    self.a.mov_ri(Rsi, s.b as i64);
                    self.a.mov_ri(Rdx, name);
                    self.a.load(Rcx, val);
                    self.call(addr!(h::h_dict_replace));
                    self.a.test_rr(Rax, Rax);
                    self.a.jcc(Cc::NE, slow);
                    self.a.mov_rr(Rax, R14);
                    self.decref(Rax);
                    inline = true;
                }
                _ => {}
            }
        }
        if inline {
            self.a.jmp(if attr_kind { adone } else { done });
        }
        self.a.bind(slow);
        }
        self.helper_sp(ins, d, hf, Some(name), Some(ep), errd);
        if ins.op == Op::LoadMethod {
            self.a.jmp(done);
            self.a.bind(adone);
            let m = self.slot(d);
            self.a.store_i(m, 0);
        }
        self.a.bind(done);
    }

    /// LoadGlobal: inline read of the dict entry the cache points at (jit5; see
    /// specialize::load_global): seqlock, keys versions of globals (and builtins),
    /// value from the entry (with threads too, see below; scale2).
    fn load_global(&mut self, ins: &Instr, d: i32) {
        use crate::interp::cache::kind;
        let e = self.tables.entry(ins.pc as usize) as *const _ as i64;
        let name = self.cd.names[ins.arg as usize].as_ptr() as i64;
        let slow = self.a.label();
        let done = self.a.label();
        let have = self.a.label();
        let in_builtins = self.a.label();
        let dict_t = crate::types::dict::dict_type() as i64;
        let kv = std::mem::offset_of!(crate::types::dict::DictTable, keys_version) as i32;
        let lay = layout();
        let (ep, esz, evalue) = (lay.entries_ptr, lay.entry_size, lay.entry_value);
        self.a.mov_ri(Rcx, e);
        self.a.load32(R8, M(Rcx, E_SEQ));
        self.a.mov_rr(R9, R8);
        self.a.and_ri(R9, 1);
        self.a.jcc(Cc::NE, slow);
        self.a.cmp32_mi(M(Rcx, E_KIND), kind::GLOBAL as i32);
        self.a.jcc(Cc::NE, slow);
        // globals: exact dict with the recorded key layout
        self.a.load(Rax, M(Rbx, FRAME_GLOBALS));
        self.a.mov_ri(Rdi, dict_t);
        self.a.cmp_rm(Rdi, M(Rax, OB_TYPE));
        self.a.jcc(Cc::NE, slow);
        self.a.mov_rr(Rdi, Rax);
        self.a.load(Rax, M(Rax, MA_KEYS));
        self.a.test_rr(Rax, Rax);
        self.a.jcc(Cc::E, slow);
        self.a.load(Rdx, M(Rax, kv));
        self.a.cmp_rm(Rdx, M(Rcx, E_A));
        self.a.jcc(Cc::NE, slow);
        self.a.load(R9, M(Rcx, E_C));
        self.a.test_ri(R9, 1);
        self.a.jcc(Cc::NE, in_builtins);
        self.a.bind(have);
        // rax = table, rdi = its dict, rdx = its keys version (checked), r9 = ix << 1 | flag.
        // (scale2) Also with threads: the entry is read without the dict lock, validated
        // by the table's sequence lock (wseq even and unchanged, keys version still the
        // recorded one) as in dict::value_at_if; a value replaced meanwhile is released
        // through QSBR when the table is marked shared_read (else only the owner can have
        // written it: allowed when this thread is the owner, the slow path otherwise).
        {
            let ok = self.a.label();
            self.a.mov_rr(R10, Rdx);
            self.a.load(R11, M(Rax, WSEQ));
            self.a.test_ri(R11, 1);
            self.a.jcc(Cc::NE, slow);
            self.a.cmp_rm(R10, M(Rax, kv));
            self.a.jcc(Cc::NE, slow);
            self.a.mov_ri(Rdx, (&raw const crate::threads::MULTI_THREADED) as i64);
            self.a.cmp_mi(M(Rdx, 0), 0);
            self.a.jcc(Cc::E, ok);
            self.a.cmp8_mi(M(Rax, SHARED_READ), 0);
            self.a.jcc(Cc::NE, ok);
            self.a.cmp_rm(R15, M(Rdi, OB_TID));
            self.a.jcc(Cc::NE, slow);
            self.a.bind(ok);
        }
        self.a.shr_ri(R9, 1);
        self.a.cmp_rm(R9, M(Rax, NENTRIES));
        self.a.jcc(Cc::AE, slow);
        self.a.imul_rri(R9, R9, esz);
        self.a.load(Rdx, M(Rax, ep));
        self.a.add_rr(Rdx, R9);
        self.a.load(Rsi, M(Rdx, evalue));
        self.a.test_rr(Rsi, Rsi);
        self.a.jcc(Cc::E, slow);
        self.a.cmp_rm(R11, M(Rax, WSEQ));
        self.a.jcc(Cc::NE, slow);
        self.a.mov_ri(Rdx, crate::types::lazyimport::lazy_import_type() as i64);
        self.a.cmp_rm(Rdx, M(Rsi, OB_TYPE));
        self.a.jcc(Cc::E, slow);
        self.a.cmp32_rm(R8, M(Rcx, E_SEQ));
        self.a.jcc(Cc::NE, slow);
        self.incref(Rsi);
        let m = self.slot(d);
        self.a.store(m, Rsi);
        self.a.jmp(done);
        // builtins: exact dict with the recorded key layout (globals checked above)
        self.a.bind(in_builtins);
        self.a.load(Rax, M(Rbx, FRAME_BUILTINS));
        self.a.mov_ri(Rdi, dict_t);
        self.a.cmp_rm(Rdi, M(Rax, OB_TYPE));
        self.a.jcc(Cc::NE, slow);
        self.a.mov_rr(Rdi, Rax);
        self.a.load(Rax, M(Rax, MA_KEYS));
        self.a.test_rr(Rax, Rax);
        self.a.jcc(Cc::E, slow);
        self.a.load(Rdx, M(Rax, kv));
        self.a.cmp_rm(Rdx, M(Rcx, E_B));
        self.a.jcc(Cc::NE, slow);
        self.a.jmp(have);
        self.a.bind(slow);
        self.set_lasti(ins.pc);
        self.a.mov_rr(Rdi, Rbx);
        self.lea_sp(Rsi, d);
        self.a.mov_ri(Rdx, name);
        self.a.mov_ri(Rcx, e);
        self.call(addr!(h::h_load_global));
        self.check(ins.pc, d);
        self.a.bind(done);
    }

    /// BinaryOp with inline compact-int add/sub/mul/and/or/xor and float add/sub/mul.
    fn binary_op(&mut self, ins: &Instr, d: i32) -> Result<(), String> {
        let op: BinOp = unsafe { std::mem::transmute(ins.arg as u8) };
        use BinOp::*;
        let int_ok = matches!(
            op,
            Add | InplaceAdd | Subtract | InplaceSubtract | Multiply | InplaceMultiply | And | InplaceAnd | Or | InplaceOr | Xor | InplaceXor
        );
        let float_ok = matches!(
            op,
            Add | InplaceAdd | Subtract | InplaceSubtract | Multiply | InplaceMultiply | TrueDivide | InplaceTrueDivide
        );
        if !int_ok && !float_ok {
            self.prof_slow(ins.pc, d - 2, Some(d - 1));
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
        if int_ok {
            self.compact_int(Rax, Rdi, Rdx, not_int);
            self.compact_int(Rcx, Rsi, Rdx, not_int);
            self.prof(ins.pc, (prof::INT as u16) | (prof::INT as u16) << 8);
            match op {
                Add | InplaceAdd => self.a.add_rr(Rdi, Rsi),
                Subtract | InplaceSubtract => self.a.sub_rr(Rdi, Rsi),
                Multiply | InplaceMultiply => self.a.imul_rr(Rdi, Rsi),
                And | InplaceAnd => self.a.and_rr(Rdi, Rsi),
                Or | InplaceOr => self.a.or_rr(Rdi, Rsi),
                _ => self.a.xor_rr(Rdi, Rsi),
            }
            self.a.call_label(self.rt_box_int);
            self.a.jmp(have_result);
        }
        self.a.bind(not_int);
        if float_ok {
            // operands: exact float, or compact int converted exactly (|v| < 2**30);
            // int x int only for true division (the int path handled the rest)
            let ft = crate::types::float::float_type() as i64;
            let both_int = self.a.label();
            self.a.mov_ri(R8, 0);
            for (obj, x, sh) in [(Rax, XMM0, 0), (Rcx, XMM1, 8)] {
                let is_float = self.a.label();
                let next = self.a.label();
                self.a.mov_ri(Rdx, ft);
                self.a.cmp_rm(Rdx, M(obj, OB_TYPE));
                self.a.jcc(Cc::E, is_float);
                self.compact_int(obj, Rdi, Rdx, slow);
                self.prof(ins.pc, (prof::INT as u16) << sh);
                self.a.cvtsi2sd(x, Rdi);
                self.a.jmp(next);
                self.a.bind(is_float);
                self.prof(ins.pc, (prof::FLOAT as u16) << sh);
                self.a.movsd_load(x, M(obj, FVAL));
                self.a.mov_ri(R8, 1);
                self.a.bind(next);
            }
            if !matches!(op, TrueDivide | InplaceTrueDivide) {
                // int x int with these ops is the int path's business (it overflowed
                // its checks, e.g. not compact): only mixed/float operands here
                self.a.test_rr(R8, R8);
                self.a.jcc(Cc::E, slow);
            }
            let _ = both_int;
            match op {
                Add | InplaceAdd => self.a.addsd(XMM0, XMM1),
                Subtract | InplaceSubtract => self.a.subsd(XMM0, XMM1),
                Multiply | InplaceMultiply => self.a.mulsd(XMM0, XMM1),
                _ => {
                    // division by zero raises: leave to the helper
                    self.a.xorpd(XMM2, XMM2);
                    self.a.ucomisd(XMM1, XMM2);
                    self.a.jcc(Cc::E, slow); // also taken for NaN (ZF=1): helper handles it
                    self.a.divsd(XMM0, XMM1);
                }
            }
            self.a.call_label(self.rt_box_float);
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
        self.prof_slow(ins.pc, d - 2, Some(d - 1));
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
        let not_int = self.a.label();
        let release = self.a.label();
        self.a.load(Rax, ma);
        self.a.load(Rcx, mb);
        self.compact_int(Rax, Rdi, Rdx, not_int);
        self.compact_int(Rcx, Rsi, Rdx, slow);
        self.prof(ins.pc, (prof::INT as u16) | (prof::INT as u16) << 8);
        self.a.cmp_rr(Rdi, Rsi);
        self.a.setcc(cc, R14);
        self.a.jmp(release);
        // float x float (IEEE semantics: comparisons with NaN are false, != is true)
        self.a.bind(not_int);
        self.a.mov_ri(Rdx, crate::types::float::float_type() as i64);
        self.a.cmp_rm(Rdx, M(Rax, OB_TYPE));
        self.a.jcc(Cc::NE, slow);
        self.a.cmp_rm(Rdx, M(Rcx, OB_TYPE));
        self.a.jcc(Cc::NE, slow);
        self.a.movsd_load(XMM0, M(Rax, FVAL));
        self.a.movsd_load(XMM1, M(Rcx, FVAL));
        self.prof(ins.pc, (prof::FLOAT as u16) | (prof::FLOAT as u16) << 8);
        match cop {
            Py_LT => {
                self.a.ucomisd(XMM1, XMM0);
                self.a.setcc(Cc::A, R14);
            }
            Py_LE => {
                self.a.ucomisd(XMM1, XMM0);
                self.a.setcc(Cc::AE, R14);
            }
            Py_GT => {
                self.a.ucomisd(XMM0, XMM1);
                self.a.setcc(Cc::A, R14);
            }
            Py_GE => {
                self.a.ucomisd(XMM0, XMM1);
                self.a.setcc(Cc::AE, R14);
            }
            Py_EQ => {
                self.a.ucomisd(XMM0, XMM1);
                self.a.setcc(Cc::E, R14);
                self.a.setcc(Cc::NP, Rdx);
                self.a.and_rr(R14, Rdx);
            }
            _ => {
                self.a.ucomisd(XMM0, XMM1);
                self.a.setcc(Cc::NE, R14);
                self.a.setcc(Cc::P, Rdx);
                self.a.or_rr(R14, Rdx);
            }
        }
        self.a.bind(release);
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
        self.prof_slow(ins.pc, d - 2, Some(d - 1));
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

/// (jit6) The routines below exist once per process; every unit binds its routine
/// labels to a 13-byte `jmp` to them (each unit used to carry its own copies, which
/// multiplied the instruction-cache footprint of hot code).
pub(crate) fn emit_routines(a: &mut Asm, box_int: Label, box_float: Label, free_num: Label) {
    static SHARED: std::sync::OnceLock<Option<[usize; 3]>> = std::sync::OnceLock::new();
    let shared = SHARED.get_or_init(|| {
        let mut b = Asm::new();
        let l = [b.label(), b.label(), b.label()];
        emit_routines_body(&mut b, l[0], l[1], l[2]);
        b.resolve().ok()?;
        let base = super::mem::install(&b.buf, |_, _| {})?;
        Some([base + b.label_pos(l[0])?, base + b.label_pos(l[1])?, base + b.label_pos(l[2])?])
    });
    match shared {
        Some(s) => {
            for (l, &t) in [box_int, box_float, free_num].into_iter().zip(s.iter()) {
                a.bind(l);
                a.mov_ri(R11, t as i64);
                a.jmp_r(R11);
            }
        }
        None => emit_routines_body(a, box_int, box_float, free_num),
    }
}

/// Shared per-function routines (called with `call`, SysV-like clobbers):
/// * box_int:   rdi = i64 value -> rax = new int object (small-int cache, inline
///              TLAB allocation of 1-digit ints, else the runtime helper)
/// * box_float: xmm0 = f64 -> rax = new float object
/// * free_num:  rdi = object whose local refcount just dropped to zero: inline free
///              for exact int/float, else the generic path.
fn emit_routines_body(a: &mut Asm, box_int: Label, box_float: Label, free_num: Label) {
    let tl = crate::tlab::jit_layout();
    let tls = tl.tls_offset as i32;
    let heads = tl.heads as i32;
    let counts = tl.counts as i32;
    let int_t = crate::types::int::int_type();
    let float_t = crate::types::float::float_type();
    let int_size = unsafe { ((*int_t).tp_basicsize as usize).max(LV_DIGIT as usize + 4) };
    let ci = crate::tlab::class_index(int_size) as i32;
    let cf = crate::tlab::class_index(std::mem::size_of::<PyFloatObject>()) as i32;
    let small = crate::interp::specialize::small_int_table() as i64;

    // header: tid, (flags,mutex,gc_bits)=0 + ref_local=1, ref_shared=0, type
    let init_header = |a: &mut Asm, tp: i64| {
        a.store(M(Rax, OB_TID), R15);
        a.mov_ri(Rsi, 1i64 << 32);
        a.store(M(Rax, 8), Rsi);
        a.store_i(M(Rax, OB_REF_SHARED), 0);
        a.mov_ri(Rsi, tp);
        a.store(M(Rax, OB_TYPE), Rsi);
    };
    let pop_block = |a: &mut Asm, class: i32, empty: Label| {
        a.lea(Rdx, M(R15, tls));
        a.load(Rax, M(Rdx, heads + 8 * class));
        a.test_rr(Rax, Rax);
        a.jcc(Cc::E, empty);
        a.load(Rsi, M(Rax, 0));
        a.store(M(Rdx, heads + 8 * class), Rsi);
        a.add32_mi(M(Rdx, counts + 4 * class), -1);
    };

    // ---- box_int
    a.bind(box_int);
    {
        let big = a.label();
        let helper = a.label();
        a.lea(Rcx, M(Rdi, 5));
        a.cmp_ri(Rcx, 1029); // -5..=1024 come from the small int table
        a.jcc(Cc::A, big);
        a.mov_ri(Rax, small);
        a.load_idx(Rax, Rax, Rcx, 8, 0);
        a.ret();
        a.bind(big);
        // rcx = |v|
        a.mov_rr(Rcx, Rdi);
        a.neg(Rcx);
        a.cmov(Cc::S, Rcx, Rdi);
        a.mov_ri(Rdx, 1 << 30);
        a.cmp_rr(Rcx, Rdx);
        a.jcc(Cc::AE, helper);
        pop_block(a, ci, helper);
        init_header(a, int_t as i64);
        a.mov_ri(Rsi, 8);
        a.mov_ri(Rdx, 10);
        a.test_rr(Rdi, Rdi);
        a.cmov(Cc::S, Rsi, Rdx);
        a.store(M(Rax, LV_TAG), Rsi);
        a.store32(M(Rax, LV_DIGIT), Rcx);
        a.ret();
        a.bind(helper);
        a.jmp_abs(addr!(h::h_new_int));
    }
    // ---- box_float
    a.bind(box_float);
    {
        let helper = a.label();
        pop_block(a, cf, helper);
        init_header(a, float_t as i64);
        a.movsd_store(M(Rax, FVAL), XMM0);
        a.ret();
        a.bind(helper);
        a.jmp_abs(addr!(h::h_new_float));
    }
    // ---- free_num
    a.bind(free_num);
    {
        let generic = a.label();
        let ok = a.label();
        a.load(Rax, M(Rdi, OB_TYPE));
        a.mov_ri(Rcx, int_t as i64);
        a.cmp_rr(Rax, Rcx);
        a.jcc(Cc::E, ok);
        a.mov_ri(Rcx, float_t as i64);
        a.cmp_rr(Rax, Rcx);
        a.jcc(Cc::NE, generic);
        a.bind(ok);
        a.cmp_mi(M(Rdi, OB_REF_SHARED), 0);
        a.jcc(Cc::NE, generic);
        a.load8(Rax, M(Rdi, GC_BITS));
        a.test_rr(Rax, Rax);
        a.jcc(Cc::NE, generic);
        a.mov_rr(Rax, Rdi);
        a.and_ri(Rax, !(crate::tlab::PAGE as i32 - 1));
        a.load32(Rax, M(Rax, tl.page_class as i32));
        a.lea(Rdx, M(R15, tls));
        a.cmp32_idx_i(Rdx, Rax, 4, counts, tl.limit as i32);
        a.jcc(Cc::AE, generic);
        a.load_idx(Rcx, Rdx, Rax, 8, heads);
        a.store(M(Rdi, 0), Rcx);
        a.store_idx(Rdx, Rax, 8, heads, Rdi);
        a.add32_idx_i(Rdx, Rax, 4, counts, 1);
        a.ret();
        a.bind(generic);
        a.jmp_abs(addr!(h::h_merge_zero));
    }
}

impl<'a> Gen<'a> {
    /// A template generator whose instruction labels / exit belong to another code
    /// generator (tier 2 emits tier-1 templates for the instructions it does not
    /// specialise). `exit`: label of the code that leaves the compiled code with the
    /// status in eax (the tier-1 epilogue contract).
    pub(crate) fn new_embedded(a: Asm, an: &'a Analysis, cd: &'a CodeData, nlp: usize, flags: u32, tables: &'a crate::interp::cache::Tables, ilabel: Vec<Label>, exit: Label) -> Gen<'a> {
        let mut a = a;
        let (rt_box_int, rt_box_float, rt_free_num) = (a.label(), a.label(), a.label());
        Gen { a, nlp: nlp as i32, an, cd, tables, ilabel, exit, stubs: Vec::new(), rt_box_int, rt_box_float, rt_free_num, flags, no_fuse: true, drain: false }
    }
    /// Emit the out-of-line stubs and shared routines; resolve labels.
    pub(crate) fn finish(mut self) -> Result<Asm, String> {
        let nlp = self.nlp;
        let exit = self.exit;
        while let Some(s) = self.stubs.pop() {
            s(&mut self.a, exit, nlp);
        }
        emit_routines(&mut self.a, self.rt_box_int, self.rt_box_float, self.rt_free_num);
        self.a.resolve()?;
        Ok(self.a)
    }
    /// Emit the inline incref / decref / xdecref sequences (r11 clobbered; decref's
    /// slow path clobbers caller-saved registers).
    /// (scale1) Jump to `fail` unless the object in `r` is referenced exactly once, by
    /// this thread (owner, local count 1, nothing in the shared field): then no other
    /// thread can reach it, and it may be changed or read without its lock.
    pub(crate) fn emit_exclusive_check(&mut self, r: R, fail: Label) {
        self.a.cmp_rm(R15, M(r, OB_TID));
        self.a.jcc(Cc::NE, fail);
        self.a.cmp32_mi(M(r, OB_REF_LOCAL), 1);
        self.a.jcc(Cc::NE, fail);
        self.a.cmp_mi(M(r, OB_REF_SHARED), 0);
        self.a.jcc(Cc::NE, fail);
    }
    pub(crate) fn emit_incref(&mut self, r: R) {
        self.incref(r)
    }
    pub(crate) fn emit_decref(&mut self, r: R) {
        self.decref(r)
    }
    pub(crate) fn emit_xdecref(&mut self, r: R) {
        self.xdecref(r)
    }
    /// Push an out-of-line stub (emitted by `finish`).
    pub(crate) fn add_stub(&mut self, s: Stub) {
        self.stubs.push(s)
    }
}

/// Compile a code object. Err: not compilable (stays in the interpreter).
pub fn compile(cd: &CodeData, nlocalsplus: usize, flags: u32, tables: &crate::interp::cache::Tables) -> Result<Output, String> {
    compile_mode(cd, nlocalsplus, flags, tables, false)
}

/// `compile`, optionally in drain mode (jit6, see the YieldValue template).
pub fn compile_mode(cd: &CodeData, nlocalsplus: usize, flags: u32, tables: &crate::interp::cache::Tables, drain: bool) -> Result<Output, String> {
    let an = super::analysis::analyze(&cd.code, &cd.exception_table)?;
    let mut a = Asm::new();
    let ilabel: Vec<Label> = (0..an.instrs.len()).map(|_| a.label()).collect();
    let exit = a.label();
    let (rt_box_int, rt_box_float, rt_free_num) = (a.label(), a.label(), a.label());
    let mut g = Gen { a, nlp: nlocalsplus as i32, an: &an, cd, tables, ilabel, exit, stubs: Vec::new(), rt_box_int, rt_box_float, rt_free_num, flags, no_fuse: false, drain };

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
        prev_yield = ins.op == Op::YieldValue || ins.op == Op::ReturnGenerator;
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
    emit_routines(&mut g.a, rt_box_int, rt_box_float, rt_free_num);
    g.a.resolve()?;
    Ok(Output { asm: g.a, entries })
}

// ---------------------------------------------------------------- sharded increfs (scale1)

/// Process-wide routine for the incref of a sharded object (crate::sharded) from JIT
/// code. Entered by `call` with the object in rdi (the caller saves rdi); preserves
/// every other general register and xmm0-15, whatever the stack alignment at entry.
pub(crate) fn incref_sharded_routine() -> Option<usize> {
    static R: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    *R.get_or_init(|| preserving_routine(h::h_incref_sharded as usize))
}

/// (scale1) Same calling convention: unlock the `ob_mutex` of the object in rdi when
/// waiters may be parked on it (tier-2 inline Unlock).
pub(crate) fn unlock_slow_routine() -> Option<usize> {
    static R: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    *R.get_or_init(|| preserving_routine(h::h_ob_unlock_slow as usize))
}

/// (scale2) Same calling convention: deferred release (QSBR) of the object in rdi
/// (tier-2 ReleaseAttr).
pub(crate) fn decref_deferred_routine() -> Option<usize> {
    static R: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    *R.get_or_init(|| preserving_routine(h::h_decref_deferred as usize))
}

/// A routine calling `target(rdi)` that preserves every other general register and
/// xmm0-15 and aligns the stack itself.
fn preserving_routine(target: usize) -> Option<usize> {
    {
        let mut a = Asm::new();
        a.push(Rbp);
        a.mov_rr(Rbp, Rsp);
        for r in [Rax, Rcx, Rdx, Rsi, R8, R9, R10, R11] {
            a.push(r);
        }
        a.and_ri(Rsp, -16);
        a.sub_ri(Rsp, 128);
        for k in 0..16u8 {
            a.movsd_store(M(Rsp, 8 * k as i32), X(k));
        }
        a.call_abs(target);
        for k in 0..16u8 {
            a.movsd_load(X(k), M(Rsp, 8 * k as i32));
        }
        a.mov_rr(Rsp, Rbp);
        a.sub_ri(Rsp, 64);
        for r in [R11, R10, R9, R8, Rsi, Rdx, Rcx, Rax] {
            a.pop(r);
        }
        a.pop(Rbp);
        a.ret();
        a.resolve().ok()?;
        crate::interp::jit::mem::install(&a.buf, |_, _| {})
    }
}

/// (scale2) Inline `qsbr::note_attr_read` for the object in `r` (not r11): a thread
/// other than the owner sets the sticky XREAD bit before its first lock-free read of
/// the object's attribute storage (`lock or` is a full barrier, so the slot load that
/// follows is ordered after it). Clobbers r11 only.
pub(crate) fn emit_note_attr_read(a: &mut Asm, r: R) {
    const OB_FLAGS: i32 = offset_of!(PyObject, ob_flags) as i32;
    let done = a.label();
    a.cmp_rm(R15, M(r, OB_TID));
    a.jcc(Cc::E, done);
    a.load8(R11, M(r, OB_FLAGS));
    a.test_ri(R11, crate::qsbr::XREAD as i32);
    a.jcc(Cc::NE, done);
    a.lock_or16_mi(M(r, OB_FLAGS), crate::qsbr::XREAD);
    a.bind(done);
}

/// Out-of-line part of an inline incref when the object in `r` (not r11) is owned by
/// another thread: a sharded object's reference goes to the thread's delta table
/// (through `incref_sharded_routine`), any other object's to its shared field.
/// Clobbers r11 only; continues at `done`.
pub(crate) fn emit_nonowner_incref(a: &mut Asm, r: R, done: Label) {
    const OB_FLAGS: i32 = offset_of!(PyObject, ob_flags) as i32;
    match incref_sharded_routine() {
        Some(rt) => {
            let sh = a.label();
            let call = a.label();
            a.load8(R11, M(r, OB_FLAGS));
            a.test_ri(R11, crate::sharded::SHARDED as i32);
            a.jcc(Cc::NE, sh);
            a.lock_add_mi(M(r, OB_REF_SHARED), 1 << REF_SHARED_SHIFT);
            a.jmp(done);
            a.bind(sh);
            if let Some(l) = crate::sharded::jit_layout() {
                // inline `sharded::add(r, 1)` (r15 = thread pointer): look the object
                // up in its bucket, else take a slot whose delta is zero, else call
                a.cmp8_mi(M(R15, l.ready), 0);
                a.jcc(Cc::E, call);
                a.mov_rr(R11, r);
                a.shr_ri(R11, 4);
                a.imul_rri(R11, R11, l.mul);
                a.shr_ri(R11, l.shift);
                a.and_ri(R11, l.bucket_mask);
                a.shl_ri(R11, l.row_shift);
                a.lea_idx(R11, R15, R11, 1, l.keys);
                let found: Vec<Label> = (0..l.ways).map(|_| a.label()).collect();
                let free: Vec<Label> = (0..l.ways).map(|_| a.label()).collect();
                for w in 0..l.ways {
                    a.cmp_rm(r, M(R11, 8 * w as i32));
                    a.jcc(Cc::E, found[w]);
                }
                for w in 0..l.ways {
                    a.cmp_mi(M(R11, l.vals_delta + 8 * w as i32), 0);
                    a.jcc(Cc::E, free[w]);
                }
                a.jmp(call);
                for w in 0..l.ways {
                    a.bind(found[w]);
                    a.add_mi(M(R11, l.vals_delta + 8 * w as i32), 1);
                    a.jmp(done);
                    a.bind(free[w]);
                    a.store(M(R11, 8 * w as i32), r);
                    a.store_i(M(R11, l.vals_delta + 8 * w as i32), 1);
                    a.jmp(done);
                }
            }
            a.bind(call);
            a.push(Rdi);
            if r != Rdi {
                a.mov_rr(Rdi, r);
            }
            a.mov_ri(R11, rt as i64);
            a.call_r(R11);
            a.pop(Rdi);
            a.jmp(done);
        }
        None => {
            a.lock_add_mi(M(r, OB_REF_SHARED), 1 << REF_SHARED_SHIFT);
            a.jmp(done);
        }
    }
}
