//! Tier-2 IR builder: abstract interpretation of the bytecode over a typed operand
//! stack and typed locals, producing low-level IR.
//!
//! State model (see notes/jit.md, "tier 2"):
//! * Locals have a fixed representation for the whole compilation unit: `Frame`
//!   (the boxed object in the frame slot is always current) or unboxed `I64`/`F64`
//!   (the value lives in a "home" vreg; the frame slot is synchronised lazily, at
//!   deoptimisation and exits, and holds a placeholder/stale object otherwise).
//! * Operand-stack entries are abstract values: already in their frame slot
//!   (`InFrame`), an object pointer in a vreg (owned, or borrowed from a local), an
//!   unboxed int/float/bool, a lazy condition, or a constant. They are written to the
//!   frame ("materialised") only when needed: before a template instruction, at joins,
//!   and in deoptimisation stubs.
//! * Speculation uses the type profiles recorded by tier 1 and the inline caches;
//!   every guard carries a snapshot of the abstract state at the start of its
//!   bytecode instruction, from which the deoptimisation stub rebuilds the frame.
//! * Loop headers are joins whose entry state is assumed; when a backedge does not
//!   satisfy the assumption it is weakened and the unit is rebuilt (a few rounds).

use super::ir::*;
use super::layout::*;
use crate::ffi::*;
use crate::interp::cache::{prof, Tables};
use crate::interp::jit::analysis::{effect, is_terminator, Analysis};
use crate::interp::jit::x64::Cc;
use crate::types::code::CodeData;
use rpy_compiler::bytecode::{self as bc, BinOp, Op};

/// Known type of an object value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ty {
    Unknown,
    /// exact type (pointer to the type object)
    Exact(usize),
    /// exact (heap) type whose version tag was verified to be this value since the
    /// last instruction that can run arbitrary code (type tags are unique per type)
    Tag(u32),
}

impl Ty {
    fn meet(self, o: Ty) -> Ty {
        if self == o { self } else { Ty::Unknown }
    }
    /// knowledge implied: does `self` (actual) satisfy the assumption `a`?
    fn satisfies(self, a: Ty) -> bool {
        a == Ty::Unknown || self == a
    }
    fn is(self, tp: *mut PyTypeObject) -> bool {
        self == Ty::Exact(tp as usize)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Own {
    Owned,
    /// borrowed from the frame slot of local #i
    Borrowed(u32),
    /// borrowed from local #i of the callee being inlined
    Inl(u32),
    /// constant / immortal: no reference counting needed while in use
    Const,
}

/// Abstract value of an operand-stack entry.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum AV {
    InFrame(Ty),
    Obj(V, Own, Ty),
    I64(V),
    F64(V),
    Bool(V),
    Cond(Cond),
    Const(usize),
    Null,
}

/// Where a failed check goes: deoptimisation, or a slow-path block.
#[derive(Clone, Copy)]
enum Fail {
    Deopt(SnapId),
    Block(BlockId),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Def {
    No,
    Maybe,
    Yes,
}

impl Def {
    fn meet(self, o: Def) -> Def {
        if self == o { self } else { Def::Maybe }
    }
}

/// Representation of a local for the whole unit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rep {
    Frame,
    I64,
    F64,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LS {
    Frame(Ty),
    Unboxed { dirty: bool, def: Def },
}

#[derive(Clone, PartialEq, Debug)]
pub struct State {
    pub stack: Vec<AV>,
    pub locals: Vec<LS>,
    /// per local: vreg holding the slot's object pointer (Frame locals)
    pub ptr: Vec<Option<V>>,
    /// per local: vreg holding the unboxed value of a Frame local (int: false/float: true)
    pub unb: Vec<Option<(V, bool)>>,
}

/// Compilation unit description.
pub struct Unit<'a> {
    pub cd: &'a CodeData,
    pub an: &'a Analysis,
    pub tables: &'a Tables,
    pub nlp: usize,
    /// instruction index (into an.instrs) where the unit is entered
    pub entry: usize,
    /// number of argument slots bound at function entry
    pub nargs: usize,
    pub globals: *mut PyObject,
    pub builtins: *mut PyObject,
    /// address of the "invalidated" byte of the code being built
    pub invalid_flag: usize,
    pub flags: u32,
    /// fold global/builtin values into constants (off after repeated invalidation)
    pub fold_globals: bool,
    /// (jit5) earlier code of this unit was discarded: monomorphic attribute sites
    /// get a generic slow path instead of a deoptimising guard (no deopt churn when
    /// a site turns polymorphic after compilation)
    pub cautious: bool,
}

impl Unit<'_> {
    /// (jit6) generic path instead of a deoptimising guard at this instruction: it
    /// trapped repeatedly in earlier code (CodeCaches::traps), or, when no trap was
    /// recorded, earlier code of the unit was discarded
    fn cautious_at(&self, ins: crate::interp::jit::analysis::Instr) -> bool {
        let t = self.cd.caches.traps.lock().unwrap_or_else(|e| e.into_inner());
        if t.iter().any(|&(p, n)| n >= 2 && (p == ins.start || p == ins.pc)) {
            return true;
        }
        self.cautious && t.is_empty()
    }
}

struct BcBlock {
    first: usize,
    last: usize,
    succs: Vec<usize>,
    npreds: u32,
    rpo: u32,
    header: bool,
}

/// Bytecode-level control flow graph.
struct Cfg {
    blocks: Vec<BcBlock>,
    block_of: Vec<u32>,
    order: Vec<usize>,
}

fn jumps(op: Op) -> bool {
    op.is_jump()
}

/// Block-ending instruction of the unit's CFG: a generator unit (`resumes` non-empty)
/// leaves through `YieldValue` and comes back through a separate entry.
fn ends_block(op: Op, is_gen: bool) -> bool {
    is_terminator(op) || (is_gen && matches!(op, Op::YieldValue | Op::ReturnGenerator))
}

/// Instruction indices where a generator unit is re-entered after a yield (empty for
/// other units).
fn resume_points(u: &Unit) -> Vec<usize> {
    use rpy_compiler::bytecode::co as cof;
    if u.entry != 0 || u.flags & (cof::GENERATOR | cof::COROUTINE | cof::ASYNC_GENERATOR | cof::ITERABLE_COROUTINE) == 0 {
        return Vec::new();
    }
    let an = u.an;
    (0..an.instrs.len()).filter(|&i| i > 0 && matches!(an.instrs[i - 1].op, Op::YieldValue | Op::ReturnGenerator) && an.depth[i] >= 0 && an.depth[i - 1] >= 0).collect()
}

fn build_cfg(an: &Analysis, entry: usize, resumes: &[usize]) -> Cfg {
    let n = an.instrs.len();
    let is_gen = !resumes.is_empty();
    let mut leader = vec![false; n + 1];
    leader[0] = true;
    leader[entry] = true;
    for &r in resumes {
        leader[r] = true;
    }
    for i in 0..n {
        let ins = an.instrs[i];
        if an.is_target[i] {
            leader[i] = true;
        }
        if jumps(ins.op) || ends_block(ins.op, is_gen) {
            leader[i + 1] = true;
            if jumps(ins.op) {
                leader[an.index_of[ins.arg as usize] as usize] = true;
            }
        }
    }
    let mut blocks = Vec::new();
    let mut block_of = vec![u32::MAX; n];
    let mut i = 0;
    while i < n {
        let first = i;
        let mut j = i;
        while j + 1 < n && !leader[j + 1] {
            j += 1;
        }
        for k in first..=j {
            block_of[k] = blocks.len() as u32;
        }
        blocks.push(BcBlock { first, last: j, succs: Vec::new(), npreds: 0, rpo: u32::MAX, header: false });
        i = j + 1;
    }
    for b in 0..blocks.len() {
        let ins = an.instrs[blocks[b].last];
        let mut s = Vec::new();
        if jumps(ins.op) {
            s.push(block_of[an.index_of[ins.arg as usize] as usize] as usize);
        }
        if !ends_block(ins.op, is_gen) && blocks[b].last + 1 < n {
            s.push(block_of[blocks[b].last + 1] as usize);
        }
        blocks[b].succs = s;
    }
    // reverse postorder from the entry blocks (resume entries first, so the main
    // entry comes first in the order); backedges mark loop headers
    let eb = block_of[entry] as usize;
    let mut post = Vec::new();
    let mut state = vec![0u8; blocks.len()]; // 0 new, 1 on stack, 2 done
    let roots: Vec<usize> = resumes.iter().rev().map(|&r| block_of[r] as usize).chain(std::iter::once(eb)).collect();
    for root in roots {
        if state[root] != 0 {
            continue;
        }
        let mut stack: Vec<(usize, usize)> = vec![(root, 0)];
        state[root] = 1;
        while let Some(&mut (b, ref mut k)) = stack.last_mut() {
            if *k < blocks[b].succs.len() {
                let s = blocks[b].succs[*k];
                *k += 1;
                match state[s] {
                    0 => {
                        state[s] = 1;
                        stack.push((s, 0));
                    }
                    1 => blocks[s].header = true,
                    _ => {}
                }
            } else {
                state[b] = 2;
                post.push(b);
                stack.pop();
            }
        }
    }
    post.reverse();
    for (r, &b) in post.iter().enumerate() {
        blocks[b].rpo = r as u32;
    }
    for &b in &post {
        for k in 0..blocks[b].succs.len() {
            let s = blocks[b].succs[k];
            blocks[s].npreds += 1;
        }
    }
    if blocks[eb].header || entry != 0 {
        // the entry is also reached from outside
        blocks[eb].npreds += 1;
    }
    for &r in resumes {
        // entered from a synthetic resume-entry block
        blocks[block_of[r] as usize].npreds += 1;
    }
    Cfg { blocks, block_of, order: post }
}

pub struct Built {
    pub func: Func,
    /// unit has folded global constants (needs invalidation checks)
    pub has_deps: bool,
    /// (dict, key, value or 0 if absent) lookups folded into the code
    pub deps: Vec<(usize, usize, usize)>,
    /// memo cells (Box<AtomicUsize> raw pointers) owned by the code
    pub cells: Vec<usize>,
    /// IR block of each bytecode instruction that starts a block (u32::MAX: none)
    pub instr_block: Vec<u32>,
    pub reps: Vec<Rep>,
    pub homes: Vec<V>,
}

struct B<'a> {
    u: &'a Unit<'a>,
    cfg: Cfg,
    /// (jit6) further instructions an instruction consumed (fused UnpackSequence + StoreFast)
    skip_extra: usize,
    f: Func,
    cur: BlockId,
    st: State,
    pending: Vec<Vec<State>>,
    entry_state: Vec<Option<State>>,
    ir_block: Vec<BlockId>,
    assume: &'a mut Vec<Option<State>>,
    restart: bool,
    reps: Vec<Rep>,
    homes: Vec<V>,
    /// per local: kinds of values stored (1 int, 2 float, 4 other)
    store_kinds: Vec<u8>,
    has_deps: bool,
    /// (dict, key) dependencies of folded globals
    deps: Vec<(usize, usize, usize)>,
    /// per-site memo cells referenced by the code (leaked boxes, freed with the code)
    cells: Vec<usize>,
    closed: bool,
    /// inlining in progress (callee context)
    inl: Option<InlCtx>,
    /// callee analyses / units referenced through `u` while inlining
    inl_an: Vec<Box<Analysis>>,
    inl_units: Vec<Box<Unit<'static>>>,
    /// contexts of the callees being inlined around the current one (nested inlining)
    inl_outer: Vec<InlCtx>,
    /// code data of the root unit and of every callee being inlined (no recursion)
    inl_cds: Vec<usize>,
    /// an inconsistent join was met: abandon the compilation
    fatal: Option<&'static str>,
    /// generator unit: YieldValue suspends natively and resumes at its own entry
    is_gen: bool,
}

/// State of an inlined call being built.
struct InlCtx {
    func: usize,
    /// stack depth of the caller below the call
    base: usize,
    /// callee locals (AV::Null: unbound)
    locals: Vec<AV>,
    cont: BlockId,
    res: V,
    err: SnapId,
    /// an instruction needed a template: give up inlining
    failed: bool,
    /// inline snapshots were made (the result arrives in `res`)
    snapped: bool,
}

const SK_INT: u8 = 1;
const SK_FLOAT: u8 = 2;
const SK_OTHER: u8 = 4;

fn int_t() -> usize {
    crate::types::int::int_type() as usize
}
fn float_t() -> usize {
    crate::types::float::float_type() as usize
}
fn list_t() -> usize {
    crate::types::list::list_type() as usize
}
fn tuple_t() -> usize {
    crate::types::tuple::tuple_type() as usize
}
fn bool_t() -> usize {
    crate::types::bool_::bool_type() as usize
}
fn none_p() -> usize {
    crate::types::none::none().as_ptr() as usize
}
fn true_p() -> usize {
    crate::types::bool_::true_obj().as_ptr() as usize
}
fn false_p() -> usize {
    crate::types::bool_::false_obj().as_ptr() as usize
}

unsafe fn obj_type(p: usize) -> usize {
    unsafe { (*(p as *mut PyObject)).type_ptr() as usize }
}

/// Value of a constant small int (at most 2 digits).
fn const_int(p: usize) -> Option<i64> {
    unsafe {
        if obj_type(p) != int_t() {
            return None;
        }
        crate::interp::specialize::small_int_value(&*(p as *mut PyObject))
    }
}
fn const_float(p: usize) -> Option<f64> {
    unsafe {
        if obj_type(p) != float_t() {
            return None;
        }
        Some(crate::types::float::float_value(&*(p as *mut PyObject)))
    }
}
/// Inlining of small callees (RPYTHON_JIT2_INLINE=0 disables it).
fn inline_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("RPYTHON_JIT2_INLINE").map_or(true, |v| v != "0"))
}

/// Is `tp` (a type pointer recorded in an inline-cache entry) still the live type
/// with version tag `tag`? Tags are unique and never reused, so a freed or modified
/// type cannot match.
fn live_type(tp: usize, tag: u64) -> bool {
    tag != 0 && tag <= u32::MAX as u64 && crate::interp::cache::type_alive_with_tag(tp, tag as u32)
}
fn is_heap_type(tp: usize) -> bool {
    unsafe { (*(tp as *mut PyTypeObject)).tp_flags & Py_TPFLAGS_HEAPTYPE != 0 }
}

impl<'a> B<'a> {
    // ------------------------------------------------------------ emission helpers
    fn v(&mut self, c: Class) -> V {
        self.f.new_v(c)
    }
    fn emit(&mut self, i: Inst) {
        // a decref may free the object and run arbitrary code (__del__, weakref
        // callbacks) that mutates classes: type facts guarded earlier no longer hold
        let forget = matches!(i, Inst::Decref(_) | Inst::XDecref(_)) && !keep_facts();
        self.f.blocks[self.cur as usize].insts.push(i);
        if forget {
            self.forget_tags();
        }
    }
    /// Decref of a value whose type is known to free without running Python code.
    fn emit_leaf_decref(&mut self, v: V) {
        self.f.blocks[self.cur as usize].insts.push(Inst::Decref(v));
    }
    fn leaf_type(t: Ty) -> bool {
        match t {
            Ty::Exact(p) => p == int_t() || p == float_t() || p == bool_t() || p == crate::types::str::str_type() as usize,
            _ => false,
        }
    }
    fn new_block(&mut self, instr: u32) -> BlockId {
        self.f.blocks.push(Block { insts: Vec::new(), term: Term::None, instr, tsuccs: Vec::new() });
        self.f.blocks.len() as BlockId - 1
    }
    fn term(&mut self, t: Term) {
        self.f.blocks[self.cur as usize].term = t;
        self.closed = true;
    }
    fn imm(&mut self, x: i64) -> V {
        let v = self.v(Class::G);
        self.emit(Inst::Imm(v, x));
        v
    }
    fn slot_off(&self, k: usize) -> i32 {
        8 * (self.u.nlp + k) as i32
    }
    fn local_off(&self, i: u32) -> i32 {
        8 * i as i32
    }

    // ------------------------------------------------------------ snapshots
    fn mat_of(&self, av: &AV) -> Mat {
        match *av {
            AV::InFrame(_) => Mat::InFrame,
            AV::Obj(v, Own::Owned, _) => Mat::Owned(v),
            AV::Obj(v, _, _) => Mat::Borrowed(v),
            AV::I64(v) => Mat::BoxInt(v),
            AV::F64(v) => Mat::BoxFloat(v),
            AV::Bool(v) => Mat::BoxBool(v),
            AV::Cond(_) => unreachable!("lazy condition in snapshot"),
            AV::Const(p) => Mat::Const(p),
            AV::Null => Mat::Const(0),
        }
    }
    /// Turn lazy conditions on the stack into bool vregs (they cannot be described
    /// in snapshots).
    fn force_conds(&mut self) {
        for k in 0..self.st.stack.len() {
            if let AV::Cond(c) = self.st.stack[k] {
                let v = self.v(Class::G);
                self.emit(Inst::SetCond(v, c));
                self.st.stack[k] = AV::Bool(v);
            }
        }
    }
    fn dirty_locals(&self) -> Vec<(u32, Mat, bool)> {
        let mut out = Vec::new();
        for (i, l) in self.st.locals.iter().enumerate() {
            if let LS::Unboxed { dirty: true, def } = *l {
                if def == Def::No {
                    continue;
                }
                let h = self.homes[i];
                let m = if self.reps[i] == Rep::F64 { Mat::BoxFloat(h) } else { Mat::BoxInt(h) };
                out.push((i as u32, m, def == Def::Maybe));
            }
        }
        out
    }
    /// Snapshot of the current state for a deoptimisation resuming at instruction `i`.
    fn snap(&mut self, i: usize, kind: SnapKind) -> SnapId {
        self.force_conds();
        let ins = self.u.an.instrs[i];
        if let Some(c) = &self.inl {
            // inside an inlined callee: describe the callee frame (all its locals and
            // its part of the value stack); the caller continues in compiled code
            let kind = if kind == SnapKind::Error { SnapKind::InlineError } else { SnapKind::InlineDeopt };
            let stack = self.st.stack[c.base..].iter().map(|a| self.mat_of(a)).collect();
            let locals = c.locals.iter().enumerate().map(|(k, a)| (k as u32, self.mat_of(a), false)).collect();
            let inl = Some(InlSnap { func: c.func, res: c.res, cont: c.cont, err: c.err });
            self.inl.as_mut().unwrap().snapped = true;
            self.f.snaps.push(Snapshot { resume: ins.start, lasti: ins.pc, stack, locals, kind, inl });
            return self.f.snaps.len() as SnapId - 1;
        }
        let stack = self.st.stack.iter().map(|a| self.mat_of(a)).collect();
        let locals = self.dirty_locals();
        self.f.snaps.push(Snapshot { resume: ins.start, lasti: ins.pc, stack, locals, kind, inl: None });
        self.f.snaps.len() as SnapId - 1
    }
    /// Snapshot for resuming at the instruction after `i` with the current state.
    fn snap_next(&mut self, i: usize) -> SnapId {
        self.force_conds();
        let resume = self.u.an.instrs.get(i + 1).map_or(self.u.an.instrs[i].pc + 1, |n| n.start);
        let stack = self.st.stack.iter().map(|a| self.mat_of(a)).collect();
        let locals = self.dirty_locals();
        self.f.snaps.push(Snapshot { resume, lasti: self.u.an.instrs[i].pc, stack, locals, kind: SnapKind::Deopt, inl: None });
        self.f.snaps.len() as SnapId - 1
    }

    // ------------------------------------------------------------ values
    fn push(&mut self, a: AV) {
        self.st.stack.push(a);
    }
    fn pop(&mut self) -> AV {
        self.st.stack.pop().expect("tier2: stack underflow")
    }
    fn peek(&self, n: usize) -> AV {
        self.st.stack[self.st.stack.len() - n]
    }

