//! `invalid_*` rules: only active in the second pass; they raise specific SyntaxErrors.
//! Each function restores nothing itself on success-without-error; callers reset `self.p`.

use super::expr::expr_name;
use super::*;
use TokenType as T;

fn is_legacy(e: &Expr) -> Option<&str> {
    match &e.node {
        ExprKind::Name { id, .. } if id == "print" || id == "exec" => Some(id),
        _ => None,
    }
}

impl<'a> Parser<'a> {
    /// expression parsed with invalid rules disabled (`expression_without_invalid`).
    fn expression_without_invalid(&mut self) -> R<Expr> {
        let save = self.invalid;
        self.invalid = false;
        let r = self.expression();
        self.invalid = save;
        r
    }

    fn disjunction_noinv(&mut self) -> R<Expr> {
        let save = self.invalid;
        self.invalid = false;
        let r = self.disjunction();
        self.invalid = save;
        r
    }

    pub(crate) fn invalid_expression(&mut self) -> Result<(), PErr> {
        let start = self.p;
        let t = self.peek();
        // !(NAME STRING | SOFT_KEYWORD) a=disjunction b=expression_without_invalid
        let skip = (t.ty == T::NAME && !t.kw.is_hard() && matches!(self.peek_n(1).ty, T::STRING | T::FSTRING_START | T::TSTRING_START))
            || (t.ty == T::NAME && t.kw.is_soft());
        if !skip {
            if let Some(a) = self.disjunction_noinv()? {
                let bstart = self.p;
                if let Some(b) = self.expression_without_invalid()? {
                    let level = if self.p > 0 { self.toks[self.p - 1].level } else { 0 };
                    let a_is_str = matches!(self.toks[start].ty, T::STRING | T::FSTRING_START | T::TSTRING_START)
                        && matches!(a.node, ExprKind::Constant { .. } | ExprKind::JoinedStr { .. } | ExprKind::TemplateStr { .. });
                    if is_legacy(&a).is_none() {
                        if level != 0 {
                            // gh-142236: for implicit string concatenation point at the last string
                            let mut aloc = a.loc;
                            if a_is_str {
                                let mut i = start;
                                let mut last = start;
                                let mut pieces = 0;
                                while i < bstart {
                                    match self.toks[i].ty {
                                        T::STRING => {
                                            last = i;
                                            pieces += 1;
                                            i += 1;
                                        }
                                        T::FSTRING_START | T::TSTRING_START => {
                                            last = i;
                                            pieces += 1;
                                            let mut d = 0;
                                            while i < bstart {
                                                match self.toks[i].ty {
                                                    T::FSTRING_START | T::TSTRING_START => d += 1,
                                                    T::FSTRING_END | T::TSTRING_END => d -= 1,
                                                    _ => {}
                                                }
                                                i += 1;
                                                if d == 0 {
                                                    break;
                                                }
                                            }
                                        }
                                        _ => break,
                                    }
                                }
                                if pieces > 1 {
                                    let lt = self.toks[last];
                                    aloc.lineno = lt.lineno as i32;
                                    aloc.col_offset = lt.col;
                                }
                            }
                            return Err(self.err_range("invalid syntax. Perhaps you forgot a comma?", aloc, b.loc));
                        }
                        if a_is_str && self.toks[bstart].ty == T::NAME {
                            return Err(self.err_range("invalid syntax. Is this intended to be part of the string?", a.loc, b.loc));
                        }
                    }
                }
            }
        }
        self.p = start;
        // a=disjunction 'if' b=disjunction !('else'|':')
        if let Some(a) = self.disjunction_noinv()? {
            if self.eat_kw(Kw::If).is_some() {
                if let Some(b) = self.disjunction_noinv()? {
                    if !self.at_kw(Kw::Else) && !self.at(T::COLON) {
                        return Err(self.err_range("expected 'else' after 'if' expression", a.loc, b.loc));
                    }
                    if self.eat_kw(Kw::Else).is_some() {
                        let st = self.peek();
                        if st.ty == T::STAR {
                            return Err(self.err_tok("cannot unpack only part of a conditional expression", &st));
                        }
                        if st.ty == T::DOUBLESTAR {
                            return Err(self.err_tok("cannot use dict unpacking on only part of a conditional expression", &st));
                        }
                        let m = self.p;
                        let ok = self.expression_without_invalid()?.is_some();
                        self.p = m;
                        if !ok && self.at_simple_stmt_keyword() {
                            return Err(self.err_next("expected expression after 'else', but statement is given"));
                        }
                    }
                }
            }
        }
        self.p = start;
        // (pass|break|continue) 'if' disjunction 'else' simple_stmt
        let t = self.peek();
        if t.ty == T::NAME && matches!(t.kw, Kw::Pass | Kw::Break | Kw::Continue) {
            self.advance();
            if self.eat_kw(Kw::If).is_some() {
                if let Some(b) = self.disjunction_noinv()? {
                    if self.eat_kw(Kw::Else).is_some() {
                        let ok = self.at_simple_stmt_start();
                        if ok {
                            return Err(self.err_range("expected expression before 'if', but statement is given", Self::tok_loc(&t), b.loc));
                        }
                    }
                }
            }
        }
        self.p = start;
        // 'lambda' [lambda_params] ':' &FSTRING_MIDDLE
        if self.at_kw(Kw::Lambda) {
            let lt = self.advance();
            if !self.at(T::COLON) {
                let save = self.invalid;
                self.invalid = false;
                let _ = self.lambda_params();
                self.invalid = save;
            }
            if let Some(colon) = self.eat(T::COLON) {
                let nt = self.peek_ty();
                if nt == T::FSTRING_MIDDLE {
                    return Err(self.err_range("f-string: lambda expressions are not allowed without parentheses", Self::tok_loc(&lt), Self::tok_loc(&colon)));
                }
                if nt == T::TSTRING_MIDDLE {
                    return Err(self.err_range("t-string: lambda expressions are not allowed without parentheses", Self::tok_loc(&lt), Self::tok_loc(&colon)));
                }
            }
        }
        self.p = start;
        Ok(())
    }

