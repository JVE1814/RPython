//! Tier-2 low-level IR: typed virtual registers, explicit memory operations,
//! reference counting, guards with deoptimisation snapshots, and "template"
//! instructions (a bytecode instruction emitted by the tier-1 template generator
//! with the whole frame state materialised).
//!
//! Virtual registers are not SSA: a vreg may be assigned in several places (the
//! homes of unboxed locals are assigned at every store). The register allocator
//! works on liveness intervals, which does not need SSA.

use crate::interp::jit::x64::Cc;

pub type V = u32;
pub type BlockId = u32;
pub type SnapId = u32;

/// Register class of a vreg.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    /// general purpose: pointers, i64, booleans
    G,
    /// f64 in an xmm register
    F,
}

/// Base register of a memory operand.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Base {
    /// the interpreter frame (rbx)
    Frame,
    /// the frame's slot array (r12)
    Slots,
    /// the thread state (r13)
    Ts,
    V(V),
}

/// Second operand of a comparison / ALU operation.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Opnd {
    V(V),
    I(i64),
}

/// Integer ALU operations.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Alu {
    Add,
    Sub,
    Mul,
    And,
    Or,
    Xor,
    Shl,
    Sar,
}

/// Float operations.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FAlu {
    Add,
    Sub,
    Mul,
    Div,
}

/// A condition evaluated by Guard / Branch / SetCond.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Cond {
    /// integer compare a ? b with condition code
    Cmp(V, Opnd, Cc),
    /// 32-bit compare of a ? b
    Cmp32(V, Opnd, Cc),
    /// compare qword [base+off] ? imm
    CmpMem(Base, i32, i64, Cc),
    /// compare dword [base+off] ? imm
    CmpMem32(Base, i32, i64, Cc),
    /// test a & b
    Test(V, Opnd, Cc),
    /// float compare with Python semantics (NaN: all false except !=); cc is one of
    /// L, LE, E, NE, G, GE meaning a ? b
    FCmp(V, V, Cc),
    /// byte at absolute address != 0
    FlagSet(usize),
}

/// Which runtime routine boxes a value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BoxK {
    Int,
    Float,
    /// 0/1 -> False/True
    Bool,
}

