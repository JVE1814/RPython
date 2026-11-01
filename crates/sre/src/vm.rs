//! Backtracking matcher with an explicit stack.
//!
//! State changes that must be undone on backtracking (capture slots,
//! `lastindex`, repeat contexts) are recorded in a trail; every choice point
//! remembers the trail length.  Atomic groups / lookarounds / possessive
//! iterations are "regions": a marker frame on the backtrack stack; leaving a
//! region successfully cuts the stack back to its marker (the trail is kept).

use crate::compiler::{
    ascii_word, locale_word, At, Inst, Program, RefMode, WordMode, NONE,
};
use crate::parser::RepKind;
use crate::unicode::UnicodeProvider;
use crate::MAXREPEAT;

/// A code unit of a subject string (PEP 393 storage or bytes).
pub trait CodeUnit: Copy + Into<u32> + Send + Sync + 'static {}
impl CodeUnit for u8 {}
impl CodeUnit for u16 {}
impl CodeUnit for u32 {}

pub(crate) const UNSET: usize = usize::MAX;

const F_CHOICE: u32 = 0;
const F_REP1_GREEDY: u32 = 1;
const F_REP1_LAZY: u32 = 2;
const F_ASSERT: u32 = 3;
const F_ATOMIC: u32 = 4;
const F_UNTIL_TAIL: u32 = 5;
const F_MIN_MORE: u32 = 6;
const F_POSS: u32 = 7;

#[derive(Clone, Copy, Debug)]
struct Frame {
    tag: u32,
    pc: u32,
    trail: u32,
    a: u32,
    pos: usize,
    b: usize,
}

const U_SLOT: u32 = 0;
const U_LASTINDEX: u32 = 1;
const U_REPCOUNT: u32 = 2;
const U_REPLAST: u32 = 3;
const U_CURREP: u32 = 4;
const U_REPPUSH: u32 = 5;

#[derive(Clone, Copy, Debug)]
struct Undo {
    kind: u32,
    idx: u32,
    val: usize,
}

#[derive(Clone, Copy, Debug)]
struct RepCtx {
    count: isize,
    last_pos: usize,
    prev: u32,
}

/// Reusable scratch memory for matching (avoids allocations per match).
#[derive(Default, Debug)]
pub struct Scratch {
    slots: Vec<usize>,
    bt: Vec<Frame>,
    trail: Vec<Undo>,
    reps: Vec<RepCtx>,
    regions: Vec<u32>,
}

impl Scratch {
    pub fn new() -> Self {
        Self::default()
    }
}

pub(crate) struct Vm<'a, T: CodeUnit> {
    prog: &'a Program,
    prov: &'a UnicodeProvider,
    s: &'a [T],
    end: usize,
    pub match_all: bool,
    pub must_advance: bool,
    /// Start of the search (for `must_advance`).
    pub origin: usize,
    lastindex: isize,
    cur_rep: u32,
    dirty: bool,
    sc: &'a mut Scratch,
}

impl<'a, T: CodeUnit> Vm<'a, T> {
    pub fn new(
        prog: &'a Program,
        prov: &'a UnicodeProvider,
        s: &'a [T],
        end: usize,
        sc: &'a mut Scratch,
    ) -> Self {
        sc.slots.clear();
        sc.slots.resize(prog.nslots, UNSET);
        Vm {
            prog,
            prov,
            s,
            end,
            match_all: false,
            must_advance: false,
            origin: 0,
            lastindex: -1,
            cur_rep: NONE,
            dirty: false,
            sc,
        }
    }

    pub fn slots(&self) -> &[usize] {
        &self.sc.slots
    }

    pub fn lastindex(&self) -> isize {
        self.lastindex
    }

    #[inline(always)]
    fn ch(&self, i: usize) -> u32 {
        // SAFETY-free: bounds are checked by callers via `end`, but keep the
        // check for safety.
        self.s[i].into()
    }