    fn at_simple_stmt_keyword(&mut self) -> bool {
        let t = self.peek();
        t.ty == T::NAME
            && matches!(
                t.kw,
                Kw::Pass | Kw::Break | Kw::Continue | Kw::Return | Kw::Raise | Kw::Del | Kw::Import | Kw::From | Kw::Global | Kw::Nonlocal | Kw::Assert
            )
    }

    fn at_simple_stmt_start(&mut self) -> bool {
        if self.at_simple_stmt_keyword() {
            return true;
        }
        let m = self.p;
        let r = matches!(self.simple_stmt_probe(), Ok(true));
        self.p = m;
        r
    }

    fn simple_stmt_probe(&mut self) -> Result<bool, PErr> {
        let save = self.invalid;
        self.invalid = false;
        let r = self.star_expressions();
        self.invalid = save;
        Ok(r?.is_some())
    }

    pub(crate) fn invalid_legacy_expression(&mut self) -> Result<(), PErr> {
        let start = self.p;
        let t = self.peek();
        if t.ty == T::NAME && !t.kw.is_hard() && self.peek_n(1).ty != T::LPAR {
            let name = self.text(&t);
            if name == "print" || name == "exec" {
                self.advance();
                let save = self.invalid;
                self.invalid = false;
                let b = self.star_expressions();
                self.invalid = save;
                if let Some(b) = b? {
                    return Err(self.err_range(format!("Missing parentheses in call to '{}'. Did you mean {}(...)?", name, name), Self::tok_loc(&t), b.loc));
                }
            }
        }
        self.p = start;
        Ok(())
    }

