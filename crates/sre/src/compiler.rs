//! AST -> program compiler (plus search-acceleration analysis).

use crate::error::CompileError;
use crate::flags::*;
use crate::parser::{width, AtCode, Category, Node, RepKind, SetItem};
use crate::unicode::UnicodeProvider;
use crate::MAXREPEAT;

pub(crate) const NONE: u32 = u32::MAX;
const MAXCODE: u128 = (1u128 << 32) - 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WordMode {
    Ascii,
    Unicode,
    Locale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum At {
    Beginning,
    BeginningLine,
    End,
    EndLine,
    EndString,
    Boundary(WordMode),
    NonBoundary(WordMode),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefMode {
    Exact,
    IgnoreAscii,
    IgnoreUnicode,
    IgnoreLocale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Inst {
    // --- single-character matchers (consume exactly one character)
    Char(u32),
    NotChar(u32),
    Char2(u32, u32),
    Any,
    AnyAll,
    Set(u32),
    /// LOCALE+IGNORECASE literal: (char, negate)
    LocLit(u32, bool),
    /// Set evaluated with the current locale at match time.
    LocSet(u32),
    // --- other
    /// Literal string (start, len) in the literal pool.
    Lit(u32, u32),
    At(At),
    Mark(u32),
    Split(u32, u32),
    Jmp(u32),
    GroupRef(u32, RefMode),
    /// (group, else-pc)
    GroupExists(u32, u32),
    /// Single-char repeat: body at pc+1, tail at pc+2.  `hint`: char the
    /// tail must start with (or NONE).
    RepeatOne { min: u32, max: u32, kind: RepKind, hint: u32 },
    RepStart(u32),
    Until(u32),
    PossStart(u32),
    PossEnd,
    AssertStart { neg: bool, back: u32, exit: u32 },
    AssertEnd,
    AtomicStart(u32),
    AtomicEnd,
    Nop,
    Match,
    Fail,
}

impl Inst {
    pub(crate) fn is_single(&self) -> bool {
        matches!(
            self,
            Inst::Char(_)
                | Inst::NotChar(_)
                | Inst::Char2(..)
                | Inst::Any
                | Inst::AnyAll
                | Inst::Set(_)
                | Inst::LocLit(..)
                | Inst::LocSet(_)
        )
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RepInfo {
    pub min: u32,
    pub max: u32,
    pub greedy: bool,
    pub body: u32,
    pub until: u32,
    pub exit: u32,
}

/// A character set.  Membership for chars < 256 is a precomputed bitmap
/// (negation already applied); higher chars use ranges + categories.
#[derive(Clone, Debug)]
pub(crate) struct CharSet {
    pub bits: [u64; 4],
    pub ranges: Vec<(u32, u32)>,
    pub cats: Vec<Category>,
    pub negate: bool,
}

impl CharSet {
    #[inline(always)]
    pub fn contains(&self, c: u32, prov: &UnicodeProvider) -> bool {
        if c < 256 {
            return self.bits[(c >> 6) as usize] >> (c & 63) & 1 != 0;
        }
        self.contains_high(c, prov)
    }
    #[inline(never)]
    fn contains_high(&self, c: u32, prov: &UnicodeProvider) -> bool {
        let r = &self.ranges;
        let i = r.partition_point(|&(_, hi)| hi < c);
        let mut m = i < r.len() && r[i].0 <= c;
        if !m {
            m = self.cats.iter().any(|&cat| cat_unicode(cat, c, prov));
        }
        m != self.negate
    }
}

/// A set whose evaluation depends on the current locale (bytes + LOCALE).
#[derive(Clone, Debug)]
pub(crate) struct LocSet {
    pub items: Vec<SetItem>,
    pub negate: bool,
    pub ignore: bool,
}

impl LocSet {
    fn raw(&self, c: u32, prov: &UnicodeProvider) -> bool {
        self.items.iter().any(|it| match *it {
            SetItem::Lit(x) => x == c,
            SetItem::Range(lo, hi) => lo <= c && c <= hi,
            SetItem::Cat(cat) => cat_locale(cat, c, prov),
        })
    }
    pub fn contains(&self, c: u32, prov: &UnicodeProvider) -> bool {
        let mut m = self.raw(c, prov);
        if !m && self.ignore && c < 256 {
            let lo = (prov.locale_lower)(c as u8) as u32;
            let up = (prov.locale_upper)(c as u8) as u32;
            m = self.raw(lo, prov) || self.raw(up, prov);
        }
        m != self.negate
    }
}

#[inline]
pub(crate) fn ascii_word(c: u32) -> bool {
    c < 128 && ((c as u8).is_ascii_alphanumeric() || c == '_' as u32)
}

#[inline]
pub(crate) fn ascii_space(c: u32) -> bool {
    matches!(c, 9..=13 | 32)
}

pub(crate) fn cat_ascii(cat: Category, c: u32) -> bool {
    match cat {
        Category::Digit => (0x30..=0x39).contains(&c),
        Category::NotDigit => !(0x30..=0x39).contains(&c),
        Category::Space => ascii_space(c),
        Category::NotSpace => !ascii_space(c),
        Category::Word => ascii_word(c),
        Category::NotWord => !ascii_word(c),
    }
}

pub(crate) fn cat_unicode(cat: Category, c: u32, prov: &UnicodeProvider) -> bool {
    match cat {
        Category::Digit => (prov.is_digit)(c),
        Category::NotDigit => !(prov.is_digit)(c),
        Category::Space => (prov.is_space)(c),
        Category::NotSpace => !(prov.is_space)(c),
        Category::Word => (prov.is_word)(c),
        Category::NotWord => !(prov.is_word)(c),
    }
}

#[inline]
pub(crate) fn locale_word(c: u32, prov: &UnicodeProvider) -> bool {
    c < 256 && ((prov.locale_is_alnum)(c as u8) || c == '_' as u32)
}

pub(crate) fn cat_locale(cat: Category, c: u32, prov: &UnicodeProvider) -> bool {
    match cat {
        Category::Word => locale_word(c, prov),
        Category::NotWord => !locale_word(c, prov),
        _ => cat_ascii(cat, c),
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FirstSet {
    pub bits: [u64; 4],
    pub high: bool,
}

impl FirstSet {
    #[inline(always)]
    pub fn test(&self, c: u32) -> bool {
        if c < 256 { self.bits[(c >> 6) as usize] >> (c & 63) & 1 != 0 } else { self.high }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Program {
    pub insts: Vec<Inst>,
    pub sets: Vec<CharSet>,
    pub locsets: Vec<LocSet>,
    pub lits: Vec<u32>,
    pub reps: Vec<RepInfo>,
    pub nslots: usize,
    /// Case-sensitive literal every match starts with.
    pub prefix: Vec<u32>,
    /// pc to continue at after the prefix has been matched (if the prefix
    /// can be skipped), else NONE.
    pub prefix_pc: u32,
    pub first: Option<FirstSet>,
    /// Pattern can only match at absolute position 0.
    pub anchored_start: bool,
    /// Minimum width of a match (saturated).
    pub min_width: usize,
    /// Whole pattern is a single literal string without groups.
    pub pure_literal: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Ascii,
    Unicode,
    Locale,
}

pub(crate) struct Compiler<'a> {
    prov: &'a UnicodeProvider,
    is_bytes: bool,
    group_widths: &'a [(u128, u128)],
    gw_opt: Vec<Option<(u128, u128)>>,
    prog: Program,
}

type R<T> = Result<T, CompileError>;

fn combine_flags(flags: u32, add: u32, del: u32) -> u32 {
    let mut f = flags;
    if add & (ASCII | LOCALE | UNICODE) != 0 {
        f &= !(ASCII | LOCALE | UNICODE);
    }
    (f | add) & !del
}

impl<'a> Compiler<'a> {
    pub(crate) fn compile(
        nodes: &[Node],
        flags: u32,
        ngroups: usize,
        group_widths: &'a [(u128, u128)],
        is_bytes: bool,
        prov: &'a UnicodeProvider,
    ) -> R<Program> {
        let mut c = Compiler {
            prov,
            is_bytes,
            group_widths,
            gw_opt: group_widths.iter().map(|&w| Some(w)).collect(),
            prog: Program {
                insts: Vec::new(),
                sets: Vec::new(),
                locsets: Vec::new(),
                lits: Vec::new(),
                reps: Vec::new(),
                nslots: ngroups * 2,
                prefix: Vec::new(),
                prefix_pc: NONE,
                first: None,
                anchored_start: false,
                min_width: 0,
                pure_literal: false,
            },
        };
        // Literal prefix (top level, case-sensitive).
        let mut prefix = Vec::new();
        if flags & IGNORECASE == 0 {
            for n in nodes {
                match n {
                    Node::Literal(ch) => prefix.push(*ch),
                    _ => break,
                }
            }
        }
        let rest = &nodes[prefix.len()..];
        if !prefix.is_empty() {
            c.emit_lit(&prefix);
            c.prog.prefix_pc = c.prog.insts.len() as u32;
            c.prog.prefix = prefix.clone();
        }
        c.seq(rest, flags)?;
        c.emit(Inst::Match);
        c.prog.pure_literal = rest.is_empty() && !prefix.is_empty() && ngroups == 0;
        // analysis
        c.prog.anchored_start = match nodes.first() {
            Some(Node::At(AtCode::BeginningString)) => true,
            Some(Node::At(AtCode::Beginning)) => flags & MULTILINE == 0,
            _ => false,
        };
        let w = width(nodes, &c.gw_opt);
        c.prog.min_width = w.0.min(usize::MAX as u128) as usize;
        if !(flags & LOCALE != 0) {
            let f = c.first_of(nodes, flags);
            if let (Some(bits), false) = (f.0, f.2) {
                c.prog.first = Some(bits);
            }
        }
        c.set_hints();
        Ok(c.prog)
    }

    fn mode(&self, flags: u32) -> Mode {
        if flags & LOCALE != 0 {
            Mode::Locale
        } else if self.is_bytes || flags & ASCII != 0 {
            Mode::Ascii
        } else {
            Mode::Unicode
        }
    }

    fn word_mode(&self, flags: u32) -> WordMode {
        match self.mode(flags) {
            Mode::Ascii => WordMode::Ascii,
            Mode::Unicode => WordMode::Unicode,
            Mode::Locale => WordMode::Locale,
        }
    }

    fn emit(&mut self, i: Inst) -> usize {
        self.prog.insts.push(i);
        self.prog.insts.len() - 1
    }

    fn pc(&self) -> u32 {
        self.prog.insts.len() as u32
    }

    fn emit_lit(&mut self, chars: &[u32]) {
        if chars.len() == 1 {
            self.emit(Inst::Char(chars[0]));
        } else {
            let start = self.prog.lits.len() as u32;
            self.prog.lits.extend_from_slice(chars);
            self.emit(Inst::Lit(start, chars.len() as u32));
        }
    }

    fn add_set(&mut self, s: CharSet) -> u32 {
        self.prog.sets.push(s);
        (self.prog.sets.len() - 1) as u32
    }

    /// Case-equivalence class of `c` under `flags` (empty: only itself).
    fn fold_class(&self, c: u32, flags: u32) -> Vec<u32> {
        if flags & IGNORECASE == 0 {
            return Vec::new();
        }
        match self.mode(flags) {
            Mode::Ascii | Mode::Locale => {
                if c < 128 && (c as u8).is_ascii_alphabetic() {
                    let l = (c as u8).to_ascii_lowercase() as u32;
                    let u = (c as u8).to_ascii_uppercase() as u32;
                    vec![u, l]
                } else {
                    Vec::new()
                }
            }
            Mode::Unicode => (self.prov.case_class)(c).to_vec(),
        }
    }

    /// Build a CharSet (non-locale) from set items.
    fn build_set(&self, items: &[SetItem], negate: bool, flags: u32) -> CharSet {
        let mode = self.mode(flags);
        let ignore = flags & IGNORECASE != 0;
        let mut bits = [0u64; 4];
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        let mut cats: Vec<Category> = Vec::new();
        fn add_range(bits: &mut [u64; 4], ranges: &mut Vec<(u32, u32)>, lo: u32, hi: u32) {
            if lo < 256 {
                for c in lo..=hi.min(255) {
                    bits[(c >> 6) as usize] |= 1 << (c & 63);
                }
            }
            if hi >= 256 {
                ranges.push((lo.max(256), hi));
            }
        }
        for it in items {
            match *it {
                SetItem::Lit(c) => add_range(&mut bits, &mut ranges, c, c),
                SetItem::Range(lo, hi) => add_range(&mut bits, &mut ranges, lo, hi),
                SetItem::Cat(cat) => match mode {
                    Mode::Unicode => {
                        for c in 0..256u32 {
                            if cat_unicode(cat, c, self.prov) {
                                bits[(c >> 6) as usize] |= 1 << (c & 63);
                            }
                        }
                        if !cats.contains(&cat) {
                            cats.push(cat);
                        }
                    }
                    _ => {
                        for c in 0..128u32 {
                            if cat_ascii(cat, c) {
                                bits[(c >> 6) as usize] |= 1 << (c & 63);
                            }
                        }
                        // negative ASCII categories match all non-ASCII chars
                        if matches!(cat, Category::NotDigit | Category::NotSpace | Category::NotWord) {
                            add_range(&mut bits, &mut ranges, 128, 0x10FFFF);
                        }
                    }
                },
            }
        }
        if ignore {
            let mut extra: Vec<u32> = Vec::new();
            match mode {
                Mode::Unicode => {
                    let cased = (self.prov.cased_chars)();
                    for it in items {
                        let (lo, hi) = match *it {
                            SetItem::Lit(c) => (c, c),
                            SetItem::Range(lo, hi) => (lo, hi),
                            SetItem::Cat(_) => continue,
                        };
                        let a = cased.partition_point(|&x| x < lo);
                        let b = cased.partition_point(|&x| x <= hi);
                        for &c in &cased[a..b] {
                            extra.extend_from_slice((self.prov.case_class)(c));
                        }
                    }
                }
                _ => {
                    for it in items {
                        let (lo, hi) = match *it {
                            SetItem::Lit(c) => (c, c),
                            SetItem::Range(lo, hi) => (lo, hi),
                            SetItem::Cat(_) => continue,
                        };
                        for c in lo.max(0x41)..=hi.min(0x7A) {
                            if (c as u8).is_ascii_alphabetic() {
                                extra.push((c as u8).to_ascii_lowercase() as u32);
                                extra.push((c as u8).to_ascii_uppercase() as u32);
                            }
                        }
                    }
                }
            }
            for c in extra {
                add_range(&mut bits, &mut ranges, c, c);
            }
        }
        ranges.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::new();
        for (lo, hi) in ranges {
            if let Some(last) = merged.last_mut() {
                if lo <= last.1.saturating_add(1) {
                    last.1 = last.1.max(hi);
                    continue;
                }
            }
            merged.push((lo, hi));
        }
        if negate {
            for b in bits.iter_mut() {
                *b = !*b;
            }
        }
        CharSet { bits, ranges: merged, cats, negate }
    }

    fn set_inst(&mut self, items: &[SetItem], negate: bool, flags: u32) -> Inst {
        if self.mode(flags) == Mode::Locale {
            let needs_runtime = flags & IGNORECASE != 0
                || items.iter().any(|i| matches!(i, SetItem::Cat(Category::Word | Category::NotWord)));
            if needs_runtime {
                self.prog.locsets.push(LocSet {
                    items: items.to_vec(),
                    negate,
                    ignore: flags & IGNORECASE != 0,
                });
                return Inst::LocSet((self.prog.locsets.len() - 1) as u32);
            }
        }
        let s = self.build_set(items, negate, flags);
        Inst::Set(self.add_set(s))
    }

    fn literal_inst(&mut self, c: u32, negate: bool, flags: u32) -> Inst {
        if flags & IGNORECASE != 0 && self.mode(flags) == Mode::Locale {
            return Inst::LocLit(c, negate);
        }
        let cls = self.fold_class(c, flags);
        if cls.len() <= 1 {
            return if negate { Inst::NotChar(c) } else { Inst::Char(c) };
        }
        if cls.len() == 2 && !negate {
            return Inst::Char2(cls[0], cls[1]);
        }
        let items: Vec<SetItem> = cls.iter().map(|&x| SetItem::Lit(x)).collect();
        let s = self.build_set(&items, negate, flags & !IGNORECASE);
        Inst::Set(self.add_set(s))
    }

    fn seq(&mut self, nodes: &[Node], flags: u32) -> R<()> {
        let mut i = 0;
        while i < nodes.len() {
            // coalesce runs of case-sensitive literals
            if flags & IGNORECASE == 0 {
                if let Node::Literal(_) = nodes[i] {
                    let mut j = i;
                    let mut run = Vec::new();
                    while j < nodes.len() {
                        if let Node::Literal(c) = nodes[j] {
                            run.push(c);
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    self.emit_lit(&run);
                    i = j;
                    continue;
                }
            }
            self.node(&nodes[i], flags)?;
            i += 1;
        }
        Ok(())
    }

    fn node(&mut self, n: &Node, flags: u32) -> R<()> {
        match n {
            Node::Literal(c) => {
                let i = self.literal_inst(*c, false, flags);
                self.emit(i);
            }
            Node::NotLiteral(c) => {
                let i = self.literal_inst(*c, true, flags);
                self.emit(i);
            }
            Node::Any => {
                self.emit(if flags & DOTALL != 0 { Inst::AnyAll } else { Inst::Any });
            }
            Node::In(items, negate) => {
                let i = self.set_inst(items, *negate, flags);
                self.emit(i);
            }
            Node::At(a) => {
                let at = match a {
                    AtCode::Beginning => {
                        if flags & MULTILINE != 0 { At::BeginningLine } else { At::Beginning }
                    }
                    AtCode::BeginningString => At::Beginning,
                    AtCode::End => {
                        if flags & MULTILINE != 0 { At::EndLine } else { At::End }
                    }
                    AtCode::EndString => At::EndString,
                    AtCode::Boundary => At::Boundary(self.word_mode(flags)),
                    AtCode::NonBoundary => At::NonBoundary(self.word_mode(flags)),
                };
                self.emit(Inst::At(at));
            }
            Node::Subpattern { group, add, del, body } => {
                let f = combine_flags(flags, *add, *del);
                if let Some(g) = group {
                    self.emit(Inst::Mark(2 * (*g as u32 - 1)));
                    self.seq(body, f)?;
                    self.emit(Inst::Mark(2 * (*g as u32 - 1) + 1));
                } else {
                    self.seq(body, f)?;
                }
            }
            Node::Branch(alts) => self.branch(alts, flags)?,
            Node::Repeat { min, max, kind, body } => self.repeat(*min, *max, *kind, body, flags)?,
            Node::GroupRef(g) => {
                let mode = if flags & IGNORECASE == 0 {
                    RefMode::Exact
                } else {
                    match self.mode(flags) {
                        Mode::Ascii => RefMode::IgnoreAscii,
                        Mode::Unicode => RefMode::IgnoreUnicode,
                        Mode::Locale => RefMode::IgnoreLocale,
                    }
                };
                self.emit(Inst::GroupRef(*g as u32, mode));
            }
            Node::GroupRefExists { group, yes, no } => {
                let at = self.emit(Inst::GroupExists(*group as u32, 0));
                self.seq(yes, flags)?;
                if let Some(no) = no {
                    let j = self.emit(Inst::Jmp(0));
                    let else_pc = self.pc();
                    self.seq(no, flags)?;
                    let end = self.pc();
                    self.prog.insts[j] = Inst::Jmp(end);
                    self.prog.insts[at] = Inst::GroupExists(*group as u32, else_pc);
                } else {
                    let end = self.pc();
                    self.prog.insts[at] = Inst::GroupExists(*group as u32, end);
                }
            }
            Node::Assert { behind, neg, body } => {
                let mut back = 0u32;
                if *behind {
                    let (lo, hi) = width(body, &self.gw_opt);
                    if lo > MAXCODE {
                        return Err(CompileError::pat_nopos("looks too much behind"));
                    }
                    if lo != hi {
                        return Err(CompileError::pat_nopos(
                            "look-behind requires fixed-width pattern",
                        ));
                    }
                    back = lo as u32;
                }
                let at = self.emit(Inst::Nop);
                self.seq(body, flags)?;
                self.emit(Inst::AssertEnd);
                let exit = self.pc();
                self.prog.insts[at] = Inst::AssertStart { neg: *neg, back, exit };
            }
            Node::Atomic(body) => {
                let at = self.emit(Inst::Nop);
                self.seq(body, flags)?;
                self.emit(Inst::AtomicEnd);
                let exit = self.pc();
                self.prog.insts[at] = Inst::AtomicStart(exit);
            }
            Node::Failure => {
                self.emit(Inst::Fail);
            }
        }
        Ok(())
    }

    fn branch(&mut self, alts: &[Vec<Node>], flags: u32) -> R<()> {
        // A branch of single literals / non-negated sets becomes one set.
        let all_simple = alts.iter().all(|a| {
            a.len() == 1 && matches!(a[0], Node::Literal(_) | Node::In(_, false))
        });
        if all_simple {
            let mut items = Vec::new();
            for a in alts {
                match &a[0] {
                    Node::Literal(c) => items.push(SetItem::Lit(*c)),
                    Node::In(its, _) => items.extend_from_slice(its),
                    _ => unreachable!(),
                }
            }
            let i = self.set_inst(&items, false, flags);
            self.emit(i);
            return Ok(());
        }
        let mut jumps = Vec::new();
        for (k, a) in alts.iter().enumerate() {
            if k + 1 < alts.len() {
                let split = self.emit(Inst::Split(0, 0));
                let first = self.pc();
                self.seq(a, flags)?;
                jumps.push(self.emit(Inst::Jmp(0)));
                let next = self.pc();
                self.prog.insts[split] = Inst::Split(first, next);
            } else {
                self.seq(a, flags)?;
            }
        }
        let end = self.pc();
        for j in jumps {
            self.prog.insts[j] = Inst::Jmp(end);
        }
        Ok(())
    }

    fn repeat(&mut self, min: u32, max: u32, kind: RepKind, body: &[Node], flags: u32) -> R<()> {
        let at = self.emit(Inst::Nop);
        let body_pc = self.pc();
        self.seq(body, flags)?;
        let body_len = self.prog.insts.len() - body_pc as usize;
        if body_len == 1 && self.prog.insts[body_pc as usize].is_single() {
            self.prog.insts[at] = Inst::RepeatOne { min, max, kind, hint: NONE };
            return Ok(());
        }
        if max == 0 {
            // matches the empty string only; the body is never run
            self.prog.insts.truncate(body_pc as usize);
            return Ok(());
        }
        let (wlo, _) = width(body, &self.gw_opt);
        let nonempty = wlo > 0;
        match kind {
            RepKind::Possessive => {
                self.emit(Inst::PossEnd);
                let exit = self.pc();
                let r = self.add_rep(RepInfo { min, max, greedy: true, body: body_pc, until: NONE, exit });
                self.prog.insts[at] = Inst::PossStart(r);
            }
            RepKind::Greedy | RepKind::Lazy => {
                let greedy = kind == RepKind::Greedy;
                if min == 1 && max == 1 {
                    // plain sequence
                } else if min == 0 && max == 1 {
                    let exit = self.pc();
                    self.prog.insts[at] =
                        if greedy { Inst::Split(body_pc, exit) } else { Inst::Split(exit, body_pc) };
                } else if nonempty && max == MAXREPEAT && min <= 1 {
                    if min == 0 {
                        // at: Split(body, exit); body; Jmp(at)
                        self.emit(Inst::Jmp(at as u32));
                        let exit = self.pc();
                        self.prog.insts[at] = if greedy {
                            Inst::Split(body_pc, exit)
                        } else {
                            Inst::Split(exit, body_pc)
                        };
                    } else {
                        // body; Split(body, exit)
                        let sp = self.emit(Inst::Nop);
                        let exit = self.pc();
                        self.prog.insts[sp] = if greedy {
                            Inst::Split(body_pc, exit)
                        } else {
                            Inst::Split(exit, body_pc)
                        };
                    }
                } else {
                    let until = self.pc();
                    let r = self.add_rep(RepInfo { min, max, greedy, body: body_pc, until, exit: until + 1 });
                    self.emit(Inst::Until(r));
                    self.prog.insts[at] = Inst::RepStart(r);
                }
            }
        }
        Ok(())
    }

    fn add_rep(&mut self, r: RepInfo) -> u32 {
        self.prog.reps.push(r);
        (self.prog.reps.len() - 1) as u32
    }

    /// Fill in `hint` for RepeatOne whose tail starts with a plain char.
    fn set_hints(&mut self) {
        let n = self.prog.insts.len();
        for pc in 0..n {
            if let Inst::RepeatOne { min, max, kind, .. } = self.prog.insts[pc] {
                if kind == RepKind::Possessive || pc + 2 >= n {
                    continue;
                }
                let hint = match self.prog.insts[pc + 2] {
                    Inst::Char(c) => c,
                    Inst::Lit(s, _) => self.prog.lits[s as usize],
                    _ => NONE,
                };
                self.prog.insts[pc] = Inst::RepeatOne { min, max, kind, hint };
            }
        }
    }

    // ---------------------------------------------------------------
    // first-character analysis: (set or None=any, _, nullable)

    fn first_of(&self, nodes: &[Node], flags: u32) -> (Option<FirstSet>, (), bool) {
        let mut acc = FirstSet { bits: [0; 4], high: false };
        for n in nodes {
            let (s, _, nullable) = self.first_node(n, flags);
            match s {
                None => return (None, (), true),
                Some(s) => {
                    for k in 0..4 {
                        acc.bits[k] |= s.bits[k];
                    }
                    acc.high |= s.high;
                }
            }
            if !nullable {
                return (Some(acc), (), false);
            }
        }
        (Some(acc), (), true)
    }

    fn first_node(&self, n: &Node, flags: u32) -> (Option<FirstSet>, (), bool) {
        let empty = FirstSet { bits: [0; 4], high: false };
        match n {
            Node::Literal(c) => {
                let mut cls = self.fold_class(*c, flags);
                if cls.is_empty() {
                    cls.push(*c);
                }
                let mut s = empty;
                for x in cls {
                    if x < 256 {
                        s.bits[(x >> 6) as usize] |= 1 << (x & 63);
                    } else {
                        s.high = true;
                    }
                }
                (Some(s), (), false)
            }
            Node::NotLiteral(_) | Node::Any => (None, (), false),
            Node::In(items, negate) => {
                let cs = self.build_set(items, *negate, flags);
                let high = cs.negate || !cs.ranges.is_empty() || !cs.cats.is_empty();
                (Some(FirstSet { bits: cs.bits, high }), (), false)
            }
            Node::At(_) | Node::Assert { .. } => (Some(empty), (), true),
            Node::Failure => (Some(empty), (), false),
            Node::Subpattern { add, del, body, .. } => {
                self.first_of(body, combine_flags(flags, *add, *del))
            }
            Node::Atomic(body) => self.first_of(body, flags),
            Node::Branch(alts) => {
                let mut acc = empty;
                let mut nullable = false;
                for a in alts {
                    let (s, _, nl) = self.first_of(a, flags);
                    match s {
                        None => return (None, (), true),
                        Some(s) => {
                            for k in 0..4 {
                                acc.bits[k] |= s.bits[k];
                            }
                            acc.high |= s.high;
                        }
                    }
                    nullable |= nl;
                }
                (Some(acc), (), nullable)
            }
            Node::Repeat { min, body, .. } => {
                let (s, _, nl) = self.first_of(body, flags);
                (s, (), nl || *min == 0)
            }
            Node::GroupRef(_) | Node::GroupRefExists { .. } => (None, (), true),
        }
    }

    #[allow(dead_code)]
    fn unused(&self) -> &[(u128, u128)] {
        self.group_widths
    }
}
