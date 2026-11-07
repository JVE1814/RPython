//! `rpy-sre`: regular expression engine with CPython `re` semantics.
//!
//! See `NOTES.md` in the crate directory for an overview of the API.

mod compiler;
mod debug;
mod error;
mod parser;
mod search;
mod template;
pub mod unicode;
mod vm;

use std::cell::RefCell;

pub use error::{format_error_message, line_col, CompileError, TemplateError, Warning, WarningKind};
pub use template::{expand_template, TemplateItem};
pub use unicode::{UnicodeProvider, DEFAULT_PROVIDER};
pub use vm::{CodeUnit, Scratch};

/// Flag values (identical to CPython's `re` flags).
pub mod flags {
    pub const TEMPLATE: u32 = 1;
    pub const IGNORECASE: u32 = 2;
    pub const LOCALE: u32 = 4;
    pub const MULTILINE: u32 = 8;
    pub const DOTALL: u32 = 16;
    pub const UNICODE: u32 = 32;
    pub const VERBOSE: u32 = 64;
    pub const DEBUG: u32 = 128;
    pub const ASCII: u32 = 256;
}

/// `_sre.MAXREPEAT`: repeat counts must be smaller than this; also the
/// "infinite" upper bound.
pub const MAXREPEAT: u32 = u32::MAX;
/// `_sre.MAXGROUPS`.
pub const MAXGROUPS: usize = 0x3FFF_FFFF;
/// `_sre.CODESIZE`.
pub const CODESIZE: usize = 4;
/// `_sre.MAGIC` (informational only).
pub const MAGIC: u32 = 20230612;

/// Pattern source text.
#[derive(Clone, Copy, Debug)]
pub enum PatternSource<'a> {
    /// str pattern stored as UCS-1 (Latin-1) code units.
    Latin1(&'a [u8]),
    /// str pattern stored as UCS-2.
    Ucs2(&'a [u16]),
    /// str pattern stored as UCS-4 (code points).
    Ucs4(&'a [u32]),
    /// bytes pattern.
    Bytes(&'a [u8]),
}

impl PatternSource<'_> {
    fn to_vec(self) -> (Vec<u32>, bool) {
        match self {
            PatternSource::Latin1(s) => (s.iter().map(|&c| c as u32).collect(), false),
            PatternSource::Ucs2(s) => (s.iter().map(|&c| c as u32).collect(), false),
            PatternSource::Ucs4(s) => (s.to_vec(), false),
            PatternSource::Bytes(s) => (s.iter().map(|&c| c as u32).collect(), true),
        }
    }
}

/// A compiled regular expression.
#[derive(Clone, Debug)]
pub struct Pattern {
    prog: compiler::Program,
    pattern: Vec<u32>,
    is_bytes: bool,
    flags: u32,
    groups: usize,
    group_names: Vec<(String, usize)>,
    warnings: Vec<Warning>,
    prov: UnicodeProvider,
}

/// Result of a successful match: spans of every group (index 0 = whole
/// match).  Unmatched groups have span `(-1, -1)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Captures {
    pub spans: Vec<(isize, isize)>,
    /// Index of the last closed group (`Match.lastindex`), if any.
    pub lastindex: Option<usize>,
}

impl Captures {
    pub fn start(&self) -> usize {
        self.spans[0].0 as usize
    }
    pub fn end(&self) -> usize {
        self.spans[0].1 as usize
    }
    pub fn span(&self, g: usize) -> Option<(usize, usize)> {
        let (a, b) = *self.spans.get(g)?;
        if a < 0 { None } else { Some((a as usize, b as usize)) }
    }
}

/// How a single match attempt is anchored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchMode {
    /// `Pattern.match` / `prefixmatch`: anchored at `pos`.
    Prefix,
    /// `Pattern.fullmatch`: anchored at `pos` and must end at `endpos`.
    Full,
    /// `Pattern.search`: first match at or after `pos`.
    Search,
}

thread_local! {
    static SCRATCH: RefCell<Vec<Scratch>> = const { RefCell::new(Vec::new()) };
}

