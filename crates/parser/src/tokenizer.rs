//! The tokenizer.
//!
//! [`Lexer`] produces the token stream the parser consumes (CPython's "raw" C tokenizer
//! stream) or, with `extra_tokens`, the stream the stdlib `tokenize` module exposes (COMMENT
//! and NL tokens, real NEWLINE strings, INDENT/DEDENT positions). [`TokenIter`] wraps it into
//! `tokenize`-style tuples (type, string, start, end, line) with character columns.
//!
//! Input is a decoded source string (use [`decode_source`] for bytes).

use crate::error::{byte_col_to_char_offset, ErrorKind, SyntaxError, Warning, WarningCategory};
use crate::token::{classify_name, Kw, Token, TokenType};
use crate::UnicodeHooks;

pub const MAXINDENT: usize = 100;
pub const MAXLEVEL: usize = 200;
pub const MAX_EXPR_NESTING: usize = 150;

#[derive(Clone, Copy, Debug)]
pub struct LexOptions {
    /// Produce the token stream of the stdlib `tokenize` module.
    pub extra_tokens: bool,
    /// Emit TYPE_COMMENT / TYPE_IGNORE tokens.
    pub type_comments: bool,
    pub hooks: UnicodeHooks,
}

impl Default for LexOptions {
    fn default() -> Self {
        LexOptions { extra_tokens: false, type_comments: false, hooks: UnicodeHooks::default() }
    }
}

#[derive(Clone, Copy, Debug)]
struct Paren {
    ch: u8,
    lineno: u32,
    col: i32,
}

#[derive(Clone, Copy, Debug)]
struct FMode {
    quote: u8,
    quote_size: u8,
    raw: bool,
    tstring: bool,
    /// byte offset of the start of the FSTRING_START token
    start: usize,
    start_lineno: u32,
    start_line_start: usize,
}

#[derive(Clone, Copy, Debug)]
struct Field {
    /// index of the owning f-string mode
    mode: usize,
    /// length of the paren stack including this field's '{'
    depth: usize,
    in_spec: bool,
    lineno: u32,
}

/// Internal lexer error: either a syntax error or the "EOF inside parentheses" condition
/// (reported by the parser as "'(' was never closed").
#[derive(Clone, Debug)]
pub enum LexError {
    Syntax(Box<SyntaxError>),
    /// EOF reached with open brackets; payload: (bracket, lineno, byte col).
    UnclosedParen(u8, u32, i32),
    /// Unexpected EOF (e.g. after a line continuation).
    Eof { lineno: u32, col: i32 },
}

pub struct Lexer<'a> {
    pub src: &'a str,
    b: &'a [u8],
    pos: usize,
    line_start: usize,
    lineno: u32,
    indents: Vec<u32>,
    alt_indents: Vec<u32>,
    pending: i32,
    atbol: bool,
    blankline: bool,
    parens: Vec<Paren>,
    modes: Vec<FMode>,
    fields: Vec<Field>,
    opts: LexOptions,
    eof: bool,
    /// The source did not end with a newline; a virtual one exists at `src.len()`.
    implicit_newline: bool,
    /// Position of the first token of the current line (for DEDENT in extra mode).
    pub warnings: Vec<Warning>,
    /// Count of tokens emitted on the current logical line (to suppress NEWLINE after blank).
    line_has_tokens: bool,
    done: bool,
    /// set once an ENDMARKER has been produced
    finished: bool,
    /// Pending token to be returned after a DEDENT etc (unused) / one-token pushback
    pushback: Option<Token>,
    eof_reads: u32,
    empty_middle_done: bool,
}

#[inline]
fn is_id_start_ascii(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}
#[inline]
fn is_id_char_ascii(c: u8) -> bool {
    ID_CHAR[c as usize]
}
#[inline]
fn is_potential_identifier_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 128
}

