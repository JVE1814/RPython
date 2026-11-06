//! Public wordcode -> internal instruction stream ("quickening", HotSpot-style bytecode
//! rewriting). Runs once per code object, on first execution.
//!
//! The public form of a code object is CPython 3.15 wordcode (`co_code`, with inline CACHE
//! units, relative jumps, the varint exception table and the location table). The
//! interpreter, the f_lineno jump analysis and the JIT all run on our internal form: 32-bit
//! words (`bytecode::Op` + 24-bit argument), absolute jump targets, one location and one
//! inline-cache slot per word, and a decoded exception table.
//!
//! The translation is instruction-by-instruction, driven by an abstract interpretation of
//! the operand stack. Every public stack slot is tracked as *real* (materialised in the
//! internal frame) or *phantom* (exists only in the public model). Phantom slots are the
//! second half of 3.15's two-slot iterators (`GET_ITER` pushes `[iter, NULL-or-index]`) and
//! the NULL of the send protocol (`[receiver, NULL, value]`), plus values that the internal
//! `ForIter`/`Send` already removed on their exit edges. This keeps the internal stack layout
//! (and thus every interpreter/JIT fast path) exactly as before; exception-table depths are
//! converted from public to internal depths with the same model.

use super::wc::{self, op, Instr, Position};
use rpy_compiler::bytecode::{self as bc, ExceptionEntry, Location, Op};

/// Constants the internal code needs in addition to co_consts (appended after them).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Extra {
    Int(i64),
    None,
    EmptyStr,
    True,
    False,
}

pub struct Input<'a> {
    pub code: &'a [u8],
    pub exceptiontable: &'a [u8],
    /// one position per code unit
    pub positions: &'a [Position],
    pub nlocalsplus: usize,
    pub nconsts: usize,
    pub nnames: usize,
    pub stacksize: u32,
}

pub struct Output {
    pub code: Vec<u32>,
    pub locations: Vec<Location>,
    pub exception_table: Vec<ExceptionEntry>,
    pub extra_consts: Vec<Extra>,
    /// per internal word: code unit of the public instruction it implements
    pub pub_of_internal: Vec<u32>,
    /// per code unit (+ one past the end): first internal word of the public instruction
    /// that contains the unit
    pub internal_of_pub: Vec<u32>,
    /// RETURN_GENERATOR is preceded by real work (genexpr `iter()`): the runtime must run
    /// the frame up to the internal `ReturnGenerator` at call time.
    pub eager_prologue: bool,
    /// internal pc where a generator's body starts (after RETURN_GENERATOR), 0 otherwise
    pub gen_start: u32,
}

/// Public stack slot: true = materialised internally.
type St = Vec<bool>;

struct Emit {
    ops: Vec<(Op, u32, Option<u32>)>,
}
impl Emit {
    fn op(&mut self, o: Op, a: u32) {
        self.ops.push((o, a, None));
    }
    fn jump(&mut self, o: Op, target: u32) {
        self.ops.push((o, 0, Some(target)));
    }
}

/// Result of one abstract step.
struct Step {
    fall: Option<St>,
    jump: Option<(u32, St)>,
}

