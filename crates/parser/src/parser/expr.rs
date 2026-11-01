//! Expressions.

use super::*;
use TokenType as T;

pub(crate) fn expr_name(e: &Expr) -> &'static str {
    use ExprKind::*;
    match &e.node {
        Attribute { .. } => "attribute",
        Subscript { .. } => "subscript",
        Starred { .. } => "starred",
        Name { .. } => "name",
        List { .. } => "list",
        Tuple { .. } => "tuple",
        Lambda { .. } => "lambda",
        Call { .. } => "function call",
        BoolOp { .. } | BinOp { .. } | UnaryOp { .. } => "expression",
        GeneratorExp { .. } => "generator expression",
        Yield { .. } | YieldFrom { .. } => "yield expression",
        Await { .. } => "await expression",
        ListComp { .. } => "list comprehension",
        SetComp { .. } => "set comprehension",
        DictComp { .. } => "dict comprehension",
        Dict { .. } => "dict literal",
        Set { .. } => "set display",
        JoinedStr { .. } | FormattedValue { .. } => "f-string expression",
        TemplateStr { .. } | Interpolation { .. } => "t-string expression",
        Constant { value, .. } => match value {
            crate::ast::Constant::None => "None",
            crate::ast::Constant::Bool(false) => "False",
            crate::ast::Constant::Bool(true) => "True",
            crate::ast::Constant::Ellipsis => "ellipsis",
            _ => "literal",
        },
        Compare { .. } => "comparison",
        IfExp { .. } => "conditional expression",
        NamedExpr { .. } => "named expression",
        Slice { .. } => "slice",
    }
}

#[inline]
pub(crate) fn bx(e: Expr) -> Box<Expr> {
    Box::new(e)
}

impl<'a> Parser<'a> {
    pub(crate) fn mk_name(&self, t: &Token, ctx: ExprContext) -> Expr {
        Expr::new(ExprKind::Name { id: self.ident(t), ctx }, Self::tok_loc(t))
    }

    // ================================================================ expression lists

    /// expressions: expression (',' expression)* [',']
    pub(crate) fn expressions(&mut self) -> R<Expr> {
        let start = self.p;
        let Some(a) = self.expression()? else { return Ok(None) };
        if !self.at(T::COMMA) {
            return Ok(Some(a));
        }
        let mut elts = vec![a];
        while self.eat(T::COMMA).is_some() {
            let m = self.p;
            match self.expression()? {
                Some(e) => elts.push(e),
                None => {
                    self.p = m;
                    break;
                }
            }
        }
        Ok(Some(Expr::new(ExprKind::Tuple { elts, ctx: ExprContext::Load }, self.loc(start))))
    }

    /// star_expressions: star_expression (',' star_expression)* [',']
    pub(crate) fn star_expressions(&mut self) -> R<Expr> {
        let start = self.p;
        let Some(a) = self.star_expression()? else { return Ok(None) };
        if !self.at(T::COMMA) {
            return Ok(Some(a));
        }
        let mut elts = vec![a];
        while self.eat(T::COMMA).is_some() {
            let m = self.p;
            match self.star_expression()? {
                Some(e) => elts.push(e),
                None => {
                    self.p = m;
                    break;
                }
            }
        }
        Ok(Some(Expr::new(ExprKind::Tuple { elts, ctx: ExprContext::Load }, self.loc(start))))
    }

    /// star_expression: '*' bitwise_or | expression
    pub(crate) fn star_expression(&mut self) -> R<Expr> {
        let start = self.p;
        if self.at(T::STAR) {
            self.advance();
            match self.bitwise_or()? {
                Some(v) => return Ok(Some(Expr::new(ExprKind::Starred { value: bx(v), ctx: ExprContext::Load }, self.loc(start)))),
                None => {
                    self.p = start;
                    return Ok(None);
                }
            }
        }
        self.expression()
    }

    /// star_named_expressions: ','.star_named_expression+ [',']
    pub(crate) fn star_named_expressions(&mut self) -> R<Vec<Expr>> {
        let Some(a) = self.star_named_expression()? else { return Ok(None) };
        let mut v = vec![a];
        while self.at(T::COMMA) {
            let m = self.p;
            self.advance();
            match self.star_named_expression()? {
                Some(e) => v.push(e),
                None => {
                    self.p = m + 1; // keep trailing comma
                    break;
                }
            }
        }
        Ok(Some(v))
    }

    /// star_named_expression: '*' bitwise_or | named_expression
    pub(crate) fn star_named_expression(&mut self) -> R<Expr> {
        let start = self.p;
        if self.at(T::STAR) {
            self.advance();
            match self.bitwise_or()? {
                Some(v) => {
                    if self.invalid && self.at_kw(Kw::If) {
                        self.invalid_starred_conditional(start)?;
                    }
                    return Ok(Some(Expr::new(ExprKind::Starred { value: bx(v), ctx: ExprContext::Load }, self.loc(start))));
                }
                None => {
                    self.p = start;
                    return Ok(None);
                }
            }
        }
        self.named_expression()
    }

    /// `'*' expression` where the expression is a conditional: "invalid starred expression..."
    /// (also for '**'). `start` is the index of the star token.
    pub(crate) fn invalid_starred_conditional(&mut self, start: usize) -> Result<(), PErr> {
        let m = self.p;
        self.p = start;
        let st = self.advance();
        let save = self.invalid;
        self.invalid = false;
        let e = self.expression()?;
        self.invalid = save;
        if let Some(e) = e {
            if matches!(e.node, ExprKind::IfExp { .. }) {
                let msg = if st.ty == T::STAR {
                    "invalid starred expression. Did you forget to wrap the conditional expression in parentheses?"
                } else {
                    "invalid double starred expression. Did you forget to wrap the conditional expression in parentheses?"
                };
                return Err(self.err_range(msg, Self::tok_loc(&st), e.loc));
            }
        }
        self.p = m;
        Ok(())
    }

