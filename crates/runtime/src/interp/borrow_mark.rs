//! (refs1) The translation step of borrowed value-stack references (see borrow.rs): which
//! `LoadFast` instructions of internal code become `LoadFastBorrow`. Depends only on the
//! bytecode definitions and the JIT's stack-effect table, so the runtime's build script
//! (start-up image, crates/runtime/build/main.rs) runs it too.

use rpy_compiler::bytecode::{self as bc, Op};


/// Rewrite the `LoadFast` instructions of internal `code` whose pushed reference can be
/// borrowed into `LoadFastBorrow` (see the module comment for the rule). Returns the
/// number rewritten.
///
/// One forward pass over the code with every load still in question tracked at once
/// (usually none or one or two): each candidate records the stack positions holding
/// copies of its value; it succeeds when the last copy is consumed and fails at a
/// block boundary, at a store to its local while a copy is live, under a handler that
/// would keep a copy, or at an instruction this model does not know. Stack depths come
/// from the same pass (forward jump targets record theirs; code reached only by a
/// backward jump, which compiled code does not have, is skipped). It runs for every
/// translated code object, so it allocates nothing for code of up to 511 words.
pub fn mark(code: &mut [u32], handlers: &[bc::ExceptionEntry]) -> usize {
    let n = code.len();
    // per word: NOT_TARGET, or a block start with its entry depth (-1: not known yet)
    const NOT_TARGET: i32 = i32::MIN;
    let mut buf = std::mem::MaybeUninit::<[i32; 512]>::uninit();
    let mut heap: Vec<i32> = Vec::new();
    let entry: &mut [i32] = if n < 512 {
        let p = buf.as_mut_ptr() as *mut i32;
        for i in 0..=n {
            unsafe { p.add(i).write(NOT_TARGET) };
        }
        unsafe { std::slice::from_raw_parts_mut(p, n + 1) }
    } else {
        heap.resize(n + 1, NOT_TARGET);
        &mut heap[..]
    };
    let mut any_load = false;
    let mut ext = 0u32;
    for &w in code.iter() {
        let (op, a) = bc::decode(w);
        let arg = a | ext;
        if op == Op::ExtendedArg {
            ext = arg << 24;
            continue;
        }
        ext = 0;
        if op.is_jump() && (arg as usize) < n {
            entry[arg as usize] = -1;
        }
        any_load |= op == Op::LoadFast;
    }
    if !any_load {
        return 0;
    }
    for h in handlers {
        if (h.target as usize) < n {
            entry[h.target as usize] = h.depth as i32 + h.lasti as i32 + 1;
        }
    }
    let mut cands: [Cand; MAX_CANDS] = [Cand { pc: 0, local: 0, al: [0; 4], na: 0 }; MAX_CANDS];
    let mut nc = 0usize;
    let mut count = 0;
    let mut cur: i32 = 0; // -1: unknown (unreachable by fall-through)
    let mut hix = 0usize; // handler entries are sorted and do not overlap
    let mut start = 0usize;
    let mut ext = 0u32;
    for pc in 0..n {
        let (op, a) = bc::decode(code[pc]);
        let arg = a | ext;
        if op == Op::ExtendedArg {
            ext = arg << 24;
            continue;
        }
        ext = 0;
        let first = std::mem::replace(&mut start, pc + 1);
        let mut e = entry[pc];
        if first < pc {
            // (EXTENDED_ARG prefixes: a jump may name the prefix)
            for k in first..pc {
                e = e.max(entry[k]);
            }
        }
        if e != NOT_TARGET {
            // a block starts here
            nc = 0;
            if cur < 0 {
                cur = e;
            }
        }
        if cur < 0 {
            continue;
        }
        if nc > 0 {
            // deepest stack level an exception handler keeps here
            while hix < handlers.len() && handlers[hix].end as usize <= pc {
                hix += 1;
            }
            let keep = match handlers.get(hix) {
                Some(h) if (h.start as usize) <= pc => h.depth as i32,
                _ => -1,
            };
            let io = io(op, arg);
            let mut k = 0;
            while k < nc {
                match cands[k].step(&io, cur, keep) {
                    Step::Live => k += 1,
                    Step::Done => {
                        let p = cands[k].pc;
                        code[p] = (code[p] & !0xff) | Op::LoadFastBorrow as u32;
                        count += 1;
                        nc -= 1;
                        cands[k] = cands[nc];
                    }
                    Step::Fail => {
                        nc -= 1;
                        cands[k] = cands[nc];
                    }
                }
            }
        }
        if op == Op::LoadFast && nc < MAX_CANDS {
            cands[nc] = Cand { pc, local: arg, al: [cur, 0, 0, 0], na: 1 };
            nc += 1;
        }
        let (fall, jump) = super::jit::analysis::effect(op, arg);
        if op.is_jump() {
            let t = arg as usize;
            if t > pc && t <= n && entry[t] < 0 {
                entry[t] = cur + jump;
            }
        }
        if super::jit::analysis::is_terminator(op) {
            cur = -1;
            nc = 0;
        } else {
            cur += fall;
            if cur < 0 {
                cur = -1;
                nc = 0;
            }
        }
    }
    count
}

/// Loads tracked at once (more are not borrowed).
const MAX_CANDS: usize = 16;

/// A `LoadFast` whose value may still be borrowed (`mark`).
#[derive(Clone, Copy)]
struct Cand {
    pc: usize,
    local: u32,
    /// stack positions (0 = bottom of the value stack) holding copies of the value
    al: [i32; 4],
    na: usize,
}

enum Step {
    Live,
    Done,
    Fail,
}