fn with_scratch<R>(f: impl FnOnce(&mut Scratch) -> R) -> R {
    let mut sc = SCRATCH.with(|p| p.borrow_mut().pop()).unwrap_or_default();
    let r = f(&mut sc);
    SCRATCH.with(|p| {
        let mut p = p.borrow_mut();
        if p.len() < 4 {
            p.push(sc);
        }
    });
    r
}

/// Compile a pattern with the default Unicode provider.
pub fn compile(src: PatternSource<'_>, flags: u32) -> Result<Pattern, CompileError> {
    compile_with(src, flags, &DEFAULT_PROVIDER)
}

/// Compile a pattern with an explicit Unicode provider.
pub fn compile_with(
    src: PatternSource<'_>,
    flags: u32,
    prov: &UnicodeProvider,
) -> Result<Pattern, CompileError> {
    let (pattern, is_bytes) = src.to_vec();
    let parsed = parser::parse(&pattern, is_bytes, flags, prov)?;
    let prog = compiler::Compiler::compile(
        &parsed.nodes,
        parsed.flags,
        parsed.groups,
        &parsed.group_widths,
        is_bytes,
        prov,
    )?;
    Ok(Pattern {
        prog,
        pattern,
        is_bytes,
        flags: parsed.flags,
        groups: parsed.groups,
        group_names: parsed.group_names,
        warnings: parsed.warnings,
        prov: *prov,
    })
}

/// Text `re.DEBUG` prints for a pattern: CPython-format parse tree and code
/// listing (tail2).  Errors are those compilation would raise.
pub fn debug_text(
    src: PatternSource<'_>,
    flags: u32,
    prov: &UnicodeProvider,
) -> Result<String, CompileError> {
    let (pattern, is_bytes) = src.to_vec();
    let parsed = parser::parse(&pattern, is_bytes, flags, prov)?;
    debug::debug_text(&parsed, prov)
}

/// Convenience: compile a str pattern given as Rust `&str`.
pub fn compile_str(pattern: &str, flags: u32) -> Result<Pattern, CompileError> {
    let v: Vec<u32> = pattern.chars().map(|c| c as u32).collect();
    compile(PatternSource::Ucs4(&v), flags)
}

/// Convenience: compile a bytes pattern.
pub fn compile_bytes(pattern: &[u8], flags: u32) -> Result<Pattern, CompileError> {
    compile(PatternSource::Bytes(pattern), flags)
}

impl Pattern {
    /// Pattern source as code points (bytes values for bytes patterns).
    pub fn pattern(&self) -> &[u32] {
        &self.pattern
    }
    pub fn is_bytes(&self) -> bool {
        self.is_bytes
    }
    /// `Pattern.flags` (includes inline global flags and implicit UNICODE).
    pub fn flags(&self) -> u32 {
        self.flags
    }
    /// `Pattern.groups`: number of capturing groups.
    pub fn groups(&self) -> usize {
        self.groups
    }
    /// `Pattern.groupindex` in definition order.
    pub fn group_index(&self) -> &[(String, usize)] {
        &self.group_names
    }
    /// Look up a named group.
    pub fn group_by_name(&self, name: &str) -> Option<usize> {
        self.group_names.iter().find(|(n, _)| n == name).map(|&(_, i)| i)
    }
    /// Name of a group (for `Match.lastgroup`).
    pub fn group_name(&self, index: usize) -> Option<&str> {
        self.group_names.iter().find(|(_, i)| *i == index).map(|(n, _)| n.as_str())
    }
    /// Warnings produced at compile time (FutureWarning for possible set
    /// operations / nested sets).
    pub fn warnings(&self) -> &[Warning] {
        &self.warnings
    }
    pub fn provider(&self) -> &UnicodeProvider {
        &self.prov
    }

    /// Run one match attempt.  `pos`/`endpos` are clamped like CPython does.
    pub fn exec<T: CodeUnit>(
        &self,
        s: &[T],
        pos: usize,
        endpos: usize,
        mode: MatchMode,
    ) -> Option<Captures> {
        with_scratch(|sc| self.exec_with(sc, s, pos, endpos, mode, false))
    }

