//! Statements.

use super::expr::bx;
use super::*;
use TokenType as T;

fn augassign_op(ty: TokenType) -> Option<Operator> {
    Some(match ty {
        T::PLUSEQUAL => Operator::Add,
        T::MINEQUAL => Operator::Sub,
        T::STAREQUAL => Operator::Mult,
        T::ATEQUAL => Operator::MatMult,
        T::SLASHEQUAL => Operator::Div,
        T::PERCENTEQUAL => Operator::Mod,
        T::AMPEREQUAL => Operator::BitAnd,
        T::VBAREQUAL => Operator::BitOr,
        T::CIRCUMFLEXEQUAL => Operator::BitXor,
        T::LEFTSHIFTEQUAL => Operator::LShift,
        T::RIGHTSHIFTEQUAL => Operator::RShift,
        T::DOUBLESTAREQUAL => Operator::Pow,
        T::DOUBLESLASHEQUAL => Operator::FloorDiv,
        _ => return None,
    })
}

impl<'a> Parser<'a> {
    /// statements: statement+
    pub(crate) fn statements(&mut self) -> R<Vec<Stmt>> {
        let mut v = Vec::new();
        loop {
            let m = self.p;
            match self.statement()? {
                Some(mut s) => v.append(&mut s),
                None => {
                    self.p = m;
                    break;
                }
            }
        }
        if v.is_empty() {
            return Ok(None);
        }
        Ok(Some(v))
    }

    /// statement: compound_stmt | simple_stmts
    pub(crate) fn statement(&mut self) -> R<Vec<Stmt>> {
        let m = self.p;
        if let Some(s) = self.compound_stmt()? {
            return Ok(Some(vec![s]));
        }
        self.p = m;
        self.simple_stmts()
    }

    /// simple_stmts: ';'.simple_stmt+ [';'] NEWLINE
    pub(crate) fn simple_stmts(&mut self) -> R<Vec<Stmt>> {
        let start = self.p;
        let Some(s) = self.simple_stmt()? else { return Ok(None) };
        let mut v = vec![s];
        while self.at(T::SEMI) {
            self.advance();
            let m = self.p;
            match self.simple_stmt()? {
                Some(s) => v.push(s),
                None => {
                    self.p = m;
                    break;
                }
            }
        }
        if self.eat(T::NEWLINE).is_none() {
            self.p = start;
            return Ok(None);
        }
        Ok(Some(v))
    }

    fn simple_stmt(&mut self) -> R<Stmt> {
        let start = self.p;
        let t = self.peek();
        if t.ty == T::NAME {
            match t.kw {
                Kw::Return => return self.return_stmt(),
                Kw::Import | Kw::From => return self.import_stmt(false),
                Kw::Raise => return self.raise_stmt(),
                Kw::Pass => {
                    self.advance();
                    return Ok(Some(Stmt::new(StmtKind::Pass, self.loc(start))));
                }
                Kw::Break => {
                    self.advance();
                    return Ok(Some(Stmt::new(StmtKind::Break, self.loc(start))));
                }
                Kw::Continue => {
                    self.advance();
                    return Ok(Some(Stmt::new(StmtKind::Continue, self.loc(start))));
                }
                Kw::Del => return self.del_stmt(),
                Kw::Assert => return self.assert_stmt(),
                Kw::Global | Kw::Nonlocal => return self.global_stmt(t.kw == Kw::Global),
                Kw::SoftType => {
                    let n1 = self.peek_n(1);
                    if n1.ty == T::NAME && !n1.kw.is_hard() && matches!(self.peek_n(2).ty, T::LSQB | T::EQUAL) {
                        if let Some(s) = self.type_alias()? {
                            return Ok(Some(s));
                        }
                        self.p = start;
                    }
                }
                Kw::SoftLazy => {
                    let n1 = self.peek_n(1);
                    if n1.ty == T::NAME && (n1.kw == Kw::Import || n1.kw == Kw::From) {
                        self.advance();
                        return self.import_stmt(true);
                    }
                }
                _ => {}
            }
        }
        let r = self.assignment_or_expr()?;
        if self.invalid && !matches!(r.as_ref().map(|s| &s.node), Some(StmtKind::Assign { .. } | StmtKind::AugAssign { .. } | StmtKind::AnnAssign { .. })) {
            let end = self.p;
            self.p = start;
            self.invalid_assignment()?;
            self.p = if r.is_some() { end } else { start };
        }
        Ok(r)
    }

