//! String literals, implicit concatenation, f-strings and t-strings.

use super::expr::bx;
use super::*;
use crate::string::{decode_bytes_escapes, decode_str_escapes, invalid_escape_messages, parse_prefix, InvalidEscape};
use TokenType as T;

/// One element of a `strings` sequence before concatenation.
enum Piece {
    /// Constant (str or bytes) from a STRING token
    Const(Expr, bool /*bytes*/),
    /// values of an f-string (already flattened)
    FString(Vec<Expr>),
    TString(Vec<Expr>),
}

fn is_empty_str_const(e: &Expr) -> bool {
    matches!(&e.node, ExprKind::Constant { value: Constant::Str(s), .. } if s.is_empty())
}

impl<'a> Parser<'a> {
    /// strings: (fstring | tstring | string)+
    #[inline(never)]
    pub(crate) fn strings(&mut self) -> R<Expr> {
        let start = self.p;
        // fast path: a single plain string literal
        let t = self.peek();
        if t.ty == T::STRING && !matches!(self.peek_n(1).ty, T::STRING | T::FSTRING_START | T::TSTRING_START) {
            self.advance();
            let (e, _) = self.string_constant(&t)?;
            return Ok(Some(e));
        }
        let mut pieces: Vec<Piece> = Vec::new();
        loop {
            let t = self.peek();
            match t.ty {
                T::STRING => {
                    self.advance();
                    let (e, is_bytes) = self.string_constant(&t)?;
                    pieces.push(Piece::Const(e, is_bytes));
                }
                T::FSTRING_START | T::TSTRING_START => {
                    let tstring = t.ty == T::TSTRING_START;
                    let m = self.p;
                    let Some(values) = self.fstring(tstring)? else {
                        self.p = m;
                        if pieces.is_empty() {
                            return Ok(None);
                        }
                        // CPython's strings rule would fail as a whole
                        self.p = start;
                        return Ok(None);
                    };
                    pieces.push(if tstring { Piece::TString(values) } else { Piece::FString(values) });
                }
                _ => break,
            }
        }
        if pieces.is_empty() {
            return Ok(None);
        }
        let loc = self.loc(start);
        self.concatenate(pieces, loc).map(Some)
    }

    fn concatenate(&mut self, pieces: Vec<Piece>, loc: Loc) -> Result<Expr, PErr> {
        let mut f_found = false;
        let mut t_found = false;
        let mut uni = false;
        let mut bytes = false;
        for p in &pieces {
            match p {
                Piece::Const(_, b) => {
                    if *b {
                        bytes = true
                    } else {
                        uni = true
                    }
                }
                Piece::FString(_) => f_found = true,
                Piece::TString(_) => t_found = true,
            }
        }
        if t_found && (uni || bytes || f_found) {
            return Err(self.err_loc("cannot mix t-string literals with string or bytes literals", loc));
        }
        if (uni || f_found) && bytes {
            return Err(self.err_last(ErrorKind::Syntax, "cannot mix bytes and nonbytes literals"));
        }
        if bytes {
            let mut out = Vec::new();
            let mut kind = None;
            for (i, p) in pieces.into_iter().enumerate() {
                if let Piece::Const(e, _) = p {
                    if let ExprKind::Constant { value: Constant::Bytes(b), kind: k } = e.node {
                        if i == 0 {
                            kind = k;
                        }
                        out.extend_from_slice(&b);
                    }
                }
            }
            return Ok(Expr::new(ExprKind::Constant { value: Constant::Bytes(out), kind }, loc));
        }
        let joined = f_found || t_found;
        if !joined && pieces.len() == 1 {
            if let Some(Piece::Const(e, _)) = pieces.into_iter().next() {
                return Ok(e);
            }
            unreachable!();
        }
        // flatten
        let mut flat: Vec<Expr> = Vec::new();
        for p in pieces {
            match p {
                Piece::Const(e, _) => flat.push(e),
                Piece::FString(v) | Piece::TString(v) => flat.extend(v),
            }
        }
        // fold adjacent constants
        let mut values: Vec<Expr> = Vec::with_capacity(flat.len());
        let mut it = flat.into_iter().peekable();
        while let Some(e) = it.next() {
            if let ExprKind::Constant { value: Constant::Str(_), .. } = &e.node {
                let mut cur = e;
                while let Some(nx) = it.peek() {
                    if !matches!(nx.node, ExprKind::Constant { value: Constant::Str(_), .. }) {
                        break;
                    }
                    let nx = it.next().unwrap();
                    let end = nx.loc;
                    if let (ExprKind::Constant { value: Constant::Str(a), .. }, ExprKind::Constant { value: Constant::Str(b), .. }) = (&mut cur.node, &nx.node) {
                        a.push_pystr(b);
                    }
                    cur.loc.end_lineno = end.end_lineno;
                    cur.loc.end_col_offset = end.end_col_offset;
                }
                if joined && is_empty_str_const(&cur) {
                    continue;
                }
                values.push(cur);
            } else {
                values.push(e);
            }
        }
        if !joined {
            return Ok(values.pop().unwrap());
        }
        if t_found {
            return Ok(Expr::new(ExprKind::TemplateStr { values }, loc));
        }
        Ok(Expr::new(ExprKind::JoinedStr { values }, loc))
    }