    fn reset(&mut self) {
        let sc = &mut *self.sc;
        if self.dirty {
            for x in sc.slots.iter_mut() {
                *x = UNSET;
            }
            self.dirty = false;
        }
        sc.bt.clear();
        sc.trail.clear();
        sc.reps.clear();
        sc.regions.clear();
        self.lastindex = -1;
        self.cur_rep = NONE;
    }

    #[inline(always)]
    fn log(&mut self, kind: u32, idx: u32, val: usize) {
        if !self.sc.bt.is_empty() {
            self.sc.trail.push(Undo { kind, idx, val });
        }
    }

    #[inline(always)]
    fn push(&mut self, tag: u32, pc: u32, pos: usize, a: u32, b: usize) {
        let trail = self.sc.trail.len() as u32;
        self.sc.bt.push(Frame { tag, pc, trail, a, pos, b });
    }

    fn undo_to(&mut self, len: usize) {
        while self.sc.trail.len() > len {
            let u = self.sc.trail.pop().unwrap();
            match u.kind {
                U_SLOT => self.sc.slots[u.idx as usize] = u.val,
                U_LASTINDEX => self.lastindex = u.val as isize,
                U_REPCOUNT => self.sc.reps[u.idx as usize].count = u.val as isize,
                U_REPLAST => self.sc.reps[u.idx as usize].last_pos = u.val,
                U_CURREP => self.cur_rep = u.val as u32,
                U_REPPUSH => {
                    self.sc.reps.pop();
                    self.cur_rep = u.val as u32;
                }
                _ => unreachable!(),
            }
        }
    }

    fn set_rep_count(&mut self, id: u32, count: isize) {
        let old = self.sc.reps[id as usize].count;
        self.log(U_REPCOUNT, id, old as usize);
        self.sc.reps[id as usize].count = count;
    }

    fn set_rep_last(&mut self, id: u32, pos: usize) {
        let old = self.sc.reps[id as usize].last_pos;
        self.log(U_REPLAST, id, old);
        self.sc.reps[id as usize].last_pos = pos;
    }

    fn set_cur_rep(&mut self, r: u32) {
        let old = self.cur_rep;
        self.log(U_CURREP, 0, old as usize);
        self.cur_rep = r;
    }

    #[inline(always)]
    fn single(&self, inst: Inst, c: u32) -> bool {
        match inst {
            Inst::Char(x) => c == x,
            Inst::NotChar(x) => c != x,
            Inst::Char2(x, y) => c == x || c == y,
            Inst::Any => c != 10,
            Inst::AnyAll => true,
            Inst::Set(i) => self.prog.sets[i as usize].contains(c, self.prov),
            Inst::LocLit(x, neg) => {
                let m = c == x
                    || (c < 256
                        && ((self.prov.locale_lower)(c as u8) as u32 == x
                            || (self.prov.locale_upper)(c as u8) as u32 == x));
                m != neg
            }
            Inst::LocSet(i) => self.prog.locsets[i as usize].contains(c, self.prov),
            _ => false,
        }
    }

    /// Count how many characters starting at `pos` (at most `maxc`) match
    /// the single-character matcher `inst`.
    #[inline]
    fn count_single(&self, inst: Inst, pos: usize, maxc: usize) -> usize {
        let lim = pos + maxc;
        let s = &self.s[pos..lim];
        match inst {
            Inst::AnyAll => maxc,
            Inst::Any => s.iter().position(|&c| c.into() == 10).unwrap_or(maxc),
            Inst::Char(x) => s.iter().position(|&c| c.into() != x).unwrap_or(maxc),
            Inst::NotChar(x) => s.iter().position(|&c| c.into() == x).unwrap_or(maxc),
            Inst::Set(i) => {
                let set = &self.prog.sets[i as usize];
                let prov = self.prov;
                s.iter().position(|&c| !set.contains(c.into(), prov)).unwrap_or(maxc)
            }
            _ => s.iter().position(|&c| !self.single(inst, c.into())).unwrap_or(maxc),
        }
    }

