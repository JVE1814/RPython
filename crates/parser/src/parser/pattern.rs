//! `match` statements and patterns.

use super::expr::bx;
use super::*;
use TokenType as T;

impl<'a> Parser<'a> {
    pub(crate) fn match_stmt(&mut self) -> R<Stmt> {
        let start = self.p;
        let mt = self.advance(); // "match"
        let Some(subject) = self.subject_expr()? else {
            self.p = start;
            return Ok(None);
        };
        if self.at(T::NEWLINE) && self.invalid {
            // "match" subject_expr NEWLINE -> expected ':'
            self.peek();
            return Err(self.err_last(ErrorKind::Syntax, "expected ':'"));
        }
        if self.eat(T::COLON).is_none() {
            self.p = start;
            return Ok(None);
        }
        if self.eat(T::NEWLINE).is_none() {
            self.p = start;
            return Ok(None);
        }
        if self.eat(T::INDENT).is_none() {
            if self.invalid {
                self.peek();
                return Err(self.err_last(ErrorKind::Indentation, format!("expected an indented block after 'match' statement on line {}", mt.lineno)));
            }
            self.p = start;
            return Ok(None);
        }
        let mut cases = Vec::new();
        while self.at_kw(Kw::SoftCase) {
            let m = self.p;
            match self.case_block()? {
                Some(c) => cases.push(c),
                None => {
                    self.p = m;
                    break;
                }
            }
        }
        if cases.is_empty() || self.eat(T::DEDENT).is_none() {
            self.p = start;
            return Ok(None);
        }
        Ok(Some(Stmt::new(StmtKind::Match { subject: bx(subject), cases }, self.loc(start))))
    }

    fn subject_expr(&mut self) -> R<Expr> {
        let start = self.p;
        if let Some(a) = self.star_named_expression()? {
            if self.at(T::COMMA) {
                let mut elts = vec![a];
                self.advance();
                let m = self.p;
                match self.star_named_expressions()? {
                    Some(mut v) => elts.append(&mut v),
                    None => self.p = m,
                }
                return Ok(Some(Expr::new(ExprKind::Tuple { elts, ctx: ExprContext::Load }, self.loc(start))));
            }
            if !matches!(a.node, ExprKind::Starred { .. }) {
                return Ok(Some(a));
            }
        }
        self.p = start;
        self.named_expression()
    }

    fn case_block(&mut self) -> R<MatchCase> {
        let start = self.p;
        let ct = self.advance(); // "case"
        let Some(pattern) = self.patterns()? else {
            self.p = start;
            return Ok(None);
        };
        let mut guard = None;
        if self.at_kw(Kw::If) {
            self.advance();
            match self.named_expression()? {
                Some(g) => guard = Some(bx(g)),
                None => {
                    self.p = start;
                    return Ok(None);
                }
            }
        }
        if self.invalid && self.at(T::NEWLINE) {
            self.peek();
            return Err(self.err_last(ErrorKind::Syntax, "expected ':'"));
        }
        if self.eat(T::COLON).is_none() {
            self.p = start;
            return Ok(None);
        }
        if self.invalid && self.at(T::NEWLINE) && self.peek_n(1).ty != T::INDENT {
            self.advance();
            self.peek();
            return Err(self.err_last(ErrorKind::Indentation, format!("expected an indented block after 'case' statement on line {}", ct.lineno)));
        }
        let Some(body) = self.block()? else {
            self.p = start;
            return Ok(None);
        };
        Ok(Some(MatchCase { pattern, guard, body }))
    }

    /// patterns: open_sequence_pattern | pattern
    pub(crate) fn patterns(&mut self) -> R<Pattern> {
        let start = self.p;
        if let Some(first) = self.maybe_star_pattern()? {
            if self.at(T::COMMA) {
                let mut v = vec![first];
                self.advance();
                let m = self.p;
                match self.maybe_sequence_pattern()? {
                    Some(mut rest) => v.append(&mut rest),
                    None => self.p = m,
                }
                return Ok(Some(Pattern { node: PatternKind::MatchSequence { patterns: v }, loc: self.loc(start) }));
            }
            if !matches!(first.node, PatternKind::MatchStar { .. }) {
                return Ok(Some(first));
            }
        }
        self.p = start;
        self.pattern()
    }