impl<'a> Lexer<'a> {
    /// Create a lexer. In parser mode the source should already have newlines normalized
    /// (`\r\n` and `\r` -> `\n`); a missing final newline is handled implicitly.
    pub fn new(src: &'a str, opts: LexOptions) -> Lexer<'a> {
        let implicit_newline = !src.is_empty() && !src.ends_with('\n') && !src.ends_with('\r');
        Lexer {
            src,
            b: src.as_bytes(),
            pos: 0,
            line_start: 0,
            lineno: 1,
            indents: vec![0],
            alt_indents: vec![0],
            pending: 0,
            atbol: true,
            blankline: false,
            parens: Vec::new(),
            modes: Vec::new(),
            fields: Vec::new(),
            opts,
            eof: false,
            implicit_newline,
            warnings: Vec::new(),
            line_has_tokens: false,
            done: false,
            finished: false,
            pushback: None,
            eof_reads: 0,
            empty_middle_done: false,
        }
    }

    pub fn lineno(&self) -> u32 {
        self.lineno
    }
    pub fn paren_level(&self) -> usize {
        self.parens.len()
    }
    /// Innermost open bracket: (char, lineno, byte col).
    pub fn innermost_paren(&self) -> Option<(u8, u32, i32)> {
        self.parens.last().map(|p| (p.ch, p.lineno, p.col))
    }
    pub fn in_fstring(&self) -> bool {
        !self.modes.is_empty()
    }

    // ------------------------------------------------------------------ char access

    #[inline]
    fn peek(&self) -> i32 {
        if self.pos < self.b.len() {
            self.b[self.pos] as i32
        } else if self.pos == self.b.len() && self.implicit_newline {
            b'\n' as i32
        } else {
            -1
        }
    }
    #[inline]
    fn peek_at(&self, off: usize) -> i32 {
        let p = self.pos + off;
        if p < self.b.len() {
            self.b[p] as i32
        } else if p == self.b.len() && self.implicit_newline {
            b'\n' as i32
        } else {
            -1
        }
    }
    /// Consume one byte, tracking line numbers. Returns -1 at EOF.
    #[inline]
    fn next(&mut self) -> i32 {
        let c = self.peek();
        if c < 0 {
            return -1;
        }
        self.pos += 1;
        if c == b'\n' as i32 {
            self.lineno += 1;
            self.line_start = self.pos;
        } else if c == b'\r' as i32 && self.peek() != b'\n' as i32 {
            // lone CR acts as a newline (only possible in extra_tokens mode)
            self.lineno += 1;
            self.line_start = self.pos;
        }
        c
    }
    #[inline]
    fn src_len(&self) -> usize {
        if self.implicit_newline {
            self.b.len() + 1
        } else {
            self.b.len()
        }
    }

    #[inline]
    fn col_of(&self, pos: usize) -> i32 {
        (pos - self.line_start) as i32
    }

    /// Text of line `lineno` (1-based), including its newline if present.
    pub fn line_text(&self, lineno: u32) -> String {
        line_text(self.src, lineno)
    }

    fn mk(&self, ty: TokenType, start: usize, lineno: u32, col: i32) -> Token {
        let end = self.pos.min(self.b.len());
        Token {
            ty,
            kw: Kw::None,
            level: self.parens.len() as u16,
            start: start as u32,
            end: end as u32,
            lineno,
            col,
            end_lineno: self.lineno,
            end_col: self.col_of(self.pos),
        }
    }

    // ------------------------------------------------------------------ errors

    /// Error at an explicit location (byte columns, end may be -1).
    fn err_at(&self, kind: ErrorKind, msg: String, lineno: u32, col: i32, end_lineno: u32, end_col: i32) -> LexError {
        LexError::Syntax(Box::new(make_error(self.src, kind, msg, lineno as i32, col, end_lineno as i32, end_col)))
    }

    /// Error pointing at the last consumed character (like CPython's tokenizer errors).
    fn err_here(&self, msg: impl Into<String>) -> LexError {
        let col = self.col_of(self.pos);
        let mut e = make_error(self.src, ErrorKind::Syntax, msg.into(), self.lineno as i32, col - 1, self.lineno as i32, col);
        // CPython's tokenizer errors use the text up to the end of the line and
        // offset = chars up to the current position.
        let line = line_text(self.src, self.lineno);
        let lt = line.trim_end_matches(['\n', '\r']).to_string();
        e.offset = byte_col_to_char_offset(&line, col) - 1;
        if e.offset < 1 {
            e.offset = 1;
        }
        e.end_offset = Some(e.offset);
        e.text = Some(lt);
        LexError::Syntax(Box::new(e))
    }

    fn indent_error(&self, kind: ErrorKind, msg: &str) -> LexError {
        // Points past the end of the current line (as CPython does).
        let line = line_text(self.src, self.lineno);
        let lt = line.trim_end_matches(['\n', '\r']);
        let col = lt.len() as i32;
        let mut e = make_error(self.src, kind, msg.to_string(), self.lineno as i32, col, self.lineno as i32, col);
        e.text = Some(line.clone());
        e.offset = byte_col_to_char_offset(&line, col);
        e.end_offset = Some(e.offset);
        LexError::Syntax(Box::new(e))
    }

    fn warn(&mut self, msg: String, lineno: u32, col: i32, end_lineno: u32, end_col: i32) {
        let line = line_text(self.src, lineno);
        let eline = line_text(self.src, end_lineno);
        self.warnings.push(Warning {
            category: WarningCategory::SyntaxWarning,
            error_msg: msg.clone(),
            msg,
            lineno: lineno as i32,
            col_offset: col,
            end_lineno: end_lineno as i32,
            end_col_offset: end_col,
            offset: byte_col_to_char_offset(&line, col),
            end_offset: byte_col_to_char_offset(&eline, end_col),
        });
    }

    fn fprefix(&self) -> char {
        match self.modes.last() {
            Some(m) if m.tstring => 't',
            _ => 'f',
        }
    }

    // ------------------------------------------------------------------ main entry

    /// Produce the next token. After ENDMARKER, keeps returning ENDMARKER.
    pub fn next_token(&mut self) -> Result<Token, LexError> {
        if let Some(t) = self.pushback.take() {
            return Ok(t);
        }
        if self.finished {
            let p = self.src_len().min(self.b.len());
            return Ok(Token { ty: TokenType::ENDMARKER, kw: Kw::None, level: self.parens.len() as u16, start: p as u32, end: p as u32, lineno: self.lineno, col: 0, end_lineno: self.lineno, end_col: 0 });
        }
        if let Some(m) = self.modes.last() {
            let midx = self.modes.len() - 1;
            let _ = m;
            match self.fields.last() {
                Some(f) if f.mode == midx && !f.in_spec => {}
                _ => {
                    if let Some(t) = self.lex_fstring_middle()? {
                        return Ok(t);
                    }
                }
            }
        }
        self.lex_normal()
    }

    // ------------------------------------------------------------------ normal mode

    fn lex_normal(&mut self) -> Result<Token, LexError> {
        'nextline: loop {
            // (blankline persists until the end of the line: COMMENT may be returned first)
            if self.atbol {
                self.atbol = false;
                self.line_has_tokens = false;
                let mut col: u32 = 0;
                let mut altcol: u32 = 0;
                let mut cont_line_col: u32 = 0;
                loop {
                    let c = self.peek();
                    if c == b' ' as i32 {
                        self.pos += 1;
                        col += 1;
                        altcol += 1;
                    } else if c == b'\t' as i32 {
                        self.pos += 1;
                        col = (col / 8 + 1) * 8;
                        altcol += 1;
                    } else if c == 0x0c {
                        self.pos += 1;
                        col = 0;
                        altcol = 0;
                    } else if c == b'\\' as i32 {
                        cont_line_col = if cont_line_col != 0 { cont_line_col } else { col };
                        self.pos += 1;
                        self.continuation_line()?;
                    } else {
                        break;
                    }
                }
                let c = self.peek();
                self.blankline = c == b'#' as i32 || c == b'\n' as i32 || c == b'\r' as i32 || c < 0;
                if self.blankline {
                    self.blankline = true;
                }
                if !self.blankline && self.parens.is_empty() {
                    if cont_line_col != 0 {
                        col = cont_line_col;
                        altcol = cont_line_col;
                    }
                    let top = *self.indents.last().unwrap();
                    let atop = *self.alt_indents.last().unwrap();
                    if col == top {
                        if altcol != atop {
                            return Err(self.indent_error(ErrorKind::Tab, "inconsistent use of tabs and spaces in indentation"));
                        }
                    } else if col > top {
                        if self.indents.len() >= MAXINDENT {
                            return Err(self.indent_error(ErrorKind::Indentation, "too many levels of indentation"));
                        }
                        if altcol <= atop {
                            return Err(self.indent_error(ErrorKind::Tab, "inconsistent use of tabs and spaces in indentation"));
                        }
                        self.pending += 1;
                        self.indents.push(col);
                        self.alt_indents.push(altcol);
                    } else {
                        while self.indents.len() > 1 && col < *self.indents.last().unwrap() {
                            self.pending -= 1;
                            self.indents.pop();
                            self.alt_indents.pop();
                        }
                        if col != *self.indents.last().unwrap() {
                            return Err(self.indent_error(ErrorKind::Indentation, "unindent does not match any outer indentation level"));
                        }
                        if altcol != *self.alt_indents.last().unwrap() {
                            return Err(self.indent_error(ErrorKind::Tab, "inconsistent use of tabs and spaces in indentation"));
                        }
                    }
                }
            }

            // pending indents / dedents
            if self.pending != 0 {
                let p = self.pos;
                let col = self.col_of(p);
                if self.pending < 0 {
                    self.pending += 1;
                    if self.opts.extra_tokens {
                        return Ok(Token { ty: TokenType::DEDENT, kw: Kw::None, level: self.parens.len() as u16, start: p as u32, end: p as u32, lineno: self.lineno, col, end_lineno: self.lineno, end_col: col });
                    }
                    return Ok(Token { ty: TokenType::DEDENT, kw: Kw::None, level: self.parens.len() as u16, start: p as u32, end: p as u32, lineno: self.lineno, col: -1, end_lineno: self.lineno, end_col: -1 });
                } else {
                    self.pending -= 1;
                    if self.opts.extra_tokens {
                        let ls = self.line_start;
                        return Ok(Token { ty: TokenType::INDENT, kw: Kw::None, level: self.parens.len() as u16, start: ls as u32, end: p as u32, lineno: self.lineno, col: 0, end_lineno: self.lineno, end_col: col });
                    }
                    return Ok(Token { ty: TokenType::INDENT, kw: Kw::None, level: self.parens.len() as u16, start: p as u32, end: p as u32, lineno: self.lineno, col: -1, end_lineno: self.lineno, end_col: -1 });
                }
            }

            'again: loop {
                // skip spaces
                loop {
                    let c = self.peek();
                    if c == b' ' as i32 || c == b'\t' as i32 || c == 0x0c {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                let start = self.pos;
                let slineno = self.lineno;
                let scol = self.col_of(start);
                let c = self.peek();

                // comment
                if c == b'#' as i32 {
                    while self.pos < self.b.len() && self.b[self.pos] != b'\n' && self.b[self.pos] != b'\r' {
                        self.pos += 1;
                    }
                    if self.opts.type_comments {
                        if let Some(t) = self.type_comment(start, slineno)? {
                            return Ok(t);
                        }
                    }
                    if self.opts.extra_tokens {
                        return Ok(self.mk(TokenType::COMMENT, start, slineno, scol));
                    }
                    continue 'again;
                }

                if c < 0 {
                    // EOF
                    if !self.parens.is_empty() {
                        let p = *self.parens.last().unwrap();
                        if self.opts.extra_tokens {
                            // tokenize module: no error for unclosed parens at EOF here
                        }
                        return Err(LexError::UnclosedParen(p.ch, p.lineno, p.col));
                    }
                    if !self.modes.is_empty() {
                        // EOF inside an f-string replacement field: handled as unclosed paren
                    }
                    if self.indents.len() > 1 {
                        // emit dedents
                        self.indents.pop();
                        self.alt_indents.pop();
                        let p = self.b.len();
                        let (ln, col) = if self.opts.extra_tokens { (self.lineno, 0) } else { (self.last_line_no(), -1) };
                        return Ok(Token { ty: TokenType::DEDENT, kw: Kw::None, level: self.parens.len() as u16, start: p as u32, end: p as u32, lineno: ln, col, end_lineno: ln, end_col: col });
                    }
                    self.finished = true;
                    let p = self.b.len();
                    let ln = self.lineno;
                    return Ok(Token { ty: TokenType::ENDMARKER, kw: Kw::None, level: self.parens.len() as u16, start: p as u32, end: p as u32, lineno: ln, col: 0, end_lineno: ln, end_col: 0 });
                }

                // newline
                if c == b'\n' as i32 || c == b'\r' as i32 {
                    let nl_start = self.pos;
                    let at_implicit = self.pos >= self.b.len();
                    if c == b'\r' as i32 && self.peek_at(1) == b'\n' as i32 {
                        self.pos += 1;
                    }
                    self.next();
                    self.atbol = true;
                    if self.blankline || !self.parens.is_empty() {
                        if self.opts.extra_tokens {
                            return Ok(self.newline_token(TokenType::NL, nl_start, slineno, scol, at_implicit));
                        }
                        continue 'nextline;
                    }
                    if !self.line_has_tokens && !self.opts.extra_tokens {
                        // can happen after a type-ignore on its own line etc.
                        continue 'nextline;
                    }
                    self.line_has_tokens = false;
                    return Ok(self.newline_token(TokenType::NEWLINE, nl_start, slineno, scol, at_implicit));
                }

                // line continuation
                if c == b'\\' as i32 {
                    self.pos += 1;
                    self.continuation_line()?;
                    continue 'again;
                }

                self.line_has_tokens = true;
                let cu = c as u8;

                // identifiers and string prefixes
                if is_id_start_ascii(cu) || cu >= 128 {
                    return self.lex_name_or_string(start, slineno, scol);
                }
                // numbers
                if cu.is_ascii_digit() {
                    return self.lex_number(start, slineno, scol);
                }
                if cu == b'.' {
                    let c2 = self.peek_at(1);
                    if c2 >= 0 && (c2 as u8).is_ascii_digit() {
                        return self.lex_number(start, slineno, scol);
                    }
                    if c2 == b'.' as i32 && self.peek_at(2) == b'.' as i32 {
                        self.pos += 3;
                        return Ok(self.mk(TokenType::ELLIPSIS, start, slineno, scol));
                    }
                    self.pos += 1;
                    return Ok(self.mk(TokenType::DOT, start, slineno, scol));
                }
                // strings
                if cu == b'\'' || cu == b'"' {
                    return self.lex_string(start, slineno, scol, start, false, false, false);
                }
                return self.lex_operator(start, slineno, scol);
            }
        }
    }

    fn last_line_no(&self) -> u32 {
        // number of the last line of the source (for DEDENT at EOF in raw mode)
        if self.b.is_empty() {
            return 1;
        }
        if self.lineno > 1 {
            self.lineno - 1
        } else {
            1
        }
    }

    fn newline_token(&self, ty: TokenType, nl_start: usize, slineno: u32, scol: i32, implicit: bool) -> Token {
        if self.opts.extra_tokens {
            let end = self.pos.min(self.b.len());
            Token {
                ty,
                kw: Kw::None,
                level: self.parens.len() as u16,
                start: nl_start as u32,
                end: if implicit { nl_start as u32 } else { end as u32 },
                lineno: slineno,
                col: scol,
                end_lineno: slineno,
                end_col: scol + if implicit { 1 } else { (end - nl_start) as i32 },
            }
        } else {
            Token { ty, kw: Kw::None, level: self.parens.len() as u16, start: nl_start as u32, end: nl_start as u32, lineno: slineno, col: scol, end_lineno: slineno, end_col: scol }
        }
    }

    /// Called after consuming a backslash outside of a string.
    fn continuation_line(&mut self) -> Result<(), LexError> {
        let mut c = self.peek();
        if c == b'\r' as i32 {
            self.pos += 1;
            c = self.peek();
        }
        if c != b'\n' as i32 {
            if c >= 0 {
                self.next();
            }
            return Err(self.err_here("unexpected character after line continuation character"));
        }
        self.next();
        if self.peek() < 0 {
            // EOF after continuation
            if let Some(p) = self.parens.last() {
                return Err(LexError::UnclosedParen(p.ch, p.lineno, p.col));
            }
            return Err(LexError::Eof { lineno: self.lineno - 1, col: (self.pos - line_start_of(self.src, self.lineno - 1)) as i32 });
        }
        Ok(())
    }

    fn type_comment(&mut self, start: usize, slineno: u32) -> Result<Option<Token>, LexError> {
        let b = self.b;
        let end = self.pos;
        let mut p = start;
        let prefix = b"# type: ";
        let mut i = 0;
        while i < prefix.len() && p < end {
            if prefix[i] == b' ' {
                while p < end && (b[p] == b' ' || b[p] == b'\t') {
                    p += 1;
                }
            } else if prefix[i] == b[p] {
                p += 1;
            } else {
                break;
            }
            i += 1;
        }
        // trailing ' ' in prefix may match zero chars at end
        while i < prefix.len() && prefix[i] == b' ' {
            i += 1;
        }
        if i < prefix.len() {
            return Ok(None);
        }
        let ignore_end = p + 6;
        let is_ignore = end >= ignore_end
            && &b[p..ignore_end] == b"ignore"
            && !(end > ignore_end && (b[ignore_end] >= 128 || b[ignore_end].is_ascii_alphanumeric()));
        if is_ignore {
            let t = Token {
                ty: TokenType::TYPE_IGNORE,
                kw: Kw::None,
                level: 0,
                start: ignore_end as u32,
                end: end as u32,
                lineno: slineno,
                col: self.col_of(ignore_end),
                end_lineno: slineno,
                end_col: self.col_of(end),
            };
            if self.blankline {
                // consume the newline too
                if self.peek() == b'\r' as i32 && self.peek_at(1) == b'\n' as i32 {
                    self.pos += 1;
                }
                if self.peek() >= 0 {
                    self.next();
                }
                self.atbol = true;
            }
            return Ok(Some(t));
        }
        if self.blankline {
            // A type comment on its own line is ignored by the parser.
            return Ok(None);
        }
        Ok(Some(Token { ty: TokenType::TYPE_COMMENT, kw: Kw::None, level: self.parens.len() as u16, start: p as u32, end: end as u32, lineno: slineno, col: self.col_of(p), end_lineno: slineno, end_col: self.col_of(end) }))
    }

    // ------------------------------------------------------------------ names

    fn lex_name_or_string(&mut self, start: usize, slineno: u32, scol: i32) -> Result<Token, LexError> {
        // string prefix?
        let (mut saw_b, mut saw_r, mut saw_u, mut saw_f, mut saw_t) = (false, false, false, false, false);
        let mut dup = false;
        let mut p = self.pos;
        loop {
            let c = if p < self.b.len() { self.b[p] } else { 0 };
            let lc = c.to_ascii_lowercase();
            let flag = match lc {
                b'b' => &mut saw_b,
                b'r' => &mut saw_r,
                b'u' => &mut saw_u,
                b'f' => &mut saw_f,
                b't' => &mut saw_t,
                _ => break,
            };
            if *flag {
                dup = true;
            }
            *flag = true;
            p += 1;
        }
        if p > self.pos && p < self.b.len() && (self.b[p] == b'\'' || self.b[p] == b'"') && !dup {
            // incompatible combos
            let pairs: [(bool, bool, char, char); 7] = [
                (saw_u, saw_b, 'u', 'b'),
                (saw_u, saw_r, 'u', 'r'),
                (saw_u, saw_f, 'u', 'f'),
                (saw_u, saw_t, 'u', 't'),
                (saw_b, saw_f, 'b', 'f'),
                (saw_b, saw_t, 'b', 't'),
                (saw_f, saw_t, 'f', 't'),
            ];
            for (x, y, a, bb) in pairs {
                if x && y {
                    let msg = format!("'{}' and '{}' prefixes are incompatible", a, bb);
                    return Err(self.err_at(ErrorKind::Syntax, msg, slineno, scol, slineno, self.col_of(p)));
                }
            }
            let prefix_end = p;
            self.pos = p;
            if saw_f || saw_t {
                return self.lex_fstring_start(start, slineno, scol, saw_r, saw_t);
            }
            return self.lex_string(start, slineno, scol, prefix_end, saw_b, saw_r, saw_u);
        }
        // identifier
        let mut nonascii = false;
        while self.pos < self.b.len() {
            let c = self.b[self.pos];
            if is_id_char_ascii(c) {
                self.pos += 1;
            } else if c >= 128 {
                nonascii = true;
                self.pos += 1;
            } else {
                break;
            }
        }
        if nonascii {
            self.verify_identifier(start)?;
        }
        let mut t = self.mk(TokenType::NAME, start, slineno, scol);
        if !nonascii {
            t.kw = classify_name(&self.b[start..self.pos]);
        }
        Ok(t)
    }

    fn verify_identifier(&mut self, start: usize) -> Result<(), LexError> {
        let s = &self.src[start..self.pos];
        let hooks = self.opts.hooks;
        let mut first = true;
        for (i, ch) in s.char_indices() {
            let ok = if first {
                ch == '_' || ch.is_ascii_alphabetic() || (!ch.is_ascii() && (hooks.is_xid_start)(ch))
            } else {
                ch == '_' || ch.is_ascii_alphanumeric() || (!ch.is_ascii() && (hooks.is_xid_continue)(ch))
            };
            if !ok {
                let at = start + i;
                self.pos = at + ch.len_utf8();
                let msg = if (hooks.is_printable)(ch) {
                    format!("invalid character '{}' (U+{:04X})", ch, ch as u32)
                } else {
                    format!("invalid non-printable character U+{:04X}", ch as u32)
                };
                return Err(self.err_here(msg));
            }
            first = false;
        }
        Ok(())
    }

    // ------------------------------------------------------------------ numbers
    // These mirror CPython's tokenizer exactly (including where the cursor is when an error
    // is raised, which determines the reported offset).

    #[inline]
    fn nextc(&mut self) -> i32 {
        let c = self.peek();
        if c >= 0 {
            self.pos += 1;
        } else {
            // EOF: CPython's tok_nextc returns EOF without advancing; mark with a sentinel
            self.eof_reads += 1;
        }
        c
    }
    #[inline]
    fn backup(&mut self, c: i32) {
        if c >= 0 {
            self.pos -= 1;
        } else if self.eof_reads > 0 {
            self.eof_reads -= 1;
        }
    }

    fn lex_number(&mut self, start: usize, slineno: u32, scol: i32) -> Result<Token, LexError> {
        let isdigit = |c: i32| c >= b'0' as i32 && c <= b'9' as i32;
        let isxdigit = |c: i32| c >= 0 && (c as u8).is_ascii_hexdigit();
        let mut c = self.nextc();
        if c == b'.' as i32 {
            // ".5" form: we're at a digit after the dot
            c = self.nextc();
            return self.number_fraction(c, start, slineno, scol);
        }
        if c == b'0' as i32 {
            c = self.nextc();
            if c == b'x' as i32 || c == b'X' as i32 {
                c = self.nextc();
                loop {
                    if c == b'_' as i32 {
                        c = self.nextc();
                    }
                    if !isxdigit(c) {
                        self.backup(c);
                        return Err(self.err_here("invalid hexadecimal literal"));
                    }
                    loop {
                        c = self.nextc();
                        if !isxdigit(c) {
                            break;
                        }
                    }
                    if c != b'_' as i32 {
                        break;
                    }
                }
                self.verify_end_of_number(c, "hexadecimal")?;
            } else if c == b'o' as i32 || c == b'O' as i32 {
                c = self.nextc();
                loop {
                    if c == b'_' as i32 {
                        c = self.nextc();
                    }
                    if c < b'0' as i32 || c >= b'8' as i32 {
                        if isdigit(c) {
                            return Err(self.err_here(format!("invalid digit '{}' in octal literal", c as u8 as char)));
                        }
                        self.backup(c);
                        return Err(self.err_here("invalid octal literal"));
                    }
                    loop {
                        c = self.nextc();
                        if !(c >= b'0' as i32 && c < b'8' as i32) {
                            break;
                        }
                    }
                    if c != b'_' as i32 {
                        break;
                    }
                }
                if isdigit(c) {
                    return Err(self.err_here(format!("invalid digit '{}' in octal literal", c as u8 as char)));
                }
                self.verify_end_of_number(c, "octal")?;
            } else if c == b'b' as i32 || c == b'B' as i32 {
                c = self.nextc();
                loop {
                    if c == b'_' as i32 {
                        c = self.nextc();
                    }
                    if c != b'0' as i32 && c != b'1' as i32 {
                        if isdigit(c) {
                            return Err(self.err_here(format!("invalid digit '{}' in binary literal", c as u8 as char)));
                        }
                        self.backup(c);
                        return Err(self.err_here("invalid binary literal"));
                    }
                    loop {
                        c = self.nextc();
                        if c != b'0' as i32 && c != b'1' as i32 {
                            break;
                        }
                    }
                    if c != b'_' as i32 {
                        break;
                    }
                }
                if isdigit(c) {
                    return Err(self.err_here(format!("invalid digit '{}' in binary literal", c as u8 as char)));
                }
                self.verify_end_of_number(c, "binary")?;
            } else {
                let mut nonzero = false;
                loop {
                    if c == b'_' as i32 {
                        c = self.nextc();
                        if !isdigit(c) {
                            self.backup(c);
                            return Err(self.err_here("invalid decimal literal"));
                        }
                    }
                    if c != b'0' as i32 {
                        break;
                    }
                    c = self.nextc();
                }
                let zeros_end = self.pos;
                if isdigit(c) {
                    nonzero = true;
                    c = self.decimal_tail()?;
                }
                if c == b'.' as i32 {
                    c = self.nextc();
                    return self.number_fraction(c, start, slineno, scol);
                } else if c == b'e' as i32 || c == b'E' as i32 {
                    return self.number_exponent(c, start, slineno, scol);
                } else if c == b'j' as i32 || c == b'J' as i32 {
                    return self.number_imaginary(start, slineno, scol);
                } else if nonzero && !self.opts.extra_tokens {
                    self.backup(c);
                    let col = self.col_of(start);
                    // zeros_end is one past the last zero... CPython: end = zeros_end - line_start
                    let ecol = (zeros_end - self.line_start) as i32 - 1;
                    return Err(self.err_at(
                        ErrorKind::Syntax,
                        "leading zeros in decimal integer literals are not permitted; use an 0o prefix for octal integers".into(),
                        slineno,
                        col,
                        slineno,
                        ecol,
                    ));
                }
                self.verify_end_of_number(c, "decimal")?;
            }
        } else {
            // decimal
            c = self.decimal_tail()?;
            if c == b'.' as i32 {
                c = self.nextc();
                return self.number_fraction(c, start, slineno, scol);
            }
            if c == b'e' as i32 || c == b'E' as i32 {
                return self.number_exponent(c, start, slineno, scol);
            }
            if c == b'j' as i32 || c == b'J' as i32 {
                return self.number_imaginary(start, slineno, scol);
            }
            self.verify_end_of_number(c, "decimal")?;
        }
        self.backup(c);
        Ok(self.mk(TokenType::NUMBER, start, slineno, scol))
    }

    /// `c` is the char after the '.'.
    fn number_fraction(&mut self, mut c: i32, start: usize, slineno: u32, scol: i32) -> Result<Token, LexError> {
        if c >= b'0' as i32 && c <= b'9' as i32 {
            c = self.decimal_tail()?;
        }
        if c == b'e' as i32 || c == b'E' as i32 {
            return self.number_exponent(c, start, slineno, scol);
        }
        if c == b'j' as i32 || c == b'J' as i32 {
            return self.number_imaginary(start, slineno, scol);
        }
        self.verify_end_of_number(c, "decimal")?;
        self.backup(c);
        Ok(self.mk(TokenType::NUMBER, start, slineno, scol))
    }

    /// `e` ('e'/'E') has been consumed.
    fn number_exponent(&mut self, e: i32, start: usize, slineno: u32, scol: i32) -> Result<Token, LexError> {
        let mut c = self.nextc();
        if c == b'+' as i32 || c == b'-' as i32 {
            c = self.nextc();
            if !(c >= b'0' as i32 && c <= b'9' as i32) {
                self.backup(c);
                return Err(self.err_here("invalid decimal literal"));
            }
        } else if !(c >= b'0' as i32 && c <= b'9' as i32) {
            self.backup(c);
            self.verify_end_of_number(e, "decimal")?;
            self.backup(e);
            return Ok(self.mk(TokenType::NUMBER, start, slineno, scol));
        }
        c = self.decimal_tail()?;
        if c == b'j' as i32 || c == b'J' as i32 {
            return self.number_imaginary(start, slineno, scol);
        }
        self.verify_end_of_number(c, "decimal")?;
        self.backup(c);
        Ok(self.mk(TokenType::NUMBER, start, slineno, scol))
    }

    /// 'j' has been consumed.
    fn number_imaginary(&mut self, start: usize, slineno: u32, scol: i32) -> Result<Token, LexError> {
        let c = self.nextc();
        self.verify_end_of_number(c, "imaginary")?;
        self.backup(c);
        Ok(self.mk(TokenType::NUMBER, start, slineno, scol))
    }

    /// Returns the first char after the digits (consumed).
    fn decimal_tail(&mut self) -> Result<i32, LexError> {
        loop {
            let mut c;
            loop {
                c = self.nextc();
                if !(c >= b'0' as i32 && c <= b'9' as i32) {
                    break;
                }
            }
            if c != b'_' as i32 {
                return Ok(c);
            }
            c = self.nextc();
            if !(c >= b'0' as i32 && c <= b'9' as i32) {
                self.backup(c);
                return Err(self.err_here("invalid decimal literal"));
            }
        }
    }

    /// Check the chars after the current one (which is `c`, already consumed).
    fn lookahead(&self, s: &[u8]) -> bool {
        for (i, &ch) in s.iter().enumerate() {
            if self.peek_at(i) != ch as i32 {
                return false;
            }
        }
        true
    }

    /// `c` is the char following the number (consumed). On success the cursor is unchanged.
    fn verify_end_of_number(&mut self, c: i32, kind: &str) -> Result<(), LexError> {
        if self.opts.extra_tokens || c < 0 {
            return Ok(());
        }
        let cu = c as u8;
        let r = match cu {
            b'a' => self.lookahead(b"nd"),
            b'e' => self.lookahead(b"lse"),
            b'f' => self.lookahead(b"or"),
            b'i' => {
                let c2 = self.peek_at(0);
                c2 == b'f' as i32 || c2 == b'n' as i32 || c2 == b's' as i32
            }
            b'o' => self.lookahead(b"r"),
            b'n' => self.lookahead(b"ot"),
            _ => false,
        };
        if r {
            self.backup(c);
            let col = self.col_of(self.pos);
            let ln = self.lineno;
            self.warn(format!("invalid {} literal", kind), ln, col - 1, ln, col);
            self.nextc();
            return Ok(());
        }
        if cu < 128 && is_potential_identifier_char(cu) {
            self.backup(c);
            return Err(self.err_here(format!("invalid {} literal", kind)));
        }
        Ok(())
    }

    // ------------------------------------------------------------------ strings

    /// Lex a (non-f) string literal; `self.pos` is at the opening quote.
    #[allow(clippy::too_many_arguments)]
    fn lex_string(&mut self, start: usize, slineno: u32, scol: i32, _prefix_end: usize, _is_b: bool, is_raw: bool, _is_u: bool) -> Result<Token, LexError> {
        let quote = self.b[self.pos];
        let first_line_start = self.line_start;
        self.pos += 1;
        let mut quote_size = 1;
        if self.peek() == quote as i32 && self.peek_at(1) == quote as i32 {
            self.pos += 2;
            quote_size = 3;
        } else if self.peek() == quote as i32 {
            // empty string
            self.pos += 1;
            let t = self.mk(TokenType::STRING, start, slineno, scol);
            return Ok(t);
        }
        let _ = is_raw;
        let mut end_quote_size = 0;
        let mut has_escaped_quote = false;
        while end_quote_size != quote_size {
            // fast path over ordinary characters
            {
                let b = self.b;
                let mut i = self.pos;
                while i < b.len() {
                    let ch = b[i];
                    if ch == quote || ch == b'\\' || ch == b'\n' || ch == b'\r' {
                        break;
                    }
                    i += 1;
                }
                if i != self.pos {
                    self.pos = i;
                    end_quote_size = 0;
                }
            }
            let c = self.peek();
            if c < 0 || (quote_size == 1 && (c == b'\n' as i32 || c == b'\r' as i32)) {
                let detected = if c < 0 { self.last_line_no() } else { self.lineno };
                // report at the string start
                if !self.modes.is_empty() {
                    let m = *self.modes.last().unwrap();
                    if m.quote == quote && m.quote_size == quote_size {
                        let fl = self.fields.last().map(|f| f.lineno).unwrap_or(m.start_lineno);
                        let msg = format!("{}-string: expecting '}}' to close '{{' on line {}", self.fprefix(), fl);
                        return Err(self.string_error(msg, slineno, first_line_start, start));
                    }
                }
                let msg = if quote_size == 3 {
                    format!("unterminated triple-quoted string literal (detected at line {})", detected)
                } else if has_escaped_quote {
                    format!("unterminated string literal (detected at line {}); perhaps you escaped the end quote?", detected)
                } else {
                    format!("unterminated string literal (detected at line {})", detected)
                };
                return Err(self.string_error(msg, slineno, first_line_start, start));
            }
            self.next();
            if c == quote as i32 {
                end_quote_size += 1;
            } else {
                end_quote_size = 0;
                if c == b'\\' as i32 {
                    let c2 = self.peek();
                    if c2 >= 0 {
                        self.next();
                        if c2 == quote as i32 {
                            has_escaped_quote = true;
                        }
                        if c2 == b'\r' as i32 && self.peek() == b'\n' as i32 {
                            self.next();
                        }
                    }
                }
            }
        }
        Ok(self.mk(TokenType::STRING, start, slineno, scol))
    }

    /// Error for unterminated strings: located at the string start (offset = start col + 1),
    /// text = the first line of the string without newline.
    fn string_error(&self, msg: String, slineno: u32, line_start: usize, start: usize) -> LexError {
        let line = line_text(self.src, slineno);
        let col = (start - line_start) as i32;
        let lt = line.trim_end_matches(['\n', '\r']).to_string();
        let off = byte_col_to_char_offset(&line, col);
        LexError::Syntax(Box::new(SyntaxError {
            kind: ErrorKind::Syntax,
            msg,
            lineno: slineno as i32,
            col_offset: col,
            end_lineno: Some(slineno as i32),
            end_col_offset: Some(col),
            offset: off,
            end_offset: Some(off),
            text: Some(lt),
        }))
    }

    fn lex_fstring_start(&mut self, start: usize, slineno: u32, scol: i32, raw: bool, tstring: bool) -> Result<Token, LexError> {
        let quote = self.b[self.pos];
        self.pos += 1;
        let mut quote_size = 1u8;
        if self.peek() == quote as i32 && self.peek_at(1) == quote as i32 {
            self.pos += 2;
            quote_size = 3;
        }
        if self.modes.len() >= MAX_EXPR_NESTING {
            return Err(self.err_here(format!("too many nested {}-strings", if tstring { 't' } else { 'f' })));
        }
        self.modes.push(FMode { quote, quote_size, raw, tstring, start, start_lineno: slineno, start_line_start: self.line_start });
        let ty = if tstring { TokenType::TSTRING_START } else { TokenType::FSTRING_START };
        Ok(self.mk(ty, start, slineno, scol))
    }

    /// Lex in f-string literal mode. Returns None if nothing was produced and the caller should
    /// fall back to normal mode (never happens currently; kept for structure).
    fn lex_fstring_middle(&mut self) -> Result<Option<Token>, LexError> {
        let midx = self.modes.len() - 1;
        let m = self.modes[midx];
        let in_spec = matches!(self.fields.last(), Some(f) if f.mode == midx && f.in_spec);
        let has_field = matches!(self.fields.last(), Some(f) if f.mode == midx);
        let (mid_ty, end_ty) = if m.tstring { (TokenType::TSTRING_MIDDLE, TokenType::TSTRING_END) } else { (TokenType::FSTRING_MIDDLE, TokenType::FSTRING_END) };
        let prefix = if m.tstring { 't' } else { 'f' };

        loop {
            let start = self.pos;
            let slineno = self.lineno;
            let scol = self.col_of(start);
            let c0 = self.peek();
            let c1 = self.peek_at(1);
            // field start / spec end at the very beginning (CPython emits an empty
            // FSTRING_MIDDLE before a brace it hands over to the regular mode, except for a
            // plain field start)
            if c0 == b'{' as i32 && c1 != b'{' as i32 {
                self.empty_middle_done = false;
                return self.open_field(start, slineno, scol).map(Some);
            }
            let brace_handoff = (c0 == b'{' as i32 && in_spec) || (c0 == b'}' as i32 && (in_spec || c1 != b'}' as i32));
            if brace_handoff {
                if !self.empty_middle_done {
                    self.empty_middle_done = true;
                    let mut t = self.mk(mid_ty, start, slineno, scol);
                    t.end = start as u32;
                    t.end_lineno = slineno;
                    t.end_col = scol;
                    return Ok(Some(t));
                }
                self.empty_middle_done = false;
                if c0 == b'{' as i32 {
                    return self.open_field(start, slineno, scol).map(Some);
                }
                if in_spec {
                    return self.close_field(start, slineno, scol).map(Some);
                }
                self.pos += 1;
                return Err(self.err_here(format!("{}-string: single '}}' is not allowed", prefix)));
            }
            // end of string?
            let mut is_end = true;
            for i in 0..m.quote_size as usize {
                if self.peek_at(i) != m.quote as i32 {
                    is_end = false;
                    break;
                }
            }
            if is_end {
                self.pos += m.quote_size as usize;
                self.modes.pop();
                while matches!(self.fields.last(), Some(f) if f.mode == midx) {
                    self.fields.pop();
                }
                return Ok(Some(self.mk(end_ty, start, slineno, scol)));
            }

            // middle
            let mut end_quote_size = 0u8;
            let mut unicode_escape = false;
            let mut content_end: Option<usize>; // exclusive end of token text
            loop {
                // fast path over ordinary characters
                {
                    let b = self.b;
                    let mut i = self.pos;
                    while i < b.len() {
                        let ch = b[i];
                        if ch == m.quote || ch == b'{' || ch == b'}' || ch == b'\\' || ch == b'\n' || ch == b'\r' {
                            break;
                        }
                        i += 1;
                    }
                    if i != self.pos {
                        self.pos = i;
                        end_quote_size = 0;
                    }
                }
                let c = self.peek();
                if c < 0 || (m.quote_size == 1 && (c == b'\n' as i32 || c == b'\r' as i32)) {
                    if in_spec && c >= 0 {
                        return Err(self.err_at(
                            ErrorKind::Syntax,
                            format!("{}-string: newlines are not allowed in format specifiers for single quoted {}-strings", prefix, prefix),
                            self.lineno,
                            self.col_of(self.pos),
                            self.lineno,
                            self.col_of(self.pos) + 1,
                        ));
                    }
                    let detected = if c < 0 { self.last_line_no() } else { self.lineno };
                    let msg = if m.quote_size == 3 {
                        format!("unterminated triple-quoted {}-string literal (detected at line {})", prefix, detected)
                    } else {
                        format!("unterminated {}-string literal (detected at line {})", prefix, detected)
                    };
                    return Err(self.string_error(msg, m.start_lineno, m.start_line_start, m.start));
                }
                if c == m.quote as i32 {
                    end_quote_size += 1;
                    self.next();
                    if end_quote_size == m.quote_size {
                        // back up the quotes
                        self.pos -= m.quote_size as usize;
                        content_end = Some(self.pos);
                        break;
                    }
                    continue;
                }
                end_quote_size = 0;
                if c == b'{' as i32 {
                    if self.peek_at(1) != b'{' as i32 || in_spec {
                        content_end = Some(self.pos);
                        break;
                    }
                    // escaped '{{'
                    self.pos += 2;
                    content_end = Some(self.pos - 1);
                    break;
                }
                if c == b'}' as i32 {
                    if unicode_escape {
                        self.pos += 1;
                        content_end = Some(self.pos);
                        break;
                    }
                    if self.peek_at(1) == b'}' as i32 && !in_spec && !has_field {
                        self.pos += 2;
                        content_end = Some(self.pos - 1);
                        break;
                    }
                    content_end = Some(self.pos);
                    break;
                }
                self.next();
                if c == b'\\' as i32 {
                    let mut p = self.peek();
                    if p == b'\r' as i32 {
                        self.next();
                        p = self.peek();
                    }
                    if p == b'{' as i32 || p == b'}' as i32 {
                        if !m.raw {
                            let ln = self.lineno;
                            let col = self.col_of(self.pos);
                            let ch = p as u8 as char;
                            let msg = format!("\"\\{}\" is an invalid escape sequence. Such sequences will not work in the future. Did you mean \"\\\\{}\"? A raw string is also an option.", ch, ch);
                            self.warn(msg, ln, col - 1, ln, col + 1);
                            if let Some(w) = self.warnings.last_mut() {
                                w.error_msg = format!("\"\\{}\" is an invalid escape sequence. Did you mean \"\\\\{}\"? A raw string is also an option.", ch, ch);
                            }
                        }
                        continue;
                    }
                    if p < 0 {
                        continue;
                    }
                    if !m.raw && p == b'N' as i32 {
                        self.next();
                        if self.peek() == b'{' as i32 {
                            self.next();
                            unicode_escape = true;
                        }
                        continue;
                    }
                    // skip the escaped char
                    if m.quote_size == 1 && (p == b'\n' as i32) {
                        self.next();
                        continue;
                    }
                    self.next();
                }
            }
            let cend = content_end.unwrap();
            if cend == start {
                // nothing accumulated; loop again to handle '{', '}' or end
                if self.pos == start {
                    // a lone '}' outside of a spec (single '}' not allowed) or '{' with spec
                    let c = self.peek();
                    if c == b'}' as i32 {
                        self.pos += 1;
                        if !has_field {
                            return Err(self.err_here(format!("{}-string: single '}}' is not allowed", prefix)));
                        }
                        // closing a field from literal mode (should not happen: in_spec handled)
                        self.pos -= 1;
                        return self.close_field(start, slineno, scol).map(Some);
                    }
                    if c == b'{' as i32 {
                        return self.open_field(start, slineno, scol).map(Some);
                    }
                }
                continue;
            }
            let t = Token {
                ty: mid_ty,
                kw: Kw::None,
                level: self.parens.len() as u16,
                start: start as u32,
                end: cend as u32,
                lineno: slineno,
                col: scol,
                end_lineno: self.lineno,
                end_col: self.col_of(cend.max(self.line_start)) - if cend < self.line_start { 0 } else { 0 },
            };
            // end position: compute from cend (may be before pos when braces were escaped)
            let t = self.fix_end(t, cend);
            let nc = self.peek();
            self.empty_middle_done = cend == self.pos && (nc == b'{' as i32 || nc == b'}' as i32);
            return Ok(Some(t));
        }
    }

    fn fix_end(&self, mut t: Token, cend: usize) -> Token {
        // cend is always on the current line or earlier; compute line/col for it.
        if cend >= self.line_start {
            t.end_lineno = self.lineno;
            t.end_col = (cend - self.line_start) as i32;
        } else {
            // compute by scanning back
            let mut ln = self.lineno;
            let mut ls = self.line_start;
            while cend < ls {
                ln -= 1;
                ls = line_start_of_before(self.b, ls);
            }
            t.end_lineno = ln;
            t.end_col = (cend - ls) as i32;
        }
        t
    }

    fn open_field(&mut self, start: usize, slineno: u32, scol: i32) -> Result<Token, LexError> {
        let midx = self.modes.len() - 1;
        let nfields = self.fields.iter().filter(|f| f.mode == midx).count();
        if nfields + 1 >= MAX_EXPR_NESTING {
            return Err(self.err_here(format!("{}-string: expressions nested too deeply", self.fprefix())));
        }
        if self.parens.len() >= MAXLEVEL {
            self.pos += 1;
            return Err(self.err_here("too many nested parentheses"));
        }
        self.pos += 1;
        self.parens.push(Paren { ch: b'{', lineno: slineno, col: scol });
        self.fields.push(Field { mode: midx, depth: self.parens.len(), in_spec: false, lineno: slineno });
        self.line_has_tokens = true;
        Ok(self.mk(TokenType::LBRACE, start, slineno, scol))
    }

    fn close_field(&mut self, start: usize, slineno: u32, scol: i32) -> Result<Token, LexError> {
        self.pos += 1;
        self.parens.pop();
        self.fields.pop();
        Ok(self.mk(TokenType::RBRACE, start, slineno, scol))
    }

    // ------------------------------------------------------------------ operators

    fn lex_operator(&mut self, start: usize, slineno: u32, scol: i32) -> Result<Token, LexError> {
        use TokenType::*;
        let c = self.b[self.pos];
        let c2 = self.peek_at(1);
        let c3 = self.peek_at(2);
        // f-string field handling for ':' and closing brackets
        let in_field = match (self.modes.len(), self.fields.last()) {
            (n, Some(f)) if n > 0 && f.mode == n - 1 && !f.in_spec => Some(*f),
            _ => None,
        };
        if let Some(f) = in_field {
            if c == b':' && self.parens.len() == f.depth {
                self.pos += 1;
                self.fields.last_mut().unwrap().in_spec = true;
                return Ok(self.mk(COLON, start, slineno, scol));
            }
        }
        let ty = match c {
            b'(' => LPAR,
            b')' => RPAR,
            b'[' => LSQB,
            b']' => RSQB,
            b'{' => LBRACE,
            b'}' => RBRACE,
            b',' => COMMA,
            b';' => SEMI,
            b'~' => TILDE,
            b':' => {
                if c2 == b'=' as i32 {
                    self.pos += 1;
                    COLONEQUAL
                } else {
                    COLON
                }
            }
            b'!' => {
                if c2 == b'=' as i32 {
                    self.pos += 1;
                    NOTEQUAL
                } else {
                    EXCLAMATION
                }
            }
            b'%' => self.op2(c2, PERCENT, PERCENTEQUAL),
            b'&' => self.op2(c2, AMPER, AMPEREQUAL),
            b'^' => self.op2(c2, CIRCUMFLEX, CIRCUMFLEXEQUAL),
            b'|' => self.op2(c2, VBAR, VBAREQUAL),
            b'@' => self.op2(c2, AT, ATEQUAL),
            b'+' => self.op2(c2, PLUS, PLUSEQUAL),
            b'=' => self.op2(c2, EQUAL, EQEQUAL),
            b'-' => {
                if c2 == b'=' as i32 {
                    self.pos += 1;
                    MINEQUAL
                } else if c2 == b'>' as i32 {
                    self.pos += 1;
                    RARROW
                } else {
                    MINUS
                }
            }
            b'*' => {
                if c2 == b'*' as i32 {
                    if c3 == b'=' as i32 {
                        self.pos += 2;
                        DOUBLESTAREQUAL
                    } else {
                        self.pos += 1;
                        DOUBLESTAR
                    }
                } else if c2 == b'=' as i32 {
                    self.pos += 1;
                    STAREQUAL
                } else {
                    STAR
                }
            }
            b'/' => {
                if c2 == b'/' as i32 {
                    if c3 == b'=' as i32 {
                        self.pos += 2;
                        DOUBLESLASHEQUAL
                    } else {
                        self.pos += 1;
                        DOUBLESLASH
                    }
                } else if c2 == b'=' as i32 {
                    self.pos += 1;
                    SLASHEQUAL
                } else {
                    SLASH
                }
            }
            b'<' => {
                if c2 == b'<' as i32 {
                    if c3 == b'=' as i32 {
                        self.pos += 2;
                        LEFTSHIFTEQUAL
                    } else {
                        self.pos += 1;
                        LEFTSHIFT
                    }
                } else if c2 == b'=' as i32 {
                    self.pos += 1;
                    LESSEQUAL
                } else if c2 == b'>' as i32 && !self.opts.extra_tokens {
                    self.pos += 1;
                    NOTEQUAL
                } else {
                    LESS
                }
            }
            b'>' => {
                if c2 == b'>' as i32 {
                    if c3 == b'=' as i32 {
                        self.pos += 2;
                        RIGHTSHIFTEQUAL
                    } else {
                        self.pos += 1;
                        RIGHTSHIFT
                    }
                } else if c2 == b'=' as i32 {
                    self.pos += 1;
                    GREATEREQUAL
                } else {
                    GREATER
                }
            }
            0 => {
                self.pos += 1;
                return Err(self.err_here("source code cannot contain null bytes"));
            }
            _ => {
                if c < 32 || c == 127 {
                    self.pos += 1;
                    return Err(self.err_here(format!("invalid non-printable character U+{:04X}", c)));
                }
                OP
            }
        };
        self.pos += 1;
        // bracket tracking
        match c {
            b'(' | b'[' | b'{' => {
                if self.parens.len() >= MAXLEVEL {
                    return Err(self.err_here("too many nested parentheses"));
                }
                self.parens.push(Paren { ch: c, lineno: slineno, col: scol });
            }
            b')' | b']' | b'}' => {
                if let Some(f) = in_field {
                    if self.parens.len() == f.depth {
                        if c == b'}' {
                            self.parens.pop();
                            self.fields.pop();
                            return Ok(self.mk(ty, start, slineno, scol));
                        }
                        if !self.opts.extra_tokens {
                            return Err(self.err_here(format!("{}-string: unmatched '{}'", self.fprefix(), c as char)));
                        }
                    }
                }
                match self.parens.last() {
                    None => {
                        if !self.opts.extra_tokens {
                            return Err(self.err_here(format!("unmatched '{}'", c as char)));
                        }
                    }
                    Some(p) => {
                        let p = *p;
                        let ok = matches!((p.ch, c), (b'(', b')') | (b'[', b']') | (b'{', b'}'));
                        if !ok && !self.opts.extra_tokens {
                            let msg = if p.lineno != self.lineno {
                                format!(
                                    "closing parenthesis '{}' does not match opening parenthesis '{}' on line {}",
                                    c as char, p.ch as char, p.lineno
                                )
                            } else {
                                format!("closing parenthesis '{}' does not match opening parenthesis '{}'", c as char, p.ch as char)
                            };
                            return Err(self.err_here(msg));
                        }
                        self.parens.pop();
                    }
                }
            }
            _ => {}
        }
        Ok(self.mk(ty, start, slineno, scol))
    }

    #[inline]
    fn op2(&mut self, c2: i32, one: TokenType, two: TokenType) -> TokenType {
        if c2 == b'=' as i32 {
            self.pos += 1;
            two
        } else {
            one
        }
    }
}

fn is_valid_identifier(s: &str, hooks: &UnicodeHooks) -> bool {
    let mut first = true;
    for ch in s.chars() {
        let ok = if first {
            ch == '_' || ch.is_ascii_alphabetic() || (!ch.is_ascii() && (hooks.is_xid_start)(ch))
        } else {
            ch == '_' || ch.is_ascii_alphanumeric() || (!ch.is_ascii() && (hooks.is_xid_continue)(ch))
        };
        if !ok {
            return false;
        }
        first = false;
    }
    !first
}

/// Byte offset of the start of line `lineno` (1-based).
pub fn line_start_of(src: &str, lineno: u32) -> usize {
    if lineno <= 1 {
        return 0;
    }
    let b = src.as_bytes();
    let mut ln = 1;
    for (i, &c) in b.iter().enumerate() {
        if c == b'\n' {
            ln += 1;
            if ln == lineno {
                return i + 1;
            }
        }
    }
    b.len()
}

fn line_start_of_before(b: &[u8], ls: usize) -> usize {
    // ls is a line start (> 0); find the start of the previous line
    let mut i = ls - 1; // at '\n'
    while i > 0 && b[i - 1] != b'\n' {
        i -= 1;
    }
    i
}

/// Text of line `lineno` (1-based) including its line terminator; if the source has no
/// terminator on that line, a '\n' is appended (as CPython's tokenizer does).
pub fn line_text(src: &str, lineno: u32) -> String {
    let s = line_start_of(src, lineno);
    let b = src.as_bytes();
    let mut e = s;
    while e < b.len() && b[e] != b'\n' {
        e += 1;
    }
    if e < b.len() {
        src[s..=e].to_string()
    } else {
        let mut t = src[s..e].to_string();
        t.push('\n');
        t
    }
}

/// Build a SyntaxError from 0-based byte positions (CPython conversion to char offsets).
pub fn make_error(src: &str, kind: ErrorKind, msg: String, lineno: i32, col: i32, end_lineno: i32, end_col: i32) -> SyntaxError {
    crate::parser::make_error_cpython(src, kind, msg, lineno, col, end_lineno, end_col)
}

// =============================================================================================
// Source decoding
// =============================================================================================

/// Find a PEP 263 coding spec in a line: `coding[:=]\s*([-\w.]+)` within a comment.
fn find_coding_spec(line: &[u8]) -> Option<String> {
    // Must be a comment line: optional whitespace then '#'
    let mut i = 0;
    while i < line.len() && (line[i] == b' ' || line[i] == b'\t' || line[i] == 0x0c) {
        i += 1;
    }
    if i >= line.len() || line[i] != b'#' {
        return None;
    }
    let rest = &line[i..];
    let mut j = 0;
    while j + 6 <= rest.len() {
        if &rest[j..j + 6] == b"coding" {
            let mut k = j + 6;
            if k < rest.len() && (rest[k] == b':' || rest[k] == b'=') {
                k += 1;
                while k < rest.len() && (rest[k] == b' ' || rest[k] == b'\t') {
                    k += 1;
                }
                let s = k;
                while k < rest.len() && (rest[k].is_ascii_alphanumeric() || rest[k] == b'-' || rest[k] == b'_' || rest[k] == b'.') {
                    k += 1;
                }
                if k > s {
                    return Some(String::from_utf8_lossy(&rest[s..k]).into_owned());
                }
            }
        }
        j += 1;
    }
    None
}

/// Normalize an encoding name like CPython's `get_normal_name`.
pub fn normalize_encoding_name(s: &str) -> String {
    let lower: String = s.chars().take(12).map(|c| if c == '_' { '-' } else { c.to_ascii_lowercase() }).collect();
    if lower == "utf-8" || lower.starts_with("utf-8-") {
        return "utf-8".into();
    }
    for p in ["latin-1", "iso-8859-1", "iso-latin-1"] {
        if lower == p || lower.starts_with(&format!("{}-", p)) {
            return "iso-8859-1".into();
        }
    }
    s.to_string()
}

/// Result of [`detect_encoding`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetectedEncoding {
    /// Normalized encoding name ("utf-8", "iso-8859-1", or the declared name).
    pub encoding: String,
    /// True if a UTF-8 BOM was present.
    pub bom: bool,
    /// Number of lines consumed while detecting (0, 1 or 2) — as tokenize.detect_encoding.
    pub lines_consumed: usize,
    /// True if the encoding came from a coding cookie.
    pub from_cookie: bool,
}