    pub(crate) fn invalid_named_expression(&mut self) -> Result<(), PErr> {
        let start = self.p;
        // a=expression ':=' expression
        let save = self.invalid;
        self.invalid = false;
        let a = self.expression()?;
        self.invalid = save;
        if let Some(a) = a {
            if self.eat(T::COLONEQUAL).is_some() {
                let save = self.invalid;
                self.invalid = false;
                let b = self.expression()?;
                self.invalid = save;
                if b.is_some() {
                    return Err(self.err_loc(format!("cannot use assignment expressions with {}", expr_name(&a)), a.loc));
                }
            }
        }
        self.p = start;
        // a=NAME '=' b=bitwise_or !('='|':=')
        let t = self.peek();
        if t.ty == T::NAME && !t.kw.is_hard() && self.peek_n(1).ty == T::EQUAL {
            self.advance();
            self.advance();
            let save = self.invalid;
            self.invalid = false;
            let b = self.bitwise_or()?;
            self.invalid = save;
            if let Some(b) = b {
                if !matches!(self.peek_ty(), T::EQUAL | T::COLONEQUAL) {
                    return Err(self.err_range("invalid syntax. Maybe you meant '==' or ':=' instead of '='?", Self::tok_loc(&t), b.loc));
                }
            }
        }
        self.p = start;
        // !(list|tuple|genexp|'True'|'None'|'False') a=bitwise_or b='=' bitwise_or !('='|':=')
        let t = self.peek();
        let excluded = matches!(t.ty, T::LSQB | T::LPAR) || (t.ty == T::NAME && matches!(t.kw, Kw::True | Kw::NoneKw | Kw::False));
        let excluded = if excluded && matches!(t.ty, T::LSQB | T::LPAR) {
            // only excluded if it actually parses as list/tuple/genexp
            let save = self.invalid;
            self.invalid = false;
            let a = self.atom()?;
            self.invalid = save;
            self.p = start;
            matches!(a.map(|e| e.node), Some(ExprKind::List { .. }) | Some(ExprKind::Tuple { .. }) | Some(ExprKind::GeneratorExp { .. }) | Some(ExprKind::ListComp { .. }))
                && !matches!(t.ty, T::LSQB) || matches!(t.ty, T::LSQB) && {
                    let save = self.invalid;
                    self.invalid = false;
                    let a = self.atom()?;
                    self.invalid = save;
                    self.p = start;
                    matches!(a.map(|e| e.node), Some(ExprKind::List { .. }))
                }
        } else {
            excluded
        };
        if !excluded {
            let save = self.invalid;
            self.invalid = false;
            let a = self.bitwise_or()?;
            self.invalid = save;
            if let Some(a) = a {
                if self.eat(T::EQUAL).is_some() {
                    let save = self.invalid;
                    self.invalid = false;
                    let b = self.bitwise_or()?;
                    self.invalid = save;
                    if b.is_some() && !matches!(self.peek_ty(), T::EQUAL | T::COLONEQUAL) {
                        return Err(self.err_loc(format!("cannot assign to {} here. Maybe you meant '==' instead of '='?", expr_name(&a)), a.loc));
                    }
                }
            }
        }
        self.p = start;
        Ok(())
    }

