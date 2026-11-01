//! Validation of hand-built ASTs (CPython's `_PyAST_Validate` position checks), for
//! `compile(ast_object, ...)`. Parser-produced trees always pass.

use crate::ast::*;
use crate::error::{CompileError, ErrorKind};

type R = Result<(), CompileError>;

fn check(l: &Loc) -> R {
    let (lineno, end_lineno, col, end_col) = (l.lineno, l.end_lineno, l.col_offset, l.end_col_offset);
    if lineno > end_lineno {
        return Err(CompileError::other(ErrorKind::ValueError, format!("AST node line range ({}, {}) is not valid", lineno, end_lineno)));
    }
    if (lineno < 0 && end_lineno != lineno) || (col < 0 && col != end_col) {
        return Err(CompileError::other(
            ErrorKind::ValueError,
            format!("AST node column range ({}, {}) for line range ({}, {}) is not valid", col, end_col, lineno, end_lineno),
        ));
    }
    if lineno == end_lineno && col > end_col {
        return Err(CompileError::other(ErrorKind::ValueError, format!("line {}, column {}-{} is not a valid range", lineno, col, end_col)));
    }
    Ok(())
}

/// Validate node positions of a (possibly hand-built) AST. Errors are `ValueError`s.
pub fn validate_positions(m: &Mod) -> R {
    let mut v = V { depth: 0 };
    match m {
        Mod::Module { body, .. } | Mod::Interactive { body } => v.stmts(body),
        Mod::Expression { body } => v.expr(body),
        Mod::FunctionType { argtypes, returns } => {
            for e in argtypes {
                v.expr(e)?;
            }
            v.expr(returns)
        }
    }
}

struct V {
    depth: u32,
}