    /// pattern: as_pattern | or_pattern
    pub(crate) fn pattern(&mut self) -> R<Pattern> {
        let start = self.p;
        self.enter()?;
        let r = self.or_pattern();
        self.leave();
        let Some(p) = r? else { return Ok(None) };
        if self.at_kw(Kw::As) {
            let m = self.p;
            self.advance();
            if let Some(n) = self.pattern_capture_target() {
                let name = self.ident(&n);
                return Ok(Some(Pattern { node: PatternKind::MatchAs { pattern: Some(Box::new(p)), name: Some(name) }, loc: self.loc(start) }));
            }
            if self.invalid {
                // invalid_as_pattern
                let t = self.peek();
                if t.ty == T::NAME && t.kw == Kw::SoftUnderscore {
                    return Err(self.err_tok("cannot use '_' as a target", &t));
                }
                if !(t.ty == T::NAME && !t.kw.is_hard()) {
                    if let Some(e) = self.expression()? {
                        return Err(self.err_loc(format!("cannot use {} as pattern target", super::expr::expr_name(&e)), e.loc));
                    }
                } else {
                    let m2 = self.p;
                    if let Some(e) = self.expression()? {
                        if !matches!(e.node, ExprKind::Name { .. }) {
                            return Err(self.err_loc(format!("cannot use {} as pattern target", super::expr::expr_name(&e)), e.loc));
                        }
                    }
                    self.p = m2;
                }
            }
            self.p = m;
        }
        Ok(Some(p))
    }

    fn or_pattern(&mut self) -> R<Pattern> {
        let start = self.p;
        let Some(first) = self.closed_pattern()? else { return Ok(None) };
        if !self.at(T::VBAR) {
            return Ok(Some(first));
        }
        let mut v = vec![first];
        while self.at(T::VBAR) {
            let m = self.p;
            self.advance();
            match self.closed_pattern()? {
                Some(p) => v.push(p),
                None => {
                    self.p = m;
                    break;
                }
            }
        }
        if v.len() == 1 {
            return Ok(v.pop());
        }
        Ok(Some(Pattern { node: PatternKind::MatchOr { patterns: v }, loc: self.loc(start) }))
    }

    /// pattern_capture_target: !"_" NAME !('.' | '(' | '=')
    fn pattern_capture_target(&mut self) -> Option<Token> {
        let t = self.peek();
        if t.ty != T::NAME || t.kw.is_hard() || t.kw == Kw::SoftUnderscore {
            return None;
        }
        let n = self.peek_n(1).ty;
        if matches!(n, T::DOT | T::LPAR | T::EQUAL) {
            return None;
        }
        self.advance();
        Some(t)
    }

