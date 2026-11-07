//! `re.DEBUG` support (tail2).
//!
//! CPython prints two things when a pattern is compiled with `re.DEBUG`: the
//! parse tree of the pattern and a listing of the compiled `_sre` code.  Our
//! matcher runs its own program format, so this module produces both views
//! independently of the real compiler:
//!
//! 1. the AST is reshaped into the tree CPython's parser builds (non-capturing
//!    groups without flags are spliced into their parent, alternatives share
//!    their common leading items, single-character alternatives become a set,
//!    set members are de-duplicated);
//! 2. that tree is printed in the parse-dump format;
//! 3. it is translated into CPython's code-word layout (INFO block, MARK,
//!    BRANCH/JUMP chains, REPEAT_ONE, charsets compressed to literal runs or
//!    bitmaps, ...);
//! 4. the code words are listed with offsets, nesting and jump targets.
//!
//! Nothing here influences matching; it only runs when `re.DEBUG` is given.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::error::CompileError;
use crate::flags::*;
use crate::parser::{py_repr, width, AtCode, Category, Node, Parsed, RepKind, SetItem, MAXWIDTH};
use crate::unicode::UnicodeProvider;
use crate::MAXREPEAT;

const MAXCODE: u32 = u32::MAX;
const INFO_PREFIX: u32 = 1;
const INFO_LITERAL: u32 = 2;
const INFO_CHARSET: u32 = 4;

// ------------------------------------------------------------------ opcodes

/// Operation words of the listing.  Only the names are ever shown; the
/// numeric values just need FAILURE == 0 (it terminates BRANCH chains).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
enum Op {
    Failure = 0,
    Success,
    Any,
    AnyAll,
    Assert,
    AssertNot,
    At,
    Branch,
    Category,
    Charset,
    BigCharset,
    GroupRef,
    GroupRefExists,
    In,
    Info,
    Jump,
    Literal,
    Mark,
    MaxUntil,
    MinUntil,
    NotLiteral,
    Negate,
    Range,
    Repeat,
    RepeatOne,
    Subpattern,
    MinRepeatOne,
    AtomicGroup,
    PossessiveRepeat,
    PossessiveRepeatOne,
    GroupRefIgnore,
    InIgnore,
    LiteralIgnore,
    NotLiteralIgnore,
    GroupRefLocIgnore,
    InLocIgnore,
    LiteralLocIgnore,
    NotLiteralLocIgnore,
    GroupRefUniIgnore,
    InUniIgnore,
    LiteralUniIgnore,
    NotLiteralUniIgnore,
    RangeUniIgnore,
}

const ALL_OPS: [Op; 43] = [
    Op::Failure, Op::Success, Op::Any, Op::AnyAll, Op::Assert, Op::AssertNot, Op::At,
    Op::Branch, Op::Category, Op::Charset, Op::BigCharset, Op::GroupRef, Op::GroupRefExists,
    Op::In, Op::Info, Op::Jump, Op::Literal, Op::Mark, Op::MaxUntil, Op::MinUntil,
    Op::NotLiteral, Op::Negate, Op::Range, Op::Repeat, Op::RepeatOne, Op::Subpattern,
    Op::MinRepeatOne, Op::AtomicGroup, Op::PossessiveRepeat, Op::PossessiveRepeatOne,
    Op::GroupRefIgnore, Op::InIgnore, Op::LiteralIgnore, Op::NotLiteralIgnore,
    Op::GroupRefLocIgnore, Op::InLocIgnore, Op::LiteralLocIgnore, Op::NotLiteralLocIgnore,
    Op::GroupRefUniIgnore, Op::InUniIgnore, Op::LiteralUniIgnore, Op::NotLiteralUniIgnore,
    Op::RangeUniIgnore,
];

impl Op {
    fn name(self) -> &'static str {
        match self {
            Op::Failure => "FAILURE",
            Op::Success => "SUCCESS",
            Op::Any => "ANY",
            Op::AnyAll => "ANY_ALL",
            Op::Assert => "ASSERT",
            Op::AssertNot => "ASSERT_NOT",
            Op::At => "AT",
            Op::Branch => "BRANCH",
            Op::Category => "CATEGORY",
            Op::Charset => "CHARSET",
            Op::BigCharset => "BIGCHARSET",
            Op::GroupRef => "GROUPREF",
            Op::GroupRefExists => "GROUPREF_EXISTS",
            Op::In => "IN",
            Op::Info => "INFO",
            Op::Jump => "JUMP",
            Op::Literal => "LITERAL",
            Op::Mark => "MARK",
            Op::MaxUntil => "MAX_UNTIL",
            Op::MinUntil => "MIN_UNTIL",
            Op::NotLiteral => "NOT_LITERAL",
            Op::Negate => "NEGATE",
            Op::Range => "RANGE",
            Op::Repeat => "REPEAT",
            Op::RepeatOne => "REPEAT_ONE",
            Op::Subpattern => "SUBPATTERN",
            Op::MinRepeatOne => "MIN_REPEAT_ONE",
            Op::AtomicGroup => "ATOMIC_GROUP",
            Op::PossessiveRepeat => "POSSESSIVE_REPEAT",
            Op::PossessiveRepeatOne => "POSSESSIVE_REPEAT_ONE",
            Op::GroupRefIgnore => "GROUPREF_IGNORE",
            Op::InIgnore => "IN_IGNORE",
            Op::LiteralIgnore => "LITERAL_IGNORE",
            Op::NotLiteralIgnore => "NOT_LITERAL_IGNORE",
            Op::GroupRefLocIgnore => "GROUPREF_LOC_IGNORE",
            Op::InLocIgnore => "IN_LOC_IGNORE",
            Op::LiteralLocIgnore => "LITERAL_LOC_IGNORE",
            Op::NotLiteralLocIgnore => "NOT_LITERAL_LOC_IGNORE",
            Op::GroupRefUniIgnore => "GROUPREF_UNI_IGNORE",
            Op::InUniIgnore => "IN_UNI_IGNORE",
            Op::LiteralUniIgnore => "LITERAL_UNI_IGNORE",
            Op::NotLiteralUniIgnore => "NOT_LITERAL_UNI_IGNORE",
            Op::RangeUniIgnore => "RANGE_UNI_IGNORE",
        }
    }
    fn from_word(w: u32) -> Option<Op> {
        ALL_OPS.get(w as usize).copied()
    }
}