    #[inline]
    fn is_word(&self, mode: WordMode, c: u32) -> bool {
        match mode {
            WordMode::Ascii => ascii_word(c),
            WordMode::Unicode => (self.prov.is_word)(c),
            WordMode::Locale => locale_word(c, self.prov),
        }
    }

    fn at(&self, at: At, pos: usize) -> bool {
        let end = self.end;
        match at {
            At::Beginning => pos == 0,
            At::BeginningLine => pos == 0 || self.ch(pos - 1) == 10,
            At::End => pos == end || (pos + 1 == end && self.ch(pos) == 10),
            At::EndLine => pos == end || (pos < end && self.ch(pos) == 10),
            At::EndString => pos == end,
            At::Boundary(m) | At::NonBoundary(m) => {
                let that = pos > 0 && pos - 1 < self.s.len() && self.is_word(m, self.ch(pos - 1));
                let this = pos < end && self.is_word(m, self.ch(pos));
                (that != this) == matches!(at, At::Boundary(_))
            }
        }
    }

    fn group_span(&self, g: u32) -> Option<(usize, usize)> {
        let i = 2 * (g as usize - 1);
        let a = self.sc.slots[i];
        let b = self.sc.slots[i + 1];
        if a == UNSET || b == UNSET || a > b {
            None
        } else {
            Some((a, b))
        }
    }

    fn groupref(&self, g: u32, mode: RefMode, pos: usize) -> Option<usize> {
        let (a, b) = self.group_span(g)?;
        let len = b - a;
        if pos + len > self.end {
            return None;
        }
        let prov = self.prov;
        for k in 0..len {
            let x = self.ch(a + k);
            let y = self.ch(pos + k);
            let ok = match mode {
                RefMode::Exact => x == y,
                RefMode::IgnoreAscii => {
                    crate::unicode::ascii_tolower(x) == crate::unicode::ascii_tolower(y)
                }
                RefMode::IgnoreUnicode => x == y || (prov.lower)(x) == (prov.lower)(y),
                RefMode::IgnoreLocale => {
                    x == y
                        || (x < 256
                            && y < 256
                            && (prov.locale_lower)(x as u8) == (prov.locale_lower)(y as u8))
                }
            };
            if !ok {
                return None;
            }
        }
        Some(pos + len)
    }

    /// Try to match starting at `pc0` with the subject position `pos0`.
    /// Returns the end position on success; on failure everything is reset.
    pub fn run(&mut self, pc0: usize, pos0: usize) -> Option<usize> {
        self.reset();
        let r = self.exec(pc0, pos0);
        r
    }