    /// In the second pass: `**` at a position where only a dict display allows it.
    /// `what` describes the context ("list comprehension", "generator expression", ...).
    fn invalid_double_star_elt(&mut self, ctx: &str) -> Result<(), PErr> {
        let start = self.p;
        if !self.at(T::DOUBLESTAR) {
            return Ok(());
        }
        let st = self.advance();
        let save = self.invalid;
        self.invalid = false;
        let v = self.bitwise_or()?;
        self.invalid = save;
        if let Some(v) = v {
            if self.at_comprehension_start() {
                return Err(self.err_range(format!("cannot use dict unpacking in {}", ctx), Self::tok_loc(&st), v.loc));
            }
            return Err(self.err_tok("cannot use dict unpacking here", &st));
        }
        self.p = start;
        Ok(())
    }

    /// assignment_expression: NAME ':=' ~ expression
    pub(crate) fn assignment_expression(&mut self) -> R<Expr> {
        let start = self.p;
        let t = self.peek();
        if t.ty == T::NAME && !t.kw.is_hard() && self.peek_n(1).ty == T::COLONEQUAL {
            self.advance();
            self.advance();
            self.check_version(8, "Assignment expressions are")?;
            match self.expression()? {
                Some(v) => {
                    let target = self.mk_name(&t, ExprContext::Store);
                    return Ok(Some(Expr::new(ExprKind::NamedExpr { target: bx(target), value: bx(v) }, self.loc(start))));
                }
                None => {
                    self.p = start;
                    return Ok(None);
                }
            }
        }
        Ok(None)
    }

    /// named_expression: assignment_expression | invalid_named_expression | expression !':='
    pub(crate) fn named_expression(&mut self) -> R<Expr> {
        let start = self.p;
        let t = self.peek();
        if t.ty == T::NAME && !t.kw.is_hard() && self.peek_n(1).ty == T::COLONEQUAL {
            // assignment_expression with cut
            return self.assignment_expression();
        }
        if self.invalid {
            self.invalid_named_expression()?;
            self.p = start;
        }
        let Some(e) = self.expression()? else { return Ok(None) };
        if self.at(T::COLONEQUAL) {
            self.p = start;
            return Ok(None);
        }
        Ok(Some(e))
    }

    // ================================================================ expression

    pub(crate) fn expression(&mut self) -> R<Expr> {
        self.enter()?;
        let r = self.expression_inner();
        self.leave();
        r
    }

    fn expression_inner(&mut self) -> R<Expr> {
        let start = self.p;
        if self.invalid {
            self.invalid_expression()?;
            self.p = start;
            self.invalid_legacy_expression()?;
            self.p = start;
        }
        if self.at_kw(Kw::Lambda) {
            return self.lambdef();
        }
        let Some(a) = self.disjunction()? else { return Ok(None) };
        if self.at_kw(Kw::If) {
            let m = self.p;
            self.advance();
            if let Some(b) = self.disjunction()? {
                if self.eat_kw(Kw::Else).is_some() {
                    if let Some(c) = self.expression()? {
                        return Ok(Some(Expr::new(ExprKind::IfExp { test: bx(b), body: bx(a), orelse: bx(c) }, self.loc(start))));
                    }
                }
            }
            self.p = m;
        }
        Ok(Some(a))
    }

    /// yield_expr: 'yield' 'from' expression | 'yield' [star_expressions]
    pub(crate) fn yield_expr(&mut self) -> R<Expr> {
        let start = self.p;
        if self.eat_kw(Kw::Yield).is_none() {
            return Ok(None);
        }
        if self.eat_kw(Kw::From).is_some() {
            match self.expression()? {
                Some(e) => return Ok(Some(Expr::new(ExprKind::YieldFrom { value: bx(e) }, self.loc(start)))),
                None => {
                    self.p = start;
                    return Ok(None);
                }
            }
        }
        let m = self.p;
        let v = match self.star_expressions()? {
            Some(e) => Some(bx(e)),
            None => {
                self.p = m;
                None
            }
        };
        Ok(Some(Expr::new(ExprKind::Yield { value: v }, self.loc(start))))
    }

    pub(crate) fn disjunction(&mut self) -> R<Expr> {
        let start = self.p;
        let Some(a) = self.conjunction()? else { return Ok(None) };
        if !self.at_kw(Kw::Or) {
            return Ok(Some(a));
        }
        let mut values = vec![a];
        while self.at_kw(Kw::Or) {
            let m = self.p;
            self.advance();
            match self.conjunction()? {
                Some(b) => values.push(b),
                None => {
                    self.p = m;
                    break;
                }
            }
        }
        if values.len() == 1 {
            return Ok(values.pop());
        }
        Ok(Some(Expr::new(ExprKind::BoolOp { op: BoolOp::Or, values }, self.loc(start))))
    }

    fn conjunction(&mut self) -> R<Expr> {
        let start = self.p;
        let Some(a) = self.inversion()? else { return Ok(None) };
        if !self.at_kw(Kw::And) {
            return Ok(Some(a));
        }
        let mut values = vec![a];
        while self.at_kw(Kw::And) {
            let m = self.p;
            self.advance();
            match self.inversion()? {
                Some(b) => values.push(b),
                None => {
                    self.p = m;
                    break;
                }
            }
        }
        if values.len() == 1 {
            return Ok(values.pop());
        }
        Ok(Some(Expr::new(ExprKind::BoolOp { op: BoolOp::And, values }, self.loc(start))))
    }

    fn inversion(&mut self) -> R<Expr> {
        let start = self.p;
        if self.at_kw(Kw::Not) {
            self.advance();
            self.enter()?;
            let r = self.inversion();
            self.leave();
            match r? {
                Some(e) => return Ok(Some(Expr::new(ExprKind::UnaryOp { op: UnaryOp::Not, operand: bx(e) }, self.loc(start)))),
                None => {
                    self.p = start;
                    return Ok(None);
                }
            }
        }
        self.comparison()
    }

