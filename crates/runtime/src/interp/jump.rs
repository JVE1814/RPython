//! `frame.f_lineno = n` from a trace function: validation and execution of the jump.
//!
//! The value stack of every instruction is described by an abstract interpretation
//! over the bytecode that records the *kind* of each stack entry (plain object,
//! iterator of a `for` loop, saved/handled exception of an `except` block, NULL
//! marker, `with` exit pair, ...). A jump is allowed when the target's stack is a
//! prefix of the current stack (the extra entries are popped) and only entries that
//! may be discarded are popped.

use crate::errors::*;
use crate::ffi::PyObject;
use crate::interp::frame::Frame;
use crate::object::*;
use crate::raise;
use crate::types::code;
use rpy_compiler::bytecode::{fast, Op};

pub const EV_CALL: u8 = 1;
pub const EV_LINE: u8 = 2;
pub const EV_RETURN: u8 = 3;
pub const EV_EXCEPTION: u8 = 4;
pub const EV_OPCODE: u8 = 5;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Object,
    /// iterator of a for loop (consumed by ForIter on exhaustion)
    Iter,
    /// previous handled exception saved by PushExcInfo
    ExcInfo,
    /// exception object of a handler
    Except,
    /// raising instruction index pushed for handlers with `lasti`
    Lasti,
    /// NULL (or maybe-NULL) marker of a call sequence
    Null,
    /// `with` exit callable / its self
    WithExit,
}

type Stack = Vec<Kind>;

fn decode(code: &[u32]) -> Vec<(usize, Op, u32)> {
    let mut v = Vec::with_capacity(code.len());
    let mut ext = 0u32;
    for (i, &w) in code.iter().enumerate() {
        let op = Op::from_u8((w & 0xff) as u8);
        let arg = (w >> 8) | ext;
        if op == Op::ExtendedArg {
            ext = arg << 24;
            v.push((i, op, 0));
            continue;
        }
        ext = 0;
        v.push((i, op, arg));
    }
    v
}

fn pop(s: &mut Stack, n: usize) {
    let l = s.len().saturating_sub(n);
    s.truncate(l);
}
fn push(s: &mut Stack, k: Kind, n: usize) {
    for _ in 0..n {
        s.push(k);
    }
}