#[derive(Clone, Debug)]
pub enum Inst {
    /// gpr = imm64
    Imm(V, i64),
    /// xmm = f64 constant
    FImm(V, f64),
    Mov(V, V),
    Load(V, Base, i32),
    Load32(V, Base, i32),
    Load8(V, Base, i32),
    Store(Base, i32, V),
    StoreImm(Base, i32, i32),
    Store32Imm(Base, i32, i32),
    /// dst = [base + idx*8 + disp]
    LoadIdx(V, V, V, i32),
    /// [base + idx*8 + disp] = src
    StoreIdx(V, V, i32, V),
    LoadF(V, Base, i32),
    StoreF(Base, i32, V),
    Lea(V, Base, i32),
    /// dst = a op b (no overflow check)
    Alu(Alu, V, V, Opnd),
    /// dst = a op b, deoptimise on signed overflow (Add/Sub/Mul)
    AluOv(Alu, V, V, Opnd, SnapId),
    Neg(V, V),
    /// dst = -a, deopt on overflow
    NegOv(V, V, SnapId),
    /// Python floor division / modulo on i64 (b != 0 and no overflow guaranteed by
    /// guards emitted before): dst = a // b or a % b
    IDivMod(V, V, V, bool),
    FAlu(FAlu, V, V, V),
    FNeg(V, V),
    FSqrt(V, V),
    /// xmm = (double) gpr
    I2F(V, V),
    /// dst = 1 if cond else 0
    SetCond(V, Cond),
    /// dst = cond ? a : b (gprs)
    Select(V, Cond, V, V),
    /// deoptimise if cond holds
    Guard(Cond, SnapId),
    Incref(V),
    Decref(V),
    XDecref(V),
    /// dst = new object for the value in src (registers preserved)
    Box(BoxK, V, V),
    /// dst = i64 value of the exact int object src (at most two 30-bit digits),
    /// deoptimise otherwise
    UnboxInt(V, V, SnapId),
    /// dst = f64 value of the exact float object src, deoptimise otherwise
    UnboxFloat(V, V, SnapId),
    /// dst = f64 value of an exact float or of a one-digit int, deoptimise otherwise
    UnboxNumF(V, V, SnapId),
    /// OSR entry of an unboxed local: if slots[slot] != NULL, home = its unboxed
    /// value (deoptimise if it has another type)
    LoadLocalUnboxed { home: V, slot: u32, k: BoxK, snap: SnapId },
    /// acquire the per-object mutex of the object in v, deoptimise if contended
    TryLock(V, SnapId),
    Unlock(V),
    /// (scale2) qsbr::note_attr_read of the object in v (before a lock-free load of
    /// its attribute storage)
    NoteRead(V),
    /// (scale2) dst = 1 if the per-object mutex of the object in v was free and is now
    /// held by this thread, else 0
    TryLockFlag(V, V),
    /// (scale2) dst = previous qword at [base + off], replaced by src (atomic swap)
    Xchg(V, Base, i32, V),
    /// (scale2) qsbr::release_attr(obj, old): release `old` (may be null), just swapped
    /// out of obj's attribute storage, at once or through QSBR (registers preserved)
    ReleaseAttr(V, V),
    /// Call a runtime function (SysV); clobbers caller-saved registers. Result in a
    /// gpr (`ret`) or xmm0 (`fret`).
    Call { addr: usize, args: Vec<Opnd>, fargs: Vec<V>, ret: Option<V>, fret: Option<V> },
    /// Like Call, but the result is a status: non-zero -> exit through the error
    /// snapshot (exception set).
    CallChecked { addr: usize, args: Vec<Opnd>, err: SnapId },
    /// lea of a frame slot address as call argument helper: dst = &slots[i]
    SlotAddr(V, u32),
    /// Emit the tier-1 template for bytecode instruction #i (all state materialised,
    /// clobbers every allocatable register). The mask (refs1): value-stack depths
    /// holding borrowed (tagged) entries the template consumes.
    Template(u32, Option<SnapId>, u64),
    /// eval-breaker poll; the slow path syncs `snap`, calls the handler, and either
    /// continues or exits with an error.
    Poll(SnapId),
    /// f.lasti = pc
    SetLasti(u32),
    /// store "None" into slot i if it is NULL (definedness marker of unboxed locals)
    MarkDefined(u32),
    /// Sync of an unboxed local: box `v` and store into slot (decref old), only if the
    /// slot is non-NULL when `maybe` (local possibly undefined).
    SyncLocal { slot: u32, v: V, k: BoxK, maybe: bool },
}

#[derive(Clone, Debug)]
pub enum Term {
    Jump(BlockId),
    Branch(Cond, BlockId, BlockId),
    /// return the owned object in v (stack_top = nlp + depth); the snapshot lists the
    /// dirty unboxed locals (synced if a frame object outlives the activation)
    Return(V, u32, Option<SnapId>),
    /// generator suspension: yield the owned object in v (stack_top = nlp + depth),
    /// sync the dirty locals listed in the snapshot (its `resume` is the instruction
    /// the next resumption enters at), yield_from_send = the i32
    Yield(V, u32, SnapId, i32),
    /// unconditional deoptimisation
    Deopt(SnapId),
    /// the block ends in a template instruction that never falls through (or jumps
    /// to tier-2 labels itself): nothing to emit
    None,
}

/// How a snapshot entry is materialised into the frame.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mat {
    /// owned object: store
    Owned(V),
    /// borrowed object: incref + store
    Borrowed(V),
    /// constant object pointer (incref unless immortal) + store
    Const(usize),
    BoxInt(V),
    BoxFloat(V),
    BoxBool(V),
    /// already in the frame slot
    InFrame,
}

/// Frame state at a deoptimisation / error / poll point.
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// instruction index to resume at (code unit index, ExtendedArg prefix start)
    pub resume: u32,
    /// f.lasti to report (error exits)
    pub lasti: u32,
    /// value-stack contents (depth = len)
    pub stack: Vec<Mat>,
    /// dirty unboxed locals: (slot, value, maybe-undefined)
    pub locals: Vec<(u32, Mat, bool)>,
    pub kind: SnapKind,
    /// InlineDeopt / InlineError: the inlined callee (its frame is rebuilt from
    /// `locals` (all callee locals in order) and `stack`, and run in the interpreter)
    pub inl: Option<InlSnap>,
}