/// Detect the source encoding from the BOM and PEP 263 cookie in the first two lines.
pub fn detect_encoding(src: &[u8]) -> Result<DetectedEncoding, SyntaxError> {
    let mut data = src;
    let mut bom = false;
    if data.starts_with(b"\xEF\xBB\xBF") {
        bom = true;
        data = &data[3..];
    }
    let mut lines = data.split_inclusive(|&c| c == b'\n');
    let mut consumed = 0;
    for idx in 0..2 {
        let Some(line) = lines.next() else { break };
        consumed += 1;
        if let Some(enc) = find_coding_spec(line) {
            if line.contains(&0) {
                return Err(SyntaxError::simple("source code cannot contain null bytes"));
            }
            let norm = normalize_encoding_name(&enc);
            if bom && norm != "utf-8" {
                let mut e = SyntaxError::simple(format!("encoding problem: {} with BOM", enc));
                e.lineno = idx as i32 + 1;
                return Err(e);
            }
            return Ok(DetectedEncoding { encoding: norm, bom, lines_consumed: consumed, from_cookie: true });
        }
        // Only continue to the second line if the first is blank or a comment.
        let t: Vec<u8> = line.iter().copied().filter(|c| !c.is_ascii_whitespace()).collect();
        if idx == 0 && !(t.is_empty() || t[0] == b'#') {
            break;
        }
    }
    Ok(DetectedEncoding { encoding: "utf-8".into(), bom, lines_consumed: consumed, from_cookie: false })
}