    fn assignment_or_expr(&mut self) -> R<Stmt> {
        let start = self.p;
        let t = self.peek();
        // NAME ':' expression ['=' annotated_rhs]
        if t.ty == T::NAME && !t.kw.is_hard() && self.peek_n(1).ty == T::COLON {
            self.advance();
            self.advance();
            let Some(ann) = self.expression()? else {
                self.p = start;
                return Ok(None);
            };
            let mut value = None;
            if self.at(T::EQUAL) {
                let m = self.p;
                self.advance();
                match self.annotated_rhs()? {
                    Some(v) => value = Some(bx(v)),
                    None => self.p = m,
                }
            }
            let target = self.mk_name(&t, ExprContext::Store);
            self.check_version(6, "Variable annotation syntax is")?;
            return Ok(Some(Stmt::new(StmtKind::AnnAssign { target: bx(target), annotation: bx(ann), value, simple: 1 }, self.loc(start))));
        }
        let first = if t.ty == T::NAME && t.kw == Kw::Yield { self.yield_expr()? } else { self.star_expressions()? };
        let Some(mut first) = first else {
            self.p = start;
            return Ok(None);
        };
        let nt = self.peek();
        match nt.ty {
            T::COLON => {
                // annotated assignment to a single target
                let parenthesized = first.loc.col_offset != t.col || first.loc.lineno != t.lineno as i32;
                let ok = match &first.node {
                    ExprKind::Name { .. } => parenthesized,
                    ExprKind::Attribute { .. } | ExprKind::Subscript { .. } => true,
                    _ => false,
                };
                if !ok {
                    self.p = start;
                    return Ok(None);
                }
                self.advance();
                let Some(ann) = self.expression()? else {
                    self.p = start;
                    return Ok(None);
                };
                let mut value = None;
                if self.at(T::EQUAL) {
                    let m = self.p;
                    self.advance();
                    match self.annotated_rhs()? {
                        Some(v) => value = Some(bx(v)),
                        None => self.p = m,
                    }
                }
                super::targets::set_ctx(&mut first, ExprContext::Store);
                self.check_version(6, "Variable annotation syntax is")?;
                Ok(Some(Stmt::new(StmtKind::AnnAssign { target: bx(first), annotation: bx(ann), value, simple: 0 }, self.loc(start))))
            }
            T::EQUAL => {
                let mut targets = Vec::new();
                let mut cur = first;
                loop {
                    if !self.to_store_target(&mut cur) {
                        self.p = start;
                        return Ok(None);
                    }
                    targets.push(cur);
                    self.advance(); // '='
                    let Some(rhs) = self.annotated_rhs()? else {
                        self.p = start;
                        return Ok(None);
                    };
                    if self.at(T::EQUAL) {
                        cur = rhs;
                        continue;
                    }
                    let mut type_comment = None;
                    if let Some(tc) = self.eat(T::TYPE_COMMENT) {
                        type_comment = Some(self.text(&tc).to_string());
                    }
                    return Ok(Some(Stmt::new(StmtKind::Assign { targets, value: bx(rhs), type_comment }, self.loc(start))));
                }
            }
            ty if augassign_op(ty).is_some() => {
                if !Self::is_single_target(&first) {
                    self.p = start;
                    return Ok(None);
                }
                let op = augassign_op(ty).unwrap();
                if op == Operator::MatMult {
                    self.check_version(5, "The '@' operator is")?;
                }
                self.advance();
                let Some(rhs) = self.annotated_rhs()? else {
                    self.p = start;
                    return Ok(None);
                };
                super::targets::set_ctx(&mut first, ExprContext::Store);
                Ok(Some(Stmt::new(StmtKind::AugAssign { target: bx(first), op, value: bx(rhs) }, self.loc(start))))
            }
            _ => Ok(Some(Stmt::new(StmtKind::Expr { value: bx(first) }, self.loc(start)))),
        }
    }

    /// annotated_rhs: yield_expr | star_expressions
    pub(crate) fn annotated_rhs(&mut self) -> R<Expr> {
        if self.at_kw(Kw::Yield) {
            return self.yield_expr();
        }
        self.star_expressions()
    }

    fn return_stmt(&mut self) -> R<Stmt> {
        let start = self.p;
        self.advance();
        let m = self.p;
        let value = match self.star_expressions()? {
            Some(e) => Some(bx(e)),
            None => {
                self.p = m;
                None
            }
        };
        Ok(Some(Stmt::new(StmtKind::Return { value }, self.loc(start))))
    }

    fn raise_stmt(&mut self) -> R<Stmt> {
        let start = self.p;
        let raise_tok = self.advance();
        if self.invalid {
            // invalid_raise_stmt
            if let Some(ft) = self.eat_kw(Kw::From) {
                return Err(self.err_range("did you forget an expression between 'raise' and 'from'?", Self::tok_loc(&raise_tok), Self::tok_loc(&ft)));
            }
            let m = self.p;
            if self.expression()?.is_some() {
                if let Some(ft) = self.eat_kw(Kw::From) {
                    if matches!(self.peek_ty(), T::NEWLINE | T::SEMI | T::ENDMARKER) {
                        return Err(self.err_tok("did you forget an expression after 'from'?", &ft));
                    }
                }
            }
            self.p = m;
        }
        let m = self.p;
        if let Some(exc) = self.expression()? {
            let mut cause = None;
            if self.at_kw(Kw::From) {
                let m2 = self.p;
                self.advance();
                match self.expression()? {
                    Some(c) => cause = Some(bx(c)),
                    None => {
                        self.p = m2;
                        // 'raise' expression 'from' <nothing> -> no match for this alternative
                        self.p = start;
                        return Ok(None);
                    }
                }
            }
            return Ok(Some(Stmt::new(StmtKind::Raise { exc: Some(bx(exc)), cause }, self.loc(start))));
        }
        self.p = m;
        Ok(Some(Stmt::new(StmtKind::Raise { exc: None, cause: None }, self.loc(start))))
    }

    fn global_stmt(&mut self, global: bool) -> R<Stmt> {
        let start = self.p;
        self.advance();
        let mut names = Vec::new();
        loop {
            let Some(n) = self.eat_name() else {
                self.p = start;
                return Ok(None);
            };
            names.push(self.ident(&n));
            if self.eat(T::COMMA).is_none() {
                break;
            }
        }
        let k = if global { StmtKind::Global { names } } else { StmtKind::Nonlocal { names } };
        Ok(Some(Stmt::new(k, self.loc(start))))
    }

    fn del_stmt(&mut self) -> R<Stmt> {
        let start = self.p;
        self.advance();
        let m = self.p;
        if let Some(targets) = self.del_targets()? {
            if matches!(self.peek_ty(), T::SEMI | T::NEWLINE) {
                return Ok(Some(Stmt::new(StmtKind::Delete { targets }, self.loc(start))));
            }
        }
        if self.invalid {
            // invalid_del_stmt: 'del' a=star_expressions
            self.p = m;
            if let Some(e) = self.star_expressions()? {
                if let Some(err) = self.invalid_target_error(TargetsType::Del, &e) {
                    return Err(err);
                }
            }
        }
        self.p = start;
        Ok(None)
    }