    fn comparison(&mut self) -> R<Expr> {
        let start = self.p;
        let Some(a) = self.bitwise_or()? else { return Ok(None) };
        let mut ops = Vec::new();
        let mut comparators = Vec::new();
        loop {
            let m = self.p;
            let t = self.peek();
            let op = match t.ty {
                T::EQEQUAL => CmpOp::Eq,
                T::NOTEQUAL => {
                    // check_barry_as_flufl
                    let txt = self.text(&t);
                    if self.flufl {
                        if txt == "!=" {
                            return Err(self.err_tok("with Barry as BDFL, use '<>' instead of '!='", &t));
                        }
                    } else if txt == "<>" {
                        if self.invalid {
                            return Err(self.err_tok("invalid syntax. Maybe you meant '!=' instead of '<>'?", &t));
                        }
                        break;
                    }
                    CmpOp::NotEq
                }
                T::LESSEQUAL => CmpOp::LtE,
                T::LESS => CmpOp::Lt,
                T::GREATEREQUAL => CmpOp::GtE,
                T::GREATER => CmpOp::Gt,
                T::NAME if t.kw == Kw::In => CmpOp::In,
                T::NAME if t.kw == Kw::Not => {
                    if self.peek_n(1).ty == T::NAME && self.peek_n(1).kw == Kw::In {
                        self.advance();
                        CmpOp::NotIn
                    } else {
                        break;
                    }
                }
                T::NAME if t.kw == Kw::Is => {
                    if self.peek_n(1).ty == T::NAME && self.peek_n(1).kw == Kw::Not {
                        self.advance();
                        CmpOp::IsNot
                    } else {
                        CmpOp::Is
                    }
                }
                _ => break,
            };
            self.advance();
            match self.bitwise_or()? {
                Some(b) => {
                    ops.push(op);
                    comparators.push(b);
                }
                None => {
                    self.p = m;
                    break;
                }
            }
        }
        if ops.is_empty() {
            return Ok(Some(a));
        }
        Ok(Some(Expr::new(ExprKind::Compare { left: bx(a), ops, comparators }, self.loc(start))))
    }

    /// bitwise_or .. term: the six left-associative binary operator levels of the grammar,
    /// implemented with precedence climbing (same trees and locations).
    pub(crate) fn bitwise_or(&mut self) -> R<Expr> {
        self.binop_expr(1)
    }

    #[inline]
    fn binop_info(ty: TokenType) -> Option<(u8, Operator)> {
        Some(match ty {
            T::VBAR => (1, Operator::BitOr),
            T::CIRCUMFLEX => (2, Operator::BitXor),
            T::AMPER => (3, Operator::BitAnd),
            T::LEFTSHIFT => (4, Operator::LShift),
            T::RIGHTSHIFT => (4, Operator::RShift),
            T::PLUS => (5, Operator::Add),
            T::MINUS => (5, Operator::Sub),
            T::STAR => (6, Operator::Mult),
            T::SLASH => (6, Operator::Div),
            T::DOUBLESLASH => (6, Operator::FloorDiv),
            T::PERCENT => (6, Operator::Mod),
            T::AT => (6, Operator::MatMult),
            _ => return None,
        })
    }

    fn binop_expr(&mut self, min_prec: u8) -> R<Expr> {
        let start = self.p;
        let Some(mut left) = self.factor()? else { return Ok(None) };
        let mut chain = 0usize;
        loop {
            chain += 1;
            if chain > 64 {
                self.check_chain(chain)?;
            }
            let Some((prec, op)) = Self::binop_info(self.peek_ty()) else { break };
            if prec < min_prec {
                break;
            }
            let m = self.p;
            self.advance();
            if op == Operator::MatMult {
                self.check_version(5, "The '@' operator is")?;
            }
            let right = if prec == 6 {
                self.factor()?
            } else {
                self.enter()?;
                let r = self.binop_expr(prec + 1);
                self.leave();
                r?
            };
            match right {
                Some(r) => left = Expr::new(ExprKind::BinOp { left: bx(left), op, right: bx(r) }, self.loc(start)),
                None => {
                    self.p = m;
                    if self.invalid && prec >= 5 {
                        self.invalid_arithmetic()?;
                        self.p = m;
                    }
                    break;
                }
            }
        }
        Ok(Some(left))
    }

    /// `op 'not' inversion` after an arithmetic operator (current token is the operator).
    fn invalid_arithmetic(&mut self) -> Result<(), PErr> {
        let m = self.p;
        if matches!(self.peek_ty(), T::PLUS | T::MINUS | T::STAR | T::SLASH | T::PERCENT | T::DOUBLESLASH | T::AT) {
            self.advance();
            if let Some(nt) = self.eat_kw(Kw::Not) {
                if let Some(b) = self.inversion()? {
                    return Err(self.err_range("'not' after an operator must be parenthesized", Self::tok_loc(&nt), b.loc));
                }
            }
        }
        self.p = m;
        Ok(())
    }

    fn factor(&mut self) -> R<Expr> {
        let start = self.p;
        let op = match self.peek_ty() {
            T::PLUS => UnaryOp::UAdd,
            T::MINUS => UnaryOp::USub,
            T::TILDE => UnaryOp::Invert,
            _ => return self.power(),
        };
        self.advance();
        if self.invalid {
            if let Some(nt) = self.eat_kw(Kw::Not) {
                if let Some(b) = self.factor_rec()? {
                    return Err(self.err_range("'not' after an operator must be parenthesized", Self::tok_loc(&nt), b.loc));
                }
                self.p = start + 1;
            }
        }
        match self.factor_rec()? {
            Some(e) => Ok(Some(Expr::new(ExprKind::UnaryOp { op, operand: bx(e) }, self.loc(start)))),
            None => {
                self.p = start;
                Ok(None)
            }
        }
    }

    fn factor_rec(&mut self) -> R<Expr> {
        self.enter()?;
        let r = self.factor();
        self.leave();
        r
    }

    fn power(&mut self) -> R<Expr> {
        let start = self.p;
        let Some(a) = self.await_primary()? else { return Ok(None) };
        if self.at(T::DOUBLESTAR) {
            let m = self.p;
            self.advance();
            match self.factor_rec()? {
                Some(b) => return Ok(Some(Expr::new(ExprKind::BinOp { left: bx(a), op: Operator::Pow, right: bx(b) }, self.loc(start)))),
                None => self.p = m,
            }
        }
        Ok(Some(a))
    }

    fn await_primary(&mut self) -> R<Expr> {
        let start = self.p;
        if self.at_kw(Kw::Await) {
            self.advance();
            self.check_version(5, "Await expressions are")?;
            match self.primary()? {
                Some(e) => return Ok(Some(Expr::new(ExprKind::Await { value: bx(e) }, self.loc(start)))),
                None => {
                    self.p = start;
                    return Ok(None);
                }
            }
        }
        self.primary()
    }