fn canonical(enc: &str) -> String {
    enc.to_ascii_lowercase().replace('_', "-")
}

#[allow(dead_code)]
fn is_known_encoding(enc: &str) -> bool {
    let e = canonical(enc);
    matches!(
        e.as_str(),
        "utf-8" | "utf8" | "u8" | "utf" | "cp65001" | "iso-8859-1" | "latin-1" | "latin1" | "l1" | "ascii" | "us-ascii" | "646"
            | "cp1252" | "windows-1252" | "iso-8859-15" | "iso8859-15" | "latin-9" | "latin9" | "iso-8859-2" | "latin2"
            | "iso8859-1" | "iso8859-2" | "8859" | "cp819" | "latin" | "iso-ir-100" | "l2" | "utf-8-sig" | "iso-8859-3"
            | "iso-8859-4" | "iso-8859-5" | "iso-8859-6" | "iso-8859-7" | "iso-8859-8" | "iso-8859-9" | "iso-8859-10"
            | "iso-8859-13" | "iso-8859-14" | "iso-8859-16" | "koi8-r" | "koi8-u" | "cp1251" | "cp1250" | "mac-roman"
            | "euc-jp" | "shift-jis" | "gbk" | "gb2312" | "big5" | "utf-16" | "utf-32" | "cp437" | "cp850" | "iso8859-5"
    ) || e.starts_with("iso8859") || e.starts_with("iso-8859") || e.starts_with("cp") || e.starts_with("latin")
}

