//! Function and lambda parameters.

use super::expr::bx;
use super::*;
use TokenType as T;

impl<'a> Parser<'a> {
    /// Parameters of a `def` (after '(' up to, not including, ')').
    pub(crate) fn params(&mut self) -> R<Arguments> {
        self.parameters(false)
    }

    /// Parameters of a lambda (up to, not including, ':').
    pub(crate) fn lambda_params(&mut self) -> R<Arguments> {
        self.parameters(true)
    }

    fn at_param_end(&mut self, lambda: bool) -> bool {
        if lambda {
            self.at(T::COLON)
        } else {
            self.at(T::RPAR)
        }
    }

    /// NAME [':' expression | ':' star_expression]
    fn param(&mut self, lambda: bool, star_annotation: bool) -> R<Arg> {
        let start = self.p;
        let Some(n) = self.eat_name() else { return Ok(None) };
        let mut annotation = None;
        if !lambda && self.at(T::COLON) {
            self.advance();
            let e = if star_annotation { self.star_expression()? } else { self.expression()? };
            match e {
                Some(e) => annotation = Some(bx(e)),
                None => {
                    self.p = start;
                    return Ok(None);
                }
            }
        }
        Ok(Some(Arg { arg: self.ident(&n), annotation, type_comment: None, loc: self.loc(start) }))
    }

    /// After a parameter: ',' [TYPE_COMMENT] | [TYPE_COMMENT] &terminator. Returns the type
    /// comment if any; Ok(false) if neither a comma nor the terminator follows.
    fn param_sep(&mut self, lambda: bool, tc: &mut Option<String>) -> Result<bool, PErr> {
        if self.eat(T::COMMA).is_some() {
            if let Some(t) = self.eat(T::TYPE_COMMENT) {
                *tc = Some(self.text(&t).to_string());
            }
            return Ok(true);
        }
        let m = self.p;
        if let Some(t) = self.eat(T::TYPE_COMMENT) {
            *tc = Some(self.text(&t).to_string());
        }
        if self.at_param_end(lambda) {
            return Ok(true);
        }
        self.p = m;
        *tc = None;
        Ok(false)
    }

    fn parameters(&mut self, lambda: bool) -> R<Arguments> {
        let start = self.p;
        if self.invalid {
            self.invalid_parameters(lambda)?;
            self.p = start;
        }
        let mut a = Arguments::default();
        let mut pos: Vec<Arg> = Vec::new();
        let mut pos_defaults: Vec<Expr> = Vec::new();
        let mut seen_slash = false;
        let mut seen_star = false;
        let mut seen_kwarg = false;
        let mut any = false;
        loop {
            if self.at_param_end(lambda) {
                break;
            }
            if seen_kwarg {
                self.p = start;
                return Ok(None);
            }
            let t = self.peek();
            match t.ty {
                T::SLASH => {
                    if seen_slash || seen_star || pos.is_empty() {
                        self.p = start;
                        return Ok(None);
                    }
                    self.advance();
                    seen_slash = true;
                    a.posonlyargs = std::mem::take(&mut pos);
                    if self.eat(T::COMMA).is_none() && !self.at_param_end(lambda) {
                        self.p = start;
                        return Ok(None);
                    }
                    any = true;
                    continue;
                }
                T::STAR => {
                    if seen_star {
                        self.p = start;
                        return Ok(None);
                    }
                    self.advance();
                    seen_star = true;
                    if self.at(T::COMMA) {
                        // bare '*': must be followed by keyword-only params
                        self.advance();
                        if self.at(T::TYPE_COMMENT) {
                            if self.invalid {
                                return Err(self.err_tok("bare * has associated type comment", &t));
                            }
                            self.p = start;
                            return Ok(None);
                        }
                        if self.at_param_end(lambda) || self.at(T::DOUBLESTAR) {
                            self.p = start;
                            return Ok(None);
                        }
                        any = true;
                        continue;
                    }
                    let Some(mut p) = self.param(lambda, true)? else {
                        self.p = start;
                        return Ok(None);
                    };
                    let mut tc = None;
                    if !self.param_sep(lambda, &mut tc)? {
                        self.p = start;
                        return Ok(None);
                    }
                    p.type_comment = tc;
                    a.vararg = Some(Box::new(p));
                    any = true;
                    continue;
                }
                T::DOUBLESTAR => {
                    self.advance();
                    let Some(mut p) = self.param(lambda, false)? else {
                        self.p = start;
                        return Ok(None);
                    };
                    let mut tc = None;
                    if !self.param_sep(lambda, &mut tc)? {
                        self.p = start;
                        return Ok(None);
                    }
                    p.type_comment = tc;
                    a.kwarg = Some(Box::new(p));
                    seen_kwarg = true;
                    any = true;
                    continue;
                }
                _ => {}
            }
            let Some(mut p) = self.param(lambda, false)? else {
                self.p = start;
                return Ok(None);
            };
            let mut default = None;
            if self.at(T::EQUAL) {
                self.advance();
                match self.expression()? {
                    Some(e) => default = Some(e),
                    None => {
                        self.p = start;
                        return Ok(None);
                    }
                }
            }
            let mut tc = None;
            if !self.param_sep(lambda, &mut tc)? {
                self.p = start;
                return Ok(None);
            }
            p.type_comment = tc;
            if seen_star {
                a.kwonlyargs.push(p);
                a.kw_defaults.push(default);
            } else {
                match default {
                    Some(d) => pos_defaults.push(d),
                    None => {
                        if !pos_defaults.is_empty() {
                            self.p = start;
                            return Ok(None);
                        }
                    }
                }
                pos.push(p);
            }
            any = true;
        }
        if !any {
            self.p = start;
            return Ok(None);
        }
        a.args = pos;
        a.defaults = pos_defaults;
        // positional-only defaults: defaults apply to posonly+args combined; a posonly param
        // without default after one with default is invalid (handled by order check above
        // only within `args`), check across the boundary:
        Ok(Some(a))
    }