    fn closed_pattern(&mut self) -> R<Pattern> {
        let start = self.p;
        let t = self.peek();
        match t.ty {
            T::NUMBER | T::MINUS | T::PLUS => {
                if let Some(e) = self.literal_number()? {
                    return Ok(Some(Pattern { node: PatternKind::MatchValue { value: bx(e) }, loc: self.loc(start) }));
                }
                self.p = start;
                Ok(None)
            }
            T::STRING | T::FSTRING_START | T::TSTRING_START => {
                let Some(e) = self.strings()? else {
                    self.p = start;
                    return Ok(None);
                };
                Ok(Some(Pattern { node: PatternKind::MatchValue { value: bx(e) }, loc: self.loc(start) }))
            }
            T::NAME => {
                match t.kw {
                    Kw::NoneKw | Kw::True | Kw::False => {
                        self.advance();
                        let c = match t.kw {
                            Kw::NoneKw => Constant::None,
                            Kw::True => Constant::Bool(true),
                            _ => Constant::Bool(false),
                        };
                        return Ok(Some(Pattern { node: PatternKind::MatchSingleton { value: c }, loc: self.loc(start) }));
                    }
                    k if k.is_hard() => return Ok(None),
                    _ => {}
                }
                // capture / wildcard / value / class pattern
                if let Some(n) = self.pattern_capture_target() {
                    let name = self.ident(&n);
                    return Ok(Some(Pattern { node: PatternKind::MatchAs { pattern: None, name: Some(name) }, loc: self.loc(start) }));
                }
                if t.kw == Kw::SoftUnderscore && !matches!(self.peek_n(1).ty, T::DOT | T::LPAR | T::EQUAL) {
                    self.advance();
                    return Ok(Some(Pattern { node: PatternKind::MatchAs { pattern: None, name: None }, loc: self.loc(start) }));
                }
                // name_or_attr
                let Some(cls) = self.name_or_attr()? else {
                    self.p = start;
                    return Ok(None);
                };
                if self.at(T::LPAR) {
                    return self.class_pattern(start, cls);
                }
                if matches!(cls.node, ExprKind::Attribute { .. }) && !matches!(self.peek_ty(), T::DOT | T::EQUAL) {
                    return Ok(Some(Pattern { node: PatternKind::MatchValue { value: bx(cls) }, loc: self.loc(start) }));
                }
                self.p = start;
                Ok(None)
            }
            T::LPAR => {
                self.advance();
                if self.eat(T::RPAR).is_some() {
                    return Ok(Some(Pattern { node: PatternKind::MatchSequence { patterns: Vec::new() }, loc: self.loc(start) }));
                }
                // group_pattern: '(' pattern ')'
                let m = self.p;
                if let Some(p) = self.pattern()? {
                    if self.eat(T::RPAR).is_some() {
                        return Ok(Some(p));
                    }
                }
                self.p = m;
                // '(' open_sequence_pattern ')'
                let Some(first) = self.maybe_star_pattern()? else {
                    self.p = start;
                    return Ok(None);
                };
                if self.eat(T::COMMA).is_none() {
                    self.p = start;
                    return Ok(None);
                }
                let mut v = vec![first];
                let m = self.p;
                match self.maybe_sequence_pattern()? {
                    Some(mut rest) => v.append(&mut rest),
                    None => self.p = m,
                }
                if self.eat(T::RPAR).is_none() {
                    self.p = start;
                    return Ok(None);
                }
                Ok(Some(Pattern { node: PatternKind::MatchSequence { patterns: v }, loc: self.loc(start) }))
            }
            T::LSQB => {
                self.advance();
                let mut v = Vec::new();
                if !self.at(T::RSQB) {
                    match self.maybe_sequence_pattern()? {
                        Some(x) => v = x,
                        None => {
                            self.p = start;
                            return Ok(None);
                        }
                    }
                }
                if self.eat(T::RSQB).is_none() {
                    self.p = start;
                    return Ok(None);
                }
                Ok(Some(Pattern { node: PatternKind::MatchSequence { patterns: v }, loc: self.loc(start) }))
            }
            T::LBRACE => self.mapping_pattern(),
            _ => Ok(None),
        }
    }

