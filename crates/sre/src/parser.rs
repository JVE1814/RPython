//! Pattern parser: code points -> AST, with CPython-compatible errors.

use std::collections::HashMap;

use crate::error::{CompileError, Warning, WarningKind};
use crate::flags::*;
use crate::unicode::UnicodeProvider;
use crate::{MAXGROUPS, MAXREPEAT};

type R<T> = Result<T, CompileError>;

/// Saturation bound for widths (CPython's MAXWIDTH = 1 << 64).
pub(crate) const MAXWIDTH: u128 = 1u128 << 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Category {
    Digit,
    NotDigit,
    Space,
    NotSpace,
    Word,
    NotWord,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SetItem {
    Lit(u32),
    Range(u32, u32),
    Cat(Category),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AtCode {
    Beginning,       // ^
    BeginningString, // \A
    End,             // $
    EndString,       // \Z \z
    Boundary,        // \b
    NonBoundary,     // \B
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RepKind {
    Greedy,
    Lazy,
    Possessive,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Node {
    Literal(u32),
    NotLiteral(u32),
    Any,
    In(Vec<SetItem>, bool),
    At(AtCode),
    Subpattern { group: Option<usize>, add: u32, del: u32, body: Vec<Node> },
    Branch(Vec<Vec<Node>>),
    Repeat { min: u32, max: u32, kind: RepKind, body: Vec<Node> },
    GroupRef(usize),
    GroupRefExists { group: usize, yes: Vec<Node>, no: Option<Vec<Node>> },
    Assert { behind: bool, neg: bool, body: Vec<Node> },
    Atomic(Vec<Node>),
    Failure,
}

pub(crate) struct Parsed {
    pub nodes: Vec<Node>,
    pub flags: u32,
    /// Number of capturing groups (not counting group 0).
    pub groups: usize,
    pub group_names: Vec<(String, usize)>,
    /// Width of every group (index 0 unused).
    pub group_widths: Vec<(u128, u128)>,
    pub warnings: Vec<Warning>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Tok {
    pub c: u32,
    pub esc: bool,
}

impl Tok {
    fn len(self) -> usize {
        if self.esc { 2 } else { 1 }
    }
    fn is(self, c: char) -> bool {
        !self.esc && self.c == c as u32
    }
    fn push_to(self, v: &mut Vec<u32>) {
        if self.esc {
            v.push('\\' as u32);
        }
        v.push(self.c);
    }
}

/// Tokenizer with one token of lookahead; a token is a character or a
/// backslash followed by a character.
pub(crate) struct Source<'a> {
    s: &'a [u32],
    index: usize,
    pub next: Option<Tok>,
}

impl<'a> Source<'a> {
    pub fn new(s: &'a [u32]) -> R<Self> {
        let mut t = Source { s, index: 0, next: None };
        t.advance()?;
        Ok(t)
    }
    fn advance(&mut self) -> R<()> {
        let i = self.index;
        if i >= self.s.len() {
            self.next = None;
            return Ok(());
        }
        let c = self.s[i];
        if c == '\\' as u32 {
            if i + 1 >= self.s.len() {
                return Err(CompileError::pat("bad escape (end of pattern)", self.s.len() - 1));
            }
            self.next = Some(Tok { c: self.s[i + 1], esc: true });
            self.index = i + 2;
        } else {
            self.next = Some(Tok { c, esc: false });
            self.index = i + 1;
        }
        Ok(())
    }
    pub fn tell(&self) -> usize {
        self.index - self.next.map_or(0, |t| t.len())
    }
    pub fn seek(&mut self, index: usize) -> R<()> {
        self.index = index;
        self.advance()
    }
    pub fn eat(&mut self, c: char) -> R<bool> {
        if let Some(t) = self.next {
            if t.is(c) {
                self.advance()?;
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub fn next_is(&self, c: char) -> bool {
        self.next.is_some_and(|t| t.is(c))
    }
    fn next_in(&self, f: impl Fn(u32) -> bool) -> bool {
        self.next.is_some_and(|t| !t.esc && f(t.c))
    }
    pub fn get(&mut self) -> R<Option<Tok>> {
        let t = self.next;
        self.advance()?;
        Ok(t)
    }
    fn getwhile(&mut self, n: usize, f: impl Fn(u32) -> bool, out: &mut Vec<u32>) -> R<()> {
        for _ in 0..n {
            match self.next {
                Some(t) if !t.esc && f(t.c) => {
                    out.push(t.c);
                    self.advance()?;
                }
                _ => break,
            }
        }
        Ok(())
    }
    pub fn getuntil(&mut self, term: char, name: &str) -> R<Vec<u32>> {
        let mut result = Vec::new();
        loop {
            let c = self.next;
            self.advance()?;
            match c {
                None => {
                    if result.is_empty() {
                        return Err(self.error(format!("missing {name}"), 0));
                    }
                    return Err(self.error(
                        format!("missing {term}, unterminated name"),
                        result.len(),
                    ));
                }
                Some(t) if t.is(term) => {
                    if result.is_empty() {
                        return Err(self.error(format!("missing {name}"), 1));
                    }
                    break;
                }
                Some(t) => t.push_to(&mut result),
            }
        }
        Ok(result)
    }
    pub fn error(&self, msg: impl Into<String>, offset: usize) -> CompileError {
        CompileError::pat(msg, self.tell().saturating_sub(offset))
    }
}

pub(crate) fn chars_to_string(v: &[u32]) -> String {
    v.iter().map(|&c| char::from_u32(c).unwrap_or('\u{FFFD}')).collect()
}

/// Python `repr()` of a str (or `ascii()` when `ascii_only`).
pub(crate) fn py_repr(v: &[u32], ascii_only: bool, prov: &UnicodeProvider) -> String {
    let has_sq = v.contains(&('\'' as u32));
    let has_dq = v.contains(&('"' as u32));
    let quote = if has_sq && !has_dq { '"' } else { '\'' };
    let mut s = String::new();
    s.push(quote);
    for &c in v {
        match c {
            0x5C => s.push_str("\\\\"),
            0x09 => s.push_str("\\t"),
            0x0A => s.push_str("\\n"),
            0x0D => s.push_str("\\r"),
            _ if c == quote as u32 => {
                s.push('\\');
                s.push(quote);
            }
            _ => {
                let printable = if c < 0x80 {
                    (0x20..0x7F).contains(&c)
                } else {
                    !ascii_only && (prov.is_printable)(c)
                };
                if printable {
                    s.push(char::from_u32(c).unwrap());
                } else if c < 0x100 {
                    s.push_str(&format!("\\x{c:02x}"));
                } else if c < 0x10000 {
                    s.push_str(&format!("\\u{c:04x}"));
                } else {
                    s.push_str(&format!("\\U{c:08x}"));
                }
            }
        }
    }
    s.push(quote);
    s
}

fn chars_str(v: &[u32]) -> String {
    chars_to_string(v)
}

#[inline]
fn is_digit(c: u32) -> bool {
    (0x30..=0x39).contains(&c)
}
#[inline]
fn is_oct(c: u32) -> bool {
    (0x30..=0x37).contains(&c)
}
#[inline]
fn is_hex(c: u32) -> bool {
    c < 128 && (c as u8).is_ascii_hexdigit()
}
#[inline]
fn is_ascii_letter(c: u32) -> bool {
    c < 128 && (c as u8).is_ascii_alphabetic()
}
#[inline]
fn is_ws(c: u32) -> bool {
    matches!(c, 0x20 | 0x09 | 0x0A | 0x0D | 0x0B | 0x0C)
}
const SPECIAL: &str = ".\\[{()*+?^$|";

fn flag_of(c: u32) -> Option<u32> {
    match char::from_u32(c)? {
        'i' => Some(IGNORECASE),
        'L' => Some(LOCALE),
        'm' => Some(MULTILINE),
        's' => Some(DOTALL),
        'x' => Some(VERBOSE),
        'a' => Some(ASCII),
        'u' => Some(UNICODE),
        _ => None,
    }
}

const TYPE_FLAGS: u32 = ASCII | LOCALE | UNICODE;
const GLOBAL_FLAGS: u32 = 0; // DEBUG is not an inline flag; no global-only inline flags remain

fn digits_value(v: &[u32]) -> u128 {
    let mut n: u128 = 0;
    for &c in v {
        n = n.saturating_mul(10).saturating_add((c - 0x30) as u128);
    }
    n
}

/// Decimal string of an all-digit token run (leading zeros stripped).
fn digits_display(v: &[u32]) -> String {
    let s: String = v.iter().map(|&c| char::from_u32(c).unwrap()).collect();
    let t = s.trim_start_matches('0');
    if t.is_empty() { "0".to_string() } else { t.to_string() }
}

pub(crate) struct Parser<'p> {
    prov: &'p UnicodeProvider,
    is_bytes: bool,
    flags: u32,
    group_widths: Vec<Option<(u128, u128)>>,
    names: Vec<(String, usize)>,
    name_map: HashMap<Vec<u32>, usize>,
    lookbehind_groups: Option<usize>,
    grouprefpos: Vec<(usize, usize)>,
    warnings: Vec<Warning>,
}

pub(crate) fn parse(
    pattern: &[u32],
    is_bytes: bool,
    flags: u32,
    prov: &UnicodeProvider,
) -> R<Parsed> {
    let mut p = Parser {
        prov,
        is_bytes,
        flags,
        group_widths: vec![Some((0, 0))],
        names: Vec::new(),
        name_map: HashMap::new(),
        lookbehind_groups: None,
        grouprefpos: Vec::new(),
        warnings: Vec::new(),
    };
    let mut src = Source::new(pattern)?;
    let nodes = p.parse_sub(&mut src, flags & VERBOSE != 0, 0)?;
    let flags = fix_flags(is_bytes, p.flags)?;
    if src.next.is_some() {
        return Err(src.error("unbalanced parenthesis", 0));
    }
    let ngroups = p.group_widths.len();
    for &(g, pos) in &p.grouprefpos {
        if g >= ngroups {
            return Err(CompileError::pat(format!("invalid group reference {g}"), pos));
        }
    }
    Ok(Parsed {
        nodes,
        flags,
        groups: ngroups - 1,
        group_names: p.names,
        group_widths: p.group_widths.iter().map(|w| w.unwrap_or((0, 0))).collect(),
        warnings: p.warnings,
    })
}

pub(crate) fn fix_flags(is_bytes: bool, mut flags: u32) -> R<u32> {
    if !is_bytes {
        if flags & LOCALE != 0 {
            return Err(CompileError::Value("cannot use LOCALE flag with a str pattern".into()));
        }
        if flags & ASCII == 0 {
            flags |= UNICODE;
        } else if flags & UNICODE != 0 {
            return Err(CompileError::Value("ASCII and UNICODE flags are incompatible".into()));
        }
    } else {
        if flags & UNICODE != 0 {
            return Err(CompileError::Value("cannot use UNICODE flag with a bytes pattern".into()));
        }
        if flags & LOCALE != 0 && flags & ASCII != 0 {
            return Err(CompileError::Value("ASCII and LOCALE flags are incompatible".into()));
        }
    }
    Ok(flags)
}

/// Width (min, max) of a sequence, saturating at MAXWIDTH.
pub(crate) fn width(nodes: &[Node], gw: &[Option<(u128, u128)>]) -> (u128, u128) {
    let mut lo: u128 = 0;
    let mut hi: u128 = 0;
    for n in nodes {
        let (i, j) = match n {
            Node::Literal(_) | Node::NotLiteral(_) | Node::Any | Node::In(..) => (1, 1),
            Node::At(_) | Node::Assert { .. } | Node::Failure => (0, 0),
            Node::Subpattern { body, .. } | Node::Atomic(body) => width(body, gw),
            Node::Branch(alts) => {
                let mut i = MAXWIDTH;
                let mut j = 0;
                for a in alts {
                    let (l, h) = width(a, gw);
                    i = i.min(l);
                    j = j.max(h);
                }
                (i, j)
            }
            Node::Repeat { min, max, body, .. } => {
                let (i, j) = width(body, gw);
                let l = i.saturating_mul(*min as u128);
                let h = if *max == MAXREPEAT && j != 0 {
                    MAXWIDTH
                } else {
                    j.saturating_mul(*max as u128)
                };
                (l, h)
            }
            Node::GroupRef(g) => gw.get(*g).copied().flatten().unwrap_or((0, MAXWIDTH)),
            Node::GroupRefExists { yes, no, .. } => {
                let (mut i, mut j) = width(yes, gw);
                if let Some(no) = no {
                    let (l, h) = width(no, gw);
                    i = i.min(l);
                    j = j.max(h);
                } else {
                    i = 0;
                }
                (i, j)
            }
        };
        lo = lo.saturating_add(i).min(MAXWIDTH);
        hi = hi.saturating_add(j).min(MAXWIDTH);
    }
    (lo.min(MAXWIDTH), hi.min(MAXWIDTH))
}

impl<'p> Parser<'p> {
    fn ngroups(&self) -> usize {
        self.group_widths.len()
    }

    fn checkgroup(&self, gid: usize) -> bool {
        gid < self.ngroups() && self.group_widths[gid].is_some()
    }

    fn checklookbehindgroup(&self, gid: usize, src: &Source) -> R<()> {
        if let Some(lb) = self.lookbehind_groups {
            if !self.checkgroup(gid) {
                return Err(src.error("cannot refer to an open group", 0));
            }
            if gid >= lb {
                return Err(src.error(
                    "cannot refer to group defined in the same lookbehind subpattern",
                    0,
                ));
            }
        }
        Ok(())
    }

    fn is_identifier(&self, v: &[u32]) -> bool {
        if v.is_empty() {
            return false;
        }
        if self.is_bytes && v.iter().any(|&c| c >= 128) {
            return false;
        }
        (self.prov.is_identifier_start)(v[0])
            && v[1..].iter().all(|&c| (self.prov.is_identifier_continue)(c))
    }

    fn repr(&self, v: &[u32]) -> String {
        py_repr(v, self.is_bytes, self.prov)
    }

    fn warn(&mut self, msg: String) {
        self.warnings.push(Warning { kind: WarningKind::FutureWarning, message: msg });
    }

    fn parse_sub(&mut self, src: &mut Source, verbose: bool, nested: usize) -> R<Vec<Node>> {
        let mut items: Vec<Vec<Node>> = Vec::new();
        loop {
            let v = if nested == 0 { verbose || self.flags & VERBOSE != 0 } else { verbose };
            let first = nested == 0 && items.is_empty();
            items.push(self.parse_seq(src, v, nested + 1, first)?);
            if !src.eat('|')? {
                break;
            }
        }
        if items.len() == 1 {
            return Ok(items.pop().unwrap());
        }
        Ok(vec![Node::Branch(items)])
    }

    fn parse_seq(
        &mut self,
        src: &mut Source,
        mut verbose: bool,
        nested: usize,
        first: bool,
    ) -> R<Vec<Node>> {
        let mut sub: Vec<Node> = Vec::new();
        loop {
            let Some(this) = src.next else { break };
            if this.is('|') || this.is(')') {
                break;
            }
            src.get()?;
            if verbose && !this.esc {
                if is_ws(this.c) {
                    continue;
                }
                if this.c == '#' as u32 {
                    loop {
                        match src.get()? {
                            None => break,
                            Some(t) if t.is('\n') => break,
                            _ => {}
                        }
                    }
                    continue;
                }
            }
            if this.esc {
                let n = self.escape(src, this)?;
                sub.push(n);
                continue;
            }
            let c = this.c;
            let ch = char::from_u32(c).unwrap_or('\u{FFFD}');
            if c >= 128 || !SPECIAL.contains(ch) {
                sub.push(Node::Literal(c));
                continue;
            }
            match ch {
                '[' => {
                    let n = self.parse_set(src, nested)?;
                    sub.push(n);
                }
                '*' | '+' | '?' | '{' => {
                    self.parse_repeat(src, ch, &mut sub)?;
                }
                '.' => sub.push(Node::Any),
                '^' => sub.push(Node::At(AtCode::Beginning)),
                '$' => sub.push(Node::At(AtCode::End)),
                '(' => {
                    if let Some(n) = self.parse_group(src, &mut verbose, nested, first, &sub)? {
                        sub.push(n);
                    }
                }
                _ => unreachable!(),
            }
        }
        Ok(sub)
    }

    fn parse_repeat(&mut self, src: &mut Source, this: char, sub: &mut Vec<Node>) -> R<()> {
        let here = src.tell();
        let (min, max): (u32, u32);
        match this {
            '?' => {
                min = 0;
                max = 1;
            }
            '*' => {
                min = 0;
                max = MAXREPEAT;
            }
            '+' => {
                min = 1;
                max = MAXREPEAT;
            }
            '{' => {
                if src.next_is('}') {
                    sub.push(Node::Literal('{' as u32));
                    return Ok(());
                }
                let mut lo = Vec::new();
                let mut hi = Vec::new();
                while src.next_in(is_digit) {
                    lo.push(src.get()?.unwrap().c);
                }
                if src.eat(',')? {
                    while src.next_in(is_digit) {
                        hi.push(src.get()?.unwrap().c);
                    }
                } else {
                    hi = lo.clone();
                }
                if !src.eat('}')? {
                    sub.push(Node::Literal('{' as u32));
                    src.seek(here)?;
                    return Ok(());
                }
                let mut mn: u32 = 0;
                let mut mx: u32 = MAXREPEAT;
                if !lo.is_empty() {
                    let v = digits_value(&lo);
                    if v >= MAXREPEAT as u128 {
                        return Err(CompileError::Overflow(
                            "the repetition number is too large".into(),
                        ));
                    }
                    mn = v as u32;
                }
                if !hi.is_empty() {
                    let v = digits_value(&hi);
                    if v >= MAXREPEAT as u128 {
                        return Err(CompileError::Overflow(
                            "the repetition number is too large".into(),
                        ));
                    }
                    mx = v as u32;
                    if mx < mn {
                        return Err(src.error(
                            "min repeat greater than max repeat",
                            src.tell() - here,
                        ));
                    }
                }
                min = mn;
                max = mx;
            }
            _ => unreachable!(),
        }
        let off = src.tell() - here + 1;
        let item = match sub.last() {
            None | Some(Node::At(_)) => {
                return Err(src.error("nothing to repeat", off));
            }
            Some(Node::Repeat { .. }) => {
                return Err(src.error("multiple repeat", off));
            }
            Some(_) => sub.pop().unwrap(),
        };
        let body = match item {
            Node::Subpattern { group: None, add: 0, del: 0, body } => body,
            other => vec![other],
        };
        let kind = if src.eat('?')? {
            RepKind::Lazy
        } else if src.eat('+')? {
            RepKind::Possessive
        } else {
            RepKind::Greedy
        };
        sub.push(Node::Repeat { min, max, kind, body });
        Ok(())
    }

    fn parse_set(&mut self, src: &mut Source, _nested: usize) -> R<Node> {
        let here = src.tell() - 1;
        let mut set: Vec<SetItem> = Vec::new();
        if src.next_is('[') {
            let pos = src.tell();
            self.warn(format!("Possible nested set at position {pos}"));
        }
        let negate = src.eat('^')?;
        loop {
            let Some(this) = src.get()? else {
                return Err(src.error("unterminated character set", src.tell() - here));
            };
            if this.is(']') && !set.is_empty() {
                break;
            }
            let code1 = if this.esc {
                self.class_escape(src, this)?
            } else {
                if !set.is_empty()
                    && matches!(char::from_u32(this.c), Some('-' | '&' | '~' | '|'))
                    && src.next_is(char::from_u32(this.c).unwrap())
                {
                    let what = match char::from_u32(this.c).unwrap() {
                        '-' => "difference",
                        '&' => "intersection",
                        '~' => "symmetric difference",
                        _ => "union",
                    };
                    let pos = src.tell() - 1;
                    self.warn(format!("Possible set {what} at position {pos}"));
                }
                SetItem::Lit(this.c)
            };
            if src.eat('-')? {
                let Some(that) = src.get()? else {
                    return Err(src.error("unterminated character set", src.tell() - here));
                };
                if that.is(']') {
                    set.push(code1);
                    set.push(SetItem::Lit('-' as u32));
                    break;
                }
                let code2 = if that.esc {
                    self.class_escape(src, that)?
                } else {
                    if that.is('-') {
                        let pos = src.tell() - 2;
                        self.warn(format!("Possible set difference at position {pos}"));
                    }
                    SetItem::Lit(that.c)
                };
                let bad = |src: &Source| {
                    let mut m = Vec::new();
                    this.push_to(&mut m);
                    m.push('-' as u32);
                    that.push_to(&mut m);
                    src.error(
                        format!("bad character range {}", chars_str(&m)),
                        this.len() + 1 + that.len(),
                    )
                };
                match (code1, code2) {
                    (SetItem::Lit(lo), SetItem::Lit(hi)) => {
                        if hi < lo {
                            return Err(bad(src));
                        }
                        set.push(SetItem::Range(lo, hi));
                    }
                    _ => return Err(bad(src)),
                }
            } else {
                set.push(code1);
            }
        }
        if set.len() == 1 {
            if let SetItem::Lit(c) = set[0] {
                return Ok(if negate { Node::NotLiteral(c) } else { Node::Literal(c) });
            }
        }
        Ok(Node::In(set, negate))
    }

    /// Parse the numeric parts of \x \u \U \N escapes shared by both escape
    /// kinds.  Returns `Some(code point)` if handled.
    fn common_escape(&mut self, src: &mut Source, c: u32, esc: &mut Vec<u32>) -> R<Option<u32>> {
        match char::from_u32(c) {
            Some('x') => {
                src.getwhile(2, is_hex, esc)?;
                if esc.len() != 4 {
                    return Err(src.error(
                        format!("incomplete escape {}", chars_str(esc)),
                        esc.len(),
                    ));
                }
                Ok(Some(hex_value(&esc[2..])))
            }
            Some('u') if !self.is_bytes => {
                src.getwhile(4, is_hex, esc)?;
                if esc.len() != 6 {
                    return Err(src.error(
                        format!("incomplete escape {}", chars_str(esc)),
                        esc.len(),
                    ));
                }
                Ok(Some(hex_value(&esc[2..])))
            }
            Some('U') if !self.is_bytes => {
                src.getwhile(8, is_hex, esc)?;
                if esc.len() != 10 {
                    return Err(src.error(
                        format!("incomplete escape {}", chars_str(esc)),
                        esc.len(),
                    ));
                }
                let v = hex_value(&esc[2..]);
                if v > 0x10FFFF {
                    return Err(src.error(format!("bad escape {}", chars_str(esc)), esc.len()));
                }
                Ok(Some(v))
            }
            Some('N') if !self.is_bytes => {
                if !src.eat('{')? {
                    return Err(src.error("missing {", 0));
                }
                let name = src.getuntil('}', "character name")?;
                let sname = chars_to_string(&name);
                match (self.prov.lookup_name)(&sname) {
                    Some(v) => Ok(Some(v)),
                    None => Err(src.error(
                        format!("undefined character name {}", self.repr_plain(&name)),
                        name.len() + 4,
                    )),
                }
            }
            _ => Ok(None),
        }
    }

    fn repr_plain(&self, v: &[u32]) -> String {
        py_repr(v, false, self.prov)
    }

    fn class_escape(&mut self, src: &mut Source, tok: Tok) -> R<SetItem> {
        let c = tok.c;
        if let Some(v) = simple_escape(c) {
            return Ok(SetItem::Lit(v));
        }
        if let Some(cat) = category_escape(c) {
            return Ok(SetItem::Cat(cat));
        }
        let mut esc = vec!['\\' as u32, c];
        if let Some(v) = self.common_escape(src, c, &mut esc)? {
            return Ok(SetItem::Lit(v));
        }
        if is_oct(c) {
            src.getwhile(2, is_oct, &mut esc)?;
            let v = oct_value(&esc[1..]);
            if v > 0o377 {
                return Err(src.error(
                    format!("octal escape value {} outside of range 0-0o377", chars_str(&esc)),
                    esc.len(),
                ));
            }
            return Ok(SetItem::Lit(v));
        }
        if is_digit(c) {
            return Err(src.error(format!("bad escape {}", chars_str(&esc)), esc.len()));
        }
        if esc.len() == 2 {
            if is_ascii_letter(c) {
                return Err(src.error(format!("bad escape {}", chars_str(&esc)), esc.len()));
            }
            return Ok(SetItem::Lit(c));
        }
        Err(src.error(format!("bad escape {}", chars_str(&esc)), esc.len()))
    }

    fn escape(&mut self, src: &mut Source, tok: Tok) -> R<Node> {
        let c = tok.c;
        match char::from_u32(c) {
            Some('A') => return Ok(Node::At(AtCode::BeginningString)),
            Some('Z') | Some('z') => return Ok(Node::At(AtCode::EndString)),
            Some('b') => return Ok(Node::At(AtCode::Boundary)),
            Some('B') => return Ok(Node::At(AtCode::NonBoundary)),
            _ => {}
        }
        if let Some(cat) = category_escape(c) {
            return Ok(Node::In(vec![SetItem::Cat(cat)], false));
        }
        if let Some(v) = simple_escape(c) {
            if c != 'b' as u32 {
                return Ok(Node::Literal(v));
            }
        }
        let mut esc = vec!['\\' as u32, c];
        if let Some(v) = self.common_escape(src, c, &mut esc)? {
            return Ok(Node::Literal(v));
        }
        if c == '0' as u32 {
            src.getwhile(2, is_oct, &mut esc)?;
            return Ok(Node::Literal(oct_value(&esc[1..])));
        }
        if is_digit(c) {
            if src.next_in(is_digit) {
                esc.push(src.get()?.unwrap().c);
                if is_oct(esc[1]) && is_oct(esc[2]) && src.next_in(is_oct) {
                    esc.push(src.get()?.unwrap().c);
                    let v = oct_value(&esc[1..]);
                    if v > 0o377 {
                        return Err(src.error(
                            format!(
                                "octal escape value {} outside of range 0-0o377",
                                chars_str(&esc)
                            ),
                            esc.len(),
                        ));
                    }
                    return Ok(Node::Literal(v));
                }
            }
            let group = digits_value(&esc[1..]) as usize;
            if group < self.ngroups() {
                if !self.checkgroup(group) {
                    return Err(src.error("cannot refer to an open group", esc.len()));
                }
                self.checklookbehindgroup(group, src)?;
                return Ok(Node::GroupRef(group));
            }
            return Err(src.error(format!("invalid group reference {group}"), esc.len() - 1));
        }
        if esc.len() == 2 {
            if is_ascii_letter(c) {
                return Err(src.error(format!("bad escape {}", chars_str(&esc)), esc.len()));
            }
            return Ok(Node::Literal(c));
        }
        Err(src.error(format!("bad escape {}", chars_str(&esc)), esc.len()))
    }

    fn opengroup(&mut self, name: Option<&[u32]>, src: &Source) -> R<usize> {
        let gid = self.ngroups();
        self.group_widths.push(None);
        let off = name.map_or(0, |n| n.len() + 1);
        if self.ngroups() > MAXGROUPS {
            return Err(src.error("too many groups", off));
        }
        if let Some(name) = name {
            if let Some(&ogid) = self.name_map.get(name) {
                return Err(src.error(
                    format!(
                        "redefinition of group name {} as group {gid}; was group {ogid}",
                        self.repr(name)
                    ),
                    off,
                ));
            }
            self.name_map.insert(name.to_vec(), gid);
            self.names.push((chars_to_string(name), gid));
        }
        Ok(gid)
    }

    fn parse_flags(&mut self, src: &mut Source, mut ch: Tok) -> R<Option<(u32, u32)>> {
        let mut add: u32 = 0;
        let mut del: u32 = 0;
        if !ch.is('-') {
            loop {
                let flag = flag_of(ch.c).unwrap();
                if !self.is_bytes {
                    if ch.is('L') {
                        return Err(src.error(
                            "bad inline flags: cannot use 'L' flag with a str pattern",
                            0,
                        ));
                    }
                } else if ch.is('u') {
                    return Err(src.error(
                        "bad inline flags: cannot use 'u' flag with a bytes pattern",
                        0,
                    ));
                }
                add |= flag;
                if flag & TYPE_FLAGS != 0 && (add & TYPE_FLAGS) != flag {
                    return Err(src.error(
                        "bad inline flags: flags 'a', 'u' and 'L' are incompatible",
                        0,
                    ));
                }
                let Some(t) = src.get()? else {
                    return Err(src.error("missing -, : or )", 0));
                };
                ch = t;
                if ch.is(')') || ch.is('-') || ch.is(':') {
                    break;
                }
                if ch.esc || flag_of(ch.c).is_none() {
                    let msg = if !ch.esc && (self.prov.is_alpha)(ch.c) {
                        "unknown flag"
                    } else {
                        "missing -, : or )"
                    };
                    return Err(src.error(msg, ch.len()));
                }
            }
        }
        if ch.is(')') {
            self.flags |= add;
            return Ok(None);
        }
        if add & GLOBAL_FLAGS != 0 {
            return Err(src.error("bad inline flags: cannot turn on global flag", 1));
        }
        if ch.is('-') {
            let Some(t) = src.get()? else {
                return Err(src.error("missing flag", 0));
            };
            ch = t;
            if ch.esc || flag_of(ch.c).is_none() {
                let msg = if !ch.esc && (self.prov.is_alpha)(ch.c) {
                    "unknown flag"
                } else {
                    "missing flag"
                };
                return Err(src.error(msg, ch.len()));
            }
            loop {
                let flag = flag_of(ch.c).unwrap();
                if flag & TYPE_FLAGS != 0 {
                    return Err(src.error(
                        "bad inline flags: cannot turn off flags 'a', 'u' and 'L'",
                        0,
                    ));
                }
                del |= flag;
                let Some(t) = src.get()? else {
                    return Err(src.error("missing :", 0));
                };
                ch = t;
                if ch.is(':') {
                    break;
                }
                if ch.esc || flag_of(ch.c).is_none() {
                    let msg = if !ch.esc && (self.prov.is_alpha)(ch.c) {
                        "unknown flag"
                    } else {
                        "missing :"
                    };
                    return Err(src.error(msg, ch.len()));
                }
            }
        }
        if del & GLOBAL_FLAGS != 0 {
            return Err(src.error("bad inline flags: cannot turn off global flag", 1));
        }
        if add & del != 0 {
            return Err(src.error("bad inline flags: flag turned on and off", 1));
        }
        Ok(Some((add, del)))
    }

    /// Parse after '(' has been consumed.  Returns `None` when nothing is to
    /// be appended (comments, global flags).
    fn parse_group(
        &mut self,
        src: &mut Source,
        verbose: &mut bool,
        nested: usize,
        first: bool,
        sub: &[Node],
    ) -> R<Option<Node>> {
        let start = src.tell() - 1;
        let mut capture = true;
        let mut atomic = false;
        let mut name: Option<Vec<u32>> = None;
        let mut add: u32 = 0;
        let mut del: u32 = 0;
        if src.eat('?')? {
            let Some(ch) = src.get()? else {
                return Err(src.error("unexpected end of pattern", 0));
            };
            if ch.is('P') {
                if src.eat('<')? {
                    let n = src.getuntil('>', "group name")?;
                    if !self.is_identifier(&n) {
                        return Err(src.error(
                            format!("bad character in group name {}", self.repr(&n)),
                            n.len() + 1,
                        ));
                    }
                    name = Some(n);
                } else if src.eat('=')? {
                    let n = src.getuntil(')', "group name")?;
                    if !self.is_identifier(&n) {
                        return Err(src.error(
                            format!("bad character in group name {}", self.repr(&n)),
                            n.len() + 1,
                        ));
                    }
                    let Some(&gid) = self.name_map.get(&n) else {
                        return Err(src.error(
                            format!("unknown group name {}", self.repr(&n)),
                            n.len() + 1,
                        ));
                    };
                    if !self.checkgroup(gid) {
                        return Err(src.error("cannot refer to an open group", n.len() + 1));
                    }
                    self.checklookbehindgroup(gid, src)?;
                    return Ok(Some(Node::GroupRef(gid)));
                } else {
                    let Some(c2) = src.get()? else {
                        return Err(src.error("unexpected end of pattern", 0));
                    };
                    let mut m = vec!['?' as u32, 'P' as u32];
                    c2.push_to(&mut m);
                    return Err(src.error(
                        format!("unknown extension {}", chars_str(&m)),
                        c2.len() + 2,
                    ));
                }
            } else if ch.is(':') {
                capture = false;
            } else if ch.is('#') {
                loop {
                    if src.next.is_none() {
                        return Err(src.error(
                            "missing ), unterminated comment",
                            src.tell() - start,
                        ));
                    }
                    if src.get()?.is_some_and(|t| t.is(')')) {
                        break;
                    }
                }
                return Ok(None);
            } else if ch.is('=') || ch.is('!') || ch.is('<') {
                let mut kind = ch;
                let mut behind = false;
                let mut saved_lb = None;
                if ch.is('<') {
                    let Some(c2) = src.get()? else {
                        return Err(src.error("unexpected end of pattern", 0));
                    };
                    if !(c2.is('=') || c2.is('!')) {
                        let mut m = vec!['?' as u32, '<' as u32];
                        c2.push_to(&mut m);
                        return Err(src.error(
                            format!("unknown extension {}", chars_str(&m)),
                            c2.len() + 2,
                        ));
                    }
                    kind = c2;
                    behind = true;
                    saved_lb = Some(self.lookbehind_groups);
                    if self.lookbehind_groups.is_none() {
                        self.lookbehind_groups = Some(self.ngroups());
                    }
                }
                let p = self.parse_sub(src, *verbose, nested + 1)?;
                if let Some(old) = saved_lb {
                    if old.is_none() {
                        self.lookbehind_groups = None;
                    }
                }
                if !src.eat(')')? {
                    return Err(src.error(
                        "missing ), unterminated subpattern",
                        src.tell() - start,
                    ));
                }
                if kind.is('=') {
                    return Ok(Some(Node::Assert { behind, neg: false, body: p }));
                } else if !p.is_empty() {
                    return Ok(Some(Node::Assert { behind, neg: true, body: p }));
                } else {
                    return Ok(Some(Node::Failure));
                }
            } else if ch.is('(') {
                let condname = src.getuntil(')', "group name")?;
                let off = condname.len() + 1;
                let condgroup: usize;
                if self.is_identifier(&condname) {
                    match self.name_map.get(&condname) {
                        Some(&g) => condgroup = g,
                        None => {
                            return Err(src.error(
                                format!("unknown group name {}", self.repr(&condname)),
                                off,
                            ));
                        }
                    }
                } else if condname.iter().all(|&c| is_digit(c)) {
                    let v = digits_value(&condname);
                    if v == 0 {
                        return Err(src.error("bad group number", off));
                    }
                    if v >= MAXGROUPS as u128 {
                        return Err(src.error(
                            format!("invalid group reference {}", digits_display(&condname)),
                            off,
                        ));
                    }
                    condgroup = v as usize;
                    if !self.grouprefpos.iter().any(|&(g, _)| g == condgroup) {
                        self.grouprefpos.push((condgroup, src.tell() - off));
                    }
                } else {
                    return Err(src.error(
                        format!("bad character in group name {}", self.repr(&condname)),
                        off,
                    ));
                }
                self.checklookbehindgroup(condgroup, src)?;
                let yes = self.parse_seq(src, *verbose, nested + 1, false)?;
                let no = if src.eat('|')? {
                    let n = self.parse_seq(src, *verbose, nested + 1, false)?;
                    if src.next_is('|') {
                        return Err(src.error(
                            "conditional backref with more than two branches",
                            0,
                        ));
                    }
                    Some(n)
                } else {
                    None
                };
                if !src.eat(')')? {
                    return Err(src.error(
                        "missing ), unterminated subpattern",
                        src.tell() - start,
                    ));
                }
                return Ok(Some(Node::GroupRefExists { group: condgroup, yes, no }));
            } else if ch.is('>') {
                capture = false;
                atomic = true;
            } else if !ch.esc && (flag_of(ch.c).is_some() || ch.c == '-' as u32) {
                match self.parse_flags(src, ch)? {
                    None => {
                        if !first || !sub.is_empty() {
                            return Err(src.error(
                                "global flags not at the start of the expression",
                                src.tell() - start,
                            ));
                        }
                        *verbose = self.flags & VERBOSE != 0 || *verbose;
                        return Ok(None);
                    }
                    Some((a, d)) => {
                        add = a;
                        del = d;
                        capture = false;
                    }
                }
            } else {
                let mut m = vec!['?' as u32];
                ch.push_to(&mut m);
                return Err(src.error(format!("unknown extension {}", chars_str(&m)), ch.len() + 1));
            }
        }
        let group = if capture { Some(self.opengroup(name.as_deref(), src)?) } else { None };
        let sub_verbose = (*verbose || add & VERBOSE != 0) && del & VERBOSE == 0;
        let p = self.parse_sub(src, sub_verbose, nested + 1)?;
        if !src.eat(')')? {
            return Err(src.error("missing ), unterminated subpattern", src.tell() - start));
        }
        if let Some(g) = group {
            let w = width(&p, &self.group_widths);
            self.group_widths[g] = Some(w);
        }
        if atomic {
            return Ok(Some(Node::Atomic(p)));
        }
        Ok(Some(Node::Subpattern { group, add, del, body: p }))
    }
}

fn simple_escape(c: u32) -> Option<u32> {
    Some(match char::from_u32(c)? {
        'a' => 7,
        'b' => 8,
        'f' => 12,
        'n' => 10,
        'r' => 13,
        't' => 9,
        'v' => 11,
        '\\' => 92,
        _ => return None,
    })
}

fn category_escape(c: u32) -> Option<Category> {
    Some(match char::from_u32(c)? {
        'd' => Category::Digit,
        'D' => Category::NotDigit,
        's' => Category::Space,
        'S' => Category::NotSpace,
        'w' => Category::Word,
        'W' => Category::NotWord,
        _ => return None,
    })
}

fn hex_value(v: &[u32]) -> u32 {
    let mut n: u32 = 0;
    for &c in v {
        n = n * 16 + char::from_u32(c).unwrap().to_digit(16).unwrap();
    }
    n
}

fn oct_value(v: &[u32]) -> u32 {
    let mut n: u32 = 0;
    for &c in v {
        n = n * 8 + (c - 0x30);
    }
    n
}

// ---------------------------------------------------------------------------
// Replacement templates