impl V {
    fn stmts(&mut self, b: &[Stmt]) -> R {
        for s in b {
            self.stmt(s)?;
        }
        Ok(())
    }
    fn args(&mut self, a: &Arguments) -> R {
        for x in a.posonlyargs.iter().chain(a.args.iter()).chain(a.kwonlyargs.iter()).chain(a.vararg.iter().map(|b| &**b)).chain(a.kwarg.iter().map(|b| &**b)) {
            check(&x.loc)?;
            if let Some(an) = &x.annotation {
                self.expr(an)?;
            }
        }
        for d in a.defaults.iter().chain(a.kw_defaults.iter().flatten()) {
            self.expr(d)?;
        }
        Ok(())
    }
    fn tps(&mut self, tps: &[TypeParam]) -> R {
        for tp in tps {
            check(&tp.loc)?;
            match &tp.node {
                TypeParamKind::TypeVar { bound, default_value, .. } => {
                    for e in bound.iter().chain(default_value.iter()) {
                        self.expr(e)?;
                    }
                }
                TypeParamKind::ParamSpec { default_value, .. } | TypeParamKind::TypeVarTuple { default_value, .. } => {
                    for e in default_value.iter() {
                        self.expr(e)?;
                    }
                }
            }
        }
        Ok(())
    }
    fn stmt(&mut self, s: &Stmt) -> R {
        check(&s.loc)?;
        self.depth += 1;
        if self.depth > crate::DEFAULT_MAX_DEPTH {
            return Err(CompileError::recursion());
        }
        let r = self.stmt_inner(s);
        self.depth -= 1;
        r
    }
    fn stmt_inner(&mut self, s: &Stmt) -> R {
        let empty = |b: &[Stmt], what: &str| -> R {
            if b.is_empty() {
                Err(CompileError::other(ErrorKind::ValueError, format!("empty body on {}", what)))
            } else {
                Ok(())
            }
        };
        match &s.node {
            StmtKind::FunctionDef(f) => empty(&f.body, "FunctionDef")?,
            StmtKind::AsyncFunctionDef(f) => empty(&f.body, "AsyncFunctionDef")?,
            StmtKind::ClassDef { body, .. } => empty(body, "ClassDef")?,
            StmtKind::For(f) => empty(&f.body, "For")?,
            StmtKind::AsyncFor(f) => empty(&f.body, "AsyncFor")?,
            StmtKind::While { body, .. } => empty(body, "While")?,
            StmtKind::If { body, .. } => empty(body, "If")?,
            StmtKind::With(w) => empty(&w.body, "With")?,
            StmtKind::AsyncWith(w) => empty(&w.body, "AsyncWith")?,
            StmtKind::Try(t) => empty(&t.body, "Try")?,
            StmtKind::TryStar(t) => empty(&t.body, "TryStar")?,
            StmtKind::Match { cases, .. } if cases.is_empty() => {
                return Err(CompileError::other(ErrorKind::ValueError, "empty cases on Match"))
            }
            _ => {}
        }
        match &s.node {
            StmtKind::FunctionDef(f) | StmtKind::AsyncFunctionDef(f) => {
                for d in &f.decorator_list {
                    self.expr(d)?;
                }
                self.tps(&f.type_params)?;
                self.args(&f.args)?;
                if let Some(r) = &f.returns {
                    self.expr(r)?;
                }
                self.stmts(&f.body)
            }
            StmtKind::ClassDef { bases, keywords, body, decorator_list, type_params, .. } => {
                for e in decorator_list.iter().chain(bases.iter()) {
                    self.expr(e)?;
                }
                for k in keywords {
                    check(&k.loc)?;
                    self.expr(&k.value)?;
                }
                self.tps(type_params)?;
                self.stmts(body)
            }
            StmtKind::Return { value } => value.as_deref().map_or(Ok(()), |e| self.expr(e)),
            StmtKind::Delete { targets } => targets.iter().try_for_each(|e| self.expr(e)),
            StmtKind::Assign { targets, value, .. } => {
                targets.iter().try_for_each(|e| self.expr(e))?;
                self.expr(value)
            }
            StmtKind::TypeAlias { name, type_params, value } => {
                self.expr(name)?;
                self.tps(type_params)?;
                self.expr(value)
            }
            StmtKind::AugAssign { target, value, .. } => {
                self.expr(target)?;
                self.expr(value)
            }
            StmtKind::AnnAssign { target, annotation, value, .. } => {
                self.expr(target)?;
                self.expr(annotation)?;
                value.as_deref().map_or(Ok(()), |e| self.expr(e))
            }
            StmtKind::For(f) | StmtKind::AsyncFor(f) => {
                self.expr(&f.target)?;
                self.expr(&f.iter)?;
                self.stmts(&f.body)?;
                self.stmts(&f.orelse)
            }
            StmtKind::While { test, body, orelse } | StmtKind::If { test, body, orelse } => {
                self.expr(test)?;
                self.stmts(body)?;
                self.stmts(orelse)
            }
            StmtKind::With(w) | StmtKind::AsyncWith(w) => {
                for it in &w.items {
                    self.expr(&it.context_expr)?;
                    if let Some(v) = &it.optional_vars {
                        self.expr(v)?;
                    }
                }
                self.stmts(&w.body)
            }
            StmtKind::Match { subject, cases } => {
                self.expr(subject)?;
                for c in cases {
                    self.pattern(&c.pattern)?;
                    if let Some(g) = &c.guard {
                        self.expr(g)?;
                    }
                    self.stmts(&c.body)?;
                }
                Ok(())
            }
            StmtKind::Raise { exc, cause } => {
                for e in exc.iter().chain(cause.iter()) {
                    self.expr(e)?;
                }
                Ok(())
            }
            StmtKind::Try(t) | StmtKind::TryStar(t) => {
                self.stmts(&t.body)?;
                for h in &t.handlers {
                    check(&h.loc)?;
                    if let Some(ty) = &h.type_ {
                        self.expr(ty)?;
                    }
                    self.stmts(&h.body)?;
                }
                self.stmts(&t.orelse)?;
                self.stmts(&t.finalbody)
            }
            StmtKind::Assert { test, msg } => {
                self.expr(test)?;
                msg.as_deref().map_or(Ok(()), |e| self.expr(e))
            }
            StmtKind::Import { names, .. } | StmtKind::ImportFrom { names, .. } => {
                for a in names {
                    check(&a.loc)?;
                }
                Ok(())
            }
            StmtKind::Expr { value } => self.expr(value),
            StmtKind::Global { .. } | StmtKind::Nonlocal { .. } | StmtKind::Pass | StmtKind::Break | StmtKind::Continue => Ok(()),
        }
    }
    fn pattern(&mut self, p: &Pattern) -> R {
        check(&p.loc)?;
        match &p.node {
            PatternKind::MatchValue { value } => self.expr(value),
            PatternKind::MatchSingleton { .. } | PatternKind::MatchStar { .. } => Ok(()),
            PatternKind::MatchSequence { patterns } | PatternKind::MatchOr { patterns } => patterns.iter().try_for_each(|q| self.pattern(q)),
            PatternKind::MatchMapping { keys, patterns, .. } => {
                keys.iter().try_for_each(|k| self.expr(k))?;
                patterns.iter().try_for_each(|q| self.pattern(q))
            }
            PatternKind::MatchClass { cls, patterns, kwd_patterns, .. } => {
                self.expr(cls)?;
                patterns.iter().chain(kwd_patterns.iter()).try_for_each(|q| self.pattern(q))
            }
            PatternKind::MatchAs { pattern, .. } => pattern.as_deref().map_or(Ok(()), |q| self.pattern(q)),
        }
    }
    fn expr(&mut self, e: &Expr) -> R {
        check(&e.loc)?;
        self.depth += 1;
        if self.depth > crate::DEFAULT_MAX_DEPTH {
            return Err(CompileError::recursion());
        }
        let r = self.expr_inner(e);
        self.depth -= 1;
        r
    }
    fn expr_inner(&mut self, e: &Expr) -> R {
        let verr = |m: &str| Err(CompileError::other(ErrorKind::ValueError, m.to_string()));
        match &e.node {
            ExprKind::BoolOp { values, .. } if values.len() < 2 => return verr("BoolOp with less than 2 values"),
            ExprKind::Compare { comparators, .. } if comparators.is_empty() => return verr("Compare with no comparators"),
            ExprKind::Compare { ops, comparators, .. } if ops.len() != comparators.len() => {
                return verr("Compare has a different number of comparators and operands")
            }
            ExprKind::Dict { keys, values } if keys.len() != values.len() => {
                return verr("Dict doesn't have the same number of keys as values")
            }
            ExprKind::ListComp { generators, .. } | ExprKind::SetComp { generators, .. } | ExprKind::GeneratorExp { generators, .. } | ExprKind::DictComp { generators, .. }
                if generators.is_empty() =>
            {
                return verr("comprehension with no generators")
            }
            _ => {}
        }
        match &e.node {
            ExprKind::BoolOp { values, .. } => values.iter().try_for_each(|x| self.expr(x)),
            ExprKind::NamedExpr { target, value } => {
                self.expr(target)?;
                self.expr(value)
            }
            ExprKind::BinOp { left, right, .. } => {
                self.expr(left)?;
                self.expr(right)
            }
            ExprKind::UnaryOp { operand, .. } => self.expr(operand),
            ExprKind::Lambda { args, body } => {
                self.args(args)?;
                self.expr(body)
            }
            ExprKind::IfExp { test, body, orelse } => {
                self.expr(test)?;
                self.expr(body)?;
                self.expr(orelse)
            }
            ExprKind::Dict { keys, values } => {
                keys.iter().flatten().try_for_each(|x| self.expr(x))?;
                values.iter().try_for_each(|x| self.expr(x))
            }
            ExprKind::Set { elts } | ExprKind::List { elts, .. } | ExprKind::Tuple { elts, .. } => elts.iter().try_for_each(|x| self.expr(x)),
            ExprKind::ListComp { elt, generators } | ExprKind::SetComp { elt, generators } | ExprKind::GeneratorExp { elt, generators } => {
                self.comps(generators)?;
                self.expr(elt)
            }
            ExprKind::DictComp { key, value, generators } => {
                self.comps(generators)?;
                self.expr(key)?;
                value.as_deref().map_or(Ok(()), |x| self.expr(x))
            }
            ExprKind::Await { value } | ExprKind::YieldFrom { value } => self.expr(value),
            ExprKind::Yield { value } => value.as_deref().map_or(Ok(()), |x| self.expr(x)),
            ExprKind::Compare { left, comparators, .. } => {
                self.expr(left)?;
                comparators.iter().try_for_each(|x| self.expr(x))
            }
            ExprKind::Call { func, args, keywords } => {
                self.expr(func)?;
                args.iter().try_for_each(|x| self.expr(x))?;
                for k in keywords {
                    check(&k.loc)?;
                    self.expr(&k.value)?;
                }
                Ok(())
            }
            ExprKind::FormattedValue { value, format_spec, .. } | ExprKind::Interpolation { value, format_spec, .. } => {
                self.expr(value)?;
                format_spec.as_deref().map_or(Ok(()), |x| self.expr(x))
            }
            ExprKind::JoinedStr { values } | ExprKind::TemplateStr { values } => values.iter().try_for_each(|x| self.expr(x)),
            ExprKind::Constant { .. } | ExprKind::Name { .. } => Ok(()),
            ExprKind::Attribute { value, .. } | ExprKind::Starred { value, .. } => self.expr(value),
            ExprKind::Subscript { value, slice, .. } => {
                self.expr(value)?;
                self.expr(slice)
            }
            ExprKind::Slice { lower, upper, step } => {
                for x in [lower, upper, step].into_iter().flatten() {
                    self.expr(x)?;
                }
                Ok(())
            }
        }
    }
    fn comps(&mut self, gens: &[Comprehension]) -> R {
        for g in gens {
            self.expr(&g.target)?;
            self.expr(&g.iter)?;
            g.ifs.iter().try_for_each(|x| self.expr(x))?;
        }
        Ok(())
    }
}