/// Decoder hook for encodings the parser does not implement natively:
/// `(encoding name, bytes) -> None` if the encoding is unknown, `Some(Err(message))` on a
/// decoding error, `Some(Ok(text))` on success. The runtime passes one backed by `codecs`.
pub type DecodeHook = fn(&str, &[u8]) -> Option<Result<String, String>>;

/// Decode raw source bytes to a string: handles the UTF-8 BOM and PEP 263 declarations.
/// Natively supported: utf-8, ascii, latin-1/iso-8859-1, iso-8859-2/-5/-7/-15, cp1251,
/// cp1252, koi8-r. Other encodings go through `hook` (or fail with "unknown encoding").
pub fn decode_source(src: &[u8]) -> Result<String, SyntaxError> {
    decode_source_with(src, None, "<string>")
}

fn single_byte(data: &[u8], table: &[u16; 128]) -> Result<String, usize> {
    let mut s = String::with_capacity(data.len());
    for (i, &c) in data.iter().enumerate() {
        if c < 0x80 {
            s.push(c as char);
        } else {
            let u = table[(c - 0x80) as usize];
            if u == 0xFFFF {
                return Err(i);
            }
            s.push(char::from_u32(u as u32).unwrap());
        }
    }
    Ok(s)
}

fn latin1_table(overrides: &[(u8, u16)]) -> [u16; 128] {
    let mut t = [0u16; 128];
    for (i, x) in t.iter_mut().enumerate() {
        *x = 0x80 + i as u16;
    }
    for &(b, u) in overrides {
        t[(b - 0x80) as usize] = u;
    }
    t
}