    /// primary: atom followed by trailers ('.' NAME | genexp | '(' args ')' | '[' slices ']')
    pub(crate) fn primary(&mut self) -> R<Expr> {
        let start = self.p;
        let Some(mut a) = self.atom()? else { return Ok(None) };
        let mut chain = 0usize;
        loop {
            chain += 1;
            if chain > 64 {
                self.check_chain(chain)?;
            }
            match self.peek_ty() {
                T::DOT => {
                    let m = self.p;
                    self.advance();
                    match self.eat_name_any() {
                        Some(n) => {
                            let attr = self.ident(&n);
                            a = Expr::new(ExprKind::Attribute { value: bx(a), attr, ctx: ExprContext::Load }, self.loc(start));
                        }
                        None => {
                            self.p = m;
                            break;
                        }
                    }
                }
                T::LPAR => {
                    let m = self.p;
                    match self.call_trailer(start)? {
                        Some((args, keywords)) => {
                            a = Expr::new(ExprKind::Call { func: bx(a), args, keywords }, self.loc(start));
                        }
                        None => {
                            self.p = m;
                            break;
                        }
                    }
                }
                T::LSQB => {
                    let m = self.p;
                    self.advance();
                    match self.slices()? {
                        Some(s) => {
                            if self.eat(T::RSQB).is_some() {
                                a = Expr::new(ExprKind::Subscript { value: bx(a), slice: bx(s), ctx: ExprContext::Load }, self.loc(start));
                            } else {
                                self.p = m;
                                break;
                            }
                        }
                        None => {
                            self.p = m;
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
        Ok(Some(a))
    }

    /// A NAME after '.', which may be any identifier (but not a hard keyword).
    pub(crate) fn eat_name_any(&mut self) -> Option<Token> {
        self.eat_name()
    }

    /// '(' [arguments] ')' for class bases (no bare generator expression).
    pub(crate) fn call_args_only(&mut self) -> R<(Vec<Expr>, Vec<Keyword>)> {
        self.no_genexp_arg = true;
        let r = self.call_trailer(0);
        self.no_genexp_arg = false;
        r
    }

    /// '(' ... ')' after a primary: either a bare generator expression argument or arguments.
    fn call_trailer(&mut self, _start: usize) -> R<(Vec<Expr>, Vec<Keyword>)> {
        let allow_genexp = !std::mem::replace(&mut self.no_genexp_arg, false);
        let lpar = self.p;
        self.advance(); // '('
        if self.eat(T::RPAR).is_some() {
            return Ok(Some((Vec::new(), Vec::new())));
        }
        self.enter()?;
        let r = self.arguments_after_lpar(lpar, allow_genexp);
        self.leave();
        let Some((args, kws)) = r? else { return Ok(None) };
        if self.eat(T::RPAR).is_none() {
            if self.invalid {
                // invalid_arguments-style errors for unparenthesized generators etc.
            }
            return Ok(None);
        }
        Ok(Some((args, kws)))
    }

    // ================================================================ slices

    /// slices: slice !',' | ','.(slice | starred_expression)+ [',']
    fn slices(&mut self) -> R<Expr> {
        let start = self.p;
        let first = if self.at(T::STAR) { self.starred_expression()? } else { self.slice()? };
        let Some(a) = first else { return Ok(None) };
        if !self.at(T::COMMA) {
            if matches!(a.node, ExprKind::Starred { .. }) {
                // a lone starred expression is a 1-tuple only with a comma
                return Ok(Some(Expr::new(ExprKind::Tuple { elts: vec![a], ctx: ExprContext::Load }, self.loc(start))));
            }
            return Ok(Some(a));
        }
        let mut elts = vec![a];
        while self.at(T::COMMA) {
            let m = self.p;
            self.advance();
            let e = if self.at(T::STAR) { self.starred_expression()? } else { self.slice()? };
            match e {
                Some(e) => elts.push(e),
                None => {
                    self.p = m + 1;
                    break;
                }
            }
        }
        Ok(Some(Expr::new(ExprKind::Tuple { elts, ctx: ExprContext::Load }, self.loc(start))))
    }

    /// slice: [expression] ':' [expression] [':' [expression]] | named_expression
    fn slice(&mut self) -> R<Expr> {
        let start = self.p;
        let mut lower = None;
        if !self.at(T::COLON) {
            match self.expression()? {
                Some(e) => lower = Some(e),
                None => {
                    self.p = start;
                    return Ok(None);
                }
            }
        }
        if self.at(T::COLON) {
            self.advance();
            let m = self.p;
            let upper = match self.expression()? {
                Some(e) => Some(bx(e)),
                None => {
                    self.p = m;
                    None
                }
            };
            let mut step = None;
            if self.at(T::COLON) {
                self.advance();
                let m = self.p;
                step = match self.expression()? {
                    Some(e) => Some(bx(e)),
                    None => {
                        self.p = m;
                        None
                    }
                };
            }
            return Ok(Some(Expr::new(ExprKind::Slice { lower: lower.map(bx), upper, step }, self.loc(start))));
        }
        // named_expression alternative
        self.p = start;
        self.named_expression()
    }

    /// starred_expression: '*' expression
    pub(crate) fn starred_expression(&mut self) -> R<Expr> {
        let start = self.p;
        if self.eat(T::STAR).is_none() {
            return Ok(None);
        }
        match self.expression()? {
            Some(e) => Ok(Some(Expr::new(ExprKind::Starred { value: bx(e), ctx: ExprContext::Load }, self.loc(start)))),
            None => {
                if self.invalid {
                    // invalid_starred_expression: '*' -> "Invalid star expression"
                    let t = self.tok_at(start);
                    if !self.at(T::EQUAL) {
                        return Err(self.err_tok("Invalid star expression", &t));
                    }
                }
                self.p = start;
                Ok(None)
            }
        }
    }

    // ================================================================ atoms

    pub(crate) fn atom(&mut self) -> R<Expr> {
        let t = self.peek();
        match t.ty {
            T::NAME => {
                let e = match t.kw {
                    Kw::True => Expr::new(ExprKind::Constant { value: Constant::Bool(true), kind: None }, Self::tok_loc(&t)),
                    Kw::False => Expr::new(ExprKind::Constant { value: Constant::Bool(false), kind: None }, Self::tok_loc(&t)),
                    Kw::NoneKw => Expr::new(ExprKind::Constant { value: Constant::None, kind: None }, Self::tok_loc(&t)),
                    k if k.is_hard() => return Ok(None),
                    _ => self.mk_name(&t, ExprContext::Load),
                };
                self.advance();
                Ok(Some(e))
            }
            T::NUMBER => {
                self.advance();
                let v = self.parse_number(&t)?;
                Ok(Some(Expr::new(ExprKind::Constant { value: v, kind: None }, Self::tok_loc(&t))))
            }
            T::STRING | T::FSTRING_START | T::TSTRING_START => self.strings(),
            T::LPAR => {
                self.enter()?;
                let r = self.paren_atom();
                self.leave();
                r
            }
            T::LSQB => {
                self.enter()?;
                let r = self.list_atom();
                self.leave();
                r
            }
            T::LBRACE => {
                self.enter()?;
                let r = self.brace_atom();
                self.leave();
                r
            }
            T::ELLIPSIS => {
                self.advance();
                Ok(Some(Expr::new(ExprKind::Constant { value: Constant::Ellipsis, kind: None }, Self::tok_loc(&t))))
            }
            _ => Ok(None),
        }
    }

    /// Parse the element of a genexp / group: assignment_expression | expression !':='
    /// (plus PEP 798 starred element).
    fn group_element(&mut self) -> R<Expr> {
        let t = self.peek();
        if t.ty == T::NAME && !t.kw.is_hard() && self.peek_n(1).ty == T::COLONEQUAL {
            return self.assignment_expression();
        }
        self.named_expression()
    }

    /// '(' ... ')': tuple | group | genexp
    fn paren_atom(&mut self) -> R<Expr> {
        let start = self.p;
        self.advance(); // '('
        if self.eat(T::RPAR).is_some() {
            return Ok(Some(Expr::new(ExprKind::Tuple { elts: Vec::new(), ctx: ExprContext::Load }, self.loc(start))));
        }
        if self.at_kw(Kw::Yield) {
            let Some(y) = self.yield_expr()? else {
                self.p = start;
                return Ok(None);
            };
            if self.eat(T::RPAR).is_some() {
                return Ok(Some(y));
            }
            self.p = start;
            return Ok(None);
        }
        let first_start = self.p;
        let first = if self.at(T::STAR) { self.star_named_expression()? } else { self.group_element()? };
        let Some(first) = first else {
            if self.invalid {
                self.p = first_start;
                self.invalid_double_star_elt("generator expression")?;
                self.p = start;
                self.invalid_group()?;
            }
            self.p = start;
            return Ok(None);
        };
        match self.peek_ty() {
            T::COMMA => {
                // tuple
                let mut elts = vec![first];
                while self.at(T::COMMA) {
                    let m = self.p;
                    self.advance();
                    match self.star_named_expression()? {
                        Some(e) => elts.push(e),
                        None => {
                            self.p = m + 1;
                            break;
                        }
                    }
                }
                if self.eat(T::RPAR).is_some() {
                    return Ok(Some(Expr::new(ExprKind::Tuple { elts, ctx: ExprContext::Load }, self.loc(start))));
                }
                self.p = start;
                Ok(None)
            }
            T::RPAR => {
                if let ExprKind::Starred { .. } = first.node {
                    if self.invalid {
                        return Err(self.err_loc("cannot use starred expression here", first.loc));
                    }
                    self.p = start;
                    return Ok(None);
                }
                self.advance();
                Ok(Some(first))
            }
            T::NAME if self.at_comprehension_start() => {
                let _ = first_start;
                let Some(generators) = self.for_if_clauses()? else {
                    self.p = start;
                    return Ok(None);
                };
                if self.eat(T::RPAR).is_some() {
                    return Ok(Some(Expr::new(ExprKind::GeneratorExp { elt: bx(first), generators }, self.loc(start))));
                }
                if self.invalid {
                    // fallthrough to generic error
                }
                self.p = start;
                Ok(None)
            }
            _ => {
                if self.invalid {
                    self.p = start;
                    self.invalid_group()?;
                }
                self.p = start;
                Ok(None)
            }
        }
    }

    pub(crate) fn at_comprehension_start(&mut self) -> bool {
        let t = self.peek();
        if t.ty != T::NAME {
            return false;
        }
        if t.kw == Kw::For {
            return true;
        }
        if t.kw == Kw::Async {
            let t2 = self.peek_n(1);
            return t2.ty == T::NAME && t2.kw == Kw::For;
        }
        false
    }

    /// '[' ... ']': list | listcomp
    fn list_atom(&mut self) -> R<Expr> {
        let start = self.p;
        self.advance();
        if self.eat(T::RSQB).is_some() {
            return Ok(Some(Expr::new(ExprKind::List { elts: Vec::new(), ctx: ExprContext::Load }, self.loc(start))));
        }
        let Some(first) = self.star_named_expression()? else {
            if self.invalid {
                self.p = start + 1;
                self.invalid_double_star_elt("list comprehension")?;
            }
            self.p = start;
            return Ok(None);
        };
        if self.at_comprehension_start() {
            let Some(generators) = self.for_if_clauses()? else {
                self.p = start;
                return Ok(None);
            };
            if self.eat(T::RSQB).is_some() {
                return Ok(Some(Expr::new(ExprKind::ListComp { elt: bx(first), generators }, self.loc(start))));
            }
            self.p = start;
            return Ok(None);
        }
        let mut elts = vec![first];
        while self.at(T::COMMA) {
            let m = self.p;
            self.advance();
            match self.star_named_expression()? {
                Some(e) => elts.push(e),
                None => {
                    self.p = m + 1;
                    break;
                }
            }
        }
        if self.eat(T::RSQB).is_some() {
            return Ok(Some(Expr::new(ExprKind::List { elts, ctx: ExprContext::Load }, self.loc(start))));
        }
        if self.invalid && elts.len() > 1 && self.at_comprehension_start() {
            // [x, y for ...]
            let a = elts[0].loc;
            let b = elts.last().unwrap().loc;
            return Err(self.err_range("did you forget parentheses around the comprehension target?", a, b));
        }
        self.p = start;
        Ok(None)
    }

    /// '{' ... '}': dict | set | dictcomp | setcomp
    fn brace_atom(&mut self) -> R<Expr> {
        let start = self.p;
        self.advance();
        if self.eat(T::RBRACE).is_some() {
            return Ok(Some(Expr::new(ExprKind::Dict { keys: Vec::new(), values: Vec::new() }, self.loc(start))));
        }
        // first item
        if self.at(T::DOUBLESTAR) {
            // dict with **, or PEP 798 dict comprehension {**d for ...}
            let m = self.p;
            let st = self.advance();
            let Some(v) = self.bitwise_or()? else {
                self.p = start;
                return Ok(None);
            };
            if self.invalid {
                if self.at_kw(Kw::If) {
                    self.invalid_starred_conditional(m)?;
                }
                if self.at(T::COLON) {
                    return Err(self.err_range("cannot use dict unpacking in a dictionary key", Self::tok_loc(&st), v.loc));
                }
            }
            if self.at_comprehension_start() {
                let Some(generators) = self.for_if_clauses()? else {
                    self.p = start;
                    return Ok(None);
                };
                if self.eat(T::RBRACE).is_some() {
                    return Ok(Some(Expr::new(ExprKind::DictComp { key: bx(v), value: None, generators }, self.loc(start))));
                }
                if self.invalid && self.at(T::COLON) {
                    return Err(self.err_tok("cannot use dict unpacking here", &st));
                }
                self.p = start;
                return Ok(None);
            }
            return self.dict_rest(start, None, v);
        }
        if self.at(T::STAR) {
            let Some(first) = self.star_named_expression()? else {
                self.p = start;
                return Ok(None);
            };
            if self.invalid && self.at(T::COLON) {
                return Err(self.err_loc("cannot use a starred expression in a dictionary key", first.loc));
            }
            return self.set_rest(start, first);
        }
        let t = self.peek();
        if t.ty == T::NAME && !t.kw.is_hard() && self.peek_n(1).ty == T::COLONEQUAL {
            let Some(first) = self.assignment_expression()? else {
                self.p = start;
                return Ok(None);
            };
            return self.set_rest(start, first);
        }
        let Some(first) = self.expression()? else {
            self.p = start;
            return Ok(None);
        };
        if self.at(T::COLON) {
            self.advance();
            let Some(v) = self.expression()? else {
                if self.invalid {
                    if self.at(T::DOUBLESTAR) {
                        let st = self.advance();
                        let save = self.invalid;
                        self.invalid = false;
                        let e = self.bitwise_or()?;
                        self.invalid = save;
                        if let Some(e) = e {
                            return Err(self.err_range("cannot use dict unpacking in a dictionary value", Self::tok_loc(&st), e.loc));
                        }
                    }
                    self.p = start;
                    self.invalid_double_starred_kvpairs()?;
                }
                self.p = start;
                return Ok(None);
            };
            if self.at_comprehension_start() {
                let Some(generators) = self.for_if_clauses()? else {
                    self.p = start;
                    return Ok(None);
                };
                if self.eat(T::RBRACE).is_some() {
                    return Ok(Some(Expr::new(ExprKind::DictComp { key: bx(first), value: Some(bx(v)), generators }, self.loc(start))));
                }
                self.p = start;
                return Ok(None);
            }
            return self.dict_rest(start, Some(first), v);
        }
        if self.at(T::COLONEQUAL) {
            self.p = start;
            return Ok(None);
        }
        if self.invalid && !matches!(self.peek_ty(), T::COMMA | T::RBRACE) && !self.at_comprehension_start() {
            // dict failed; the set alternative parses a named_expression here
            let m = self.p;
            self.p = start;
            self.invalid_double_starred_kvpairs()?;
            self.p = start + 1;
            self.invalid_named_expression()?;
            self.p = m;
        }
        self.set_rest(start, first)
    }

    fn set_rest(&mut self, start: usize, first: Expr) -> R<Expr> {
        if self.at_comprehension_start() {
            let Some(generators) = self.for_if_clauses()? else {
                self.p = start;
                return Ok(None);
            };
            if self.eat(T::RBRACE).is_some() {
                return Ok(Some(Expr::new(ExprKind::SetComp { elt: bx(first), generators }, self.loc(start))));
            }
            self.p = start;
            return Ok(None);
        }
        let mut elts = vec![first];
        while self.at(T::COMMA) {
            let m = self.p;
            self.advance();
            match self.star_named_expression()? {
                Some(e) => elts.push(e),
                None => {
                    self.p = m + 1;
                    break;
                }
            }
        }
        if self.eat(T::RBRACE).is_some() {
            return Ok(Some(Expr::new(ExprKind::Set { elts }, self.loc(start))));
        }
        if self.invalid && elts.len() > 1 && self.at_comprehension_start() {
            let a = elts[0].loc;
            let b = elts.last().unwrap().loc;
            return Err(self.err_range("did you forget parentheses around the comprehension target?", a, b));
        }
        self.p = start;
        Ok(None)
    }

    fn dict_rest(&mut self, start: usize, k: Option<Expr>, v: Expr) -> R<Expr> {
        let mut keys = vec![k];
        let mut values = vec![v];
        while self.at(T::COMMA) {
            let m = self.p;
            self.advance();
            if self.at(T::DOUBLESTAR) {
                self.advance();
                match self.bitwise_or()? {
                    Some(v) => {
                        keys.push(None);
                        values.push(v);
                        continue;
                    }
                    None => {
                        self.p = m + 1;
                        break;
                    }
                }
            }
            let m2 = self.p;
            let Some(k) = self.expression()? else {
                self.p = m + 1;
                break;
            };
            if self.eat(T::COLON).is_none() {
                self.p = m2;
                self.p = m + 1;
                break;
            }
            let Some(v) = self.expression()? else {
                self.p = m + 1;
                break;
            };
            keys.push(Some(k));
            values.push(v);
        }
        if self.eat(T::RBRACE).is_some() {
            return Ok(Some(Expr::new(ExprKind::Dict { keys, values }, self.loc(start))));
        }
        if self.invalid {
            self.p = start;
            self.invalid_double_starred_kvpairs()?;
        }
        self.p = start;
        Ok(None)
    }

    // ================================================================ comprehensions

    /// for_if_clauses: for_if_clause+
    pub(crate) fn for_if_clauses(&mut self) -> R<Vec<Comprehension>> {
        let mut v = Vec::new();
        while self.at_comprehension_start() {
            let m = self.p;
            let is_async = if self.at_kw(Kw::Async) {
                self.advance();
                self.check_version(6, "Async comprehensions are")?;
                1
            } else {
                0
            };
            self.advance(); // 'for'
            let tstart = self.p;
            let target = self.star_targets()?;
            if target.is_none() || !self.at_kw(Kw::In) {
                if self.invalid {
                    self.p = tstart;
                    self.invalid_for_if_clause()?;
                    self.p = tstart;
                    self.invalid_for_target_tail(TargetsType::For)?;
                }
                self.p = m;
                break;
            }
            let target = target.unwrap();
            self.advance(); // 'in'
            // cut: after 'in' a failure fails the comprehension
            let Some(iter) = self.disjunction()? else {
                self.p = m;
                return Ok(None);
            };
            let mut ifs = Vec::new();
            while self.at_kw(Kw::If) {
                let m2 = self.p;
                self.advance();
                match self.disjunction()? {
                    Some(c) => ifs.push(c),
                    None => {
                        self.p = m2;
                        break;
                    }
                }
            }
            v.push(Comprehension { target, iter, ifs, is_async });
        }
        if v.is_empty() {
            return Ok(None);
        }
        Ok(Some(v))
    }

    /// invalid_for_if_clause: 'async'? 'for' (bitwise_or (',' bitwise_or)* [',']) !'in'
    /// (called right after 'for').
    fn invalid_for_if_clause(&mut self) -> Result<(), PErr> {
        let save = self.invalid;
        self.invalid = false;
        let mut any = false;
        loop {
            let m = self.p;
            if self.bitwise_or()?.is_none() {
                self.p = m;
                break;
            }
            any = true;
            if self.eat(T::COMMA).is_none() {
                break;
            }
        }
        self.invalid = save;
        if any && !self.at_kw(Kw::In) {
            self.peek();
            return Err(self.err_last(ErrorKind::Syntax, "'in' expected after for-loop variables"));
        }
        Ok(())
    }

    // ================================================================ arguments

    /// After '(' of a call: either a bare generator expression or argument list (the ')' is
    /// left for the caller).
    fn arguments_after_lpar(&mut self, lpar: usize, allow_genexp: bool) -> R<(Vec<Expr>, Vec<Keyword>)> {
        let mut args: Vec<Expr> = Vec::new();
        let mut keywords: Vec<Keyword> = Vec::new();
        let mut first = true;
        loop {
            let istart = self.p;
            let t = self.peek();
            if t.ty == T::RPAR && !first {
                break;
            }
            if t.ty == T::STAR {
                // starred_expression
                let Some(e) = self.starred_expression()? else {
                    if self.invalid {
                        self.p = lpar;
                        self.invalid_arguments_at(lpar)?;
                    }
                    return Ok(None);
                };
                if self.invalid && self.at(T::EQUAL) {
                    return Err(self.err_range("cannot assign to iterable argument unpacking", e.loc, e.loc));
                }
                if keywords.iter().any(|k| k.arg.is_none()) {
                    if self.invalid {
                        let comma = self.tok_at(istart - 1);
                        return Err(self.err_tok("iterable argument unpacking follows keyword argument unpacking", &comma));
                    }
                    return Ok(None);
                }
                if first && allow_genexp && self.at_comprehension_start() {
                    // PEP 798: f(*x for x in y)
                    let Some(generators) = self.for_if_clauses()? else { return Ok(None) };
                    if self.at(T::RPAR) {
                        let rp = self.peek();
                        let l = self.start_pos(lpar);
                        let loc = Loc { lineno: l.0, col_offset: l.1, end_lineno: rp.end_lineno as i32, end_col_offset: rp.end_col };
                        args.push(Expr::new(ExprKind::GeneratorExp { elt: bx(e), generators }, loc));
                        return Ok(Some((args, keywords)));
                    }
                    return Ok(None);
                }
                args.push(e);
            } else if t.ty == T::DOUBLESTAR {
                let st = self.advance();
                let Some(v) = self.expression()? else {
                    self.p = istart;
                    if self.invalid {
                        self.p = lpar;
                        self.invalid_arguments_at(lpar)?;
                    }
                    return Ok(None);
                };
                if self.invalid && self.at(T::EQUAL) {
                    return Err(self.err_range("cannot assign to keyword argument unpacking", self.loc(istart), self.loc(istart)));
                }
                if self.at_comprehension_start() {
                    if self.invalid {
                        return Err(self.err_range("cannot use dict unpacking in generator expression", Self::tok_loc(&st), v.loc));
                    }
                    return Ok(None);
                }
                keywords.push(Keyword { arg: None, value: v, loc: self.loc(istart) });
            } else if t.ty == T::NAME && !t.kw.is_hard() && self.peek_n(1).ty == T::EQUAL {
                // keyword argument
                self.advance();
                self.advance();
                let Some(v) = self.expression()? else {
                    if self.invalid {
                        // "expected argument value expression"
                        let eq = self.tok_at(istart + 1);
                        if matches!(self.peek_ty(), T::COMMA | T::RPAR) {
                            return Err(self.err_range("expected argument value expression", Self::tok_loc(&t), Self::tok_loc(&eq)));
                        }
                        self.p = lpar;
                        self.invalid_arguments_at(lpar)?;
                    }
                    return Ok(None);
                };
                keywords.push(Keyword { arg: Some(self.ident(&t)), value: v, loc: self.loc(istart) });
            } else {
                if self.invalid {
                    self.invalid_kwarg(istart)?;
                    self.p = istart;
                }
                let e = if t.ty == T::NAME && !t.kw.is_hard() && self.peek_n(1).ty == T::COLONEQUAL {
                    self.assignment_expression()?
                } else {
                    let r = self.expression()?;
                    if r.is_some() && self.at(T::COLONEQUAL) {
                        self.p = istart;
                        None
                    } else {
                        r
                    }
                };
                let Some(e) = e else {
                    if self.invalid {
                        self.p = lpar;
                        self.invalid_arguments_at(lpar)?;
                    }
                    return Ok(None);
                };
                if first && allow_genexp && self.at_comprehension_start() {
                    // bare generator expression argument: primary genexp
                    let Some(generators) = self.for_if_clauses()? else { return Ok(None) };
                    if self.at(T::RPAR) {
                        // location from '(' to ')'
                        let rp = self.peek();
                        let l = self.start_pos(lpar);
                        let loc = Loc { lineno: l.0, col_offset: l.1, end_lineno: rp.end_lineno as i32, end_col_offset: rp.end_col };
                        args.push(Expr::new(ExprKind::GeneratorExp { elt: bx(e), generators }, loc));
                        return Ok(Some((args, keywords)));
                    }
                    if self.invalid {
                        self.p = lpar;
                        self.invalid_arguments_at(lpar)?;
                    }
                    return Ok(None);
                }
                if self.at(T::EQUAL) {
                    // e.g. f(x()=1) -> invalid_kwarg
                    if self.invalid {
                        self.p = istart;
                        self.invalid_kwarg(istart)?;
                    }
                    return Ok(None);
                }
                if !keywords.is_empty() {
                    // positional argument after keyword arguments
                    if self.invalid {
                        self.p = lpar;
                        self.invalid_arguments_at(lpar)?;
                    }
                    return Ok(None);
                }
                if self.at_comprehension_start() {
                    if self.invalid {
                        self.p = lpar;
                        self.invalid_arguments_at(lpar)?;
                    }
                    return Ok(None);
                }
                args.push(e);
            }
            first = false;
            if self.eat(T::COMMA).is_none() {
                break;
            }
        }
        if !self.at(T::RPAR) {
            if self.invalid {
                let save = self.p;
                self.p = lpar;
                self.invalid_arguments_at(lpar)?;
                self.p = save;
            }
            return Ok(None);
        }
        Ok(Some((args, keywords)))
    }

    // ================================================================ lambda

    fn lambdef(&mut self) -> R<Expr> {
        let start = self.p;
        self.advance(); // 'lambda'
        let args = if self.at(T::COLON) {
            Arguments::default()
        } else {
            match self.lambda_params()? {
                Some(a) => a,
                None => {
                    self.p = start;
                    return Ok(None);
                }
            }
        };
        if self.eat(T::COLON).is_none() {
            self.p = start;
            return Ok(None);
        }
        self.in_lambda += 1;
        let body = self.expression();
        self.in_lambda -= 1;
        match body? {
            Some(b) => Ok(Some(Expr::new(ExprKind::Lambda { args: Box::new(args), body: bx(b) }, self.loc(start)))),
            None => {
                self.p = start;
                Ok(None)
            }
        }
    }

    // ================================================================ numbers

    pub(crate) fn parse_number(&self, t: &Token) -> Result<Constant, PErr> {
        let s = self.text(t);
        if s.contains('_') {
            self.check_version(6, "Underscores in numeric literals are")?;
        }
        parse_number_literal(s).map_err(|m| self.err_tok(m, t))
    }
}

/// Convert a NUMBER token text to a constant.
pub fn parse_number_literal(s: &str) -> Result<Constant, String> {
    let clean: std::borrow::Cow<str> = if s.contains('_') { s.replace('_', "").into() } else { s.into() };
    let b = clean.as_bytes();
    let last = *b.last().unwrap();
    if last == b'j' || last == b'J' {
        let f: f64 = parse_float(&clean[..clean.len() - 1]);
        return Ok(Constant::Complex { real: 0.0, imag: f });
    }
    if b.len() > 1 && b[0] == b'0' && matches!(b[1], b'x' | b'X' | b'o' | b'O' | b'b' | b'B') {
        let radix = match b[1] {
            b'x' | b'X' => 16,
            b'o' | b'O' => 8,
            _ => 2,
        };
        let digits = &clean[2..];
        if let Ok(v) = i64::from_str_radix(digits, radix) {
            return Ok(Constant::Int(Int::Small(v)));
        }
        return Ok(Constant::Int(Int::Big(radix_to_decimal(digits, radix))));
    }
    if clean.contains(['.', 'e', 'E']) {
        return Ok(Constant::Float(parse_float(&clean)));
    }
    if let Ok(v) = clean.parse::<i64>() {
        return Ok(Constant::Int(Int::Small(v)));
    }
    let t = clean.trim_start_matches('0');
    if t.len() > 4300 {
        return Err(format!(
            "Exceeds the limit (4300 digits) for integer string conversion: value has {} digits; use sys.set_int_max_str_digits() to increase the limit - Consider hexadecimal for huge integer literals to avoid decimal conversion limits.",
            t.len()
        ));
    }
    Ok(Constant::Int(Int::Big(if t.is_empty() { "0".into() } else { t.to_string() })))
}

fn parse_float(s: &str) -> f64 {
    // Rust's parser is correctly rounded; it accepts the Python float literal syntax.
    let s = if s.ends_with('.') { &s[..s.len() - 1] } else { s };
    let s2;
    let s = if s.starts_with('.') {
        s2 = format!("0{}", s);
        &s2
    } else {
        s
    };
    s.parse::<f64>().unwrap_or_else(|_| {
        // forms like "1.e5"
        let fixed = s.replace(".e", ".0e").replace(".E", ".0E");
        fixed.parse::<f64>().unwrap_or(f64::NAN)
    })
}

/// Convert digits in `radix` to a decimal digit string.
pub fn radix_to_decimal(digits: &str, radix: u32) -> String {
    // little-endian base 10^9 limbs
    let mut limbs: Vec<u32> = vec![0];
    for ch in digits.chars() {
        let d = ch.to_digit(radix).unwrap();
        let mut carry = d as u64;
        for l in limbs.iter_mut() {
            let v = *l as u64 * radix as u64 + carry;
            *l = (v % 1_000_000_000) as u32;
            carry = v / 1_000_000_000;
        }
        while carry > 0 {
            limbs.push((carry % 1_000_000_000) as u32);
            carry /= 1_000_000_000;
        }
    }
    let mut s = String::new();
    let mut first = true;
    for l in limbs.iter().rev() {
        if first {
            s.push_str(&l.to_string());
            first = false;
        } else {
            s.push_str(&format!("{:09}", l));
        }
    }
    s
}
