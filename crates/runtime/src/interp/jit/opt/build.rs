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

fn build_cfg(an: &Analysis, entry: usize) -> Cfg {
    let n = an.instrs.len();
    let mut leader = vec![false; n + 1];
    leader[0] = true;
    leader[entry] = true;
    for i in 0..n {
        let ins = an.instrs[i];
        if an.is_target[i] {
            leader[i] = true;
        }
        if jumps(ins.op) || is_terminator(ins.op) {
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
        if !is_terminator(ins.op) && blocks[b].last + 1 < n {
            s.push(block_of[blocks[b].last + 1] as usize);
        }
        blocks[b].succs = s;
    }
    // reverse postorder from the entry block; backedges mark loop headers
    let eb = block_of[entry] as usize;
    let mut post = Vec::new();
    let mut state = vec![0u8; blocks.len()]; // 0 new, 1 on stack, 2 done
    let mut stack: Vec<(usize, usize)> = vec![(eb, 0)];
    state[eb] = 1;
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
/// Is `tp` (a type pointer recorded in an inline-cache entry) still the live type
/// with version tag `tag`? Tags are unique and never reused, so a freed or modified
/// type cannot match.
fn live_type(tp: usize, tag: u64) -> bool {
    tag != 0 && crate::interp::cache::type_tag(tp as *mut PyTypeObject) as u64 == tag
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
        self.f.blocks[self.cur as usize].insts.push(i);
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
        let stack = self.st.stack.iter().map(|a| self.mat_of(a)).collect();
        let locals = self.dirty_locals();
        self.f.snaps.push(Snapshot { resume: ins.start, lasti: ins.pc, stack, locals, kind });
        self.f.snaps.len() as SnapId - 1
    }
    /// Snapshot for resuming at the instruction after `i` with the current state.
    fn snap_next(&mut self, i: usize) -> SnapId {
        self.force_conds();
        let resume = self.u.an.instrs.get(i + 1).map_or(self.u.an.instrs[i].pc + 1, |n| n.start);
        let stack = self.st.stack.iter().map(|a| self.mat_of(a)).collect();
        let locals = self.dirty_locals();
        self.f.snaps.push(Snapshot { resume, lasti: self.u.an.instrs[i].pc, stack, locals, kind: SnapKind::Deopt });
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
        self.materialize_stack();
        let locals = self.dirty_locals();
        let exit = if locals.is_empty() {
            None
        } else {
            let ins = self.u.an.instrs[i];
            self.f.snaps.push(Snapshot { resume: ins.start, lasti: ins.pc, stack: Vec::new(), locals, kind: SnapKind::TemplateExit });
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
        let n = self.u.nlp;
        let mut locals = Vec::with_capacity(n);
        for i in 0..n {
            locals.push(match self.reps[i] {
                Rep::Frame => LS::Frame(Ty::Unknown),
                _ => LS::Unboxed { dirty: false, def: if self.u.entry == 0 { Def::No } else { Def::Maybe } },
            });
        }
        let d = self.u.an.depth[self.u.entry].max(0) as usize;
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
                let w = meet(es, &st);
                self.assume[t] = Some(w);
                self.restart = true;
            }
        } else {
            self.pending[t].push(st);
        }
    }

    fn block_entry(&mut self, b: usize) -> State {
        if b == self.cfg.block_of[self.u.entry] as usize && self.pending[b].is_empty() && self.u.entry == self.cfg.blocks[b].first {
            let mut s = self.initial_state();
            if let Some(a) = &self.assume[b] {
                s = meet(&s, a);
            }
            return s;
        }
        let mut it = std::mem::take(&mut self.pending[b]).into_iter();
        let mut s = it.next().expect("tier2: block without predecessor state");
        for o in it {
            s = meet(&s, &o);
        }
        if self.cfg.blocks[b].first == self.u.entry {
            s = meet(&s, &self.initial_state());
        }
        if self.cfg.blocks[b].header {
            if let Some(a) = &self.assume[b] {
                s = meet(&s, a);
            }
            s.ptr.iter_mut().for_each(|p| *p = None);
            s.unb.iter_mut().for_each(|p| *p = None);
        }
        s
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

fn meet(a: &State, b: &State) -> State {
    assert_eq!(a.stack.len(), b.stack.len(), "tier2: stack depth mismatch at join");
    let stack = a
        .stack
        .iter()
        .zip(&b.stack)
        .map(|(x, y)| match (x, y) {
            (AV::InFrame(s), AV::InFrame(t)) => AV::InFrame(s.meet(*t)),
            _ if x == y => *x,
            _ => panic!("tier2: unmaterialised stack entry at join"),
        })
        .collect();
    let locals = a
        .locals
        .iter()
        .zip(&b.locals)
        .map(|(x, y)| match (x, y) {
            (LS::Frame(s), LS::Frame(t)) => LS::Frame(s.meet(*t)),
            (LS::Unboxed { dirty: d1, def: f1 }, LS::Unboxed { dirty: d2, def: f2 }) => LS::Unboxed { dirty: *d1 || *d2, def: f1.meet(*f2) },
            _ => panic!("tier2: local representation mismatch"),
        })
        .collect();
    let ptr = a.ptr.iter().zip(&b.ptr).map(|(x, y)| if x == y { *x } else { None }).collect();
    let unb = a.unb.iter().zip(&b.unb).map(|(x, y)| if x == y { *x } else { None }).collect();
    State { stack, locals, ptr, unb }
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
        let st = self.block_entry(b);
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
            i += 1 + consumed_next as usize;
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
        debug_assert_eq!(self.st.stack.len() as i32, self.u.an.depth[i], "tier2: depth mismatch at {}", ins.pc);
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
                _ => self.template(i),
            },
            Op::PopJumpIfFalse | Op::PopJumpIfTrue => {
                let on_true = ins.op == Op::PopJumpIfTrue;
                let c = match self.peek(1) {
                    AV::Cond(c) => {
                        self.pop();
                        Some(c)
                    }
                    AV::Bool(v) => {
                        self.pop();
                        Some(Cond::Test(v, Opnd::V(v), Cc::NE))
                    }
                    AV::I64(v) => {
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
                };
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
                    self.f.snaps.push(Snapshot { resume: ins.start, lasti: ins.pc, stack: Vec::new(), locals, kind: SnapKind::TemplateExit });
                    Some(self.f.snaps.len() as SnapId - 1)
                };
                self.term(Term::Return(v, d, snap));
            }
            Op::BinarySubscr => self.binary_subscr(i),
            Op::LoadAttr => self.load_attr(i),
            Op::LoadMethod => self.load_method(i),
            Op::StoreAttr => self.store_attr(i),
            Op::StoreSubscr => self.store_subscr(i),
            Op::UnpackSequence => self.unpack_sequence(i),
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

    fn peek_ptr_if_inframe(&mut self, n: usize) {
        if let AV::InFrame(_) = self.peek(n) {
            self.peek_ptr(n);
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

    /// Current shape of values word `w`.
    fn shape_of(&mut self, w: V) -> V {
        let (sh_off, _vals, _id, _len, _keys) = crate::shape::jit_layout();
        let sh = self.v(Class::G);
        self.emit(Inst::Load(sh, Base::V(w), sh_off - 1));
        sh
    }

    /// Leave for `f` unless the shape of values word `w` has `name` at index h.
    fn check_shape_key(&mut self, w: V, h: u64, name: usize, f: Fail) {
        let (_sh, _vals, _id, len_off, keys_off) = crate::shape::jit_layout();
        let sh = self.shape_of(w);
        self.check(Cond::Test(sh, Opnd::V(sh), Cc::E), f);
        self.check(Cond::CmpMem32(Base::V(sh), len_off, h as i64, Cc::BE), f);
        let keys = self.v(Class::G);
        self.emit(Inst::Load(keys, Base::V(sh), keys_off));
        self.check(Cond::CmpMem(Base::V(keys), 8 * h as i32, name as i64, Cc::NE), f);
    }

    fn vals_off(h: u64) -> i32 {
        let (_sh, vals_off, _id, _len, _keys) = crate::shape::jit_layout();
        vals_off - 1 + 8 * h as i32
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
        if es.len() == 1 && cases[0].kind != kind::INSTANCE_DICT {
            // monomorphic slot / class attribute: no slow path needed
            let s = cases[0];
            let p = self.guard_tag(1, s.a as u32, snap);
            let k = self.st.stack.len() - 1;
            if s.kind == kind::SLOT {
                let x = self.v(Class::G);
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
        let bodies = if es.len() == 1 {
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
                    let x = self.v(Class::G);
                    self.emit(Inst::Load(x, Base::V(w), Self::vals_off(s.b)));
                    self.check(Cond::Test(x, Opnd::V(x), Cc::E), f);
                    x
                }
                kind::SLOT => {
                    let x = self.v(Class::G);
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
        self.emit(Inst::Call { addr: h2_load_attr as usize, args: vec![Opnd::V(p), Opnd::I(name as i64), Opnd::I(ea)], fargs: vec![], ret: Some(r), fret: None });
        self.emit(Inst::Guard(Cond::Test(r, Opnd::V(r), Cc::E), esnap));
        self.emit(Inst::Mov(res, r));
        self.term(Term::Jump(cont));
        self.switch_to(cont);
        let k = self.st.stack.len() - 1;
        let a = self.pop();
        self.discard(a, k);
        self.push(AV::Obj(res, Own::Owned, Ty::Unknown));
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
            if d.is_null() || !crate::types::dict::is_dict_exact(&*d) {
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
        self.check(Cond::Test(w, Opnd::I(1), Cc::E), f);
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
                    let off = Self::vals_off(s.b);
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
                        self.check(Cond::CmpMem32(Base::V(w), crate::shape::values_cap_offset() - 1, s.b as i64, Cc::BE), f);
                        self.term(Term::Jump(add));
                        self.switch_to(add);
                        self.emit(Inst::Store(Base::V(w), off, val));
                        let (sh_off, _vals, _id, _len, _keys) = crate::shape::jit_layout();
                        let cv = self.imm(child as i64);
                        self.emit(Inst::Store(Base::V(w), sh_off - 1, cv));
                        self.term(Term::Jump(cont));
                        self.switch_to(replace);
                    } else {
                        self.check_shape_key(w, s.b, name, f);
                    }
                    let old = self.v(Class::G);
                    self.emit(Inst::Load(old, Base::V(w), off));
                    self.emit(Inst::Store(Base::V(w), off, val));
                    self.emit(Inst::XDecref(old));
                }
                _ => {
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
        self.emit(Inst::CallChecked { addr: h2_store_attr as usize, args: vec![Opnd::V(p), Opnd::I(name as i64), Opnd::V(val), Opnd::I(ea)], err: esnap });
        self.term(Term::Jump(cont));
        self.switch_to(cont);
        // value moved into the object / released by the runtime
        let k = self.st.stack.len() - 1;
        let a = self.pop();
        self.discard(a, k);
        self.pop();
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

    fn unpack_sequence(&mut self, i: usize) {
        let ins = self.u.an.instrs[i];
        let n = ins.arg as usize;
        let (pa, _) = self.prof(i);
        let is_list = pa == prof::LIST;
        let is_tuple = pa == prof::TUPLE;
        if !(is_list || is_tuple) || n == 0 || n > 16 {
            self.template(i);
            return;
        }
        let tp = if is_list { list_t() } else { tuple_t() };
        let snap = self.snap(i, SnapKind::Deopt);
        let s = self.peek_ptr(1);
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

/// Generic attribute load through the inline cache (slow path of tier-2 LoadAttr):
/// new reference, or NULL with an exception set.
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
    let cfg = build_cfg(u.an, u.entry);
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
    };
    // OSR unit: a separate entry block unboxes the unboxed locals from the frame and
    // jumps to the loop header
    let mut entry_block = b.ir_block[b.cfg.block_of[u.entry] as usize];
    if u.entry != 0 {
        let eb = b.new_block(u32::MAX);
        entry_block = eb;
        b.cur = eb;
        b.closed = false;
        b.st = b.initial_state();
        for li in 0..u.nlp {
            if b.reps[li] != Rep::Frame {
                let snap = b.snap(u.entry, SnapKind::Deopt);
                let k = if b.reps[li] == Rep::F64 { BoxK::Float } else { BoxK::Int };
                let home = b.homes[li];
                b.emit(Inst::LoadLocalUnboxed { home, slot: li as u32, k, snap });
            }
        }
        let hb = b.cfg.block_of[u.entry] as usize;
        let t = b.edge(hb);
        b.term(Term::Jump(t));
    }
    for &bb in &order {
        b.build_block(bb)?;
        if b.restart {
            break;
        }
    }
    let restart = b.restart;
    let mut instr_block = vec![u32::MAX; u.an.instrs.len()];
    for &bb in &order {
        instr_block[b.cfg.blocks[bb].first] = b.ir_block[bb];
    }
    let mut f = b.f;
    f.entries.push((u.an.instrs[u.entry].start, entry_block));
    // emission order: creation order (bytecode RPO with synthetic blocks appended)
    f.order = (0..f.blocks.len() as u32).collect();
    let sk = b.store_kinds;
    Ok((Built { func: f, has_deps: b.has_deps, deps: b.deps, cells: b.cells, instr_block, reps: b.reps, homes: b.homes }, restart, sk))
}

/// Build the IR of a unit: choose local representations, iterate loop-header
/// assumptions to a fixed point.
pub fn build(u: &Unit) -> Result<Built, String> {
    let cand = unbox_candidates(u);
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