    /// Decode a STRING token into a Constant expression; returns (expr, is_bytes).
    fn string_constant(&mut self, t: &Token) -> Result<(Expr, bool), PErr> {
        let text = self.text(t);
        let pre = parse_prefix(text);
        let rest = &text[pre.len..];
        let q = rest.as_bytes()[0];
        let qs = if rest.len() >= 6 && rest.as_bytes()[1] == q && rest.as_bytes()[2] == q { 3 } else { 1 };
        let body = &rest[qs..rest.len() - qs];
        let loc = Self::tok_loc(t);
        let kind = if pre.unicode { Some("u".to_string()) } else { None };
        if pre.bytes {
            if !body.is_ascii() {
                return Err(self.err_tok("bytes can only contain ASCII literal characters", t));
            }
            let v = if pre.raw {
                body.as_bytes().to_vec()
            } else {
                let mut inv = None;
                let r = decode_bytes_escapes(body, &mut inv).map_err(|e| self.err_tok(e.msg, t))?;
                if let Some(inv) = inv {
                    self.warn_invalid_escape(t, body, &inv, pre.len + qs)?;
                }
                r
            };
            return Ok((Expr::new(ExprKind::Constant { value: Constant::Bytes(v), kind }, loc), true));
        }
        let v = if pre.raw {
            PyStr::Utf8(body.to_string())
        } else {
            let mut inv = None;
            let r = decode_str_escapes(body, self.opts.hooks.lookup_name, &mut inv).map_err(|e| self.err_tok(e.msg, t))?;
            if let Some(inv) = inv {
                self.warn_invalid_escape(t, body, &inv, pre.len + qs)?;
            }
            r
        };
        Ok((Expr::new(ExprKind::Constant { value: Constant::Str(v), kind }, loc), false))
    }

