//! Instruction IR (basic blocks with pseudo-instructions for exception handling) and the
//! assembler that turns it into a [`CodeUnit`]: dead block elimination, NOP removal, stack
//! depth, zero-cost exception table, definite-assignment analysis (LoadFastCheck -> LoadFast),
//! jump resolution with ExtendedArg.

use crate::bytecode::*;
use crate::error::{CompileError, ErrorKind};

pub(crate) type Label = u32;
pub(crate) const NO_LABEL: Label = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IOp {
    Op(Op),
    /// Handler pushes [exc] at the current depth.
    SetupFinally,
    /// Handler pushes [lasti, exc] at the current depth.
    SetupCleanup,
    /// Like SetupCleanup but the handler depth is one less (the `__enter__` result is popped
    /// as the first instruction of the protected block).
    SetupWith,
    /// Handler for the YieldValue inside await/yield-from loops: handler depth is one less
    /// than the depth at the setup (keeps only the receiver), pushes [exc], no lasti.
    SetupYield,
    PopBlock,
}

/// IR instruction flag: StoreFast that may store NULL (restoring after an inlined comprehension).
pub(crate) const F_MAY_STORE_NULL: u8 = 1;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Instr {
    pub op: IOp,
    pub arg: u32,
    pub target: Label,
    pub loc: Location,
    pub flags: u8,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Block {
    pub instrs: Vec<Instr>,
}

/// Slot references for deref-like ops are symbolic until assembly.
pub(crate) const CELL_TAG: u32 = 1 << 30;
pub(crate) const FREE_TAG: u32 = 1 << 31;

#[derive(Default)]
pub(crate) struct Ir {
    pub blocks: Vec<Block>,
    /// Layout order of blocks.
    pub order: Vec<Label>,
    pub cur: Label,
}

impl Ir {
    pub fn new() -> Ir {
        let mut ir = Ir { blocks: vec![Block::default()], order: vec![0], cur: 0 };
        ir.cur = 0;
        ir
    }
    pub fn new_label(&mut self) -> Label {
        self.blocks.push(Block::default());
        (self.blocks.len() - 1) as Label
    }
    /// Start emitting into `l` (appended to the layout).
    pub fn use_label(&mut self, l: Label) {
        self.order.push(l);
        self.cur = l;
    }
    pub fn emit(&mut self, i: Instr) {
        let cur = self.cur;
        let b = &mut self.blocks[cur as usize];
        if let Some(last) = b.instrs.last() {
            if is_terminal(last.op) {
                // start a fresh (unreachable unless jumped to) block
                let l = self.new_label();
                self.use_label(l);
                self.blocks[l as usize].instrs.push(i);
                return;
            }
        }
        b.instrs.push(i);
    }
    pub fn last_is_terminal(&self) -> bool {
        self.blocks[self.cur as usize].instrs.last().map(|i| is_terminal(i.op)).unwrap_or(false)
    }
}

pub(crate) fn is_terminal(op: IOp) -> bool {
    matches!(op, IOp::Op(Op::Jump) | IOp::Op(Op::ReturnValue) | IOp::Op(Op::RaiseVarargs) | IOp::Op(Op::Reraise))
}

fn is_setup(op: IOp) -> bool {
    matches!(op, IOp::SetupFinally | IOp::SetupCleanup | IOp::SetupWith | IOp::SetupYield)
}

/// Stack effect of an instruction. `jump`: effect when the branch is taken.
pub(crate) fn stack_effect(op: IOp, arg: u32, jump: bool) -> i32 {
    let n = arg as i32;
    match op {
        IOp::SetupFinally => {
            if jump {
                1
            } else {
                0
            }
        }
        IOp::SetupCleanup => {
            if jump {
                2
            } else {
                0
            }
        }
        IOp::SetupWith => {
            if jump {
                1
            } else {
                0
            }
        }
        IOp::SetupYield => 0,
        IOp::PopBlock => 0,
        IOp::Op(o) => match o {
            Op::Nop | Op::Resume | Op::ExtendedArg => 0,
            Op::PopTop => -1,
            Op::PushNull | Op::Copy => 1,
            Op::Swap => 0,
            Op::LoadConst | Op::LoadFast | Op::LoadFastCheck | Op::LoadFastAndClear => 1,
            Op::StoreFast => -1,
            Op::DeleteFast | Op::MakeCell | Op::CopyFreeVars => 0,
            Op::LoadDeref => 1,
            Op::StoreDeref => -1,
            Op::DeleteDeref => 0,
            Op::LoadFromDictOrDeref | Op::LoadFromDictOrGlobals => 0,
            Op::LoadLocals | Op::LoadName | Op::LoadGlobal => 1,
            Op::StoreName | Op::StoreGlobal => -1,
            Op::DeleteName | Op::DeleteGlobal => 0,
            Op::LoadAttr => 0,
            Op::LoadMethod => 1,
            Op::StoreAttr => -2,
            Op::DeleteAttr => -1,
            Op::LoadSuperAttr => {
                if arg & 1 != 0 {
                    -1
                } else {
                    -2
                }
            }
            Op::BinaryOp => -1,
            Op::UnaryNegative | Op::UnaryPositive | Op::UnaryInvert | Op::UnaryNot | Op::ToBool => 0,
            Op::CompareOp | Op::IsOp | Op::ContainsOp | Op::BinarySubscr => -1,
            Op::StoreSubscr => -3,
            Op::DeleteSubscr => -2,
            Op::BinarySlice => -2,
            Op::StoreSlice => -4,
            Op::BuildSlice => 1 - n,
            Op::BuildTuple | Op::BuildList | Op::BuildSet | Op::BuildString => 1 - n,
            Op::BuildMap => 1 - 2 * n,
            Op::ListAppend | Op::SetAdd => -1,
            Op::MapAdd => -2,
            Op::ListExtend | Op::SetUpdate | Op::DictUpdate | Op::DictMerge => -1,
            Op::UnpackSequence => n - 1,
            Op::UnpackEx => ((arg & 0xff) + (arg >> 8)) as i32,
            Op::Jump => 0,
            Op::PopJumpIfFalse | Op::PopJumpIfTrue | Op::PopJumpIfNone | Op::PopJumpIfNotNone => -1,
            Op::JumpIfFalseOrPop | Op::JumpIfTrueOrPop => {
                if jump {
                    0
                } else {
                    -1
                }
            }
            Op::GetIter | Op::GetAiter => 0,
            Op::ForIter => {
                if jump {
                    -1
                } else {
                    1
                }
            }
            Op::GetAnext => 1,
            Op::EndAsyncFor => -2,
            Op::GetAwaitable | Op::GetYieldFromIter => 0,
            Op::Send => {
                if jump {
                    -1
                } else {
                    0
                }
            }
            Op::YieldValue => 0,
            Op::CleanupThrow => -1,
            Op::ReturnValue => -1,
            Op::Call => -n - 1,
            Op::CallKw => -n - 2,
            Op::CallFunctionEx => -3,
            Op::CallIntrinsic1 => 0,
            Op::CallIntrinsic2 => -1,
            Op::MakeFunction => 0,
            Op::SetFunctionAttribute => -1,
            Op::LoadBuildClass => 1,
            Op::RaiseVarargs => -n,
            Op::Reraise => {
                if arg != 0 {
                    -2
                } else {
                    -1
                }
            }
            Op::PushExcInfo => 1,
            Op::PopExcept => -1,
            Op::CheckExcMatch | Op::CheckEgMatch => 0,
            Op::WithExceptStart => 1,
            Op::BeforeWith | Op::BeforeAsyncWith => 2,
            Op::SetupAnnotations => 0,
            Op::ImportName => -1,
            Op::ImportFrom => 1,
            Op::FormatSimple | Op::ConvertValue => 0,
            Op::FormatWithSpec => -1,
            Op::BuildTemplate => -1,
            Op::BuildInterpolation => -1 - (arg & 1) as i32,
            Op::MatchClass => -2,
            Op::MatchMapping | Op::MatchSequence | Op::MatchKeys | Op::GetLen => 1,
            Op::LoadCommonConstant => 1,
            Op::Unused1 => -1,
            Op::Unused2 => 0,
            Op::Unused3 => -1,
        },
    }
}

/// Information the assembler needs about the unit besides its instructions.
pub(crate) struct UnitInfo {
    pub name: String,
    pub qualname: String,
    pub filename: String,
    pub firstlineno: u32,
    pub argcount: u32,
    pub posonlyargcount: u32,
    pub kwonlyargcount: u32,
    pub flags: u32,
    pub consts: Vec<Constant>,
    pub names: Vec<String>,
    /// fast locals (parameters first); kinds
    pub varnames: Vec<String>,
    pub varkinds: Vec<u8>,
    /// cells not sharing a slot with an argument; `cell_arg[i]` = Some(varnames index) if shared
    pub cellvars: Vec<String>,
    pub cellkinds: Vec<u8>,
    pub cell_arg: Vec<Option<u32>>,
    pub freevars: Vec<String>,
    /// number of parameters (bound at entry)
    pub nparams: u32,
}

fn ierr(msg: &str) -> CompileError {
    CompileError::other(ErrorKind::SystemError, format!("internal compiler error: {}", msg))
}

pub(crate) fn assemble(mut ir: Ir, info: UnitInfo) -> Result<CodeUnit, CompileError> {
    // ---- map symbolic cell/free slots to localsplus indices
    let nvars = info.varnames.len() as u32;
    let mut cell_index: Vec<u32> = Vec::with_capacity(info.cellvars.len());
    let mut next = nvars;
    for c in &info.cell_arg {
        match c {
            Some(v) => cell_index.push(*v),
            None => {
                cell_index.push(next);
                next += 1;
            }
        }
    }
    let free_base = next;
    let nlocalsplus = free_base + info.freevars.len() as u32;
    for b in ir.blocks.iter_mut() {
        for i in b.instrs.iter_mut() {
            if i.arg & FREE_TAG != 0 && is_slot_op(i.op) {
                i.arg = free_base + (i.arg & !FREE_TAG);
            } else if i.arg & CELL_TAG != 0 && is_slot_op(i.op) {
                i.arg = cell_index[(i.arg & !CELL_TAG) as usize];
            }
        }
    }

    // ---- reachability, jump threading, exit duplication
    // Truncate instructions after a terminal one (dead tail) in each block.
    for b in ir.blocks.iter_mut() {
        if let Some(p) = b.instrs.iter().position(|i| is_terminal(i.op)) {
            b.instrs.truncate(p + 1);
        }
    }
    let order = std::mem::take(&mut ir.order);
    let mut order = filter_reachable(&ir, order);
    thread_jumps(&mut ir, &order);
    duplicate_exits_without_lineno(&mut ir, &mut order);
    let order = filter_reachable(&ir, order);
    let nblocks = ir.blocks.len();
    let mut pos_in_order = vec![usize::MAX; nblocks];
    for (p, &b) in order.iter().enumerate() {
        pos_in_order[b as usize] = p;
    }
    let next_of = |b: Label| -> Option<Label> {
        let p = pos_in_order[b as usize];
        if p + 1 < order.len() {
            Some(order[p + 1])
        } else {
            None
        }
    };
    let falls_through = |blk: &Block| -> bool { blk.instrs.last().map(|i| !is_terminal(i.op)).unwrap_or(true) };

    // ---- location propagation within blocks and along fallthrough edges
    {
        let mut is_target = vec![false; nblocks];
        for &b in &order {
            for i in &ir.blocks[b as usize].instrs {
                if i.target != NO_LABEL {
                    is_target[i.target as usize] = true;
                }
            }
        }
        let mut prev_loc: Option<Location> = None;
        for &b in &order {
            let blk = &mut ir.blocks[b as usize];
            let mut cur = if is_target[b as usize] { None } else { prev_loc };
            for i in blk.instrs.iter_mut() {
                if i.loc.lineno == 0 {
                    if let Some(l) = cur {
                        if i.flags & 0x80 == 0 {
                            i.loc = l;
                        }
                    }
                } else {
                    cur = Some(i.loc);
                }
            }
            prev_loc = if falls_through(blk) { cur } else { None };
        }
    }

    // ---- NOP removal (keep NOPs that carry the only instance of a line number)
    for &b in &order {
        let blk = &mut ir.blocks[b as usize];
        let mut out: Vec<Instr> = Vec::with_capacity(blk.instrs.len());
        let n = blk.instrs.len();
        for k in 0..n {
            let i = blk.instrs[k];
            if i.op == IOp::Op(Op::Nop) {
                if i.loc.lineno == 0 {
                    continue;
                }
                if let Some(p) = out.last() {
                    if p.loc.lineno == i.loc.lineno {
                        continue;
                    }
                }
                if k + 1 < n {
                    let nx = &mut blk.instrs[k + 1];
                    if nx.loc.lineno == i.loc.lineno {
                        continue;
                    }
                    if nx.loc.lineno == 0 && nx.op != IOp::Op(Op::Nop) {
                        nx.loc = i.loc;
                        continue;
                    }
                }
            }
            out.push(i);
        }
        blk.instrs = out;
    }

    // ---- stack depth
    let mut depth_at = vec![-1i32; nblocks];
    let mut setup_depth: std::collections::HashMap<(Label, usize), i32> = std::collections::HashMap::new();
    let mut maxdepth = 0i32;
    {
        let mut work: Vec<Label> = vec![order[0]];
        depth_at[order[0] as usize] = 0;
        let push = |work: &mut Vec<Label>, depth_at: &mut Vec<i32>, b: Label, d: i32| -> Result<(), CompileError> {
            let cur = depth_at[b as usize];
            if cur < 0 {
                depth_at[b as usize] = d;
                work.push(b);
            } else if cur != d {
                return Err(ierr(&format!("inconsistent stack depth at block {} ({} vs {})", b, cur, d)));
            }
            Ok(())
        };
        while let Some(b) = work.pop() {
            let mut d = depth_at[b as usize];
            let blk = &ir.blocks[b as usize];
            for (k, i) in blk.instrs.iter().enumerate() {
                if i.target != NO_LABEL {
                    let jd = d + stack_effect(i.op, i.arg, true);
                    if jd < 0 {
                        return Err(ierr("negative stack depth (jump)"));
                    }
                    maxdepth = maxdepth.max(jd);
                    if is_setup(i.op) {
                        setup_depth.insert((b, k), d);
                    }
                    push(&mut work, &mut depth_at, i.target, jd)?;
                }
                d += stack_effect(i.op, i.arg, false);
                if d < 0 {
                    return Err(ierr(&format!("negative stack depth in {} at {:?}", info.qualname, i.op)));
                }
                maxdepth = maxdepth.max(d);
            }
            if falls_through(blk) {
                if let Some(n) = next_of(b) {
                    push(&mut work, &mut depth_at, n, d)?;
                }
            }
        }
    }

    // ---- exception handler labelling
    #[derive(Clone, Copy, PartialEq, Eq)]
    struct Handler {
        target: Label,
        depth: u32,
        lasti: bool,
    }
    let mut handlers: Vec<Vec<Option<Handler>>> = vec![Vec::new(); nblocks];
    {
        let mut entry: Vec<Option<Vec<Handler>>> = vec![None; nblocks];
        entry[order[0] as usize] = Some(Vec::new());
        let mut work = vec![order[0]];
        while let Some(b) = work.pop() {
            let mut stack = entry[b as usize].clone().unwrap();
            let blk = &ir.blocks[b as usize];
            let mut hs = Vec::with_capacity(blk.instrs.len());
            for (k, i) in blk.instrs.iter().enumerate() {
                match i.op {
                    IOp::SetupFinally | IOp::SetupCleanup | IOp::SetupWith | IOp::SetupYield => {
                        if entry[i.target as usize].is_none() {
                            entry[i.target as usize] = Some(stack.clone());
                            work.push(i.target);
                        }
                        let d = *setup_depth.get(&(b, k)).unwrap_or(&0);
                        let h = match i.op {
                            IOp::SetupFinally => Handler { target: i.target, depth: d as u32, lasti: false },
                            IOp::SetupCleanup => Handler { target: i.target, depth: d as u32, lasti: true },
                            IOp::SetupYield => Handler { target: i.target, depth: (d - 1).max(0) as u32, lasti: false },
                            _ => Handler { target: i.target, depth: (d - 1).max(0) as u32, lasti: true },
                        };
                        stack.push(h);
                        hs.push(None);
                    }
                    IOp::PopBlock => {
                        stack.pop();
                        hs.push(None);
                    }
                    _ => {
                        hs.push(stack.last().copied());
                        if i.target != NO_LABEL && entry[i.target as usize].is_none() {
                            entry[i.target as usize] = Some(stack.clone());
                            work.push(i.target);
                        }
                    }
                }
            }
            if falls_through(blk) {
                if let Some(n) = next_of(b) {
                    if entry[n as usize].is_none() {
                        entry[n as usize] = Some(stack.clone());
                        work.push(n);
                    }
                }
            }
            handlers[b as usize] = hs;
        }
    }

    // ---- flatten (drop pseudo ops)
    struct Flat {
        op: Op,
        arg: u32,
        target: Label,
        loc: Location,
        handler: Option<Handler>,
        flags: u8,
    }
    let mut flat: Vec<Flat> = Vec::new();
    let mut block_start = vec![u32::MAX; nblocks];
    let mut flat_block_start: Vec<(Label, usize)> = Vec::new();
    for &b in &order {
        flat_block_start.push((b, flat.len()));
        let blk = &ir.blocks[b as usize];
        let hs = &handlers[b as usize];
        for (k, i) in blk.instrs.iter().enumerate() {
            if let IOp::Op(op) = i.op {
                flat.push(Flat { op, arg: i.arg, target: i.target, loc: i.loc, handler: hs.get(k).copied().flatten(), flags: i.flags });
            }
        }
    }

    // ---- definite assignment: LoadFastCheck -> LoadFast where proven bound
    {
        let nfast = nlocalsplus.min(64);
        if nfast > 0 {
            let mut fblock_of: Vec<(Label, usize, usize)> = Vec::new(); // (label, start, end)
            for (idx, &(b, s)) in flat_block_start.iter().enumerate() {
                let e = if idx + 1 < flat_block_start.len() { flat_block_start[idx + 1].1 } else { flat.len() };
                fblock_of.push((b, s, e));
            }
            let mut label_pos = vec![usize::MAX; nblocks];
            for (idx, &(b, _, _)) in fblock_of.iter().enumerate() {
                label_pos[b as usize] = idx;
            }
            let all: u64 = if nfast == 64 { u64::MAX } else { (1u64 << nfast) - 1 };
            let param_mask: u64 = if info.nparams >= 64 { u64::MAX } else { (1u64 << info.nparams) - 1 };
            // cells/free slots are always "bound" (they hold cells)
            let mut cellfree_mask: u64 = 0;
            for (ci, c) in info.cell_arg.iter().enumerate() {
                let slot = match c {
                    Some(v) => *v,
                    None => cell_index[ci],
                };
                if slot < 64 {
                    cellfree_mask |= 1 << slot;
                }
            }
            for s in free_base..nlocalsplus.min(64) {
                cellfree_mask |= 1 << s;
            }
            let init = all & !param_mask & !cellfree_mask;
            let nb = fblock_of.len();
            let mut in_state: Vec<Option<u64>> = vec![None; nb];
            in_state[0] = Some(init);
            let mut work: Vec<usize> = vec![0];
            let mut inq = vec![false; nb];
            inq[0] = true;
            let merge = |in_state: &mut Vec<Option<u64>>, work: &mut Vec<usize>, inq: &mut Vec<bool>, t: usize, s: u64| {
                let new = match in_state[t] {
                    None => s,
                    Some(o) => o | s,
                };
                if in_state[t] != Some(new) {
                    in_state[t] = Some(new);
                    if !inq[t] {
                        inq[t] = true;
                        work.push(t);
                    }
                }
            };
            while let Some(bi) = work.pop() {
                inq[bi] = false;
                let (_, s, e) = fblock_of[bi];
                let mut st = in_state[bi].unwrap();
                let mut terminal = false;
                for f in &flat[s..e] {
                    if let Some(h) = f.handler {
                        let t = label_pos[h.target as usize];
                        merge(&mut in_state, &mut work, &mut inq, t, st);
                    }
                    let a = f.arg;
                    match f.op {
                        Op::StoreFast if a < 64 => {
                            if f.flags & F_MAY_STORE_NULL != 0 {
                                st |= 1 << a;
                            } else {
                                st &= !(1 << a);
                            }
                        }
                        Op::DeleteFast | Op::LoadFastAndClear if a < 64 => st |= 1 << a,
                        Op::MakeCell if a < 64 => st &= !(1 << a),
                        _ => {}
                    }
                    if let Some(h) = f.handler {
                        let t = label_pos[h.target as usize];
                        merge(&mut in_state, &mut work, &mut inq, t, st);
                    }
                    if f.target != NO_LABEL {
                        let t = label_pos[f.target as usize];
                        merge(&mut in_state, &mut work, &mut inq, t, st);
                    }
                    if matches!(f.op, Op::Jump | Op::ReturnValue | Op::RaiseVarargs | Op::Reraise) {
                        terminal = true;
                    }
                }
                if !terminal && bi + 1 < nb {
                    merge(&mut in_state, &mut work, &mut inq, bi + 1, st);
                }
            }
            // rewrite
            for bi in 0..nb {
                let (_, s, e) = fblock_of[bi];
                let mut st = match in_state[bi] {
                    Some(x) => x,
                    None => continue,
                };
                for f in flat[s..e].iter_mut() {
                    let a = f.arg;
                    match f.op {
                        Op::LoadFastCheck if a < 64 => {
                            if st & (1 << a) == 0 {
                                f.op = Op::LoadFast;
                            } else {
                                // after a successful check the variable is bound
                                st &= !(1 << a);
                            }
                        }
                        Op::StoreFast if a < 64 => {
                            if f.flags & F_MAY_STORE_NULL != 0 {
                                st |= 1 << a;
                            } else {
                                st &= !(1 << a);
                            }
                        }
                        Op::DeleteFast | Op::LoadFastAndClear if a < 64 => st |= 1 << a,
                        Op::MakeCell if a < 64 => st &= !(1 << a),
                        _ => {}
                    }
                }
            }
        }
    }

    // ---- drop unused constants (keep consts[0] when it is the docstring)
    let consts = {
        let mut used = vec![false; info.consts.len()];
        if info.flags & co::HAS_DOCSTRING != 0 && !used.is_empty() {
            used[0] = true;
        }
        for f in &flat {
            if f.op == Op::LoadConst {
                if let Some(u) = used.get_mut(f.arg as usize) {
                    *u = true;
                }
            }
        }
        let mut remap = vec![u32::MAX; info.consts.len()];
        let mut out = Vec::with_capacity(info.consts.len());
        for (i, c) in info.consts.iter().enumerate() {
            if used[i] {
                remap[i] = out.len() as u32;
                out.push(c.clone());
            }
        }
        for f in flat.iter_mut() {
            if f.op == Op::LoadConst {
                if let Some(&r) = remap.get(f.arg as usize) {
                    f.arg = r;
                }
            }
        }
        out
    };

    // ---- compute instruction indices with ExtendedArg prefixes
    for (b, s) in &flat_block_start {
        block_start[*b as usize] = *s as u32; // provisional (flat index)
    }
    let flat_index_of_label = block_start.clone();
    let n = flat.len();
    let mut size: Vec<u32> = flat.iter().map(|f| if f.target == NO_LABEL { arg_size(f.arg) } else { 1 }).collect();
    let mut offset: Vec<u32> = vec![0; n + 1];
    loop {
        let mut o = 0u32;
        for k in 0..n {
            offset[k] = o;
            o += size[k];
        }
        offset[n] = o;
        let mut changed = false;
        for k in 0..n {
            if flat[k].target != NO_LABEL {
                let t = flat_index_of_label[flat[k].target as usize];
                let toff = offset[t as usize];
                let s = arg_size(toff);
                if s != size[k] {
                    size[k] = s;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    let total = offset[n] as usize;
    let mut code = Vec::with_capacity(total);
    let mut locations = Vec::with_capacity(total);
    let mut inst_handler: Vec<Option<Handler>> = Vec::with_capacity(total);
    for (k, f) in flat.iter().enumerate() {
        let arg = if f.target != NO_LABEL { offset[flat_index_of_label[f.target as usize] as usize] } else { f.arg };
        let mut loc = f.loc;
        if loc.lineno == 0 {
            loc = Location { lineno: 0, end_lineno: 0, col: u32::MAX, end_col: u32::MAX };
        }
        if size[k] > 1 {
            code.push(encode(Op::ExtendedArg, arg >> 24));
            locations.push(loc);
            inst_handler.push(f.handler);
        }
        code.push(encode(f.op, arg & 0xFF_FFFF));
        locations.push(loc);
        inst_handler.push(f.handler);
    }

    // ---- exception table: runs of equal handlers
    let mut table = Vec::new();
    let mut k = 0;
    while k < total {
        match inst_handler[k] {
            None => k += 1,
            Some(h) => {
                let start = k;
                while k < total && inst_handler[k] == Some(h) {
                    k += 1;
                }
                table.push(ExceptionEntry {
                    start: start as u32,
                    end: k as u32,
                    target: offset[flat_index_of_label[h.target as usize] as usize],
                    depth: h.depth,
                    lasti: h.lasti,
                });
            }
        }
    }

    // ---- localsplus
    let mut names = info.varnames.clone();
    let mut kinds = info.varkinds.clone();
    for (ci, c) in info.cell_arg.iter().enumerate() {
        match c {
            Some(v) => kinds[*v as usize] |= fast::CELL,
            None => {
                names.push(info.cellvars[ci].clone());
                kinds.push(info.cellkinds[ci]);
            }
        }
    }
    for f in &info.freevars {
        names.push(f.clone());
        kinds.push(fast::FREE);
    }
    let mut flags = info.flags;
    if info.freevars.is_empty() && info.cellvars.is_empty() {
        flags |= co::NOFREE;
    }
    Ok(CodeUnit {
        name: info.name,
        qualname: info.qualname,
        filename: info.filename,
        firstlineno: info.firstlineno,
        argcount: info.argcount,
        posonlyargcount: info.posonlyargcount,
        kwonlyargcount: info.kwonlyargcount,
        flags,
        stacksize: maxdepth as u32,
        code,
        locations,
        consts,
        names: info.names,
        localsplusnames: names,
        localspluskinds: kinds,
        exception_table: table,
    })
}

fn arg_size(arg: u32) -> u32 {
    if arg >= (1 << 24) {
        2
    } else {
        1
    }
}

fn is_slot_op(op: IOp) -> bool {
    matches!(
        op,
        IOp::Op(Op::LoadDeref)
            | IOp::Op(Op::StoreDeref)
            | IOp::Op(Op::DeleteDeref)
            | IOp::Op(Op::LoadFromDictOrDeref)
            | IOp::Op(Op::MakeCell)
            | IOp::Op(Op::LoadFast)
            | IOp::Op(Op::LoadFastAndClear)
            | IOp::Op(Op::StoreFast)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> UnitInfo {
        UnitInfo {
            name: "t".into(),
            qualname: "t".into(),
            filename: "f".into(),
            firstlineno: 1,
            argcount: 0,
            posonlyargcount: 0,
            kwonlyargcount: 0,
            flags: 0,
            consts: vec![],
            names: vec![],
            varnames: vec![],
            varkinds: vec![],
            cellvars: vec![],
            cellkinds: vec![],
            cell_arg: vec![],
            freevars: vec![],
            nparams: 0,
        }
    }

    fn ins(op: Op, arg: u32, target: Label) -> Instr {
        Instr { op: IOp::Op(op), arg, target, loc: Location { lineno: 1, end_lineno: 1, col: 0, end_col: 1 }, flags: 0 }
    }

    #[test]
    fn extended_arg_and_jumps() {
        let mut ir = Ir::new();
        let l = ir.new_label();
        ir.emit(ins(Op::LoadConst, 1 << 25, NO_LABEL));
        ir.emit(ins(Op::PopJumpIfFalse, 0, l));
        ir.emit(ins(Op::LoadConst, 0, NO_LABEL));
        ir.emit(ins(Op::ReturnValue, 0, NO_LABEL));
        ir.use_label(l);
        ir.emit(ins(Op::LoadConst, 0, NO_LABEL));
        ir.emit(ins(Op::ReturnValue, 0, NO_LABEL));
        let c = assemble(ir, info()).unwrap();
        assert_eq!(decode(c.code[0]), (Op::ExtendedArg, 2));
        assert_eq!(decode(c.code[1]), (Op::LoadConst, 0));
        assert_eq!(decode(c.code[2]), (Op::PopJumpIfFalse, 5));
        assert_eq!(c.locations.len(), c.code.len());
        assert_eq!(c.stacksize, 1);
    }

    #[test]
    fn handler_depth_and_dead_code() {
        let mut ir = Ir::new();
        let h = ir.new_label();
        ir.emit(ins(Op::LoadConst, 0, NO_LABEL));
        ir.emit(Instr { op: IOp::SetupCleanup, arg: 0, target: h, loc: NO_LOC_T, flags: 0 });
        ir.emit(ins(Op::LoadConst, 0, NO_LABEL));
        ir.emit(ins(Op::ReturnValue, 0, NO_LABEL));
        ir.emit(ins(Op::Nop, 0, NO_LABEL)); // dead
        ir.use_label(h);
        ir.emit(ins(Op::Reraise, 1, NO_LABEL));
        let c = assemble(ir, info()).unwrap();
        assert_eq!(c.exception_table.len(), 1);
        let e = c.exception_table[0];
        assert_eq!((e.start, e.end, e.target, e.depth, e.lasti), (1, 3, 3, 1, true));
        assert_eq!(c.stacksize, 3);
        assert_eq!(c.code.len(), 4);
    }

    const NO_LOC_T: Location = Location { lineno: 0, end_lineno: 0, col: u32::MAX, end_col: u32::MAX };
}

/// CPython's `duplicate_exits_without_lineno`: a small exit block (ending in ReturnValue)
/// without any location that is reached by jumps gets one copy per jumping predecessor,
/// carrying the location of the jump, so that the implicit `return None` reports the line of
/// the code that reached it (matters for sys.settrace 'line'/'return' events).
fn duplicate_exits_without_lineno(ir: &mut Ir, order: &mut Vec<Label>) {
    let is_exit = |b: &Block| -> bool {
        !b.instrs.is_empty()
            && b.instrs.len() <= 4
            && b.instrs.iter().all(|i| i.loc.lineno == 0)
            && matches!(b.instrs.last().map(|i| i.op), Some(IOp::Op(Op::ReturnValue)))
    };
    let n = order.len();
    let mut pos = vec![usize::MAX; ir.blocks.len()];
    for (p, &b) in order.iter().enumerate() {
        pos[b as usize] = p;
    }
    // skip over empty blocks (they fall through)
    let orig_order: Vec<Label> = order.clone();
    let resolve = |ir: &Ir, mut t: usize| -> usize {
        let mut guard = 0;
        while ir.blocks[t].instrs.is_empty() && pos[t] != usize::MAX && pos[t] + 1 < n && guard < n {
            t = orig_order[pos[t] + 1] as usize;
            guard += 1;
        }
        t
    };
    for p in 0..n {
        let b = order[p] as usize;
        let mut last_loc: Option<Location> = None;
        for k in 0..ir.blocks[b].instrs.len() {
            let i = ir.blocks[b].instrs[k];
            if i.loc.lineno != 0 {
                last_loc = Some(i.loc);
            }
            if i.target == NO_LABEL || matches!(i.op, IOp::SetupFinally | IOp::SetupCleanup | IOp::SetupWith | IOp::SetupYield) {
                continue;
            }
            let t = resolve(ir, i.target as usize);
            if !is_exit(&ir.blocks[t]) {
                continue;
            }
            let loc = match last_loc {
                Some(l) => l,
                None => continue,
            };
            let mut copy = ir.blocks[t].clone();
            for ci in copy.instrs.iter_mut() {
                ci.loc = loc;
            }
            ir.blocks.push(copy);
            let nl = (ir.blocks.len() - 1) as Label;
            order.push(nl);
            ir.blocks[b].instrs[k].target = nl;
        }
    }
}

/// Blocks reachable from the entry (following jumps, setup targets and fallthrough), in order.
fn filter_reachable(ir: &Ir, order: Vec<Label>) -> Vec<Label> {
    let nblocks = ir.blocks.len();
    let mut pos = vec![usize::MAX; nblocks];
    for (p, &b) in order.iter().enumerate() {
        pos[b as usize] = p;
    }
    let mut reachable = vec![false; nblocks];
    let mut stack = vec![order[0]];
    reachable[order[0] as usize] = true;
    while let Some(b) = stack.pop() {
        let blk = &ir.blocks[b as usize];
        let mut falls = true;
        for i in &blk.instrs {
            if i.target != NO_LABEL && !reachable[i.target as usize] {
                reachable[i.target as usize] = true;
                stack.push(i.target);
            }
            if is_terminal(i.op) {
                falls = false;
                break;
            }
        }
        if falls {
            let p = pos[b as usize];
            if p + 1 < order.len() {
                let n = order[p + 1];
                if !reachable[n as usize] {
                    reachable[n as usize] = true;
                    stack.push(n);
                }
            }
        }
    }
    order.into_iter().filter(|&b| reachable[b as usize]).collect()
}

/// Retarget jumps whose target (after skipping empty blocks) is an unconditional `Jump`
/// without its own line number (or on the same line).
fn thread_jumps(ir: &mut Ir, order: &[Label]) {
    let mut pos = vec![usize::MAX; ir.blocks.len()];
    for (p, &b) in order.iter().enumerate() {
        pos[b as usize] = p;
    }
    let first_real = |ir: &Ir, mut t: usize| -> Option<(usize, Instr)> {
        for _ in 0..order.len() {
            if let Some(i) = ir.blocks[t].instrs.first() {
                return Some((t, *i));
            }
            let p = pos[t];
            if p == usize::MAX || p + 1 >= order.len() {
                return None;
            }
            t = order[p + 1] as usize;
        }
        None
    };
    for &b in order {
        for k in 0..ir.blocks[b as usize].instrs.len() {
            let i = ir.blocks[b as usize].instrs[k];
            let jumpy = matches!(i.op, IOp::Op(op) if op.is_jump() && !matches!(op, Op::ForIter | Op::Send));
            if !jumpy || i.target == NO_LABEL {
                continue;
            }
            // Only the unconditional Jump may go backwards: the interpreter and
            // the JIT poll the eval breaker (signals, GC, stop-the-world) on
            // backward Jumps, so a conditional jump must keep jumping forward to
            // the Jump that closes a loop.
            let conditional = i.op != IOp::Op(Op::Jump);
            let mut t = i.target;
            for _ in 0..8 {
                match first_real(ir, t as usize) {
                    Some((_, j))
                        if j.op == IOp::Op(Op::Jump)
                            && j.target != t
                            && (j.loc.lineno == 0 || j.loc.lineno == i.loc.lineno)
                            && !(conditional && pos[j.target as usize] <= pos[b as usize]) =>
                    {
                        t = j.target;
                    }
                    _ => break,
                }
            }
            ir.blocks[b as usize].instrs[k].target = t;
        }
    }
}