    /// invalid_parameters / invalid_lambda_parameters / invalid_star_etc / invalid_kwds /
    /// invalid_default, implemented procedurally over the token stream.
    fn invalid_parameters(&mut self, lambda: bool) -> Result<(), PErr> {
        let start = self.p;
        let fun = if lambda { "Lambda expression parameters" } else { "Function parameters" };
        let mut seen_slash = false;
        let mut seen_star = false;
        let mut seen_default = false;
        let mut nparams = 0usize;
        loop {
            if self.at_param_end(lambda) {
                break;
            }
            let t = self.peek();
            match t.ty {
                T::SLASH => {
                    if nparams == 0 && !seen_star {
                        return Err(self.err_tok("at least one parameter must precede /", &t));
                    }
                    if seen_slash {
                        return Err(self.err_tok("/ may appear only once", &t));
                    }
                    if seen_star {
                        return Err(self.err_tok("/ must be ahead of *", &t));
                    }
                    seen_slash = true;
                    self.advance();
                    if self.at(T::STAR) {
                        let st = self.peek();
                        return Err(self.err_tok("expected comma between / and *", &st));
                    }
                }
                T::STAR => {
                    if seen_star {
                        return Err(self.err_tok("* may appear only once", &t));
                    }
                    seen_star = true;
                    self.advance();
                    if self.at_param_end(lambda) || (self.at(T::COMMA) && {
                        let n = self.peek_n(1);
                        (if lambda { n.ty == T::COLON } else { n.ty == T::RPAR }) || n.ty == T::DOUBLESTAR
                    }) {
                        return Err(self.err_tok("named parameters must follow bare *", &t));
                    }
                    if self.at(T::COMMA) && self.peek_n(1).ty == T::TYPE_COMMENT {
                        return Err(self.err_tok("bare * has associated type comment", &t));
                    }
                    if self.at_name() {
                        let pm = self.p;
                        if self.param(lambda, true)?.is_some() && self.at(T::EQUAL) {
                            let eq = self.peek();
                            return Err(self.err_tok("var-positional parameter cannot have default value", &eq));
                        }
                        self.p = pm;
                    }
                }
                T::DOUBLESTAR => {
                    self.advance();
                    if self.param(lambda, false)?.is_none() {
                        return Ok(());
                    }
                    if self.at(T::EQUAL) {
                        let eq = self.peek();
                        return Err(self.err_tok("var-keyword parameter cannot have default value", &eq));
                    }
                    if self.eat(T::COMMA).is_some() {
                        let n = self.peek();
                        if matches!(n.ty, T::STAR | T::DOUBLESTAR | T::SLASH) || (n.ty == T::NAME && !n.kw.is_hard()) {
                            if n.ty == T::NAME {
                                let pm = self.p;
                                let pa = self.param(lambda, false)?;
                                self.p = pm;
                                if let Some(pa) = pa {
                                    return Err(self.err_loc("parameters cannot follow var-keyword parameter", pa.loc));
                                }
                            }
                            return Err(self.err_tok("parameters cannot follow var-keyword parameter", &n));
                        }
                    }
                    return Ok(());
                }
                T::LPAR => {
                    // '(' param_no_default+ ','? ')'
                    let lp = t;
                    self.advance();
                    let mut k = 0;
                    while self.eat_name().is_some() {
                        k += 1;
                        if self.eat(T::COMMA).is_none() {
                            break;
                        }
                    }
                    if k > 0 && self.at(T::RPAR) {
                        let rp = self.peek();
                        return Err(self.err_range(format!("{} cannot be parenthesized", fun), Self::tok_loc(&lp), Self::tok_loc(&rp)));
                    }
                    return Ok(());
                }
                T::NAME if !t.kw.is_hard() => {
                    let pstart = self.p;
                    let Some(pa) = self.param(lambda, false)? else { return Ok(()) };
                    let _ = pstart;
                    nparams += 1;
                    if self.at(T::EQUAL) {
                        let eq = self.peek();
                        self.advance();
                        if matches!(self.peek_ty(), T::RPAR | T::COMMA) || (lambda && self.at(T::COLON)) {
                            return Err(self.err_tok("expected default value expression", &eq));
                        }
                        if self.expression()?.is_none() {
                            return Ok(());
                        }
                        seen_default = true;
                    } else if seen_default && !seen_star {
                        return Err(self.err_loc("parameter without a default follows parameter with a default", pa.loc));
                    }
                }
                _ => return Ok(()),
            }
            if self.eat(T::COMMA).is_none() {
                if self.at(T::TYPE_COMMENT) {
                    self.advance();
                }
                break;
            }
            if self.at(T::TYPE_COMMENT) {
                self.advance();
            }
        }
        self.p = start;
        Ok(())
    }
}