/// (pops, pushes) of a base instruction on the public stack (fall-through edge).
fn stack_io(o: u8, arg: u32) -> Option<(u32, u32)> {
    use op::*;
    Some(match o {
        NOP | NOT_TAKEN | RESUME | DELETE_FAST | DELETE_DEREF | DELETE_NAME | DELETE_GLOBAL | MAKE_CELL
        | COPY_FREE_VARS | SETUP_ANNOTATIONS | JUMP_FORWARD | JUMP_BACKWARD | JUMP_BACKWARD_NO_INTERRUPT => (0, 0),
        POP_TOP | END_FOR | STORE_FAST | STORE_DEREF | STORE_NAME | STORE_GLOBAL | DELETE_ATTR | POP_EXCEPT
        | POP_JUMP_IF_FALSE | POP_JUMP_IF_TRUE | POP_JUMP_IF_NONE | POP_JUMP_IF_NOT_NONE | EXIT_INIT_CHECK => (1, 0),
        POP_ITER | STORE_FAST_STORE_FAST | STORE_ATTR | DELETE_SUBSCR | END_ASYNC_FOR => (2, 0),
        STORE_SUBSCR => (3, 0),
        STORE_SLICE => (4, 0),
        PUSH_NULL | LOAD_CONST | LOAD_SMALL_INT | LOAD_COMMON_CONSTANT | LOAD_FAST | LOAD_FAST_BORROW | LOAD_FAST_CHECK
        | LOAD_FAST_AND_CLEAR | LOAD_DEREF | LOAD_NAME | LOAD_LOCALS | LOAD_BUILD_CLASS | RETURN_GENERATOR => (0, 1),
        LOAD_FAST_LOAD_FAST | LOAD_FAST_BORROW_LOAD_FAST_BORROW => (0, 2),
        LOAD_GLOBAL => (0, 1 + (arg & 1)),
        STORE_FAST_LOAD_FAST | LOAD_FROM_DICT_OR_DEREF | LOAD_FROM_DICT_OR_GLOBALS | UNARY_INVERT | UNARY_NEGATIVE
        | UNARY_NOT | TO_BOOL | FORMAT_SIMPLE | CONVERT_VALUE | GET_AITER | GET_AWAITABLE | MAKE_FUNCTION
        | CALL_INTRINSIC_1 | YIELD_VALUE => (1, 1),
        LOAD_ATTR => (1, 1 + (arg & 1)),
        LOAD_SUPER_ATTR => (3, 1 + (arg & 1)),
        LOAD_SPECIAL | GET_ITER | GET_ANEXT | GET_LEN | MATCH_MAPPING | MATCH_SEQUENCE | PUSH_EXC_INFO
        | IMPORT_FROM => (1, 2),
        MATCH_KEYS => (2, 3),
        MATCH_CLASS | BINARY_SLICE => (3, 1),
        BINARY_OP | COMPARE_OP | IS_OP | CONTAINS_OP | FORMAT_WITH_SPEC | CALL_INTRINSIC_2 | BUILD_TEMPLATE
        | SET_FUNCTION_ATTRIBUTE | IMPORT_NAME => (2, 1),
        CHECK_EXC_MATCH | CHECK_EG_MATCH => (2, 2),
        BUILD_SLICE => (arg, 1),
        BUILD_TUPLE | BUILD_LIST | BUILD_SET | BUILD_STRING => (arg, 1),
        BUILD_MAP => (arg.checked_mul(2)?, 1),
        BUILD_INTERPOLATION => (2 + (arg & 1), 1),
        LIST_APPEND | SET_ADD | LIST_EXTEND | SET_UPDATE | DICT_UPDATE | DICT_MERGE => (1, 0),
        MAP_ADD => (2, 0),
        UNPACK_SEQUENCE => (1, arg),
        UNPACK_EX => (1, (arg & 0xff) + (arg >> 8) + 1),
        CALL => (arg.checked_add(2)?, 1),
        CALL_KW => (arg.checked_add(3)?, 1),
        CALL_FUNCTION_EX => (4, 1),
        RETURN_VALUE => (1, 0),
        RAISE_VARARGS => (arg, 0),
        RERAISE => (arg.checked_add(1)?, 0),
        SEND => (3, 3),
        FOR_ITER => (2, 3),
        END_SEND => (3, 1),
        CLEANUP_THROW => (4, 3),
        WITH_EXCEPT_START => (5, 6),
        COPY => (arg, arg + 1),
        SWAP => (arg, arg),
        _ => return None,
    })
}

fn is_terminator(o: u8) -> bool {
    use op::*;
    matches!(o, RETURN_VALUE | RAISE_VARARGS | RERAISE | JUMP_FORWARD | JUMP_BACKWARD | JUMP_BACKWARD_NO_INTERRUPT)
}

/// Is the NULL pushed by the PUSH_NULL at `idx` consumed in the null position of an
/// iterator pair / send triple? Scans the straight-line code that follows.
fn null_is_phantom(instrs: &[Instr], idx: usize) -> bool {
    use op::*;
    let mut d: u32 = 1; // depth of the NULL, 1 = top of stack
    for ins in instrs.iter().skip(idx + 1).take(32) {
        let Some(o) = ins.op else { return false };
        match o {
            FOR_ITER | POP_ITER if d == 1 => return true,
            SEND | END_SEND if d == 2 => return true,
            CLEANUP_THROW if d == 3 => return true,
            COPY => {
                d += 1;
                continue;
            }
            SWAP => {
                if d == 1 {
                    d = ins.arg;
                } else if d == ins.arg {
                    d = 1;
                }
                continue;
            }
            _ => {}
        }
        let Some((pops, pushes)) = stack_io(o, ins.arg) else { return false };
        if pops >= d {
            return false;
        }
        d = d + pushes - pops;
        if is_terminator(o) {
            return false;
        }
    }
    false
}

fn reals(s: &[bool]) -> u32 {
    s.iter().filter(|&&r| r).count() as u32
}

struct Ctx<'a> {
    inp: &'a Input<'a>,
    instrs: Vec<Instr>,
    eager_prologue: bool,
    extras: Vec<Extra>,
}