/// Assertion codes (the AT operand), in listing order.
const AT_NAMES: [&str; 12] = [
    "AT_BEGINNING", "AT_BEGINNING_LINE", "AT_BEGINNING_STRING", "AT_BOUNDARY",
    "AT_NON_BOUNDARY", "AT_END", "AT_END_LINE", "AT_END_STRING", "AT_LOC_BOUNDARY",
    "AT_LOC_NON_BOUNDARY", "AT_UNI_BOUNDARY", "AT_UNI_NON_BOUNDARY",
];

fn at_index(name: &str) -> u32 {
    AT_NAMES.iter().position(|n| *n == name).unwrap() as u32
}

/// Category codes (the CATEGORY operand).
const CAT_NAMES: [&str; 18] = [
    "CATEGORY_DIGIT", "CATEGORY_NOT_DIGIT", "CATEGORY_SPACE", "CATEGORY_NOT_SPACE",
    "CATEGORY_WORD", "CATEGORY_NOT_WORD", "CATEGORY_LINEBREAK", "CATEGORY_NOT_LINEBREAK",
    "CATEGORY_LOC_WORD", "CATEGORY_LOC_NOT_WORD", "CATEGORY_UNI_DIGIT",
    "CATEGORY_UNI_NOT_DIGIT", "CATEGORY_UNI_SPACE", "CATEGORY_UNI_NOT_SPACE",
    "CATEGORY_UNI_WORD", "CATEGORY_UNI_NOT_WORD", "CATEGORY_UNI_LINEBREAK",
    "CATEGORY_UNI_NOT_LINEBREAK",
];

fn cat_index(name: &str) -> u32 {
    CAT_NAMES.iter().position(|n| *n == name).unwrap() as u32
}

fn at_name(a: AtCode) -> &'static str {
    match a {
        AtCode::Beginning => "AT_BEGINNING",
        AtCode::BeginningString => "AT_BEGINNING_STRING",
        AtCode::End => "AT_END",
        AtCode::EndString => "AT_END_STRING",
        AtCode::Boundary => "AT_BOUNDARY",
        AtCode::NonBoundary => "AT_NON_BOUNDARY",
    }
}

fn cat_name(c: Category) -> &'static str {
    match c {
        Category::Digit => "CATEGORY_DIGIT",
        Category::NotDigit => "CATEGORY_NOT_DIGIT",
        Category::Space => "CATEGORY_SPACE",
        Category::NotSpace => "CATEGORY_NOT_SPACE",
        Category::Word => "CATEGORY_WORD",
        Category::NotWord => "CATEGORY_NOT_WORD",
    }
}

// ------------------------------------------------------- tree reshaping

/// Leaf nodes compare by value when alternatives are factored; anything with
/// a nested sequence never compares equal (CPython compares those by identity).
fn same_leaf(a: &Node, b: &Node) -> bool {
    match (a, b) {
        (Node::Literal(_), _)
        | (Node::NotLiteral(_), _)
        | (Node::Any, _)
        | (Node::In(..), _)
        | (Node::At(_), _)
        | (Node::GroupRef(_), _)
        | (Node::Failure, _) => a == b,
        _ => false,
    }
}

fn uniq_items(items: &[SetItem]) -> Vec<SetItem> {
    let mut out: Vec<SetItem> = Vec::with_capacity(items.len());
    for it in items {
        if !out.contains(it) {
            out.push(*it);
        }
    }
    out
}

/// Reshape one sequence (and everything below it) the way CPython's parser
/// shapes it.
fn reshape_seq(nodes: &[Node]) -> Vec<Node> {
    let mut out = Vec::with_capacity(nodes.len());
    for n in nodes {
        match n {
            Node::Subpattern { group: None, add: 0, del: 0, body } => {
                out.extend(reshape_seq(body));
            }
            Node::Branch(alts) => {
                let alts: Vec<Vec<Node>> = alts.iter().map(|a| reshape_seq(a)).collect();
                reshape_branch(alts, &mut out);
            }
            _ => out.push(reshape_node(n)),
        }
    }
    out
}

fn reshape_node(n: &Node) -> Node {
    match n {
        Node::In(items, neg) => {
            let items = uniq_items(items);
            if items.len() == 1 {
                if let SetItem::Lit(c) = items[0] {
                    return if *neg { Node::NotLiteral(c) } else { Node::Literal(c) };
                }
            }
            Node::In(items, *neg)
        }
        Node::Subpattern { group, add, del, body } => {
            Node::Subpattern { group: *group, add: *add, del: *del, body: reshape_seq(body) }
        }
        Node::Repeat { min, max, kind, body } => {
            Node::Repeat { min: *min, max: *max, kind: *kind, body: reshape_seq(body) }
        }
        Node::GroupRefExists { group, yes, no } => Node::GroupRefExists {
            group: *group,
            yes: reshape_seq(yes),
            no: no.as_ref().map(|v| reshape_seq(v)).filter(|v| !v.is_empty()),
        },
        Node::Assert { behind, neg, body } => {
            Node::Assert { behind: *behind, neg: *neg, body: reshape_seq(body) }
        }
        Node::Atomic(body) => Node::Atomic(reshape_seq(body)),
        Node::Branch(alts) => Node::Branch(alts.iter().map(|a| reshape_seq(a)).collect()),
        other => other.clone(),
    }
}