    /// invalid_assignment (tried after the valid assignment alternatives failed).
    pub(crate) fn invalid_assignment(&mut self) -> Result<(), PErr> {
        let start = self.p;
        // a=invalid_ann_assign_target ':' expression
        {
            let mut depth = 0;
            while self.at(T::LPAR) && {
                // '(' invalid_ann_assign_target ')'
                let n = self.peek_n(1).ty;
                n == T::LPAR || n == T::LSQB
            } {
                self.advance();
                depth += 1;
            }
            let t = self.peek();
            if t.ty == T::LPAR || t.ty == T::LSQB {
                let save = self.invalid;
                self.invalid = false;
                let a = self.atom()?;
                self.invalid = save;
                if let Some(a) = a {
                    if matches!(a.node, ExprKind::List { .. } | ExprKind::Tuple { .. }) {
                        let mut ok = true;
                        for _ in 0..depth {
                            if self.eat(T::RPAR).is_none() {
                                ok = false;
                                break;
                            }
                        }
                        if ok && self.eat(T::COLON).is_some() {
                            let save = self.invalid;
                            self.invalid = false;
                            let e = self.expression()?;
                            self.invalid = save;
                            if e.is_some() {
                                return Err(self.err_loc(format!("only single target (not {}) can be annotated", expr_name(&a)), a.loc));
                            }
                        }
                    }
                }
            }
            self.p = start;
        }
        // a=star_named_expression ',' star_named_expressions* ':' expression
        if let Some(a) = self.star_named_expression()? {
            if self.at(T::COMMA) {
                self.advance();
                loop {
                    let m = self.p;
                    let save = self.invalid;
                    self.invalid = false;
                    let r = self.star_named_expressions();
                    self.invalid = save;
                    if r?.is_none() {
                        self.p = m;
                        break;
                    }
                }
                if self.eat(T::COLON).is_some() {
                    let save = self.invalid;
                    self.invalid = false;
                    let e = self.expression()?;
                    self.invalid = save;
                    if e.is_some() {
                        return Err(self.err_loc("only single target (not tuple) can be annotated", a.loc));
                    }
                }
            }
        }
        self.p = start;
        // a=expression ':' expression
        let save = self.invalid;
        self.invalid = false;
        let a = self.expression()?;
        self.invalid = save;
        if let Some(a) = a {
            if self.eat(T::COLON).is_some() {
                let save = self.invalid;
                self.invalid = false;
                let e = self.expression()?;
                self.invalid = save;
                if e.is_some() {
                    return Err(self.err_loc("illegal target for annotation", a.loc));
                }
            }
        }
        self.p = start;
        // (star_targets '=')* a=star_expressions '='
        let skip_targets = |s: &mut Self| -> Result<(), PErr> {
            loop {
                let m = s.p;
                let save = s.invalid;
                s.invalid = false;
                let t = s.star_targets();
                s.invalid = save;
                if t?.is_some() && s.at(T::EQUAL) {
                    s.advance();
                    continue;
                }
                s.p = m;
                break;
            }
            Ok(())
        };
        skip_targets(self)?;
        let m = self.p;
        if let Some(a) = self.star_expressions()? {
            if self.at(T::EQUAL) {
                if let Some(e) = self.invalid_target_error(TargetsType::Star, &a) {
                    return Err(e);
                }
            }
        }
        self.p = m;
        // (star_targets '=')* a=yield_expr '='
        if self.at_kw(Kw::Yield) {
            if let Some(a) = self.yield_expr()? {
                if self.at(T::EQUAL) {
                    return Err(self.err_loc("assignment to yield expression not possible", a.loc));
                }
            }
        }
        self.p = start;
        // a=star_expressions augassign annotated_rhs
        let save = self.invalid;
        self.invalid = false;
        let a = self.star_expressions()?;
        self.invalid = save;
        if let Some(a) = a {
            let t = self.peek_ty();
            if matches!(
                t,
                T::PLUSEQUAL
                    | T::MINEQUAL
                    | T::STAREQUAL
                    | T::ATEQUAL
                    | T::SLASHEQUAL
                    | T::PERCENTEQUAL
                    | T::AMPEREQUAL
                    | T::VBAREQUAL
                    | T::CIRCUMFLEXEQUAL
                    | T::LEFTSHIFTEQUAL
                    | T::RIGHTSHIFTEQUAL
                    | T::DOUBLESTAREQUAL
                    | T::DOUBLESLASHEQUAL
            ) {
                self.advance();
                let save = self.invalid;
                self.invalid = false;
                let r = self.annotated_rhs()?;
                self.invalid = save;
                if r.is_some() {
                    return Err(self.err_loc(format!("'{}' is an illegal expression for augmented assignment", expr_name(&a)), a.loc));
                }
            }
        }
        self.p = start;
        Ok(())
    }