/// Location info for an undecodable byte at `off` of `data`: (lineno, 1-based char offset, line text).
fn bad_byte_location(data: &[u8], off: usize) -> (i32, i32, String) {
    let lineno = data[..off].iter().filter(|&&c| c == b'\n').count() as i32 + 1;
    let ls = data[..off].iter().rposition(|&c| c == b'\n').map(|p| p + 1).unwrap_or(0);
    let le = data[off..].iter().position(|&c| c == b'\n').map(|p| off + p).unwrap_or(data.len());
    let col_chars = String::from_utf8_lossy(&data[ls..off]).chars().count() as i32;
    let text = String::from_utf8_lossy(&data[ls..le]).into_owned();
    (lineno, col_chars + 1, text)
}

/// `decode_source` with a fallback decoder and the file name used in error messages.
pub fn decode_source_with(src: &[u8], hook: Option<DecodeHook>, filename: &str) -> Result<String, SyntaxError> {
    let det = detect_encoding(src)?;
    let data = if det.bom { &src[3..] } else { src };
    let enc = canonical(&det.encoding);
    let codec_error = |what: &str, off: usize, reason: &str| -> SyntaxError {
        let (lineno, offset, text) = bad_byte_location(data, off);
        let mut e = SyntaxError::simple(format!("(unicode error) '{}' codec can't decode byte 0x{:02x} in position {}: {}", what, data[off], off, reason));
        e.lineno = lineno;
        e.offset = offset;
        e.end_lineno = Some(lineno);
        e.end_offset = Some(offset);
        e.text = Some(text);
        e
    };
    let r: Result<String, SyntaxError> = match enc.as_str() {
        "utf-8" | "utf8" | "u8" | "utf" | "cp65001" | "utf-8-sig" => match std::str::from_utf8(data) {
            Ok(s) => Ok(s.to_string()),
            Err(e) => {
                let off = e.valid_up_to();
                let (lineno, offset, text) = bad_byte_location(data, off);
                let mut err = if det.from_cookie || det.bom {
                    let reason = if e.error_len().is_none() { "unexpected end of data" } else if data[off] >= 0xC2 && data[off] <= 0xF4 { "invalid continuation byte" } else { "invalid start byte" };
                    SyntaxError::simple(format!("(unicode error) 'utf-8' codec can't decode byte 0x{:02x} in position {}: {}", data[off], off, reason))
                } else {
                    SyntaxError::simple(format!(
                        "Non-UTF-8 code starting with '\\x{:02x}' in file {} on line {}, but no encoding declared; see https://peps.python.org/pep-0263/ for details",
                        data[off], filename, lineno
                    ))
                };
                err.lineno = lineno;
                err.offset = offset;
                err.end_lineno = Some(lineno);
                err.end_offset = Some(offset);
                err.text = Some(text);
                Err(err)
            }
        },
        "iso-8859-1" | "latin-1" | "latin1" | "l1" | "iso8859-1" | "8859" | "cp819" | "latin" | "iso-ir-100" => {
            Ok(data.iter().map(|&c| c as char).collect())
        }
        "ascii" | "us-ascii" | "646" => match data.iter().position(|&c| c >= 128) {
            Some(p) => Err(codec_error("ascii", p, "ordinal not in range(128)")),
            None => Ok(String::from_utf8(data.to_vec()).unwrap()),
        },
        "cp1252" | "windows-1252" => Ok(data.iter().map(|&c| cp1252(c)).collect()),
        "koi8-r" | "koi8r" => single_byte(data, &KOI8R).map_err(|p| codec_error("koi8-r", p, "character maps to <undefined>")),
        "cp1251" | "windows-1251" => single_byte(data, &CP1251).map_err(|p| codec_error("charmap", p, "character maps to <undefined>")),
        "iso-8859-15" | "iso8859-15" | "latin-9" | "latin9" => {
            let t = latin1_table(&[(0xA4, 0x20AC), (0xA6, 0x0160), (0xA8, 0x0161), (0xB4, 0x017D), (0xB8, 0x017E), (0xBC, 0x0152), (0xBD, 0x0153), (0xBE, 0x0178)]);
            single_byte(data, &t).map_err(|p| codec_error("iso8859-15", p, "character maps to <undefined>"))
        }
        "iso-8859-2" | "iso8859-2" | "latin2" | "latin-2" | "l2" => {
            single_byte(data, &ISO8859_2).map_err(|p| codec_error("iso8859-2", p, "character maps to <undefined>"))
        }
        "iso-8859-5" | "iso8859-5" | "cyrillic" => {
            let mut t = latin1_table(&[]);
            for b in 0xA1u16..=0xFF {
                t[(b - 0x80) as usize] = match b {
                    0xAD => 0x00AD,
                    0xF0 => 0x2116,
                    0xFD => 0x00A7,
                    _ => 0x0400 + (b - 0xA0),
                };
            }
            single_byte(data, &t).map_err(|p| codec_error("iso8859-5", p, "character maps to <undefined>"))
        }
        "iso-8859-7" | "iso8859-7" | "greek" | "greek8" => {
            let mut t = latin1_table(&[
                (0xA1, 0x2018), (0xA2, 0x2019), (0xA4, 0x20AC), (0xA5, 0x20AF), (0xAA, 0x037A), (0xAE, 0xFFFF), (0xAF, 0x2015),
                (0xB4, 0x0384), (0xB5, 0x0385), (0xB6, 0x0386), (0xB8, 0x0388), (0xB9, 0x0389), (0xBA, 0x038A), (0xBC, 0x038C),
                (0xBE, 0x038E), (0xBF, 0x038F), (0xD2, 0xFFFF), (0xFF, 0xFFFF),
            ]);
            for b in 0xC0u16..=0xFE {
                if b != 0xD2 {
                    t[(b - 0x80) as usize] = 0x0390 + (b - 0xC0);
                }
            }
            single_byte(data, &t).map_err(|p| codec_error("iso8859-7", p, "character maps to <undefined>"))
        }
        _ => match hook.and_then(|h| h(&det.encoding, data)) {
            Some(Ok(s)) => Ok(s),
            Some(Err(m)) => Err(SyntaxError::simple(format!("(unicode error) {}", m))),
            None => {
                let mut e = SyntaxError::simple(format!("unknown encoding: {}", det.encoding));
                e.lineno = if det.lines_consumed > 0 { det.lines_consumed as i32 } else { 1 };
                Err(e)
            }
        },
    };
    r
}