    /// signed_number !('+'|'-') | complex_number
    fn literal_number(&mut self) -> R<Expr> {
        let start = self.p;
        let sign = if self.at(T::MINUS) || self.at(T::PLUS) { Some(self.advance().ty) } else { None };
        let neg = sign.is_some();
        let Some(nt) = self.eat(T::NUMBER) else {
            self.p = start;
            return Ok(None);
        };
        let v = self.parse_number(&nt)?;
        let num = Expr::new(ExprKind::Constant { value: v, kind: None }, Self::tok_loc(&nt));
        let real = if neg {
            let op = if sign == Some(T::PLUS) { UnaryOp::UAdd } else { UnaryOp::USub };
            Expr::new(ExprKind::UnaryOp { op, operand: bx(num) }, self.loc(start))
        } else {
            num
        };
        if matches!(self.peek_ty(), T::PLUS | T::MINUS) {
            // complex_number: signed_real_number ('+'|'-') imaginary_number
            let op = if self.peek_ty() == T::PLUS { Operator::Add } else { Operator::Sub };
            self.advance();
            let Some(it) = self.eat(T::NUMBER) else {
                self.p = start;
                return Ok(None);
            };
            // ensure real
            let real_is_complex = {
                let inner = match &real.node {
                    ExprKind::UnaryOp { operand, .. } => operand.as_ref(),
                    _ => &real,
                };
                matches!(inner.node, ExprKind::Constant { value: Constant::Complex { .. }, .. })
            };
            if real_is_complex {
                return Err(self.err_loc("real number required in complex literal", real.loc));
            }
            let iv = self.parse_number(&it)?;
            if !matches!(iv, Constant::Complex { .. }) {
                return Err(self.err_tok("imaginary number required in complex literal", &it));
            }
            let imag = Expr::new(ExprKind::Constant { value: iv, kind: None }, Self::tok_loc(&it));
            return Ok(Some(Expr::new(ExprKind::BinOp { left: bx(real), op, right: bx(imag) }, self.loc(start))));
        }
        Ok(Some(real))
    }

    fn name_or_attr(&mut self) -> R<Expr> {
        let start = self.p;
        let Some(n) = self.eat_name() else { return Ok(None) };
        let mut e = self.mk_name(&n, ExprContext::Load);
        while self.at(T::DOT) {
            let m = self.p;
            self.advance();
            match self.eat_name() {
                Some(a) => {
                    let attr = self.ident(&a);
                    e = Expr::new(ExprKind::Attribute { value: bx(e), attr, ctx: ExprContext::Load }, self.loc(start));
                }
                None => {
                    self.p = m;
                    break;
                }
            }
        }
        Ok(Some(e))
    }

    fn maybe_star_pattern(&mut self) -> R<Pattern> {
        let start = self.p;
        if self.at(T::STAR) {
            self.advance();
            if let Some(n) = self.pattern_capture_target() {
                let name = self.ident(&n);
                return Ok(Some(Pattern { node: PatternKind::MatchStar { name: Some(name) }, loc: self.loc(start) }));
            }
            let t = self.peek();
            if t.ty == T::NAME && t.kw == Kw::SoftUnderscore && !matches!(self.peek_n(1).ty, T::DOT | T::LPAR | T::EQUAL) {
                self.advance();
                return Ok(Some(Pattern { node: PatternKind::MatchStar { name: None }, loc: self.loc(start) }));
            }
            self.p = start;
            return Ok(None);
        }
        self.pattern()
    }

    /// ','.maybe_star_pattern+ ','?
    fn maybe_sequence_pattern(&mut self) -> R<Vec<Pattern>> {
        let Some(first) = self.maybe_star_pattern()? else { return Ok(None) };
        let mut v = vec![first];
        while self.at(T::COMMA) {
            let m = self.p;
            self.advance();
            match self.maybe_star_pattern()? {
                Some(p) => v.push(p),
                None => {
                    self.p = m + 1;
                    break;
                }
            }
        }
        Ok(Some(v))
    }

    fn mapping_key(&mut self) -> R<Expr> {
        let t = self.peek();
        match t.ty {
            T::NUMBER | T::MINUS | T::PLUS => self.literal_number(),
            T::STRING | T::FSTRING_START | T::TSTRING_START => self.strings(),
            T::NAME => match t.kw {
                Kw::NoneKw | Kw::True | Kw::False => self.atom(),
                k if k.is_hard() => Ok(None),
                _ => {
                    let start = self.p;
                    let Some(e) = self.name_or_attr()? else { return Ok(None) };
                    if matches!(e.node, ExprKind::Attribute { .. }) {
                        return Ok(Some(e));
                    }
                    self.p = start;
                    Ok(None)
                }
            },
            _ => Ok(None),
        }
    }