    /// Emit the SyntaxWarning for an invalid escape (first pass only), CPython-compatible
    /// location computation.
    fn warn_invalid_escape(&mut self, t: &Token, body: &str, inv: &InvalidEscape, quote_offset: usize) -> Result<(), PErr> {
        if self.invalid {
            return Ok(());
        }
        let first = inv.text.as_bytes()[0];
        if (t.ty == T::FSTRING_MIDDLE || t.ty == T::TSTRING_MIDDLE) && (first == b'{' || first == b'}') {
            return Ok(());
        }
        // CPython: walk from the body start to the char after the backslash
        let upto = inv.offset + 1;
        let mut lineno = t.lineno as i32;
        let mut col = t.col;
        for &c in &body.as_bytes()[..upto] {
            if c == b'\n' {
                lineno += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        if lineno == t.lineno as i32 {
            // CPython adds the number of leading quote chars of the token (0 if a prefix
            // precedes them).
            let tb = self.text(t).as_bytes();
            let mut qc = 0;
            let mut fq = 0u8;
            for &c in tb {
                if c == b'\'' || c == b'"' {
                    if qc == 0 {
                        fq = c;
                    }
                    if c == fq {
                        qc += 1;
                    }
                } else {
                    break;
                }
            }
            col += qc;
            let _ = quote_offset;
        }
        let (wmsg, emsg) = invalid_escape_messages(inv);
        let l = Loc { lineno, col_offset: col - 1, end_lineno: lineno, end_col_offset: col + 1 };
        self.warn(wmsg, emsg, l);
        if let Some(fv) = self.opts.feature_version {
            if fv < 12 {
                if let Some(w) = self.warnings.last_mut() {
                    w.category = crate::error::WarningCategory::DeprecationWarning;
                }
            }
        }
        Ok(())
    }

    /// fstring: FSTRING_START fstring_middle* FSTRING_END (or the t-string equivalent).
    /// Returns the (flattened) values.
    fn fstring(&mut self, tstring: bool) -> R<Vec<Expr>> {
        let start_tok = self.advance();
        if tstring {
            self.check_version(14, "t-strings are")?;
        }
        let raw = parse_prefix(self.text(&start_tok)).raw;
        let (mid, end) = if tstring { (T::TSTRING_MIDDLE, T::TSTRING_END) } else { (T::FSTRING_MIDDLE, T::FSTRING_END) };
        let mut values = Vec::new();
        loop {
            let t = self.peek();
            if t.ty == end {
                self.advance();
                break;
            }
            if t.ty == mid {
                self.advance();
                let s = self.decode_fstring_middle(&t, raw)?;
                if !s.is_empty() {
                    values.push(Expr::new(ExprKind::Constant { value: Constant::Str(s), kind: None }, Self::tok_loc(&t)));
                }
                continue;
            }
            if t.ty == T::LBRACE {
                let Some(mut v) = self.replacement_field(tstring, raw)? else { return Ok(None) };
                values.append(&mut v);
                continue;
            }
            return Ok(None);
        }
        Ok(Some(values))
    }

    fn decode_fstring_middle(&mut self, t: &Token, raw: bool) -> Result<PyStr, PErr> {
        let s = self.text(t);
        if raw || !s.contains('\\') {
            return Ok(PyStr::Utf8(s.to_string()));
        }
        let mut inv = None;
        // an unpaired backslash at the end of a piece is followed by the `{` of a
        // replacement field: `\{` is an invalid escape that keeps the backslash
        let dangling = s.bytes().rev().take_while(|&c| c == b'\\').count() % 2 == 1;
        let body = if dangling { &s[..s.len() - 1] } else { s };
        let mut r = decode_str_escapes(body, self.opts.hooks.lookup_name, &mut inv).map_err(|e| self.err_tok(e.msg, t))?;
        if dangling {
            r.push_char('\\');
            if inv.is_none() {
                inv = Some(InvalidEscape { offset: s.len() - 1, text: "{".to_string(), octal: false });
            }
        }
        if let Some(inv) = inv {
            self.warn_invalid_escape(t, s, &inv, 0)?;
        }
        Ok(r)
    }

    /// Source text between two byte offsets with comments removed from the gaps between the
    /// tokens in `[tok_from, tok_to)`.
    fn expr_text(&self, from: usize, tok_from: usize, tok_to: usize, to: usize) -> String {
        let mut out = String::new();
        let mut pos = from;
        let push_gap = |out: &mut String, gap: &str| {
            // remove comments: '#' to end of line
            let mut rest = gap;
            while let Some(i) = rest.find('#') {
                out.push_str(&rest[..i]);
                let after = &rest[i..];
                match after.find('\n') {
                    Some(nl) => rest = &after[nl..],
                    None => rest = "",
                }
            }
            out.push_str(rest);
        };
        for i in tok_from..tok_to {
            let t = self.toks[i];
            if t.col < 0 && (t.ty == T::INDENT || t.ty == T::DEDENT) {
                continue;
            }
            let ts = t.start as usize;
            let te = t.end as usize;
            if ts >= pos {
                push_gap(&mut out, &self.src[pos..ts]);
                out.push_str(&self.src[ts..te]);
                pos = te;
            } else if te > pos {
                out.push_str(&self.src[pos..te]);
                pos = te;
            }
        }
        if to > pos {
            push_gap(&mut out, &self.src[pos..to]);
        }
        out
    }

    fn fprefix(tstring: bool) -> char {
        if tstring {
            't'
        } else {
            'f'
        }
    }

    /// fstring_replacement_field: '{' annotated_rhs '='? [conversion] [format_spec] '}'
    fn replacement_field(&mut self, tstring: bool, raw: bool) -> R<Vec<Expr>> {
        let start = self.p;
        let lbrace = self.advance();
        let pc = Self::fprefix(tstring);
        if self.invalid {
            self.invalid_replacement_field(tstring)?;
            self.p = start + 1;
        }
        self.fstring_depth += 1;
        let e = self.annotated_rhs();
        self.fstring_depth -= 1;
        let Some(expr) = e? else {
            self.p = start;
            return Ok(None);
        };
        let expr_end_tok = self.p;
        let mut debug = None;
        if self.at(T::EQUAL) {
            debug = Some(self.advance());
        }
        let after_debug_tok = self.p;
        let mut conversion: Option<(Token, Token)> = None;
        if self.at(T::EXCLAMATION) {
            let m = self.p;
            let bang = self.advance();
            let nt = self.peek();
            if nt.ty == T::NAME && !nt.kw.is_hard() {
                self.advance();
                // check_fstring_conversion
                if bang.lineno != nt.lineno || bang.end_col != nt.col {
                    return Err(self.err_range(
                        format!("{}-string: conversion type must come right after the exclamation mark", pc),
                        Self::tok_loc(&bang),
                        Self::tok_loc(&nt),
                    ));
                }
                conversion = Some((bang, nt));
            } else {
                self.p = m;
            }
        }
        let mut format_spec: Option<Expr> = None;
        if self.at(T::COLON) {
            let colon = self.advance();
            let cstart = self.p - 1;
            let mid = if tstring { T::TSTRING_MIDDLE } else { T::FSTRING_MIDDLE };
            let mut spec = Vec::new();
            loop {
                let t = self.peek();
                if t.ty == mid {
                    self.advance();
                    let s = if raw {
                        PyStr::Utf8(self.text(&t).to_string())
                    } else {
                        self.decode_fstring_middle(&t, false)?
                    };
                    if !s.is_empty() {
                        spec.push(Expr::new(ExprKind::Constant { value: Constant::Str(s), kind: None }, Self::tok_loc(&t)));
                    }
                    continue;
                }
                if t.ty == T::LBRACE {
                    let m = self.p;
                    match self.replacement_field(tstring, raw)? {
                        Some(v) => {
                            // adjacent literal text (e.g. the debug text of a nested
                            // `{y=}`) is merged into one Constant, like CPython
                            for e in v {
                                if let (Some(Expr { node: ExprKind::Constant { value: Constant::Str(prev), .. }, loc: ploc }), ExprKind::Constant { value: Constant::Str(cur), .. }) =
                                    (spec.last_mut(), &e.node)
                                {
                                    prev.push_pystr(cur);
                                    ploc.end_lineno = e.loc.end_lineno;
                                    ploc.end_col_offset = e.loc.end_col_offset;
                                    continue;
                                }
                                spec.push(e);
                            }
                            continue;
                        }
                        None => {
                            self.p = m;
                            break;
                        }
                    }
                }
                break;
            }
            let (sl, sc) = self.start_pos(cstart);
            let loc = if spec.is_empty() {
                Loc { lineno: sl, col_offset: sc, end_lineno: colon.end_lineno as i32, end_col_offset: colon.end_col }
            } else if spec.len() == 1 && matches!(spec[0].node, ExprKind::Constant { .. }) {
                Loc { lineno: sl, col_offset: sc, end_lineno: spec[0].loc.end_lineno, end_col_offset: spec[0].loc.end_col_offset }
            } else {
                self.loc(cstart)
            };
            format_spec = Some(Expr::new(ExprKind::JoinedStr { values: spec }, loc));
        }
        let Some(rbrace) = self.eat(T::RBRACE) else {
            if self.invalid {
                self.p = start;
                self.invalid_replacement_field(tstring)?;
            }
            self.p = start;
            return Ok(None);
        };
        // conversion value
        let conv: i32 = if let Some((_, nt)) = &conversion {
            let name = self.text(nt);
            let mut chars = name.chars();
            let first = chars.next().unwrap();
            if chars.next().is_some() || !matches!(first, 's' | 'r' | 'a') {
                return Err(self.err_tok(format!("{}-string: invalid conversion character '{}': expected 's', 'r', or 'a'", pc, name), nt));
            }
            first as i32
        } else if debug.is_some() && format_spec.is_none() {
            'r' as i32
        } else {
            -1
        };
        let loc = self.loc(start);
        let fspec = format_spec.map(bx);
        let delim_tok = self.tok_at(after_debug_tok);
        let node = if tstring {
            let s = self.expr_text(lbrace.end as usize, start + 1, expr_end_tok, self.tok_at(expr_end_tok).start as usize);
            ExprKind::Interpolation { value: bx(expr), str: PyStr::Utf8(s), conversion: conv, format_spec: fspec }
        } else {
            ExprKind::FormattedValue { value: bx(expr), conversion: conv, format_spec: fspec }
        };
        let fv = Expr::new(node, loc);
        if debug.is_some() {
            // debug text: from after '{' up to the conversion '!', the ':' or the '}'
            let text = self.expr_text(lbrace.end as usize, start + 1, after_debug_tok, delim_tok.start as usize);
            let (dl, dc) = (delim_tok.lineno as i32, delim_tok.col);
            let dloc = Loc { lineno: lbrace.lineno as i32, col_offset: lbrace.col + 1, end_lineno: dl, end_col_offset: dc };
            let _ = rbrace;
            let c = Expr::new(ExprKind::Constant { value: Constant::Str(PyStr::Utf8(text)), kind: None }, dloc);
            return Ok(Some(vec![c, fv]));
        }
        Ok(Some(vec![fv]))
    }

    /// invalid_replacement_field (second pass). `self.p` is right after '{'.
    fn invalid_replacement_field(&mut self, tstring: bool) -> Result<(), PErr> {
        let pc = Self::fprefix(tstring);
        let after = self.p;
        let t = self.peek();
        let before = |s: &str| format!("{}-string: valid expression required before '{}'", pc, s);
        match t.ty {
            T::EQUAL => return Err(self.err_tok(before("="), &t)),
            T::EXCLAMATION => return Err(self.err_tok(before("!"), &t)),
            T::COLON => return Err(self.err_tok(before(":"), &t)),
            T::RBRACE => return Err(self.err_tok(before("}"), &t)),
            _ => {}
        }
        let e = self.annotated_rhs()?;
        if e.is_none() {
            self.p = after;
            return Err(self.err_next(format!("{}-string: expecting a valid expression after '{{'", pc)));
        }
        let nt = self.peek_ty();
        if !matches!(nt, T::EQUAL | T::EXCLAMATION | T::COLON | T::RBRACE) {
            return Err(self.err_next(format!("{}-string: expecting '=', or '!', or ':', or '}}'", pc)));
        }
        if nt == T::EQUAL {
            self.advance();
            if !matches!(self.peek_ty(), T::EXCLAMATION | T::COLON | T::RBRACE) {
                return Err(self.err_next(format!("{}-string: expecting '!', or ':', or '}}'", pc)));
            }
        }
        if self.at(T::EXCLAMATION) {
            self.advance();
            if matches!(self.peek_ty(), T::COLON | T::RBRACE) {
                return Err(self.err_next(format!("{}-string: missing conversion character", pc)));
            }
            if !self.at_name() {
                return Err(self.err_next(format!("{}-string: invalid conversion character", pc)));
            }
            self.advance();
        }
        if !matches!(self.peek_ty(), T::COLON | T::RBRACE) {
            return Err(self.err_next(format!("{}-string: expecting ':' or '}}'", pc)));
        }
        if self.at(T::COLON) {
            self.advance();
            let mid = if tstring { T::TSTRING_MIDDLE } else { T::FSTRING_MIDDLE };
            loop {
                if self.at(mid) {
                    self.advance();
                    continue;
                }
                if self.at(T::LBRACE) {
                    let m = self.p;
                    if self.replacement_field(tstring, false)?.is_some() {
                        continue;
                    }
                    self.p = m;
                }
                break;
            }
            if !self.at(T::RBRACE) {
                return Err(self.err_next(format!("{}-string: expecting '}}', or format specs", pc)));
            }
        }
        if !self.at(T::RBRACE) {
            return Err(self.err_next(format!("{}-string: expecting '}}'", pc)));
        }
        self.p = after;
        Ok(())
    }
}
