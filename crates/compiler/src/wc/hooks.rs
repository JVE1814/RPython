//! Compiler stages exposed for `_testinternalcapi` (compiler_codegen / optimize_cfg /
//! assemble_code_object): instruction sequences in CPython's numbering where jump and
//! exception-handler arguments are instruction indices.

use super::cfg::{self, Cfg, UnitInfo};
use super::ir::{has_target, is_setup, is_terminator, Instr, Ir, Label, Pos, NO_LABEL, NO_POS};
use super::Compiler;
use crate::error::{CompileError, ErrorKind};
use crate::ast::Mod;
use crate::bytecode::fast;
use crate::wordcode::{self, op as O, CodeObject, Constant};
use crate::CompileOptions;

/// One instruction: opcode, argument (jump targets as instruction indices) and location
/// `(lineno, col_offset, end_lineno, end_col_offset)` (-1 = unknown).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeqInstr {
    pub op: u16,
    pub arg: i32,
    pub loc: [i32; 4],
}

/// The instruction sequence of a code unit and of the units nested in it.
#[derive(Clone, Debug, Default)]
pub struct Seq {
    pub instrs: Vec<SeqInstr>,
    pub nested: Vec<Seq>,
    /// constants referred to by LOAD_CONST (codegen output only)
    pub consts: Vec<Constant>,
}

fn pos_tuple(p: Pos) -> [i32; 4] {
    if p.is_some() {
        [p.line, p.col, p.end_line, p.end_col]
    } else {
        [-1, -1, -1, -1]
    }
}

fn tuple_pos(t: [i32; 4]) -> Pos {
    if t[0] < 0 {
        NO_POS
    } else {
        Pos { line: t[0], col: t[1], end_line: if t[2] < 0 { t[0] } else { t[2] }, end_col: t[3] }
    }
}

/// Flatten blocks (in layout order) into a sequence; targets become instruction indices.
fn linearize(blocks: &[super::ir::Block], order: &[Label]) -> Vec<SeqInstr> {
    let mut start = vec![usize::MAX; blocks.len()];
    let mut n = 0;
    for &b in order {
        start[b as usize] = n;
        n += blocks[b as usize].instrs.len();
    }
    let mut out = Vec::with_capacity(n);
    for &b in order {
        for i in &blocks[b as usize].instrs {
            let arg = if i.target != NO_LABEL { start[i.target as usize] as i32 } else { i.arg };
            out.push(SeqInstr { op: i.op, arg, loc: pos_tuple(i.loc) });
        }
    }
    out
}

/// Blocks from a sequence: a block starts at every jump target and after every jump.
fn to_ir(instrs: &[SeqInstr]) -> Ir {
    let n = instrs.len();
    let mut starts = vec![false; n + 1];
    starts[0] = true;
    for (k, i) in instrs.iter().enumerate() {
        if has_target(i.op) {
            if (i.arg as usize) <= n {
                starts[i.arg as usize] = true;
            }
            if !is_setup(i.op) {
                starts[k + 1] = true;
            }
        } else if is_terminator(i.op) {
            starts[k + 1] = true;
        }
    }
    let mut ir = Ir::new();
    let mut label_at = vec![NO_LABEL; n + 1];
    label_at[0] = 0;
    for k in 1..=n {
        if starts[k] {
            label_at[k] = ir.new_label();
        }
    }
    let mut cur = 0;
    for (k, i) in instrs.iter().enumerate() {
        if starts[k] && k > 0 {
            cur = label_at[k];
            ir.order.push(cur);
        }
        let target = if has_target(i.op) { label_at.get(i.arg as usize).copied().unwrap_or(NO_LABEL) } else { NO_LABEL };
        let arg = if has_target(i.op) { 0 } else { i.arg };
        ir.blocks[cur as usize].instrs.push(Instr { op: i.op, arg, target, loc: tuple_pos(i.loc), handler: NO_LABEL });
    }
    if starts[n] && n > 0 && label_at[n] != NO_LABEL {
        ir.order.push(label_at[n]);
    }
    ir
}