    pub(crate) fn invalid_group(&mut self) -> Result<(), PErr> {
        let start = self.p;
        if self.eat(T::LPAR).is_none() {
            return Ok(());
        }
        if self.at(T::STAR) {
            let save = self.invalid;
            self.invalid = false;
            let a = self.starred_expression()?;
            self.invalid = save;
            if let Some(a) = a {
                if self.at(T::RPAR) {
                    return Err(self.err_loc("cannot use starred expression here", a.loc));
                }
            }
        } else if self.at(T::DOUBLESTAR) {
            let a = self.advance();
            let save = self.invalid;
            self.invalid = false;
            let e = self.expression()?;
            self.invalid = save;
            if e.is_some() && self.at(T::RPAR) {
                return Err(self.err_tok("cannot use dict unpacking here", &a));
            }
        }
        self.p = start;
        Ok(())
    }

    pub(crate) fn invalid_double_starred_kvpairs(&mut self) -> Result<(), PErr> {
        let start = self.p;
        if self.eat(T::LBRACE).is_none() {
            return Ok(());
        }
        let save = self.invalid;
        self.invalid = false;
        let r = self.invalid_kvpairs_inner();
        self.invalid = save;
        r?;
        self.p = start;
        Ok(())
    }

    fn invalid_kvpairs_inner(&mut self) -> Result<(), PErr> {
        let mut first = true;
        loop {
            // double_starred_kvpair or invalid_kvpair
            if self.at(T::DOUBLESTAR) {
                self.advance();
                if self.bitwise_or()?.is_none() {
                    return Ok(());
                }
            } else {
                let Some(a) = self.expression()? else { return Ok(()) };
                if !self.at(T::COLON) {
                    if first {
                        return Ok(());
                    }
                    // invalid_kvpair: a=expression !(':')
                    return Err(self.error_at(
                        ErrorKind::Syntax,
                        "':' expected after dictionary key",
                        a.loc.lineno,
                        a.loc.end_col_offset - 1,
                        a.loc.end_lineno,
                        -1,
                    ));
                }
                let colon = self.advance();
                if self.at(T::STAR) {
                    let st = self.peek();
                    let m = self.p;
                    self.advance();
                    if self.bitwise_or()?.is_some() {
                        return Err(self.error_at(ErrorKind::Syntax, "cannot use a starred expression in a dictionary value", st.lineno as i32, st.col, -1, -1));
                    }
                    self.p = m;
                }
                if matches!(self.peek_ty(), T::RBRACE | T::COMMA) {
                    return Err(self.err_tok("expression expected after dictionary key and ':'", &colon));
                }
                if self.expression()?.is_none() {
                    return Ok(());
                }
            }
            first = false;
            if self.eat(T::COMMA).is_none() {
                return Ok(());
            }
        }
    }

    /// invalid_kwarg at an argument position.
    pub(crate) fn invalid_kwarg(&mut self, istart: usize) -> Result<(), PErr> {
        self.p = istart;
        let t = self.peek();
        // ('True'|'False'|'None') '='
        if t.ty == T::NAME && matches!(t.kw, Kw::True | Kw::False | Kw::NoneKw) && self.peek_n(1).ty == T::EQUAL {
            let eq = self.peek_n(1);
            return Err(self.err_range(format!("cannot assign to {}", self.text(&t)), Self::tok_loc(&t), Self::tok_loc(&eq)));
        }
        // NAME '=' expression for_if_clauses
        if t.ty == T::NAME && !t.kw.is_hard() && self.peek_n(1).ty == T::EQUAL {
            let eq = self.peek_n(1);
            self.advance();
            self.advance();
            let save = self.invalid;
            self.invalid = false;
            let e = self.expression()?;
            let fc = if e.is_some() && self.at_comprehension_start() { self.for_if_clauses()? } else { None };
            self.invalid = save;
            if fc.is_some() {
                return Err(self.err_range("invalid syntax. Maybe you meant '==' or ':=' instead of '='?", Self::tok_loc(&t), Self::tok_loc(&eq)));
            }
            self.p = istart;
            return Ok(());
        }
        // !(NAME '=') a=expression b='='
        if t.ty != T::DOUBLESTAR {
            let save = self.invalid;
            self.invalid = false;
            let a = self.expression()?;
            self.invalid = save;
            if let Some(a) = a {
                if let Some(b) = self.eat(T::EQUAL) {
                    return Err(self.err_range("expression cannot contain assignment, perhaps you meant \"==\"?", a.loc, Self::tok_loc(&b)));
                }
            }
        } else {
            // '**' expression '=' b=expression
            let st = self.advance();
            let save = self.invalid;
            self.invalid = false;
            let a = self.expression()?;
            if a.is_some() && self.eat(T::EQUAL).is_some() {
                if let Some(b) = self.expression()? {
                    self.invalid = save;
                    return Err(self.err_range("cannot assign to keyword argument unpacking", Self::tok_loc(&st), b.loc));
                }
            }
            self.invalid = save;
        }
        self.p = istart;
        Ok(())
    }