impl<'a> Ctx<'a> {
    fn extra(&mut self, e: Extra) -> u32 {
        let i = match self.extras.iter().position(|x| *x == e) {
            Some(i) => i,
            None => {
                self.extras.push(e);
                self.extras.len() - 1
            }
        };
        (self.inp.nconsts + i) as u32
    }

    /// Abstract execution of instruction `k` from state `s`; emits internal ops into `em`.
    fn step(&mut self, k: usize, mut s: St, em: &mut Emit) -> Result<Step, String> {
        use op::*;
        let ins = self.instrs[k];
        let o = ins.op.ok_or_else(|| format!("unknown opcode {}", ins.raw))?;
        let arg = ins.arg;
        let at = || format!(" ({} at offset {})", o, ins.offset * 2);
        let (pops, _) = stack_io(o, arg).ok_or_else(|| format!("unknown opcode {}", ins.raw))?;
        if (pops as usize) > s.len() {
            return Err(format!("stack underflow{}", at()));
        }
        let n = s.len();
        // all consumed slots must be real, except where an instruction says otherwise
        let need_real = |s: &St, cnt: usize| -> Result<(), String> {
            if s[s.len() - cnt..].iter().all(|&r| r) { Ok(()) } else { Err(format!("unexpected NULL slot{}", at())) }
        };
        let local = |i: u32| -> Result<u32, String> {
            if (i as usize) < self.inp.nlocalsplus { Ok(i) } else { Err(format!("bad local index{}", at())) }
        };
        let name = |i: u32| -> Result<u32, String> {
            if (i as usize) < self.inp.nnames { Ok(i) } else { Err(format!("bad name index{}", at())) }
        };
        let mut jump: Option<(u32, St)> = None;
        let target = wc::jump_target(&ins);
        macro_rules! simple {
            ($op:expr, $a:expr, $pops:expr, $pushes:expr) => {{
                need_real(&s, $pops)?;
                em.op($op, $a);
                s.truncate(n - $pops);
                for _ in 0..$pushes {
                    s.push(true);
                }
            }};
        }
        match o {
            NOP => em.op(Op::Nop, 0),
            NOT_TAKEN => {}
            RESUME => em.op(Op::Resume, arg & 3),
            POP_TOP | END_FOR => {
                if s[n - 1] {
                    em.op(Op::PopTop, 0);
                }
                s.pop();
            }
            POP_ITER => {
                for _ in 0..2 {
                    if s.pop() == Some(true) {
                        em.op(Op::PopTop, 0);
                    }
                }
            }
            END_SEND => {
                need_real(&s, 1)?;
                for r in [s[n - 2], s[n - 3]] {
                    if r {
                        em.op(Op::Swap, 2);
                        em.op(Op::PopTop, 0);
                    }
                }
                s.truncate(n - 3);
                s.push(true);
            }
            PUSH_NULL => {
                if null_is_phantom(&self.instrs, k) {
                    s.push(false);
                } else {
                    em.op(Op::PushNull, 0);
                    s.push(true);
                }
            }
            COPY => {
                let c = arg as usize;
                if c == 0 || c > n {
                    return Err(format!("bad COPY{}", at()));
                }
                if s[n - c] {
                    em.op(Op::Copy, reals(&s[n - c..]));
                    s.push(true);
                } else {
                    s.push(false);
                }
            }
            SWAP => {
                let c = arg as usize;
                if c < 2 || c > n {
                    return Err(format!("bad SWAP{}", at()));
                }
                let win: Vec<(usize, bool)> = (0..c).map(|i| (i, s[n - c + i])).collect();
                let mut after = win.clone();
                after.swap(0, c - 1);
                let before_ids: Vec<usize> = win.iter().filter(|x| x.1).map(|x| x.0).collect();
                let after_ids: Vec<usize> = after.iter().filter(|x| x.1).map(|x| x.0).collect();
                emit_permutation(em, &before_ids, &after_ids);
                s.swap(n - c, n - 1);
            }
            LOAD_CONST => {
                if arg as usize >= self.inp.nconsts {
                    return Err(format!("bad constant index{}", at()));
                }
                simple!(Op::LoadConst, arg, 0, 1)
            }
            LOAD_SMALL_INT => {
                let c = self.extra(Extra::Int(arg as i64));
                simple!(Op::LoadConst, c, 0, 1)
            }
            LOAD_COMMON_CONSTANT => match arg {
                0..=6 => simple!(Op::LoadCommonConstant, arg, 0, 1),
                7..=11 => {
                    let e = [Extra::None, Extra::EmptyStr, Extra::True, Extra::False, Extra::Int(-1)][arg as usize - 7];
                    let c = self.extra(e);
                    simple!(Op::LoadConst, c, 0, 1)
                }
                _ => return Err(format!("bad common constant{}", at())),
            },
            LOAD_FAST | LOAD_FAST_BORROW => simple!(Op::LoadFast, local(arg)?, 0, 1),
            LOAD_FAST_CHECK => simple!(Op::LoadFastCheck, local(arg)?, 0, 1),
            LOAD_FAST_AND_CLEAR => simple!(Op::LoadFastAndClear, local(arg)?, 0, 1),
            LOAD_FAST_LOAD_FAST | LOAD_FAST_BORROW_LOAD_FAST_BORROW => {
                em.op(Op::LoadFast, local(arg >> 4)?);
                em.op(Op::LoadFast, local(arg & 15)?);
                s.push(true);
                s.push(true);
            }
            STORE_FAST => simple!(Op::StoreFast, local(arg)?, 1, 0),
            STORE_FAST_LOAD_FAST => {
                need_real(&s, 1)?;
                em.op(Op::StoreFast, local(arg >> 4)?);
                em.op(Op::LoadFast, local(arg & 15)?);
            }
            STORE_FAST_STORE_FAST => {
                need_real(&s, 2)?;
                em.op(Op::StoreFast, local(arg >> 4)?);
                em.op(Op::StoreFast, local(arg & 15)?);
                s.truncate(n - 2);
            }
            DELETE_FAST => em.op(Op::DeleteFast, local(arg)?),
            MAKE_CELL => em.op(Op::MakeCell, local(arg)?),
            COPY_FREE_VARS => {
                if arg as usize > self.inp.nlocalsplus {
                    return Err(format!("bad COPY_FREE_VARS{}", at()));
                }
                em.op(Op::CopyFreeVars, arg)
            }
            LOAD_DEREF => simple!(Op::LoadDeref, local(arg)?, 0, 1),
            STORE_DEREF => simple!(Op::StoreDeref, local(arg)?, 1, 0),
            DELETE_DEREF => em.op(Op::DeleteDeref, local(arg)?),
            LOAD_FROM_DICT_OR_DEREF => simple!(Op::LoadFromDictOrDeref, local(arg)?, 1, 1),
            LOAD_LOCALS => simple!(Op::LoadLocals, 0, 0, 1),
            LOAD_NAME => simple!(Op::LoadName, name(arg)?, 0, 1),
            STORE_NAME => simple!(Op::StoreName, name(arg)?, 1, 0),
            DELETE_NAME => em.op(Op::DeleteName, name(arg)?),
            LOAD_GLOBAL => {
                em.op(Op::LoadGlobal, name(arg >> 1)?);
                s.push(true);
                if arg & 1 != 0 {
                    em.op(Op::PushNull, 0);
                    s.push(true);
                }
            }
            STORE_GLOBAL => simple!(Op::StoreGlobal, name(arg)?, 1, 0),
            DELETE_GLOBAL => em.op(Op::DeleteGlobal, name(arg)?),
            LOAD_FROM_DICT_OR_GLOBALS => simple!(Op::LoadFromDictOrGlobals, name(arg)?, 1, 1),
            LOAD_ATTR => {
                if arg & 1 != 0 {
                    simple!(Op::LoadMethod, name(arg >> 1)?, 1, 2)
                } else {
                    simple!(Op::LoadAttr, name(arg >> 1)?, 1, 1)
                }
            }
            STORE_ATTR => simple!(Op::StoreAttr, name(arg)?, 2, 0),
            DELETE_ATTR => simple!(Op::DeleteAttr, name(arg)?, 1, 0),
            LOAD_SUPER_ATTR => {
                name(arg >> 2)?;
                simple!(Op::LoadSuperAttr, arg, 3, 1 + (arg & 1) as usize)
            }
            LOAD_SPECIAL => {
                if arg > 3 {
                    return Err(format!("bad LOAD_SPECIAL argument{}", at()));
                }
                simple!(Op::LoadSpecial, arg, 1, 2)
            }
            BINARY_OP => match arg {
                0..=25 => simple!(Op::BinaryOp, arg, 2, 1),
                26 => simple!(Op::BinarySubscr, 0, 2, 1),
                _ => return Err(format!("bad BINARY_OP argument{}", at())),
            },
            UNARY_NEGATIVE => simple!(Op::UnaryNegative, 0, 1, 1),
            UNARY_INVERT => simple!(Op::UnaryInvert, 0, 1, 1),
            UNARY_NOT => simple!(Op::UnaryNot, 0, 1, 1),
            TO_BOOL => simple!(Op::ToBool, 0, 1, 1),
            COMPARE_OP => {
                if arg >> 5 > 5 {
                    return Err(format!("bad COMPARE_OP argument{}", at()));
                }
                simple!(Op::CompareOp, (arg >> 5) | (arg & 16), 2, 1)
            }
            IS_OP => simple!(Op::IsOp, (arg != 0) as u32, 2, 1),
            CONTAINS_OP => simple!(Op::ContainsOp, (arg != 0) as u32, 2, 1),
            STORE_SUBSCR => simple!(Op::StoreSubscr, 0, 3, 0),
            DELETE_SUBSCR => simple!(Op::DeleteSubscr, 0, 2, 0),
            BINARY_SLICE => simple!(Op::BinarySlice, 0, 3, 1),
            STORE_SLICE => simple!(Op::StoreSlice, 0, 4, 0),
            BUILD_SLICE => {
                if arg != 2 && arg != 3 {
                    return Err(format!("bad BUILD_SLICE argument{}", at()));
                }
                simple!(Op::BuildSlice, arg, arg as usize, 1)
            }
            BUILD_TUPLE => simple!(Op::BuildTuple, arg, arg as usize, 1),
            BUILD_LIST => simple!(Op::BuildList, arg, arg as usize, 1),
            BUILD_SET => simple!(Op::BuildSet, arg, arg as usize, 1),
            BUILD_STRING => simple!(Op::BuildString, arg, arg as usize, 1),
            BUILD_MAP => simple!(Op::BuildMap, arg, 2 * arg as usize, 1),
            LIST_APPEND | SET_ADD | LIST_EXTEND | SET_UPDATE | DICT_UPDATE | DICT_MERGE | MAP_ADD => {
                let npop = if o == MAP_ADD { 2 } else { 1 };
                let depth = arg as usize;
                need_real(&s, npop)?;
                let rest = n - npop;
                if depth == 0 || depth > rest || !s[rest - depth] {
                    return Err(format!("bad container depth{}", at()));
                }
                let idepth = reals(&s[rest - depth..rest]);
                let iop = match o {
                    LIST_APPEND => Op::ListAppend,
                    SET_ADD => Op::SetAdd,
                    LIST_EXTEND => Op::ListExtend,
                    SET_UPDATE => Op::SetUpdate,
                    DICT_UPDATE => Op::DictUpdate,
                    DICT_MERGE => Op::DictMerge,
                    _ => Op::MapAdd,
                };
                em.op(iop, idepth);
                s.truncate(rest);
            }
            UNPACK_SEQUENCE => simple!(Op::UnpackSequence, arg, 1, arg as usize),
            UNPACK_EX => simple!(Op::UnpackEx, arg, 1, ((arg & 0xff) + (arg >> 8) + 1) as usize),
            JUMP_FORWARD | JUMP_BACKWARD | JUMP_BACKWARD_NO_INTERRUPT => {
                let t = target.unwrap();
                em.jump(Op::Jump, t);
                jump = Some((t, s.clone()));
            }
            POP_JUMP_IF_FALSE | POP_JUMP_IF_TRUE | POP_JUMP_IF_NONE | POP_JUMP_IF_NOT_NONE => {
                need_real(&s, 1)?;
                let iop = match o {
                    POP_JUMP_IF_FALSE => Op::PopJumpIfFalse,
                    POP_JUMP_IF_TRUE => Op::PopJumpIfTrue,
                    POP_JUMP_IF_NONE => Op::PopJumpIfNone,
                    _ => Op::PopJumpIfNotNone,
                };
                s.pop();
                let t = target.unwrap();
                em.jump(iop, t);
                jump = Some((t, s.clone()));
            }
            GET_ITER => {
                need_real(&s, 1)?;
                em.op(if arg == 1 { Op::GetYieldFromIter } else { Op::GetIter }, 0);
                s.push(false);
            }
            FOR_ITER => {
                if s[n - 1] || !s[n - 2] {
                    return Err(format!("FOR_ITER needs an iterator pair{}", at()));
                }
                let t = target.unwrap();
                em.jump(Op::ForIter, t);
                let mut js = s[..n - 2].to_vec();
                js.extend([false, false, false]);
                jump = Some((t, js));
                s.push(true);
            }
            SEND => {
                if !s[n - 1] || s[n - 2] || !s[n - 3] {
                    return Err(format!("SEND needs [receiver, NULL, value]{}", at()));
                }
                let t = target.unwrap();
                em.jump(Op::Send, t);
                let mut js = s[..n - 3].to_vec();
                js.extend([false, false, true]);
                jump = Some((t, js));
            }
            YIELD_VALUE => simple!(Op::YieldValue, arg, 1, 1),
            RETURN_VALUE => simple!(Op::ReturnValue, 0, 1, 0),
            RETURN_GENERATOR => {
                if self.eager_prologue {
                    em.op(Op::ReturnGenerator, 0);
                }
                s.push(false);
            }
            CALL => simple!(Op::Call, arg, arg as usize + 2, 1),
            CALL_KW => simple!(Op::CallKw, arg, arg as usize + 3, 1),
            CALL_FUNCTION_EX => {
                // [func, self_or_null, args, kwargs_or_null]: the NULLs are real values
                simple!(Op::CallFunctionEx, 0, 4, 1)
            }
            CALL_INTRINSIC_1 => {
                let i = match arg {
                    1..=3 => arg,
                    4 => bc::Intrinsic1::AsyncGenWrap as u32,
                    5..=11 => arg - 1,
                    _ => return Err(format!("bad intrinsic{}", at())),
                };
                simple!(Op::CallIntrinsic1, i, 1, 1)
            }
            CALL_INTRINSIC_2 => {
                if !(1..=5).contains(&arg) {
                    return Err(format!("bad intrinsic{}", at()));
                }
                simple!(Op::CallIntrinsic2, arg, 2, 1)
            }
            MAKE_FUNCTION => simple!(Op::MakeFunction, 0, 1, 1),
            SET_FUNCTION_ATTRIBUTE => {
                if ![1, 2, 4, 8, 16].contains(&arg) {
                    return Err(format!("bad SET_FUNCTION_ATTRIBUTE flag{}", at()));
                }
                simple!(Op::SetFunctionAttribute, arg, 2, 1)
            }
            LOAD_BUILD_CLASS => simple!(Op::LoadBuildClass, 0, 0, 1),
            SETUP_ANNOTATIONS => em.op(Op::SetupAnnotations, 0),
            RAISE_VARARGS => {
                if arg > 2 {
                    return Err(format!("bad RAISE_VARARGS argument{}", at()));
                }
                simple!(Op::RaiseVarargs, arg, arg as usize, 0)
            }
            RERAISE => {
                need_real(&s, 1)?;
                // the n values below the exception stay on the stack (the handler search
                // truncates to its depth); the internal Reraise only pops the exception
                // (bc-compiler fix: Reraise 1 popped `prev` for RERAISE 2 in `with`)
                em.op(Op::Reraise, 0);
                s.truncate(n - 1 - arg as usize);
            }
            PUSH_EXC_INFO => simple!(Op::PushExcInfo, 0, 1, 2),
            POP_EXCEPT => simple!(Op::PopExcept, 0, 1, 0),
            CHECK_EXC_MATCH => simple!(Op::CheckExcMatch, 0, 2, 2),
            CHECK_EG_MATCH => simple!(Op::CheckEgMatch, 0, 2, 2),
            WITH_EXCEPT_START => simple!(Op::WithExceptStart, 0, 5, 6),
            END_ASYNC_FOR => simple!(Op::EndAsyncFor, 0, 2, 0),
            CLEANUP_THROW => {
                if !s[n - 1] || !s[n - 2] || s[n - 3] || !s[n - 4] {
                    return Err(format!("CLEANUP_THROW needs [iter, NULL, value, exc]{}", at()));
                }
                em.op(Op::CleanupThrow, 0);
                em.op(Op::Swap, 2);
                em.op(Op::PopTop, 0);
                s.truncate(n - 4);
                s.extend([false, false, true]);
            }
            GET_AITER => simple!(Op::GetAiter, 0, 1, 1),
            GET_ANEXT => {
                need_real(&s, 1)?;
                em.op(Op::GetAnext, 0);
                s.push(true);
            }
            GET_AWAITABLE => simple!(Op::GetAwaitable, arg, 1, 1),
            GET_LEN => {
                need_real(&s, 1)?;
                em.op(Op::GetLen, 0);
                s.push(true);
            }
            IMPORT_NAME => {
                let nm = name(arg >> 2)?;
                let bits = match arg & 3 {
                    0 => 1 << 24,
                    1 => 1 << 23,
                    _ => 0,
                };
                simple!(Op::ImportName, nm | bits, 2, 1)
            }
            IMPORT_FROM => {
                need_real(&s, 1)?;
                em.op(Op::ImportFrom, name(arg)?);
                s.push(true);
            }
            FORMAT_SIMPLE => simple!(Op::FormatSimple, 0, 1, 1),
            FORMAT_WITH_SPEC => simple!(Op::FormatWithSpec, 0, 2, 1),
            CONVERT_VALUE => {
                if !(1..=3).contains(&arg) {
                    return Err(format!("bad CONVERT_VALUE argument{}", at()));
                }
                simple!(Op::ConvertValue, arg, 1, 1)
            }
            BUILD_TEMPLATE => simple!(Op::BuildTemplate, 0, 2, 1),
            BUILD_INTERPOLATION => simple!(Op::BuildInterpolation, arg, 2 + (arg & 1) as usize, 1),
            MATCH_CLASS => simple!(Op::MatchClass, arg, 3, 1),
            MATCH_MAPPING | MATCH_SEQUENCE => {
                need_real(&s, 1)?;
                em.op(if o == MATCH_MAPPING { Op::MatchMapping } else { Op::MatchSequence }, 0);
                s.push(true);
            }
            MATCH_KEYS => {
                need_real(&s, 2)?;
                em.op(Op::MatchKeys, 0);
                s.push(true);
            }
            _ => return Err(format!("unsupported opcode {}{}", ins.raw, at())),
        }
        let fall = if is_terminator(o) { None } else { Some(s) };
        Ok(Step { fall, jump })
    }
}