    /// Like [`Pattern::exec`] with caller-provided scratch memory and the
    /// `must_advance` flag used by the scanner (an empty match at `pos` is
    /// rejected).
    pub fn exec_with<T: CodeUnit>(
        &self,
        sc: &mut Scratch,
        s: &[T],
        pos: usize,
        endpos: usize,
        mode: MatchMode,
        must_advance: bool,
    ) -> Option<Captures> {
        let end = endpos.min(s.len());
        let pos = pos.min(s.len());
        if pos > end {
            return None;
        }
        search::exec(self, sc, s, pos, end, mode, must_advance)
    }

    /// `Pattern.match(s, pos, endpos)`.
    pub fn match_at<T: CodeUnit>(&self, s: &[T], pos: usize, endpos: usize) -> Option<Captures> {
        self.exec(s, pos, endpos, MatchMode::Prefix)
    }
    /// `Pattern.fullmatch(s, pos, endpos)`.
    pub fn fullmatch<T: CodeUnit>(&self, s: &[T], pos: usize, endpos: usize) -> Option<Captures> {
        self.exec(s, pos, endpos, MatchMode::Full)
    }
    /// `Pattern.search(s, pos, endpos)`.
    pub fn search<T: CodeUnit>(&self, s: &[T], pos: usize, endpos: usize) -> Option<Captures> {
        self.exec(s, pos, endpos, MatchMode::Search)
    }

    /// Iterator-style scanner implementing the finditer/findall/sub/split
    /// empty-match rules.
    pub fn scanner<'p, 's, T: CodeUnit>(
        &'p self,
        s: &'s [T],
        pos: usize,
        endpos: usize,
    ) -> Scanner<'p, 's, T> {
        let end = endpos.min(s.len());
        let pos = pos.min(s.len());
        Scanner { pat: self, s, pos, end, must_advance: false, done: pos > end, sc: Scratch::new() }
    }

    /// Parse a replacement template (`re.sub` repl string) for this pattern.
    /// `repl` holds code points (or byte values for bytes templates).
    pub fn parse_template(&self, repl: &[u32]) -> Result<Vec<TemplateItem>, TemplateError> {
        let lookup = |name: &[u32]| {
            let s = parser::chars_to_string(name);
            self.group_by_name(&s)
        };
        template::parse_template_impl(repl, self.is_bytes, self.groups, &lookup, &self.prov)
    }

    /// Debug helper: number of instructions in the compiled program.
    pub fn program_len(&self) -> usize {
        self.prog.insts.len()
    }
}

/// State for `finditer` & co.  Each call to [`Scanner::search`] returns the
/// next match following CPython's rules (since 3.7): an empty match is not
/// allowed at the position where the previous match ended if that previous
/// match was itself empty.
pub struct Scanner<'p, 's, T: CodeUnit> {
    pat: &'p Pattern,
    s: &'s [T],
    pos: usize,
    end: usize,
    must_advance: bool,
    done: bool,
    sc: Scratch,
}

impl<T: CodeUnit> Scanner<'_, '_, T> {
    fn step(&mut self, mode: MatchMode) -> Option<Captures> {
        if self.done {
            return None;
        }
        let m = self.pat.exec_with(&mut self.sc, self.s, self.pos, self.end, mode, self.must_advance);
        match m {
            Some(c) => {
                let (st, en) = (c.start(), c.end());
                self.must_advance = st == en;
                self.pos = en;
                Some(c)
            }
            None => {
                self.done = true;
                None
            }
        }
    }
    /// `scanner.search()`.
    pub fn search(&mut self) -> Option<Captures> {
        self.step(MatchMode::Search)
    }
    /// `scanner.match()`.
    pub fn match_(&mut self) -> Option<Captures> {
        self.step(MatchMode::Prefix)
    }
    /// Current position (end of the previous match).
    pub fn pos(&self) -> usize {
        self.pos
    }
}

impl<T: CodeUnit> Iterator for Scanner<'_, '_, T> {
    type Item = Captures;
    fn next(&mut self) -> Option<Captures> {
        self.search()
    }
}

/// `_sre.ascii_iscased`, `_sre.ascii_tolower`, ... helpers.
pub use unicode::{ascii_iscased, ascii_tolower, unicode_iscased, unicode_tolower};