impl<'a> Compiler<'a> {
    /// Codegen-only exit of a unit: record its sequence, return a stand-in code object (it
    /// only needs the free variables, for the parent's closure).
    pub(crate) fn finish_codegen_only(&mut self, ir: Ir, info: UnitInfo) -> CodeObject {
        let nested = self.seq_stack.pop().unwrap_or_default();
        let seq = Seq { instrs: linearize(&ir.blocks, &ir.order), nested, consts: info.consts.clone() };
        match self.seq_stack.last_mut() {
            Some(parent) => parent.push(seq),
            None => self.seq_result = Some(seq),
        }
        CodeObject {
            name: info.name,
            qualname: info.qualname,
            filename: info.filename,
            firstlineno: info.firstlineno,
            localsplusnames: info.freevars.clone(),
            localspluskinds: vec![fast::FREE; info.freevars.len()],
            ..Default::default()
        }
    }
}

/// `compiler_codegen`: the unoptimised instruction sequence of a module.
pub fn codegen(module: &Mod, opts: &CompileOptions) -> Result<Seq, CompileError> {
    let (_, seq) = super::compile_module_impl(module, opts, true)?;
    seq.ok_or_else(|| CompileError::other(ErrorKind::SystemError, "no code generated"))
}

/// `optimize_cfg`: run the CFG optimiser on a sequence; `consts` is updated in place.
pub fn optimize(instrs: &[SeqInstr], consts: &mut Vec<Constant>, nlocals: usize) -> Result<Vec<SeqInstr>, String> {
    for i in instrs {
        if i.op == O::LOAD_CONST && (i.arg < 0 || i.arg as usize >= consts.len()) {
            return Err(format!("LOAD_CONST index {} is out of range for consts (length {})", i.arg, consts.len()));
        }
        if wordcode::opname(i.op).is_none() {
            return Err(format!("invalid opcode {}", i.op));
        }
    }
    let mut cfg = Cfg::from_ir(to_ir(instrs));
    let bound = vec![false; nlocals];
    cfg::optimize_cfg(&mut cfg, consts, nlocals, &bound).map_err(|e| e.msg)?;
    // SETUP_* stay in this output and carry their line, so a NOP on the same line right
    // after one is redundant
    for &b in &cfg.order {
        let instrs = &mut cfg.blocks[b as usize].instrs;
        let mut k = 1;
        while k < instrs.len() {
            if instrs[k].op == O::NOP && is_setup(instrs[k - 1].op) && instrs[k].loc.line == instrs[k - 1].loc.line {
                instrs.remove(k);
            } else {
                k += 1;
            }
        }
    }
    Ok(linearize(&cfg.blocks, &cfg.order))
}

/// Metadata of `assemble_code_object`.
#[derive(Clone, Debug, Default)]
pub struct AsmMeta {
    pub name: String,
    pub qualname: String,
    pub filename: String,
    pub firstlineno: u32,
    pub argcount: u32,
    pub posonlyargcount: u32,
    pub kwonlyargcount: u32,
    pub nconsts: usize,
    pub names: Vec<String>,
    pub varnames: Vec<String>,
    pub cellvars: Vec<String>,
    pub freevars: Vec<String>,
    pub flags: u32,
}

/// `assemble_code_object`: assemble a sequence as it is (no optimisation). The constants of
/// the result are placeholders (`Opaque(i)` for index i): the caller supplies the objects.
pub fn assemble(instrs: &[SeqInstr], meta: AsmMeta) -> Result<CodeObject, String> {
    let cell_arg: Vec<Option<u32>> = meta.cellvars.iter().map(|c| meta.varnames.iter().position(|v| v == c).map(|p| p as u32)).collect();
    let nparams = meta.argcount + meta.kwonlyargcount;
    let info = UnitInfo {
        name: meta.name,
        qualname: meta.qualname,
        filename: meta.filename,
        firstlineno: meta.firstlineno,
        argcount: meta.argcount,
        posonlyargcount: meta.posonlyargcount,
        kwonlyargcount: meta.kwonlyargcount,
        flags: meta.flags,
        consts: (0..meta.nconsts).map(|i| Constant::Opaque(i as u32)).collect(),
        names: meta.names,
        varkinds: vec![fast::LOCAL; meta.varnames.len()],
        varnames: meta.varnames,
        cellkinds: vec![fast::CELL; meta.cellvars.len()],
        cellvars: meta.cellvars,
        cell_arg,
        freevars: meta.freevars,
        nparams,
    };
    let cfg = Cfg::from_ir(to_ir(instrs));
    cfg::assemble_only(cfg, info).map_err(|e| e.msg)
}

/// Whether the argument of `op` is a jump target / handler (an instruction index here).
pub fn is_target_op(op: u16) -> bool {
    has_target(op)
}