#[derive(Clone, Copy, Debug)]
pub struct InlSnap {
    /// the callee function object
    pub func: usize,
    /// the callee result goes here, then execution continues at block `cont`
    pub res: V,
    pub cont: BlockId,
    /// error snapshot of the call in the caller (exception from the callee)
    pub err: SnapId,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SnapKind {
    /// resume in the interpreter at `resume`
    Deopt,
    /// exception set: exit with status error (lasti, stack depth)
    Error,
    /// eval-breaker slow path (sync locals, handle, continue or error)
    Poll,
    /// exit path of a template instruction: sync the dirty locals, keep the status
    TemplateExit,
    /// guard failure inside an inlined callee: rebuild the callee frame, finish it
    /// in the interpreter, continue in compiled code with its result
    InlineDeopt,
    /// exception inside an inlined callee: like InlineDeopt, raising it at `lasti`
    InlineError,
}

pub struct Block {
    pub insts: Vec<Inst>,
    pub term: Term,
    /// blocks a template instruction of this block jumps to by itself (control flow
    /// not described by `term`; needed for liveness)
    pub tsuccs: Vec<BlockId>,
    /// first bytecode instruction index of the block (u32::MAX for synthetic blocks)
    pub instr: u32,
}

pub struct Func {
    pub blocks: Vec<Block>,
    pub classes: Vec<Class>,
    pub snaps: Vec<Snapshot>,
    /// emission order of blocks
    pub order: Vec<BlockId>,
    /// entry blocks: (code unit pc, block)
    pub entries: Vec<(u32, BlockId)>,
}

impl Func {
    pub fn new_v(&mut self, c: Class) -> V {
        self.classes.push(c);
        self.classes.len() as V - 1
    }
}

impl Inst {
    /// Registers read (uses) and written (defs) by an instruction.
    pub fn uses_defs(&self, uses: &mut Vec<V>, defs: &mut Vec<V>) {
        fn b(base: &Base, u: &mut Vec<V>) {
            if let Base::V(v) = base {
                u.push(*v)
            }
        }
        fn o(op: &Opnd, u: &mut Vec<V>) {
            if let Opnd::V(v) = op {
                u.push(*v)
            }
        }
        fn c(cd: &Cond, u: &mut Vec<V>) {
            match cd {
                Cond::Cmp(a, x, _) | Cond::Cmp32(a, x, _) | Cond::Test(a, x, _) => {
                    u.push(*a);
                    o(x, u)
                }
                Cond::CmpMem(bs, _, _, _) | Cond::CmpMem32(bs, _, _, _) => b(bs, u),
                Cond::FCmp(a, x, _) => {
                    u.push(*a);
                    u.push(*x)
                }
                Cond::FlagSet(_) => {}
            }
        }
        use Inst::*;
        match self {
            Imm(d, _) | FImm(d, _) | SlotAddr(d, _) => defs.push(*d),
            LoadLocalUnboxed { home, .. } => defs.push(*home),
            TryLock(v, _) | Unlock(v) | NoteRead(v) => uses.push(*v),
            TryLockFlag(d, v) => {
                uses.push(*v);
                defs.push(*d)
            }
            Xchg(d, bs, _, s) => {
                b(bs, uses);
                uses.push(*s);
                defs.push(*d)
            }
            ReleaseAttr(o, x) => {
                uses.push(*o);
                uses.push(*x)
            }
            Mov(d, s)
            | Neg(d, s)
            | NegOv(d, s, _)
            | FNeg(d, s)
            | FSqrt(d, s)
            | I2F(d, s)
            | Box(_, d, s)
            | UnboxInt(d, s, _)
            | UnboxFloat(d, s, _)
            | UnboxNumF(d, s, _) => {
                uses.push(*s);
                defs.push(*d)
            }
            Load(d, bs, _) | Load32(d, bs, _) | Load8(d, bs, _) | LoadF(d, bs, _) | Lea(d, bs, _) => {
                b(bs, uses);
                defs.push(*d)
            }
            Store(bs, _, s) | StoreF(bs, _, s) => {
                b(bs, uses);
                uses.push(*s)
            }
            StoreImm(bs, _, _) | Store32Imm(bs, _, _) => b(bs, uses),
            LoadIdx(d, bs, ix, _) => {
                uses.push(*bs);
                uses.push(*ix);
                defs.push(*d)
            }
            StoreIdx(bs, ix, _, s) => {
                uses.push(*bs);
                uses.push(*ix);
                uses.push(*s)
            }
            Alu(_, d, a, x) | AluOv(_, d, a, x, _) => {
                uses.push(*a);
                o(x, uses);
                defs.push(*d)
            }
            IDivMod(d, a, x, _) | FAlu(_, d, a, x) => {
                uses.push(*a);
                uses.push(*x);
                defs.push(*d)
            }
            SetCond(d, cd) => {
                c(cd, uses);
                defs.push(*d)
            }
            Select(d, cd, a, x) => {
                c(cd, uses);
                uses.push(*a);
                uses.push(*x);
                defs.push(*d)
            }
            Guard(cd, _) => c(cd, uses),
            Incref(v) | Decref(v) | XDecref(v) => uses.push(*v),
            Call { args, fargs, ret, fret, .. } => {
                for a in args {
                    o(a, uses)
                }
                uses.extend(fargs.iter().copied());
                if let Some(r) = ret {
                    defs.push(*r)
                }
                if let Some(r) = fret {
                    defs.push(*r)
                }
            }
            CallChecked { args, .. } => {
                for a in args {
                    o(a, uses)
                }
            }
            SyncLocal { v, .. } => uses.push(*v),
            Template(..) | Poll(_) | SetLasti(_) | MarkDefined(_) => {}
        }
    }

