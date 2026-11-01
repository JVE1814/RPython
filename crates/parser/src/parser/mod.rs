//! Hand-written PEG-style recursive descent parser for the Python 3.15 grammar.
//!
//! Like CPython, parsing runs in two passes: a fast first pass without the `invalid_*` rules,
//! and (only on failure) a second pass with them enabled to produce specific error messages.

mod expr;
mod invalid;
mod params;
mod pattern;
mod stmt;
mod strings;
mod targets;

use crate::ast::*;
use crate::error::{byte_col_to_char_offset, ErrorKind, SyntaxError, Warning};
use crate::token::{Kw, Token, TokenType};
use crate::tokenizer::{self, lex_error_to_syntax, LexError, LexOptions, Lexer};
use crate::UnicodeHooks;

pub(crate) type PErr = Box<SyntaxError>;
/// `Ok(Some(x))`: rule matched; `Ok(None)`: no match (caller backtracks); `Err`: hard error.
pub(crate) type R<T> = Result<Option<T>, PErr>;

/// Compile mode (the `mode` argument of `compile()`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// 'exec' -> `Mod::Module`
    Exec,
    /// 'eval' -> `Mod::Expression`
    Eval,
    /// 'single' -> `Mod::Interactive`
    Single,
    /// 'func_type' -> `Mod::FunctionType`
    FuncType,
}

/// Parser options.
#[derive(Clone, Debug)]
pub struct ParseOptions {
    pub mode: Mode,
    /// PyCF_TYPE_COMMENTS: produce type_comment fields and TypeIgnore entries.
    pub type_comments: bool,
    /// `feature_version` minor version (e.g. Some(7) for 3.7); None = current.
    pub feature_version: Option<u32>,
    /// CO_FUTURE_BARRY_AS_BDFL already in effect (from compile flags).
    pub barry_as_flufl: bool,
    /// PyCF_ALLOW_INCOMPLETE_INPUT (codeop): report "incomplete input" when input ends early.
    pub allow_incomplete_input: bool,
    /// PyCF_DONT_IMPLY_DEDENT ('single' mode): no implicit DEDENTs at a mid-line EOF.
    pub dont_imply_dedent: bool,
    /// Stack the parser may use on the caller's thread (bytes, default 256 KiB). Deeper
    /// recursion continues on scoped helper threads with 16 MiB stacks (if `grow_stack`),
    /// otherwise raises MemoryError("... too complex ..."). Nesting is in any case limited
    /// to 6000 levels, like CPython's parser.
    pub max_stack_bytes: usize,
    /// Allow helper threads for very deep nesting (default true).
    pub grow_stack: bool,
    /// `sys.get_int_max_str_digits()` for decimal int literals (0 = unlimited; default 4300).
    pub int_max_str_digits: usize,
    /// Unicode database hooks.
    pub hooks: UnicodeHooks,
    /// File name used in a few messages (e.g. "Non-UTF-8 code ... in file X").
    pub filename: String,
    /// Decoder for declared source encodings not implemented natively (see
    /// [`tokenizer::DecodeHook`]).
    pub decode: Option<tokenizer::DecodeHook>,
}

impl Default for ParseOptions {
    fn default() -> Self {
        ParseOptions {
            mode: Mode::Exec,
            type_comments: false,
            feature_version: None,
            barry_as_flufl: false,
            allow_incomplete_input: false,
            dont_imply_dedent: false,
            max_stack_bytes: 256 << 10,
            grow_stack: true,
            int_max_str_digits: 4300,
            hooks: UnicodeHooks::default(),
            filename: "<string>".to_string(),
            decode: None,
        }
    }
}

impl ParseOptions {
    pub fn new(mode: Mode) -> ParseOptions {
        ParseOptions { mode, ..Default::default() }
    }
}

/// Result of parsing: the AST or a syntax error, plus warnings (in source order, deduplicated)
/// that the runtime should emit (and possibly turn into errors).
#[derive(Debug)]
pub struct ParseOutput {
    pub result: Result<Mod, SyntaxError>,
    pub warnings: Vec<Warning>,
}