/// Successor states of instruction (op, arg) with stack `s`: (fallthrough, jump).
fn step(op: Op, arg: u32, s: &Stack) -> (Option<Stack>, Option<(usize, Stack)>) {
    use Kind::*;
    let n = arg as usize;
    let mut f = s.clone();
    let mut jump: Option<(usize, Stack)> = None;
    match op {
        Op::Nop | Op::Resume | Op::ExtendedArg | Op::SetupAnnotations | Op::DeleteFast | Op::MakeCell | Op::CopyFreeVars => {}
        Op::DeleteDeref | Op::DeleteName | Op::DeleteGlobal | Op::Unused2 => {}
        Op::PopTop | Op::StoreFast | Op::StoreDeref | Op::StoreName | Op::StoreGlobal | Op::DeleteAttr | Op::Unused1 | Op::Unused3 => pop(&mut f, 1),
        Op::ListAppend | Op::SetAdd | Op::ListExtend | Op::SetUpdate | Op::DictUpdate | Op::DictMerge => pop(&mut f, 1),
        Op::PushNull => f.push(Null),
        Op::Copy => {
            let k = if n >= 1 && n <= f.len() { f[f.len() - n] } else { Object };
            f.push(if k == Null { Null } else { k });
        }
        Op::Swap => {
            let l = f.len();
            if n >= 2 && n <= l {
                f.swap(l - 1, l - n);
            }
        }
        Op::LoadConst | Op::LoadFast | Op::LoadFastCheck | Op::LoadFastAndClear | Op::LoadDeref | Op::LoadLocals | Op::LoadName => f.push(Object),
        Op::LoadGlobal | Op::LoadBuildClass | Op::LoadCommonConstant | Op::ImportFrom | Op::GetLen => f.push(Object),
        Op::MatchMapping | Op::MatchSequence | Op::MatchKeys | Op::GetAnext => f.push(Object),
        Op::LoadFromDictOrDeref | Op::LoadFromDictOrGlobals | Op::LoadAttr | Op::UnaryNegative | Op::UnaryPositive | Op::UnaryInvert => {
            pop(&mut f, 1);
            f.push(Object)
        }
        Op::UnaryNot | Op::ToBool | Op::FormatSimple | Op::ConvertValue | Op::CallIntrinsic1 | Op::MakeFunction | Op::GetAwaitable | Op::GetYieldFromIter | Op::YieldValue => {
            pop(&mut f, 1);
            f.push(Object)
        }
        Op::ReturnGenerator => {}
        Op::LoadMethod | Op::LoadSpecial => {
            pop(&mut f, 1);
            f.push(Object);
            f.push(Null);
        }
        Op::StoreAttr | Op::DeleteSubscr | Op::MapAdd => pop(&mut f, 2),
        Op::StoreSubscr => pop(&mut f, 3),
        Op::StoreSlice => pop(&mut f, 4),
        Op::LoadSuperAttr => {
            pop(&mut f, 3);
            f.push(Object);
            if arg & 1 != 0 {
                f.push(Null);
            }
        }
        Op::BinaryOp | Op::CompareOp | Op::IsOp | Op::ContainsOp | Op::BinarySubscr | Op::FormatWithSpec | Op::CallIntrinsic2 | Op::SetFunctionAttribute | Op::BuildTemplate | Op::ImportName => {
            pop(&mut f, 2);
            f.push(Object)
        }
        Op::BinarySlice => {
            pop(&mut f, 3);
            f.push(Object)
        }
        Op::BuildSlice | Op::BuildTuple | Op::BuildList | Op::BuildSet | Op::BuildString => {
            pop(&mut f, n);
            f.push(Object)
        }
        Op::BuildMap => {
            pop(&mut f, 2 * n);
            f.push(Object)
        }
        Op::BuildInterpolation => {
            pop(&mut f, 2 + (arg & 1) as usize);
            f.push(Object)
        }
        Op::UnpackSequence => {
            pop(&mut f, 1);
            push(&mut f, Object, n)
        }
        Op::UnpackEx => {
            pop(&mut f, 1);
            push(&mut f, Object, ((arg & 0xff) + (arg >> 8) + 1) as usize)
        }
        Op::Jump => return (None, Some((n, f))),
        Op::PopJumpIfFalse | Op::PopJumpIfTrue | Op::PopJumpIfNone | Op::PopJumpIfNotNone => {
            pop(&mut f, 1);
            jump = Some((n, f.clone()));
        }
        Op::JumpIfFalseOrPop | Op::JumpIfTrueOrPop => {
            jump = Some((n, f.clone()));
            pop(&mut f, 1);
        }
        Op::GetIter | Op::GetAiter => {
            pop(&mut f, 1);
            f.push(Iter)
        }
        Op::ForIter => {
            let mut j = f.clone();
            pop(&mut j, 1);
            jump = Some((n, j));
            f.push(Object);
        }
        Op::EndAsyncFor => pop(&mut f, 2),
        Op::Send => {
            let mut j = f.clone();
            pop(&mut j, 2);
            j.push(Object);
            jump = Some((n, j));
            pop(&mut f, 1);
            f.push(Object);
        }
        Op::CleanupThrow => {
            pop(&mut f, 3);
            push(&mut f, Object, 2)
        }
        Op::ReturnValue | Op::RaiseVarargs | Op::Reraise => return (None, None),
        Op::Call => {
            pop(&mut f, n + 2);
            f.push(Object)
        }
        Op::CallKw => {
            pop(&mut f, n + 3);
            f.push(Object)
        }
        Op::CallFunctionEx => {
            pop(&mut f, 4);
            f.push(Object)
        }
        Op::PushExcInfo => {
            pop(&mut f, 1);
            f.push(ExcInfo);
            f.push(Except);
        }
        Op::PopExcept => pop(&mut f, 1),
        Op::CheckExcMatch | Op::CheckEgMatch => {
            pop(&mut f, 1);
            f.push(Object)
        }
        Op::WithExceptStart => f.push(Object),
        Op::BeforeWith | Op::BeforeAsyncWith => {
            pop(&mut f, 1);
            f.push(WithExit);
            f.push(WithExit);
            f.push(Object);
        }
        Op::MatchClass => {
            pop(&mut f, 3);
            f.push(Object)
        }
    }
    (Some(f), jump)
}

