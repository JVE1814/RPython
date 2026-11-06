//! CFG optimisation and assembly of the wordcode pipeline (D24).
//!
//! Input: the IR of one code unit as produced by code generation (blocks of CPython-numbered
//! instructions with pseudo-instructions and symbolic labels). Output: a [`CodeObject`].
//!
//! Passes, in order:
//! 1. block splitting, exception-handler labelling (each instruction learns its innermost
//!    handler; RESUMEs after yields at handler depth 1 get the DEPTH1 flag),
//! 2. unreachable-code removal, inlining of small exit blocks into jumps, jump threading,
//! 3. constant folding and peephole rewrites (iterated), NOP removal,
//! 4. constant-table compaction, LOAD_FAST_CHECK for possibly-unbound locals,
//!    LOAD_FAST_BORROW for loads whose value is consumed within its basic block,
//!    super-instructions,
//! 5. cold (exception-only) blocks moved to the end, line-number propagation,
//! 6. jump direction normalisation, NOT_TAKEN insertion, stack depth, encoding with
//!    EXTENDED_ARG and inline caches, exception table and location table.

use super::ir::*;
use crate::ast;
use crate::bytecode::fast;
use crate::error::{CompileError, ErrorKind};
use crate::wordcode::{self, common, op as O, CodeObject, Constant, ExcEntry, Position};

/// Everything about a unit except its instructions.
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
    /// fast locals (parameters first) and their kinds
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

type R<T> = Result<T, CompileError>;

fn ierr(msg: &str) -> CompileError {
    CompileError::other(ErrorKind::SystemError, format!("internal compiler error: {}", msg))
}

/// Copy limit for inlining exit blocks into the jumps that reach them.
const MAX_COPY_SIZE: usize = 4;

pub(crate) struct Cfg {
    pub blocks: Vec<Block>,
    pub order: Vec<Label>,
}

/// Instructions with a control-flow edge to their target (SETUP_* edges are handler edges;
/// END_ASYNC_FOR's target only feeds its oparg).
fn is_jump_edge(op: u16) -> bool {
    has_target(op) && !is_setup(op) && op != O::END_ASYNC_FOR
}

fn is_exit_op(op: u16) -> bool {
    matches!(op, O::RETURN_VALUE | O::RAISE_VARARGS | O::RERAISE)
}

fn is_pseudo_marker(op: u16) -> bool {
    is_setup(op) || op == O::POP_BLOCK || op == O::ANNOTATIONS_PLACEHOLDER
}

impl Cfg {
    pub fn from_ir(ir: Ir) -> Cfg {
        let mut seen = vec![false; ir.blocks.len()];
        let mut order = Vec::with_capacity(ir.order.len());
        for &l in &ir.order {
            if !seen[l as usize] {
                seen[l as usize] = true;
                order.push(l);
            }
        }
        let mut cfg = Cfg { blocks: ir.blocks, order };
        cfg.split_blocks();
        cfg
    }

    fn new_block(&mut self) -> Label {
        self.blocks.push(Block { depth: -1, ..Default::default() });
        (self.blocks.len() - 1) as Label
    }

    /// Basic blocks end after every jump; code after a terminator in the same block is dead.
    fn split_blocks(&mut self) {
        let old = std::mem::take(&mut self.order);
        let mut order = Vec::with_capacity(old.len());
        for b in old {
            let instrs = std::mem::take(&mut self.blocks[b as usize].instrs);
            let mut cur = b;
            let mut acc: Vec<Instr> = Vec::new();
            order.push(b);
            let n = instrs.len();
            for (k, ins) in instrs.into_iter().enumerate() {
                acc.push(ins);
                if is_terminator(ins.op) {
                    break;
                }
                if is_jump_edge(ins.op) && k + 1 < n {
                    self.blocks[cur as usize].instrs = std::mem::take(&mut acc);
                    cur = self.new_block();
                    order.push(cur);
                }
            }
            self.blocks[cur as usize].instrs = acc;
        }
        self.order = order;
    }

    fn positions(&self) -> Vec<usize> {
        let mut p = vec![usize::MAX; self.blocks.len()];
        for (i, &b) in self.order.iter().enumerate() {
            p[b as usize] = i;
        }
        p
    }

    fn falls_through(&self, b: Label) -> bool {
        self.blocks[b as usize].instrs.last().map(|i| !is_terminator(i.op)).unwrap_or(true)
    }

    fn next_in_order(&self, pos: &[usize], b: Label) -> Option<Label> {
        let p = pos[b as usize];
        self.order.get(p + 1).copied()
    }

    /// Successor blocks through normal control flow (and SETUP_* edges if `handlers`).
    fn successors(&self, pos: &[usize], b: Label, handlers: bool) -> Vec<Label> {
        let mut out = Vec::new();
        for i in &self.blocks[b as usize].instrs {
            if is_jump_edge(i.op) || (handlers && is_setup(i.op)) {
                out.push(i.target);
            }
        }
        if self.falls_through(b) {
            if let Some(n) = self.next_in_order(pos, b) {
                out.push(n);
            }
        }
        out
    }

    fn mark_reachable(&self, handlers: bool) -> Vec<bool> {
        let pos = self.positions();
        let mut seen = vec![false; self.blocks.len()];
        let Some(&entry) = self.order.first() else { return seen };
        let mut work = vec![entry];
        seen[entry as usize] = true;
        while let Some(b) = work.pop() {
            for s in self.successors(&pos, b, handlers) {
                if pos[s as usize] != usize::MAX && !seen[s as usize] {
                    seen[s as usize] = true;
                    work.push(s);
                }
            }
        }
        seen
    }

    fn remove_unreachable(&mut self) {
        let seen = self.mark_reachable(true);
        self.order.retain(|b| seen[*b as usize]);
    }

    /// Number of normal-flow predecessors of each block.
    fn predecessor_counts(&self) -> Vec<u32> {
        let pos = self.positions();
        let mut n = vec![0u32; self.blocks.len()];
        for &b in &self.order {
            for s in self.successors(&pos, b, false) {
                n[s as usize] += 1;
            }
        }
        n
    }

    /// First real instruction of a block (skipping pseudo markers), following empty
    /// fall-through blocks.
    fn first_real(&self, pos: &[usize], mut b: Label) -> Option<(Label, usize)> {
        for _ in 0..self.order.len() + 1 {
            let blk = &self.blocks[b as usize];
            if let Some(k) = blk.instrs.iter().position(|i| !is_pseudo_marker(i.op)) {
                return Some((b, k));
            }
            if !self.falls_through(b) {
                return None;
            }
            b = self.next_in_order(pos, b)?;
        }
        None
    }
}

// ============================================================================ entry point

pub(crate) fn assemble_unit(ir: Ir, info: UnitInfo, _codegen_only: bool) -> R<CodeObject> {
    let mut info = info;
    let mut cfg = Cfg::from_ir(ir);
    let layout = Layout::new(&info);
    layout.resolve_slots(&mut cfg);
    let nlocalsplus = layout.nlocalsplus;
    let bound = layout.bound_at_entry(&info);
    optimize_cfg(&mut cfg, &mut info.consts, nlocalsplus, &bound)?;
    finish(cfg, info, layout)
}

/// The localsplus layout: varnames, then cells that do not share an argument's slot, then
/// free variables.
pub(crate) struct Layout {
    cell_index: Vec<u32>,
    free_base: u32,
    pub nlocalsplus: usize,
}

impl Layout {
    fn new(info: &UnitInfo) -> Layout {
        let nvars = info.varnames.len() as u32;
        let mut cell_index = Vec::with_capacity(info.cellvars.len());
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
        Layout { cell_index, free_base: next, nlocalsplus: (next as usize) + info.freevars.len() }
    }

    fn slot(&self, arg: i32) -> i32 {
        if arg & FREE_TAG != 0 {
            self.free_base as i32 + (arg & !FREE_TAG)
        } else if arg & CELL_TAG != 0 {
            self.cell_index[(arg & !CELL_TAG) as usize] as i32
        } else {
            arg
        }
    }

    fn resolve_slots(&self, cfg: &mut Cfg) {
        for b in cfg.blocks.iter_mut() {
            for i in b.instrs.iter_mut() {
                if wordcode::has_local(i.op) || wordcode::has_free(i.op) {
                    i.arg = self.slot(i.arg);
                }
            }
        }
    }

    /// Slots bound when the frame starts: parameters and cell / free slots.
    fn bound_at_entry(&self, info: &UnitInfo) -> Vec<bool> {
        let mut v = vec![false; self.nlocalsplus];
        for x in v.iter_mut().take(info.nparams as usize) {
            *x = true;
        }
        for &c in &self.cell_index {
            v[c as usize] = true;
        }
        for x in v.iter_mut().skip(self.free_base as usize) {
            *x = true;
        }
        v
    }
}

/// The optimisation passes (also used by the `optimize_cfg` test hook).
pub(crate) fn optimize_cfg(cfg: &mut Cfg, consts: &mut Vec<Constant>, nlocals: usize, bound: &[bool]) -> R<()> {
    if cfg.order.is_empty() {
        return Ok(());
    }
    skip_empty_targets(cfg);
    mark_handlers(cfg);
    label_exception_targets(cfg);
    cfg.remove_unreachable();
    inline_small_exit_blocks(cfg);
    cfg.remove_unreachable();
    propagate_line_numbers(cfg);
    for _ in 0..4 {
        let mut changed = false;
        for k in 0..cfg.order.len() {
            let b = cfg.order[k];
            changed |= fold_constants(cfg, b, consts);
            changed |= peephole_block(cfg, b, consts);
        }
        changed |= thread_jumps(cfg);
        remove_redundant_nops(cfg);
        changed |= remove_redundant_jumps(cfg);
        cfg.remove_unreachable();
        if !changed {
            break;
        }
    }
    inline_small_exit_blocks(cfg);
    cfg.remove_unreachable();
    convert_constant_loads(cfg, consts);
    remove_unused_consts(cfg, consts);
    add_checks_for_unbound_locals(cfg, nlocals, bound);
    convert_closure_loads(cfg);
    optimize_load_fast(cfg);
    insert_superinstructions(cfg);
    remove_redundant_nops(cfg);
    Ok(())
}