/// Parse decoded source text.
pub fn parse(src: &str, opts: &ParseOptions) -> ParseOutput {
    let translated = tokenizer::translate_newlines(src);
    let src: &str = &translated;
    let mut p = Parser::new(src, opts);
    let result = p.run();
    let mut warnings = std::mem::take(&mut p.lex_warnings);
    warnings.append(&mut p.warnings);
    warnings.sort_by_key(|w| (w.lineno, w.col_offset));
    warnings.dedup_by(|a, b| a.lineno == b.lineno && a.col_offset == b.col_offset && a.msg == b.msg);
    ParseOutput { result, warnings }
}

/// Parse raw source bytes (handles BOM and PEP 263 encoding declarations).
pub fn parse_bytes(src: &[u8], opts: &ParseOptions) -> ParseOutput {
    match tokenizer::decode_source_with(src, opts.decode, &opts.filename) {
        Ok(s) => parse(&s, opts),
        Err(e) => ParseOutput { result: Err(e), warnings: Vec::new() },
    }
}

/// Which kind of targets are being validated (for error messages).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TargetsType {
    Star,
    Del,
    For,
}

pub(crate) struct Parser<'a> {
    pub(crate) src: &'a str,
    pub(crate) toks: Vec<Token>,
    /// tokenizer error located at the last token (ERRORTOKEN)
    lex_err: Option<LexError>,
    /// the tokenizer stopped at the end of the input (E_EOF/E_EOFS/E_EOLS)
    lex_eof: bool,
    /// index of the first token produced after the tokenizer read EOF
    eof_start: usize,
    pub(crate) p: usize,
    /// number of tokens "fetched" (max index looked at + 1), as CPython's p->fill
    pub(crate) fill: usize,
    pub(crate) invalid: bool,
    pub(crate) opts: &'a ParseOptions,
    pub(crate) warnings: Vec<Warning>,
    lex_warnings: Vec<Warning>,
    pub(crate) flufl: bool,
    pub(crate) depth: usize,
    /// Within an f-string replacement field expression (nesting count).
    pub(crate) fstring_depth: usize,
    /// lazy-import / misc state
    pub(crate) in_lambda: usize,
    pub(crate) no_genexp_arg: bool,
    /// first pass: top-level statement start -> end token index
    top_ok: std::collections::HashMap<usize, usize>,
    /// furthest token index fetched by the first pass
    first_fail: usize,
    stack_base: usize,
    /// number of helper-thread stack segments currently in use
    segment: usize,
    /// Tokens that had a type comment consumed etc. (unused)
    pub(crate) type_ignores: Vec<TypeIgnore>,
}

/// Recursion limit of the parser (in addition to the stack budget, see
/// `ParseOptions::max_stack_bytes`).
const MAX_DEPTH: usize = 6000;
/// Maximum length of iteratively parsed chains that produce nested ASTs.
const MAX_CHAIN: usize = 5000;
/// Stack size of the helper threads used to continue very deep recursions.
const STACK_SEGMENT: usize = 16 << 20;

