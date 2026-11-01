//! Bytecode analysis for the JIT: instruction decoding (ExtendedArg folded), static
//! value-stack depth at every instruction, jump targets.

use rpy_compiler::bytecode::{self as bc, Op};

#[derive(Clone, Copy, Debug)]
pub struct Instr {
    /// index of the first code unit (ExtendedArg prefix or the op itself)
    pub start: u32,
    /// index of the op (what the interpreter stores in f.lasti)
    pub pc: u32,
    pub op: Op,
    pub arg: u32,
}

/// Decode `code` into instructions.
pub fn decode(code: &[u32]) -> Vec<Instr> {
    let mut v = Vec::with_capacity(code.len());
    let mut ext = 0u32;
    let mut start = 0usize;
    for (i, &w) in code.iter().enumerate() {
        let (op, a) = bc::decode(w);
        let arg = a | ext;
        if op == Op::ExtendedArg {
            ext = arg << 24;
            continue;
        }
        v.push(Instr { start: start as u32, pc: i as u32, op, arg });
        ext = 0;
        start = i + 1;
    }
    v
}

/// Stack effect: (fallthrough delta, jump delta).
pub fn effect(op: Op, arg: u32) -> (i32, i32) {
    use Op::*;
    let a = arg as i32;
    let d = match op {
        Nop | Resume | ExtendedArg | Swap | DeleteFast | MakeCell | CopyFreeVars | DeleteDeref | LoadFromDictOrDeref
        | DeleteName | DeleteGlobal | LoadFromDictOrGlobals | LoadAttr | UnaryNegative | UnaryPositive | UnaryInvert
        | UnaryNot | ToBool | GetIter | GetAiter | GetAwaitable | GetYieldFromIter | YieldValue | CallIntrinsic1
        | MakeFunction | CheckExcMatch | CheckEgMatch | SetupAnnotations | FormatSimple | ConvertValue => 0,
        PopTop | StoreFast | StoreDeref | StoreName | StoreGlobal | DeleteAttr | BinaryOp | CompareOp | IsOp
        | ContainsOp | BinarySubscr | ListAppend | SetAdd | ListExtend | SetUpdate | DictUpdate | DictMerge
        | CallIntrinsic2 | SetFunctionAttribute | PopExcept | ImportName | FormatWithSpec | BuildTemplate
        | CleanupThrow => -1,
        PushNull | Copy | LoadConst | LoadFast | LoadFastCheck | LoadFastAndClear | LoadDeref | LoadLocals | LoadName
        | LoadGlobal | LoadMethod | GetAnext | LoadBuildClass | PushExcInfo | WithExceptStart | ImportFrom
        | MatchMapping | MatchSequence | MatchKeys | GetLen | LoadCommonConstant => 1,
        StoreAttr | MapAdd | DeleteSubscr | BinarySlice | EndAsyncFor | MatchClass => -2,
        StoreSubscr | CallFunctionEx => -3,
        StoreSlice => -4,
        LoadSuperAttr => {
            if arg & 1 != 0 {
                -1
            } else {
                -2
            }
        }
        BuildSlice => 1 - a,
        BuildTuple | BuildList | BuildSet | BuildString => 1 - a,
        BuildMap => 1 - 2 * a,
        UnpackSequence => a - 1,
        UnpackEx => ((arg & 0xff) + (arg >> 8)) as i32,
        Call => -a - 1,
        CallKw => -a - 2,
        BeforeWith | BeforeAsyncWith => 2,
        BuildInterpolation => -1 - (arg & 1) as i32,
        ReturnValue => -1,
        RaiseVarargs => -a,
        Reraise => -1 - (arg != 0) as i32,
        // jumps
        Jump => 0,
        PopJumpIfFalse | PopJumpIfTrue | PopJumpIfNone | PopJumpIfNotNone => -1,
        JumpIfFalseOrPop | JumpIfTrueOrPop => return (-1, 0),
        ForIter => return (1, -1),
        Send => return (0, -1),
        Unused1 | Unused2 | Unused3 => 0,
    };
    (d, d)
}

pub fn is_terminator(op: Op) -> bool {
    matches!(op, Op::Jump | Op::ReturnValue | Op::RaiseVarargs | Op::Reraise)
}

pub struct Analysis {
    pub instrs: Vec<Instr>,
    /// code index (start or pc) -> instruction number
    pub index_of: Vec<u32>,
    /// stack depth before each instruction (-1: unreachable)
    pub depth: Vec<i32>,
    /// instruction is a jump target or handler entry
    pub is_target: Vec<bool>,
}

/// Compute stack depths; Err if inconsistent.
pub fn analyze(code: &[u32], handlers: &[bc::ExceptionEntry]) -> Result<Analysis, String> {
    let instrs = decode(code);
    let mut index_of = vec![u32::MAX; code.len() + 1];
    for (i, ins) in instrs.iter().enumerate() {
        for k in ins.start..=ins.pc {
            index_of[k as usize] = i as u32;
        }
    }
    index_of[code.len()] = instrs.len() as u32;
    let n = instrs.len();
    let mut depth = vec![-1i32; n];
    let mut is_target = vec![false; n];
    let mut work: Vec<(usize, i32)> = vec![(0, 0)];
    for h in handlers {
        let t = index_of[h.target as usize] as usize;
        if t < n {
            is_target[t] = true;
            work.push((t, h.depth as i32 + h.lasti as i32 + 1));
        }
    }
    while let Some((i, d)) = work.pop() {
        if i >= n {
            continue;
        }
        if depth[i] >= 0 {
            if depth[i] != d {
                return Err(format!("stack depth mismatch at {}: {} vs {}", instrs[i].pc, depth[i], d));
            }
            continue;
        }
        if d < 0 {
            return Err(format!("negative stack depth at {}", instrs[i].pc));
        }
        depth[i] = d;
        let ins = instrs[i];
        let (fall, jump) = effect(ins.op, ins.arg);
        if ins.op.is_jump() {
            let t = index_of[ins.arg as usize] as usize;
            is_target[t.min(n.saturating_sub(1))] = true;
            work.push((t, d + jump));
        }
        if !is_terminator(ins.op) {
            work.push((i + 1, d + fall));
        }
    }
    Ok(Analysis { instrs, index_of, depth, is_target })
}