    fn exec(&mut self, pc0: usize, pos0: usize) -> Option<usize> {
        let prog = self.prog;
        let insts = &prog.insts[..];
        let end = self.end;
        let mut pc = pc0;
        let mut pos = pos0;
        loop {
            let ok: bool = 'step: {
                match insts[pc] {
                    Inst::Char(x) => {
                        if pos < end && self.ch(pos) == x {
                            pos += 1;
                            pc += 1;
                            break 'step true;
                        }
                        false
                    }
                    Inst::Lit(st, n) => {
                        let n = n as usize;
                        if pos + n <= end {
                            let lit = &prog.lits[st as usize..st as usize + n];
                            let s = &self.s[pos..pos + n];
                            if s.iter().zip(lit).all(|(&a, &b)| a.into() == b) {
                                pos += n;
                                pc += 1;
                                break 'step true;
                            }
                        }
                        false
                    }
                    i @ (Inst::NotChar(_)
                    | Inst::Char2(..)
                    | Inst::Any
                    | Inst::AnyAll
                    | Inst::Set(_)
                    | Inst::LocLit(..)
                    | Inst::LocSet(_)) => {
                        if pos < end && self.single(i, self.ch(pos)) {
                            pos += 1;
                            pc += 1;
                            true
                        } else {
                            false
                        }
                    }
                    Inst::At(a) => {
                        if self.at(a, pos) {
                            pc += 1;
                            true
                        } else {
                            false
                        }
                    }
                    Inst::Mark(slot) => {
                        let old = self.sc.slots[slot as usize];
                        self.log(U_SLOT, slot, old);
                        self.sc.slots[slot as usize] = pos;
                        self.dirty = true;
                        if slot & 1 == 1 {
                            let li = self.lastindex;
                            self.log(U_LASTINDEX, 0, li as usize);
                            self.lastindex = (slot / 2 + 1) as isize;
                        }
                        pc += 1;
                        true
                    }
                    Inst::Split(a, b) => {
                        self.push(F_CHOICE, b, pos, 0, 0);
                        pc = a as usize;
                        true
                    }
                    Inst::SplitG(a, b, g) => {
                        if pos < end && self.ch(pos) == g {
                            self.push(F_CHOICE, b, pos, 0, 0);
                            pc = a as usize;
                        } else {
                            pc = b as usize;
                        }
                        true
                    }
                    Inst::SplitS(a, b, si) => {
                        if pos < end && prog.sets[si as usize].contains(self.ch(pos), self.prov) {
                            self.push(F_CHOICE, b, pos, 0, 0);
                            pc = a as usize;
                        } else {
                            pc = b as usize;
                        }
                        true
                    }
                    Inst::Jmp(t) => {
                        pc = t as usize;
                        true
                    }
                    Inst::Nop => {
                        pc += 1;
                        true
                    }
                    Inst::GroupRef(g, mode) => match self.groupref(g, mode, pos) {
                        Some(p) => {
                            pos = p;
                            pc += 1;
                            true
                        }
                        None => false,
                    },
                    Inst::GroupExists(g, else_pc) => {
                        if self.group_span(g).is_some() {
                            pc += 1;
                        } else {
                            pc = else_pc as usize;
                        }
                        true
                    }
                    Inst::RepeatOne { min, max, kind, hint } => {
                        let body = insts[pc + 1];
                        let avail = end - pos;
                        let maxc = if max == MAXREPEAT { avail } else { (max as usize).min(avail) };
                        let min = min as usize;
                        if min > maxc {
                            break 'step false;
                        }
                        match kind {
                            RepKind::Greedy => {
                                let count = self.count_single(body, pos, maxc);
                                if count < min {
                                    break 'step false;
                                }
                                let minpos = pos + min;
                                let mut cur = pos + count;
                                if hint != NONE {
                                    loop {
                                        if cur < end && self.ch(cur) == hint {
                                            break;
                                        }
                                        if cur == minpos {
                                            break 'step false;
                                        }
                                        cur -= 1;
                                    }
                                }
                                if cur > minpos {
                                    self.push(F_REP1_GREEDY, pc as u32, cur, 0, minpos);
                                }
                                pos = cur;
                                pc += 2;
                                true
                            }
                            RepKind::Lazy => {
                                if min > 0 {
                                    let c = self.count_single(body, pos, min);
                                    if c < min {
                                        break 'step false;
                                    }
                                }
                                let maxpos = pos + maxc;
                                let mut cur = pos + min;
                                if hint != NONE {
                                    loop {
                                        if cur < end && self.ch(cur) == hint {
                                            break;
                                        }
                                        if cur < maxpos && self.single(body, self.ch(cur)) {
                                            cur += 1;
                                        } else {
                                            break 'step false;
                                        }
                                    }
                                }
                                if cur < maxpos {
                                    self.push(F_REP1_LAZY, pc as u32, cur, 0, maxpos);
                                }
                                pos = cur;
                                pc += 2;
                                true
                            }
                            RepKind::Possessive => {
                                let count = self.count_single(body, pos, maxc);
                                if count < min {
                                    break 'step false;
                                }
                                pos += count;
                                pc += 2;
                                true
                            }
                        }
                    }
                    Inst::RepStart(r) => {
                        let info = prog.reps[r as usize];
                        let id = self.sc.reps.len() as u32;
                        let old = self.cur_rep;
                        self.log(U_REPPUSH, 0, old as usize);
                        self.sc.reps.push(RepCtx { count: -1, last_pos: UNSET, prev: old });
                        self.cur_rep = id;
                        pc = info.until as usize;
                        true
                    }
                    Inst::Until(r) => {
                        let info = prog.reps[r as usize];
                        let ctx = self.cur_rep;
                        let rc = self.sc.reps[ctx as usize];
                        let count = rc.count + 1;
                        if count < info.min as isize {
                            self.set_rep_count(ctx, count);
                            pc = info.body as usize;
                            break 'step true;
                        }
                        if info.greedy {
                            if (count < info.max as isize || info.max == MAXREPEAT)
                                && pos != rc.last_pos
                            {
                                self.push(F_UNTIL_TAIL, pc as u32, pos, 0, 0);
                                self.set_rep_count(ctx, count);
                                self.set_rep_last(ctx, pos);
                                pc = info.body as usize;
                                break 'step true;
                            }
                            self.set_cur_rep(rc.prev);
                            pc = info.exit as usize;
                            true
                        } else {
                            self.push(F_MIN_MORE, pc as u32, pos, 0, 0);
                            self.set_cur_rep(rc.prev);
                            pc = info.exit as usize;
                            true
                        }
                    }
                    Inst::PossStart(r) => {
                        let info = prog.reps[r as usize];
                        let b = if info.min == 0 { pos } else { UNSET };
                        self.push(F_POSS, pc as u32, pos, 0, b);
                        self.sc.regions.push((self.sc.bt.len() - 1) as u32);
                        pc = info.body as usize;
                        true
                    }
                    Inst::PossEnd => {
                        let idx = self.sc.regions.pop().unwrap() as usize;
                        let m = self.sc.bt[idx];
                        self.sc.bt.truncate(idx);
                        let r = match insts[m.pc as usize] {
                            Inst::PossStart(r) => r,
                            _ => unreachable!(),
                        };
                        let info = prog.reps[r as usize];
                        let count = m.a + 1;
                        if count < info.min {
                            self.push(F_POSS, m.pc, pos, count, UNSET);
                            self.sc.regions.push((self.sc.bt.len() - 1) as u32);
                            pc = info.body as usize;
                        } else if (count < info.max || info.max == MAXREPEAT)
                            && !(m.b != UNSET && pos == m.b)
                        {
                            self.push(F_POSS, m.pc, pos, count, pos);
                            self.sc.regions.push((self.sc.bt.len() - 1) as u32);
                            pc = info.body as usize;
                        } else {
                            pc = info.exit as usize;
                        }
                        true
                    }
                    Inst::AssertStart { neg, back, exit } => {
                        let back = back as usize;
                        if pos < back {
                            if neg {
                                pc = exit as usize;
                                break 'step true;
                            }
                            break 'step false;
                        }
                        self.push(F_ASSERT, pc as u32, pos, 0, 0);
                        self.sc.regions.push((self.sc.bt.len() - 1) as u32);
                        pos -= back;
                        pc += 1;
                        true
                    }
                    Inst::AssertEnd => {
                        let idx = self.sc.regions.pop().unwrap() as usize;
                        let m = self.sc.bt[idx];
                        self.sc.bt.truncate(idx);
                        match insts[m.pc as usize] {
                            Inst::AssertStart { neg: false, exit, .. } => {
                                pos = m.pos;
                                pc = exit as usize;
                                true
                            }
                            _ => false,
                        }
                    }
                    Inst::AtomicStart(_) => {
                        self.push(F_ATOMIC, pc as u32, pos, 0, 0);
                        self.sc.regions.push((self.sc.bt.len() - 1) as u32);
                        pc += 1;
                        true
                    }
                    Inst::AtomicEnd => {
                        let idx = self.sc.regions.pop().unwrap() as usize;
                        self.sc.bt.truncate(idx);
                        pc += 1;
                        true
                    }
                    Inst::Match => {
                        if (self.match_all && pos != end) || (self.must_advance && pos == self.origin)
                        {
                            false
                        } else {
                            return Some(pos);
                        }
                    }
                    Inst::Fail => false,
                }
            };
            if ok {
                continue;
            }
            // ---- backtrack
            loop {
                let Some(f) = self.sc.bt.pop() else {
                    return None;
                };
                self.undo_to(f.trail as usize);
                match f.tag {
                    F_CHOICE => {
                        pc = f.pc as usize;
                        pos = f.pos;
                        break;
                    }
                    F_REP1_GREEDY => {
                        let rpc = f.pc as usize;
                        let hint = match insts[rpc] {
                            Inst::RepeatOne { hint, .. } => hint,
                            _ => unreachable!(),
                        };
                        let minpos = f.b;
                        let mut cur = f.pos - 1;
                        if hint != NONE {
                            let mut found = false;
                            loop {
                                if cur < end && self.ch(cur) == hint {
                                    found = true;
                                    break;
                                }
                                if cur == minpos {
                                    break;
                                }
                                cur -= 1;
                            }
                            if !found {
                                continue;
                            }
                        }
                        if cur > minpos {
                            self.push(F_REP1_GREEDY, rpc as u32, cur, 0, minpos);
                        }
                        pc = rpc + 2;
                        pos = cur;
                        break;
                    }
                    F_REP1_LAZY => {
                        let rpc = f.pc as usize;
                        let hint = match insts[rpc] {
                            Inst::RepeatOne { hint, .. } => hint,
                            _ => unreachable!(),
                        };
                        let body = insts[rpc + 1];
                        let maxpos = f.b;
                        let mut cur = f.pos;
                        if !(cur < maxpos && self.single(body, self.ch(cur))) {
                            continue;
                        }
                        cur += 1;
                        if hint != NONE {
                            let mut ok = true;
                            loop {
                                if cur < end && self.ch(cur) == hint {
                                    break;
                                }
                                if cur < maxpos && self.single(body, self.ch(cur)) {
                                    cur += 1;
                                } else {
                                    ok = false;
                                    break;
                                }
                            }
                            if !ok {
                                continue;
                            }
                        }
                        if cur < maxpos {
                            self.push(F_REP1_LAZY, rpc as u32, cur, 0, maxpos);
                        }
                        pc = rpc + 2;
                        pos = cur;
                        break;
                    }
                    F_ASSERT => {
                        self.sc.regions.pop();
                        match insts[f.pc as usize] {
                            Inst::AssertStart { neg: true, exit, .. } => {
                                pc = exit as usize;
                                pos = f.pos;
                                break;
                            }
                            _ => continue,
                        }
                    }
                    F_ATOMIC => {
                        self.sc.regions.pop();
                        continue;
                    }
                    F_UNTIL_TAIL => {
                        let r = match insts[f.pc as usize] {
                            Inst::Until(r) => r,
                            _ => unreachable!(),
                        };
                        let info = prog.reps[r as usize];
                        let prev = self.sc.reps[self.cur_rep as usize].prev;
                        self.set_cur_rep(prev);
                        pc = info.exit as usize;
                        pos = f.pos;
                        break;
                    }
                    F_MIN_MORE => {
                        let r = match insts[f.pc as usize] {
                            Inst::Until(r) => r,
                            _ => unreachable!(),
                        };
                        let info = prog.reps[r as usize];
                        let ctx = self.cur_rep;
                        let rc = self.sc.reps[ctx as usize];
                        let count = rc.count + 1;
                        if (count >= info.max as isize && info.max != MAXREPEAT)
                            || f.pos == rc.last_pos
                        {
                            continue;
                        }
                        self.set_rep_count(ctx, count);
                        self.set_rep_last(ctx, f.pos);
                        pc = info.body as usize;
                        pos = f.pos;
                        break;
                    }
                    F_POSS => {
                        self.sc.regions.pop();
                        let r = match insts[f.pc as usize] {
                            Inst::PossStart(r) => r,
                            _ => unreachable!(),
                        };
                        let info = prog.reps[r as usize];
                        if f.a < info.min {
                            continue;
                        }
                        pc = info.exit as usize;
                        pos = f.pos;
                        break;
                    }
                    _ => unreachable!(),
                }
            }
        }
    }
}