impl<'a> Parser<'a> {
    pub(crate) fn new(src: &'a str, opts: &'a ParseOptions) -> Parser<'a> {
        let lopts = LexOptions {
            extra_tokens: false,
            type_comments: opts.type_comments,
            hooks: opts.hooks,
            implicit_newline: opts.mode == Mode::Exec || opts.mode == Mode::FuncType,
            single_input: opts.mode == Mode::Single,
            dont_imply_dedent: opts.dont_imply_dedent,
        };
        let mut lx = Lexer::new(src, lopts);
        let mut toks = Vec::with_capacity(src.len() / 4 + 16);
        let mut lex_err = None;
        let mut type_ignores = Vec::new();
        let mut eof_start: Option<usize> = None;
        loop {
            match lx.next_token() {
                Ok(t) => {
                    if t.ty == TokenType::TYPE_IGNORE {
                        type_ignores.push(TypeIgnore { lineno: t.lineno as i32, tag: t.text(src).to_string() });
                        continue;
                    }
                    let end = t.ty == TokenType::ENDMARKER;
                    toks.push(t);
                    if lx.reached_eof && eof_start.is_none() {
                        eof_start = Some(toks.len() - 1);
                    }
                    if end {
                        break;
                    }
                }
                Err(e) => {
                    let (ln, col) = match toks.last() {
                        Some(t) => (t.end_lineno, t.end_col),
                        None => (1, 0),
                    };
                    let p = src.len() as u32;
                    toks.push(Token { ty: TokenType::ERRORTOKEN, kw: Kw::None, level: 0, start: p, end: p, lineno: ln, col, end_lineno: ln, end_col: col });
                    lex_err = Some(e);
                    break;
                }
            }
        }
        let lex_warnings = std::mem::take(&mut lx.warnings);
        let lex_eof = lx.eof_error;
        let eof_start = eof_start.unwrap_or(toks.len() - 1);
        Parser {
            src,
            toks,
            lex_err,
            lex_eof,
            eof_start,
            p: 0,
            fill: 0,
            invalid: false,
            opts,
            warnings: Vec::new(),
            lex_warnings,
            flufl: opts.barry_as_flufl,
            depth: 0,
            fstring_depth: 0,
            in_lambda: 0,
            no_genexp_arg: false,
            top_ok: std::collections::HashMap::new(),
            first_fail: usize::MAX,
            stack_base: 0,
            segment: 0,
            type_ignores,
        }
    }

    // ------------------------------------------------------------------ token access

    #[inline]
    pub(crate) fn peek(&mut self) -> Token {
        let p = self.p;
        if p >= self.fill {
            self.fill = p + 1;
        }
        self.toks[p]
    }
    #[inline]
    pub(crate) fn peek_ty(&mut self) -> TokenType {
        self.peek().ty
    }
    /// Look ahead `k` tokens beyond the current one (never past the last token).
    #[inline]
    pub(crate) fn peek_n(&mut self, k: usize) -> Token {
        let i = (self.p + k).min(self.toks.len() - 1);
        if i >= self.fill {
            self.fill = i + 1;
        }
        self.toks[i]
    }
    #[inline]
    pub(crate) fn advance(&mut self) -> Token {
        let t = self.peek();
        if self.p < self.toks.len() - 1 {
            self.p += 1;
        }
        t
    }
    #[inline]
    pub(crate) fn at(&mut self, ty: TokenType) -> bool {
        self.peek().ty == ty
    }
    #[inline]
    pub(crate) fn at_kw(&mut self, kw: Kw) -> bool {
        let t = self.peek();
        t.ty == TokenType::NAME && t.kw == kw
    }
    #[inline]
    pub(crate) fn eat(&mut self, ty: TokenType) -> Option<Token> {
        let t = self.peek();
        if t.ty == ty {
            self.advance();
            Some(t)
        } else {
            None
        }
    }
    #[inline]
    pub(crate) fn eat_kw(&mut self, kw: Kw) -> Option<Token> {
        let t = self.peek();
        if t.ty == TokenType::NAME && t.kw == kw {
            self.advance();
            Some(t)
        } else {
            None
        }
    }
    /// A NAME token that is not a hard keyword.
    #[inline]
    pub(crate) fn at_name(&mut self) -> bool {
        let t = self.peek();
        t.ty == TokenType::NAME && !t.kw.is_hard()
    }
    pub(crate) fn eat_name(&mut self) -> Option<Token> {
        let t = self.peek();
        if t.ty == TokenType::NAME && !t.kw.is_hard() {
            self.advance();
            Some(t)
        } else {
            None
        }
    }
    #[inline]
    pub(crate) fn text(&self, t: &Token) -> &'a str {
        &self.src[t.start as usize..t.end as usize]
    }
    pub(crate) fn tok_at(&self, i: usize) -> Token {
        self.toks[i]
    }

    /// Identifier string for a NAME token (NFKC-normalized if non-ASCII).
    pub(crate) fn ident(&self, t: &Token) -> Identifier {
        let s = self.text(t);
        if s.is_ascii() {
            s.to_string()
        } else {
            (self.opts.hooks.nfkc)(s)
        }
    }

    // ------------------------------------------------------------------ locations

    #[inline]
    pub(crate) fn start_pos(&self, i: usize) -> (i32, i32) {
        let t = &self.toks[i];
        (t.lineno as i32, t.col)
    }
    /// End position of the last non-whitespace token before the current position.
    #[inline]
    pub(crate) fn end_pos(&self) -> (i32, i32) {
        let mut m = self.p;
        while m > 0 {
            m -= 1;
            let t = &self.toks[m];
            let ty = t.ty;
            if ty != TokenType::ENDMARKER && !(ty == TokenType::NEWLINE || ty == TokenType::INDENT || ty == TokenType::DEDENT) {
                return (t.end_lineno as i32, t.end_col);
            }
        }
        let t = &self.toks[0];
        (t.end_lineno as i32, t.end_col)
    }
    #[inline]
    pub(crate) fn loc(&self, start: usize) -> Loc {
        let (l, c) = self.start_pos(start);
        let (el, ec) = self.end_pos();
        Loc { lineno: l, col_offset: c, end_lineno: el, end_col_offset: ec }
    }
    pub(crate) fn tok_loc(t: &Token) -> Loc {
        Loc { lineno: t.lineno as i32, col_offset: t.col, end_lineno: t.end_lineno as i32, end_col_offset: t.end_col }
    }
    #[allow(dead_code)]
    pub(crate) fn span(a: Loc, b: Loc) -> Loc {
        Loc { lineno: a.lineno, col_offset: a.col_offset, end_lineno: b.end_lineno, end_col_offset: b.end_col_offset }
    }

    // ------------------------------------------------------------------ errors

    /// Build a SyntaxError at known byte positions (0-based columns), CPython style.
    #[cold]
    #[inline(never)]
    pub(crate) fn error_at(&self, kind: ErrorKind, msg: impl Into<String>, lineno: i32, col: i32, end_lineno: i32, end_col: i32) -> PErr {
        Box::new(make_error_cpython(self.src, kind, msg.into(), lineno, col, end_lineno, end_col))
    }
    pub(crate) fn err_loc(&self, msg: impl Into<String>, l: Loc) -> PErr {
        self.error_at(ErrorKind::Syntax, msg, l.lineno, l.col_offset, l.end_lineno, l.end_col_offset)
    }
    pub(crate) fn err_range(&self, msg: impl Into<String>, a: Loc, b: Loc) -> PErr {
        self.error_at(ErrorKind::Syntax, msg, a.lineno, a.col_offset, b.end_lineno, b.end_col_offset)
    }
    pub(crate) fn err_tok(&self, msg: impl Into<String>, t: &Token) -> PErr {
        self.err_loc(msg, Self::tok_loc(t))
    }
    /// RAISE_SYNTAX_ERROR: location of the last fetched token.
    #[cold]
    #[inline(never)]
    pub(crate) fn err_last(&self, kind: ErrorKind, msg: impl Into<String>) -> PErr {
        let i = self.fill.max(1) - 1;
        let t = self.toks[i];
        if t.col < 0 {
            // INDENT/DEDENT: CPython uses the tokenizer position
            let (l, c) = self.after_tok_pos(i);
            return self.error_at(kind, msg, l, c - 1, l, -1);
        }
        self.error_at(kind, msg, t.lineno as i32, t.col, t.end_lineno as i32, t.end_col)
    }
    fn after_tok_pos(&self, i: usize) -> (i32, i32) {
        // position of the next real token (approximates tokenizer cur for INDENT/DEDENT)
        let mut j = i;
        while j < self.toks.len() && self.toks[j].col < 0 {
            j += 1;
        }
        if j < self.toks.len() {
            let t = &self.toks[j];
            (t.lineno as i32, t.col)
        } else {
            (self.toks[i].lineno as i32, 0)
        }
    }
    /// RAISE_SYNTAX_ERROR_ON_NEXT_TOKEN
    pub(crate) fn err_next(&mut self, msg: impl Into<String>) -> PErr {
        let t = self.peek();
        self.err_tok(msg, &t)
    }

    pub(crate) fn enter(&mut self) -> Result<(), PErr> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(Self::too_complex());
        }
        Ok(())
    }

    #[cold]
    fn too_complex() -> PErr {
        Box::new(SyntaxError { kind: ErrorKind::MemoryError, ..SyntaxError::simple("Parser stack overflowed - Python source too complex to parse") })
    }

    /// Run a recursive step with depth accounting. When the stack used on the current stack
    /// segment exceeds the budget, the step continues on a scoped helper thread with a fresh
    /// `STACK_SEGMENT` stack, so deep (but legal) nesting never overflows the caller's stack.
    #[inline]
    pub(crate) fn recurse<T: Send>(&mut self, f: impl FnOnce(&mut Self) -> Result<T, PErr> + Send) -> Result<T, PErr> {
        self.enter()?;
        let limit = if self.segment > 0 { STACK_SEGMENT - STACK_SEGMENT / 4 } else { self.opts.max_stack_bytes };
        let r = if self.depth & 3 == 0 && self.stack_used() > limit { self.on_new_stack(f) } else { f(self) };
        self.leave();
        r
    }

    #[cold]
    #[inline(never)]
    fn on_new_stack<T: Send>(&mut self, f: impl FnOnce(&mut Self) -> Result<T, PErr> + Send) -> Result<T, PErr> {
        if !self.opts.grow_stack {
            return Err(Self::too_complex());
        }
        let saved_base = self.stack_base;
        self.segment += 1;
        let r = std::thread::scope(|s| {
            let h = std::thread::Builder::new().name("rpy-parser-stack".into()).stack_size(STACK_SEGMENT).spawn_scoped(s, || {
                let here = 0u8;
                self.stack_base = &here as *const u8 as usize;
                f(self)
            });
            match h {
                Ok(h) => Some(h.join()),
                Err(_) => None,
            }
        });
        self.segment -= 1;
        self.stack_base = saved_base;
        match r {
            Some(Ok(v)) => v,
            Some(Err(panic)) => std::panic::resume_unwind(panic),
            None => Err(Self::too_complex()),
        }
    }

    #[inline(never)]
    fn stack_used(&self) -> usize {
        let here = 0u8;
        let addr = &here as *const u8 as usize;
        self.stack_base.saturating_sub(addr)
    }

    /// Guard for constructs parsed iteratively that still build deeply nested ASTs (trailer
    /// chains, binary operator chains, elif chains): consumers recurse over the AST.
    pub(crate) fn check_chain(&self, n: usize) -> Result<(), PErr> {
        if n > MAX_CHAIN {
            return Err(Box::new(SyntaxError { kind: ErrorKind::RecursionError, ..SyntaxError::simple("maximum recursion depth exceeded during compilation") }));
        }
        Ok(())
    }
    #[inline]
    pub(crate) fn leave(&mut self) {
        self.depth -= 1;
    }

    pub(crate) fn warn(&mut self, msg: String, error_msg: String, l: Loc) {
        let line = tokenizer::line_text(self.src, l.lineno.max(1) as u32);
        let eline = tokenizer::line_text(self.src, l.end_lineno.max(1) as u32);
        self.warnings.push(Warning {
            category: crate::error::WarningCategory::SyntaxWarning,
            msg,
            error_msg,
            lineno: l.lineno,
            col_offset: l.col_offset,
            end_lineno: l.end_lineno,
            end_col_offset: l.end_col_offset,
            offset: byte_col_to_char_offset(&line, l.col_offset),
            end_offset: byte_col_to_char_offset(&eline, l.end_col_offset),
        });
    }

    // ------------------------------------------------------------------ driver

    fn run(&mut self) -> Result<Mod, SyntaxError> {
        let here = 0u8;
        self.stack_base = &here as *const u8 as usize;
        // First pass.
        let r = self.start_rule();
        match r {
            Ok(Some(m)) => {
                if self.opts.mode == Mode::Single && self.bad_single_statement() {
                    return Err(*self.err_last(ErrorKind::Syntax, "multiple statements found while compiling a single statement"));
                }
                return Ok(m);
            }
            Err(e) => return Err(*e),
            Ok(None) => {}
        }
        if self.opts.allow_incomplete_input && self.is_end_of_source() {
            let t = self.toks[self.fill.max(1) - 1];
            return Err(*self.error_at(ErrorKind::IncompleteInput, "incomplete input", t.lineno as i32, t.col.max(0), t.end_lineno as i32, t.end_col.max(0)));
        }
        // Tokenizer error reached?
        let err_idx = if self.lex_err.is_some() { Some(self.toks.len() - 1) } else { None };
        if let Some(ei) = err_idx {
            if self.fill > ei {
                return Err(lex_error_to_syntax(self.src, self.lex_err.clone().unwrap()));
            }
        }
        let last_fill = self.fill;
        self.first_fail = last_fill.saturating_sub(1);
        let last_token = self.toks[last_fill.max(1) - 1];
        // Second pass with invalid rules.
        self.p = 0;
        self.invalid = true;
        self.depth = 0;
        self.fstring_depth = 0;
        let r2 = self.start_rule();
        let err = match r2 {
            Err(e) => {
                // a specific error; tokenizer errors later in the file take precedence
                let e = *e;
                if let Some(ei) = err_idx {
                    if self.fill > ei {
                        return Err(lex_error_to_syntax(self.src, self.lex_err.clone().unwrap()));
                    }
                }
                return Err(self.check_rest_for_errors(e, last_fill));
            }
            Ok(_) => {
                if let Some(ei) = err_idx {
                    if self.fill > ei {
                        return Err(lex_error_to_syntax(self.src, self.lex_err.clone().unwrap()));
                    }
                }
                if last_token.ty == TokenType::INDENT {
                    *self.indentation_error_at(last_fill.max(1) - 1, "unexpected indent")
                } else if last_token.ty == TokenType::DEDENT {
                    *self.indentation_error_at(last_fill.max(1) - 1, "unexpected unindent")
                } else {
                    let e = self.error_at(ErrorKind::Syntax, "invalid syntax", last_token.lineno as i32, last_token.col, last_token.end_lineno as i32, last_token.end_col);
                    return Err(self.check_rest_for_errors(*e, last_fill));
                }
            }
        };
        Err(err)
    }

    fn indentation_error_at(&self, i: usize, msg: &str) -> PErr {
        let (l, c) = self.after_tok_pos(i);
        self.error_at(ErrorKind::Indentation, msg, l, c - 1, l, -1)
    }

    /// `_PyPegen_tokenize_full_source_to_check_for_errors`: a tokenizer error later in the
    /// source replaces a parser error (unless the error is inside an f-string).
    fn check_rest_for_errors(&self, e: SyntaxError, last_fill: usize) -> SyntaxError {
        let Some(le) = &self.lex_err else { return e };
        // f-string context at the error token: keep the parser error
        if self.fstring_open_at(last_fill.max(1) - 1) {
            return e;
        }
        match le {
            LexError::UnclosedParen(_, ln, _) => {
                if e.lineno > *ln as i32 {
                    lex_error_to_syntax(self.src, le.clone())
                } else {
                    e
                }
            }
            LexError::Eof { .. } => e,
            LexError::Syntax(_) => lex_error_to_syntax(self.src, le.clone()),
        }
    }

    fn fstring_open_at(&self, idx: usize) -> bool {
        let mut d: i32 = 0;
        for t in &self.toks[..=idx.min(self.toks.len() - 1)] {
            match t.ty {
                TokenType::FSTRING_START | TokenType::TSTRING_START => d += 1,
                TokenType::FSTRING_END | TokenType::TSTRING_END => d -= 1,
                _ => {}
            }
        }
        d > 0
    }

    fn is_end_of_source(&self) -> bool {
        // the parser reached the end of the input (ENDMARKER or EOF error token)
        self.lex_eof && self.fill > self.eof_start
    }

    fn start_rule(&mut self) -> R<Mod> {
        match self.opts.mode {
            Mode::Exec => self.file_rule(),
            Mode::Eval => self.eval_rule(),
            Mode::Single => self.interactive_rule(),
            Mode::FuncType => self.func_type_rule(),
        }
    }

    fn file_rule(&mut self) -> R<Mod> {
        // statements+ ENDMARKER, written out so that the second pass can skip the invalid_*
        // rules for top-level statements that already parsed in the first pass and end
        // before the first-pass failure point (a big speedup for errors late in large files).
        let mut body = Vec::new();
        loop {
            if self.at(TokenType::ENDMARKER) {
                break;
            }
            let m = self.p;
            let fast = self.invalid && self.top_ok.get(&m).map(|&e| e < self.first_fail).unwrap_or(false);
            let save = self.invalid;
            if fast {
                self.invalid = false;
            }
            let r = self.statement_into(&mut body);
            self.invalid = save;
            if r? {
                if !self.invalid {
                    self.top_ok.insert(m, self.p);
                }
            } else {
                self.p = m;
                break;
            }
        }
        if !self.at(TokenType::ENDMARKER) {
            return Ok(None);
        }
        let type_ignores = std::mem::take(&mut self.type_ignores);
        Ok(Some(Mod::Module { body, type_ignores }))
    }


    fn eval_rule(&mut self) -> R<Mod> {
        let Some(e) = self.expressions()? else { return Ok(None) };
        while self.eat(TokenType::NEWLINE).is_some() {}
        if !self.at(TokenType::ENDMARKER) {
            return Ok(None);
        }
        Ok(Some(Mod::Expression { body: Box::new(e) }))
    }

    fn interactive_rule(&mut self) -> R<Mod> {
        // statement_newline: compound_stmt NEWLINE | simple_stmts | NEWLINE | ENDMARKER
        let t = self.peek();
        if t.ty == TokenType::NEWLINE {
            self.advance();
            return Ok(Some(Mod::Interactive { body: Vec::new() }));
        }
        if t.ty == TokenType::ENDMARKER {
            return Ok(Some(Mod::Interactive { body: Vec::new() }));
        }
        let mark = self.p;
        if let Some(s) = self.compound_stmt()? {
            // CPython's parser turns the ENDMARKER into the NEWLINE that ends the statement
            if self.eat(TokenType::NEWLINE).is_some() || self.at(TokenType::ENDMARKER) {
                return Ok(Some(Mod::Interactive { body: vec![s] }));
            }
            return Ok(None);
        }
        self.p = mark;
        if let Some(v) = self.simple_stmts()? {
            return Ok(Some(Mod::Interactive { body: v }));
        }
        Ok(None)
    }

    fn func_type_rule(&mut self) -> R<Mod> {
        // '(' [type_expressions] ')' '->' expression NEWLINE* ENDMARKER
        if self.eat(TokenType::LPAR).is_none() {
            return Ok(None);
        }
        let mut argtypes = Vec::new();
        if !self.at(TokenType::RPAR) {
            match self.type_expressions()? {
                Some(v) => argtypes = v,
                None => return Ok(None),
            }
        }
        if self.eat(TokenType::RPAR).is_none() || self.eat(TokenType::RARROW).is_none() {
            return Ok(None);
        }
        let Some(ret) = self.expression()? else { return Ok(None) };
        while self.eat(TokenType::NEWLINE).is_some() {}
        if !self.at(TokenType::ENDMARKER) {
            return Ok(None);
        }
        Ok(Some(Mod::FunctionType { argtypes, returns: Box::new(ret) }))
    }

    fn type_expressions(&mut self) -> R<Vec<Expr>> {
        let mut v = Vec::new();
        loop {
            if self.at(TokenType::STAR) || self.at(TokenType::DOUBLESTAR) {
                let star = self.peek().ty == TokenType::STAR;
                self.advance();
                let Some(e) = self.expression()? else { return Ok(None) };
                v.push(e);
                if star && self.at(TokenType::COMMA) && self.peek_n(1).ty == TokenType::DOUBLESTAR {
                    self.advance();
                    self.advance();
                    let Some(e) = self.expression()? else { return Ok(None) };
                    v.push(e);
                }
                return Ok(Some(v));
            }
            let Some(e) = self.expression()? else { return Ok(None) };
            v.push(e);
            if self.eat(TokenType::COMMA).is_none() {
                break;
            }
            if !(self.at(TokenType::STAR) || self.at(TokenType::DOUBLESTAR)) && self.at(TokenType::RPAR) {
                return Ok(None);
            }
        }
        Ok(Some(v))
    }
}