/// Stack kinds at the start of every instruction (None = unreachable).
fn analyse(co: &PyObject) -> Vec<Option<Stack>> {
    let cd = code::code_data(co);
    let ins = decode(&cd.code);
    let n = ins.len();
    let mut st: Vec<Option<Stack>> = vec![None; n];
    let mut work: Vec<usize> = Vec::new();
    if n > 0 {
        st[0] = Some(Vec::new());
        work.push(0);
    }
    let mut set = |st: &mut Vec<Option<Stack>>, work: &mut Vec<usize>, i: usize, s: Stack| {
        if i < st.len() && st[i].is_none() {
            st[i] = Some(s);
            work.push(i);
        }
    };
    while let Some(i) = work.pop() {
        let s = st[i].clone().unwrap();
        // exception handlers covering this instruction
        for e in cd.exception_table.iter() {
            if (e.start as usize) <= i && i < e.end as usize {
                let mut h: Stack = s.iter().take(e.depth as usize).copied().collect();
                while h.len() < e.depth as usize {
                    h.push(Kind::Object);
                }
                if e.lasti {
                    h.push(Kind::Lasti);
                }
                h.push(Kind::Except);
                set(&mut st, &mut work, e.target as usize, h);
            }
        }
        let (_, op, arg) = ins[i];
        let (fall, jump) = step(op, arg, &s);
        if let Some(f) = fall {
            set(&mut st, &mut work, i + 1, f);
        }
        if let Some((t, j)) = jump {
            set(&mut st, &mut work, t, j);
        }
    }
    st
}

fn incompatible(target: &Stack) -> String {
    match target.iter().rev().find(|k| **k != Kind::Object) {
        Some(Kind::Iter) => "can't jump into the body of a for loop".into(),
        Some(Kind::Except) | Some(Kind::ExcInfo) | Some(Kind::Lasti) => "can't jump into an 'except' block as there's no exception".into(),
        _ => "can't jump: the target's value stack is incompatible with the current one".into(),
    }
}

/// Implements `frame.f_lineno = value`.
pub fn set_lineno(f: &mut Frame, value: Option<&PyObject>) -> PyResult<()> {
    match f.trace_event {
        0 => raise!(ValueError, "f_lineno can only be set by a trace function"),
        EV_CALL => raise!(ValueError, "can't jump from the 'call' trace event of a new frame"),
        EV_LINE | EV_OPCODE => {}
        _ => raise!(ValueError, "can only jump from a 'line' trace event"),
    }
    let Some(v) = value else { raise!(AttributeError, "cannot delete f_lineno") };
    if !crate::types::int::is_int(v) {
        raise!(ValueError, "lineno must be an integer");
    }
    let new_line = crate::types::int::as_i64(v).unwrap_or(i64::MAX);
    let co = f.code().to_ref();
    let cd = code::code_data(&co);
    let first = crate::types::code::code(&co).obj.co_firstlineno as i64;
    let lines: Vec<i64> = cd.locations.iter().map(|l| l.lineno as i64).collect();
    let max_line = lines.iter().copied().max().unwrap_or(first);
    if new_line < first {
        raise!(ValueError, "line {} comes before the current code block", new_line);
    }
    if new_line > max_line {
        raise!(ValueError, "line {} comes after the current code block", new_line);
    }
    // the first line at or after new_line that has code
    let target_line = lines.iter().copied().filter(|&l| l >= new_line && l > 0).min().unwrap_or(new_line);
    let ins = decode(&cd.code);
    let candidates: Vec<usize> = (0..lines.len())
        .filter(|&i| lines[i] == target_line && (i == 0 || lines[i - 1] != target_line))
        .filter(|&i| ins.get(i).is_some_and(|x| x.1 != Op::ExtendedArg))
        .collect();
    let stacks = analyse(&co);
    let cur = match stacks.get(f.lasti as usize).cloned().flatten() {
        Some(s) => s,
        None => raise!(ValueError, "can't jump from here"),
    };
    let mut err: Option<String> = None;
    let mut chosen: Option<(usize, Stack)> = None;
    for &t in &candidates {
        let Some(ts) = stacks.get(t).cloned().flatten() else { continue };
        if ts.len() > cur.len() || ts[..] != cur[..ts.len()] {
            if err.is_none() {
                err = Some(incompatible(&ts));
            }
            continue;
        }
        if cur[ts.len()..].iter().any(|k| *k == Kind::Null) {
            if err.is_none() {
                err = Some("can't jump: popping a NULL from the value stack is not allowed".into());
            }
            continue;
        }
        chosen = Some((t, ts));
        break;
    }
    let Some((target, ts)) = chosen else {
        match err {
            Some(m) => raise!(ValueError, "{}", m),
            None => raise!(ValueError, "line {} has no code that can be jumped to", new_line),
        }
    };
    // pop the extra entries (restoring a saved handled exception)
    let base = f.nlocalsplus;
    while f.stack_top - base > ts.len() {
        let k = cur[f.stack_top - base - 1];
        f.stack_top -= 1;
        let p = std::mem::replace(&mut f.slots[f.stack_top], std::ptr::null_mut());
        if k == Kind::ExcInfo {
            let prev = if !p.is_null() && unsafe { crate::types::none::is_none(&*p) } {
                unsafe { decref(p) };
                std::ptr::null_mut()
            } else {
                p
            };
            let old = crate::interp::eval::restore_handled_exception(prev);
            unsafe { xdecref(old) };
        } else {
            unsafe { xdecref(p) };
        }
    }
    let unbound = fill_unbound_locals(f, &ins, target);
    f.jump_to = target as i32;
    if unbound > 0 {
        crate::modules::warnings::warn(
            crate::types::exceptions::RuntimeWarning(),
            &format!("assigning None to {} unbound local{}", unbound, if unbound == 1 { "" } else { "s" }),
            0,
        )?;
    }
    Ok(())
}