const CP1251: [u16; 128] = [
    0x0402, 0x0403, 0x201A, 0x0453, 0x201E, 0x2026, 0x2020, 0x2021, 0x20AC, 0x2030, 0x0409, 0x2039, 0x040A, 0x040C, 0x040B, 0x040F,
    0x0452, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0xFFFF, 0x2122, 0x0459, 0x203A, 0x045A, 0x045C, 0x045B, 0x045F,
    0x00A0, 0x040E, 0x045E, 0x0408, 0x00A4, 0x0490, 0x00A6, 0x00A7, 0x0401, 0x00A9, 0x0404, 0x00AB, 0x00AC, 0x00AD, 0x00AE, 0x0407,
    0x00B0, 0x00B1, 0x0406, 0x0456, 0x0491, 0x00B5, 0x00B6, 0x00B7, 0x0451, 0x2116, 0x0454, 0x00BB, 0x0458, 0x0405, 0x0455, 0x0457,
    0x0410, 0x0411, 0x0412, 0x0413, 0x0414, 0x0415, 0x0416, 0x0417, 0x0418, 0x0419, 0x041A, 0x041B, 0x041C, 0x041D, 0x041E, 0x041F,
    0x0420, 0x0421, 0x0422, 0x0423, 0x0424, 0x0425, 0x0426, 0x0427, 0x0428, 0x0429, 0x042A, 0x042B, 0x042C, 0x042D, 0x042E, 0x042F,
    0x0430, 0x0431, 0x0432, 0x0433, 0x0434, 0x0435, 0x0436, 0x0437, 0x0438, 0x0439, 0x043A, 0x043B, 0x043C, 0x043D, 0x043E, 0x043F,
    0x0440, 0x0441, 0x0442, 0x0443, 0x0444, 0x0445, 0x0446, 0x0447, 0x0448, 0x0449, 0x044A, 0x044B, 0x044C, 0x044D, 0x044E, 0x044F,
];