/// Build a SyntaxError like `_PyPegen_raise_error_known_location`: 0-based byte columns are
/// converted to 1-based character offsets (`offset`, exclusive `end_offset`).
pub fn make_error_cpython(src: &str, kind: ErrorKind, msg: String, lineno: i32, col: i32, end_lineno: i32, end_col: i32) -> SyntaxError {
    let line = if lineno >= 1 { tokenizer::line_text(src, lineno as u32) } else { String::new() };
    let offset = if col >= 0 { byte_to_char_1based(&line, col) } else { 0 };
    let (eln, ecol, eoff) = if end_lineno >= 1 && end_col >= 0 {
        let off = if end_lineno == lineno {
            byte_to_char_1based(&line, end_col)
        } else {
            byte_to_char_1based(&tokenizer::line_text(src, end_lineno as u32), end_col)
        };
        (Some(end_lineno), Some(end_col), Some(off))
    } else if end_lineno >= 1 {
        (Some(end_lineno), None, Some(0))
    } else {
        (None, None, None)
    };
    SyntaxError {
        kind,
        msg,
        lineno,
        col_offset: col,
        end_lineno: eln,
        end_col_offset: ecol,
        offset,
        end_offset: eoff,
        text: if lineno >= 1 { Some(line) } else { None },
    }
}