/// Alternatives: hoist the common leading items, then turn a choice between
/// single characters / positive sets into one set.
fn reshape_branch(mut alts: Vec<Vec<Node>>, out: &mut Vec<Node>) {
    loop {
        let Some(first) = alts.first().and_then(|a| a.first()).cloned() else { break };
        if !alts.iter().all(|a| a.first().is_some_and(|x| same_leaf(x, &first))) {
            break;
        }
        for a in alts.iter_mut() {
            a.remove(0);
        }
        out.push(first);
    }
    let mut members: Vec<SetItem> = Vec::new();
    let mut all_chars = true;
    for a in &alts {
        match a.as_slice() {
            [Node::Literal(c)] => members.push(SetItem::Lit(*c)),
            [Node::In(items, false)] => members.extend_from_slice(items),
            _ => {
                all_chars = false;
                break;
            }
        }
    }
    if all_chars {
        out.push(Node::In(uniq_items(&members), false));
    } else {
        out.push(Node::Branch(alts));
    }
}

// ------------------------------------------------------- parse-tree dump

fn set_item_text(it: &SetItem) -> String {
    match it {
        SetItem::Lit(c) => format!("LITERAL {c}"),
        SetItem::Range(a, b) => format!("RANGE ({a}, {b})"),
        SetItem::Cat(c) => format!("CATEGORY {}", cat_name(*c)),
    }
}

fn repeat_bound(v: u32) -> String {
    if v == MAXREPEAT { "MAXREPEAT".to_string() } else { v.to_string() }
}

fn dump_tree(nodes: &[Node], level: usize, out: &mut String) {
    let pad = "  ".repeat(level);
    for n in nodes {
        match n {
            Node::Literal(c) => {
                let _ = writeln!(out, "{pad}LITERAL {c}");
            }
            Node::NotLiteral(c) => {
                let _ = writeln!(out, "{pad}NOT_LITERAL {c}");
            }
            Node::Any => {
                let _ = writeln!(out, "{pad}ANY None");
            }
            Node::At(a) => {
                let _ = writeln!(out, "{pad}AT {}", at_name(*a));
            }
            Node::GroupRef(g) => {
                let _ = writeln!(out, "{pad}GROUPREF {g}");
            }
            Node::Failure => {
                let _ = writeln!(out, "{pad}FAILURE");
            }
            Node::In(items, neg) => {
                let _ = writeln!(out, "{pad}IN");
                if *neg {
                    let _ = writeln!(out, "{pad}  NEGATE None");
                }
                for it in items {
                    let _ = writeln!(out, "{pad}  {}", set_item_text(it));
                }
            }
            Node::Branch(alts) => {
                let _ = writeln!(out, "{pad}BRANCH");
                for (i, a) in alts.iter().enumerate() {
                    if i > 0 {
                        let _ = writeln!(out, "{pad}OR");
                    }
                    dump_tree(a, level + 1, out);
                }
            }
            Node::GroupRefExists { group, yes, no } => {
                let _ = writeln!(out, "{pad}GROUPREF_EXISTS {group}");
                dump_tree(yes, level + 1, out);
                if let Some(no) = no {
                    if !no.is_empty() {
                        let _ = writeln!(out, "{pad}ELSE");
                        dump_tree(no, level + 1, out);
                    }
                }
            }
            Node::Subpattern { group, add, del, body } => {
                let g = group.map_or("None".to_string(), |g| g.to_string());
                let _ = writeln!(out, "{pad}SUBPATTERN {g} {add} {del}");
                dump_tree(body, level + 1, out);
            }
            Node::Repeat { min, max, kind, body } => {
                let name = match kind {
                    RepKind::Greedy => "MAX_REPEAT",
                    RepKind::Lazy => "MIN_REPEAT",
                    RepKind::Possessive => "POSSESSIVE_REPEAT",
                };
                let _ = writeln!(out, "{pad}{name} {} {}", repeat_bound(*min), repeat_bound(*max));
                dump_tree(body, level + 1, out);
            }
            Node::Assert { behind, neg, body } => {
                let name = if *neg { "ASSERT_NOT" } else { "ASSERT" };
                let dir = if *behind { -1 } else { 1 };
                let _ = writeln!(out, "{pad}{name} {dir}");
                dump_tree(body, level + 1, out);
            }
            Node::Atomic(body) => {
                let _ = writeln!(out, "{pad}ATOMIC_GROUP");
                dump_tree(body, level + 1, out);
            }
        }
    }
}

// ------------------------------------------------------- code emission

/// One member of a compiled character set.
#[derive(Clone, Debug, PartialEq)]
enum Member {
    Negate,
    Lit(u32),
    Range(u32, u32),
    RangeUniIgnore(u32, u32),
    Cat(Category),
    Bitmap(Vec<u32>),
    Big(Vec<u32>),
}

/// Case handling in effect for one sequence.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CaseMode {
    Exact,
    Locale,
    Ascii,
    Unicode,
}

struct Emitter<'a> {
    code: Vec<u32>,
    prov: &'a UnicodeProvider,
    gw: Vec<Option<(u128, u128)>>,
}

fn case_mode(flags: u32) -> CaseMode {
    if flags & IGNORECASE == 0 {
        CaseMode::Exact
    } else if flags & LOCALE != 0 {
        CaseMode::Locale
    } else if flags & UNICODE != 0 {
        CaseMode::Unicode
    } else {
        CaseMode::Ascii
    }
}

fn merge_flags(flags: u32, add: u32, del: u32) -> u32 {
    let type_flags = ASCII | LOCALE | UNICODE;
    let base = if add & type_flags != 0 { flags & !type_flags } else { flags };
    (base | add) & !del
}

