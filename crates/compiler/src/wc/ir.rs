//! Instruction IR of the wordcode pipeline: basic blocks of CPython-numbered instructions
//! (real opcodes and pseudo-instructions such as JUMP, SETUP_FINALLY, POP_BLOCK), produced by
//! code generation and rewritten by the CFG optimiser before assembly.

use crate::wordcode::op as O;

/// Source position of an instruction; `line < 0` means "no location".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Pos {
    pub line: i32,
    pub end_line: i32,
    pub col: i32,
    pub end_col: i32,
}

pub const NO_POS: Pos = Pos { line: -1, end_line: -1, col: -1, end_col: -1 };
/// "The location of the next instruction" (resolved by `cfg::resolve_next_positions`): used
/// where a branch event should report the code it leads to.
pub const NEXT_POS: Pos = Pos { line: -2, end_line: -2, col: -2, end_col: -2 };

impl Pos {
    pub fn is_some(&self) -> bool {
        self.line >= 0
    }
    pub fn new(line: i32, end_line: i32, col: i32, end_col: i32) -> Pos {
        Pos { line, end_line, col, end_col }
    }
}

pub type Label = u32;
pub const NO_LABEL: Label = u32::MAX;

/// Symbolic slot references for cell/free variables (resolved to localsplus indices by the
/// assembler, once the cell layout is known).
pub const CELL_TAG: i32 = 1 << 28;
pub const FREE_TAG: i32 = 1 << 29;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Instr {
    pub op: u16,
    pub arg: i32,
    /// jump / handler target block (pseudo SETUP_* and jumps)
    pub target: Label,
    pub loc: Pos,
    /// innermost exception handler block covering this instruction (set by the optimiser)
    pub handler: Label,
}

impl Instr {
    pub fn new(op: u16, arg: i32, loc: Pos) -> Instr {
        Instr { op, arg, target: NO_LABEL, loc, handler: NO_LABEL }
    }
    pub fn jump(op: u16, target: Label, loc: Pos) -> Instr {
        Instr { op, arg: 0, target, loc, handler: NO_LABEL }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Block {
    pub instrs: Vec<Instr>,
    /// target of a SETUP_* (exception handler entry)
    pub is_handler: bool,
    /// handler pushes the raising instruction's offset (lasti) before the exception
    pub preserve_lasti: bool,
    /// stack depth at block entry (-1 = unknown)
    pub depth: i32,
    /// only reachable through exception handlers
    pub cold: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Ir {
    pub blocks: Vec<Block>,
    /// layout order of blocks
    pub order: Vec<Label>,
    /// block currently emitted into
    pub cur: Label,
}

pub fn is_terminator(op: u16) -> bool {
    matches!(
        op,
        O::RETURN_VALUE
            | O::RAISE_VARARGS
            | O::RERAISE
            | O::JUMP
            | O::JUMP_NO_INTERRUPT
            | O::JUMP_FORWARD
            | O::JUMP_BACKWARD
            | O::JUMP_BACKWARD_NO_INTERRUPT
    )
}

pub fn is_uncond_jump(op: u16) -> bool {
    matches!(op, O::JUMP | O::JUMP_NO_INTERRUPT | O::JUMP_FORWARD | O::JUMP_BACKWARD | O::JUMP_BACKWARD_NO_INTERRUPT)
}

pub fn is_cond_jump(op: u16) -> bool {
    matches!(op, O::POP_JUMP_IF_FALSE | O::POP_JUMP_IF_TRUE | O::POP_JUMP_IF_NONE | O::POP_JUMP_IF_NOT_NONE)
}

pub fn is_setup(op: u16) -> bool {
    matches!(op, O::SETUP_FINALLY | O::SETUP_CLEANUP | O::SETUP_WITH)
}

/// Instructions that refer to a target block (jumps, SETUP_*, FOR_ITER, SEND, END_ASYNC_FOR).
pub fn has_target(op: u16) -> bool {
    is_uncond_jump(op)
        || is_cond_jump(op)
        || is_setup(op)
        || matches!(op, O::FOR_ITER | O::SEND | O::END_ASYNC_FOR | O::JUMP_IF_FALSE | O::JUMP_IF_TRUE)
}

impl Ir {
    pub fn new() -> Ir {
        Ir { blocks: vec![Block { depth: -1, ..Default::default() }], order: vec![0], cur: 0 }
    }
    pub fn new_label(&mut self) -> Label {
        self.blocks.push(Block { depth: -1, ..Default::default() });
        (self.blocks.len() - 1) as Label
    }
    /// Start emitting into `l` (appended to the layout).
    pub fn use_label(&mut self, l: Label) {
        self.order.push(l);
        self.cur = l;
    }
    pub fn emit(&mut self, i: Instr) {
        let cur = self.cur as usize;
        if let Some(last) = self.blocks[cur].instrs.last() {
            if is_terminator(last.op) {
                let l = self.new_label();
                self.use_label(l);
                self.blocks[l as usize].instrs.push(i);
                return;
            }
        }
        self.blocks[cur].instrs.push(i);
    }
    pub fn last_is_terminator(&self) -> bool {
        self.blocks[self.cur as usize].instrs.last().map(|i| is_terminator(i.op)).unwrap_or(false)
    }
    pub fn last_instr(&self) -> Option<&Instr> {
        self.blocks[self.cur as usize].instrs.last()
    }
    pub fn last_instr_mut(&mut self) -> Option<&mut Instr> {
        let c = self.cur as usize;
        self.blocks[c].instrs.last_mut()
    }
}