/// CPython's `_PyPegen_byte_offset_to_character_offset(line, col + 1)`.
pub(crate) fn byte_to_char_1based(line: &str, col: i32) -> i32 {
    let b = line.as_bytes();
    let k = ((col + 1).max(0) as usize).min(b.len() + 1);
    let n = String::from_utf8_lossy(&b[..k.min(b.len())]).chars().count();
    (n + if k > b.len() { 1 } else { 0 }) as i32
}

impl<'a> Parser<'a> {
    /// CHECK_VERSION: raise "<what> only supported in Python 3.<minor> and greater" when a
    /// lower `feature_version` was requested.
    #[cold]
    #[inline(never)]
    pub(crate) fn check_version(&self, minor: u32, what: &str) -> Result<(), PErr> {
        match self.opts.feature_version {
            Some(v) if v < minor => Err(self.err_last(ErrorKind::Syntax, format!("{} only supported in Python 3.{} and greater", what, minor))),
            _ => Ok(()),
        }
    }
}

impl<'a> Parser<'a> {
    /// CPython's `bad_single_statement`: anything but whitespace and comments after the
    /// last fetched token.
    fn bad_single_statement(&self) -> bool {
        let t = self.toks[self.fill.max(1) - 1];
        let b = self.src.as_bytes();
        let mut i = (t.end as usize).min(b.len());
        loop {
            while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | 0x0c | b'\r') {
                i += 1;
            }
            if i >= b.len() {
                return false;
            }
            if b[i] != b'#' {
                return true;
            }
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        }
    }
}