const ISO8859_2: [u16; 128] = [
    0x0080, 0x0081, 0x0082, 0x0083, 0x0084, 0x0085, 0x0086, 0x0087, 0x0088, 0x0089, 0x008A, 0x008B, 0x008C, 0x008D, 0x008E, 0x008F,
    0x0090, 0x0091, 0x0092, 0x0093, 0x0094, 0x0095, 0x0096, 0x0097, 0x0098, 0x0099, 0x009A, 0x009B, 0x009C, 0x009D, 0x009E, 0x009F,
    0x00A0, 0x0104, 0x02D8, 0x0141, 0x00A4, 0x013D, 0x015A, 0x00A7, 0x00A8, 0x0160, 0x015E, 0x0164, 0x0179, 0x00AD, 0x017D, 0x017B,
    0x00B0, 0x0105, 0x02DB, 0x0142, 0x00B4, 0x013E, 0x015B, 0x02C7, 0x00B8, 0x0161, 0x015F, 0x0165, 0x017A, 0x02DD, 0x017E, 0x017C,
    0x0154, 0x00C1, 0x00C2, 0x0102, 0x00C4, 0x0139, 0x0106, 0x00C7, 0x010C, 0x00C9, 0x0118, 0x00CB, 0x011A, 0x00CD, 0x00CE, 0x010E,
    0x0110, 0x0143, 0x0147, 0x00D3, 0x00D4, 0x0150, 0x00D6, 0x00D7, 0x0158, 0x016E, 0x00DA, 0x0170, 0x00DC, 0x00DD, 0x0162, 0x00DF,
    0x0155, 0x00E1, 0x00E2, 0x0103, 0x00E4, 0x013A, 0x0107, 0x00E7, 0x010D, 0x00E9, 0x0119, 0x00EB, 0x011B, 0x00ED, 0x00EE, 0x010F,
    0x0111, 0x0144, 0x0148, 0x00F3, 0x00F4, 0x0151, 0x00F6, 0x00F7, 0x0159, 0x016F, 0x00FA, 0x0171, 0x00FC, 0x00FD, 0x0163, 0x02D9,
];

fn cp1252(c: u8) -> char {
    const HI: [u16; 32] = [
        0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0x008D,
        0x017D, 0x008F, 0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A,
        0x0153, 0x009D, 0x017E, 0x0178,
    ];
    if (0x80..0xA0).contains(&c) {
        char::from_u32(HI[(c - 0x80) as usize] as u32).unwrap()
    } else {
        c as char
    }
}

/// Translate `\r\n` and lone `\r` to `\n` (as CPython does for compile()).
pub fn translate_newlines(src: &str) -> std::borrow::Cow<'_, str> {
    if !src.as_bytes().contains(&b'\r') {
        return std::borrow::Cow::Borrowed(src);
    }
    let mut out = String::with_capacity(src.len());
    let b = src.as_bytes();
    let mut i = 0;
    let mut last = 0;
    while i < b.len() {
        if b[i] == b'\r' {
            out.push_str(&src[last..i]);
            out.push('\n');
            if i + 1 < b.len() && b[i + 1] == b'\n' {
                i += 1;
            }
            last = i + 1;
        }
        i += 1;
    }
    out.push_str(&src[last..]);
    std::borrow::Cow::Owned(out)
}

// =============================================================================================
// tokenize-module style iterator
// =============================================================================================

/// A token as exposed by the stdlib `tokenize` module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PyToken {
    /// Generic type (operators are `OP`).
    pub ty: TokenType,
    /// Exact type (operators get their specific type).
    pub exact_type: TokenType,
    pub string: String,
    /// (lineno, character column)
    pub start: (u32, i32),
    pub end: (u32, i32),
    /// The physical line(s) containing the token.
    pub line: String,
}

/// Iterator over `tokenize`-style tokens (what `_tokenize.TokenizerIter` yields).
pub struct TokenIter<'a> {
    lexer: Lexer<'a>,
    extra: bool,
    done: bool,
}

impl<'a> TokenIter<'a> {
    /// `extra_tokens = true` gives the `tokenize.tokenize()`/`generate_tokens()` stream;
    /// false gives the raw C-tokenizer stream.
    pub fn new(src: &'a str, extra_tokens: bool, hooks: UnicodeHooks) -> TokenIter<'a> {
        TokenIter { lexer: Lexer::new(src, LexOptions { extra_tokens, type_comments: false, hooks }), extra: extra_tokens, done: false }
    }

    fn convert(&self, t: &Token) -> PyToken {
        let src = self.lexer.src;
        let mut string = if t.ty == TokenType::INDENT && !self.extra { String::new() } else { t.text(src).to_string() };
        if (t.ty == TokenType::FSTRING_MIDDLE || t.ty == TokenType::TSTRING_MIDDLE) && string.ends_with(['{', '}']) {
            // escaped braces: token text already excludes the doubled brace
        }
        if t.ty == TokenType::ENDMARKER || t.ty == TokenType::DEDENT {
            string.clear();
        }
        let line = if t.ty == TokenType::ENDMARKER {
            String::new()
        } else {
            let mut l = String::new();
            let last = t.end_lineno.max(t.lineno);
            for ln in t.lineno..=last {
                l.push_str(&raw_line_text(src, ln));
            }
            if t.ty == TokenType::NEWLINE || t.ty == TokenType::NL || t.end_col == 0 && t.end_lineno > t.lineno {
                // keep as is
            }
            l
        };
        let to_char = |ln: u32, col: i32| -> i32 {
            if col <= 0 {
                return col;
            }
            let lt = raw_line_text(src, ln);
            let off = byte_col_to_char_offset(&lt, col) - 1;
            off
        };
        let ty = if t.ty.is_operator() && self.extra { TokenType::OP } else { t.ty };
        PyToken {
            ty,
            exact_type: t.ty,
            string,
            start: (t.lineno, to_char(t.lineno, t.col)),
            end: (t.end_lineno, to_char(t.end_lineno, t.end_col)),
            line,
        }
    }

    /// Unclosed-paren errors at EOF become SyntaxErrors.
    fn lex_err(&self, e: LexError) -> SyntaxError {
        lex_error_to_syntax(self.lexer.src, e)
    }
}

/// Raw line text without implicit newline.
fn raw_line_text(src: &str, lineno: u32) -> String {
    let s = line_start_of(src, lineno);
    let b = src.as_bytes();
    let mut e = s;
    while e < b.len() && b[e] != b'\n' {
        e += 1;
    }
    if e < b.len() {
        src[s..=e].to_string()
    } else {
        src[s..e].to_string()
    }
}

pub(crate) fn lex_error_to_syntax(src: &str, e: LexError) -> SyntaxError {
    match e {
        LexError::Syntax(s) => *s,
        LexError::UnclosedParen(ch, ln, col) => {
            make_error(src, ErrorKind::Syntax, format!("'{}' was never closed", ch as char), ln as i32, col, ln as i32, col + 1)
        }
        LexError::Eof { lineno, col } => {
            let mut e = make_error(src, ErrorKind::Syntax, "unexpected EOF while parsing".into(), lineno as i32, col, lineno as i32, col);
            let line = line_text(src, lineno);
            e.offset = byte_col_to_char_offset(&line, col);
            e.end_offset = Some(e.offset);
            e
        }
    }
}

impl<'a> Iterator for TokenIter<'a> {
    type Item = Result<PyToken, SyntaxError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.lexer.next_token() {
            Ok(t) => {
                if t.ty == TokenType::ENDMARKER {
                    self.done = true;
                }
                let mut pt = self.convert(&t);
                if t.ty == TokenType::ENDMARKER && self.extra {
                    pt.start = (t.lineno, 0);
                    pt.end = (t.lineno, 0);
                }
                Some(Ok(pt))
            }
            Err(LexError::UnclosedParen(..)) if self.extra => {
                // tokenize module: emit NEWLINE/ENDMARKER-ish end; CPython raises TokenError
                self.done = true;
                let e = SyntaxError::simple("unexpected EOF in multi-line statement");
                Some(Err(e))
            }
            Err(e) => {
                self.done = true;
                Some(Err(self.lex_err(e)))
            }
        }
    }
}

/// Tokenize a whole source into parser tokens (no COMMENT/NL), for testing/benchmarks.
pub fn tokenize_all(src: &str) -> Result<Vec<Token>, SyntaxError> {
    let mut lx = Lexer::new(src, LexOptions::default());
    let mut out = Vec::new();
    loop {
        match lx.next_token() {
            Ok(t) => {
                let end = t.ty == TokenType::ENDMARKER;
                out.push(t);
                if end {
                    return Ok(out);
                }
            }
            Err(e) => return Err(lex_error_to_syntax(src, e)),
        }
    }
}

const KOI8R: [u16; 128] = [
    0x2500, 0x2502, 0x250C, 0x2510, 0x2514, 0x2518, 0x251C, 0x2524, 0x252C, 0x2534, 0x253C, 0x2580, 0x2584, 0x2588, 0x258C, 0x2590,
    0x2591, 0x2592, 0x2593, 0x2320, 0x25A0, 0x2219, 0x221A, 0x2248, 0x2264, 0x2265, 0x00A0, 0x2321, 0x00B0, 0x00B2, 0x00B7, 0x00F7,
    0x2550, 0x2551, 0x2552, 0x0451, 0x2553, 0x2554, 0x2555, 0x2556, 0x2557, 0x2558, 0x2559, 0x255A, 0x255B, 0x255C, 0x255D, 0x255E,
    0x255F, 0x2560, 0x2561, 0x0401, 0x2562, 0x2563, 0x2564, 0x2565, 0x2566, 0x2567, 0x2568, 0x2569, 0x256A, 0x256B, 0x256C, 0x00A9,
    0x044E, 0x0430, 0x0431, 0x0446, 0x0434, 0x0435, 0x0444, 0x0433, 0x0445, 0x0438, 0x0439, 0x043A, 0x043B, 0x043C, 0x043D, 0x043E,
    0x043F, 0x044F, 0x0440, 0x0441, 0x0442, 0x0443, 0x0436, 0x0432, 0x044C, 0x044B, 0x0437, 0x0448, 0x044D, 0x0449, 0x0447, 0x044A,
    0x042E, 0x0410, 0x0411, 0x0426, 0x0414, 0x0415, 0x0424, 0x0413, 0x0425, 0x0418, 0x0419, 0x041A, 0x041B, 0x041C, 0x041D, 0x041E,
    0x041F, 0x042F, 0x0420, 0x0421, 0x0422, 0x0423, 0x0416, 0x0412, 0x042C, 0x042B, 0x0417, 0x0428, 0x042D, 0x0429, 0x0427, 0x042A,
];

static ID_CHAR: [bool; 256] = {
    let mut t = [false; 256];
    let mut i = 0;
    while i < 256 {
        let c = i as u8;
        t[i] = c.is_ascii_alphanumeric() || c == b'_';
        i += 1;
    }
    t
};