    fn mapping_pattern(&mut self) -> R<Pattern> {
        let start = self.p;
        self.advance(); // '{'
        let mut keys = Vec::new();
        let mut patterns = Vec::new();
        let mut rest = None;
        loop {
            if self.at(T::RBRACE) {
                break;
            }
            if self.at(T::DOUBLESTAR) {
                self.advance();
                let Some(n) = self.pattern_capture_target() else {
                    self.p = start;
                    return Ok(None);
                };
                if self.at(T::COMMA) {
                    self.advance();
                }
                if !self.at(T::RBRACE) {
                    if self.invalid {
                        // double star pattern followed by more items
                        let m = self.p;
                        if self.mapping_key()?.is_some() {
                            return Err(self.err_tok("double star pattern must be the last (right-most) subpattern in the mapping pattern", &n));
                        }
                        self.p = m;
                    }
                    self.p = start;
                    return Ok(None);
                }
                rest = Some(self.ident(&n));
                break;
            }
            let Some(k) = self.mapping_key()? else {
                self.p = start;
                return Ok(None);
            };
            if self.eat(T::COLON).is_none() {
                self.p = start;
                return Ok(None);
            }
            let Some(p) = self.pattern()? else {
                self.p = start;
                return Ok(None);
            };
            keys.push(k);
            patterns.push(p);
            if self.eat(T::COMMA).is_none() {
                break;
            }
        }
        if self.eat(T::RBRACE).is_none() {
            self.p = start;
            return Ok(None);
        }
        Ok(Some(Pattern { node: PatternKind::MatchMapping { keys, patterns, rest }, loc: self.loc(start) }))
    }

    fn class_pattern(&mut self, start: usize, cls: Expr) -> R<Pattern> {
        self.advance(); // '('
        let mut patterns = Vec::new();
        let mut kwd_attrs = Vec::new();
        let mut kwd_patterns: Vec<Pattern> = Vec::new();
        loop {
            if self.at(T::RPAR) {
                break;
            }
            let t = self.peek();
            if t.ty == T::NAME && !t.kw.is_hard() && self.peek_n(1).ty == T::EQUAL {
                self.advance();
                self.advance();
                let Some(p) = self.pattern()? else {
                    self.p = start;
                    return Ok(None);
                };
                kwd_attrs.push(self.ident(&t));
                kwd_patterns.push(p);
            } else {
                let Some(p) = self.pattern()? else {
                    self.p = start;
                    return Ok(None);
                };
                if !kwd_patterns.is_empty() {
                    if self.invalid {
                        // positional patterns follow keyword patterns: range over the
                        // trailing positional patterns
                        let first = p.loc;
                        let mut last = p.loc;
                        while self.at(T::COMMA) {
                            let m = self.p;
                            self.advance();
                            let tt = self.peek();
                            if tt.ty == T::NAME && self.peek_n(1).ty == T::EQUAL {
                                self.p = m;
                                break;
                            }
                            match self.pattern()? {
                                Some(q) => last = q.loc,
                                None => {
                                    self.p = m;
                                    break;
                                }
                            }
                        }
                        return Err(self.err_range("positional patterns follow keyword patterns", first, last));
                    }
                    self.p = start;
                    return Ok(None);
                }
                patterns.push(p);
            }
            if self.eat(T::COMMA).is_none() {
                break;
            }
        }
        if self.eat(T::RPAR).is_none() {
            self.p = start;
            return Ok(None);
        }
        Ok(Some(Pattern { node: PatternKind::MatchClass { cls: bx(cls), patterns, kwd_attrs, kwd_patterns }, loc: self.loc(start) }))
    }
}