    /// invalid_arguments, procedurally. `lpar` is the index of '('.
    pub(crate) fn invalid_arguments_at(&mut self, lpar: usize) -> Result<(), PErr> {
        let save_inv = self.invalid;
        self.invalid = false;
        let r = self.invalid_arguments_inner(lpar);
        self.invalid = save_inv;
        r
    }

    fn invalid_arguments_inner(&mut self, lpar: usize) -> Result<(), PErr> {
        #[derive(Clone)]
        struct Item {
            kind: u8, // 0 positional, 1 starred, 2 keyword, 3 doublestar
            loc: Loc,
            name_tok: Option<Token>,
            eq_tok: Option<Token>,
            genexp_end: Option<Loc>,
            comma_before: Option<Token>,
        }
        self.p = lpar + 1;
        let mut items: Vec<Item> = Vec::new();
        let mut comma_before: Option<Token> = None;
        let mut followed_comma: Vec<bool> = Vec::new();
        loop {
            if self.at(T::RPAR) {
                break;
            }
            let istart = self.p;
            let t = self.peek();
            let mut it = Item { kind: 0, loc: Self::tok_loc(&t), name_tok: None, eq_tok: None, genexp_end: None, comma_before };
            if t.ty == T::STAR {
                self.advance();
                let Some(_) = self.expression()? else { break };
                it.kind = 1;
                it.loc = self.loc(istart);
            } else if t.ty == T::DOUBLESTAR {
                self.advance();
                let Some(_) = self.expression()? else { break };
                it.kind = 3;
                it.loc = self.loc(istart);
            } else if t.ty == T::NAME && !t.kw.is_hard() && self.peek_n(1).ty == T::EQUAL {
                let eq = self.peek_n(1);
                self.advance();
                self.advance();
                it.kind = 2;
                it.name_tok = Some(t);
                it.eq_tok = Some(eq);
                if matches!(self.peek_ty(), T::COMMA | T::RPAR) {
                    return Err(self.err_range("expected argument value expression", Self::tok_loc(&t), Self::tok_loc(&eq)));
                }
                let Some(_) = self.expression()? else { break };
                if self.at_comprehension_start() {
                    return Err(self.err_range("invalid syntax. Maybe you meant '==' or ':=' instead of '='?", Self::tok_loc(&t), Self::tok_loc(&eq)));
                }
                it.loc = self.loc(istart);
            } else {
                let e = if t.ty == T::NAME && !t.kw.is_hard() && self.peek_n(1).ty == T::COLONEQUAL {
                    self.assignment_expression()?
                } else {
                    self.expression()?
                };
                let Some(e) = e else { break };
                it.loc = e.loc;
                if self.at_comprehension_start() {
                    let Some(gens) = self.for_if_clauses()? else { break };
                    let last = gens.last().unwrap();
                    let end = last.ifs.last().map(|x| x.loc).unwrap_or(last.iter.loc);
                    it.genexp_end = Some(end);
                }
            }
            items.push(it);
            while followed_comma.len() < items.len() - 1 {
                followed_comma.push(false);
            }
            match self.eat(T::COMMA) {
                Some(c) => {
                    comma_before = Some(c);
                    followed_comma.push(true);
                }
                None => break,
            }
        }
        let n = items.len();
        // genexp checks
        for (i, it) in items.iter().enumerate() {
            if let Some(end) = it.genexp_end {
                if n > 1 || followed_comma.get(i).copied().unwrap_or(false) {
                    if i == 0 {
                        return Err(self.err_range("Generator expression must be parenthesized", it.loc, end));
                    }
                    return Err(self.err_range("Generator expression must be parenthesized", it.loc, end));
                }
            }
        }
        // iterable unpacking after keyword unpacking
        let mut seen_dstar = false;
        for it in &items {
            if it.kind == 3 {
                seen_dstar = true;
            }
            if it.kind == 1 && seen_dstar {
                if let Some(c) = it.comma_before {
                    return Err(self.err_tok("iterable argument unpacking follows keyword argument unpacking", &c));
                }
            }
        }
        // positional after keyword
        let mut seen_kw = false;
        let mut seen_dstar = false;
        for it in &items {
            if it.kind == 2 {
                seen_kw = true;
            }
            if it.kind == 3 {
                seen_dstar = true;
                seen_kw = true;
            }
            if it.kind == 0 && seen_kw {
                let msg = if seen_dstar { "positional argument follows keyword argument unpacking" } else { "positional argument follows keyword argument" };
                return Err(self.err_last(ErrorKind::Syntax, msg));
            }
        }
        Ok(())
    }