/// How an instruction treats the stack, for `borrowable`.
enum Io {
    /// pops `0`, then pushes `1` new (owned) values
    Plain(u32, u32),
    /// like Plain, and the value popped from the top reappears as the topmost push
    /// (LoadMethod's `self`)
    Through(u32, u32),
    /// rebinds or deletes local `1` after popping `0`
    Store(u32, u32),
    /// pops `0` and ends the basic block (control leaves or the frame suspends)
    End(u32),
    Swap(u32),
    /// anything not modelled: give up
    Unknown,
}

#[inline]
fn io(op: Op, arg: u32) -> Io {
    use Io::*;
    use Op::*;
    match op {
        Nop | Resume | SetupAnnotations | DeleteDeref | DeleteName | DeleteGlobal => Plain(0, 0),
        PopTop | StoreDeref | StoreName | StoreGlobal | DeleteAttr | ListAppend | SetAdd | ListExtend | SetUpdate
        | DictUpdate | DictMerge => Plain(1, 0),
        PushNull | Copy | LoadConst | LoadFast | LoadFastBorrow | LoadFastCheck | LoadDeref | LoadLocals | LoadName
        | LoadGlobal | LoadBuildClass | LoadCommonConstant | GetAnext | ImportFrom | GetLen | MatchMapping
        | MatchSequence | MatchKeys => Plain(0, 1),
        LoadFromDictOrDeref | LoadFromDictOrGlobals | LoadAttr | UnaryNegative | UnaryPositive | UnaryInvert
        | UnaryNot | ToBool | GetIter | GetAiter | GetAwaitable | GetYieldFromIter | CallIntrinsic1 | MakeFunction
        | FormatSimple | ConvertValue => Plain(1, 1),
        BinaryOp | CompareOp | IsOp | ContainsOp | BinarySubscr | CallIntrinsic2 | SetFunctionAttribute
        | FormatWithSpec | ImportName | BuildTemplate => Plain(2, 1),
        StoreAttr | MapAdd | DeleteSubscr => Plain(2, 0),
        StoreSubscr => Plain(3, 0),
        StoreSlice => Plain(4, 0),
        BinarySlice | MatchClass => Plain(3, 1),
        BuildSlice => Plain(arg, 1),
        BuildTuple | BuildList | BuildSet | BuildString => Plain(arg, 1),
        BuildMap => Plain(2 * arg, 1),
        BuildInterpolation => Plain(2 + (arg & 1), 1),
        UnpackSequence => Plain(1, arg),
        UnpackEx => Plain(1, (arg & 0xff) + (arg >> 8) + 1),
        Call => Plain(arg + 2, 1),
        CallKw => Plain(arg + 3, 1),
        CallFunctionEx => Plain(4, 1),
        BeforeWith | BeforeAsyncWith => Plain(1, 3),
        LoadMethod | LoadSpecial => Through(1, 2),
        LoadSuperAttr => {
            if arg & 1 != 0 {
                Through(3, 2)
            } else {
                Plain(3, 1)
            }
        }
        StoreFast => Store(1, arg),
        DeleteFast => Store(0, arg),
        LoadFastAndClear => Store(0, arg),
        Op::Swap => Io::Swap(arg),
        Jump | ForIter | Send | JumpIfFalseOrPop | JumpIfTrueOrPop | RaiseVarargs | Reraise => End(0),
        PopJumpIfFalse | PopJumpIfTrue | PopJumpIfNone | PopJumpIfNotNone | ReturnValue | YieldValue => End(1),
        _ => Unknown,
    }
}

impl Cand {
    /// Apply one instruction (stack depth `cur` before it; `keep`: the deepest stack
    /// level an exception handler keeps here, -1 if none).
    fn step(&mut self, io: &Io, cur: i32, keep: i32) -> Step {
        let al = &mut self.al;
        // an exception raised here would leave the copy on the stack for the handler
        if al[..self.na].iter().any(|&p| p < keep) {
            return Step::Fail;
        }
        let (pops, pushes, through, store, end) = match *io {
            Io::Plain(p, q) => (p, q, false, None, false),
            Io::Through(p, q) => (p, q, true, None, false),
            Io::Store(p, l) => (p, 0, false, Some(l), false),
            Io::End(p) => (p, 0, false, None, true),
            Io::Swap(k) => {
                let (a, b) = (cur - 1, cur - k as i32);
                for x in al[..self.na].iter_mut() {
                    if *x == a {
                        *x = b;
                    } else if *x == b {
                        *x = a;
                    }
                }
                return Step::Live;
            }
            Io::Unknown => return Step::Fail,
        };
        let base = cur - pops as i32;
        if base < 0 {
            return Step::Fail;
        }
        let mut passed = false;
        let mut k = 0;
        while k < self.na {
            if al[k] >= base {
                if through && al[k] == cur - 1 {
                    passed = true;
                }
                al[k] = al[self.na - 1];
                self.na -= 1;
            } else {
                k += 1;
            }
        }
        if passed {
            if self.na == al.len() {
                return Step::Fail;
            }
            al[self.na] = base + pushes as i32 - 1;
            self.na += 1;
        }
        if self.na == 0 {
            return Step::Done;
        }
        if end || store == Some(self.local) {
            return Step::Fail;
        }
        Step::Live
    }
}

/// Values an instruction pops in the model of `mark` (None: not modelled).
pub fn pops(op: Op, arg: u32) -> Option<u32> {
    match io(op, arg) {
        Io::Plain(p, _) | Io::Through(p, _) | Io::Store(p, _) | Io::End(p) => Some(p),
        Io::Swap(_) => Some(0),
        Io::Unknown => None,
    }
}