    /// invalid_assert_stmt: 'assert' [expression ','] a=expression ('=' | ':=') b=expression
    fn invalid_assert_stmt(&mut self) -> Result<(), PErr> {
        let start = self.p;
        self.advance();
        for _ in 0..2 {
            let save = self.invalid;
            self.invalid = false;
            let a = self.expression()?;
            self.invalid = save;
            let Some(a) = a else { break };
            let t = self.peek();
            if t.ty == T::EQUAL || t.ty == T::COLONEQUAL {
                self.advance();
                let save = self.invalid;
                self.invalid = false;
                let b = self.expression()?;
                self.invalid = save;
                if let Some(b) = b {
                    if t.ty == T::EQUAL {
                        return Err(self.err_range(format!("cannot assign to {} here. Maybe you meant '==' instead of '='?", super::expr::expr_name(&a)), a.loc, b.loc));
                    }
                    return Err(self.err_range("cannot use named expression without parentheses here", a.loc, b.loc));
                }
                break;
            }
            if self.eat(T::COMMA).is_none() {
                break;
            }
        }
        self.p = start;
        Ok(())
    }

    fn assert_stmt(&mut self) -> R<Stmt> {
        let start = self.p;
        if self.invalid {
            self.invalid_assert_stmt()?;
        }
        self.advance();
        let Some(test) = self.expression()? else {
            self.p = start;
            return Ok(None);
        };
        let mut msg = None;
        if self.at(T::COMMA) {
            let m = self.p;
            self.advance();
            match self.expression()? {
                Some(e) => msg = Some(bx(e)),
                None => self.p = m,
            }
        }
        Ok(Some(Stmt::new(StmtKind::Assert { test: bx(test), msg }, self.loc(start))))
    }

    fn type_alias(&mut self) -> R<Stmt> {
        let start = self.p;
        self.advance(); // "type"
        self.check_version(12, "Type statement is")?;
        let n = self.advance();
        let name = self.mk_name(&n, ExprContext::Store);
        let mut type_params = Vec::new();
        if self.at(T::LSQB) {
            match self.type_params()? {
                Some(tp) => type_params = tp,
                None => {
                    self.p = start;
                    return Ok(None);
                }
            }
        }
        if self.eat(T::EQUAL).is_none() {
            self.p = start;
            return Ok(None);
        }
        let Some(value) = self.expression()? else {
            self.p = start;
            return Ok(None);
        };
        Ok(Some(Stmt::new(StmtKind::TypeAlias { name: bx(name), type_params, value: bx(value) }, self.loc(start))))
    }

    // ------------------------------------------------------------------ imports

    fn dotted_name(&mut self) -> Option<String> {
        let n = self.eat_name()?;
        let mut s = self.ident(&n);
        while self.at(T::DOT) && {
            let n1 = self.peek_n(1);
            n1.ty == T::NAME && !n1.kw.is_hard()
        } {
            self.advance();
            let n = self.advance();
            s.push('.');
            s.push_str(&self.ident(&n));
        }
        Some(s)
    }

    fn import_stmt(&mut self, lazy: bool) -> R<Stmt> {
        // `self.p` is at 'import'/'from'; if lazy, the 'lazy' token is just before.
        let start = if lazy { self.p - 1 } else { self.p };
        if self.invalid {
            let m = self.p;
            self.invalid_import(lazy)?;
            self.p = m;
        }
        let is_lazy = lazy as i32;
        if self.eat_kw(Kw::Import).is_some() {
            let mut names = Vec::new();
            loop {
                let astart = self.p;
                let Some(name) = self.dotted_name() else {
                    self.p = start;
                    return Ok(None);
                };
                let mut asname = None;
                if self.at_kw(Kw::As) {
                    let m = self.p;
                    self.advance();
                    match self.import_as_name(m)? {
                        Some(n) => asname = Some(n),
                        None => {
                            self.p = start;
                            return Ok(None);
                        }
                    }
                }
                names.push(Alias { name, asname, loc: self.loc(astart) });
                if self.eat(T::COMMA).is_none() {
                    break;
                }
            }
            return Ok(Some(Stmt::new(StmtKind::Import { names, is_lazy }, self.loc(start))));
        }
        // from
        self.advance();
        let mut level = 0;
        let mut last_dot_end: Option<Token> = None;
        loop {
            let t = self.peek();
            match t.ty {
                T::DOT => level += 1,
                T::ELLIPSIS => level += 3,
                _ => break,
            }
            last_dot_end = Some(t);
            self.advance();
        }
        let mstart = self.p;
        let module = if self.at_kw(Kw::Import) { None } else { self.dotted_name() };
        if module.is_none() && level == 0 {
            self.p = start;
            return Ok(None);
        }
        if self.at_kw(Kw::Import) {
            // gh-150459: `from . lazy import x` warning
            if !lazy && level > 0 && module.as_deref() == Some("lazy") && self.p == mstart + 1 {
                let lt = self.tok_at(mstart);
                if let Some(d) = last_dot_end {
                    if d.end != lt.start {
                        let dots: String = ".".repeat(level as usize);
                        let msg = format!("did you mean 'lazy from {} import'?", dots);
                        self.warn(msg.clone(), msg, Self::tok_loc(&lt));
                    }
                }
            }
        }
        if self.eat_kw(Kw::Import).is_none() {
            self.p = start;
            return Ok(None);
        }
        let names = if self.at(T::STAR) {
            let st = self.advance();
            vec![Alias { name: "*".into(), asname: None, loc: Self::tok_loc(&st) }]
        } else {
            let paren = self.eat(T::LPAR).is_some();
            let mut names = Vec::new();
            loop {
                let astart = self.p;
                let Some(n) = self.eat_name() else {
                    if paren && !names.is_empty() {
                        break;
                    }
                    self.p = start;
                    return Ok(None);
                };
                let mut asname = None;
                if self.at_kw(Kw::As) {
                    let m = self.p;
                    self.advance();
                    match self.import_as_name(m)? {
                        Some(n) => asname = Some(n),
                        None => {
                            self.p = start;
                            return Ok(None);
                        }
                    }
                }
                names.push(Alias { name: self.ident(&n), asname, loc: self.loc(astart) });
                if !self.at(T::COMMA) {
                    break;
                }
                if !paren && !(self.peek_n(1).ty == T::NAME && !self.peek_n(1).kw.is_hard()) {
                    // import_from_as_names !','
                    if self.invalid {
                        let ct = self.peek();
                        if matches!(self.peek_n(1).ty, T::NEWLINE | T::SEMI) {
                            return Err(self.err_tok("trailing comma not allowed without surrounding parentheses", &ct));
                        }
                    }
                    self.p = start;
                    return Ok(None);
                }
                self.advance();
            }
            if paren && self.eat(T::RPAR).is_none() {
                self.p = start;
                return Ok(None);
            }
            names
        };
        if level == 0 && module.as_deref() == Some("__future__") && names.iter().any(|a| a.name == "barry_as_FLUFL") {
            self.flufl = true;
        }
        Ok(Some(Stmt::new(StmtKind::ImportFrom { module, names, level, is_lazy }, self.loc(start))))
    }

