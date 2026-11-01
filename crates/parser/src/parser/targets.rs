//! Assignment / deletion / for-loop targets.

use super::expr::{bx, expr_name};
use super::*;
use TokenType as T;

/// `_PyPegen_get_invalid_target`: the first sub-expression that makes `e` an invalid target.
pub(crate) fn get_invalid_target(e: &Expr, ty: TargetsType) -> Option<&Expr> {
    match &e.node {
        ExprKind::List { elts, .. } | ExprKind::Tuple { elts, .. } => {
            for x in elts {
                if let Some(c) = get_invalid_target(x, ty) {
                    return Some(c);
                }
            }
            None
        }
        ExprKind::Starred { value, .. } => {
            if ty == TargetsType::Del {
                Some(e)
            } else {
                get_invalid_target(value, ty)
            }
        }
        ExprKind::Compare { left, ops, .. } => {
            if ty == TargetsType::For {
                if ops[0] == CmpOp::In {
                    return get_invalid_target(left, ty);
                }
                return None;
            }
            Some(e)
        }
        ExprKind::Name { .. } | ExprKind::Subscript { .. } | ExprKind::Attribute { .. } => None,
        _ => Some(e),
    }
}

/// Is `e` (parsed as an expression) a valid star target per the grammar?
fn valid_star_target(e: &Expr, top: bool) -> bool {
    let _ = top;
    match &e.node {
        ExprKind::Name { .. } | ExprKind::Attribute { .. } | ExprKind::Subscript { .. } => true,
        ExprKind::Starred { value, .. } => !matches!(value.node, ExprKind::Starred { .. }) && valid_star_target(value, false),
        ExprKind::Tuple { elts, .. } | ExprKind::List { elts, .. } => elts.iter().all(|x| valid_star_target(x, false)),
        _ => false,
    }
}

fn valid_del_target(e: &Expr) -> bool {
    match &e.node {
        ExprKind::Name { .. } | ExprKind::Attribute { .. } | ExprKind::Subscript { .. } => true,
        ExprKind::Tuple { elts, .. } | ExprKind::List { elts, .. } => elts.iter().all(valid_del_target),
        _ => false,
    }
}

/// Set the context of a (valid) target expression recursively.
pub(crate) fn set_ctx(e: &mut Expr, c: ExprContext) {
    match &mut e.node {
        ExprKind::Name { ctx, .. } | ExprKind::Attribute { ctx, .. } | ExprKind::Subscript { ctx, .. } => *ctx = c,
        ExprKind::Starred { value, ctx } => {
            *ctx = c;
            set_ctx(value, c);
        }
        ExprKind::Tuple { elts, ctx } | ExprKind::List { elts, ctx } => {
            *ctx = c;
            for x in elts {
                set_ctx(x, c);
            }
        }
        _ => {}
    }
}

impl<'a> Parser<'a> {
    /// star_target: '*' (!'*' star_target) | target_with_star_atom
    pub(crate) fn star_target(&mut self) -> R<Expr> {
        let start = self.p;
        if self.at(T::STAR) {
            self.advance();
            if self.at(T::STAR) {
                self.p = start;
                return Ok(None);
            }
            let r = self.recurse(|p| p.star_target());
            return match r? {
                Some(inner) => Ok(Some(Expr::new(ExprKind::Starred { value: bx(inner), ctx: ExprContext::Store }, self.loc(start)))),
                None => {
                    self.p = start;
                    Ok(None)
                }
            };
        }
        let Some(mut e) = self.primary()? else {
            self.p = start;
            return Ok(None);
        };
        if !valid_star_target(&e, true) || matches!(e.node, ExprKind::Starred { .. }) {
            self.p = start;
            return Ok(None);
        }
        set_ctx(&mut e, ExprContext::Store);
        Ok(Some(e))
    }

    /// star_targets: star_target !',' | star_target (',' star_target)* [',']
    pub(crate) fn star_targets(&mut self) -> R<Expr> {
        let start = self.p;
        let Some(a) = self.star_target()? else { return Ok(None) };
        if !self.at(T::COMMA) {
            return Ok(Some(a));
        }
        let mut elts = vec![a];
        while self.at(T::COMMA) {
            let m = self.p;
            self.advance();
            match self.star_target()? {
                Some(e) => elts.push(e),
                None => {
                    self.p = m + 1;
                    break;
                }
            }
        }
        Ok(Some(Expr::new(ExprKind::Tuple { elts, ctx: ExprContext::Store }, self.loc(start))))
    }

    /// Convert an expression to an assignment target; returns false if invalid.
    pub(crate) fn to_store_target(&self, e: &mut Expr) -> bool {
        if !valid_star_target(e, true) {
            return false;
        }
        set_ctx(e, ExprContext::Store);
        true
    }

    /// del_targets: ','.del_target+ [','] (followed by ';' or NEWLINE)
    pub(crate) fn del_targets(&mut self) -> R<Vec<Expr>> {
        let mut v = Vec::new();
        loop {
            let m = self.p;
            let Some(mut e) = self.primary()? else {
                self.p = m;
                break;
            };
            if !valid_del_target(&e) {
                self.p = m;
                break;
            }
            set_ctx(&mut e, ExprContext::Del);
            v.push(e);
            if !self.at(T::COMMA) {
                break;
            }
            self.advance();
        }
        if v.is_empty() {
            return Ok(None);
        }
        Ok(Some(v))
    }

    /// single_target: single_subscript_attribute_target | NAME | '(' single_target ')'
    pub(crate) fn is_single_target(e: &Expr) -> bool {
        matches!(e.node, ExprKind::Name { .. } | ExprKind::Attribute { .. } | ExprKind::Subscript { .. })
    }

    /// RAISE_SYNTAX_ERROR_INVALID_TARGET
    pub(crate) fn invalid_target_error(&self, ty: TargetsType, e: &Expr) -> Option<PErr> {
        let inv = get_invalid_target(e, ty)?;
        let msg = match ty {
            TargetsType::Star | TargetsType::For => format!("cannot assign to {}", expr_name(inv)),
            TargetsType::Del => format!("cannot delete {}", expr_name(inv)),
        };
        Some(self.err_loc(msg, inv.loc))
    }

    /// invalid_for_target: ASYNC? 'for' a=star_expressions -> invalid target error.
    /// Called with the position right after 'for'.
    pub(crate) fn invalid_for_target_tail(&mut self, ty: TargetsType) -> Result<(), PErr> {
        let m = self.p;
        if let Some(e) = self.star_expressions()? {
            if let Some(err) = self.invalid_target_error(ty, &e) {
                return Err(err);
            }
        }
        self.p = m;
        Ok(())
    }
}