    /// Store `a` into stack slot k as an owned object.
    fn materialize_at(&mut self, k: usize) {
        let a = self.st.stack[k];
        let off = self.slot_off(k);
        let ty = self.ty_of(&a);
        match a {
            AV::InFrame(_) => return,
            AV::Null => self.emit(Inst::StoreImm(Base::Slots, off, 0)),
            _ => {
                let v = self.obj_owned(a);
                self.emit(Inst::Store(Base::Slots, off, v));
            }
        }
        self.st.stack[k] = AV::InFrame(ty);
    }
    fn materialize_stack(&mut self) {
        for k in 0..self.st.stack.len() {
            self.materialize_at(k);
        }
    }
    fn ty_of(&self, a: &AV) -> Ty {
        match *a {
            AV::InFrame(t) | AV::Obj(_, _, t) => t,
            AV::I64(_) => Ty::Exact(int_t()),
            AV::F64(_) => Ty::Exact(float_t()),
            AV::Bool(_) | AV::Cond(_) => Ty::Exact(bool_t()),
            AV::Const(p) => Ty::Exact(unsafe { obj_type(p) }),
            AV::Null => Ty::Unknown,
        }
    }
    /// An owned object reference for the abstract value (boxing / increfs as needed).
    /// InFrame entries must not be passed (their slot position matters).
    fn obj_owned(&mut self, a: AV) -> V {
        match a {
            AV::Obj(v, Own::Owned, _) => v,
            AV::Obj(v, _, _) => {
                self.emit(Inst::Incref(v));
                v
            }
            AV::Const(p) => {
                let v = self.imm(p as i64);
                if unsafe { !(*(p as *mut PyObject)).is_immortal() } {
                    self.emit(Inst::Incref(v));
                }
                v
            }
            AV::I64(x) => {
                let v = self.v(Class::G);
                self.emit(Inst::Box(BoxK::Int, v, x));
                v
            }
            AV::F64(x) => {
                let v = self.v(Class::G);
                self.emit(Inst::Box(BoxK::Float, v, x));
                v
            }
            AV::Bool(x) => {
                let v = self.v(Class::G);
                self.emit(Inst::Box(BoxK::Bool, v, x));
                v
            }
            AV::Cond(c) => {
                let x = self.v(Class::G);
                self.emit(Inst::SetCond(x, c));
                let v = self.v(Class::G);
                self.emit(Inst::Box(BoxK::Bool, v, x));
                v
            }
            AV::Null => self.imm(0),
            AV::InFrame(_) => unreachable!(),
        }
    }
    /// Pop the top entry as an object pointer (any ownership).
    fn pop_obj(&mut self) -> (V, Own, Ty) {
        let k = self.st.stack.len() - 1;
        let a = self.pop();
        match a {
            AV::InFrame(t) => {
                let v = self.v(Class::G);
                let off = self.slot_off(k);
                self.emit(Inst::Load(v, Base::Slots, off));
                (v, Own::Owned, t)
            }
            AV::Obj(v, o, t) => (v, o, t),
            AV::Const(p) => {
                let v = self.imm(p as i64);
                (v, Own::Const, Ty::Exact(unsafe { obj_type(p) }))
            }
            other => {
                let t = self.ty_of(&other);
                (self.obj_owned(other), Own::Owned, t)
            }
        }
    }
    /// Object pointer of the entry at depth n from the top (without popping).
    fn peek_ptr(&mut self, n: usize) -> V {
        let k = self.st.stack.len() - n;
        match self.st.stack[k] {
            AV::InFrame(t) => {
                let v = self.v(Class::G);
                let off = self.slot_off(k);
                self.emit(Inst::Load(v, Base::Slots, off));
                // the slot still owns it; keep it as an owned vreg entry
                self.st.stack[k] = AV::Obj(v, Own::Owned, t);
                v
            }
            AV::Obj(v, _, _) => v,
            AV::Const(p) => {
                let v = self.imm(p as i64);
                self.st.stack[k] = AV::Obj(v, Own::Const, Ty::Exact(unsafe { obj_type(p) }));
                v
            }
            AV::Null => self.imm(0),
            other => {
                let t = self.ty_of(&other);
                let v = self.obj_owned(other);
                self.st.stack[k] = AV::Obj(v, Own::Owned, t);
                v
            }
        }
    }
    fn release(&mut self, v: V, o: Own) {
        if o == Own::Owned {
            self.emit(Inst::Decref(v));
        }
    }
    /// Drop an abstract value (release owned references).
    fn discard(&mut self, a: AV, k: usize) {
        match a {
            AV::InFrame(_) => {
                let v = self.v(Class::G);
                let off = self.slot_off(k);
                self.emit(Inst::Load(v, Base::Slots, off));
                self.emit(Inst::XDecref(v));
            }
            AV::Obj(v, Own::Owned, t) if Self::leaf_type(t) => self.emit_leaf_decref(v),
            AV::Obj(v, Own::Owned, _) => self.emit(Inst::Decref(v)),
            _ => {}
        }
    }
    /// Record that the stack entry at depth n is known to have type t.
    fn learn(&mut self, n: usize, t: Ty) {
        let k = self.st.stack.len() - n;
        match &mut self.st.stack[k] {
            AV::InFrame(x) | AV::Obj(_, _, x) => *x = t,
            _ => {}
        }
        // a borrowed local value: the local has that type too
        if let AV::Obj(_, Own::Borrowed(l), _) = self.st.stack[k] {
            if let LS::Frame(x) = &mut self.st.locals[l as usize] {
                *x = t;
            }
        }
    }

    /// i64 value of the entry at depth n (speculating with the profile byte), or None.
    fn as_i64(&mut self, n: usize, bits: u8, snap: SnapId) -> Option<V> {
        let k = self.st.stack.len() - n;
        match self.st.stack[k] {
            AV::I64(v) => Some(v),
            AV::Const(p) => const_int(p).map(|x| self.imm(x)),
            a @ (AV::InFrame(_) | AV::Obj(..)) => {
                let t = self.ty_of(&a);
                if !(t.is(crate::types::int::int_type()) || (t == Ty::Unknown && bits == prof::INT)) {
                    return None;
                }
                if let AV::Obj(_, Own::Borrowed(l), _) = a {
                    if let Some((u, false)) = self.st.unb[l as usize] {
                        return Some(u);
                    }
                }
                let p = self.peek_ptr(n);
                let v = self.v(Class::G);
                self.emit(Inst::UnboxInt(v, p, snap));
                self.learn(n, Ty::Exact(int_t()));
                if let AV::Obj(_, Own::Borrowed(l), _) = self.st.stack[k] {
                    self.st.unb[l as usize] = Some((v, false));
                }
                Some(v)
            }
            _ => None,
        }
    }
    /// f64 value of the entry at depth n. `mixed`: ints are acceptable (converted).
    fn as_f64(&mut self, n: usize, bits: u8, snap: SnapId, mixed: bool) -> Option<V> {
        let k = self.st.stack.len() - n;
        match self.st.stack[k] {
            AV::F64(v) => Some(v),
            AV::I64(v) if mixed => {
                // exact conversion needs |v| < 2**53
                let lim = 1i64 << 53;
                self.emit(Inst::Guard(Cond::Cmp(v, Opnd::I(lim), Cc::G), snap));
                self.emit(Inst::Guard(Cond::Cmp(v, Opnd::I(-lim), Cc::L), snap));
                let f = self.v(Class::F);
                self.emit(Inst::I2F(f, v));
                Some(f)
            }
            AV::Const(p) => {
                if let Some(x) = const_float(p) {
                    let f = self.v(Class::F);
                    self.emit(Inst::FImm(f, x));
                    return Some(f);
                }
                if mixed {
                    if let Some(x) = const_int(p) {
                        if x.abs() < (1 << 53) {
                            let f = self.v(Class::F);
                            self.emit(Inst::FImm(f, x as f64));
                            return Some(f);
                        }
                    }
                }
                None
            }
            a @ (AV::InFrame(_) | AV::Obj(..)) => {
                let t = self.ty_of(&a);
                let is_f = t.is(crate::types::float::float_type());
                let is_i = t.is(crate::types::int::int_type());
                let unknown = t == Ty::Unknown;
                let prof_f = bits == prof::FLOAT;
                let prof_num = bits & !(prof::FLOAT | prof::INT) == 0 && bits != 0;
                if let AV::Obj(_, Own::Borrowed(l), _) = a {
                    if let Some((u, true)) = self.st.unb[l as usize] {
                        return Some(u);
                    }
                }
                if is_f || (unknown && prof_f) {
                    let p = self.peek_ptr(n);
                    let f = self.v(Class::F);
                    self.emit(Inst::UnboxFloat(f, p, snap));
                    self.learn(n, Ty::Exact(float_t()));
                    if let AV::Obj(_, Own::Borrowed(l), _) = self.st.stack[k] {
                        self.st.unb[l as usize] = Some((f, true));
                    }
                    return Some(f);
                }
                if mixed && (is_i || (unknown && prof_num)) {
                    let p = self.peek_ptr(n);
                    let f = self.v(Class::F);
                    self.emit(Inst::UnboxNumF(f, p, snap));
                    return Some(f);
                }
                None
            }
            _ => None,
        }
    }
    /// Is the entry at depth n float-typed (statically or by profile)?
    fn floatish(&self, n: usize, bits: u8) -> bool {
        let a = self.peek(n);
        match a {
            AV::F64(_) => true,
            AV::Const(p) => const_float(p).is_some(),
            AV::InFrame(t) | AV::Obj(_, _, t) => t.is(crate::types::float::float_type()) || (t == Ty::Unknown && bits == prof::FLOAT),
            _ => false,
        }
    }
    fn intish(&self, n: usize, bits: u8) -> bool {
        let a = self.peek(n);
        match a {
            AV::I64(_) => true,
            AV::Const(p) => const_int(p).is_some(),
            AV::InFrame(t) | AV::Obj(_, _, t) => t.is(crate::types::int::int_type()) || (t == Ty::Unknown && bits == prof::INT),
            _ => false,
        }
    }
    fn numish(&self, n: usize, bits: u8) -> bool {
        if self.floatish(n, bits) || self.intish(n, bits) {
            return true;
        }
        match self.peek(n) {
            AV::InFrame(Ty::Unknown) | AV::Obj(_, _, Ty::Unknown) => bits != 0 && bits & !(prof::FLOAT | prof::INT) == 0,
            _ => false,
        }
    }
    /// Pop and release two consumed operands.
    fn pop2_release(&mut self) {
        for _ in 0..2 {
            let k = self.st.stack.len() - 1;
            let a = self.pop();
            self.discard(a, k);
        }
    }

    // ------------------------------------------------------------ locals
    fn load_local(&mut self, i: u32) -> AV {
        let li = i as usize;
        match self.st.locals[li] {
            LS::Unboxed { .. } => {
                let h = self.homes[li];
                if self.reps[li] == Rep::F64 {
                    let v = self.v(Class::F);
                    self.emit(Inst::Mov(v, h));
                    AV::F64(v)
                } else {
                    let v = self.v(Class::G);
                    self.emit(Inst::Mov(v, h));
                    AV::I64(v)
                }
            }
            LS::Frame(t) => {
                let v = match self.st.ptr[li] {
                    Some(v) => v,
                    None => {
                        let v = self.v(Class::G);
                        let off = self.local_off(i);
                        self.emit(Inst::Load(v, Base::Slots, off));
                        self.st.ptr[li] = Some(v);
                        v
                    }
                };
                AV::Obj(v, Own::Borrowed(i), t)
            }
        }
    }
    /// Stack entries borrowed from local i become owned (before the local changes).
    fn unborrow(&mut self, i: u32) {
        for k in 0..self.st.stack.len() {
            if let AV::Obj(v, Own::Borrowed(l), t) = self.st.stack[k] {
                if l == i {
                    self.emit(Inst::Incref(v));
                    self.st.stack[k] = AV::Obj(v, Own::Owned, t);
                }
            }
        }
    }
    fn note_store_kind(&mut self, i: usize, a: &AV) {
        let k = match *a {
            AV::I64(_) => SK_INT,
            AV::F64(_) => SK_FLOAT,
            AV::Const(p) if const_int(p).is_some() => SK_INT,
            AV::Const(p) if const_float(p).is_some() => SK_FLOAT,
            AV::InFrame(t) | AV::Obj(_, _, t) if t.is(crate::types::float::float_type()) => SK_FLOAT,
            _ => SK_OTHER,
        };
        self.store_kinds[i] |= k;
    }
    fn store_local(&mut self, i: u32, ins_i: usize) {
        let li = i as usize;
        let a = *self.st.stack.last().unwrap();
        self.note_store_kind(li, &a);
        match self.reps[li] {
            Rep::F64 | Rep::I64 => {
                let snap = self.snap(ins_i, SnapKind::Deopt);
                let h = self.homes[li];
                let val = if self.reps[li] == Rep::F64 { self.as_f64(1, 0, snap, false) } else { self.as_i64(1, 0, snap) };
                let val = match val {
                    Some(v) => v,
                    None => {
                        // not representable: take the generic path via the interpreter
                        self.term(Term::Deopt(snap));
                        return;
                    }
                };
                self.emit(Inst::Mov(h, val));
                let k = self.st.stack.len() - 1;
                let a = self.pop();
                self.discard(a, k);
                if let LS::Unboxed { def, .. } = self.st.locals[li] {
                    if def != Def::Yes {
                        self.emit(Inst::MarkDefined(i));
                    }
                }
                self.st.locals[li] = LS::Unboxed { dirty: true, def: Def::Yes };
                self.unborrow(i);
            }
            Rep::Frame => {
                let k = self.st.stack.len() - 1;
                let a = self.pop();
                let ty = self.ty_of(&a);
                let (nv, unb) = match a {
                    AV::InFrame(_) => {
                        let v = self.v(Class::G);
                        let off = self.slot_off(k);
                        self.emit(Inst::Load(v, Base::Slots, off));
                        (v, None)
                    }
                    AV::Obj(_, Own::Borrowed(l), _) if l == i => {
                        // x = x: nothing changes
                        return;
                    }
                    AV::I64(x) => (self.obj_owned(a), Some((x, false))),
                    AV::F64(x) => (self.obj_owned(a), Some((x, true))),
                    _ => (self.obj_owned(a), None),
                };
                self.unborrow(i);
                let old = self.v(Class::G);
                let off = self.local_off(i);
                self.emit(Inst::Load(old, Base::Slots, off));
                self.emit(Inst::Store(Base::Slots, off, nv));
                self.emit(Inst::XDecref(old));
                self.st.locals[li] = LS::Frame(ty);
                self.st.ptr[li] = Some(nv);
                self.st.unb[li] = unb;
            }
        }
    }

    // ------------------------------------------------------------ templates
    /// Emit instruction #i through the tier-1 template generator.
    fn template(&mut self, i: usize) {
        if let Some(c) = self.inl.as_mut() {
            // inlined callees are compiled without templates: give up inlining
            c.failed = true;
            return;
        }
        self.materialize_stack();
        let locals = self.dirty_locals();
        let exit = if locals.is_empty() {
            None
        } else {
            let ins = self.u.an.instrs[i];
            self.f.snaps.push(Snapshot { resume: ins.start, lasti: ins.pc, stack: Vec::new(), locals, kind: SnapKind::TemplateExit, inl: None });
            Some(self.f.snaps.len() as SnapId - 1)
        };
        self.emit(Inst::Template(i as u32, exit));
        let ins = self.u.an.instrs[i];
        if matches!(ins.op, Op::DeleteFast | Op::LoadFastAndClear | Op::MakeCell | Op::CopyFreeVars) {
            // the template changed a frame slot: forget cached pointers / values
            let li = ins.arg as usize;
            if li < self.st.ptr.len() {
                self.st.ptr[li] = None;
                self.st.unb[li] = None;
                if let LS::Frame(_) = self.st.locals[li] {
                    self.st.locals[li] = LS::Frame(Ty::Unknown);
                }
            }
        }
        let (fall, jump) = effect(ins.op, ins.arg);
        let d = self.st.stack.len() as i32;
        if jumps(ins.op) {
            let t = self.cfg.block_of[self.u.an.index_of[ins.arg as usize] as usize] as usize;
            let mut js = self.st.clone();
            let nd = (d + jump).max(0) as usize;
            self.settle_after_template(&mut js, nd);
            self.edge_state(t, js);
            // the template jumps there itself: record the edge for liveness
            let tb = self.ir_block[t];
            self.f.blocks[self.cur as usize].tsuccs.push(tb);
        }
        let nd = (d + fall).max(0) as usize;
        let mut st = std::mem::replace(&mut self.st, State { stack: vec![], locals: vec![], ptr: vec![], unb: vec![] });
        self.settle_after_template(&mut st, nd);
        self.st = st;
        if self.has_deps && !is_terminator(ins.op) {
            let s = self.snap_next(i);
            self.emit(Inst::Guard(Cond::FlagSet(self.u.invalid_flag), s));
        }
    }
    fn settle_after_template(&mut self, st: &mut State, nd: usize) {
        let keep = nd.saturating_sub(3).min(st.stack.len());
        st.stack.truncate(keep);
        while st.stack.len() < nd {
            st.stack.push(AV::InFrame(Ty::Unknown));
        }
        for l in st.locals.iter_mut() {
            if let LS::Frame(Ty::Exact(t)) = *l {
                if is_heap_type(t) {
                    *l = LS::Frame(Ty::Unknown);
                }
            }
            if let LS::Frame(Ty::Tag(_)) = *l {
                *l = LS::Frame(Ty::Unknown);
            }
        }
        for a in st.stack.iter_mut() {
            if let AV::InFrame(Ty::Exact(t)) = *a {
                if is_heap_type(t) {
                    *a = AV::InFrame(Ty::Unknown);
                }
            }
            if let AV::InFrame(Ty::Tag(_)) = *a {
                *a = AV::InFrame(Ty::Unknown);
            }
        }
    }

    // ------------------------------------------------------------ control flow
    fn initial_state(&mut self) -> State {
        self.initial_state_at(self.u.entry)
    }
    /// State when the unit is entered at instruction `at` (everything in the frame).
    fn initial_state_at(&mut self, at: usize) -> State {
        let n = self.u.nlp;
        let mut locals = Vec::with_capacity(n);
        for i in 0..n {
            locals.push(match self.reps[i] {
                Rep::Frame => LS::Frame(Ty::Unknown),
                _ => LS::Unboxed { dirty: false, def: if at == 0 { Def::No } else { Def::Maybe } },
            });
        }
        let d = self.u.an.depth[at].max(0) as usize;
        State { stack: vec![AV::InFrame(Ty::Unknown); d], locals, ptr: vec![None; n], unb: vec![None; n] }
    }

    /// Hand the current state to bytecode block `t` along an edge (materialising if
    /// `t` is a join). Returns the IR block to jump to.
    fn edge(&mut self, t: usize) -> BlockId {
        if self.cfg.blocks[t].npreds > 1 {
            self.materialize_stack();
        }
        let st = self.st.clone();
        self.edge_state(t, st);
        self.ir_block[t]
    }
    fn edge_state(&mut self, t: usize, mut st: State) {
        if self.cfg.blocks[t].npreds > 1 {
            st.ptr.iter_mut().for_each(|p| *p = None);
            st.unb.iter_mut().for_each(|p| *p = None);
        }
        if let Some(es) = &self.entry_state[t] {
            // backedge: the target's assumed state must hold
            if !compatible(&st, es) {
                match meet(es, &st) {
                    Ok(w) => self.assume[t] = Some(w),
                    Err(e) => self.fatal = Some(e),
                }
                self.restart = true;
            }
        } else {
            self.pending[t].push(st);
        }
    }