/// Emit internal Swaps that turn the internal stack window `before` (bottom -> top) into
/// `after` (same ids, permuted). Uses the top of stack as the exchange slot.
fn emit_permutation(em: &mut Emit, before: &[usize], after: &[usize]) {
    let len = before.len();
    if len < 2 || before == after {
        return;
    }
    let mut cur = before.to_vec();
    let swap_top = |cur: &mut Vec<usize>, em: &mut Emit, j: usize| {
        // swap top with index j (bottom-based)
        em.op(Op::Swap, (len - j) as u32);
        cur.swap(len - 1, j);
    };
    for pos in 0..len - 1 {
        let want = after[pos];
        if cur[pos] == want {
            continue;
        }
        let j = cur.iter().position(|&x| x == want).unwrap();
        if j != len - 1 {
            swap_top(&mut cur, em, j);
        }
        swap_top(&mut cur, em, pos);
    }
}

fn location_of(p: &Position) -> Location {
    match p.lineno {
        None => Location { lineno: 0, end_lineno: 0, col: u32::MAX, end_col: u32::MAX },
        Some(l) => Location {
            lineno: l,
            end_lineno: p.end_lineno.unwrap_or(l),
            col: p.col.unwrap_or(u32::MAX),
            end_col: p.end_col.unwrap_or(u32::MAX),
        },
    }
}