    /// invalid_import (second pass). `self.p` is at 'import'/'from'.
    pub(crate) fn invalid_import(&mut self, lazy: bool) -> Result<(), PErr> {
        let start = self.p;
        let t = self.peek();
        let _ = lazy;
        if t.kw == Kw::Import {
            self.advance();
            if self.at(T::NEWLINE) {
                let nl = self.peek();
                return Err(self.err_tok("Expected one or more names after 'import'", &nl));
            }
            // 'import' ','.dotted_name+ 'from' dotted_name
            loop {
                if self.eat_name().is_none() {
                    break;
                }
                while self.at(T::DOT) && self.peek_n(1).ty == T::NAME {
                    self.advance();
                    self.advance();
                }
                if self.at_kw(Kw::From) {
                    self.advance();
                    if self.at_name() {
                        return Err(self.error_at(ErrorKind::Syntax, "Did you mean to use 'from ... import ...' instead?", t.lineno as i32, t.col, -1, -1));
                    }
                    break;
                }
                if self.eat(T::COMMA).is_none() {
                    break;
                }
            }
        } else if t.kw == Kw::From {
            self.advance();
            while matches!(self.peek_ty(), T::DOT | T::ELLIPSIS) {
                self.advance();
            }
            // dotted_name
            if self.eat_name().is_some() {
                while self.at(T::DOT) && self.peek_n(1).ty == T::NAME {
                    self.advance();
                    self.advance();
                }
            }
            if self.at_kw(Kw::SoftLazy) && self.peek_n(1).kw == Kw::Import {
                let lt = self.peek();
                return Err(self.err_tok("use 'lazy from ... ' instead of 'from ... lazy import'", &lt));
            }
            if self.eat_kw(Kw::Import).is_some() && self.at(T::NEWLINE) {
                let nl = self.peek();
                return Err(self.err_tok("Expected one or more names after 'import'", &nl));
            }
        }
        self.p = start;
        Ok(())
    }

    /// After 'as' failed to be followed by a NAME: "cannot use X as import target".
    /// `m` is the index of the 'as' token.
    pub(crate) fn invalid_import_as_target(&mut self, m: usize) -> Result<(), PErr> {
        self.p = m + 1;
        let save = self.invalid;
        self.invalid = false;
        let e = self.expression()?;
        self.invalid = save;
        if let Some(e) = e {
            if !matches!(e.node, ExprKind::Name { .. }) {
                return Err(self.err_loc(format!("cannot use {} as import target", expr_name(&e)), e.loc));
            }
        }
        self.p = m;
        Ok(())
    }
}