/// Locals that are unbound in the frame but read without a check (LoadFast) on some
/// path from `target` before being stored get None. Returns how many were set.
fn fill_unbound_locals(f: &mut Frame, ins: &[(usize, Op, u32)], target: usize) -> usize {
    let cd = code::code_data(f.code());
    let nl = cd.localspluskinds.len().min(f.nlocalsplus);
    let candidates: Vec<usize> = (0..nl)
        .filter(|&i| f.slots[i].is_null() && cd.localspluskinds[i] & (fast::CELL | fast::FREE) == 0)
        .collect();
    if candidates.is_empty() {
        return 0;
    }
    let idx = |i: usize| candidates.iter().position(|&c| c == i);
    // state: bitset over candidates of "stored on this path"
    let mut best: Vec<Option<Vec<bool>>> = vec![None; ins.len()];
    let mut need = vec![false; candidates.len()];
    let mut work = vec![(target, vec![false; candidates.len()])];
    while let Some((i, stored)) = work.pop() {
        if i >= ins.len() {
            continue;
        }
        if let Some(b) = &best[i] {
            // only revisit with fewer stored locals
            if b.iter().zip(stored.iter()).all(|(old, new)| *old <= *new) {
                continue;
            }
        }
        best[i] = Some(match &best[i] {
            Some(b) => b.iter().zip(stored.iter()).map(|(a, b)| *a && *b).collect(),
            None => stored.clone(),
        });
        let (_, op, arg) = ins[i];
        let mut s = stored.clone();
        match op {
            Op::LoadFast => {
                if let Some(k) = idx(arg as usize) {
                    if !s[k] {
                        need[k] = true;
                    }
                }
            }
            Op::StoreFast | Op::LoadFastCheck | Op::DeleteFast => {
                if let Some(k) = idx(arg as usize) {
                    s[k] = true;
                }
            }
            _ => {}
        }
        let (fall, jump) = step(op, arg, &Vec::new());
        if fall.is_some() {
            work.push((i + 1, s.clone()));
        }
        if let Some((t, _)) = jump {
            work.push((t, s));
        }
    }
    let mut n = 0;
    for (k, &slot) in candidates.iter().enumerate() {
        if need[k] {
            f.slots[slot] = crate::types::none::none_ref().into_raw();
            n += 1;
        }
    }
    n
}