    /// Does the instruction clobber caller-saved registers (gpr and xmm)?
    pub fn clobbers_caller_saved(&self) -> bool {
        matches!(self, Inst::Call { .. } | Inst::CallChecked { .. } | Inst::Template(..))
    }
    /// Does it also clobber r14 (the tier-1 templates use it as scratch)?
    pub fn clobbers_r14(&self) -> bool {
        matches!(self, Inst::Template(..))
    }
    /// Snapshot referenced by the instruction, if any.
    pub fn snap(&self) -> Option<SnapId> {
        match self {
            Inst::AluOv(_, _, _, _, s)
            | Inst::NegOv(_, _, s)
            | Inst::Guard(_, s)
            | Inst::Poll(s)
            | Inst::UnboxInt(_, _, s)
            | Inst::UnboxFloat(_, _, s)
            | Inst::UnboxNumF(_, _, s)
            | Inst::TryLock(_, s) => Some(*s),
            Inst::LoadLocalUnboxed { snap, .. } => Some(*snap),
            Inst::CallChecked { err, .. } => Some(*err),
            Inst::Template(_, s, _) => *s,
            _ => None,
        }
    }
}

impl Mat {
    pub fn vreg(&self) -> Option<V> {
        match *self {
            Mat::Owned(v) | Mat::Borrowed(v) | Mat::BoxInt(v) | Mat::BoxFloat(v) | Mat::BoxBool(v) => Some(v),
            _ => None,
        }
    }
}

impl Snapshot {
    pub fn vregs(&self, out: &mut Vec<V>) {
        for m in &self.stack {
            if let Some(v) = m.vreg() {
                out.push(v)
            }
        }
        for (_, m, _) in &self.locals {
            if let Some(v) = m.vreg() {
                out.push(v)
            }
        }
    }
}

impl Term {
    pub fn uses(&self, u: &mut Vec<V>) {
        match self {
            Term::Branch(c, _, _) => {
                let mut d = Vec::new();
                Inst::Guard(*c, 0).uses_defs(u, &mut d);
            }
            Term::Return(v, _, _) | Term::Yield(v, _, _, _) => u.push(*v),
            _ => {}
        }
    }
    pub fn succs(&self) -> Vec<BlockId> {
        match self {
            Term::Jump(b) => vec![*b],
            Term::Branch(_, t, f) => vec![*t, *f],
            _ => vec![],
        }
    }
}