/// A repeat body that matches exactly one character with one operation.
fn single_unit(body: &[Node]) -> bool {
    match body {
        [Node::Literal(_)] | [Node::NotLiteral(_)] | [Node::Any] | [Node::In(..)] => true,
        [Node::Subpattern { group: None, body, .. }] => single_unit(body),
        _ => false,
    }
}

fn cap(v: u128) -> u32 {
    if v >= MAXCODE as u128 { MAXCODE } else { v as u32 }
}

impl<'a> Emitter<'a> {
    fn w(&mut self, v: u32) {
        self.code.push(v);
    }
    fn op(&mut self, o: Op) {
        self.code.push(o as u32);
    }
    /// Reserve a skip word; returns its position.
    fn hole(&mut self) -> usize {
        self.code.push(0);
        self.code.len() - 1
    }
    /// Make the skip word at `at` point at the current end.
    fn patch(&mut self, at: usize) {
        self.code[at] = (self.code.len() - at) as u32;
    }

    fn iscased(&self, mode: CaseMode, c: u32) -> bool {
        match mode {
            CaseMode::Ascii => c < 128 && (c as u8).is_ascii_alphabetic(),
            CaseMode::Unicode => (self.prov.lower)(c) != c || (self.prov.upper)(c) != c,
            _ => false,
        }
    }
    fn lower(&self, mode: CaseMode, c: u32) -> u32 {
        match mode {
            CaseMode::Ascii => {
                if c < 128 { (c as u8).to_ascii_lowercase() as u32 } else { c }
            }
            CaseMode::Unicode => (self.prov.lower)(c),
            _ => c,
        }
    }
    /// Other lowercase characters that match `lo` under Unicode IGNORECASE.
    fn extra_lowers(&self, mode: CaseMode, lo: u32) -> Vec<u32> {
        if mode != CaseMode::Unicode {
            return Vec::new();
        }
        let mut v: Vec<u32> = (self.prov.case_class)(lo)
            .iter()
            .map(|&x| (self.prov.lower)(x))
            .filter(|&x| x != lo)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    fn seq(&mut self, nodes: &[Node], flags: u32) -> Result<(), CompileError> {
        let mode = case_mode(flags);
        for n in nodes {
            match n {
                Node::Literal(c) | Node::NotLiteral(c) => {
                    let neg = matches!(n, Node::NotLiteral(_));
                    self.literal(*c, neg, mode);
                }
                Node::In(items, neg) => {
                    let mut src: Vec<Member> = Vec::new();
                    if *neg {
                        src.push(Member::Negate);
                    }
                    for it in items {
                        src.push(match it {
                            SetItem::Lit(c) => Member::Lit(*c),
                            SetItem::Range(a, b) => Member::Range(*a, *b),
                            SetItem::Cat(c) => Member::Cat(*c),
                        });
                    }
                    let (members, cased) = self.optimize_set(&src, mode);
                    let o = match mode {
                        CaseMode::Locale => Op::InLocIgnore,
                        _ if !cased => Op::In,
                        CaseMode::Ascii => Op::InIgnore,
                        _ => Op::InUniIgnore,
                    };
                    self.op(o);
                    let h = self.hole();
                    self.members(&members, flags);
                    self.patch(h);
                }
                Node::Any => self.op(if flags & DOTALL != 0 { Op::AnyAll } else { Op::Any }),
                Node::Repeat { min, max, kind, body } => {
                    if single_unit(body) {
                        self.op(match kind {
                            RepKind::Greedy => Op::RepeatOne,
                            RepKind::Lazy => Op::MinRepeatOne,
                            RepKind::Possessive => Op::PossessiveRepeatOne,
                        });
                        let h = self.hole();
                        self.w(*min);
                        self.w(*max);
                        self.seq(body, flags)?;
                        self.op(Op::Success);
                        self.patch(h);
                    } else {
                        self.op(if *kind == RepKind::Possessive {
                            Op::PossessiveRepeat
                        } else {
                            Op::Repeat
                        });
                        let h = self.hole();
                        self.w(*min);
                        self.w(*max);
                        self.seq(body, flags)?;
                        self.patch(h);
                        self.op(match kind {
                            RepKind::Greedy => Op::MaxUntil,
                            RepKind::Lazy => Op::MinUntil,
                            RepKind::Possessive => Op::Success,
                        });
                    }
                }
                Node::Subpattern { group, add, del, body } => {
                    if let Some(g) = group {
                        self.op(Op::Mark);
                        self.w(((*g - 1) * 2) as u32);
                    }
                    self.seq(body, merge_flags(flags, *add, *del))?;
                    if let Some(g) = group {
                        self.op(Op::Mark);
                        self.w(((*g - 1) * 2 + 1) as u32);
                    }
                }
                Node::Atomic(body) => {
                    self.op(Op::AtomicGroup);
                    let h = self.hole();
                    self.seq(body, flags)?;
                    self.op(Op::Success);
                    self.patch(h);
                }
                Node::Assert { behind, neg, body } => {
                    self.op(if *neg { Op::AssertNot } else { Op::Assert });
                    let h = self.hole();
                    if *behind {
                        let (lo, hi) = width(body, &self.gw);
                        if lo > MAXCODE as u128 {
                            return Err(CompileError::Value("looks too much behind".into()));
                        }
                        if lo != hi {
                            return Err(CompileError::Value(
                                "look-behind requires fixed-width pattern".into(),
                            ));
                        }
                        self.w(lo as u32);
                    } else {
                        self.w(0);
                    }
                    self.seq(body, flags)?;
                    self.op(Op::Success);
                    self.patch(h);
                }
                Node::At(a) => {
                    let mut name = at_name(*a);
                    if flags & MULTILINE != 0 {
                        name = match name {
                            "AT_BEGINNING" => "AT_BEGINNING_LINE",
                            "AT_END" => "AT_END_LINE",
                            other => other,
                        };
                    }
                    if flags & LOCALE != 0 {
                        name = match name {
                            "AT_BOUNDARY" => "AT_LOC_BOUNDARY",
                            "AT_NON_BOUNDARY" => "AT_LOC_NON_BOUNDARY",
                            other => other,
                        };
                    } else if flags & UNICODE != 0 {
                        name = match name {
                            "AT_BOUNDARY" => "AT_UNI_BOUNDARY",
                            "AT_NON_BOUNDARY" => "AT_UNI_NON_BOUNDARY",
                            other => other,
                        };
                    }
                    self.op(Op::At);
                    self.w(at_index(name));
                }
                Node::Branch(alts) => {
                    self.op(Op::Branch);
                    let mut jumps = Vec::with_capacity(alts.len());
                    for a in alts {
                        let h = self.hole();
                        self.seq(a, flags)?;
                        self.op(Op::Jump);
                        jumps.push(self.hole());
                        self.patch(h);
                    }
                    self.op(Op::Failure);
                    for j in jumps {
                        self.patch(j);
                    }
                }
                Node::GroupRef(g) => {
                    self.op(match mode {
                        CaseMode::Exact => Op::GroupRef,
                        CaseMode::Locale => Op::GroupRefLocIgnore,
                        CaseMode::Unicode => Op::GroupRefUniIgnore,
                        CaseMode::Ascii => Op::GroupRefIgnore,
                    });
                    self.w((*g - 1) as u32);
                }
                Node::GroupRefExists { group, yes, no } => {
                    self.op(Op::GroupRefExists);
                    self.w((*group - 1) as u32);
                    // the yes-skip counts from the group word (one before the skip word)
                    let yes_hole = self.hole();
                    self.seq(yes, flags)?;
                    match no {
                        Some(no) if !no.is_empty() => {
                            self.op(Op::Jump);
                            let no_hole = self.hole();
                            self.patch(yes_hole);
                            self.code[yes_hole] += 1;
                            self.seq(no, flags)?;
                            self.patch(no_hole);
                        }
                        _ => {
                            self.patch(yes_hole);
                            self.code[yes_hole] += 1;
                        }
                    }
                }
                Node::Failure => self.op(Op::Failure),
            }
        }
        Ok(())
    }

    fn literal(&mut self, c: u32, neg: bool, mode: CaseMode) {
        match mode {
            CaseMode::Exact => {
                self.op(if neg { Op::NotLiteral } else { Op::Literal });
                self.w(c);
            }
            CaseMode::Locale => {
                self.op(if neg { Op::NotLiteralLocIgnore } else { Op::LiteralLocIgnore });
                self.w(c);
            }
            _ if !self.iscased(mode, c) => {
                self.op(if neg { Op::NotLiteral } else { Op::Literal });
                self.w(c);
            }
            _ => {
                let lo = self.lower(mode, c);
                let extra = self.extra_lowers(mode, lo);
                if mode == CaseMode::Ascii {
                    self.op(if neg { Op::NotLiteralIgnore } else { Op::LiteralIgnore });
                    self.w(lo);
                } else if extra.is_empty() {
                    self.op(if neg { Op::NotLiteralUniIgnore } else { Op::LiteralUniIgnore });
                    self.w(lo);
                } else {
                    self.op(Op::InUniIgnore);
                    let h = self.hole();
                    if neg {
                        self.op(Op::Negate);
                    }
                    for k in std::iter::once(lo).chain(extra) {
                        self.op(Op::Literal);
                        self.w(k);
                    }
                    self.op(Op::Failure);
                    self.patch(h);
                }
            }
        }
    }

    /// Compress a set: characters go into a membership map, which becomes at
    /// most two literal/range runs, a 256-bit bitmap, or a chunked big
    /// charset.  Categories and astral members are kept as they are.
    /// Returns the members and whether case folding applies.
    fn optimize_set(&self, src: &[Member], mode: CaseMode) -> (Vec<Member>, bool) {
        let folding = matches!(mode, CaseMode::Ascii | CaseMode::Unicode);
        let mut head: Vec<Member> = Vec::new();
        let mut tail: Vec<Member> = Vec::new();
        let mut map: Vec<bool> = vec![false; 256];
        let mut cased = false;
        let mark = |map: &mut Vec<bool>, c: u32| -> bool {
            if c > 0xFFFF {
                return false;
            }
            if c as usize >= map.len() {
                map.resize(0x10000, false);
            }
            map[c as usize] = true;
            true
        };
        for m in src {
            match m {
                Member::Negate => head.push(Member::Negate),
                Member::Lit(c) | Member::Range(c, _) => {
                    let (lo, hi) = match m {
                        Member::Range(a, b) => (*a, *b),
                        _ => (*c, *c),
                    };
                    if hi > 0xFFFF {
                        // astral members stay explicit
                        if folding {
                            cased = true;
                            tail.push(match m {
                                Member::Range(a, b) => Member::RangeUniIgnore(*a, *b),
                                other => other.clone(),
                            });
                        } else {
                            tail.push(m.clone());
                        }
                        continue;
                    }
                    for ch in lo..=hi {
                        if folding {
                            let l = self.lower(mode, ch);
                            mark(&mut map, l);
                            for k in self.extra_lowers(mode, l) {
                                mark(&mut map, k);
                            }
                            if !cased && self.iscased(mode, ch) {
                                cased = true;
                            }
                        } else {
                            mark(&mut map, ch);
                        }
                    }
                }
                other => tail.push(other.clone()),
            }
        }
        // at most two runs of set characters -> literals / ranges
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut i = 0;
        let mut too_many = false;
        while i < map.len() {
            if !map[i] {
                i += 1;
                continue;
            }
            if runs.len() == 2 {
                too_many = true;
                break;
            }
            let start = i;
            while i < map.len() && map[i] {
                i += 1;
            }
            runs.push((start, i));
        }
        if !too_many {
            let mut out = head;
            for (a, b) in runs {
                if b - a == 1 {
                    out.push(Member::Lit(a as u32));
                } else {
                    out.push(Member::Range(a as u32, (b - 1) as u32));
                }
            }
            out.extend(tail);
            if cased || out.len() < src.len() {
                return (out, cased);
            }
            return (src.to_vec(), cased);
        }
        let mut out = head;
        if map.len() == 256 {
            out.push(Member::Bitmap(bitmap_words(&map)));
        } else {
            // 256 chunks of 256 characters; identical chunks are stored once
            let mut chunk_ids: Vec<u8> = Vec::with_capacity(256);
            let mut chunks: Vec<&[bool]> = Vec::new();
            for blk in map.chunks(256) {
                let id = match chunks.iter().position(|c| *c == blk) {
                    Some(p) => p,
                    None => {
                        chunks.push(blk);
                        chunks.len() - 1
                    }
                };
                chunk_ids.push(id as u8);
            }
            let mut data = vec![chunks.len() as u32];
            for quad in chunk_ids.chunks(4) {
                data.push(u32::from_le_bytes([quad[0], quad[1], quad[2], quad[3]]));
            }
            for c in &chunks {
                data.extend(bitmap_words(c));
            }
            out.push(Member::Big(data));
        }
        out.extend(tail);
        (out, cased)
    }

    fn members(&mut self, members: &[Member], flags: u32) {
        for m in members {
            match m {
                Member::Negate => self.op(Op::Negate),
                Member::Lit(c) => {
                    self.op(Op::Literal);
                    self.w(*c);
                }
                Member::Range(a, b) => {
                    self.op(Op::Range);
                    self.w(*a);
                    self.w(*b);
                }
                Member::RangeUniIgnore(a, b) => {
                    self.op(Op::RangeUniIgnore);
                    self.w(*a);
                    self.w(*b);
                }
                Member::Bitmap(words) => {
                    self.op(Op::Charset);
                    self.code.extend_from_slice(words);
                }
                Member::Big(words) => {
                    self.op(Op::BigCharset);
                    self.code.extend_from_slice(words);
                }
                Member::Cat(c) => {
                    let base = cat_name(*c);
                    let name = if flags & LOCALE != 0 {
                        match base {
                            "CATEGORY_WORD" => "CATEGORY_LOC_WORD".to_string(),
                            "CATEGORY_NOT_WORD" => "CATEGORY_LOC_NOT_WORD".to_string(),
                            other => other.to_string(),
                        }
                    } else if flags & UNICODE != 0 {
                        base.replacen("CATEGORY_", "CATEGORY_UNI_", 1)
                    } else {
                        base.to_string()
                    };
                    self.op(Op::Category);
                    self.w(cat_index(&name));
                }
            }
        }
        self.op(Op::Failure);
    }

    // ---- INFO block

    /// Literal characters every match starts with.  Returns (prefix, how many
    /// of them can be skipped by the search before running the code, whether
    /// the whole sequence is that literal).
    fn literal_prefix(&self, nodes: &[Node], flags: u32) -> (Vec<u32>, Option<usize>, bool) {
        let mode = case_mode(flags);
        let mut prefix: Vec<u32> = Vec::new();
        let mut skip: Option<usize> = None;
        for n in nodes {
            match n {
                Node::Literal(c) => {
                    if self.iscased(mode, *c) {
                        return (prefix, skip, false);
                    }
                    prefix.push(*c);
                }
                Node::Subpattern { group, add, del, body } => {
                    let f = merge_flags(flags, *add, *del);
                    if f & IGNORECASE != 0 && f & LOCALE != 0 {
                        return (prefix, skip, false);
                    }
                    let (inner, inner_skip, whole) = self.literal_prefix(body, f);
                    if skip.is_none() {
                        if group.is_some() {
                            skip = Some(prefix.len());
                        } else if let Some(s) = inner_skip {
                            skip = Some(prefix.len() + s);
                        }
                    }
                    prefix.extend(inner);
                    if !whole {
                        return (prefix, skip, false);
                    }
                }
                _ => return (prefix, skip, false),
            }
        }
        (prefix, skip, true)
    }

    /// Set of characters every match starts with (when there is no literal prefix).
    fn charset_prefix(&self, nodes: &[Node], mut flags: u32) -> Option<Vec<Member>> {
        let mut nodes = nodes;
        let first = loop {
            let first = nodes.first()?;
            match first {
                Node::Subpattern { add, del, body, .. } => {
                    flags = merge_flags(flags, *add, *del);
                    if flags & IGNORECASE != 0 && flags & LOCALE != 0 {
                        return None;
                    }
                    nodes = body;
                }
                _ => break first,
            }
        };
        let mode = case_mode(flags);
        let cased = |c: u32| self.iscased(mode, c);
        match first {
            Node::Literal(c) => {
                if cased(*c) {
                    return None;
                }
                Some(vec![Member::Lit(*c)])
            }
            Node::Branch(alts) => {
                let mut out = Vec::new();
                for a in alts {
                    match a.first() {
                        Some(Node::Literal(c)) if !cased(*c) => out.push(Member::Lit(*c)),
                        _ => return None,
                    }
                }
                Some(out)
            }
            Node::In(items, neg) => {
                let mut out = Vec::new();
                if *neg {
                    out.push(Member::Negate);
                }
                for it in items {
                    match it {
                        SetItem::Lit(c) => {
                            if cased(*c) {
                                return None;
                            }
                            out.push(Member::Lit(*c));
                        }
                        SetItem::Range(a, b) => {
                            if mode != CaseMode::Exact {
                                if *b > 0xFFFF || (*a..=*b).any(cased) {
                                    return None;
                                }
                            }
                            out.push(Member::Range(*a, *b));
                        }
                        SetItem::Cat(c) => out.push(Member::Cat(*c)),
                    }
                }
                Some(out)
            }
            _ => None,
        }
    }

    fn info(&mut self, nodes: &[Node], flags: u32) {
        let (lo, hi) = width(nodes, &self.gw);
        let (lo, hi) = (cap(lo.min(MAXWIDTH)), cap(hi.min(MAXWIDTH)));
        if lo == 0 {
            self.code.extend_from_slice(&[Op::Info as u32, 4, 0, lo, hi]);
            return;
        }
        let mut prefix = Vec::new();
        let mut skip = None;
        let mut whole = false;
        let mut charset = None;
        if !(flags & IGNORECASE != 0 && flags & LOCALE != 0) {
            (prefix, skip, whole) = self.literal_prefix(nodes, flags);
            if prefix.is_empty() {
                charset = self.charset_prefix(nodes, flags);
            }
        }
        self.op(Op::Info);
        let h = self.hole();
        let mut mask = 0;
        if !prefix.is_empty() {
            mask = INFO_PREFIX;
            if skip.is_none() && whole {
                mask |= INFO_LITERAL;
            }
        } else if charset.is_some() {
            mask |= INFO_CHARSET;
        }
        self.w(mask);
        self.w(lo);
        self.w(hi);
        if !prefix.is_empty() {
            self.w(prefix.len() as u32);
            self.w(skip.unwrap_or(prefix.len()) as u32);
            self.code.extend_from_slice(&prefix);
            self.code.extend(overlap_table(&prefix));
        } else if let Some(cs) = charset {
            let (members, _) = self.optimize_set(&cs, CaseMode::Exact);
            self.members(&members, flags);
        }
        self.patch(h);
    }
}

fn bitmap_words(bits: &[bool]) -> Vec<u32> {
    bits.chunks(32)
        .map(|c| c.iter().enumerate().fold(0u32, |acc, (i, &b)| acc | ((b as u32) << i)))
        .collect()
}

/// KMP failure table: entry i is the length of the longest proper suffix of
/// prefix[..=i] that is also a prefix.
fn overlap_table(prefix: &[u32]) -> Vec<u32> {
    let mut t = vec![0u32; prefix.len()];
    let mut k = 0usize;
    for i in 1..prefix.len() {
        while k > 0 && prefix[i] != prefix[k] {
            k = t[k - 1] as usize;
        }
        if prefix[i] == prefix[k] {
            k += 1;
        }
        t[i] = k as u32;
    }
    t
}

// ------------------------------------------------------- code listing

struct Lister<'a> {
    code: &'a [u32],
    width: usize,
    targets: BTreeSet<usize>,
    out: String,
    prov: &'a UnicodeProvider,
}