    /// The NAME after 'as' in an import (index `m` is the 'as' token); in the second pass
    /// raises "cannot use X as import target" for other expressions.
    fn import_as_name(&mut self, m: usize) -> R<String> {
        if self.invalid {
            let t = self.peek();
            let n1 = self.peek_n(1).ty;
            let good = t.ty == T::NAME && !t.kw.is_hard() && matches!(n1, T::COMMA | T::RPAR | T::NEWLINE | T::SEMI);
            if !good {
                self.invalid_import_as_target(m)?;
                self.p = m + 1;
            }
        }
        match self.eat_name() {
            Some(n) => Ok(Some(self.ident(&n))),
            None => Ok(None),
        }
    }

    // ------------------------------------------------------------------ compound statements

    pub(crate) fn compound_stmt(&mut self) -> R<Stmt> {
        let t = self.peek();
        match t.ty {
            T::AT => self.decorated(),
            T::NAME => match t.kw {
                Kw::Def => self.function_def(Vec::new()),
                Kw::Class => self.class_def(Vec::new()),
                Kw::If => self.if_stmt(),
                Kw::While => self.while_stmt(),
                Kw::For => self.for_stmt(),
                Kw::With => self.with_stmt(),
                Kw::Try => self.try_stmt(),
                Kw::Async => {
                    let n = self.peek_n(1);
                    if n.ty != T::NAME {
                        return Ok(None);
                    }
                    match n.kw {
                        Kw::Def => self.function_def(Vec::new()),
                        Kw::For => self.for_stmt(),
                        Kw::With => self.with_stmt(),
                        _ => Ok(None),
                    }
                }
                Kw::SoftMatch => self.match_stmt(),
                Kw::SoftCase if self.invalid => {
                    // invalid_case_block outside match: "case" patterns guard? ':'
                    let m = self.p;
                    self.advance();
                    if self.patterns()?.is_some() {
                        if self.at_kw(Kw::If) {
                            self.advance();
                            let _ = self.named_expression()?;
                        }
                        if self.at(T::COLON) {
                            return Err(self.err_range("case statement must be inside match statement", Self::tok_loc(&t), Self::tok_loc(&t)));
                        }
                    }
                    self.p = m;
                    Ok(None)
                }
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    /// block: NEWLINE INDENT statements DEDENT | simple_stmts
    pub(crate) fn block(&mut self) -> R<Vec<Stmt>> {
        let start = self.p;
        if self.at(T::NEWLINE) {
            self.advance();
            if self.eat(T::INDENT).is_none() {
                if self.invalid {
                    return Err(self.err_last(ErrorKind::Indentation, "expected an indented block"));
                }
                self.p = start;
                return Ok(None);
            }
            self.enter()?;
            let r = self.statements();
            self.leave();
            let Some(body) = r? else {
                self.p = start;
                return Ok(None);
            };
            if self.eat(T::DEDENT).is_none() {
                self.p = start;
                return Ok(None);
            }
            return Ok(Some(body));
        }
        self.simple_stmts()
    }

    /// Block following a compound statement header; in the second pass raises the specific
    /// IndentationError "expected an indented block after ...".
    fn block_after(&mut self, what: &str, lineno: i32) -> R<Vec<Stmt>> {
        if self.invalid && self.at(T::NEWLINE) && self.peek_n(1).ty != T::INDENT {
            self.advance();
            self.peek();
            return Err(self.err_last(ErrorKind::Indentation, format!("expected an indented block after {} on line {}", what, lineno)));
        }
        self.block()
    }

    /// Expect ':' (forced token: raise "expected ':'" if missing).
    fn forced(&mut self, ty: TokenType, s: &str) -> Result<Token, PErr> {
        let t = self.peek();
        if t.ty != ty {
            return Err(self.err_tok(format!("expected '{}'", s), &t));
        }
        self.advance();
        Ok(t)
    }

    /// In the second pass: if the header is followed by NEWLINE instead of ':', raise
    /// "expected ':'" (RAISE_SYNTAX_ERROR at the last token).
    fn expect_colon_invalid(&mut self) -> Result<(), PErr> {
        if self.invalid && self.at(T::NEWLINE) {
            self.peek();
            return Err(self.err_last(ErrorKind::Syntax, "expected ':'"));
        }
        Ok(())
    }

    fn decorators(&mut self) -> R<Vec<Expr>> {
        let mut v = Vec::new();
        while self.at(T::AT) {
            let m = self.p;
            self.advance();
            let Some(e) = self.named_expression()? else {
                self.p = m;
                return if v.is_empty() { Ok(None) } else { Ok(None) };
            };
            if self.eat(T::NEWLINE).is_none() {
                self.p = m;
                return Ok(None);
            }
            v.push(e);
        }
        Ok(Some(v))
    }

    fn decorated(&mut self) -> R<Stmt> {
        let start = self.p;
        let Some(decos) = self.decorators()? else {
            self.p = start;
            return Ok(None);
        };
        let t = self.peek();
        let r = if t.ty == T::NAME && t.kw == Kw::Class {
            self.class_def(decos)?
        } else if t.ty == T::NAME && (t.kw == Kw::Def || (t.kw == Kw::Async && self.peek_n(1).kw == Kw::Def)) {
            self.function_def(decos)?
        } else {
            None
        };
        if r.is_none() {
            self.p = start;
        }
        Ok(r)
    }

    fn function_def(&mut self, decorator_list: Vec<Expr>) -> R<Stmt> {
        let start = self.p;
        let is_async = self.eat_kw(Kw::Async).is_some();
        if is_async {
            self.check_version(5, "Async functions are")?;
        }
        let def_tok = self.advance(); // 'def'
        let _ = def_tok;
        let Some(n) = self.eat_name() else {
            self.p = start;
            return Ok(None);
        };
        let name = self.ident(&n);
        let mut type_params = Vec::new();
        if self.at(T::LSQB) {
            match self.type_params()? {
                Some(tp) => type_params = tp,
                None => {
                    self.p = start;
                    return Ok(None);
                }
            }
        }
        self.forced(T::LPAR, "(")?;
        let args = if self.at(T::RPAR) {
            Arguments::default()
        } else {
            match self.params()? {
                Some(a) => a,
                None => {
                    self.p = start;
                    return Ok(None);
                }
            }
        };
        if self.eat(T::RPAR).is_none() {
            self.p = start;
            return Ok(None);
        }
        let mut returns = None;
        if self.at(T::RARROW) {
            let m = self.p;
            self.advance();
            match self.expression()? {
                Some(e) => returns = Some(bx(e)),
                None => {
                    self.p = m;
                }
            }
        }
        self.forced(T::COLON, ":")?;
        // func_type_comment: NEWLINE TYPE_COMMENT &(NEWLINE INDENT) | TYPE_COMMENT
        let mut type_comment = None;
        if self.at(T::NEWLINE) && self.peek_n(1).ty == T::TYPE_COMMENT && self.peek_n(2).ty == T::NEWLINE && self.peek_n(3).ty == T::INDENT {
            self.advance();
            let tc = self.advance();
            type_comment = Some(self.text(&tc).to_string());
            // the NEWLINE before INDENT remains for the block; we consumed the first NEWLINE.
        } else if self.at(T::TYPE_COMMENT) {
            if self.peek_n(1).ty == T::NEWLINE && self.peek_n(2).ty == T::TYPE_COMMENT && self.peek_n(3).ty == T::NEWLINE && self.peek_n(4).ty == T::INDENT {
                return Err(self.err_last(ErrorKind::Syntax, "Cannot have two type comments on def"));
            }
            let tc = self.advance();
            type_comment = Some(self.text(&tc).to_string());
        }
        let Some(body) = self.block_after("function definition", def_tok.lineno as i32)? else {
            self.p = start;
            return Ok(None);
        };
        let f = FunctionDef { name, args: Box::new(args), body, decorator_list, returns, type_comment, type_params };
        let k = if is_async { StmtKind::AsyncFunctionDef(f) } else { StmtKind::FunctionDef(f) };
        Ok(Some(Stmt::new(k, self.loc(start))))
    }

    fn class_def(&mut self, decorator_list: Vec<Expr>) -> R<Stmt> {
        let start = self.p;
        let ct = self.advance(); // 'class'
        let Some(n) = self.eat_name() else {
            self.p = start;
            return Ok(None);
        };
        let name = self.ident(&n);
        let mut type_params = Vec::new();
        if self.at(T::LSQB) {
            match self.type_params()? {
                Some(tp) => type_params = tp,
                None => {
                    self.p = start;
                    return Ok(None);
                }
            }
        }
        let mut bases = Vec::new();
        let mut keywords = Vec::new();
        if self.at(T::LPAR) {
            let m = self.p;
            self.advance();
            if self.eat(T::RPAR).is_none() {
                match self.class_arguments(m)? {
                    Some((b, k)) => {
                        bases = b;
                        keywords = k;
                    }
                    None => {
                        self.p = start;
                        return Ok(None);
                    }
                }
            }
        }
        self.expect_colon_invalid()?;
        if self.eat(T::COLON).is_none() {
            self.p = start;
            return Ok(None);
        }
        let Some(body) = self.block_after("class definition", ct.lineno as i32)? else {
            self.p = start;
            return Ok(None);
        };
        Ok(Some(Stmt::new(StmtKind::ClassDef { name, bases, keywords, body, decorator_list, type_params }, self.loc(start))))
    }

    fn class_arguments(&mut self, lpar: usize) -> R<(Vec<Expr>, Vec<Keyword>)> {
        // reuse call argument parsing via a synthetic primary: parse as call trailer
        self.p = lpar;
        let save = self.p;
        // Build a dummy call: parse '(' args ')' using the primary call machinery.
        let r = self.call_args_only()?;
        if r.is_none() {
            self.p = save;
        }
        Ok(r)
    }

    /// if_stmt / elif_stmt. Written iteratively so that long `elif` chains do not recurse
    /// (the AST is still nested: each `elif` is an `If` in the previous `orelse`).
    fn if_stmt(&mut self) -> R<Stmt> {
        let start = self.p;
        // (start index, test, body) for 'if' and each 'elif'
        let mut parts: Vec<(usize, Expr, Vec<Stmt>)> = Vec::new();
        loop {
            let pstart = self.p;
            let it = self.advance(); // 'if' or 'elif'
            let Some(test) = self.named_expression()? else {
                self.p = start;
                return Ok(None);
            };
            self.expect_colon_invalid()?;
            if self.eat(T::COLON).is_none() {
                self.p = start;
                return Ok(None);
            }
            let what = if it.kw == Kw::If { "'if' statement" } else { "'elif' statement" };
            let Some(body) = self.block_after(what, it.lineno as i32)? else {
                self.p = start;
                return Ok(None);
            };
            parts.push((pstart, test, body));
            self.check_chain(parts.len())?;
            if !self.at_kw(Kw::Elif) {
                break;
            }
        }
        let mut orelse = if self.at_kw(Kw::Else) {
            match self.else_block()? {
                Some(b) => {
                    if self.invalid && self.at_kw(Kw::Elif) {
                        let et = self.peek();
                        return Err(self.err_tok("'elif' block follows an 'else' block", &et));
                    }
                    b
                }
                None => {
                    self.p = start;
                    return Ok(None);
                }
            }
        } else {
            Vec::new()
        };
        // every If (including the nested elifs) ends where the whole statement ends
        while let Some((pstart, test, body)) = parts.pop() {
            let s = Stmt::new(StmtKind::If { test: bx(test), body, orelse }, self.loc(pstart));
            if parts.is_empty() {
                return Ok(Some(s));
            }
            orelse = vec![s];
        }
        unreachable!()
    }

    fn else_block(&mut self) -> R<Vec<Stmt>> {
        let et = self.advance(); // 'else'
        self.forced(T::COLON, ":")?;
        self.block_after("'else' statement", et.lineno as i32)
    }

    fn while_stmt(&mut self) -> R<Stmt> {
        let start = self.p;
        let wt = self.advance();
        let Some(test) = self.named_expression()? else {
            self.p = start;
            return Ok(None);
        };
        self.expect_colon_invalid()?;
        if self.eat(T::COLON).is_none() {
            self.p = start;
            return Ok(None);
        }
        let Some(body) = self.block_after("'while' statement", wt.lineno as i32)? else {
            self.p = start;
            return Ok(None);
        };
        let orelse = if self.at_kw(Kw::Else) {
            match self.else_block()? {
                Some(b) => b,
                None => {
                    self.p = start;
                    return Ok(None);
                }
            }
        } else {
            Vec::new()
        };
        Ok(Some(Stmt::new(StmtKind::While { test: bx(test), body, orelse }, self.loc(start))))
    }

    fn for_stmt(&mut self) -> R<Stmt> {
        let start = self.p;
        let is_async = self.eat_kw(Kw::Async).is_some();
        if is_async {
            self.check_version(5, "Async for loops are")?;
        }
        let ft = self.advance(); // 'for'
        let tstart = self.p;
        let Some(target) = self.star_targets()? else {
            if self.invalid {
                self.p = tstart;
                self.invalid_for_target_tail(TargetsType::For)?;
            }
            self.p = start;
            return Ok(None);
        };
        if self.eat_kw(Kw::In).is_none() {
            if self.invalid {
                self.p = tstart;
                self.invalid_for_target_tail(TargetsType::For)?;
            }
            self.p = start;
            return Ok(None);
        }
        // cut
        let Some(iter) = self.star_expressions()? else {
            self.p = start;
            return Ok(None);
        };
        self.expect_colon_invalid()?;
        if self.eat(T::COLON).is_none() {
            self.p = start;
            return Ok(None);
        }
        let mut type_comment = None;
        if let Some(tc) = self.eat(T::TYPE_COMMENT) {
            type_comment = Some(self.text(&tc).to_string());
        }
        let Some(body) = self.block_after("'for' statement", ft.lineno as i32)? else {
            self.p = start;
            return Ok(None);
        };
        let orelse = if self.at_kw(Kw::Else) {
            match self.else_block()? {
                Some(b) => b,
                None => {
                    self.p = start;
                    return Ok(None);
                }
            }
        } else {
            Vec::new()
        };
        let f = For { target: bx(target), iter: bx(iter), body, orelse, type_comment };
        Ok(Some(Stmt::new(if is_async { StmtKind::AsyncFor(f) } else { StmtKind::For(f) }, self.loc(start))))
    }

    /// with_item: expression 'as' star_target &(',' | ')' | ':') | expression
    fn with_item(&mut self) -> R<WithItem> {
        let start = self.p;
        let Some(e) = self.expression()? else { return Ok(None) };
        if self.at_kw(Kw::As) {
            let m = self.p;
            self.advance();
            let tstart = self.p;
            if let Some(t) = self.star_target()? {
                if matches!(self.peek_ty(), T::COMMA | T::RPAR | T::COLON) {
                    return Ok(Some(WithItem { context_expr: e, optional_vars: Some(bx(t)) }));
                }
            }
            if self.invalid {
                // invalid_with_item: expression 'as' a=expression &(',' | ')' | ':')
                self.p = tstart;
                if let Some(a) = self.expression()? {
                    if matches!(self.peek_ty(), T::COMMA | T::RPAR | T::COLON) {
                        if let Some(err) = self.invalid_target_error(TargetsType::Star, &a) {
                            return Err(err);
                        }
                    }
                }
            }
            self.p = m;
            let _ = start;
            return Ok(Some(WithItem { context_expr: e, optional_vars: None }));
        }
        Ok(Some(WithItem { context_expr: e, optional_vars: None }))
    }

    /// invalid_with_stmt: [ASYNC] 'with' ','.(expression ['as' star_target])+ NEWLINE
    ///                  | [ASYNC] 'with' '(' ','.(expressions ['as' star_target])+ ','? ')' NEWLINE
    fn invalid_with_stmt(&mut self) -> Result<(), PErr> {
        let start = self.p;
        self.eat_kw(Kw::Async);
        self.advance(); // 'with'
        let save = self.invalid;
        self.invalid = false;
        let after = self.p;
        for paren in [false, true] {
            self.p = after;
            if paren && self.eat(T::LPAR).is_none() {
                continue;
            }
            let mut ok = false;
            loop {
                let e = if paren { self.expressions()? } else { self.expression()? };
                if e.is_none() {
                    break;
                }
                ok = true;
                if self.at_kw(Kw::As) {
                    let m = self.p;
                    self.advance();
                    if self.star_target()?.is_none() {
                        self.p = m;
                    }
                }
                if self.eat(T::COMMA).is_none() {
                    break;
                }
            }
            if !ok {
                continue;
            }
            if paren && self.eat(T::RPAR).is_none() {
                continue;
            }
            if self.at(T::NEWLINE) {
                self.invalid = save;
                self.peek();
                return Err(self.err_last(ErrorKind::Syntax, "expected ':'"));
            }
        }
        self.invalid = save;
        self.p = start;
        Ok(())
    }

    fn with_stmt(&mut self) -> R<Stmt> {
        let start = self.p;
        if self.invalid {
            self.invalid_with_stmt()?;
        }
        let is_async = self.eat_kw(Kw::Async).is_some();
        if is_async {
            self.check_version(5, "Async with statements are")?;
        }
        let wt = self.advance(); // 'with'
        let after_with = self.p;
        // parenthesized form: '(' ','.with_item+ ','? ')' ':'
        if self.at(T::LPAR) {
            self.advance();
            let mut items = Vec::new();
            let mut ok = true;
            loop {
                match self.with_item()? {
                    Some(it) => items.push(it),
                    None => {
                        ok = false;
                        break;
                    }
                }
                if self.eat(T::COMMA).is_none() {
                    break;
                }
                if self.at(T::RPAR) {
                    break;
                }
            }
            if ok && !items.is_empty() && self.eat(T::RPAR).is_some() && self.at(T::COLON) {
                self.advance();
                let mut type_comment = None;
                if let Some(tc) = self.eat(T::TYPE_COMMENT) {
                    type_comment = Some(self.text(&tc).to_string());
                }
                let Some(body) = self.block_after("'with' statement", wt.lineno as i32)? else {
                    self.p = start;
                    return Ok(None);
                };
                let w = With { items, body, type_comment };
                return Ok(Some(Stmt::new(if is_async { StmtKind::AsyncWith(w) } else { StmtKind::With(w) }, self.loc(start))));
            }
            self.p = after_with;
        }
        let mut items = Vec::new();
        loop {
            match self.with_item()? {
                Some(it) => items.push(it),
                None => {
                    self.p = start;
                    return Ok(None);
                }
            }
            if self.eat(T::COMMA).is_none() {
                break;
            }
            if self.invalid && self.at(T::COLON) {
                let c = self.tok_at(self.p - 1);
                return Err(self.err_tok("the last 'with' item has a trailing comma", &c));
            }
        }
        self.expect_colon_invalid()?;
        if self.eat(T::COLON).is_none() {
            self.p = start;
            return Ok(None);
        }
        let mut type_comment = None;
        if let Some(tc) = self.eat(T::TYPE_COMMENT) {
            type_comment = Some(self.text(&tc).to_string());
        }
        let Some(body) = self.block_after("'with' statement", wt.lineno as i32)? else {
            self.p = start;
            return Ok(None);
        };
        let w = With { items, body, type_comment };
        Ok(Some(Stmt::new(if is_async { StmtKind::AsyncWith(w) } else { StmtKind::With(w) }, self.loc(start))))
    }

    fn try_stmt(&mut self) -> R<Stmt> {
        let start = self.p;
        let tt = self.advance(); // 'try'
        self.forced(T::COLON, ":")?;
        let Some(body) = self.block_after("'try' statement", tt.lineno as i32)? else {
            self.p = start;
            return Ok(None);
        };
        let mut handlers = Vec::new();
        let mut star: Option<bool> = None;
        while self.at_kw(Kw::Except) {
            let is_star = self.peek_n(1).ty == T::STAR;
            if let Some(s) = star {
                if s != is_star {
                    if self.invalid {
                        let et = self.peek();
                        if is_star {
                            let st = self.peek_n(1);
                            return Err(self.err_range("cannot have both 'except' and 'except*' on the same 'try'", Self::tok_loc(&et), Self::tok_loc(&st)));
                        }
                        return Err(self.err_tok("cannot have both 'except' and 'except*' on the same 'try'", &et));
                    }
                    self.p = start;
                    return Ok(None);
                }
            }
            star = Some(is_star);
            if is_star {
                self.check_version(11, "Exception groups are")?;
            }
            let Some(h) = self.except_block(is_star)? else {
                self.p = start;
                return Ok(None);
            };
            handlers.push(h);
        }
        let mut orelse = Vec::new();
        if !handlers.is_empty() && self.at_kw(Kw::Else) {
            match self.else_block()? {
                Some(b) => orelse = b,
                None => {
                    self.p = start;
                    return Ok(None);
                }
            }
        }
        let mut finalbody = Vec::new();
        if self.at_kw(Kw::Finally) {
            let ft = self.advance();
            self.forced(T::COLON, ":")?;
            match self.block_after("'finally' statement", ft.lineno as i32)? {
                Some(b) => finalbody = b,
                None => {
                    self.p = start;
                    return Ok(None);
                }
            }
        } else if handlers.is_empty() {
            if self.invalid {
                return Err(self.err_last(ErrorKind::Syntax, "expected 'except' or 'finally' block"));
            }
            self.p = start;
            return Ok(None);
        }
        let t = Try { body, handlers, orelse, finalbody };
        Ok(Some(Stmt::new(if star == Some(true) { StmtKind::TryStar(t) } else { StmtKind::Try(t) }, self.loc(start))))
    }

    fn except_block(&mut self, is_star: bool) -> R<ExceptHandler> {
        let start = self.p;
        let et = self.advance(); // 'except'
        if is_star {
            self.advance();
        }
        let what = if is_star { "'except*' statement" } else { "'except' statement" };
        if !is_star && self.at(T::COLON) {
            self.advance();
            let Some(body) = self.block_after(what, et.lineno as i32)? else {
                self.p = start;
                return Ok(None);
            };
            return Ok(Some(ExceptHandler { type_: None, name: None, body, loc: self.loc(start) }));
        }
        if is_star && self.at(T::COLON) {
            if self.invalid {
                let c = self.peek();
                return Err(self.err_tok("expected one or more exception types", &c));
            }
            self.p = start;
            return Ok(None);
        }
        let tstart = self.p;
        let Some(e) = self.expression()? else {
            self.expect_colon_invalid()?;
            self.p = start;
            return Ok(None);
        };
        let mut type_ = e;
        let mut name = None;
        if self.at_kw(Kw::As) {
            self.advance();
            let simple = self.at_name() && matches!(self.peek_n(1).ty, T::COLON | T::NEWLINE);
            if self.invalid && !simple {
                // invalid_except_stmt: 'except' expression 'as' a=expression
                let m = self.p;
                let save = self.invalid;
                self.invalid = false;
                let a = self.expression()?;
                self.invalid = save;
                if let Some(a) = a {
                    if !matches!(a.node, ExprKind::Name { .. }) {
                        let kind = if is_star { "except*" } else { "except" };
                        return Err(self.err_loc(format!("cannot use {} statement with {}", kind, super::expr::expr_name(&a)), a.loc));
                    }
                }
                self.p = m;
            }
            match self.eat_name() {
                Some(n) => name = Some(self.ident(&n)),
                None => {
                    if self.invalid {
                        // invalid_except_stmt: 'except' expression 'as' a=expression
                        if let Some(a) = self.expression()? {
                            let kind = if is_star { "except*" } else { "except" };
                            return Err(self.err_loc(format!("cannot use {} statement with {}", kind, super::expr::expr_name(&a)), a.loc));
                        }
                    }
                    self.p = start;
                    return Ok(None);
                }
            }
        } else if self.at(T::COMMA) {
            // PEP 758: except A, B: (tuple without parentheses, no 'as')
            self.check_version(14, "except expressions without parentheses are")?;
            let mut elts = vec![type_];
            while self.at(T::COMMA) {
                let m = self.p;
                self.advance();
                match self.expression()? {
                    Some(x) => elts.push(x),
                    None => {
                        self.p = m;
                        break;
                    }
                }
            }
            if self.at_kw(Kw::As) {
                if self.invalid {
                    let a = elts[0].loc;
                    let b = elts.last().unwrap().loc;
                    return Err(self.err_range("multiple exception types must be parenthesized when using 'as'", a, b));
                }
                self.p = start;
                return Ok(None);
            }
            type_ = Expr::new(ExprKind::Tuple { elts, ctx: ExprContext::Load }, self.loc(tstart));
        }
        self.expect_colon_invalid()?;
        if self.eat(T::COLON).is_none() {
            self.p = start;
            return Ok(None);
        }
        let Some(body) = self.block_after(what, et.lineno as i32)? else {
            self.p = start;
            return Ok(None);
        };
        Ok(Some(ExceptHandler { type_: Some(bx(type_)), name, body, loc: self.loc(start) }))
    }

    // ------------------------------------------------------------------ type params

    /// type_params: '[' ','.type_param+ [','] ']'
    pub(crate) fn type_params(&mut self) -> R<Vec<TypeParam>> {
        let start = self.p;
        let lb = self.advance(); // '['
        self.check_version(12, "Type parameter lists are")?;
        if self.at(T::RSQB) {
            if self.invalid {
                let rb = self.peek();
                return Err(self.err_range("Type parameter list cannot be empty", Self::tok_loc(&lb), Self::tok_loc(&rb)));
            }
            self.p = start;
            return Ok(None);
        }
        let mut v = Vec::new();
        loop {
            let Some(tp) = self.type_param()? else {
                self.p = start;
                return Ok(None);
            };
            v.push(tp);
            if self.eat(T::COMMA).is_none() {
                break;
            }
            if self.at(T::RSQB) {
                break;
            }
        }
        if self.eat(T::RSQB).is_none() {
            self.p = start;
            return Ok(None);
        }
        Ok(Some(v))
    }

    fn type_param(&mut self) -> R<TypeParam> {
        let start = self.p;
        let t = self.peek();
        match t.ty {
            T::STAR | T::DOUBLESTAR => {
                self.advance();
                let Some(n) = self.eat_name() else {
                    self.p = start;
                    return Ok(None);
                };
                let name = self.ident(&n);
                if self.at(T::COLON) {
                    // bound on TypeVarTuple/ParamSpec
                    let colon = self.peek();
                    self.advance();
                    let m = self.p;
                    if self.expression()?.is_some() {
                        let msg = if t.ty == T::STAR { "cannot use bound with TypeVarTuple" } else { "cannot use bound with ParamSpec" };
                        let _ = m;
                        return Err(self.err_range(msg, Self::tok_loc(&colon), self.loc(m)));
                    }
                    self.p = start;
                    return Ok(None);
                }
                let mut default_value = None;
                if self.at(T::EQUAL) {
                    self.advance();
                    let e = if t.ty == T::STAR { self.star_expression()? } else { self.expression()? };
                    match e {
                        Some(e) => {
                            self.check_version(13, "Type parameter defaults are")?;
                            default_value = Some(bx(e));
                        }
                        None => {
                            if self.invalid {
                                let eq = self.tok_at(self.p - 1);
                                return Err(self.err_tok("expected default value expression", &eq));
                            }
                            self.p = start;
                            return Ok(None);
                        }
                    }
                }
                let k = if t.ty == T::STAR { TypeParamKind::TypeVarTuple { name, default_value } } else { TypeParamKind::ParamSpec { name, default_value } };
                Ok(Some(TypeParam { node: k, loc: self.loc(start) }))
            }
            _ => {
                let Some(n) = self.eat_name() else {
                    self.p = start;
                    return Ok(None);
                };
                let name = self.ident(&n);
                let mut bound = None;
                if self.at(T::COLON) {
                    self.advance();
                    match self.expression()? {
                        Some(e) => bound = Some(bx(e)),
                        None => {
                            self.p = start;
                            return Ok(None);
                        }
                    }
                }
                let mut default_value = None;
                if self.at(T::EQUAL) {
                    self.advance();
                    match self.expression()? {
                        Some(e) => {
                            self.check_version(13, "Type parameter defaults are")?;
                            default_value = Some(bx(e));
                        }
                        None => {
                            if self.invalid {
                                let eq = self.tok_at(self.p - 1);
                                if matches!(self.peek_ty(), T::COMMA | T::RSQB) {
                                    return Err(self.err_tok("expected default value expression", &eq));
                                }
                            }
                            self.p = start;
                            return Ok(None);
                        }
                    }
                }
                Ok(Some(TypeParam { node: TypeParamKind::TypeVar { name, bound, default_value }, loc: self.loc(start) }))
            }
        }
    }
}