    fn block_entry(&mut self, b: usize) -> Result<State, String> {
        if b == self.cfg.block_of[self.u.entry] as usize && self.pending[b].is_empty() && self.u.entry == self.cfg.blocks[b].first {
            let mut s = self.initial_state();
            if let Some(a) = &self.assume[b] {
                s = meet(&s, a)?;
            }
            return Ok(s);
        }
        let mut it = std::mem::take(&mut self.pending[b]).into_iter();
        let Some(mut s) = it.next() else { return Err("tier2: block without predecessor state".into()) };
        for o in it {
            s = meet(&s, &o)?;
        }
        if self.cfg.blocks[b].first == self.u.entry {
            let i = self.initial_state();
            s = meet(&s, &i)?;
        }
        if self.cfg.blocks[b].header {
            if let Some(a) = &self.assume[b] {
                s = meet(&s, a)?;
            }
            s.ptr.iter_mut().for_each(|p| *p = None);
            s.unb.iter_mut().for_each(|p| *p = None);
        }
        Ok(s)
    }
}

fn compatible(actual: &State, assumed: &State) -> bool {
    if actual.stack.len() != assumed.stack.len() {
        return false;
    }
    for (a, e) in actual.stack.iter().zip(&assumed.stack) {
        match (a, e) {
            (AV::InFrame(x), AV::InFrame(y)) => {
                if !x.satisfies(*y) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    for (a, e) in actual.locals.iter().zip(&assumed.locals) {
        match (a, e) {
            (LS::Frame(x), LS::Frame(y)) => {
                if !x.satisfies(*y) {
                    return false;
                }
            }
            (LS::Unboxed { dirty: d1, def: f1 }, LS::Unboxed { dirty: d2, def: f2 }) => {
                if *d1 && !*d2 {
                    return false;
                }
                if f1 != f2 && *f2 != Def::Maybe {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

/// Join of two states (an inconsistent pair abandons the compilation: `B::fatal`).
fn meet(a: &State, b: &State) -> Result<State, &'static str> {
    if a.stack.len() != b.stack.len() || a.locals.len() != b.locals.len() {
        return Err("tier2: stack depth mismatch at join");
    }
    let mut bad = None;
    let stack = a
        .stack
        .iter()
        .zip(&b.stack)
        .map(|(x, y)| match (x, y) {
            (AV::InFrame(s), AV::InFrame(t)) => AV::InFrame(s.meet(*t)),
            _ if x == y => *x,
            _ => {
                bad = Some("tier2: unmaterialised stack entry at join");
                *x
            }
        })
        .collect();
    let locals = a
        .locals
        .iter()
        .zip(&b.locals)
        .map(|(x, y)| match (x, y) {
            (LS::Frame(s), LS::Frame(t)) => LS::Frame(s.meet(*t)),
            (LS::Unboxed { dirty: d1, def: f1 }, LS::Unboxed { dirty: d2, def: f2 }) => LS::Unboxed { dirty: *d1 || *d2, def: f1.meet(*f2) },
            _ => {
                bad = Some("tier2: local representation mismatch");
                *x
            }
        })
        .collect();
    let ptr = a.ptr.iter().zip(&b.ptr).map(|(x, y)| if x == y { *x } else { None }).collect();
    let unb = a.unb.iter().zip(&b.unb).map(|(x, y)| if x == y { *x } else { None }).collect();
    match bad {
        Some(e) => Err(e),
        None => Ok(State { stack, locals, ptr, unb }),
    }
}

// ================================================================ instructions

impl<'a> B<'a> {
    fn prof(&self, i: usize) -> (u8, u8) {
        let w = self.u.tables.profile_at(self.u.an.instrs[i].pc as usize);
        ((w & 0xff) as u8, (w >> 8) as u8)
    }
    fn target_block(&self, i: usize) -> usize {
        let ins = self.u.an.instrs[i];
        self.cfg.block_of[self.u.an.index_of[ins.arg as usize] as usize] as usize
    }
    fn next_block(&self, i: usize) -> usize {
        self.cfg.block_of[i + 1] as usize
    }

    /// Build one bytecode block.
    fn build_block(&mut self, b: usize) -> Result<(), String> {
        if let Some(e) = self.fatal {
            return Err(e.into());
        }
        let st = self.block_entry(b)?;
        self.entry_state[b] = Some(st.clone());
        self.st = st;
        self.cur = self.ir_block[b];
        self.closed = false;
        let (first, last) = (self.cfg.blocks[b].first, self.cfg.blocks[b].last);
        let mut i = first;
        while i <= last {
            if self.closed {
                break;
            }
            let consumed_next = self.instr(i, last)?;
            i += 1 + consumed_next as usize + std::mem::take(&mut self.skip_extra);
        }
        if !self.closed {
            // fall through into the next block
            let nb = self.next_block(last);
            let t = self.edge(nb);
            self.term(Term::Jump(t));
        }
        Ok(())
    }

    /// Lower instruction i. Returns true if it also consumed instruction i+1.
    fn instr(&mut self, i: usize, last: usize) -> Result<bool, String> {
        let ins = self.u.an.instrs[i];
        let arg = ins.arg;
        debug_assert_eq!((self.st.stack.len() - self.inl.as_ref().map_or(0, |c| c.base)) as i32, self.u.an.depth[i], "tier2: depth mismatch at {}", ins.pc);
        match ins.op {
            Op::Nop | Op::ExtendedArg => {}
            Op::Resume => {
                let s = self.snap(i, SnapKind::Poll);
                self.emit(Inst::Poll(s));
            }
            Op::LoadFast | Op::LoadFastCheck => {
                if ins.op == Op::LoadFastCheck {
                    if let LS::Frame(_) = self.st.locals[arg as usize] {
                        let a = self.load_local(arg);
                        if let AV::Obj(v, _, _) = a {
                            let s = self.snap(i, SnapKind::Deopt);
                            self.emit(Inst::Guard(Cond::Test(v, Opnd::V(v), Cc::E), s));
                        }
                        self.push(a);
                        return Ok(false);
                    }
                    if let LS::Unboxed { def, .. } = self.st.locals[arg as usize] {
                        if def != Def::Yes {
                            let s = self.snap(i, SnapKind::Deopt);
                            self.emit(Inst::Guard(Cond::CmpMem(Base::Slots, self.local_off(arg), 0, Cc::E), s));
                        }
                    }
                }
                let a = self.load_local(arg);
                self.push(a);
            }
            Op::StoreFast => self.store_local(arg, i),
            Op::LoadConst => {
                let p = self.u.cd.consts[arg as usize].as_ptr() as usize;
                self.push(AV::Const(p));
            }
            Op::PushNull => self.push(AV::Null),
            Op::PopTop => {
                let k = self.st.stack.len() - 1;
                let a = self.pop();
                self.discard(a, k);
            }
            Op::Copy => {
                let n = arg as usize;
                let k = self.st.stack.len() - n;
                let a = self.st.stack[k];
                let c = match a {
                    AV::InFrame(t) => {
                        let v = self.peek_ptr(n);
                        self.emit(Inst::Incref(v));
                        AV::Obj(v, Own::Owned, t)
                    }
                    AV::Obj(v, Own::Owned, t) => {
                        self.emit(Inst::Incref(v));
                        AV::Obj(v, Own::Owned, t)
                    }
                    other => other,
                };
                self.push(c);
            }
            Op::Swap => {
                let n = arg as usize;
                let len = self.st.stack.len();
                // entries change position: InFrame ones move into vregs
                self.peek_ptr_if_inframe(1);
                self.peek_ptr_if_inframe(n);
                self.st.stack.swap(len - 1, len - n);
            }
            Op::LoadGlobal => {
                if let Some(p) = self.folded_global(i) {
                    self.push(AV::Const(p));
                } else {
                    self.template(i);
                }
            }
            Op::BinaryOp => self.binary_op(i)?,
            Op::CompareOp => return self.compare_op(i, last),
            Op::IsOp => {
                let ok = matches!(self.peek(1), AV::InFrame(_) | AV::Obj(..) | AV::Const(_) | AV::Null)
                    && matches!(self.peek(2), AV::InFrame(_) | AV::Obj(..) | AV::Const(_) | AV::Null);
                if !ok {
                    self.template(i);
                } else {
                    let b = self.peek_ptr(1);
                    let a = self.peek_ptr(2);
                    let r = self.v(Class::G);
                    self.emit(Inst::SetCond(r, Cond::Cmp(a, Opnd::V(b), if arg == 1 { Cc::NE } else { Cc::E })));
                    self.pop2_release();
                    self.push(AV::Bool(r));
                }
            }
            Op::UnaryNegative => {
                let (pa, _) = self.prof(i);
                let snap = self.snap(i, SnapKind::Deopt);
                if self.intish(1, pa) {
                    let x = self.as_i64(1, pa, snap).unwrap();
                    let r = self.v(Class::G);
                    self.emit(Inst::NegOv(r, x, snap));
                    let k = self.st.stack.len() - 1;
                    let a = self.pop();
                    self.discard(a, k);
                    self.push(AV::I64(r));
                } else if self.floatish(1, pa) {
                    let x = self.as_f64(1, pa, snap, false).unwrap();
                    let r = self.v(Class::F);
                    self.emit(Inst::FNeg(r, x));
                    let k = self.st.stack.len() - 1;
                    let a = self.pop();
                    self.discard(a, k);
                    self.push(AV::F64(r));
                } else {
                    self.template(i);
                }
            }
            Op::UnaryNot | Op::ToBool => match self.peek(1) {
                AV::Bool(v) => {
                    if ins.op == Op::UnaryNot {
                        self.pop();
                        let r = self.v(Class::G);
                        self.emit(Inst::Alu(Alu::Xor, r, v, Opnd::I(1)));
                        self.push(AV::Bool(r));
                    }
                }
                AV::Cond(c) => {
                    if ins.op == Op::UnaryNot {
                        self.pop();
                        self.push(AV::Cond(negate(c)));
                    }
                }
                AV::InFrame(t) | AV::Obj(_, _, t) if t.is(crate::types::bool_::bool_type()) => {
                    let p = self.peek_ptr(1);
                    let r = self.v(Class::G);
                    let tp = true_p() as i64;
                    let tv = self.imm(tp);
                    self.emit(Inst::SetCond(r, Cond::Cmp(p, Opnd::V(tv), if ins.op == Op::UnaryNot { Cc::NE } else { Cc::E })));
                    let k = self.st.stack.len() - 1;
                    let a = self.pop();
                    self.discard(a, k);
                    self.push(AV::Bool(r));
                }
                AV::I64(v) => {
                    // (jit6) unboxed int: no template (an inlined callee testing an int
                    // argument used to roll back here)
                    let r = self.v(Class::G);
                    self.emit(Inst::SetCond(r, Cond::Test(v, Opnd::V(v), if ins.op == Op::UnaryNot { Cc::E } else { Cc::NE })));
                    self.pop();
                    self.push(AV::Bool(r));
                }
                AV::InFrame(_) | AV::Obj(..) => {
                    // truth of any object: True/False/None without a call, otherwise
                    // the object's truth protocol (may run arbitrary code)
                    let es = self.snap(i, SnapKind::Error);
                    let p = self.peek_ptr(1);
                    let r = self.v(Class::G);
                    self.emit(Inst::Call { addr: h2_truth as usize, args: vec![Opnd::V(p)], fargs: vec![], ret: Some(r), fret: None });
                    self.emit(Inst::Guard(Cond::Cmp(r, Opnd::I(0), Cc::L), es));
                    let k = self.st.stack.len() - 1;
                    let a = self.pop();
                    self.discard(a, k);
                    self.forget_tags();
                    if ins.op == Op::UnaryNot {
                        let n = self.v(Class::G);
                        self.emit(Inst::Alu(Alu::Xor, n, r, Opnd::I(1)));
                        self.push(AV::Bool(n));
                    } else {
                        self.push(AV::Bool(r));
                    }
                }
                _ => self.template(i),
            },
            Op::PopJumpIfFalse | Op::PopJumpIfTrue => {
                let on_true = ins.op == Op::PopJumpIfTrue;
                let c = self.pop_truth_cond();
                match c {
                    Some(c) => {
                        let tb = self.target_block(i);
                        let fb = self.next_block(i);
                        self.branch(c, on_true, tb, fb);
                    }
                    None => {
                        self.template(i);
                        let fb = self.next_block(i);
                        let t = self.edge(fb);
                        self.term(Term::Jump(t));
                    }
                }
            }
            Op::PopJumpIfNone | Op::PopJumpIfNotNone => {
                let (v, own, _) = self.pop_obj();
                let nv = self.imm(none_p() as i64);
                let c = Cond::Cmp(v, Opnd::V(nv), Cc::E);
                // release before branching (pointer compare stays valid)
                let r = self.v(Class::G);
                self.emit(Inst::SetCond(r, c));
                self.release(v, own);
                let tb = self.target_block(i);
                let fb = self.next_block(i);
                self.branch(Cond::Test(r, Opnd::V(r), Cc::NE), ins.op == Op::PopJumpIfNone, tb, fb);
            }
            Op::Jump => {
                let tb = self.target_block(i);
                if arg <= ins.start {
                    // backedge: safepoint poll (+ invalidation check)
                    self.materialize_stack();
                    let s = self.snap(i, SnapKind::Poll);
                    self.emit(Inst::Poll(s));
                    if self.has_deps {
                        let s = self.snap(i, SnapKind::Deopt);
                        self.emit(Inst::Guard(Cond::FlagSet(self.u.invalid_flag), s));
                    }
                }
                let t = self.edge(tb);
                self.term(Term::Jump(t));
            }
            Op::ForIter => self.for_iter(i)?,
            Op::ReturnValue => {
                let (v, own, _) = self.pop_obj();
                let v = match own {
                    Own::Owned => v,
                    _ => {
                        self.emit(Inst::Incref(v));
                        v
                    }
                };
                self.materialize_stack();
                let d = self.st.stack.len() as u32;
                let locals = self.dirty_locals();
                let snap = if locals.is_empty() {
                    None
                } else {
                    let ins = self.u.an.instrs[i];
                    self.f.snaps.push(Snapshot { resume: ins.start, lasti: ins.pc, stack: Vec::new(), locals, kind: SnapKind::TemplateExit, inl: None });
                    Some(self.f.snaps.len() as SnapId - 1)
                };
                self.term(Term::Return(v, d, snap));
            }
            Op::ReturnGenerator if self.is_gen => {
                // the eager prologue runs in the interpreter (interp::call_function);
                // compiled code only ever resumes after it
                let s = self.snap(i, SnapKind::Deopt);
                self.term(Term::Deopt(s));
            }
            Op::YieldValue if self.is_gen && self.inl.is_none() => {
                // suspend: everything goes to the frame; the next resumption enters
                // this unit at the instruction after the yield (`resume_points`)
                let (v, own, _) = self.pop_obj();
                let v = match own {
                    Own::Owned => v,
                    _ => {
                        self.emit(Inst::Incref(v));
                        v
                    }
                };
                self.materialize_stack();
                let d = self.st.stack.len() as u32;
                let locals = self.dirty_locals();
                let ins = self.u.an.instrs[i];
                let next = self.u.an.instrs.get(i + 1).map_or(ins.pc + 1, |n| n.start);
                let yfs = if i > 0 && self.u.an.instrs[i - 1].op == Op::Send { self.u.an.instrs[i - 1].pc as i32 } else { -1 };
                self.f.snaps.push(Snapshot { resume: next, lasti: ins.pc, stack: Vec::new(), locals, kind: SnapKind::TemplateExit, inl: None });
                let snap = self.f.snaps.len() as SnapId - 1;
                self.term(Term::Yield(v, d, snap, yfs));
            }
            Op::BinarySubscr => self.binary_subscr(i),
            Op::LoadAttr => self.load_attr(i),
            Op::LoadMethod => self.load_method(i),
            Op::StoreAttr => self.store_attr(i),
            Op::Call => {
                if self.try_range_iter(i, last) {
                    return Ok(true);
                }
                if self.try_method_builtin(i) {
                    return Ok(false);
                }
                if !self.try_builtin(i) && !self.try_inline(i)? && !self.try_inline_new(i)? {
                    self.template(i);
                }
            }
            Op::StoreSubscr => self.store_subscr(i),
            Op::UnpackSequence => self.unpack_sequence(i, last),
            Op::DeleteFast | Op::LoadFastAndClear => self.template(i),
            _ => {
                self.template(i);
                let ins = self.u.an.instrs[i];
                if is_terminator(ins.op) {
                    self.term(Term::None);
                } else if jumps(ins.op) {
                    let fb = self.next_block(i);
                    let t = self.edge(fb);
                    self.term(Term::Jump(t));
                }
            }
        }
        Ok(false)
    }

    /// Branch(Test(r)) on the flag of a SetCond(r, c) emitted just before (nothing
    /// in between, r used by nothing else) -> Branch(c).
    fn fuse_branch(&mut self) {
        let b = &mut self.f.blocks[self.cur as usize];
        if let Term::Branch(Cond::Test(r, Opnd::V(r2), cc), t, e) = b.term.clone() {
            if r == r2 && matches!(cc, Cc::NE | Cc::E) {
                if let Some(Inst::SetCond(d, c)) = b.insts.last().cloned() {
                    if d == r {
                        b.insts.pop();
                        b.term = if cc == Cc::NE { Term::Branch(c, t, e) } else { Term::Branch(c, e, t) };
                    }
                }
            }
        }
    }

    fn peek_ptr_if_inframe(&mut self, n: usize) {
        if let AV::InFrame(_) = self.peek(n) {
            self.peek_ptr(n);
        }
    }

    /// After a call that may run arbitrary code: type facts that such code can break
    /// (version tags: class mutation; exact heap types: `__class__` assignment) no
    /// longer hold.
    fn forget_tags(&mut self) {
        fn weak(t: Ty) -> Ty {
            match t {
                Ty::Tag(_) => Ty::Unknown,
                Ty::Exact(p) if is_heap_type(p) => Ty::Unknown,
                t => t,
            }
        }
        for l in self.st.locals.iter_mut() {
            if let LS::Frame(t) = *l {
                *l = LS::Frame(weak(t));
            }
        }
        let fix = |a: &mut AV| match *a {
            AV::InFrame(t) => *a = AV::InFrame(weak(t)),
            AV::Obj(v, o, t) => *a = AV::Obj(v, o, weak(t)),
            _ => {}
        };
        self.st.stack.iter_mut().for_each(fix);
        if let Some(c) = self.inl.as_mut() {
            c.locals.iter_mut().for_each(fix);
        }
    }

    /// Truth value of the top of the stack as a condition, popping it, when it can be
    /// tested natively (None: nothing popped).
    fn pop_truth_cond(&mut self) -> Option<Cond> {
        match self.peek(1) {
            AV::Cond(c) => {
                self.pop();
                Some(c)
            }
            AV::Bool(v) | AV::I64(v) => {
                self.pop();
                Some(Cond::Test(v, Opnd::V(v), Cc::NE))
            }
            AV::Const(p) if p == true_p() || p == false_p() || p == none_p() => {
                self.pop();
                let v = self.imm((p == true_p()) as i64);
                Some(Cond::Test(v, Opnd::V(v), Cc::NE))
            }
            AV::InFrame(t) | AV::Obj(_, _, t) if t.is(crate::types::bool_::bool_type()) => {
                let p = self.peek_ptr(1);
                let tv = self.imm(true_p() as i64);
                let k = self.st.stack.len() - 1;
                let a = self.pop();
                self.discard(a, k);
                Some(Cond::Cmp(p, Opnd::V(tv), Cc::E))
            }
            _ => None,
        }
    }

    fn branch(&mut self, c: Cond, on_true: bool, tb: usize, fb: usize) {
        // the condition's vregs are read by the terminator: compute it before any
        // materialisation code that edge() may emit, then branch on the result
        let r = self.v(Class::G);
        self.emit(Inst::SetCond(r, c));
        let tl = self.edge(tb);
        let fl = self.edge(fb);
        let c2 = Cond::Test(r, Opnd::V(r), if on_true { Cc::NE } else { Cc::E });
        self.term(Term::Branch(c2, tl, fl));
        self.fuse_branch();
    }

    /// Value of a LoadGlobal folded into a constant: the current value of the name in
    /// globals (or builtins, if globals lacks it). Records the (dict, key)
    /// dependencies that keep the folding valid (see opt::dict_written).
    fn folded_global(&mut self, i: usize) -> Option<usize> {
        if !self.u.fold_globals {
            return None;
        }
        let ins = self.u.an.instrs[i];
        let nm = &self.u.cd.names[ins.arg as usize];
        if !crate::types::str::is_str_exact(nm) {
            return None;
        }
        // a name this code rebinds itself would invalidate the code at every store
        if self.u.an.instrs.iter().any(|x| matches!(x.op, Op::StoreGlobal | Op::DeleteGlobal) && x.arg == ins.arg) {
            return None;
        }
        let (gp, bp) = (self.u.globals, self.u.builtins);
        let (g, b) = unsafe { (&*gp, &*bp) };
        if !crate::types::dict::is_dict_exact(g) || !crate::types::dict::is_dict_exact(b) {
            return None;
        }
        let key = nm.as_ptr() as usize;
        // rebound after an earlier fold: read it through the cache instead
        if super::unstable_key(gp as usize, nm.as_ptr()) || super::unstable_key(bp as usize, nm.as_ptr()) {
            return None;
        }
        let (v, from_builtins) = match crate::types::dict::get_item_str(g, nm) {
            Some(v) => (v, false),
            None => (crate::types::dict::get_item_str(b, nm)?, true),
        };
        if crate::types::lazyimport::is_lazy(&v) {
            return None; // must be reified by the generic path
        }
        let vp = v.as_ptr() as usize;
        if from_builtins {
            self.deps.push((gp as usize, key, 0));
            self.deps.push((bp as usize, key, vp));
        } else {
            self.deps.push((gp as usize, key, vp));
        }
        // borrowed from the dict: a write of the key invalidates the code first
        self.has_deps = true;
        Some(v.as_ptr() as usize)
    }

    fn binary_op(&mut self, i: usize) -> Result<(), String> {
        use BinOp::*;
        let ins = self.u.an.instrs[i];
        let op: BinOp = unsafe { std::mem::transmute(ins.arg as u8) };
        let (pa, pb) = self.prof(i);
        let int_ops = matches!(
            op,
            Add | InplaceAdd
                | Subtract
                | InplaceSubtract
                | Multiply
                | InplaceMultiply
                | And
                | InplaceAnd
                | Or
                | InplaceOr
                | Xor
                | InplaceXor
                | FloorDivide
                | InplaceFloorDivide
                | Remainder
                | InplaceRemainder
                | Lshift
                | InplaceLshift
                | Rshift
                | InplaceRshift
                | TrueDivide
                | InplaceTrueDivide
        );
        let float_ops = matches!(
            op,
            Add | InplaceAdd | Subtract | InplaceSubtract | Multiply | InplaceMultiply | TrueDivide | InplaceTrueDivide | Power | InplacePower
        );
        // int x int
        if int_ops && self.intish(2, pa) && self.intish(1, pb) {
            let snap = self.snap(i, SnapKind::Deopt);
            let a = self.as_i64(2, pa, snap).unwrap();
            let b = self.as_i64(1, pb, snap).unwrap();
            let r = self.v(Class::G);
            let res = match op {
                Add | InplaceAdd => {
                    self.emit(Inst::AluOv(Alu::Add, r, a, Opnd::V(b), snap));
                    AV::I64(r)
                }
                Subtract | InplaceSubtract => {
                    self.emit(Inst::AluOv(Alu::Sub, r, a, Opnd::V(b), snap));
                    AV::I64(r)
                }
                Multiply | InplaceMultiply => {
                    self.emit(Inst::AluOv(Alu::Mul, r, a, Opnd::V(b), snap));
                    AV::I64(r)
                }
                And | InplaceAnd => {
                    self.emit(Inst::Alu(Alu::And, r, a, Opnd::V(b)));
                    AV::I64(r)
                }
                Or | InplaceOr => {
                    self.emit(Inst::Alu(Alu::Or, r, a, Opnd::V(b)));
                    AV::I64(r)
                }
                Xor | InplaceXor => {
                    self.emit(Inst::Alu(Alu::Xor, r, a, Opnd::V(b)));
                    AV::I64(r)
                }
                FloorDivide | InplaceFloorDivide | Remainder | InplaceRemainder => {
                    self.emit(Inst::Guard(Cond::Cmp(b, Opnd::I(0), Cc::E), snap));
                    self.emit(Inst::Guard(Cond::Cmp(b, Opnd::I(-1), Cc::E), snap));
                    self.emit(Inst::IDivMod(r, a, b, matches!(op, FloorDivide | InplaceFloorDivide)));
                    AV::I64(r)
                }
                Lshift | InplaceLshift => {
                    self.emit(Inst::Guard(Cond::Cmp(b, Opnd::I(0), Cc::L), snap));
                    self.emit(Inst::Guard(Cond::Cmp(b, Opnd::I(62), Cc::G), snap));
                    self.emit(Inst::Alu(Alu::Shl, r, a, Opnd::V(b)));
                    let back = self.v(Class::G);
                    self.emit(Inst::Alu(Alu::Sar, back, r, Opnd::V(b)));
                    self.emit(Inst::Guard(Cond::Cmp(back, Opnd::V(a), Cc::NE), snap));
                    AV::I64(r)
                }
                Rshift | InplaceRshift => {
                    self.emit(Inst::Guard(Cond::Cmp(b, Opnd::I(0), Cc::L), snap));
                    self.emit(Inst::Guard(Cond::Cmp(b, Opnd::I(63), Cc::G), snap));
                    self.emit(Inst::Alu(Alu::Sar, r, a, Opnd::V(b)));
                    AV::I64(r)
                }
                _ => {
                    // true division of ints: exact when both fit in 53 bits
                    let lim = 1i64 << 53;
                    for x in [a, b] {
                        self.emit(Inst::Guard(Cond::Cmp(x, Opnd::I(lim), Cc::G), snap));
                        self.emit(Inst::Guard(Cond::Cmp(x, Opnd::I(-lim), Cc::L), snap));
                    }
                    self.emit(Inst::Guard(Cond::Cmp(b, Opnd::I(0), Cc::E), snap));
                    let fa = self.v(Class::F);
                    let fb = self.v(Class::F);
                    self.emit(Inst::I2F(fa, a));
                    self.emit(Inst::I2F(fb, b));
                    let fr = self.v(Class::F);
                    self.emit(Inst::FAlu(FAlu::Div, fr, fa, fb));
                    AV::F64(fr)
                }
            };
            self.pop2_release();
            self.push(res);
            return Ok(());
        }
        // float (or mixed int/float)
        let any_float = self.floatish(2, pa) || self.floatish(1, pb);
        if float_ops && any_float && self.numish(2, pa) && self.numish(1, pb) {
            let snap = self.snap(i, SnapKind::Deopt);
            let a = self.as_f64(2, pa, snap, true);
            let b = self.as_f64(1, pb, snap, true);
            let (a, b) = match (a, b) {
                (Some(a), Some(b)) => (a, b),
                _ => {
                    self.template(i);
                    return Ok(());
                }
            };
            let r = self.v(Class::F);
            match op {
                Add | InplaceAdd => self.emit(Inst::FAlu(FAlu::Add, r, a, b)),
                Subtract | InplaceSubtract => self.emit(Inst::FAlu(FAlu::Sub, r, a, b)),
                Multiply | InplaceMultiply => self.emit(Inst::FAlu(FAlu::Mul, r, a, b)),
                TrueDivide | InplaceTrueDivide => {
                    let z = self.v(Class::F);
                    self.emit(Inst::FImm(z, 0.0));
                    // b == 0 (or NaN, which the generic path handles too) -> deopt
                    self.emit(Inst::Guard(Cond::FCmp(b, z, Cc::E), snap));
                    self.emit(Inst::FAlu(FAlu::Div, r, a, b));
                }
                _ => {
                    // power: real result only for a > 0; overflow raises
                    let z = self.v(Class::F);
                    self.emit(Inst::FImm(z, 0.0));
                    let pos = self.v(Class::G);
                    self.emit(Inst::SetCond(pos, Cond::FCmp(a, z, Cc::G)));
                    self.emit(Inst::Guard(Cond::Test(pos, Opnd::V(pos), Cc::E), snap));
                    self.emit(Inst::Call { addr: h_fpow as usize, args: vec![], fargs: vec![a, b], ret: None, fret: Some(r) });
                    let inf = self.v(Class::F);
                    self.emit(Inst::FImm(inf, f64::INFINITY));
                    self.emit(Inst::Guard(Cond::FCmp(r, inf, Cc::E), snap));
                }
            }
            self.pop2_release();
            self.push(AV::F64(r));
            return Ok(());
        }
        self.template(i);
        Ok(())
    }

    /// CompareOp; fused with a following PopJumpIf when possible.
    fn compare_op(&mut self, i: usize, last: usize) -> Result<bool, String> {
        let ins = self.u.an.instrs[i];
        let cop = (ins.arg & 15) as i32;
        let (pa, pb) = self.prof(i);
        let cc = match cop {
            Py_LT => Cc::L,
            Py_LE => Cc::LE,
            Py_EQ => Cc::E,
            Py_NE => Cc::NE,
            Py_GT => Cc::G,
            _ => Cc::GE,
        };
        let cond = if self.intish(2, pa) && self.intish(1, pb) {
            let snap = self.snap(i, SnapKind::Deopt);
            let a = self.as_i64(2, pa, snap).unwrap();
            let b = self.as_i64(1, pb, snap).unwrap();
            Some(Cond::Cmp(a, Opnd::V(b), cc))
        } else if (self.floatish(2, pa) || self.floatish(1, pb)) && self.numish(2, pa) && self.numish(1, pb) {
            let snap = self.snap(i, SnapKind::Deopt);
            let a = self.as_f64(2, pa, snap, true);
            let b = self.as_f64(1, pb, snap, true);
            match (a, b) {
                (Some(a), Some(b)) => Some(Cond::FCmp(a, b, cc)),
                _ => None,
            }
        } else {
            None
        };
        let c = match cond {
            Some(c) => c,
            None => {
                self.template(i);
                return Ok(false);
            }
        };
        // compute the flag now (operands are released below)
        let r = self.v(Class::G);
        self.emit(Inst::SetCond(r, c));
        self.pop2_release();
        let next = self.u.an.instrs.get(i + 1).copied();
        if i < last {
            if let Some(n) = next {
                if matches!(n.op, Op::PopJumpIfFalse | Op::PopJumpIfTrue) {
                    let tb = self.target_block(i + 1);
                    let fb = self.next_block(i + 1);
                    let tl = self.edge(tb);
                    let fl = self.edge(fb);
                    let c2 = Cond::Test(r, Opnd::V(r), if n.op == Op::PopJumpIfTrue { Cc::NE } else { Cc::E });
                    self.term(Term::Branch(c2, tl, fl));
                    self.fuse_branch();
                    return Ok(true);
                }
            }
        }
        self.push(AV::Bool(r));
        Ok(false)
    }

    fn for_iter(&mut self, i: usize) -> Result<(), String> {
        let (pa, _) = self.prof(i);
        let tb = self.target_block(i);
        let fb = self.next_block(i);
        let range_t = (&raw mut crate::types::range::PyRangeIter_Type) as usize;
        let list_it = (&raw mut crate::types::list::PyListIter_Type) as usize;
        let tuple_it = (&raw mut crate::types::tuple::PyTupleIter_Type) as usize;
        let kind = if !crate::interp::jit::tier1::iter_layout_ok() {
            0
        } else if pa == prof::IT_RANGE {
            1
        } else if pa == prof::IT_LIST {
            2
        } else if pa == prof::IT_TUPLE {
            3
        } else {
            0
        };
        if kind == 0 {
            // generic: the template jumps to the exhausted target itself
            self.template(i);
            let t = self.edge(fb);
            self.term(Term::Jump(t));
            return Ok(());
        }
        let tp = [0, range_t, list_it, tuple_it][kind];
        // the iterator stays in its frame slot (owned there)
        self.materialize_stack();
        let snap = self.snap(i, SnapKind::Deopt);
        let d = self.st.stack.len();
        let it = self.v(Class::G);
        let off = self.slot_off(d - 1);
        self.emit(Inst::Load(it, Base::Slots, off));
        let known = self.ty_of(&self.peek(1)) == Ty::Exact(tp);
        if !known {
            let t = self.imm(tp as i64);
            let ty = self.v(Class::G);
            self.emit(Inst::Load(ty, Base::V(it), OB_TYPE));
            self.emit(Inst::Guard(Cond::Cmp(ty, Opnd::V(t), Cc::NE), snap));
            self.learn(1, Ty::Exact(tp));
        }
        let cont = self.new_block(u32::MAX);
        let done = self.new_block(u32::MAX);
        let item: AV;
        match kind {
            1 => {
                let len = self.v(Class::G);
                self.emit(Inst::Load(len, Base::V(it), 48));
                self.term(Term::Branch(Cond::Cmp(len, Opnd::I(0), Cc::LE), done, cont));
                self.cur = cont;
                self.closed = false;
                let len2 = self.v(Class::G);
                self.emit(Inst::Alu(Alu::Sub, len2, len, Opnd::I(1)));
                self.emit(Inst::Store(Base::V(it), 48, len2));
                let cur = self.v(Class::G);
                self.emit(Inst::Load(cur, Base::V(it), 32));
                let step = self.v(Class::G);
                self.emit(Inst::Load(step, Base::V(it), 40));
                let nx = self.v(Class::G);
                self.emit(Inst::Alu(Alu::Add, nx, cur, Opnd::V(step)));
                self.emit(Inst::Store(Base::V(it), 32, nx));
                item = AV::I64(cur);
            }
            2 => {
                let seq = self.v(Class::G);
                self.emit(Inst::Load(seq, Base::V(it), 40));
                let idx = self.v(Class::G);
                self.emit(Inst::Load(idx, Base::V(it), 32));
                // seq == NULL: already exhausted -> generic path (deopt)
                self.emit(Inst::Guard(Cond::Test(seq, Opnd::V(seq), Cc::E), snap));
                let size = self.v(Class::G);
                self.emit(Inst::Load(size, Base::V(seq), LIST_SIZE));
                self.term(Term::Branch(Cond::Cmp(idx, Opnd::V(size), Cc::GE), done, cont));
                self.cur = cont;
                self.closed = false;
                let items = self.v(Class::G);
                self.emit(Inst::Load(items, Base::V(seq), LIST_ITEMS));
                let x = self.v(Class::G);
                self.emit(Inst::LoadIdx(x, items, idx, 0));
                self.emit(Inst::Incref(x));
                let i2 = self.v(Class::G);
                self.emit(Inst::Alu(Alu::Add, i2, idx, Opnd::I(1)));
                self.emit(Inst::Store(Base::V(it), 32, i2));
                item = AV::Obj(x, Own::Owned, Ty::Unknown);
                // exhausted: drop the list reference like the iterator does
                let save = self.cur;
                self.cur = done;
                self.emit(Inst::StoreImm(Base::V(it), 40, 0));
                self.emit(Inst::Decref(seq));
                self.cur = save;
            }
            _ => {
                let seq = self.v(Class::G);
                self.emit(Inst::Load(seq, Base::V(it), 40));
                self.emit(Inst::Guard(Cond::Test(seq, Opnd::V(seq), Cc::E), snap));
                let idx = self.v(Class::G);
                self.emit(Inst::Load(idx, Base::V(it), 32));
                let size = self.v(Class::G);
                self.emit(Inst::Load(size, Base::V(seq), LIST_SIZE));
                self.term(Term::Branch(Cond::Cmp(idx, Opnd::V(size), Cc::GE), done, cont));
                self.cur = cont;
                self.closed = false;
                let x = self.v(Class::G);
                self.emit(Inst::LoadIdx(x, seq, idx, TUPLE_ITEMS));
                self.emit(Inst::Incref(x));
                let i2 = self.v(Class::G);
                self.emit(Inst::Alu(Alu::Add, i2, idx, Opnd::I(1)));
                self.emit(Inst::Store(Base::V(it), 32, i2));
                item = AV::Obj(x, Own::Owned, Ty::Unknown);
                let save = self.cur;
                self.cur = done;
                self.emit(Inst::StoreImm(Base::V(it), 40, 0));
                self.emit(Inst::Decref(seq));
                self.cur = save;
            }
        }
        // continue: push the item, fall into the body
        let saved = self.st.clone();
        self.push(item);
        let t = self.edge(fb);
        self.term(Term::Jump(t));
        // exhausted: pop and release the iterator, jump to the target
        self.st = saved;
        self.cur = done;
        self.closed = false;
        self.emit(Inst::Decref(it));
        self.st.stack.pop();
        let t = self.edge(tb);
        self.term(Term::Jump(t));
        Ok(())
    }

    // ------------------------------------------------------------ attributes

    /// Filled inline-cache entries of attribute instruction i (None: a writer is
    /// active or the site is megamorphic).
    fn attr_entries(&self, i: usize) -> Option<Vec<crate::interp::cache::Snapshot>> {
        use crate::interp::cache::kind;
        let pc = self.u.an.instrs[i].pc as usize;
        let mut out = Vec::new();
        for e in self.u.tables.poly(pc) {
            let s = e.read()?;
            match s.kind {
                kind::EMPTY => {}
                kind::UNCACHEABLE => return None,
                // (`a` is a dict version there, not a type tag)
                kind::MODULE | kind::GLOBAL => out.push(s),
                // entries of classes that died since: their pointers dangle
                _ if !crate::interp::cache::tag_alive(s.a) => {}
                _ => out.push(s),
            }
        }
        Some(out)
    }

    fn entries_addr(&self, i: usize) -> i64 {
        let pc = self.u.an.instrs[i].pc as usize;
        self.u.tables.poly(pc).as_ptr() as i64
    }

    /// Leave for `f` when `c` holds: deoptimise, or branch to a slow-path block.
    fn check(&mut self, c: Cond, f: Fail) {
        match f {
            Fail::Deopt(s) => self.emit(Inst::Guard(c, s)),
            Fail::Block(b) => {
                let next = self.new_block(u32::MAX);
                self.term(Term::Branch(c, b, next));
                self.cur = next;
                self.closed = false;
            }
        }
    }

    /// Continue emitting into block b.
    fn switch_to(&mut self, b: BlockId) {
        self.cur = b;
        self.closed = false;
    }

    /// Receiver at depth n, guarded (deoptimising) to have an exact type with version
    /// tag `tag`.
    fn guard_tag(&mut self, n: usize, tag: u32, snap: SnapId) -> V {
        let p = self.peek_ptr(n);
        if self.ty_of(&self.peek(n)) != Ty::Tag(tag) {
            let ty = self.v(Class::G);
            self.emit(Inst::Load(ty, Base::V(p), OB_TYPE));
            self.emit(Inst::Guard(Cond::CmpMem32(Base::V(ty), TP_TAG, tag as i64, Cc::NE), snap));
            self.learn(n, Ty::Tag(tag));
        }
        p
    }

    /// Type-tag dispatch over the cases of a polymorphic site: returns, per case, the
    /// block where the receiver is known to have that case's tag; the current block
    /// then continues as the "no case matched" path.
    fn tag_switch(&mut self, p: V, tags: &[u32]) -> Vec<BlockId> {
        let ty = self.v(Class::G);
        self.emit(Inst::Load(ty, Base::V(p), OB_TYPE));
        let tg = self.v(Class::G);
        self.emit(Inst::Load32(tg, Base::V(ty), TP_TAG));
        let mut out = Vec::new();
        for &t in tags {
            let body = self.new_block(u32::MAX);
            let next = self.new_block(u32::MAX);
            self.term(Term::Branch(Cond::Cmp32(tg, Opnd::I(t as i64), Cc::E), body, next));
            out.push(body);
            self.switch_to(next);
        }
        out
    }

    /// Values word of `p` (tagged pointer to its inline values), leaving for `f` if the
    /// object keeps its attributes in a dict (or has no managed dict slot).
    fn values_word(&mut self, p: V, f: Fail) -> V {
        let gb = self.v(Class::G);
        self.emit(Inst::Load8(gb, Base::V(p), GC_BITS));
        self.check(Cond::Test(gb, Opnd::I(crate::alloc::GC_BIT_MANAGED as i64), Cc::E), f);
        let w = self.v(Class::G);
        self.emit(Inst::Load(w, Base::V(p), MANAGED_DICT));
        self.check(Cond::Test(w, Opnd::I(1), Cc::E), f);
        w
    }

    /// Current shape of values word `w` (perf2 layout: the word is `shape | 1`).
    fn shape_of(&mut self, w: V) -> V {
        let sh = self.v(Class::G);
        self.emit(Inst::Lea(sh, Base::V(w), -1));
        sh
    }

    /// Leave for `f` unless the shape of values word `w` has `name` at index h.
    fn check_shape_key(&mut self, w: V, h: u64, name: usize, f: Fail) {
        let (_id, len_off, keys_off, _voff) = crate::shape::jit_layout();
        let sh = self.shape_of(w);
        self.check(Cond::Test(sh, Opnd::V(sh), Cc::E), f);
        self.check(Cond::CmpMem32(Base::V(sh), len_off, h as i64, Cc::BE), f);
        let keys = self.v(Class::G);
        self.emit(Inst::Load(keys, Base::V(sh), keys_off));
        self.check(Cond::CmpMem(Base::V(keys), 8 * h as i32, name as i64, Cc::NE), f);
    }

    /// Address of value slot 0 of `p`, whose values word is `w`: `p + shape.voff`.
    fn values_base(&mut self, p: V, w: V) -> V {
        let (_id, _len, _keys, voff_off) = crate::shape::jit_layout();
        let vo = self.v(Class::G);
        self.emit(Inst::Load32(vo, Base::V(w), voff_off - 1));
        let vb = self.v(Class::G);
        self.emit(Inst::Alu(Alu::Add, vb, p, Opnd::V(vo)));
        vb
    }

    fn load_attr(&mut self, i: usize) {
        use crate::interp::cache::kind;
        let ins = self.u.an.instrs[i];
        let name = self.u.cd.names[ins.arg as usize].as_ptr() as usize;
        // module attribute of a folded module object: fold the value too
        if let AV::Const(m) = self.peek(1) {
            if let Some(v) = self.folded_module_attr(m, i) {
                self.pop();
                self.push(AV::Const(v));
                return;
            }
        }
        let Some(es) = self.attr_entries(i) else {
            self.template(i);
            return;
        };
        if es.len() == 1 && es[0].kind == kind::TYPE_ATTR {
            // attribute of a class object (b): guard identity + version tag
            let s = es[0];
            let snap = self.snap(i, SnapKind::Deopt);
            let t = if let AV::Const(p) = self.peek(1) {
                if p as u64 != s.b {
                    self.template(i);
                    return;
                }
                self.imm(p as i64)
            } else {
                let p = self.peek_ptr(1);
                self.emit(Inst::Guard(Cond::Cmp(p, Opnd::I(s.b as i64), Cc::NE), snap));
                p
            };
            self.emit(Inst::Guard(Cond::CmpMem32(Base::V(t), TP_TAG, s.a as u32 as i64, Cc::NE), snap));
            let k = self.st.stack.len() - 1;
            let a = self.pop();
            self.discard(a, k);
            self.push(AV::Const(s.c));
            return;
        }
        let ok = |s: &crate::interp::cache::Snapshot| match s.kind {
            kind::INSTANCE_DICT => s.b < (1 << 20),
            kind::SLOT => true,
            kind::CLASS_ATTR => unsafe { (*(s.c as *mut PyObject)).ty().tp_descr_get.is_none() },
            _ => false,
        };
        let cases: Vec<_> = es.iter().copied().filter(|s| ok(s)).collect();
        if cases.is_empty() {
            self.template(i);
            return;
        }
        let snap = self.snap(i, SnapKind::Deopt);
        if es.len() == 1 && cases[0].kind != kind::INSTANCE_DICT && !self.u.cautious_at(self.u.an.instrs[i]) {
            // monomorphic slot / class attribute: no slow path needed
            let s = cases[0];
            let p = self.guard_tag(1, s.a as u32, snap);
            let k = self.st.stack.len() - 1;
            if s.kind == kind::SLOT {
                let x = self.v(Class::G);
                self.emit(Inst::NoteRead(p)); // scale2
                self.emit(Inst::Load(x, Base::V(p), s.c as i32));
                self.emit(Inst::Guard(Cond::Test(x, Opnd::V(x), Cc::E), snap));
                self.emit(Inst::Incref(x));
                let a = self.pop();
                self.discard(a, k);
                self.push(AV::Obj(x, Own::Owned, Ty::Unknown));
            } else {
                let a = self.pop();
                self.discard(a, k);
                // borrowed from the class dict, which the tag guard keeps unchanged
                self.push(AV::Const(s.c));
            }
            return;
        }
        let esnap = self.snap(i, SnapKind::Error);
        let res = self.v(Class::G);
        let cont = self.new_block(u32::MAX);
        let slow = self.new_block(u32::MAX);
        let p;
        let bodies = if es.len() == 1 && !self.u.cautious_at(self.u.an.instrs[i]) {
            p = self.guard_tag(1, cases[0].a as u32, snap);
            vec![self.cur]
        } else {
            p = self.peek_ptr(1);
            let tags: Vec<u32> = cases.iter().map(|s| s.a as u32).collect();
            let b = self.tag_switch(p, &tags);
            self.term(Term::Jump(slow));
            b
        };
        for (s, b) in cases.iter().zip(bodies) {
            self.switch_to(b);
            let f = Fail::Block(slow);
            let x = match s.kind {
                kind::INSTANCE_DICT => {
                    let w = self.values_word(p, f);
                    self.check_shape_key(w, s.b, name, f);
                    let vb = self.values_base(p, w);
                    let x = self.v(Class::G);
                    self.emit(Inst::NoteRead(p)); // scale2
                    self.emit(Inst::Load(x, Base::V(vb), 8 * s.b as i32));
                    self.check(Cond::Test(x, Opnd::V(x), Cc::E), f);
                    x
                }
                kind::SLOT => {
                    let x = self.v(Class::G);
                    self.emit(Inst::NoteRead(p)); // scale2
                    self.emit(Inst::Load(x, Base::V(p), s.c as i32));
                    self.check(Cond::Test(x, Opnd::V(x), Cc::E), f);
                    x
                }
                _ => self.imm(s.c as i64),
            };
            self.emit(Inst::Incref(x));
            self.emit(Inst::Mov(res, x));
            self.term(Term::Jump(cont));
        }
        // generic lookup (refills the inline cache)
        self.switch_to(slow);
        let ea = self.entries_addr(i);
        let r = self.v(Class::G);
        if self.inl.is_none() {
            // (cache misses refresh the adaptive view of the running instruction)
            self.emit(Inst::SetLasti(self.u.an.instrs[i].pc));
        }
        let e0 = if self.inl.is_none() { Some(self.epoch_load()) } else { None };
        self.emit(Inst::Call { addr: h2_load_attr as usize, args: vec![Opnd::V(p), Opnd::I(name as i64), Opnd::I(ea)], fargs: vec![], ret: Some(r), fret: None });
        self.emit(Inst::Guard(Cond::Test(r, Opnd::V(r), Cc::E), esnap));
        if let Some(e0) = e0 {
            self.epoch_check(i, e0, false, Some(r));
        }
        self.emit(Inst::Mov(res, r));
        self.term(Term::Jump(cont));
        self.switch_to(cont);
        if self.inl.is_some() {
            self.forget_tags(); // (no resumable snapshot inside an inlined callee)
        }
        let k = self.st.stack.len() - 1;
        let a = self.pop();
        self.discard(a, k);
        self.push(AV::Obj(res, Own::Owned, Ty::Unknown));
    }

    /// Current value of the type epoch (cache::TYPE_EPOCH).
    fn epoch_load(&mut self) -> V {
        let a = self.imm(&crate::interp::cache::TYPE_EPOCH as *const _ as i64);
        let v = self.v(Class::G);
        self.emit(Inst::Load(v, Base::V(a), 0));
        v
    }

    /// After a generic attribute call (which may run arbitrary code: __getattr__,
    /// properties, descriptors): if a type was modified or an object's class reassigned
    /// meanwhile (epoch differs from `e0`), the type facts of this unit may be stale, so
    /// deoptimise, resuming after instruction i with the operands popped (the receiver
    /// released; for a store the value below it was consumed) and `result` pushed.
    fn epoch_check(&mut self, i: usize, e0: V, store: bool, result: Option<V>) {
        let e1 = self.epoch_load();
        let chg = self.new_block(u32::MAX);
        let ok = self.new_block(u32::MAX);
        self.term(Term::Branch(Cond::Cmp(e1, Opnd::V(e0), Cc::NE), chg, ok));
        self.switch_to(chg);
        let saved = self.st.clone();
        let k = self.st.stack.len() - 1;
        let a = self.pop();
        self.discard(a, k);
        if store {
            self.pop();
        }
        if let Some(r) = result {
            self.push(AV::Obj(r, Own::Owned, Ty::Unknown));
        }
        let s = self.snap_next(i);
        self.term(Term::Deopt(s));
        self.st = saved;
        self.switch_to(ok);
    }

    /// Value of attribute `name` of a module object folded into the code, with a
    /// dependency on the module dict entry.
    fn folded_module_attr(&mut self, m: usize, i: usize) -> Option<usize> {
        if !self.u.fold_globals {
            return None;
        }
        unsafe {
            let mo = &*(m as *mut PyObject);
            if !mo.is_type(crate::types::module::module_type()) {
                return None;
            }
            // a module subclass or a module type attribute could intercept the lookup
            let nm = &self.u.cd.names[self.u.an.instrs[i].arg as usize];
            if !crate::types::str::is_str_exact(nm) || crate::types::typeobject::lookup(mo.type_ptr(), nm).is_some() {
                return None;
            }
            let d = crate::types::module::m(mo).md_dict;
            if d.is_null() || !crate::types::dict::is_dict_exact(&*d) || super::unstable_key(d as usize, nm.as_ptr()) {
                return None;
            }
            let v = crate::types::dict::get_item_str(&*d, nm)?;
            if crate::types::lazyimport::is_lazy(&v) {
                return None;
            }
            let vp = v.as_ptr() as usize;
            self.deps.push((d as usize, nm.as_ptr() as usize, vp));
            self.has_deps = true;
            Some(vp)
        }
    }

    /// Instances may shadow method `name` (inline values): leave for `f` unless the
    /// shape lacks it. Per-site memo of the last shape seen lacking the name.
    fn check_not_shadowed(&mut self, p: V, name: usize, f: Fail, snap: SnapId) {
        let w = self.v(Class::G);
        self.emit(Inst::Load(w, Base::V(p), MANAGED_DICT));
        let cont = self.new_block(u32::MAX);
        let check = self.new_block(u32::MAX);
        // no dict and no values: nothing can shadow
        self.term(Term::Branch(Cond::Test(w, Opnd::V(w), Cc::E), cont, check));
        self.switch_to(check);
        // a real instance dict (objects that outgrew their inline values): look the
        // name up instead of deoptimising
        let _ = f;
        let in_dict = self.new_block(u32::MAX);
        let values = self.new_block(u32::MAX);
        self.term(Term::Branch(Cond::Test(w, Opnd::I(1), Cc::E), in_dict, values));
        self.switch_to(in_dict);
        let rd = self.v(Class::G);
        self.emit(Inst::Call { addr: h2_dict_lacks as usize, args: vec![Opnd::V(w), Opnd::I(name as i64)], fargs: vec![], ret: Some(rd), fret: None });
        self.emit(Inst::Guard(Cond::Test(rd, Opnd::V(rd), Cc::E), snap));
        self.term(Term::Jump(cont));
        self.switch_to(values);
        let sh = self.shape_of(w);
        let cell = Box::leak(Box::new(std::sync::atomic::AtomicUsize::new(0))) as *const _ as usize;
        self.cells.push(cell);
        let cv = self.imm(cell as i64);
        let memo = self.v(Class::G);
        self.emit(Inst::Load(memo, Base::V(cv), 0));
        let slow = self.new_block(u32::MAX);
        self.term(Term::Branch(Cond::Cmp(sh, Opnd::V(memo), Cc::E), cont, slow));
        self.switch_to(slow);
        let r = self.v(Class::G);
        self.emit(Inst::Call { addr: h_shape_lacks as usize, args: vec![Opnd::V(sh), Opnd::I(name as i64), Opnd::I(cell as i64)], fargs: vec![], ret: Some(r), fret: None });
        self.emit(Inst::Guard(Cond::Test(r, Opnd::V(r), Cc::E), snap));
        self.term(Term::Jump(cont));
        self.switch_to(cont);
    }

    fn load_method(&mut self, i: usize) {
        use crate::interp::cache::kind;
        let ins = self.u.an.instrs[i];
        let name = self.u.cd.names[ins.arg as usize].as_ptr() as usize;
        let Some(es) = self.attr_entries(i) else {
            self.template(i);
            return;
        };
        let ok = |s: &crate::interp::cache::Snapshot| match s.kind {
            kind::METHOD => true,
            // instance dict at tp_dictoffset / dict-only managed dicts: generic path
            kind::METHOD_SHADOWABLE => s.b != 0 && live_type(s.b as usize, s.a) && unsafe { (*(s.b as *mut PyTypeObject)).tp_flags } & crate::ffi::Py_TPFLAGS_INLINE_VALUES != 0,
            _ => false,
        };
        if es.is_empty() || !es.iter().all(|s| ok(s)) {
            self.template(i);
            return;
        }
        let snap = self.snap(i, SnapKind::Deopt);
        let meth = if es.len() == 1 {
            let s = es[0];
            let p = self.guard_tag(1, s.a as u32, snap);
            if s.kind == kind::METHOD_SHADOWABLE {
                self.check_not_shadowed(p, name, Fail::Deopt(snap), snap);
            }
            AV::Const(s.c)
        } else {
            // polymorphic: the method per receiver type (borrowed from the type dicts)
            let p = self.peek_ptr(1);
            let res = self.v(Class::G);
            let cont = self.new_block(u32::MAX);
            let tags: Vec<u32> = es.iter().map(|s| s.a as u32).collect();
            let bodies = self.tag_switch(p, &tags);
            self.term(Term::Deopt(snap));
            for (s, b) in es.iter().zip(bodies) {
                self.switch_to(b);
                if s.kind == kind::METHOD_SHADOWABLE {
                    self.check_not_shadowed(p, name, Fail::Deopt(snap), snap);
                }
                let m = self.imm(s.c as i64);
                self.emit(Inst::Mov(res, m));
                self.term(Term::Jump(cont));
            }
            self.switch_to(cont);
            AV::Obj(res, Own::Const, Ty::Unknown)
        };
        // [recv] -> [method, recv]: the receiver keeps its abstract value
        let k = self.st.stack.len() - 1;
        let recv = self.st.stack[k];
        self.st.stack[k] = meth;
        self.push(recv);
    }

    fn store_attr(&mut self, i: usize) {
        use crate::interp::cache::kind;
        let ins = self.u.an.instrs[i];
        let name = self.u.cd.names[ins.arg as usize].as_ptr() as usize;
        let Some(es) = self.attr_entries(i) else {
            self.template(i);
            return;
        };
        let ok = |s: &crate::interp::cache::Snapshot| match s.kind {
            kind::INSTANCE_DICT => s.b < (1 << 20),
            kind::SLOT => true,
            _ => false,
        };
        let cases: Vec<_> = es.iter().copied().filter(|s| ok(s)).collect();
        if cases.is_empty() {
            self.template(i);
            return;
        }
        // [value, owner]
        let snap = self.snap(i, SnapKind::Deopt);
        let p = if es.len() == 1 { self.guard_tag(1, cases[0].a as u32, snap) } else { self.peek_ptr(1) };
        // the value as an owned reference (moved into the object on the fast paths)
        let val = match self.peek(2) {
            AV::InFrame(_) => self.peek_ptr(2),
            other => {
                let t = self.ty_of(&other);
                let v = self.obj_owned(other);
                let k = self.st.stack.len() - 2;
                self.st.stack[k] = AV::Obj(v, Own::Owned, t);
                v
            }
        };
        let esnap = self.snap(i, SnapKind::Error);
        let cont = self.new_block(u32::MAX);
        let slow = self.new_block(u32::MAX);
        let bodies = if es.len() == 1 {
            vec![self.cur]
        } else {
            let tags: Vec<u32> = cases.iter().map(|s| s.a as u32).collect();
            let b = self.tag_switch(p, &tags);
            self.term(Term::Jump(slow));
            b
        };
        let mt_addr = (&raw const crate::threads::MULTI_THREADED) as i64;
        for (s, b) in cases.iter().zip(bodies) {
            self.switch_to(b);
            let f = Fail::Block(slow);
            match s.kind {
                kind::INSTANCE_DICT => {
                    // other threads may access the values lock-free: locked runtime path
                    let mta = self.imm(mt_addr);
                    self.check(Cond::CmpMem(Base::V(mta), 0, 0, Cc::NE), f);
                    let w = self.values_word(p, f);
                    let off = 8 * s.b as i32;
                    let trans = if s.c != 0 && live_type(s.c, s.a) { crate::shape::transitions_adding(s.c as *mut PyTypeObject, name as *mut PyObject, s.b as usize) } else { Vec::new() };
                    if trans.len() == 1 {
                        // replace (shape = child) or add (shape = parent, room left)
                        let (parent, child) = trans[0];
                        let sh = self.shape_of(w);
                        let add = self.new_block(u32::MAX);
                        let replace = self.new_block(u32::MAX);
                        let notc = self.new_block(u32::MAX);
                        self.term(Term::Branch(Cond::Cmp(sh, Opnd::I(child as i64), Cc::E), replace, notc));
                        self.switch_to(notc);
                        self.check(Cond::Cmp(sh, Opnd::I(parent as i64), Cc::NE), f);
                        // room left: the capacity byte (shape.rs) must exceed the index
                        let cap = self.v(Class::G);
                        self.emit(Inst::Load8(cap, Base::V(p), crate::shape::capacity_offset()));
                        self.check(Cond::Cmp(cap, Opnd::I(s.b as i64), Cc::BE), f);
                        self.term(Term::Jump(add));
                        self.switch_to(add);
                        let vb = self.values_base(p, w);
                        self.emit(Inst::Store(Base::V(vb), off, val));
                        let cv = self.imm(child as i64 | 1);
                        self.emit(Inst::Store(Base::V(p), MANAGED_DICT, cv));
                        self.term(Term::Jump(cont));
                        self.switch_to(replace);
                    } else {
                        self.check_shape_key(w, s.b, name, f);
                    }
                    let vb = self.values_base(p, w);
                    let old = self.v(Class::G);
                    self.emit(Inst::Load(old, Base::V(vb), off));
                    self.emit(Inst::Store(Base::V(vb), off, val));
                    self.emit(Inst::XDecref(old));
                }
                _ => {
                    // __slots__ member: lock-free readers on other threads need the old
                    // value released through QSBR, which the runtime path does
                    let mta = self.imm(mt_addr);
                    self.check(Cond::CmpMem(Base::V(mta), 0, 0, Cc::NE), f);
                    let old = self.v(Class::G);
                    self.emit(Inst::Load(old, Base::V(p), s.c as i32));
                    self.emit(Inst::Store(Base::V(p), s.c as i32, val));
                    self.emit(Inst::XDecref(old));
                }
            }
            self.term(Term::Jump(cont));
        }
        // generic store (refills the inline cache); the value reference is released
        self.switch_to(slow);
        let ea = self.entries_addr(i);
        if self.inl.is_none() {
            // (cache misses refresh the adaptive view of the running instruction)
            self.emit(Inst::SetLasti(self.u.an.instrs[i].pc));
        }
        let e0 = if self.inl.is_none() { Some(self.epoch_load()) } else { None };
        self.emit(Inst::CallChecked { addr: h2_store_attr as usize, args: vec![Opnd::V(p), Opnd::I(name as i64), Opnd::V(val), Opnd::I(ea)], err: esnap });
        if let Some(e0) = e0 {
            self.epoch_check(i, e0, true, None);
        }
        self.term(Term::Jump(cont));
        self.switch_to(cont);
        if self.inl.is_some() {
            self.forget_tags();
        }
        // value moved into the object / released by the runtime
        let k = self.st.stack.len() - 1;
        let a = self.pop();
        self.discard(a, k);
        self.pop();
    }

    // ------------------------------------------------------------ builtins

    /// Name of builtin function `p` if it belongs to the builtins module the code runs
    /// with.
    fn builtin_name(&self, p: usize) -> Option<&'static str> {
        unsafe {
            let o = &*(p as *mut PyObject);
            if !o.is_type(crate::types::cfunction::cfunction_type()) {
                return None;
            }
            let cf = p as *mut PyCFunctionObject;
            let (ml, ms) = ((*cf).m_ml, (*cf).m_self);
            if ml.is_null() || ms.is_null() || !(*ms).is_type(crate::types::module::module_type()) {
                return None;
            }
            if crate::types::module::m(&*ms).md_dict != self.u.builtins {
                return None;
            }
            std::ffi::CStr::from_ptr((*ml).ml_name).to_str().ok()
        }
    }

    /// `iter(range(...))` (a Call of the builtin range directly followed by GetIter in
    /// the same block): the range iterator is created by one helper call, without
    /// the argument tuple, the range object and its int fields. True: instruction
    /// i + 1 was consumed too.
    fn try_range_iter(&mut self, i: usize, last: usize) -> bool {
        let n = self.u.an.instrs[i].arg as usize;
        if i + 1 > last || self.u.an.instrs[i + 1].op != Op::GetIter || !(1..=3).contains(&n) || self.inl.is_some() {
            return false;
        }
        let d = self.st.stack.len();
        let AV::Const(fp) = self.st.stack[d - n - 2] else { return false };
        if !matches!(self.st.stack[d - n - 1], AV::Null) || fp != crate::types::range::range_type() as usize {
            return false;
        }
        let esnap = self.snap(i, SnapKind::Error);
        let mut args = Vec::new();
        for k in (1..=n).rev() {
            args.push(Opnd::V(self.peek_ptr(k)));
        }
        while args.len() < 3 {
            args.push(Opnd::I(0));
        }
        args.push(Opnd::I(n as i64));
        let r = self.v(Class::G);
        self.emit(Inst::Call { addr: h2_range_iter as usize, args, fargs: vec![], ret: Some(r), fret: None });
        self.emit(Inst::Guard(Cond::Test(r, Opnd::V(r), Cc::E), esnap));
        for _ in 0..n {
            let k = self.st.stack.len() - 1;
            let a = self.pop();
            self.discard(a, k);
        }
        self.pop(); // NULL
        self.pop(); // range (a constant)
        self.push(AV::Obj(r, Own::Owned, Ty::Unknown));
        true
    }

    /// Calls of a few builtin methods found by LoadMethod (stack: [descriptor, receiver,
    /// args...]): dict.get and list.append go straight to their implementation
    /// instead of through the generic call (descriptor vectorcall, argument checks).
    fn try_method_builtin(&mut self, i: usize) -> bool {
        let n = self.u.an.instrs[i].arg as usize;
        let d = self.st.stack.len();
        if d < n + 2 {
            return false;
        }
        let AV::Const(dp) = self.st.stack[d - n - 2] else { return false };
        if matches!(self.st.stack[d - n - 1], AV::Null) {
            return false;
        }
        let Some((tp, name)) = (unsafe { method_descr_name(dp) }) else { return false };
        let helper = if tp == crate::types::dict::dict_type() as usize && name == "get" && (n == 1 || n == 2) {
            h2_dict_get as usize
        } else if tp == list_t() && name == "append" && n == 1 {
            h2_list_append as usize
        } else {
            return false;
        };
        let esnap = self.snap(i, SnapKind::Error);
        let recv = self.peek_ptr(n + 1);
        let a1 = self.peek_ptr(n);
        let a2 = if n == 2 { Opnd::V(self.peek_ptr(1)) } else { Opnd::I(0) };
        // a key's __hash__/__eq__ may run Python code (exact str/int keys do not)
        let key_ty = self.ty_of(&self.peek(n));
        let safe_key = key_ty.is(crate::types::str::str_type()) || key_ty.is(int_t() as *mut PyTypeObject);
        let r = self.v(Class::G);
        self.emit(Inst::Call { addr: helper, args: vec![Opnd::V(recv), Opnd::V(a1), a2], fargs: vec![], ret: Some(r), fret: None });
        self.emit(Inst::Guard(Cond::Test(r, Opnd::V(r), Cc::E), esnap));
        for _ in 0..n + 1 {
            let k = self.st.stack.len() - 1;
            let a = self.pop();
            self.discard(a, k);
        }
        self.pop(); // the descriptor (a constant)
        if helper == h2_list_append as usize {
            self.push(AV::Const(none_p()));
        } else {
            if !safe_key {
                self.forget_tags();
            }
            self.push(AV::Obj(r, Own::Owned, Ty::Unknown));
        }
        true
    }

    /// Calls of a few builtins compiled inline: len() of lists/tuples/dicts/strs.
    fn try_builtin(&mut self, i: usize) -> bool {
        let n = self.u.an.instrs[i].arg as usize;
        let d = self.st.stack.len();
        let AV::Const(fp) = self.st.stack[d - n - 2] else { return false };
        if !matches!(self.st.stack[d - n - 1], AV::Null) {
            return false;
        }
        match (self.builtin_name(fp), n) {
            (Some("len"), 1) => {
                let esnap = self.snap(i, SnapKind::Error);
                let x = self.peek_ptr(1);
                let res = self.v(Class::G);
                let cont = self.new_block(u32::MAX);
                let ty = self.v(Class::G);
                self.emit(Inst::Load(ty, Base::V(x), OB_TYPE));
                let cases: [(usize, i32); 4] = [
                    (list_t(), LIST_SIZE),
                    (tuple_t(), LIST_SIZE),
                    (crate::types::dict::dict_type() as usize, std::mem::offset_of!(PyDictObject, ma_used) as i32),
                    (crate::types::str::str_type() as usize, std::mem::offset_of!(PyASCIIObject, length) as i32),
                ];
                for (tp, off) in cases {
                    let hit = self.new_block(u32::MAX);
                    let next = self.new_block(u32::MAX);
                    self.term(Term::Branch(Cond::Cmp(ty, Opnd::I(tp as i64), Cc::E), hit, next));
                    self.switch_to(hit);
                    let sz = self.v(Class::G);
                    self.emit(Inst::Load(sz, Base::V(x), off));
                    self.emit(Inst::Mov(res, sz));
                    self.term(Term::Jump(cont));
                    self.switch_to(next);
                }
                let r = self.v(Class::G);
                self.emit(Inst::Call { addr: h2_len as usize, args: vec![Opnd::V(x)], fargs: vec![], ret: Some(r), fret: None });
                self.emit(Inst::Guard(Cond::Cmp(r, Opnd::I(0), Cc::L), esnap));
                self.emit(Inst::Mov(res, r));
                self.term(Term::Jump(cont));
                self.switch_to(cont);
                let k = self.st.stack.len() - 1;
                let a = self.pop();
                self.discard(a, k);
                self.pop();
                self.pop();
                self.push(AV::I64(res));
                true
            }
            (Some("isinstance"), 2) => {
                // isinstance(x, C) with a constant class: exact type match inline,
                // the full check (subclasses, __instancecheck__) through a call
                let AV::Const(cls) = self.st.stack[d - 1] else { return false };
                if !unsafe { (*(cls as *mut PyObject)).ty().tp_flags } & Py_TPFLAGS_TYPE_SUBCLASS != 0 {
                    return false;
                }
                let esnap = self.snap(i, SnapKind::Error);
                let x = self.peek_ptr(2);
                let res = self.v(Class::G);
                let (hit, slow, cont) = (self.new_block(u32::MAX), self.new_block(u32::MAX), self.new_block(u32::MAX));
                let ty = self.v(Class::G);
                self.emit(Inst::Load(ty, Base::V(x), OB_TYPE));
                self.term(Term::Branch(Cond::Cmp(ty, Opnd::I(cls as i64), Cc::E), hit, slow));
                self.switch_to(hit);
                let one = self.imm(1);
                self.emit(Inst::Mov(res, one));
                self.term(Term::Jump(cont));
                self.switch_to(slow);
                let r = self.v(Class::G);
                self.emit(Inst::Call { addr: h2_isinstance as usize, args: vec![Opnd::V(x), Opnd::I(cls as i64)], fargs: vec![], ret: Some(r), fret: None });
                self.emit(Inst::Guard(Cond::Cmp(r, Opnd::I(0), Cc::L), esnap));
                self.emit(Inst::Mov(res, r));
                self.term(Term::Jump(cont));
                self.switch_to(cont);
                self.pop(); // the class (a constant)
                let k = self.st.stack.len() - 1;
                let a = self.pop();
                self.discard(a, k);
                self.pop();
                self.pop();
                // __instancecheck__ may have run arbitrary code
                self.forget_tags();
                self.push(AV::Bool(res));
                true
            }
            _ => false,
        }
    }

    // ------------------------------------------------------------ inlining

    /// The callee of Call instruction i if it can be inlined: a constant plain
    /// function with exactly the given positional arguments, a small straight-line
    /// body (no jumps, no handlers, no cells) of instructions the builder compiles
    /// natively, and tier-1 profiles. Returns (function, its unit, analysis).
    fn inline_candidate(&self, i: usize) -> Option<(usize, Box<Analysis>)> {
        let depth = self.inl_outer.len() + self.inl.is_some() as usize;
        if depth >= 3 || !inline_enabled() {
            return None;
        }
        let n = self.u.an.instrs[i].arg as usize;
        let d = self.st.stack.len();
        let AV::Const(fp) = self.st.stack[d - n - 2] else { return None };
        let has_self = !matches!(self.st.stack[d - n - 1], AV::Null);
        unsafe {
            let fo = &*(fp as *mut PyObject);
            if !fo.is_type(crate::types::funcobject::function_type()) {
                return None;
            }
            let f = crate::types::funcobject::func(fo);
            if f.func_code.is_null() || f.vectorcall.map(|v| v as usize) != Some(crate::interp::function_vectorcall as usize) {
                return None; // replaced vectorcall (C API): the call must go through it
            }
            let code = &*f.func_code;
            let cd = crate::types::code::code_data(code);
            if std::ptr::eq(cd, self.u.cd) || self.inl_cds.contains(&(cd as *const CodeData as usize)) || cd.caches.jit.load(std::sync::atomic::Ordering::Acquire).is_null() {
                return None; // recursion, or not warm (no profiles)
            }
            let co = crate::types::code::code(code).obj;
            use rpy_compiler::bytecode::co as cof;
            let bad = cof::VARARGS | cof::VARKEYWORDS | cof::GENERATOR | cof::COROUTINE | cof::ASYNC_GENERATOR | cof::ITERABLE_COROUTINE;
            if co.co_flags as u32 & bad != 0 || co.co_flags as u32 & cof::OPTIMIZED == 0 || co.co_kwonlyargcount != 0 {
                return None;
            }
            if co.co_argcount as usize != n + has_self as usize || co.co_nlocalsplus as usize > 16 {
                return None;
            }
            if !cd.exception_table.is_empty() || cd.localspluskinds.iter().any(|k| k & (bc::fast::CELL | bc::fast::FREE) != 0) {
                return None;
            }
            if f.func_globals.is_null() || f.func_builtins.is_null() {
                return None;
            }
            let an = super::super::analysis::analyze(&cd.code, &cd.exception_table).ok()?;
            if an.instrs.len() > 64 {
                return None;
            }
            for (k, ins) in an.instrs.iter().enumerate() {
                let ok = match ins.op {
                    Op::ReturnValue => true,
                    // forward branches only (callee paths are duplicated, no loops)
                    Op::PopJumpIfFalse | Op::PopJumpIfTrue | Op::PopJumpIfNone | Op::PopJumpIfNotNone | Op::Jump => {
                        an.index_of[ins.arg as usize] as usize > k
                    }
                    Op::Nop | Op::ExtendedArg | Op::LoadFast | Op::LoadFastCheck | Op::StoreFast | Op::LoadConst | Op::PopTop | Op::PushNull | Op::Copy | Op::Swap => true,
                    Op::LoadGlobal | Op::LoadAttr | Op::StoreAttr | Op::BinaryOp | Op::CompareOp | Op::IsOp | Op::UnaryNegative | Op::UnaryNot | Op::ToBool => true,
                    Op::BinarySubscr | Op::StoreSubscr | Op::UnpackSequence | Op::Call | Op::LoadMethod => true,
                    _ => ins.op == Op::Resume,
                };
                if !ok || an.depth[k] < 0 {
                    return None;
                }
            }
            if an.instrs.last().map(|x| x.op) != Some(Op::ReturnValue) {
                return None;
            }
            Some((fp, Box::new(an)))
        }
    }

    /// Try to compile Call instruction i by inlining its callee. False: nothing was
    /// emitted (the caller emits the call normally).
    fn try_inline(&mut self, i: usize) -> Result<bool, String> {
        let Some((fp, an)) = self.inline_candidate(i) else {
            if super::debug_level() >= 3 {
                let n = self.u.an.instrs[i].arg as usize;
                let d = self.st.stack.len();
                if let AV::Const(p) = self.st.stack[d - n - 2] {
                    let o = unsafe { &*(p as *mut PyObject) };
                    if o.is_type(crate::types::funcobject::function_type()) {
                        let nm = crate::types::str::to_string_lossy(unsafe { &*crate::types::funcobject::func(o).func_qualname });
                        eprintln!("[jit] inline {} not a candidate", nm);
                    }
                } else {
                    eprintln!("[jit] call at {} in {}: callee not constant", i, crate::types::str::to_string_lossy(&self.u.cd.co_qualname));
                }
            }
            return Ok(false);
        };
        let n = self.u.an.instrs[i].arg as usize;
        // rollback point
        let save_st = self.st.clone();
        let save_cur = self.cur;
        let save_insts = self.f.blocks[self.cur as usize].insts.len();
        let save_term = self.f.blocks[self.cur as usize].term.clone();
        let save_tsuccs = self.f.blocks[self.cur as usize].tsuccs.len();
        let (save_nblocks, save_nsnaps, save_ncls) = (self.f.blocks.len(), self.f.snaps.len(), self.f.classes.len());
        let (save_deps, save_cells, save_has_deps, save_closed) = (self.deps.len(), self.cells.len(), self.has_deps, self.closed);
        let caller_u = self.u;
        // error of the call as a whole: the caller's state without the call operands
        self.force_conds();
        let d = self.st.stack.len();
        let base = d - n - 2;
        let ins = self.u.an.instrs[i];
        let err = if let Some(c) = &self.inl {
            // nested: the exception is raised in the enclosing inlined callee's frame
            // (rebuilt by its InlineError stub), which then propagates it further
            let stack = self.st.stack[c.base..base].iter().map(|a| self.mat_of(a)).collect();
            let locals = c.locals.iter().enumerate().map(|(k, a)| (k as u32, self.mat_of(a), false)).collect();
            let inl = Some(InlSnap { func: c.func, res: c.res, cont: c.cont, err: c.err });
            self.inl.as_mut().unwrap().snapped = true;
            self.f.snaps.push(Snapshot { resume: ins.start, lasti: ins.pc, stack, locals, kind: SnapKind::InlineError, inl });
            self.f.snaps.len() as SnapId - 1
        } else {
            let stack = self.st.stack[..base].iter().map(|a| self.mat_of(a)).collect();
            let locals = self.dirty_locals();
            self.f.snaps.push(Snapshot { resume: ins.start, lasti: ins.pc, stack, locals, kind: SnapKind::Error, inl: None });
            self.f.snaps.len() as SnapId - 1
        };
        // callee locals: the arguments (moved from the caller's stack)
        let has_self = !matches!(self.st.stack[d - n - 1], AV::Null);
        let f = unsafe { crate::types::funcobject::func(&*(fp as *mut PyObject)) };
        let code = unsafe { &*f.func_code };
        let cd = crate::types::code::code_data(code);
        let co = crate::types::code::code(code).obj;
        let nlp = co.co_nlocalsplus as usize;
        let mut locals = vec![AV::Null; nlp];
        let first = if has_self { d - n - 1 } else { d - n };
        for (k, slot) in (first..d).enumerate() {
            let a = self.st.stack[slot];
            locals[k] = match a {
                AV::InFrame(t) => {
                    let v = self.v(Class::G);
                    let off = self.slot_off(slot);
                    self.emit(Inst::Load(v, Base::Slots, off));
                    AV::Obj(v, Own::Owned, t)
                }
                AV::Cond(c) => {
                    let v = self.v(Class::G);
                    self.emit(Inst::SetCond(v, c));
                    AV::Bool(v)
                }
                // borrowed from a local of the enclosing inlined callee: the inner
                // callee's local indices mean something else, so own it
                AV::Obj(v, Own::Inl(_), t) => {
                    self.emit(Inst::Incref(v));
                    AV::Obj(v, Own::Owned, t)
                }
                other => other,
            };
        }
        self.st.stack.truncate(base);
        let cont = self.new_block(u32::MAX);
        let res = self.v(Class::G);
        // an enclosing inlined callee's context is suspended while this one is built
        let outer = self.inl.take();
        let nested = outer.is_some();
        if let Some(o) = outer {
            self.inl_outer.push(o);
        }
        self.inl_cds.push(cd as *const CodeData as usize);
        // switch to the callee
        self.inl_an.push(an);
        // (boxed: the addresses stay valid while `inl_an` grows; B outlives every use)
        let an_ref: &'static Analysis = unsafe { &*(&**self.inl_an.last().unwrap() as *const Analysis) };
        let tables: &'static crate::interp::cache::Tables = unsafe { &*(cd.caches.tables(&cd.code) as *const _) };
        let cd_s: &'static CodeData = unsafe { &*(cd as *const CodeData) };
        let unit = Box::new(Unit {
            cd: cd_s,
            an: an_ref,
            tables,
            nlp,
            entry: 0,
            nargs: co.co_argcount as usize,
            globals: f.func_globals,
            builtins: f.func_builtins,
            invalid_flag: self.u.invalid_flag,
            flags: co.co_flags as u32,
            fold_globals: self.u.fold_globals,
            cautious: self.u.cautious,
        });
        self.inl_units.push(unit);
        let up: &'a Unit<'a> = unsafe { &*(&**self.inl_units.last().unwrap() as *const Unit<'static> as *const Unit<'a>) };
        self.u = up;
        self.inl = Some(InlCtx { func: fp, base, locals, cont, res, err, failed: false, snapped: false });
        // Callees with forward branches are compiled path by path (no joins: the
        // code after a branch is duplicated per path, bounded by `steps`); every path
        // ends in its own return, which moves the result into `res` and jumps to
        // `cont`. Straight-line callees keep their result as an abstract value.
        let branchy = an_ref.instrs.iter().any(|x| jumps(x.op));
        let mut ok = true;
        let mut result: Option<AV> = None;
        let mut work: Vec<(usize, State, Vec<AV>, BlockId)> = Vec::new();
        let (mut steps, mut paths) = (0usize, 1usize);
        let mut j = 0usize;
        'paths: loop {
            if j >= an_ref.instrs.len() {
                ok = false;
                break;
            }
            steps += 1;
            if steps > 160 {
                ok = false;
                break;
            }
            let ci = an_ref.instrs[j];
            match ci.op {
                Op::Resume | Op::Nop | Op::ExtendedArg => {}
                Op::LoadFast | Op::LoadFastCheck => {
                    let k = ci.arg as usize;
                    let a = self.inl.as_ref().unwrap().locals[k];
                    let pushed = match a {
                        AV::Null => {
                            ok = false;
                            break;
                        }
                        AV::Obj(v, Own::Owned, t) => AV::Obj(v, Own::Inl(k as u32), t),
                        other => other,
                    };
                    self.push(pushed);
                }
                Op::StoreFast => {
                    let k = ci.arg as usize;
                    let mut a = self.pop();
                    let slot = self.st.stack.len();
                    let mut same = false;
                    match a {
                        AV::InFrame(t) => {
                            let v = self.v(Class::G);
                            let off = self.slot_off(slot);
                            self.emit(Inst::Load(v, Base::Slots, off));
                            a = AV::Obj(v, Own::Owned, t);
                        }
                        AV::Cond(c) => {
                            let v = self.v(Class::G);
                            self.emit(Inst::SetCond(v, c));
                            a = AV::Bool(v);
                        }
                        // a value borrowed from another callee local: own it
                        AV::Obj(v, Own::Inl(o), t) if o as usize != k => {
                            self.emit(Inst::Incref(v));
                            a = AV::Obj(v, Own::Owned, t);
                        }
                        AV::Obj(_, Own::Inl(_), _) => same = true, // x = x
                        _ => {}
                    }
                    if !same {
                        // stack entries borrowed from the old value become owned
                        for s in 0..self.st.stack.len() {
                            if let AV::Obj(v, Own::Inl(o), t) = self.st.stack[s] {
                                if o as usize == k {
                                    self.emit(Inst::Incref(v));
                                    self.st.stack[s] = AV::Obj(v, Own::Owned, t);
                                }
                            }
                        }
                        let old = std::mem::replace(&mut self.inl.as_mut().unwrap().locals[k], a);
                        if let AV::Obj(v, Own::Owned, _) = old {
                            self.emit(Inst::Decref(v));
                        }
                    }
                }
                Op::ReturnValue if !branchy => {
                    result = Some(self.pop());
                    break;
                }
                Op::ReturnValue => {
                    // finish this path: result into `res`, release the callee locals
                    let r = self.pop();
                    if self.st.stack.len() != base {
                        ok = false;
                        break;
                    }
                    let v = self.obj_owned(r);
                    let locals = self.inl.as_ref().unwrap().locals.clone();
                    for a in locals {
                        if let AV::Obj(x, Own::Owned, _) = a {
                            self.emit(Inst::Decref(x));
                        }
                    }
                    self.emit(Inst::Mov(res, v));
                    self.term(Term::Jump(cont));
                    match work.pop() {
                        None => break 'paths,
                        Some((nj, st, locals, blk)) => {
                            self.st = st;
                            self.inl.as_mut().unwrap().locals = locals;
                            self.cur = blk;
                            self.closed = false;
                            j = nj;
                            continue;
                        }
                    }
                }
                Op::Jump => {
                    // forward only (inline_candidate)
                    j = an_ref.index_of[ci.arg as usize] as usize;
                    continue;
                }
                Op::PopJumpIfFalse | Op::PopJumpIfTrue | Op::PopJumpIfNone | Op::PopJumpIfNotNone => {
                    paths += 1;
                    if paths > 8 {
                        ok = false;
                        break;
                    }
                    let (c, jump_if) = match ci.op {
                        Op::PopJumpIfNone | Op::PopJumpIfNotNone => {
                            let (v, own, _) = self.pop_obj();
                            let nv = self.imm(none_p() as i64);
                            let r = self.v(Class::G);
                            self.emit(Inst::SetCond(r, Cond::Cmp(v, Opnd::V(nv), Cc::E)));
                            self.release(v, own);
                            (Cond::Test(r, Opnd::V(r), Cc::NE), ci.op == Op::PopJumpIfNone)
                        }
                        _ => match self.pop_truth_cond() {
                            Some(c) => (c, ci.op == Op::PopJumpIfTrue),
                            None => {
                                ok = false;
                                break;
                            }
                        },
                    };
                    let r = self.v(Class::G);
                    self.emit(Inst::SetCond(r, c));
                    let (jb, fb) = (self.new_block(u32::MAX), self.new_block(u32::MAX));
                    let c2 = Cond::Test(r, Opnd::V(r), if jump_if { Cc::NE } else { Cc::E });
                    self.term(Term::Branch(c2, jb, fb));
                    let target = an_ref.index_of[ci.arg as usize] as usize;
                    let locals = self.inl.as_ref().unwrap().locals.clone();
                    work.push((target, self.st.clone(), locals, jb));
                    self.cur = fb;
                    self.closed = false;
                }
                _ => {
                    self.instr(j, j)?;
                }
            }
            if self.inl.as_ref().unwrap().failed || self.closed {
                ok = false;
                break;
            }
            j += 1;
        }
        let ctx = self.inl.take().unwrap();
        self.u = caller_u;
        self.inl_cds.pop();
        if nested {
            self.inl = self.inl_outer.pop();
        }
        let bad = if branchy { !ok } else { !ok || result.is_none() || self.st.stack.len() != base };
        if bad {
            if super::debug_level() >= 3 {
                let nm = unsafe { crate::types::str::to_string_lossy(&*crate::types::funcobject::func(&*(fp as *mut PyObject)).func_qualname) };
                eprintln!("[jit] inline {} rolled back at callee instr {} ({:?})", nm, j, an_ref.instrs.get(j).map(|x| x.op));
            }
            // roll back everything emitted for the attempt
            self.st = save_st;
            self.f.blocks.truncate(save_nblocks);
            self.cur = save_cur;
            let b = &mut self.f.blocks[save_cur as usize];
            b.insts.truncate(save_insts);
            b.term = save_term;
            b.tsuccs.truncate(save_tsuccs);
            self.f.snaps.truncate(save_nsnaps);
            self.f.classes.truncate(save_ncls);
            self.deps.truncate(save_deps);
            super::free_cells(&self.cells[save_cells..]);
            self.cells.truncate(save_cells);
            self.has_deps = save_has_deps;
            self.closed = save_closed;
            return Ok(false);
        }
        if branchy {
            // every path returned into `res`; the caller's state is what it was
            // before the call, without the call operands (the callee cannot change
            // caller locals, and vregs defined before the call dominate `cont`)
            let mut st = save_st;
            st.stack.truncate(base);
            self.st = st;
            self.cur = cont;
            self.closed = false;
            self.push(AV::Obj(res, Own::Owned, Ty::Unknown));
            return Ok(true);
        }
        // the result: owned (or a value needing no reference); release the callee locals
        let mut r = result.unwrap();
        let mut keep: Option<usize> = None;
        if let AV::Obj(v, Own::Inl(k), t) = r {
            if let AV::Obj(_, Own::Owned, _) = ctx.locals[k as usize] {
                keep = Some(k as usize); // ownership moves to the result
                r = AV::Obj(v, Own::Owned, t);
            } else {
                r = match ctx.locals[k as usize] {
                    AV::Obj(_, o, _) => AV::Obj(v, o, t),
                    other => other,
                };
            }
        }
        for (k, a) in ctx.locals.iter().enumerate() {
            if Some(k) != keep {
                if let AV::Obj(v, Own::Owned, _) = *a {
                    self.emit(Inst::Decref(v));
                }
            }
        }
        if ctx.snapped {
            // deoptimisation stubs deliver the callee's result in `res` at `cont`
            let v = self.obj_owned(r);
            self.emit(Inst::Mov(res, v));
            self.term(Term::Jump(cont));
            self.cur = cont;
            self.closed = false;
            self.push(AV::Obj(res, Own::Owned, Ty::Unknown));
        } else {
            self.term(Term::Jump(cont));
            self.cur = cont;
            self.closed = false;
            self.push(r);
        }
        Ok(true)
    }

    /// Call instruction i of a constant class whose instances are made by
    /// object.__new__ and a Python `__init__`: allocate the instance, then inline
    /// `__init__` as a method call on it (the class's version tag guards both).
    /// False: nothing was emitted.
    fn try_inline_new(&mut self, i: usize) -> Result<bool, String> {
        if !inline_enabled() || self.inl_outer.len() + self.inl.is_some() as usize >= 3 {
            return Ok(false);
        }
        let n = self.u.an.instrs[i].arg as usize;
        let d = self.st.stack.len();
        let AV::Const(tpo) = self.st.stack[d - n - 2] else { return Ok(false) };
        if !matches!(self.st.stack[d - n - 1], AV::Null) {
            return Ok(false);
        }
        let tp = tpo as *mut PyTypeObject;
        let (init, tag) = unsafe {
            let o = &*(tpo as *mut PyObject);
            if !o.is_type(&raw mut crate::types::typeobject::PyType_Type) {
                return Ok(false); // not a class, or a metaclass that may override __call__
            }
            if (*tp).tp_new != Some(crate::types::object_type::object_new)
                || (*tp).tp_init != Some(crate::types::slots::slot_tp_init)
                || (*tp).tp_flags & Py_TPFLAGS_IS_ABSTRACT != 0
                || (*tp).tp_version_tag == 0
            {
                return Ok(false);
            }
            let Some(init) = crate::types::typeobject::lookup(tp, crate::name!("__init__")) else { return Ok(false) };
            if !crate::types::funcobject::is_function(&init) {
                return Ok(false);
            }
            (init.as_ptr() as usize, (*tp).tp_version_tag)
        };
        // rollback point (try_inline rolls back only its own part)
        let save_st = self.st.clone();
        let save_cur = self.cur;
        let save_insts = self.f.blocks[self.cur as usize].insts.len();
        let save_term = self.f.blocks[self.cur as usize].term.clone();
        let save_tsuccs = self.f.blocks[self.cur as usize].tsuccs.len();
        let (save_nblocks, save_nsnaps, save_ncls) = (self.f.blocks.len(), self.f.snaps.len(), self.f.classes.len());
        let save_closed = self.closed;
        // the class must still be the one compiled against (attributes, __init__)
        let snap = self.snap(i, SnapKind::Deopt);
        let tv = self.imm(tpo as i64);
        self.emit(Inst::Guard(Cond::CmpMem32(Base::V(tv), TP_TAG, tag as i64, Cc::NE), snap));
        let obj = self.v(Class::G);
        self.emit(Inst::Call { addr: h2_alloc_instance as usize, args: vec![Opnd::V(tv)], fargs: vec![], ret: Some(obj), fret: None });
        self.emit(Inst::Guard(Cond::Test(obj, Opnd::V(obj), Cc::E), snap));
        // one reference for the result, one moved into __init__'s `self`. The result
        // reference sits on the stack below the call (in the slot of the class), so
        // error exits of the inlined __init__ release it with the rest of the stack.
        self.emit(Inst::Incref(obj));
        self.st.stack.insert(d - n - 2, AV::Obj(obj, Own::Owned, Ty::Exact(tpo)));
        self.st.stack[d - n - 1] = AV::Const(init);
        self.st.stack[d - n] = AV::Obj(obj, Own::Owned, Ty::Exact(tpo));
        let ok = self.try_inline(i)?;
        if !ok {
            self.st = save_st;
            self.f.blocks.truncate(save_nblocks);
            self.cur = save_cur;
            let b = &mut self.f.blocks[save_cur as usize];
            b.insts.truncate(save_insts);
            b.term = save_term;
            b.tsuccs.truncate(save_tsuccs);
            self.f.snaps.truncate(save_nsnaps);
            self.f.classes.truncate(save_ncls);
            self.closed = save_closed;
            return Ok(false);
        }
        // __init__'s result must be None
        let k = self.st.stack.len() - 1;
        match self.pop() {
            AV::Const(p) if p == none_p() => {}
            a => {
                let es = if self.inl.is_some() { self.snap(i, SnapKind::Error) } else {
                    let stack = self.st.stack.iter().map(|x| self.mat_of(x)).collect();
                    let locals = self.dirty_locals();
                    let ins = self.u.an.instrs[i];
                    self.f.snaps.push(Snapshot { resume: ins.start, lasti: ins.pc, stack, locals, kind: SnapKind::Error, inl: None });
                    self.f.snaps.len() as SnapId - 1
                };
                let _ = k;
                let r = self.obj_owned(a);
                let st = self.v(Class::G);
                self.emit(Inst::Call { addr: h2_init_done as usize, args: vec![Opnd::V(r)], fargs: vec![], ret: Some(st), fret: None });
                self.emit(Inst::Guard(Cond::Test(st, Opnd::V(st), Cc::NE), es));
            }
        }
        // the instance is on top of the stack
        Ok(true)
    }

    fn binary_subscr(&mut self, i: usize) {
        let (pa, pb) = self.prof(i);
        let is_list = pa == prof::LIST;
        let is_tuple = pa == prof::TUPLE;
        if !(is_list || is_tuple) || !self.intish(1, pb) {
            self.template(i);
            return;
        }
        let tp = if is_list { list_t() } else { tuple_t() };
        let snap = self.snap(i, SnapKind::Deopt);
        let idx = self.as_i64(1, pb, snap).unwrap();
        let c = self.peek_ptr(2);
        if !self.ty_of(&self.peek(2)).is(tp as *mut PyTypeObject) {
            let t = self.imm(tp as i64);
            let ty = self.v(Class::G);
            self.emit(Inst::Load(ty, Base::V(c), OB_TYPE));
            self.emit(Inst::Guard(Cond::Cmp(ty, Opnd::V(t), Cc::NE), snap));
            self.learn(2, Ty::Exact(tp));
        }
        let size = self.v(Class::G);
        self.emit(Inst::Load(size, Base::V(c), LIST_SIZE));
        // negative index: add size
        let adj = self.v(Class::G);
        self.emit(Inst::Alu(Alu::Add, adj, idx, Opnd::V(size)));
        let ix = self.v(Class::G);
        self.emit(Inst::Select(ix, Cond::Cmp(idx, Opnd::I(0), Cc::L), adj, idx));
        // unsigned compare catches both ends
        self.emit(Inst::Guard(Cond::Cmp(ix, Opnd::V(size), Cc::AE), snap));
        let x = self.v(Class::G);
        if is_list {
            let items = self.v(Class::G);
            self.emit(Inst::Load(items, Base::V(c), LIST_ITEMS));
            self.emit(Inst::LoadIdx(x, items, ix, 0));
        } else {
            self.emit(Inst::LoadIdx(x, c, ix, TUPLE_ITEMS));
        }
        self.emit(Inst::Incref(x));
        self.pop2_release();
        self.push(AV::Obj(x, Own::Owned, Ty::Unknown));
    }

    fn store_subscr(&mut self, i: usize) {
        // [value, container, key]
        let (pa, pb) = self.prof(i);
        if pa != prof::LIST || !self.intish(1, pb) {
            self.template(i);
            return;
        }
        let snap = self.snap(i, SnapKind::Deopt);
        let idx = self.as_i64(1, pb, snap).unwrap();
        let c = self.peek_ptr(2);
        if !self.ty_of(&self.peek(2)).is(crate::types::list::list_type()) {
            let t = self.imm(list_t() as i64);
            let ty = self.v(Class::G);
            self.emit(Inst::Load(ty, Base::V(c), OB_TYPE));
            self.emit(Inst::Guard(Cond::Cmp(ty, Opnd::V(t), Cc::NE), snap));
            self.learn(2, Ty::Exact(list_t()));
        }
        self.emit(Inst::TryLock(c, snap));
        let size = self.v(Class::G);
        self.emit(Inst::Load(size, Base::V(c), LIST_SIZE));
        let adj = self.v(Class::G);
        self.emit(Inst::Alu(Alu::Add, adj, idx, Opnd::V(size)));
        let ix = self.v(Class::G);
        self.emit(Inst::Select(ix, Cond::Cmp(idx, Opnd::I(0), Cc::L), adj, idx));
        // out of range: unlock happens in the deopt path? no: check before locking
        // effects; on failure release the lock first via a separate guard sequence
        let bad = self.v(Class::G);
        self.emit(Inst::SetCond(bad, Cond::Cmp(ix, Opnd::V(size), Cc::AE)));
        let ok = self.new_block(u32::MAX);
        let fail = self.new_block(u32::MAX);
        self.term(Term::Branch(Cond::Test(bad, Opnd::V(bad), Cc::NE), fail, ok));
        self.cur = fail;
        self.closed = false;
        self.emit(Inst::Unlock(c));
        self.term(Term::Deopt(snap));
        self.cur = ok;
        self.closed = false;
        // value: owned object
        let k = self.st.stack.len() - 3;
        let va = self.st.stack[k];
        let val = match va {
            AV::InFrame(_) => {
                let v = self.v(Class::G);
                let off = self.slot_off(k);
                self.emit(Inst::Load(v, Base::Slots, off));
                v
            }
            other => self.obj_owned(other),
        };
        let items = self.v(Class::G);
        self.emit(Inst::Load(items, Base::V(c), LIST_ITEMS));
        let old = self.v(Class::G);
        self.emit(Inst::LoadIdx(old, items, ix, 0));
        self.emit(Inst::StoreIdx(items, ix, 0, val));
        self.emit(Inst::Unlock(c));
        self.emit(Inst::Decref(old));
        // pop key, container (release) and the value (moved into the list)
        self.pop2_release();
        self.pop();
    }

    fn unpack_sequence(&mut self, i: usize, last: usize) {
        let ins = self.u.an.instrs[i];
        let n = ins.arg as usize;
        let (pa, _) = self.prof(i);
        let is_list = pa == prof::LIST;
        let is_tuple = pa == prof::TUPLE;
        let both = pa == prof::LIST | prof::TUPLE;
        if !(is_list || is_tuple || both) || n == 0 || n > 16 {
            self.template(i);
            return;
        }
        let snap = self.snap(i, SnapKind::Deopt);
        let s = self.peek_ptr(1);
        if both {
            // tuple or list: items inline or behind ob_item; ob_size at the same offset
            let ty = self.v(Class::G);
            self.emit(Inst::Load(ty, Base::V(s), OB_TYPE));
            let tt = self.imm(tuple_t() as i64);
            let items = self.v(Class::G);
            let (bt, bl, join) = (self.new_block(u32::MAX), self.new_block(u32::MAX), self.new_block(u32::MAX));
            self.term(Term::Branch(Cond::Cmp(ty, Opnd::V(tt), Cc::E), bt, bl));
            self.switch_to(bt);
            self.emit(Inst::Alu(Alu::Add, items, s, Opnd::I(TUPLE_ITEMS as i64)));
            self.term(Term::Jump(join));
            self.switch_to(bl);
            let lt = self.imm(list_t() as i64);
            self.emit(Inst::Guard(Cond::Cmp(ty, Opnd::V(lt), Cc::NE), snap));
            self.emit(Inst::Load(items, Base::V(s), LIST_ITEMS));
            self.term(Term::Jump(join));
            self.switch_to(join);
            self.emit(Inst::Guard(Cond::CmpMem(Base::V(s), LIST_SIZE, n as i64, Cc::NE), snap));
            self.unpack_or_fuse(i, last, n, (items, 0));
            return;
        }
        let tp = if is_list { list_t() } else { tuple_t() };
        if !self.ty_of(&self.peek(1)).is(tp as *mut PyTypeObject) {
            let t = self.imm(tp as i64);
            let ty = self.v(Class::G);
            self.emit(Inst::Load(ty, Base::V(s), OB_TYPE));
            self.emit(Inst::Guard(Cond::Cmp(ty, Opnd::V(t), Cc::NE), snap));
        }
        self.emit(Inst::Guard(Cond::CmpMem(Base::V(s), LIST_SIZE, n as i64, Cc::NE), snap));
        let base = if is_list {
            let items = self.v(Class::G);
            self.emit(Inst::Load(items, Base::V(s), LIST_ITEMS));
            (items, 0)
        } else {
            (s, TUPLE_ITEMS)
        };
        self.unpack_or_fuse(i, last, n, base);
    }

    /// (jit6) `a, b, c, ... = seq` with every target a distinct frame-represented
    /// local: one call to `h2_unpack_store` instead of n inline incref/store/decref
    /// sequences (the inline form is ~1.8 KB of machine code per statement, which
    /// made functions with many such statements instruction-fetch bound). Otherwise
    /// the items are pushed as usual.
    fn unpack_or_fuse(&mut self, i: usize, last: usize, n: usize, base: (V, i32)) {
        let ok = n >= 4 && n <= 15 && i + n <= last && self.inl.is_none() && {
            let mut seen = Vec::with_capacity(n);
            (1..=n).all(|k| {
                let ins = self.u.an.instrs[i + k];
                let li = ins.arg as usize;
                let fine = ins.op == Op::StoreFast && li < 255 && self.reps[li] == Rep::Frame && !seen.contains(&li);
                seen.push(li);
                fine
            })
        };
        if !ok {
            self.unpack_items(n, base);
            return;
        }
        let targets: Vec<u32> = (1..=n).map(|k| self.u.an.instrs[i + k].arg).collect();
        for &t in &targets {
            self.unborrow(t);
            let dummy = AV::Obj(0, Own::Owned, Ty::Unknown);
            self.note_store_kind(t as usize, &dummy);
        }
        let items = if base.1 == 0 {
            base.0
        } else {
            let p = self.v(Class::G);
            self.emit(Inst::Lea(p, Base::V(base.0), base.1));
            p
        };
        let sl = self.v(Class::G);
        self.emit(Inst::SlotAddr(sl, 0));
        let (mut lo, mut hi) = (0u64, (n as u64) << 56);
        for (k, &t) in targets.iter().enumerate() {
            if k < 8 {
                lo |= (t as u64) << (8 * k);
            } else {
                hi |= (t as u64) << (8 * (k - 8));
            }
        }
        self.emit(Inst::Call { addr: h2_unpack_store as usize, args: vec![Opnd::V(items), Opnd::V(sl), Opnd::I(lo as i64), Opnd::I(hi as i64)], fargs: vec![], ret: None, fret: None });
        // the old values' release may run arbitrary code
        self.forget_tags();
        let k = self.st.stack.len() - 1;
        let a = self.pop();
        self.discard(a, k);
        for &t in &targets {
            let li = t as usize;
            self.st.locals[li] = LS::Frame(Ty::Unknown);
            self.st.ptr[li] = None;
            self.st.unb[li] = None;
        }
        self.skip_extra = n;
    }

    /// Push the n items at `base` (incref'd, last item deepest) in place of the
    /// sequence on top of the stack.
    fn unpack_items(&mut self, n: usize, base: (V, i32)) {
        let mut vals = Vec::with_capacity(n);
        for k in 0..n {
            let x = self.v(Class::G);
            self.emit(Inst::Load(x, Base::V(base.0), base.1 + 8 * k as i32));
            self.emit(Inst::Incref(x));
            vals.push(x);
        }
        let k = self.st.stack.len() - 1;
        let a = self.pop();
        self.discard(a, k);
        for k in (0..n).rev() {
            self.push(AV::Obj(vals[k], Own::Owned, Ty::Unknown));
        }
    }
}

fn negate(c: Cond) -> Cond {
    match c {
        Cond::Cmp(a, b, cc) => Cond::Cmp(a, b, cc.not()),
        Cond::Cmp32(a, b, cc) => Cond::Cmp32(a, b, cc.not()),
        Cond::CmpMem(a, o, b, cc) => Cond::CmpMem(a, o, b, cc.not()),
        Cond::CmpMem32(a, o, b, cc) => Cond::CmpMem32(a, o, b, cc.not()),
        Cond::Test(a, b, cc) => Cond::Test(a, b, cc.not()),
        // float compares: not(a < b) is not (a >= b) with NaNs; keep a flag form
        other => other,
    }
}

/// x ** y for x > 0 (guarded by the caller).
pub extern "C" fn h_fpow(x: f64, y: f64) -> f64 {
    x.powf(y)
}

/// Does shape `sh` lack attribute `name` (interned)? If so remember the shape in the
/// site's memo cell. 1: lacks it, 0: has it (or no shape).
/// Does instance dict `d` (an exact dict) certainly lack the interned str `name`? 1 if
/// so; 0 if it has it or the answer would need a comparison that can run code (a
/// non-str key with the same hash) or a long scan. Only reads the entries.
pub unsafe extern "C" fn h2_dict_lacks(d: *mut PyObject, name: *mut PyObject) -> u64 {
    unsafe {
        let d = &*d;
        if !crate::types::dict::is_dict_exact(d) {
            return 0;
        }
        let t = crate::types::dict::table(d);
        let es = t.entries();
        if es.len() > 64 {
            return 0;
        }
        let h = crate::types::str::str_hash(&*name);
        for e in es {
            if e.key.is_null() || e.value.is_null() || e.hash != h {
                continue;
            }
            if e.key == name || !crate::types::str::is_str_exact(&*e.key) || crate::types::str::str_eq(&*e.key, &*name) {
                return 0;
            }
        }
        1
    }
}

pub unsafe extern "C" fn h_shape_lacks(sh: *const crate::shape::Shape, name: *mut PyObject, cell: *const std::sync::atomic::AtomicUsize) -> u64 {
    unsafe {
        if sh.is_null() {
            return 0;
        }
        if (*sh).keys().contains(&name) {
            return 0;
        }
        (*cell).store(sh as usize, std::sync::atomic::Ordering::Relaxed);
        1
    }
}

/// (defining type, name) of a builtin method descriptor.
unsafe fn method_descr_name(p: usize) -> Option<(usize, &'static str)> {
    unsafe {
        let o = &*(p as *mut PyObject);
        if !o.is_type(crate::types::descr::method_descr_type()) {
            return None;
        }
        let md = p as *mut PyMethodDescrObject;
        let ml = (*md).d_method;
        if ml.is_null() || (*ml).ml_name.is_null() {
            return None;
        }
        let tp = (*md).d_common.d_type as usize;
        std::ffi::CStr::from_ptr((*ml).ml_name).to_str().ok().map(|s| (tp, s))
    }
}

/// dict.get(key[, default]) (try_method_builtin): `d` is a dict (or subclass), the
/// default NULL when absent. New reference, or NULL with an exception set.
pub unsafe extern "C" fn h2_dict_get(d: *mut PyObject, key: *mut PyObject, dflt: *mut PyObject) -> *mut PyObject {
    unsafe {
        match crate::types::dict::get_item(&*d, &*key) {
            Ok(Some(v)) => v.into_raw(),
            Ok(None) => {
                let r = if dflt.is_null() { crate::types::none::none().as_ptr() } else { dflt };
                crate::object::incref(r);
                r
            }
            Err(_) => std::ptr::null_mut(),
        }
    }
}

/// list.append(x) (try_method_builtin): 1, or 0 with an exception set.
pub unsafe extern "C" fn h2_list_append(l: *mut PyObject, x: *mut PyObject, _unused: *mut PyObject) -> u64 {
    unsafe {
        crate::types::list::append(&*l, (*x).to_ref());
        1
    }
}

/// (jit6) Fused `UnpackSequence n` + n `StoreFast` (`unpack_or_fuse`): `items` are the
/// sequence's n items (borrowed); local slot k gets item k. Slot indices are bytes:
/// items 0..8 in `lo`, 8..15 in `hi`, n in the top byte of `hi`. All items are
/// referenced before any old value is released (a release may run arbitrary code
/// that drops the sequence).
pub unsafe extern "C" fn h2_unpack_store(items: *const *mut PyObject, slots: *mut *mut PyObject, lo: u64, hi: u64) {
    unsafe {
        let n = ((hi >> 56) as usize).min(15);
        let idx: [u8; 16] = std::mem::transmute([lo.to_le(), hi.to_le()]);
        let mut old: [std::mem::MaybeUninit<*mut PyObject>; 15] = [std::mem::MaybeUninit::uninit(); 15];
        for k in 0..n {
            let x = *items.add(k);
            crate::object::incref(x);
            let s = slots.add(*idx.get_unchecked(k) as usize);
            old.get_unchecked_mut(k).write(*s);
            *s = x;
        }
        for k in 0..n {
            crate::object::xdecref(old.get_unchecked(k).assume_init());
        }
    }
}

/// `iter(range(*args))` (try_range_iter): borrowed arguments, n of them (1..=3).
/// New reference, or NULL with an exception set.
pub unsafe extern "C" fn h2_range_iter(a: *mut PyObject, b: *mut PyObject, c: *mut PyObject, n: u64) -> *mut PyObject {
    unsafe {
        let args: [*mut PyObject; 3] = [a, b, c];
        let args = &args[..n as usize];
        let mut v = [0i64; 3];
        let mut fast = true;
        for (k, &p) in args.iter().enumerate() {
            match crate::types::int::is_int_exact(&*p).then(|| crate::types::int::as_i64(&*p)).flatten() {
                Some(x) => v[k] = x,
                None => fast = false,
            }
        }
        if fast {
            let (start, stop, step) = match n {
                1 => (0, v[0], 1),
                2 => (v[0], v[1], 1),
                _ => (v[0], v[1], v[2]),
            };
            if step != 0 {
                return crate::types::range::iter_i64(start, stop, step).into_raw();
            }
        }
        // anything else: the real range() (errors, __index__) and iter()
        let refs: Vec<&PyObject> = args.iter().map(|&p| &*p).collect();
        let r = crate::abstract_::call(crate::types::typeobject::type_obj(crate::types::range::range_type()), &refs)
            .and_then(|r| crate::abstract_::get_iter(&r));
        crate::errors::into_c(r)
    }
}

/// len(o) for types without an inline path: the length, or -1 with an exception set.
pub unsafe extern "C" fn h2_len(o: *mut PyObject) -> i64 {
    unsafe {
        match crate::abstract_::length(&*o) {
            Ok(n) => n as i64,
            Err(_) => -1,
        }
    }
}

/// Generic attribute load through the inline cache (slow path of tier-2 LoadAttr):
/// new reference, or NULL with an exception set.
/// New instance of the class `tp` for an inlined constructor call (tp_alloc), or
/// NULL with no exception set (the code deoptimises and the interpreter retries).
pub unsafe extern "C" fn h2_alloc_instance(tp: *mut PyTypeObject) -> *mut PyObject {
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(crate::types::typeobject::generic_alloc);
        let o = alloc(tp, 0);
        if o.is_null() {
            crate::errors::clear_exc();
        }
        o
    }
}

/// Result `r` (owned) of an inlined `__init__`: 0 if it is None, else TypeError (as
/// type.__call__ reports it) and 1.
pub unsafe extern "C" fn h2_init_done(r: *mut PyObject) -> u64 {
    unsafe {
        if r == none_p() as *mut PyObject {
            crate::object::decref(r);
            return 0;
        }
        let _ = crate::err!(TypeError, "__init__() should return None, not '{}'", crate::types::typeobject::type_name((*r).ty()));
        crate::object::decref(r);
        1
    }
}

/// RPYTHON_JIT2_KEEPFACTS=1: keep type facts across decrefs (measurement switch;
/// unsafe when finalizers mutate classes).
fn keep_facts() -> bool {
    static F: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *F.get_or_init(|| std::env::var("RPYTHON_JIT2_KEEPFACTS").is_ok_and(|v| v == "1"))
}

/// isinstance(o, cls) for compiled code: 1, 0, or -1 with an exception set.
pub unsafe extern "C" fn h2_isinstance(o: *mut PyObject, cls: *mut PyObject) -> i64 {
    unsafe {
        match crate::abstract_::isinstance(&*o, &*cls) {
            Ok(b) => b as i64,
            Err(_) => -1,
        }
    }
}

/// Truth value of `o` for compiled ToBool / UnaryNot: 1, 0, or -1 with an exception set.
pub unsafe extern "C" fn h2_truth(o: *mut PyObject) -> i64 {
    unsafe {
        if o == true_p() as *mut PyObject {
            return 1;
        }
        if o == false_p() as *mut PyObject || o == none_p() as *mut PyObject {
            return 0;
        }
        match crate::abstract_::is_true(&*o) {
            Ok(b) => b as i64,
            Err(_) => -1,
        }
    }
}

pub unsafe extern "C" fn h2_load_attr(o: *mut PyObject, name: *mut PyObject, es: *const crate::interp::cache::Entry) -> *mut PyObject {
    unsafe {
        let es = std::slice::from_raw_parts(es, crate::interp::cache::POLY);
        match crate::interp::specialize::load_attr(&*o, &*name, es) {
            Ok(v) => v.into_raw(),
            Err(_) => std::ptr::null_mut(),
        }
    }
}

/// Generic attribute store through the inline cache (slow path of tier-2 StoreAttr).
/// 0: done, the reference to `v` was consumed; 1: exception set, `v` untouched.
pub unsafe extern "C" fn h2_store_attr(o: *mut PyObject, name: *mut PyObject, v: *mut PyObject, es: *const crate::interp::cache::Entry) -> u64 {
    unsafe {
        let es = std::slice::from_raw_parts(es, crate::interp::cache::POLY);
        match crate::interp::specialize::store_attr(&*o, &*name, &*v, es) {
            Ok(()) => {
                crate::object::decref(v);
                0
            }
            Err(_) => 1,
        }
    }
}

// ================================================================ driver

/// Locals that may be given an unboxed representation: plain locals (not cells,
/// not arguments) only accessed by LoadFast / LoadFastCheck / StoreFast.
fn unbox_candidates(u: &Unit) -> Vec<bool> {
    let n = u.nlp;
    let mut ok = vec![true; n];
    for i in 0..n.min(u.nargs) {
        ok[i] = false;
    }
    for (i, k) in u.cd.localspluskinds.iter().enumerate() {
        if i < n && k & (bc::fast::CELL | bc::fast::FREE) != 0 {
            ok[i] = false;
        }
    }
    for ins in &u.an.instrs {
        match ins.op {
            Op::LoadFast | Op::LoadFastCheck | Op::StoreFast => {}
            Op::DeleteFast | Op::LoadFastAndClear | Op::MakeCell | Op::LoadDeref | Op::StoreDeref | Op::DeleteDeref | Op::LoadFromDictOrDeref => {
                if (ins.arg as usize) < n {
                    ok[ins.arg as usize] = false;
                }
            }
            _ => {}
        }
    }
    ok
}

fn build_once(u: &Unit, reps: &[Rep], assume: &mut Vec<Option<State>>) -> Result<(Built, bool, Vec<u8>), String> {
    let resumes = resume_points(u);
    let cfg = build_cfg(u.an, u.entry, &resumes);
    let nb = cfg.blocks.len();
    let mut f = Func { blocks: Vec::new(), classes: Vec::new(), snaps: Vec::new(), order: Vec::new(), entries: Vec::new() };
    let mut ir_block = vec![u32::MAX; nb];
    for &b in &cfg.order {
        f.blocks.push(Block { insts: Vec::new(), term: Term::None, instr: cfg.blocks[b].first as u32, tsuccs: Vec::new() });
        ir_block[b] = f.blocks.len() as u32 - 1;
    }
    let mut homes = vec![u32::MAX; u.nlp];
    for i in 0..u.nlp {
        match reps[i] {
            Rep::I64 => homes[i] = f.new_v(Class::G),
            Rep::F64 => homes[i] = f.new_v(Class::F),
            Rep::Frame => {}
        }
    }
    if assume.len() != nb {
        *assume = vec![None; nb];
    }
    let order = cfg.order.clone();
    let mut b = B {
        u,
        cfg,
        f,
        cur: 0,
        st: State { stack: vec![], locals: vec![], ptr: vec![], unb: vec![] },
        pending: vec![Vec::new(); nb],
        entry_state: vec![None; nb],
        ir_block,
        assume,
        restart: false,
        reps: reps.to_vec(),
        homes,
        store_kinds: vec![0; u.nlp],
        has_deps: u.fold_globals && u.an.instrs.iter().any(|x| x.op == Op::LoadGlobal),
        deps: Vec::new(),
        cells: Vec::new(),
        closed: false,
        inl: None,
        inl_an: Vec::new(),
        inl_units: Vec::new(),
        is_gen: !resumes.is_empty(),
        inl_outer: Vec::new(),
        fatal: None,
        skip_extra: 0,
        inl_cds: vec![u.cd as *const CodeData as usize],
    };
    // OSR unit / generator resumption: a separate entry block unboxes the unboxed
    // locals from the frame and jumps to the loop header / instruction after the yield
    let synthetic_entry = |b: &mut B, at: usize| -> BlockId {
        let eb = b.new_block(u32::MAX);
        b.cur = eb;
        b.closed = false;
        b.st = b.initial_state_at(at);
        for li in 0..u.nlp {
            if b.reps[li] != Rep::Frame {
                let snap = b.snap(at, SnapKind::Deopt);
                let k = if b.reps[li] == Rep::F64 { BoxK::Float } else { BoxK::Int };
                let home = b.homes[li];
                b.emit(Inst::LoadLocalUnboxed { home, slot: li as u32, k, snap });
            }
        }
        let hb = b.cfg.block_of[at] as usize;
        let t = b.edge(hb);
        b.term(Term::Jump(t));
        eb
    };
    let mut entry_block = b.ir_block[b.cfg.block_of[u.entry] as usize];
    if u.entry != 0 {
        entry_block = synthetic_entry(&mut b, u.entry);
    }
    let mut resume_blocks = Vec::new();
    for &r in &resumes {
        resume_blocks.push((u.an.instrs[r].start, synthetic_entry(&mut b, r)));
    }
    for &bb in &order {
        b.build_block(bb)?;
        if b.restart {
            break;
        }
    }
    if let Some(e) = b.fatal {
        return Err(e.into());
    }
    let restart = b.restart;
    let mut instr_block = vec![u32::MAX; u.an.instrs.len()];
    for &bb in &order {
        instr_block[b.cfg.blocks[bb].first] = b.ir_block[bb];
    }
    let mut f = b.f;
    f.entries.push((u.an.instrs[u.entry].start, entry_block));
    f.entries.extend(resume_blocks);
    // emission order: creation order (bytecode RPO with synthetic blocks appended)
    f.order = (0..f.blocks.len() as u32).collect();
    let sk = b.store_kinds;
    Ok((Built { func: f, has_deps: b.has_deps, deps: b.deps, cells: b.cells, instr_block, reps: b.reps, homes: b.homes }, restart, sk))
}

/// Build the IR of a unit: choose local representations, iterate loop-header
/// assumptions to a fixed point.
pub fn build(u: &Unit) -> Result<Built, String> {
    let mut cand = unbox_candidates(u);
    if !resume_points(u).is_empty() {
        // generator units keep locals boxed: every suspension would have to box the
        // unboxed ones into the frame and every resumption unbox them again
        cand.iter_mut().for_each(|c| *c = false);
    }
    let mut reps = vec![Rep::Frame; u.nlp];
    let mut assume: Vec<Option<State>> = Vec::new();
    let mut chose_reps = false;
    for _round in 0..12 {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build_once(u, &reps, &mut assume)));
        let (built, restart, sk) = match r {
            Ok(x) => x?,
            Err(_) => return Err("tier2 builder panicked".into()),
        };
        if restart {
            super::free_cells(&built.cells);
            continue;
        }
        if !chose_reps {
            chose_reps = true;
            let mut changed = false;
            for i in 0..u.nlp {
                if !cand[i] {
                    continue;
                }
                let r = match sk[i] {
                    SK_INT => Rep::I64,
                    SK_FLOAT => Rep::F64,
                    _ => Rep::Frame,
                };
                if r != Rep::Frame {
                    reps[i] = r;
                    changed = true;
                }
            }
            if changed {
                super::free_cells(&built.cells);
                assume.clear();
                continue;
            }
        }
        return Ok(built);
    }
    Err("tier2: no fixed point".into())
}