fn hex(v: u32) -> String {
    format!("{v:#x}")
}

impl Lister<'_> {
    fn chr(&self, c: u32) -> String {
        py_repr(&[c], false, self.prov)
    }

    /// One instruction line; `to` (a jump target) is recorded and appended.
    fn line(&mut self, at: usize, depth: usize, text: &str, to: Option<usize>) {
        let mark = if self.targets.contains(&at) { ':' } else { '.' };
        let _ = write!(self.out, "{:>w$}{} {}{}", at, mark, "  ".repeat(depth), text, w = self.width);
        if let Some(t) = to {
            self.targets.insert(t);
            let _ = write!(self.out, " (to {t})");
        }
        self.out.push('\n');
    }

    /// A continuation line without an offset.
    fn note(&mut self, depth: usize, text: &str) {
        let _ = writeln!(self.out, "{}{}{}", " ".repeat(self.width + 2), "  ".repeat(depth + 1), text);
    }

    fn word(&self, i: usize) -> u32 {
        self.code.get(i).copied().unwrap_or(0)
    }

    fn list(&mut self, mut i: usize, end: usize, depth: usize) {
        while i < end {
            let start = i;
            let Some(op) = Op::from_word(self.word(i)) else {
                let t = format!("<invalid {}>", self.word(i));
                self.line(start, depth, &t, None);
                i += 1;
                continue;
            };
            i += 1;
            let name = op.name();
            match op {
                Op::Success | Op::Failure | Op::Any | Op::AnyAll | Op::MaxUntil
                | Op::MinUntil | Op::Negate => self.line(start, depth, name, None),
                Op::Literal | Op::NotLiteral | Op::LiteralIgnore | Op::NotLiteralIgnore
                | Op::LiteralUniIgnore | Op::NotLiteralUniIgnore | Op::LiteralLocIgnore
                | Op::NotLiteralLocIgnore => {
                    let c = self.word(i);
                    i += 1;
                    let t = format!("{name} {} ({})", hex(c), self.chr(c));
                    self.line(start, depth, &t, None);
                }
                Op::At => {
                    let a = self.word(i) as usize;
                    i += 1;
                    let n = AT_NAMES.get(a).map_or("?", |n| &n[3..]);
                    self.line(start, depth, &format!("{name} {n}"), None);
                }
                Op::Category => {
                    let a = self.word(i) as usize;
                    i += 1;
                    let n = CAT_NAMES.get(a).map_or("?", |n| &n[9..]);
                    self.line(start, depth, &format!("{name} {n}"), None);
                }
                Op::In | Op::InIgnore | Op::InUniIgnore | Op::InLocIgnore => {
                    let skip = self.word(i) as usize;
                    self.line(start, depth, &format!("{name} {skip}"), Some(i + skip));
                    self.list(i + 1, i + skip, depth + 1);
                    i += skip;
                }
                Op::Range | Op::RangeUniIgnore => {
                    let (a, b) = (self.word(i), self.word(i + 1));
                    i += 2;
                    let t = format!("{name} {} {} ({}-{})", hex(a), hex(b), self.chr(a), self.chr(b));
                    self.line(start, depth, &t, None);
                }
                Op::Charset => {
                    let words: Vec<String> =
                        (0..8).map(|k| format!("{:#010x}", self.word(i + k))).collect();
                    i += 8;
                    self.line(start, depth, &format!("{name} [{}]", words.join(", ")), None);
                }
                Op::BigCharset => {
                    let n = self.word(i) as usize;
                    i += 1;
                    let mut ids = Vec::with_capacity(256);
                    for k in 0..64 {
                        ids.extend(self.word(i + k).to_le_bytes());
                    }
                    i += 64;
                    let ids_text: Vec<String> = ids.iter().map(|b| b.to_string()).collect();
                    self.line(start, depth, &format!("{name} {n}"), None);
                    self.note(depth, &format!("[{}]", ids_text.join(", ")));
                    for _ in 0..n {
                        let words: Vec<String> =
                            (0..8).map(|k| format!("{:#010x}", self.word(i + k))).collect();
                        self.note(depth, &format!("[{}]", words.join(", ")));
                        i += 8;
                    }
                }
                Op::Mark | Op::GroupRef | Op::GroupRefIgnore | Op::GroupRefUniIgnore
                | Op::GroupRefLocIgnore => {
                    let a = self.word(i);
                    i += 1;
                    self.line(start, depth, &format!("{name} {a}"), None);
                }
                Op::Jump => {
                    let skip = self.word(i) as usize;
                    self.line(start, depth, &format!("{name} {skip}"), Some(i + skip));
                    i += 1;
                }
                Op::Branch => {
                    let mut skip = self.word(i) as usize;
                    self.line(start, depth, &format!("{name} {skip}"), Some(i + skip));
                    while skip != 0 {
                        self.list(i + 1, i + skip, depth + 1);
                        i += skip;
                        let at = i;
                        skip = self.word(i) as usize;
                        if skip != 0 {
                            self.line(at, depth, &format!("branch {skip}"), Some(i + skip));
                        }
                    }
                }
                Op::Repeat | Op::RepeatOne | Op::MinRepeatOne | Op::PossessiveRepeat
                | Op::PossessiveRepeatOne => {
                    let skip = self.word(i) as usize;
                    let (mn, mx) = (self.word(i + 1), self.word(i + 2));
                    let t = format!("{name} {skip} {} {}", repeat_bound(mn), repeat_bound(mx));
                    self.line(start, depth, &t, Some(i + skip));
                    self.list(i + 3, i + skip, depth + 1);
                    i += skip;
                }
                Op::GroupRefExists => {
                    let (g, skip) = (self.word(i), self.word(i + 1) as usize);
                    self.line(start, depth, &format!("{name} {g} {skip}"), Some(i + skip));
                    i += 2;
                }
                Op::Assert | Op::AssertNot => {
                    let (skip, back) = (self.word(i) as usize, self.word(i + 1));
                    self.line(start, depth, &format!("{name} {skip} {back}"), Some(i + skip));
                    self.list(i + 2, i + skip, depth + 1);
                    i += skip;
                }
                Op::AtomicGroup => {
                    let skip = self.word(i) as usize;
                    self.line(start, depth, &format!("{name} {skip}"), Some(i + skip));
                    self.list(i + 1, i + skip, depth + 1);
                    i += skip;
                }
                Op::Info => {
                    let skip = self.word(i) as usize;
                    let (fl, mn, mx) = (self.word(i + 1), self.word(i + 2), self.word(i + 3));
                    let t = format!("{name} {skip} {fl:#b} {} {}", repeat_bound(mn), repeat_bound(mx));
                    self.line(start, depth, &t, Some(i + skip));
                    let mut p = i + 4;
                    if fl & INFO_PREFIX != 0 {
                        let (n, pskip) = (self.word(p) as usize, self.word(p + 1));
                        p += 2;
                        let chars: Vec<u32> = (0..n).map(|k| self.word(p + k)).collect();
                        p += n;
                        let table: Vec<String> = (0..n).map(|k| self.word(p + k).to_string()).collect();
                        let hexes: Vec<String> = chars.iter().map(|&c| hex(c)).collect();
                        self.note(depth, &format!("prefix_skip {pskip}"));
                        let text = py_repr(&chars, false, self.prov);
                        self.note(depth, &format!("prefix [{}] ({})", hexes.join(", "), text));
                        self.note(depth, &format!("overlap [{}]", table.join(", ")));
                    } else if fl & INFO_CHARSET != 0 {
                        self.note(depth, "in");
                        self.list(p, i + skip, depth + 2);
                    }
                    i += skip;
                }
                Op::Subpattern => self.line(start, depth, name, None),
            }
        }
    }
}

/// Text printed for a pattern compiled with `re.DEBUG`: the parse tree, a
/// blank line, then the code listing.
pub(crate) fn debug_text(parsed: &Parsed, prov: &UnicodeProvider) -> Result<String, CompileError> {
    let tree = reshape_seq(&parsed.nodes);
    let mut out = String::new();
    dump_tree(&tree, 0, &mut out);
    let gw: Vec<Option<(u128, u128)>> = parsed.group_widths.iter().map(|w| Some(*w)).collect();
    let mut em = Emitter { code: Vec::new(), prov, gw };
    em.info(&tree, parsed.flags);
    em.seq(&tree, parsed.flags)?;
    em.op(Op::Success);
    let code = em.code;
    out.push('\n');
    let width = (code.len().max(1) - 1).to_string().len();
    let mut l = Lister { code: &code, width, targets: BTreeSet::new(), out, prov };
    l.list(0, code.len(), 0);
    Ok(l.out)
}