/// Jumps (and SETUP_* targets) to empty blocks are retargeted to the first non-empty block
/// reached by falling through.
fn skip_empty_targets(cfg: &mut Cfg) {
    let pos = cfg.positions();
    let resolve = |cfg: &Cfg, mut t: Label| -> Label {
        for _ in 0..cfg.order.len() + 1 {
            if !cfg.blocks[t as usize].instrs.is_empty() || pos[t as usize] == usize::MAX {
                return t;
            }
            match cfg.next_in_order(&pos, t) {
                Some(n) => t = n,
                None => return t,
            }
        }
        t
    };
    for k in 0..cfg.order.len() {
        let b = cfg.order[k];
        for j in 0..cfg.blocks[b as usize].instrs.len() {
            let i = cfg.blocks[b as usize].instrs[j];
            if has_target(i.op) && i.target != NO_LABEL {
                let t = resolve(cfg, i.target);
                cfg.blocks[b as usize].instrs[j].target = t;
            }
        }
    }
}

// ============================================================================ handlers

fn mark_handlers(cfg: &mut Cfg) {
    let mut marks: Vec<(Label, bool)> = Vec::new();
    for &b in &cfg.order {
        for i in &cfg.blocks[b as usize].instrs {
            if is_setup(i.op) {
                marks.push((i.target, i.op != O::SETUP_FINALLY));
            }
        }
    }
    for (t, lasti) in marks {
        cfg.blocks[t as usize].is_handler = true;
        cfg.blocks[t as usize].preserve_lasti = lasti;
    }
}

/// Assign every instruction its innermost exception handler by walking the CFG with a stack
/// of active handlers (SETUP_* pushes, POP_BLOCK pops). Also sets the DEPTH1 flag of the
/// RESUME following a YIELD_VALUE executed with exactly one active handler.
fn label_exception_targets(cfg: &mut Cfg) {
    let pos = cfg.positions();
    let n = cfg.blocks.len();
    let mut entry: Vec<Option<Vec<Label>>> = vec![None; n];
    let start = cfg.order[0];
    entry[start as usize] = Some(Vec::new());
    let mut work = vec![start];
    while let Some(b) = work.pop() {
        let mut stack = entry[b as usize].clone().unwrap_or_default();
        let mut pending: Vec<(Label, Vec<Label>)> = Vec::new();
        let mut yield_depth: Option<usize> = None;
        let len = cfg.blocks[b as usize].instrs.len();
        for k in 0..len {
            let ins = cfg.blocks[b as usize].instrs[k];
            let top = stack.last().copied().unwrap_or(NO_LABEL);
            let mut handler = top;
            if is_setup(ins.op) {
                pending.push((ins.target, stack.clone()));
                stack.push(ins.target);
            } else if ins.op == O::POP_BLOCK {
                stack.pop();
                handler = stack.last().copied().unwrap_or(NO_LABEL);
            } else {
                if ins.op == O::YIELD_VALUE {
                    yield_depth = Some(stack.len());
                } else if ins.op == O::RESUME {
                    if yield_depth == Some(1) && (1..=3).contains(&ins.arg) {
                        cfg.blocks[b as usize].instrs[k].arg |= wordcode::resume::DEPTH1 as i32;
                    }
                    yield_depth = None;
                }
                if is_jump_edge(ins.op) {
                    pending.push((ins.target, stack.clone()));
                }
            }
            cfg.blocks[b as usize].instrs[k].handler = handler;
        }
        if cfg.falls_through(b) {
            if let Some(nb) = cfg.next_in_order(&pos, b) {
                pending.push((nb, stack.clone()));
            }
        }
        for (t, st) in pending {
            if entry[t as usize].is_none() {
                entry[t as usize] = Some(st);
                work.push(t);
            }
        }
    }
}

// ============================================================================ block-level CFG cleanups

fn block_is_small_exit(cfg: &Cfg, b: Label) -> bool {
    let instrs = &cfg.blocks[b as usize].instrs;
    let real = instrs.iter().filter(|i| !is_pseudo_marker(i.op)).count();
    match instrs.last() {
        Some(last) => is_exit_op(last.op) && real <= MAX_COPY_SIZE && !instrs.iter().any(|i| is_pseudo_marker(i.op)),
        None => false,
    }
}

fn block_has_no_lineno(cfg: &Cfg, b: Label) -> bool {
    cfg.blocks[b as usize].instrs.iter().all(|i| !i.loc.is_some())
}

/// A block ending in an unconditional jump to a small exit block (or a small block without
/// line numbers ending in a jump) gets a copy of the target's instructions instead.
fn inline_small_exit_blocks(cfg: &mut Cfg) {
    loop {
        let mut changed = false;
        for k in 0..cfg.order.len() {
            let b = cfg.order[k];
            let Some(last) = cfg.blocks[b as usize].instrs.last().copied() else { continue };
            if !is_uncond_jump(last.op) {
                continue;
            }
            let t = last.target;
            if t == b {
                continue;
            }
            let tinstrs = &cfg.blocks[t as usize].instrs;
            let small_nolineno = block_has_no_lineno(cfg, t)
                && tinstrs.len() <= MAX_COPY_SIZE
                && tinstrs.last().map(|i| is_terminator(i.op)).unwrap_or(false)
                && !tinstrs.iter().any(|i| is_pseudo_marker(i.op));
            if !(block_is_small_exit(cfg, t) || small_nolineno) {
                continue;
            }
            let mut copy = cfg.blocks[t as usize].instrs.clone();
            for c in copy.iter_mut() {
                c.handler = last.handler;
            }
            let blk = &mut cfg.blocks[b as usize];
            blk.instrs.pop();
            if last.loc.is_some() {
                // keep the jump's line event
                blk.instrs.push(Instr { op: O::NOP, arg: 0, target: NO_LABEL, loc: last.loc, handler: last.handler });
            }
            blk.instrs.extend(copy);
            changed = true;
        }
        if !changed {
            break;
        }
    }
}

/// Retarget jumps whose target starts with an unconditional jump.
fn thread_jumps(cfg: &mut Cfg) -> bool {
    let pos = cfg.positions();
    let mut changed = false;
    for k in 0..cfg.order.len() {
        let b = cfg.order[k];
        let Some(idx) = cfg.blocks[b as usize].instrs.len().checked_sub(1) else { continue };
        let ins = cfg.blocks[b as usize].instrs[idx];
        if !(is_uncond_jump(ins.op) || is_cond_jump(ins.op)) {
            continue;
        }
        let mut target = ins.target;
        for _ in 0..8 {
            let Some((tb, ti)) = cfg.first_real(&pos, target) else { break };
            let tins = cfg.blocks[tb as usize].instrs[ti];
            if !is_uncond_jump(tins.op) || tins.target == target {
                break;
            }
            // do not lose the line event of a jump that has its own line
            if tins.loc.is_some() && (!ins.loc.is_some() || tins.loc.line != ins.loc.line) {
                break;
            }
            target = tins.target;
        }
        if target != ins.target {
            cfg.blocks[b as usize].instrs[idx].target = target;
            changed = true;
        }
    }
    changed
}

/// Jumps to the block that follows anyway become NOPs (keeping their line).
fn remove_redundant_jumps(cfg: &mut Cfg) -> bool {
    let mut changed = false;
    let pos = cfg.positions();
    for k in 0..cfg.order.len() {
        let b = cfg.order[k];
        let next = cfg.order.get(k + 1).copied();
        let Some(last) = cfg.blocks[b as usize].instrs.last().copied() else { continue };
        if is_uncond_jump(last.op) {
            // the target may be an empty block chain ending at `next`
            let mut t = last.target;
            let mut hops = 0;
            while cfg_block_is_empty_fallthrough(&cfg.blocks, t) && hops < 8 {
                let p = pos[t as usize];
                match cfg.order.get(p + 1) {
                    Some(&n) => t = n,
                    None => break,
                }
                hops += 1;
            }
            if Some(last.target) == next || Some(t) == next {
                let li = cfg.blocks[b as usize].instrs.last_mut().unwrap();
                li.op = O::NOP;
                li.target = NO_LABEL;
                changed = true;
            }
        }
    }
    changed
}

fn cfg_block_is_empty_fallthrough(blocks: &[Block], b: Label) -> bool {
    blocks[b as usize].instrs.iter().all(|i| is_pseudo_marker(i.op))
}