/// Find the exception-table entry covering code unit `off` (entries are sorted).
fn handler_for(t: &[wc::ExcEntry], off: u32) -> Option<&wc::ExcEntry> {
    t.iter().find(|e| e.start <= off && off < e.end)
}

pub fn translate(inp: &Input) -> Result<Output, String> {
    if inp.code.len() % 2 != 0 {
        return Err("co_code length must be even".into());
    }
    let ncu = inp.code.len() / 2;
    let instrs = wc::instructions(inp.code);
    let exc = wc::decode_exception_table(inp.exceptiontable).ok_or("malformed exception table")?;
    // instruction index by code unit (CACHE/EXTENDED_ARG units map to their instruction)
    let mut index_of = vec![usize::MAX; ncu + 1];
    for (k, ins) in instrs.iter().enumerate() {
        for u in ins.start..ins.next().min(ncu as u32) {
            index_of[u as usize] = k;
        }
    }
    index_of[ncu] = instrs.len();
    let lookup = |cu: u32| -> Result<usize, String> {
        let k = *index_of.get(cu as usize).ok_or_else(|| format!("jump target {} out of range", cu * 2))?;
        if k == usize::MAX || (k < instrs.len() && instrs[k].start != cu && instrs[k].offset != cu) {
            return Err(format!("jump into the middle of an instruction at {}", cu * 2));
        }
        Ok(k)
    };
    // generators: is there real work before RETURN_GENERATOR?
    let mut eager_prologue = false;
    for ins in &instrs {
        match ins.op {
            Some(op::RETURN_GENERATOR) => break,
            Some(op::COPY_FREE_VARS | op::MAKE_CELL | op::RESUME | op::NOP | op::NOT_TAKEN) => {}
            _ => {
                if instrs.iter().any(|i| i.op == Some(op::RETURN_GENERATOR)) {
                    eager_prologue = true;
                }
                break;
            }
        }
    }
    let mut cx = Ctx { inp, instrs: instrs.clone(), eager_prologue, extras: Vec::new() };

    // ---- pass 1: abstract interpretation (entry state of every reachable instruction)
    let n = instrs.len();
    let mut state: Vec<Option<St>> = vec![None; n];
    let mut work: Vec<(usize, St)> = Vec::new();
    if n > 0 {
        state[0] = Some(Vec::new());
        work.push((0, Vec::new()));
    }
    let merge = |state: &mut Vec<Option<St>>, work: &mut Vec<(usize, St)>, k: usize, s: St, lenient: bool| -> Result<(), String> {
        if k >= n {
            return Err("control flow falls off the end of the code".into());
        }
        if s.len() > inp.stacksize as usize {
            return Err(format!("stack depth exceeds co_stacksize at offset {}", instrs[k].offset * 2));
        }
        match &state[k] {
            Some(old) => {
                // handler entries may be reached from instructions whose stacks differ below
                // the handler depth (CPython tables cover e.g. a loop epilogue too): the
                // handler code only works on the top, keep the first state
                if *old != s && !lenient {
                    return Err(format!("inconsistent stack at offset {}", instrs[k].offset * 2));
                }
            }
            None => {
                state[k] = Some(s.clone());
                work.push((k, s));
            }
        }
        Ok(())
    };
    while let Some((k, s)) = work.pop() {
        // exception edge
        if let Some(h) = handler_for(&exc, instrs[k].offset) {
            let mut hs = s[..(h.depth as usize).min(s.len())].to_vec();
            if h.lasti {
                hs.push(true);
            }
            hs.push(true);
            let t = lookup(h.target)?;
            merge(&mut state, &mut work, t, hs, true)?;
        }
        let mut em = Emit { ops: Vec::new() };
        let st = cx.step(k, s, &mut em)?;
        if let Some((t, js)) = st.jump {
            let tk = lookup(t)?;
            merge(&mut state, &mut work, tk, js, false)?;
        }
        if let Some(fs) = st.fall {
            merge(&mut state, &mut work, k + 1, fs, false)?;
        }
    }

    // ---- pass 2: emission in public order
    let mut code: Vec<u32> = Vec::with_capacity(n + n / 4);
    let mut locations: Vec<Location> = Vec::with_capacity(code.capacity());
    let mut pub_of_internal: Vec<u32> = Vec::with_capacity(code.capacity());
    let mut first_of_instr: Vec<u32> = vec![0; n + 1];
    let mut fixups: Vec<(usize, u32)> = Vec::new(); // (index of the op word, public target)
    let mut gen_start = 0u32;
    for k in 0..n {
        first_of_instr[k] = code.len() as u32;
        let Some(s) = state[k].clone() else { continue };
        let ins = instrs[k];
        let mut em = Emit { ops: Vec::new() };
        cx.step(k, s, &mut em)?;
        let loc = location_of(inp.positions.get(ins.offset as usize).unwrap_or(&Position::default()));
        for (o, a, t) in em.ops {
            if t.is_some() {
                fixups.push((code.len(), t.unwrap()));
                // jump targets are patched below; reserve no ExtendedArg (internal targets < 2^24)
                code.push(bc::encode(o, 0));
                locations.push(loc);
                pub_of_internal.push(ins.offset);
                continue;
            }
            if a >= 1 << 24 {
                code.push(bc::encode(Op::ExtendedArg, a >> 24));
                locations.push(loc);
                pub_of_internal.push(ins.offset);
            }
            code.push(bc::encode(o, a & 0xff_ffff));
            locations.push(loc);
            pub_of_internal.push(ins.offset);
        }
        if ins.op == Some(op::RETURN_GENERATOR) {
            gen_start = code.len() as u32;
        }
    }
    first_of_instr[n] = code.len() as u32;
    if code.len() >= 1 << 24 {
        return Err("code too large".into());
    }
    let ipc_of = |cu: u32| -> Result<u32, String> { Ok(first_of_instr[lookup(cu)?]) };
    for (i, t) in fixups {
        let it = ipc_of(t)?;
        code[i] |= it << 8;
    }
    let mut internal_of_pub = vec![0u32; ncu + 1];
    for (u, slot) in internal_of_pub.iter_mut().enumerate() {
        let k = index_of[u];
        *slot = if k == usize::MAX { code.len() as u32 } else { first_of_instr[k] };
    }

    // ---- exception table: one internal entry per run of words with the same handler
    let mut exception_table: Vec<ExceptionEntry> = Vec::new();
    for k in 0..n {
        let Some(s) = &state[k] else { continue };
        let Some(h) = handler_for(&exc, instrs[k].offset) else { continue };
        let (a, b) = (first_of_instr[k], first_of_instr[k + 1]);
        if a == b {
            continue;
        }
        let e = ExceptionEntry {
            start: a,
            end: b,
            target: ipc_of(h.target)?,
            depth: reals(&s[..(h.depth as usize).min(s.len())]),
            lasti: h.lasti,
        };
        match exception_table.last_mut() {
            Some(l) if l.end == e.start && l.target == e.target && l.depth == e.depth && l.lasti == e.lasti => l.end = e.end,
            _ => exception_table.push(e),
        }
    }

    Ok(Output {
        code,
        locations,
        exception_table,
        extra_consts: cx.extras,
        pub_of_internal,
        internal_of_pub,
        eager_prologue,
        gen_start,
    })
}