/// NOPs survive only when they carry the only instance of a line.
fn remove_redundant_nops(cfg: &mut Cfg) {
    let pos = cfg.positions();
    let preds = cfg.predecessor_counts();
    for k in 0..cfg.order.len() {
        let b = cfg.order[k];
        let instrs = std::mem::take(&mut cfg.blocks[b as usize].instrs);
        let mut out: Vec<Instr> = Vec::with_capacity(instrs.len());
        let n = instrs.len();
        let mut i = 0;
        let mut pending_loc: Option<Pos> = None;
        while i < n {
            let mut ins = instrs[i];
            if let Some(l) = pending_loc.take() {
                if !ins.loc.is_some() && !is_pseudo_marker(ins.op) {
                    ins.loc = l;
                } else if is_pseudo_marker(ins.op) {
                    pending_loc = Some(l);
                }
            }
            if ins.op == O::NOP {
                if !ins.loc.is_some() {
                    i += 1;
                    continue;
                }
                let prev_line = out.iter().rev().find(|x| !is_pseudo_marker(x.op)).map(|x| x.loc.line);
                if prev_line == Some(ins.loc.line) {
                    i += 1;
                    continue;
                }
                // next real instruction in this block
                let next = instrs[i + 1..].iter().find(|x| !is_pseudo_marker(x.op));
                match next {
                    Some(nx) => {
                        if nx.loc.line == ins.loc.line && nx.loc.is_some() {
                            i += 1;
                            continue;
                        }
                        if !nx.loc.is_some() {
                            pending_loc = Some(ins.loc);
                            i += 1;
                            continue;
                        }
                    }
                    None => {
                        // last in block: drop if the fall-through successor (with this block
                        // as its only predecessor) starts on the same line
                        if out.iter().all(|x| is_pseudo_marker(x.op) || x.op == O::NOP) || true {
                            if let Some(nb) = cfg.order.get(k + 1).copied() {
                                if preds[nb as usize] == 1 {
                                    if let Some((fb, fi)) = cfg.first_real(&pos, nb) {
                                        if fb != b {
                                            let f = cfg.blocks[fb as usize].instrs[fi];
                                            if f.loc.is_some() && f.loc.line == ins.loc.line {
                                                i += 1;
                                                continue;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            out.push(ins);
            i += 1;
        }
        cfg.blocks[b as usize].instrs = out;
    }
}

/// Location-less instructions inherit the previous location in their block; a block whose
/// only (normal-flow) predecessor reaches it inherits that predecessor's last location.
pub(crate) fn propagate_line_numbers(cfg: &mut Cfg) {
    let pos = cfg.positions();
    let preds = cfg.predecessor_counts();
    // the location flowing out of each block
    for k in 0..cfg.order.len() {
        let b = cfg.order[k];
        let mut cur: Option<Pos> = None;
        for i in cfg.blocks[b as usize].instrs.iter_mut() {
            if i.loc.is_some() {
                cur = Some(i.loc);
            } else if let Some(l) = cur {
                if !is_pseudo_marker(i.op) {
                    i.loc = l;
                }
            }
        }
        let Some(out_loc) = cur else { continue };
        if cfg.blocks[b as usize].instrs.is_empty() {
            continue;
        }
        for s in cfg.successors(&pos, b, false) {
            if preds[s as usize] != 1 || cfg.blocks[s as usize].is_handler {
                continue;
            }
            if let Some(first) = cfg.blocks[s as usize].instrs.iter().find(|i| !is_pseudo_marker(i.op)) {
                if !first.loc.is_some() {
                    // set the leading location-less instructions now
                    for i in cfg.blocks[s as usize].instrs.iter_mut() {
                        if is_pseudo_marker(i.op) {
                            continue;
                        }
                        if i.loc.is_some() {
                            break;
                        }
                        i.loc = out_loc;
                    }
                }
            }
        }
    }
}

/// Exit blocks without a line number that are reached by several jumps get one copy per
/// jumping predecessor, so that each copy can carry its predecessor's line.
fn duplicate_exits_without_lineno(cfg: &mut Cfg) {
    let preds = cfg.predecessor_counts();
    let pos = cfg.positions();
    let order = cfg.order.clone();
    let mut new_blocks: Vec<(usize, Label)> = Vec::new();
    for (k, &b) in order.iter().enumerate() {
        let Some(last) = cfg.blocks[b as usize].instrs.last().copied() else { continue };
        if !is_jump_edge(last.op) {
            continue;
        }
        let t = last.target;
        if preds[t as usize] < 2 || !block_has_no_lineno(cfg, t) || cfg.blocks[t as usize].is_handler {
            continue;
        }
        let tl = cfg.blocks[t as usize].instrs.last().copied();
        if !tl.map(|i| is_exit_op(i.op)).unwrap_or(false) || cfg.blocks[t as usize].instrs.len() > MAX_COPY_SIZE {
            continue;
        }
        if cfg.blocks[t as usize].instrs.iter().any(|i| is_pseudo_marker(i.op)) {
            continue;
        }
        let _ = pos;
        let nb = cfg.new_block();
        let mut instrs = cfg.blocks[t as usize].instrs.clone();
        for i in instrs.iter_mut() {
            i.loc = last.loc;
            i.handler = last.handler;
        }
        cfg.blocks[nb as usize].instrs = instrs;
        let n = cfg.blocks[b as usize].instrs.len();
        cfg.blocks[b as usize].instrs[n - 1].target = nb;
        new_blocks.push((k, nb));
    }
    // copies go to the end (they are only reached by jumps)
    for (_, nb) in new_blocks {
        cfg.order.push(nb);
    }
}

// ============================================================================ constants

fn common_const_value(arg: i32) -> Option<Constant> {
    Some(match arg as u32 {
        common::NONE => Constant::None,
        common::EMPTY_STR => Constant::Str(Vec::new()),
        common::TRUE => Constant::Bool(true),
        common::FALSE => Constant::Bool(false),
        common::MINUS_ONE => Constant::Int(-1),
        _ => return None,
    })
}

fn const_of(i: &Instr, consts: &[Constant]) -> Option<Constant> {
    match i.op {
        O::LOAD_CONST => consts.get(i.arg as usize).cloned(),
        O::LOAD_SMALL_INT => Some(Constant::Int(i.arg as i64)),
        O::LOAD_COMMON_CONSTANT => common_const_value(i.arg),
        _ => None,
    }
}

fn same_const(a: &Constant, b: &Constant) -> bool {
    match (a, b) {
        (Constant::Float(x), Constant::Float(y)) => x.to_bits() == y.to_bits(),
        (Constant::Complex { real: a1, imag: b1 }, Constant::Complex { real: a2, imag: b2 }) => {
            a1.to_bits() == a2.to_bits() && b1.to_bits() == b2.to_bits()
        }
        (Constant::Tuple(x), Constant::Tuple(y)) | (Constant::FrozenSet(x), Constant::FrozenSet(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| same_const(p, q))
        }
        (Constant::Slice(x), Constant::Slice(y)) => (0..3).all(|k| same_const(&x[k], &y[k])),
        (Constant::Code(_), _) | (_, Constant::Code(_)) => false,
        _ => std::mem::discriminant(a) == std::mem::discriminant(b) && a == b,
    }
}

pub(crate) fn add_const(consts: &mut Vec<Constant>, c: Constant) -> u32 {
    if let Some(i) = consts.iter().position(|x| same_const(x, &c)) {
        return i as u32;
    }
    consts.push(c);
    (consts.len() - 1) as u32
}

fn to_ast(c: &Constant) -> Option<ast::Constant> {
    Some(match c {
        Constant::None => ast::Constant::None,
        Constant::Bool(b) => ast::Constant::Bool(*b),
        Constant::Ellipsis => ast::Constant::Ellipsis,
        Constant::Int(i) => ast::Constant::Int(ast::Int::Small(*i)),
        Constant::BigInt { neg, decimal } => {
            ast::Constant::Int(ast::Int::Big(if *neg { format!("-{}", decimal) } else { decimal.clone() }))
        }
        Constant::Float(f) => ast::Constant::Float(*f),
        Constant::Complex { real, imag } => ast::Constant::Complex { real: *real, imag: *imag },
        Constant::Str(s) => {
            let mut p = ast::PyStr::new();
            for &cp in s {
                p.push_code_point(cp);
            }
            ast::Constant::Str(p)
        }
        Constant::Bytes(b) => ast::Constant::Bytes(b.clone()),
        Constant::Tuple(v) => ast::Constant::Tuple(v.iter().map(to_ast).collect::<Option<Vec<_>>>()?),
        Constant::FrozenSet(v) => ast::Constant::FrozenSet(v.iter().map(to_ast).collect::<Option<Vec<_>>>()?),
        Constant::Slice(_) | Constant::Code(_) => return None,
    })
}

fn from_ast(c: &ast::Constant) -> Constant {
    super::conv_const(c)
}

fn const_truth(c: &Constant) -> Option<bool> {
    Some(match c {
        Constant::None => false,
        Constant::Bool(b) => *b,
        Constant::Ellipsis => true,
        Constant::Int(i) => *i != 0,
        Constant::BigInt { .. } => true,
        Constant::Float(f) => *f != 0.0,
        Constant::Complex { real, imag } => *real != 0.0 || *imag != 0.0,
        Constant::Str(s) => !s.is_empty(),
        Constant::Bytes(b) => !b.is_empty(),
        Constant::Tuple(v) | Constant::FrozenSet(v) => !v.is_empty(),
        Constant::Slice(_) | Constant::Code(_) => true,
    })
}

fn nb_operator(nb: i32) -> Option<ast::Operator> {
    use ast::Operator::*;
    Some(match nb {
        0 => Add,
        1 => BitAnd,
        2 => FloorDiv,
        3 => LShift,
        4 => MatMult,
        5 => Mult,
        6 => Mod,
        7 => BitOr,
        8 => Pow,
        9 => RShift,
        10 => Sub,
        11 => Div,
        12 => BitXor,
        _ => return None,
    })
}

fn fold_subscr(container: &Constant, index: &Constant) -> Option<Constant> {
    let i = match index {
        Constant::Int(i) => *i,
        Constant::Bool(b) => *b as i64,
        _ => return None,
    };
    let at = |len: usize| -> Option<usize> {
        let j = if i < 0 { i + len as i64 } else { i };
        if j >= 0 && (j as usize) < len {
            Some(j as usize)
        } else {
            None
        }
    };
    match container {
        Constant::Tuple(v) => Some(v[at(v.len())?].clone()),
        Constant::Str(s) => Some(Constant::Str(vec![s[at(s.len())?]])),
        Constant::Bytes(b) => Some(Constant::Int(b[at(b.len())?] as i64)),
        _ => None,
    }
}

fn fold_binary(nb: i32, a: &Constant, b: &Constant) -> Option<Constant> {
    if nb as u32 == wordcode::NB_SUBSCR {
        return fold_subscr(a, b);
    }
    let op = nb_operator(nb)?;
    let r = crate::fold::fold_binop(op, &to_ast(a)?, &to_ast(b)?)?;
    Some(from_ast(&r))
}

fn fold_unary(op: u16, arg: i32, c: &Constant) -> Option<Constant> {
    let u = match (op, arg as u32) {
        (O::UNARY_NEGATIVE, _) => ast::UnaryOp::USub,
        (O::UNARY_INVERT, _) => {
            if matches!(c, Constant::Bool(_)) {
                return None; // ~bool is deprecated: leave the warning to run time
            }
            ast::UnaryOp::Invert
        }
        (O::CALL_INTRINSIC_1, wordcode::intrinsic1::UNARY_POSITIVE) => ast::UnaryOp::UAdd,
        _ => return None,
    };
    let r = crate::fold::fold_unary(u, &to_ast(c)?)?;
    Some(from_ast(&r))
}

/// Indices of the `n` instructions producing the top `n` stack values just before index
/// `end` in `instrs`, if all are constant loads (NOPs in between are skipped).
fn const_operands(instrs: &[Instr], end: usize, n: usize, consts: &[Constant]) -> Option<Vec<(usize, Constant)>> {
    let mut out = Vec::with_capacity(n);
    let mut k = end;
    while out.len() < n {
        if k == 0 {
            return None;
        }
        k -= 1;
        let i = &instrs[k];
        if i.op == O::NOP {
            continue;
        }
        let c = const_of(i, consts)?;
        out.push((k, c));
    }
    out.reverse();
    Some(out)
}

fn set_load_const(ins: &mut Instr, c: Constant, consts: &mut Vec<Constant>) {
    let idx = add_const(consts, c);
    ins.op = O::LOAD_CONST;
    ins.arg = idx as i32;
    ins.target = NO_LABEL;
}

fn nop_out(ins: &mut Instr) {
    ins.op = O::NOP;
    ins.arg = 0;
    ins.target = NO_LABEL;
}

/// Size limit for folded tuples/sets (same order as CPython's).
const MAX_FOLD_COLLECTION: usize = 256;

/// Constant folding within one block. Returns whether anything changed.
fn fold_constants(cfg: &mut Cfg, b: Label, consts: &mut Vec<Constant>) -> bool {
    let mut changed = false;
    let instrs = &mut cfg.blocks[b as usize].instrs;
    let mut k = 0;
    while k < instrs.len() {
        let ins = instrs[k];
        match ins.op {
            O::BUILD_TUPLE => {
                let n = ins.arg as usize;
                if let Some(ops) = const_operands(instrs, k, n, consts) {
                    let items: Vec<Constant> = ops.iter().map(|(_, c)| c.clone()).collect();
                    for (j, _) in &ops {
                        nop_out(&mut instrs[*j]);
                    }
                    set_load_const(&mut instrs[k], Constant::Tuple(items), consts);
                    changed = true;
                }
            }
            O::BUILD_LIST | O::BUILD_SET => {
                let n = ins.arg as usize;
                let next = instrs[k + 1..].iter().find(|x| x.op != O::NOP).map(|x| (x.op, x.arg));
                let is_set = ins.op == O::BUILD_SET;
                // `x in [a, b]` / `for x in [a, b]`: an immutable constant will do
                let consumer = matches!(next, Some((O::CONTAINS_OP, _)) | Some((O::GET_ITER, _)));
                if n >= 1 && n <= MAX_FOLD_COLLECTION {
                    if let Some(ops) = const_operands(instrs, k, n, consts) {
                        let items: Vec<Constant> = ops.iter().map(|(_, c)| c.clone()).collect();
                        let value = if is_set { Constant::FrozenSet(dedup_set(items)) } else { Constant::Tuple(items) };
                        if consumer {
                            for (j, _) in &ops {
                                nop_out(&mut instrs[*j]);
                            }
                            set_load_const(&mut instrs[k], value, consts);
                            changed = true;
                        } else if n >= 3 {
                            // BUILD_LIST 0; LOAD_CONST tuple; LIST_EXTEND 1
                            let first = ops[0].0;
                            let second = ops[1].0;
                            for (j, _) in &ops[2..] {
                                nop_out(&mut instrs[*j]);
                            }
                            let loc0 = instrs[first].loc;
                            instrs[first] = Instr { op: ins.op, arg: 0, target: NO_LABEL, loc: loc0, handler: instrs[first].handler };
                            set_load_const(&mut instrs[second], value, consts);
                            instrs[k].op = if is_set { O::SET_UPDATE } else { O::LIST_EXTEND };
                            instrs[k].arg = 1;
                            changed = true;
                        }
                    }
                }
                if !changed && !is_set && matches!(next, Some((O::GET_ITER, _))) && n > 0 {
                    // iterating over a list display: a tuple is cheaper
                    instrs[k].op = O::BUILD_TUPLE;
                    changed = true;
                }
            }
            O::BINARY_OP => {
                if let Some(ops) = const_operands(instrs, k, 2, consts) {
                    if let Some(r) = fold_binary(ins.arg, &ops[0].1, &ops[1].1) {
                        nop_out(&mut instrs[ops[0].0]);
                        nop_out(&mut instrs[ops[1].0]);
                        set_load_const(&mut instrs[k], r, consts);
                        changed = true;
                    }
                }
            }
            O::UNARY_NEGATIVE | O::UNARY_INVERT | O::CALL_INTRINSIC_1 => {
                if let Some(ops) = const_operands(instrs, k, 1, consts) {
                    if ins.op == O::CALL_INTRINSIC_1 && ins.arg as u32 == wordcode::intrinsic1::LIST_TO_TUPLE {
                        // handled below (needs the list's construction)
                    } else if let Some(r) = fold_unary(ins.op, ins.arg, &ops[0].1) {
                        nop_out(&mut instrs[ops[0].0]);
                        set_load_const(&mut instrs[k], r, consts);
                        changed = true;
                    }
                }
                if ins.op == O::CALL_INTRINSIC_1 && ins.arg as u32 == wordcode::intrinsic1::LIST_TO_TUPLE {
                    if fold_list_to_tuple(instrs, k, consts) {
                        changed = true;
                    }
                }
            }
            O::TO_BOOL => {
                if let Some(ops) = const_operands(instrs, k, 1, consts) {
                    if let Some(t) = const_truth(&ops[0].1) {
                        nop_out(&mut instrs[ops[0].0]);
                        set_load_const(&mut instrs[k], Constant::Bool(t), consts);
                        changed = true;
                    }
                }
            }
            O::UNARY_NOT => {
                if let Some(ops) = const_operands(instrs, k, 1, consts) {
                    if let Constant::Bool(v) = ops[0].1 {
                        nop_out(&mut instrs[ops[0].0]);
                        set_load_const(&mut instrs[k], Constant::Bool(!v), consts);
                        changed = true;
                    }
                }
            }
            _ => {}
        }
        k += 1;
    }
    changed
}

fn dedup_set(items: Vec<Constant>) -> Vec<Constant> {
    let mut out: Vec<Constant> = Vec::new();
    for i in items {
        // set semantics: 1 == 1.0 == True; keep the first
        if !out.iter().any(|o| set_equal(o, &i)) {
            out.push(i);
        }
    }
    out
}

fn num_value(c: &Constant) -> Option<f64> {
    match c {
        Constant::Bool(b) => Some(*b as i64 as f64),
        Constant::Int(i) => Some(*i as f64),
        Constant::Float(f) => Some(*f),
        _ => None,
    }
}

fn set_equal(a: &Constant, b: &Constant) -> bool {
    if let (Some(x), Some(y)) = (num_value(a), num_value(b)) {
        return x == y;
    }
    a == b
}

/// `BUILD_LIST 0; (LOAD_CONST x; LIST_APPEND 1)*; CALL_INTRINSIC_1 LIST_TO_TUPLE` (also with a
/// LIST_EXTEND of a constant tuple) -> `LOAD_CONST tuple`.
fn fold_list_to_tuple(instrs: &mut [Instr], k: usize, consts: &mut Vec<Constant>) -> bool {
    let mut items: Vec<Constant> = Vec::new();
    let mut idxs: Vec<usize> = Vec::new();
    let mut j = k;
    // walk backwards over (const, LIST_APPEND/LIST_EXTEND 1) pairs to BUILD_LIST 0
    let mut rev: Vec<Constant> = Vec::new();
    loop {
        if j == 0 {
            return false;
        }
        j -= 1;
        let i = instrs[j];
        match i.op {
            O::NOP => {
                idxs.push(j);
            }
            O::BUILD_LIST if i.arg == 0 => {
                idxs.push(j);
                break;
            }
            O::LIST_APPEND | O::LIST_EXTEND if i.arg == 1 => {
                idxs.push(j);
                // the operand
                let mut m = j;
                loop {
                    if m == 0 {
                        return false;
                    }
                    m -= 1;
                    if instrs[m].op != O::NOP {
                        break;
                    }
                }
                let Some(c) = const_of(&instrs[m], consts) else { return false };
                if i.op == O::LIST_APPEND {
                    rev.push(c);
                } else {
                    match c {
                        Constant::Tuple(v) => {
                            for x in v.into_iter().rev() {
                                rev.push(x);
                            }
                        }
                        _ => return false,
                    }
                }
                idxs.push(m);
                j = m;
            }
            _ => return false,
        }
    }
    rev.reverse();
    items.extend(rev);
    for i in idxs {
        nop_out(&mut instrs[i]);
    }
    set_load_const(&mut instrs[k], Constant::Tuple(items), consts);
    true
}

/// LOAD_CONST of small ints and of the common constants become LOAD_SMALL_INT /
/// LOAD_COMMON_CONSTANT.
fn convert_constant_loads(cfg: &mut Cfg, consts: &[Constant]) {
    for &b in &cfg.order {
        for i in cfg.blocks[b as usize].instrs.iter_mut() {
            if i.op != O::LOAD_CONST {
                continue;
            }
            match consts.get(i.arg as usize) {
                Some(Constant::Int(v)) if (0..256).contains(v) => {
                    i.op = O::LOAD_SMALL_INT;
                    i.arg = *v as i32;
                }
                Some(c) => {
                    let common = match c {
                        Constant::None => Some(common::NONE),
                        Constant::Str(s) if s.is_empty() => Some(common::EMPTY_STR),
                        Constant::Bool(true) => Some(common::TRUE),
                        Constant::Bool(false) => Some(common::FALSE),
                        Constant::Int(-1) => Some(common::MINUS_ONE),
                        _ => None,
                    };
                    if let Some(x) = common {
                        i.op = O::LOAD_COMMON_CONSTANT;
                        i.arg = x as i32;
                    }
                }
                None => {}
            }
        }
    }
}

/// Drop constants no LOAD_CONST refers to (consts[0] always stays: it may be the docstring).
fn remove_unused_consts(cfg: &mut Cfg, consts: &mut Vec<Constant>) {
    if consts.is_empty() {
        return;
    }
    let mut used = vec![false; consts.len()];
    used[0] = true;
    for &b in &cfg.order {
        for i in &cfg.blocks[b as usize].instrs {
            if i.op == O::LOAD_CONST {
                if let Some(u) = used.get_mut(i.arg as usize) {
                    *u = true;
                }
            }
        }
    }
    let mut remap = vec![u32::MAX; consts.len()];
    let mut out = Vec::with_capacity(consts.len());
    for (k, c) in consts.drain(..).enumerate() {
        if used[k] {
            remap[k] = out.len() as u32;
            out.push(c);
        }
    }
    *consts = out;
    for &b in &cfg.order {
        for i in cfg.blocks[b as usize].instrs.iter_mut() {
            if i.op == O::LOAD_CONST {
                if let Some(&r) = remap.get(i.arg as usize) {
                    i.arg = r as i32;
                }
            }
        }
    }
}

// ============================================================================ peepholes

fn prev_real(instrs: &[Instr], k: usize) -> Option<usize> {
    (0..k).rev().find(|&j| instrs[j].op != O::NOP && !is_pseudo_marker(instrs[j].op))
}
fn next_real(instrs: &[Instr], k: usize) -> Option<usize> {
    (k + 1..instrs.len()).find(|&j| instrs[j].op != O::NOP && !is_pseudo_marker(instrs[j].op))
}

fn invert_jump(op: u16) -> u16 {
    match op {
        O::POP_JUMP_IF_FALSE => O::POP_JUMP_IF_TRUE,
        O::POP_JUMP_IF_TRUE => O::POP_JUMP_IF_FALSE,
        O::POP_JUMP_IF_NONE => O::POP_JUMP_IF_NOT_NONE,
        O::POP_JUMP_IF_NOT_NONE => O::POP_JUMP_IF_NONE,
        o => o,
    }
}

fn produces_exact_bool(i: &Instr) -> bool {
    match i.op {
        O::COMPARE_OP => i.arg & wordcode::CMP_TO_BOOL as i32 != 0,
        O::IS_OP | O::CONTAINS_OP | O::UNARY_NOT | O::TO_BOOL => true,
        _ => false,
    }
}

fn peephole_block(cfg: &mut Cfg, b: Label, consts: &mut Vec<Constant>) -> bool {
    let mut changed = false;
    let instrs = &mut cfg.blocks[b as usize].instrs;
    let n = instrs.len();
    let mut k = 0;
    while k < n {
        let ins = instrs[k];
        let next = next_real(instrs, k);
        match ins.op {
            O::TO_BOOL => {
                if let Some(p) = prev_real(instrs, k) {
                    let pi = instrs[p];
                    if produces_exact_bool(&pi) {
                        nop_out(&mut instrs[k]);
                        changed = true;
                    } else if pi.op == O::COMPARE_OP {
                        instrs[p].arg |= wordcode::CMP_TO_BOOL as i32;
                        nop_out(&mut instrs[k]);
                        changed = true;
                    }
                }
            }
            O::UNARY_NOT => {
                if let Some(nx) = next {
                    match instrs[nx].op {
                        O::POP_JUMP_IF_FALSE | O::POP_JUMP_IF_TRUE => {
                            if prev_real(instrs, k).map(|p| produces_exact_bool(&instrs[p])).unwrap_or(false) {
                                instrs[nx].op = invert_jump(instrs[nx].op);
                                nop_out(&mut instrs[k]);
                                changed = true;
                            }
                        }
                        O::UNARY_NOT => {
                            if prev_real(instrs, k).map(|p| produces_exact_bool(&instrs[p])).unwrap_or(false) {
                                nop_out(&mut instrs[k]);
                                nop_out(&mut instrs[nx]);
                                changed = true;
                            }
                        }
                        O::TO_BOOL => {
                            nop_out(&mut instrs[nx]);
                            changed = true;
                        }
                        _ => {}
                    }
                }
                if instrs[k].op == O::UNARY_NOT {
                    if let Some(p) = prev_real(instrs, k) {
                        if matches!(instrs[p].op, O::IS_OP | O::CONTAINS_OP) {
                            instrs[p].arg ^= 1;
                            nop_out(&mut instrs[k]);
                            changed = true;
                        }
                    }
                }
            }
            O::LOAD_CONST | O::LOAD_SMALL_INT | O::LOAD_COMMON_CONSTANT => {
                if let (Some(nx), Some(c)) = (next, const_of(&ins, consts)) {
                    let nop = instrs[nx];
                    match nop.op {
                        O::POP_JUMP_IF_FALSE | O::POP_JUMP_IF_TRUE => {
                            if let Some(t) = const_truth(&c) {
                                let jump = (nop.op == O::POP_JUMP_IF_TRUE) == t;
                                nop_out(&mut instrs[k]);
                                if jump {
                                    instrs[nx].op = O::JUMP;
                                } else {
                                    nop_out(&mut instrs[nx]);
                                }
                                changed = true;
                            }
                        }
                        O::POP_JUMP_IF_NONE | O::POP_JUMP_IF_NOT_NONE => {
                            let is_none = matches!(c, Constant::None);
                            let jump = (nop.op == O::POP_JUMP_IF_NONE) == is_none;
                            nop_out(&mut instrs[k]);
                            if jump {
                                instrs[nx].op = O::JUMP;
                            } else {
                                nop_out(&mut instrs[nx]);
                            }
                            changed = true;
                        }
                        O::POP_TOP => {
                            nop_out(&mut instrs[k]);
                            nop_out(&mut instrs[nx]);
                            changed = true;
                        }
                        O::IS_OP if matches!(c, Constant::None) => {
                            // x is None; POP_JUMP_IF_*  ->  POP_JUMP_IF_[NOT_]NONE
                            if let Some(nn) = next_real(instrs, nx) {
                                if matches!(instrs[nn].op, O::POP_JUMP_IF_FALSE | O::POP_JUMP_IF_TRUE) {
                                    let is_not = instrs[nx].arg != 0;
                                    let on_true = instrs[nn].op == O::POP_JUMP_IF_TRUE;
                                    // jump when (x is None) == on_true, inverted by `is not`
                                    let jump_if_none = on_true != is_not;
                                    instrs[nn].op = if jump_if_none { O::POP_JUMP_IF_NONE } else { O::POP_JUMP_IF_NOT_NONE };
                                    nop_out(&mut instrs[k]);
                                    nop_out(&mut instrs[nx]);
                                    changed = true;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            O::LOAD_GLOBAL => {
                if ins.arg & 1 == 0 {
                    if let Some(nx) = next {
                        if instrs[nx].op == O::PUSH_NULL && nx == k + 1 {
                            instrs[k].arg |= 1;
                            nop_out(&mut instrs[nx]);
                            changed = true;
                        }
                    }
                }
            }
            O::COPY if ins.arg == 1 => {
                if let Some(nx) = next {
                    if instrs[nx].op == O::POP_TOP {
                        nop_out(&mut instrs[k]);
                        nop_out(&mut instrs[nx]);
                        changed = true;
                    }
                }
            }
            O::LOAD_FAST | O::LOAD_FAST_BORROW => {
                if let Some(nx) = next {
                    if instrs[nx].op == O::POP_TOP && nx == k + 1 {
                        nop_out(&mut instrs[k]);
                        nop_out(&mut instrs[nx]);
                        changed = true;
                    }
                }
            }
            O::BUILD_TUPLE => {
                if let Some(nx) = next {
                    if instrs[nx].op == O::UNPACK_SEQUENCE && instrs[nx].arg == ins.arg {
                        match ins.arg {
                            1 => {
                                nop_out(&mut instrs[k]);
                                nop_out(&mut instrs[nx]);
                                changed = true;
                            }
                            2 | 3 => {
                                nop_out(&mut instrs[k]);
                                instrs[nx].op = O::SWAP;
                                changed = true;
                            }
                            _ => {}
                        }
                    }
                }
            }
            O::SWAP => {
                if apply_static_swap(instrs, k) {
                    changed = true;
                }
            }
            O::STORE_FAST => {
                // a store immediately overwritten by another store to the same local
                if let Some(nx) = next {
                    if instrs[nx].op == O::STORE_FAST && instrs[nx].arg == ins.arg && instrs[nx].loc.line == ins.loc.line {
                        instrs[k].op = O::POP_TOP;
                        instrs[k].arg = 0;
                        changed = true;
                    }
                }
            }
            _ => {}
        }
        k += 1;
    }
    changed
}

/// Instructions that store the top of stack and nothing else.
fn is_swappable_store(i: &Instr) -> bool {
    matches!(i.op, O::STORE_FAST | O::POP_TOP | O::STORE_FAST_MAYBE_NULL)
}

fn store_target(i: &Instr) -> Option<i32> {
    match i.op {
        O::STORE_FAST | O::STORE_FAST_MAYBE_NULL => Some(i.arg),
        _ => None,
    }
}

/// `SWAP n` followed by the stores that consume the swapped items: reorder the stores
/// instead (when they are independent and on the same line).
fn apply_static_swap(instrs: &mut [Instr], k: usize) -> bool {
    let depth = instrs[k].arg as usize;
    if depth < 2 {
        return false;
    }
    // collect the following `depth` real instructions
    let mut idx = Vec::new();
    let mut j = k;
    while idx.len() < depth {
        match next_real(instrs, j) {
            Some(nx) => {
                idx.push(nx);
                j = nx;
            }
            None => return false,
        }
    }
    if !idx.iter().all(|&x| is_swappable_store(&instrs[x])) {
        return false;
    }
    let line = instrs[idx[0]].loc.line;
    if idx.iter().any(|&x| instrs[x].loc.line != line) {
        return false;
    }
    // stores to the same local in between must keep their order
    let first = idx[0];
    let last = idx[depth - 1];
    let (ta, tb) = (store_target(&instrs[first]), store_target(&instrs[last]));
    for &m in &idx[1..depth - 1] {
        let t = store_target(&instrs[m]);
        if t.is_some() && (t == ta || t == tb) {
            return false;
        }
    }
    if ta.is_some() && ta == tb {
        return false;
    }
    instrs.swap(first, last);
    // locations stay with their positions
    let (l1, l2) = (instrs[first].loc, instrs[last].loc);
    instrs[first].loc = l2;
    instrs[last].loc = l1;
    nop_out(&mut instrs[k]);
    true
}

// ============================================================================ locals

fn convert_closure_loads(cfg: &mut Cfg) {
    for &b in &cfg.order {
        for i in cfg.blocks[b as usize].instrs.iter_mut() {
            if i.op == O::LOAD_CLOSURE {
                i.op = O::LOAD_FAST;
            }
        }
    }
}

/// LOAD_FAST of a local that may be unbound at that point becomes LOAD_FAST_CHECK.
fn add_checks_for_unbound_locals(cfg: &mut Cfg, nlocals: usize, bound: &[bool]) {
    if nlocals == 0 {
        return;
    }
    let words = nlocals.div_ceil(64);
    let pos = cfg.positions();
    let nb = cfg.blocks.len();
    type Bits = Vec<u64>;
    let mut init: Bits = vec![0; words];
    for s in 0..nlocals {
        if !bound.get(s).copied().unwrap_or(false) {
            init[s / 64] |= 1 << (s % 64);
        }
    }
    let mut state: Vec<Option<Bits>> = vec![None; nb];
    let entry = cfg.order[0];
    state[entry as usize] = Some(init);
    let mut work = vec![entry];
    let mut inq = vec![false; nb];
    inq[entry as usize] = true;
    fn merge(state: &mut [Option<Bits>], work: &mut Vec<Label>, inq: &mut [bool], t: Label, s: &Bits) {
        let new: Bits = match &state[t as usize] {
            None => s.clone(),
            Some(o) => o.iter().zip(s.iter()).map(|(a, b)| a | b).collect(),
        };
        if state[t as usize].as_ref() != Some(&new) {
            state[t as usize] = Some(new);
            if !inq[t as usize] {
                inq[t as usize] = true;
                work.push(t);
            }
        }
    }
    let set = |s: &mut Bits, i: usize, v: bool| {
        if i / 64 < s.len() {
            if v {
                s[i / 64] |= 1 << (i % 64);
            } else {
                s[i / 64] &= !(1 << (i % 64));
            }
        }
    };
    let transfer = |s: &mut Bits, i: &Instr| match i.op {
        O::STORE_FAST | O::LOAD_FAST_CHECK | O::LOAD_FAST => set(s, i.arg as usize, false),
        O::STORE_FAST_MAYBE_NULL | O::DELETE_FAST | O::LOAD_FAST_AND_CLEAR => set(s, i.arg as usize, true),
        O::MAKE_CELL => set(s, i.arg as usize, false),
        _ => {}
    };
    while let Some(b) = work.pop() {
        inq[b as usize] = false;
        let mut s = state[b as usize].clone().unwrap();
        let instrs = cfg.blocks[b as usize].instrs.clone();
        for i in &instrs {
            if i.handler != NO_LABEL {
                merge(&mut state, &mut work, &mut inq, i.handler, &s);
            }
            if is_setup(i.op) {
                merge(&mut state, &mut work, &mut inq, i.target, &s);
            }
            // a failed LOAD_FAST_CHECK does not continue: the local is bound afterwards
            transfer(&mut s, i);
            if i.handler != NO_LABEL {
                merge(&mut state, &mut work, &mut inq, i.handler, &s);
            }
            if is_jump_edge(i.op) {
                merge(&mut state, &mut work, &mut inq, i.target, &s);
            }
        }
        if cfg.falls_through(b) {
            if let Some(n) = cfg.next_in_order(&pos, b) {
                merge(&mut state, &mut work, &mut inq, n, &s);
            }
        }
    }
    for &b in &cfg.order.clone() {
        let Some(mut s) = state[b as usize].clone() else { continue };
        for i in cfg.blocks[b as usize].instrs.iter_mut() {
            if i.op == O::LOAD_FAST {
                let a = i.arg as usize;
                if a < nlocals && s[a / 64] & (1 << (a % 64)) != 0 {
                    i.op = O::LOAD_FAST_CHECK;
                }
            }
            transfer(&mut s, i);
        }
    }
}

/// Stack inputs/outputs of an instruction for the borrow analysis.
fn stack_io(op: u16, arg: i32) -> (usize, usize) {
    let n = arg.max(0) as usize;
    match op {
        O::NOP | O::RESUME | O::EXTENDED_ARG | O::CACHE | O::NOT_TAKEN | O::MAKE_CELL | O::COPY_FREE_VARS
        | O::DELETE_FAST | O::DELETE_DEREF | O::DELETE_GLOBAL | O::DELETE_NAME | O::SETUP_ANNOTATIONS => (0, 0),
        O::POP_TOP | O::STORE_FAST | O::STORE_NAME | O::STORE_GLOBAL | O::STORE_DEREF | O::DELETE_ATTR
        | O::RETURN_VALUE | O::POP_EXCEPT | O::END_FOR | O::EXIT_INIT_CHECK | O::POP_JUMP_IF_FALSE
        | O::POP_JUMP_IF_TRUE | O::POP_JUMP_IF_NONE | O::POP_JUMP_IF_NOT_NONE | O::STORE_FAST_MAYBE_NULL => (1, 0),
        O::PUSH_NULL | O::LOAD_CONST | O::LOAD_SMALL_INT | O::LOAD_COMMON_CONSTANT | O::LOAD_NAME | O::LOAD_DEREF
        | O::LOAD_FAST | O::LOAD_FAST_BORROW | O::LOAD_FAST_CHECK | O::LOAD_FAST_AND_CLEAR | O::LOAD_CLOSURE
        | O::LOAD_BUILD_CLASS | O::LOAD_LOCALS | O::RETURN_GENERATOR | O::IMPORT_FROM | O::GET_LEN
        | O::MATCH_MAPPING | O::MATCH_SEQUENCE | O::MATCH_KEYS | O::GET_ANEXT | O::WITH_EXCEPT_START => (0, 1),
        O::LOAD_GLOBAL => (0, 1 + (n & 1)),
        O::LOAD_ATTR => (1, 1 + (n & 1)),
        O::LOAD_SUPER_ATTR => (3, 1 + (n & 1)),
        O::LOAD_SPECIAL => (1, 2),
        O::LOAD_FROM_DICT_OR_DEREF | O::LOAD_FROM_DICT_OR_GLOBALS => (1, 1),
        O::STORE_ATTR => (2, 0),
        O::STORE_SUBSCR => (3, 0),
        O::DELETE_SUBSCR => (2, 0),
        O::STORE_SLICE => (4, 0),
        O::BINARY_SLICE => (3, 1),
        O::BINARY_OP | O::COMPARE_OP | O::IS_OP | O::CONTAINS_OP | O::FORMAT_WITH_SPEC | O::IMPORT_NAME
        | O::BUILD_TEMPLATE | O::CALL_INTRINSIC_2 | O::SET_FUNCTION_ATTRIBUTE => (2, 1),
        O::UNARY_NEGATIVE | O::UNARY_INVERT | O::UNARY_NOT | O::TO_BOOL | O::FORMAT_SIMPLE | O::CONVERT_VALUE
        | O::CALL_INTRINSIC_1 | O::MAKE_FUNCTION | O::GET_AWAITABLE | O::GET_AITER | O::YIELD_VALUE => (1, 1),
        O::BUILD_TUPLE | O::BUILD_LIST | O::BUILD_SET | O::BUILD_STRING | O::BUILD_SLICE => (n, 1),
        O::BUILD_MAP => (2 * n, 1),
        O::BUILD_INTERPOLATION => (2 + (n & 1), 1),
        O::LIST_APPEND | O::SET_ADD | O::LIST_EXTEND | O::SET_UPDATE | O::DICT_UPDATE | O::DICT_MERGE => (1, 0),
        O::MAP_ADD => (2, 0),
        O::UNPACK_SEQUENCE => (1, n),
        O::UNPACK_EX => (1, (n & 0xff) + (n >> 8) + 1),
        O::CALL => (n + 2, 1),
        O::CALL_KW => (n + 3, 1),
        O::CALL_FUNCTION_EX => (4, 1),
        O::RAISE_VARARGS => (n, 0),
        O::RERAISE => (1, 0),
        O::GET_ITER => (1, 2),
        O::FOR_ITER => (0, 1),
        O::POP_ITER => (2, 0),
        O::SEND => (1, 1),
        O::END_SEND => (3, 1),
        O::CLEANUP_THROW => (2, 1),
        O::END_ASYNC_FOR => (2, 0),
        O::MATCH_CLASS => (3, 1),
        O::CHECK_EXC_MATCH => (1, 1),
        O::CHECK_EG_MATCH => (2, 2),
        O::PUSH_EXC_INFO => (1, 2),
        _ => (0, 0),
    }
}

/// LOAD_FAST -> LOAD_FAST_BORROW when the pushed reference is consumed inside its basic
/// block before the local can change (and is not stored into a local itself). Only blocks
/// reachable without exception edges are considered.
fn optimize_load_fast(cfg: &mut Cfg) {
    let warm = cfg.mark_reachable(false);
    #[derive(Clone, Copy)]
    struct Ref {
        instr: usize,
        local: i32,
    }
    for &b in &cfg.order.clone() {
        if !warm[b as usize] || cfg.blocks[b as usize].is_handler {
            continue;
        }
        let instrs = &mut cfg.blocks[b as usize].instrs;
        let mut unsafe_ = vec![false; instrs.len()];
        let mut stack: Vec<Option<Ref>> = Vec::new();
        let kill = |stack: &[Option<Ref>], unsafe_: &mut [bool], local: i32| {
            for r in stack.iter().flatten() {
                if r.local == local {
                    unsafe_[r.instr] = true;
                }
            }
        };
        for k in 0..instrs.len() {
            let i = instrs[k];
            match i.op {
                O::LOAD_FAST => stack.push(Some(Ref { instr: k, local: i.arg })),
                O::STORE_FAST | O::STORE_FAST_MAYBE_NULL => {
                    if let Some(Some(r)) = stack.pop() {
                        unsafe_[r.instr] = true;
                    }
                    kill(&stack, &mut unsafe_, i.arg);
                }
                O::DELETE_FAST => kill(&stack, &mut unsafe_, i.arg),
                O::LOAD_FAST_AND_CLEAR => {
                    kill(&stack, &mut unsafe_, i.arg);
                    stack.push(None);
                }
                O::COPY => {
                    let d = i.arg as usize;
                    let v = if d >= 1 && d <= stack.len() { stack[stack.len() - d] } else { None };
                    stack.push(v);
                }
                O::SWAP => {
                    let d = i.arg as usize;
                    let len = stack.len();
                    if d >= 2 && d <= len {
                        stack.swap(len - 1, len - d);
                    }
                }
                O::GET_ITER => {
                    // iter(x) may return x itself
                    let v = stack.pop().flatten();
                    stack.push(v);
                    stack.push(None);
                }
                O::LOAD_SPECIAL => {
                    let v = stack.pop().flatten();
                    stack.push(None);
                    stack.push(v);
                }
                O::LOAD_ATTR if i.arg & 1 != 0 => {
                    let v = stack.pop().flatten();
                    stack.push(None);
                    stack.push(v);
                }
                O::YIELD_VALUE => {
                    // a suspended frame must not hold borrowed references
                    for r in stack.iter().flatten() {
                        unsafe_[r.instr] = true;
                    }
                    stack.pop();
                    stack.push(None);
                }
                _ => {
                    let (pops, pushes) = stack_io(i.op, i.arg);
                    for _ in 0..pops {
                        stack.pop();
                    }
                    for _ in 0..pushes {
                        stack.push(None);
                    }
                }
            }
        }
        // references still on the stack at the end of the block escape it
        for r in stack.iter().flatten() {
            unsafe_[r.instr] = true;
        }
        for k in 0..instrs.len() {
            if instrs[k].op == O::LOAD_FAST && !unsafe_[k] {
                instrs[k].op = O::LOAD_FAST_BORROW;
            }
        }
    }
}

fn insert_superinstructions(cfg: &mut Cfg) {
    for &b in &cfg.order.clone() {
        let instrs = &mut cfg.blocks[b as usize].instrs;
        let mut k = 0;
        while k + 1 < instrs.len() {
            let (a, c) = (instrs[k], instrs[k + 1]);
            let same_line = a.loc.line == c.loc.line;
            let small = (0..16).contains(&a.arg) && (0..16).contains(&c.arg);
            let fused = match (a.op, c.op) {
                (O::LOAD_FAST, O::LOAD_FAST) => Some(O::LOAD_FAST_LOAD_FAST),
                (O::LOAD_FAST_BORROW, O::LOAD_FAST_BORROW) => Some(O::LOAD_FAST_BORROW_LOAD_FAST_BORROW),
                (O::STORE_FAST, O::LOAD_FAST) => Some(O::STORE_FAST_LOAD_FAST),
                (O::STORE_FAST, O::STORE_FAST) => Some(O::STORE_FAST_STORE_FAST),
                _ => None,
            };
            if let Some(op) = fused {
                if same_line && small && a.handler == c.handler {
                    instrs[k].op = op;
                    instrs[k].arg = (a.arg << 4) | c.arg;
                    instrs.remove(k + 1);
                }
            }
            k += 1;
        }
    }
}

// ============================================================================ layout

/// Blocks only reachable through exception handlers go to the end of the code.
fn push_cold_blocks_to_end(cfg: &mut Cfg) {
    let warm = cfg.mark_reachable(false);
    if cfg.order.iter().all(|b| warm[*b as usize]) {
        return;
    }
    let pos = cfg.positions();
    // explicit jumps where a block falls through into a block that changes side
    let order = cfg.order.clone();
    for (k, &b) in order.iter().enumerate() {
        if !cfg.falls_through(b) {
            continue;
        }
        let Some(&n) = order.get(k + 1) else { continue };
        if warm[b as usize] != warm[n as usize] {
            // (the added jump is not covered by any handler)
            cfg.blocks[b as usize].instrs.push(Instr { op: O::JUMP_NO_INTERRUPT, arg: 0, target: n, loc: NO_POS, handler: NO_LABEL });
        }
    }
    let _ = pos;
    let (w, c): (Vec<Label>, Vec<Label>) = order.iter().partition(|b| warm[**b as usize]);
    cfg.order = w;
    cfg.order.extend(c);
    for b in &cfg.order {
        cfg.blocks[*b as usize].cold = !warm[*b as usize];
    }
}

/// Conditional jumps must go forward: a backward one is inverted around a new block holding
/// an unconditional (backward) jump. Unconditional jumps get their direction; jumps to the
/// next block disappear; NOT_TAKEN follows every conditional jump.
fn normalize_jumps(cfg: &mut Cfg) {
    // conditional backward jumps
    let mut k = 0;
    while k < cfg.order.len() {
        let pos = cfg.positions();
        let b = cfg.order[k];
        if let Some(last) = cfg.blocks[b as usize].instrs.last().copied() {
            if is_cond_jump(last.op) && pos[last.target as usize] <= k {
                let fall = cfg.order.get(k + 1).copied();
                let nb = cfg.new_block();
                cfg.blocks[nb as usize].instrs.push(Instr { op: O::JUMP, arg: 0, target: last.target, loc: last.loc, handler: last.handler });
                cfg.blocks[nb as usize].cold = cfg.blocks[b as usize].cold;
                let n = cfg.blocks[b as usize].instrs.len();
                let li = &mut cfg.blocks[b as usize].instrs[n - 1];
                li.op = invert_jump(li.op);
                match fall {
                    Some(f) => li.target = f,
                    None => {
                        // nothing follows: cannot happen for well-formed code
                    }
                }
                cfg.order.insert(k + 1, nb);
            }
        }
        k += 1;
    }
    let pos = cfg.positions();
    for (k, &b) in cfg.order.clone().iter().enumerate() {
        let next = cfg.order.get(k + 1).copied();
        let n = cfg.blocks[b as usize].instrs.len();
        if n == 0 {
            continue;
        }
        let last = cfg.blocks[b as usize].instrs[n - 1];
        if is_uncond_jump(last.op) {
            if Some(last.target) == next || (next.is_some() && empty_chain_reaches(cfg, &pos, next.unwrap(), last.target)) {
                let li = &mut cfg.blocks[b as usize].instrs[n - 1];
                li.op = O::NOP;
                li.target = NO_LABEL;
                if !li.loc.is_some() {
                    cfg.blocks[b as usize].instrs.pop();
                }
                continue;
            }
            let backward = pos[last.target as usize] <= k;
            let op = match (last.op, backward) {
                (O::JUMP, false) | (O::JUMP_NO_INTERRUPT, false) => O::JUMP_FORWARD,
                (O::JUMP, true) => O::JUMP_BACKWARD,
                (O::JUMP_NO_INTERRUPT, true) => O::JUMP_BACKWARD_NO_INTERRUPT,
                (o, _) => o,
            };
            cfg.blocks[b as usize].instrs[n - 1].op = op;
        }
        if is_cond_jump(last.op) {
            let nt = Instr { op: O::NOT_TAKEN, arg: 0, target: NO_LABEL, loc: last.loc, handler: last.handler };
            match next {
                Some(nb) if !cfg.blocks[nb as usize].is_handler => cfg.blocks[nb as usize].instrs.insert(0, nt),
                _ => {
                    let nb = cfg.new_block();
                    cfg.blocks[nb as usize].instrs.push(nt);
                    cfg.order.insert(k + 1, nb);
                }
            }
        }
    }
}

/// Does the chain of empty blocks starting at `from` reach `to` before any instruction?
fn empty_chain_reaches(cfg: &Cfg, pos: &[usize], from: Label, to: Label) -> bool {
    let mut b = from;
    for _ in 0..16 {
        if b == to {
            return true;
        }
        if !cfg_block_is_empty_fallthrough(&cfg.blocks, b) {
            return false;
        }
        match cfg.next_in_order(pos, b) {
            Some(n) => b = n,
            None => return false,
        }
    }
    false
}

// ============================================================================ stack depth

fn stack_depth(cfg: &mut Cfg) -> R<u32> {
    let pos = cfg.positions();
    for b in cfg.blocks.iter_mut() {
        b.depth = -1;
    }
    let entry = cfg.order[0];
    cfg.blocks[entry as usize].depth = 0;
    let mut work = vec![entry];
    let mut maxd = 0i32;
    let push = |blocks: &mut [Block], work: &mut Vec<Label>, t: Label, d: i32| -> R<()> {
        let cur = blocks[t as usize].depth;
        if cur < 0 {
            blocks[t as usize].depth = d;
            work.push(t);
        } else if cur != d {
            return Err(ierr(&format!("inconsistent stack depth ({} vs {})", cur, d)));
        }
        Ok(())
    };
    while let Some(b) = work.pop() {
        let mut d = cfg.blocks[b as usize].depth;
        let instrs = cfg.blocks[b as usize].instrs.clone();
        for i in &instrs {
            if is_jump_edge(i.op) || is_setup(i.op) {
                let jd = d + wordcode::stack_effect(i.op, i.arg, Some(true)).ok_or_else(|| ierr("unknown opcode"))?;
                if jd < 0 {
                    return Err(ierr("negative stack depth at jump"));
                }
                maxd = maxd.max(jd);
                push(&mut cfg.blocks, &mut work, i.target, jd)?;
            }
            d += wordcode::stack_effect(i.op, i.arg, Some(false)).ok_or_else(|| ierr(&format!("no stack effect for opcode {}", i.op)))?;
            if d < 0 {
                return Err(ierr(&format!("negative stack depth at opcode {}", wordcode::opname(i.op).unwrap_or("?"))));
            }
            maxd = maxd.max(d);
        }
        if cfg.falls_through(b) {
            if let Some(n) = cfg.next_in_order(&pos, b) {
                push(&mut cfg.blocks, &mut work, n, d)?;
            }
        }
    }
    Ok(maxd as u32)
}

// ============================================================================ final assembly

fn finish(mut cfg: Cfg, info: UnitInfo, layout: Layout) -> R<CodeObject> {
    push_cold_blocks_to_end(&mut cfg);
    duplicate_exits_without_lineno(&mut cfg);
    propagate_line_numbers(&mut cfg);
    normalize_jumps(&mut cfg);
    // prefix: COPY_FREE_VARS, MAKE_CELL for every (non-hidden) cell
    {
        let mut pre: Vec<Instr> = Vec::new();
        if !info.freevars.is_empty() {
            pre.push(Instr::new(O::COPY_FREE_VARS, info.freevars.len() as i32, NO_POS));
        }
        for (ci, k) in info.cellkinds.iter().enumerate() {
            if k & fast::HIDDEN != 0 {
                continue;
            }
            pre.push(Instr::new(O::MAKE_CELL, layout.cell_index[ci] as i32, NO_POS));
        }
        if !pre.is_empty() {
            let entry = cfg.order[0];
            let blk = &mut cfg.blocks[entry as usize];
            pre.extend(blk.instrs.drain(..));
            blk.instrs = pre;
        }
    }
    let stacksize = stack_depth(&mut cfg)?;

    // flatten, dropping pseudo markers
    struct Flat {
        op: u16,
        arg: i32,
        target: Label,
        loc: Pos,
        handler: Label,
    }
    let mut flat: Vec<Flat> = Vec::new();
    let mut block_first: Vec<usize> = vec![usize::MAX; cfg.blocks.len()];
    for &b in &cfg.order {
        block_first[b as usize] = flat.len();
        for i in &cfg.blocks[b as usize].instrs {
            let op = match i.op {
                o if is_pseudo_marker(o) => continue,
                O::STORE_FAST_MAYBE_NULL => O::STORE_FAST,
                O::LOAD_CLOSURE => O::LOAD_FAST,
                O::JUMP => O::JUMP_FORWARD,
                O::JUMP_NO_INTERRUPT => O::JUMP_FORWARD,
                o => o,
            };
            if op >= 256 {
                return Err(ierr(&format!("pseudo-instruction {} left", wordcode::opname(op).unwrap_or("?"))));
            }
            flat.push(Flat { op, arg: i.arg, target: i.target, loc: i.loc, handler: i.handler });
        }
    }
    // a block's first instruction may be in a later block if it is empty
    let n = flat.len();
    let mut target_index = vec![n; cfg.blocks.len()];
    {
        let mut next_start = n;
        for &b in cfg.order.iter().rev() {
            if block_first[b as usize] != usize::MAX && block_first[b as usize] < n && !cfg.blocks[b as usize].instrs.iter().all(|i| is_pseudo_marker(i.op)) {
                next_start = block_first[b as usize];
            }
            target_index[b as usize] = next_start;
        }
    }
    // sizes with EXTENDED_ARG prefixes, iterated until jump offsets are stable
    let ncache: Vec<u32> = flat.iter().map(|f| wordcode::cache_entries(f.op)).collect();
    let arg_units = |a: u32| -> u32 {
        if a < 1 << 8 {
            1
        } else if a < 1 << 16 {
            2
        } else if a < 1 << 24 {
            3
        } else {
            4
        }
    };
    let mut size: Vec<u32> = flat.iter().map(|f| if f.target == NO_LABEL { arg_units(f.arg as u32) } else { 1 }).collect();
    let mut offset: Vec<u32> = vec![0; n + 1];
    let mut jarg: Vec<u32> = vec![0; n];
    loop {
        let mut o = 0u32;
        for k in 0..n {
            offset[k] = o;
            o += size[k] + ncache[k];
        }
        offset[n] = o;
        let mut changed = false;
        for k in 0..n {
            if flat[k].target == NO_LABEL {
                continue;
            }
            let t = target_index[flat[k].target as usize];
            let toff = offset[t];
            // the jump is relative to the end of the instruction (after its caches)
            let op_off = offset[k] + size[k] - 1;
            let after = op_off + 1 + ncache[k];
            let a = if wordcode::is_backward_jump(flat[k].op) { after.saturating_sub(toff) } else { toff.saturating_sub(after) };
            jarg[k] = a;
            let s = arg_units(a);
            if s != size[k] {
                size[k] = s;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // encode
    let total = offset[n] as usize;
    let mut code: Vec<u8> = Vec::with_capacity(total * 2);
    let mut runs: Vec<(u32, Position)> = Vec::new();
    let to_pos = |l: Pos| -> Position {
        if !l.is_some() {
            return Position::NONE;
        }
        Position {
            lineno: Some(l.line as u32),
            end_lineno: Some(l.end_line.max(l.line) as u32),
            col: if l.col >= 0 { Some(l.col as u32) } else { None },
            end_col: if l.end_col >= 0 { Some(l.end_col as u32) } else { None },
        }
    };
    for k in 0..n {
        let f = &flat[k];
        let arg = if f.target != NO_LABEL { jarg[k] } else { f.arg as u32 };
        let s = size[k];
        for j in (1..s).rev() {
            code.push(O::EXTENDED_ARG as u8);
            code.push((arg >> (8 * j)) as u8);
        }
        code.push(f.op as u8);
        code.push(arg as u8);
        for _ in 0..ncache[k] {
            code.push(0);
            code.push(0);
        }
        let units = s + ncache[k];
        let p = to_pos(f.loc);
        match runs.last_mut() {
            Some((len, lp)) if *lp == p => *len += units,
            _ => runs.push((units, p)),
        }
    }
    let linetable = wordcode::encode_linetable(&runs, info.firstlineno);
    // exception table: NOPs inherit the previous instruction's handler
    let mut entries: Vec<ExcEntry> = Vec::new();
    {
        let mut prev_handler = NO_LABEL;
        let mut cur: Option<(u32, Label)> = None;
        for k in 0..n {
            let h = if flat[k].op == O::NOP { prev_handler } else { flat[k].handler };
            prev_handler = h;
            let start = offset[k];
            match cur {
                Some((s, ch)) if ch == h => {
                    let _ = s;
                }
                _ => {
                    if let Some((s, ch)) = cur.take() {
                        if ch != NO_LABEL {
                            entries.push(exc_entry(&cfg, &target_index, &offset, s, start, ch)?);
                        }
                    }
                    cur = Some((start, h));
                }
            }
        }
        if let Some((s, ch)) = cur {
            if ch != NO_LABEL {
                entries.push(exc_entry(&cfg, &target_index, &offset, s, offset[n], ch)?);
            }
        }
    }
    let exceptiontable = wordcode::encode_exception_table(&entries);

    // localsplus
    let mut names = info.varnames.clone();
    let mut kinds = info.varkinds.clone();
    for (ci, c) in info.cellvars.iter().enumerate() {
        match info.cell_arg[ci] {
            Some(v) => kinds[v as usize] |= fast::CELL,
            None => {
                names.push(c.clone());
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
        flags |= crate::bytecode::co::NOFREE;
    }
    let _ = flags;
    Ok(CodeObject {
        name: info.name,
        qualname: info.qualname,
        filename: info.filename,
        firstlineno: info.firstlineno,
        argcount: info.argcount,
        posonlyargcount: info.posonlyargcount,
        kwonlyargcount: info.kwonlyargcount,
        flags: info.flags,
        stacksize,
        code,
        consts: info.consts,
        names: info.names,
        localsplusnames: names,
        localspluskinds: kinds,
        linetable,
        exceptiontable,
    })
}

fn exc_entry(cfg: &Cfg, target_index: &[usize], offset: &[u32], start: u32, end: u32, h: Label) -> R<ExcEntry> {
    let blk = &cfg.blocks[h as usize];
    let t = target_index[h as usize];
    if blk.depth < 0 {
        return Err(ierr("exception handler without stack depth"));
    }
    let depth = blk.depth - 1 - blk.preserve_lasti as i32;
    Ok(ExcEntry { start, end, target: offset[t], depth: depth.max(0) as u32, lasti: blk.preserve_lasti })
}
