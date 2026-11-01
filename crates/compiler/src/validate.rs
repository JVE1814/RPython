//! Validation of hand-built ASTs (CPython's `_PyAST_Validate`), for `compile(ast_object, ...)`:
//! node positions, expression contexts, structural rules (empty bodies, arity mismatches,
//! pattern rules) and identifiers that cannot represent `True`/`False`/`None`.
//! Parser-produced trees always pass.  Errors are ValueErrors (a few are TypeErrors, as in
//! CPython).

use crate::ast::*;
use crate::error::{CompileError, ErrorKind};

type R = Result<(), CompileError>;

fn verr(msg: impl Into<String>) -> R {
    Err(CompileError::other(ErrorKind::ValueError, msg))
}
fn terr(msg: impl Into<String>) -> R {
    Err(CompileError::other(ErrorKind::TypeError, msg))
}

fn check(l: &Loc) -> R {
    let (lineno, end_lineno, col, end_col) = (l.lineno, l.end_lineno, l.col_offset, l.end_col_offset);
    if lineno > end_lineno {
        return verr(format!("AST node line range ({}, {}) is not valid", lineno, end_lineno));
    }
    if (lineno < 0 && end_lineno != lineno) || (col < 0 && col != end_col) {
        return verr(format!("AST node column range ({}, {}) for line range ({}, {}) is not valid", col, end_col, lineno, end_lineno));
    }
    if lineno == end_lineno && col > end_col {
        return verr(format!("line {}, column {}-{} is not a valid range", lineno, col, end_col));
    }
    Ok(())
}

fn validate_name(name: &str) -> R {
    if name == "None" || name == "True" || name == "False" {
        return verr(format!("identifier field can't represent '{}' constant", name));
    }
    Ok(())
}

fn ctx_name(c: ExprContext) -> &'static str {
    match c {
        ExprContext::Load => "Load",
        ExprContext::Store => "Store",
        ExprContext::Del => "Del",
    }
}

/// Validate a (possibly hand-built) AST. Errors are `ValueError`s / `TypeError`s.
pub fn validate_positions(m: &Mod) -> R {
    let mut v = V { depth: 0 };
    match m {
        Mod::Module { body, .. } | Mod::Interactive { body } => v.stmts(body),
        Mod::Expression { body } => v.expr(body, ExprContext::Load),
        Mod::FunctionType { argtypes, returns } => {
            for e in argtypes {
                v.expr(e, ExprContext::Load)?;
            }
            v.expr(returns, ExprContext::Load)
        }
    }
}

struct V {
    depth: u32,
}

const LOAD: ExprContext = ExprContext::Load;
const STORE: ExprContext = ExprContext::Store;

impl V {
    fn stmts(&mut self, b: &[Stmt]) -> R {
        for s in b {
            self.stmt(s)?;
        }
        Ok(())
    }
    fn body(&mut self, b: &[Stmt], owner: &str) -> R {
        if b.is_empty() {
            return verr(format!("empty body on {}", owner));
        }
        self.stmts(b)
    }
    fn exprs(&mut self, es: &[Expr], ctx: ExprContext) -> R {
        for e in es {
            self.expr(e, ctx)?;
        }
        Ok(())
    }
    fn opt(&mut self, e: &Option<Box<Expr>>, ctx: ExprContext) -> R {
        match e {
            Some(e) => self.expr(e, ctx),
            None => Ok(()),
        }
    }
    fn arg(&mut self, a: &Arg) -> R {
        check(&a.loc)?;
        validate_name(&a.arg)?;
        self.opt(&a.annotation, LOAD)
    }
    fn args(&mut self, a: &Arguments) -> R {
        for x in a.posonlyargs.iter().chain(a.args.iter()) {
            self.arg(x)?;
        }
        if let Some(v) = &a.vararg {
            self.arg(v)?;
        }
        for x in &a.kwonlyargs {
            self.arg(x)?;
        }
        if let Some(k) = &a.kwarg {
            self.arg(k)?;
        }
        if a.defaults.len() > a.posonlyargs.len() + a.args.len() {
            return verr("more positional defaults than args on arguments");
        }
        if a.kw_defaults.len() != a.kwonlyargs.len() {
            return verr("length of kwonlyargs is not the same as kw_defaults on arguments");
        }
        self.exprs(&a.defaults, LOAD)?;
        for d in a.kw_defaults.iter().flatten() {
            self.expr(d, LOAD)?;
        }
        Ok(())
    }
    fn keywords(&mut self, kws: &[Keyword]) -> R {
        for k in kws {
            check(&k.loc)?;
            if let Some(a) = &k.arg {
                validate_name(a)?;
            }
            self.expr(&k.value, LOAD)?;
        }
        Ok(())
    }
    fn tps(&mut self, tps: &[TypeParam]) -> R {
        for tp in tps {
            check(&tp.loc)?;
            match &tp.node {
                TypeParamKind::TypeVar { name, bound, default_value } => {
                    validate_name(name)?;
                    self.opt(bound, LOAD)?;
                    self.opt(default_value, LOAD)?;
                }
                TypeParamKind::ParamSpec { name, default_value } | TypeParamKind::TypeVarTuple { name, default_value } => {
                    validate_name(name)?;
                    self.opt(default_value, LOAD)?;
                }
            }
        }
        Ok(())
    }
    fn aliases(&mut self, names: &[Alias], owner: &str) -> R {
        if names.is_empty() {
            return verr(format!("empty names on {}", owner));
        }
        for a in names {
            check(&a.loc)?;
            validate_name(&a.name)?;
            if let Some(n) = &a.asname {
                validate_name(n)?;
            }
        }
        Ok(())
    }
    fn enter(&mut self) -> R {
        self.depth += 1;
        if self.depth > crate::DEFAULT_MAX_DEPTH {
            return Err(CompileError::recursion());
        }
        Ok(())
    }
    fn stmt(&mut self, s: &Stmt) -> R {
        check(&s.loc)?;
        self.enter()?;
        let r = self.stmt_inner(s);
        self.depth -= 1;
        r
    }
    fn stmt_inner(&mut self, s: &Stmt) -> R {
        match &s.node {
            StmtKind::FunctionDef(f) | StmtKind::AsyncFunctionDef(f) => {
                let owner = if matches!(s.node, StmtKind::FunctionDef(_)) { "FunctionDef" } else { "AsyncFunctionDef" };
                validate_name(&f.name)?;
                self.body(&f.body, owner)?;
                self.tps(&f.type_params)?;
                self.args(&f.args)?;
                self.exprs(&f.decorator_list, LOAD)?;
                self.opt(&f.returns, LOAD)
            }
            StmtKind::ClassDef { name, bases, keywords, body, decorator_list, type_params } => {
                validate_name(name)?;
                self.body(body, "ClassDef")?;
                self.tps(type_params)?;
                self.exprs(bases, LOAD)?;
                self.keywords(keywords)?;
                self.exprs(decorator_list, LOAD)
            }
            StmtKind::Return { value } => self.opt(value, LOAD),
            StmtKind::Delete { targets } => {
                if targets.is_empty() {
                    return verr("empty targets on Delete");
                }
                self.exprs(targets, ExprContext::Del)
            }
            StmtKind::Assign { targets, value, .. } => {
                if targets.is_empty() {
                    return verr("empty targets on Assign");
                }
                self.exprs(targets, STORE)?;
                self.expr(value, LOAD)
            }
            StmtKind::TypeAlias { name, type_params, value } => {
                if !matches!(name.node, ExprKind::Name { .. }) {
                    return terr("TypeAlias with non-Name name");
                }
                self.expr(name, STORE)?;
                self.tps(type_params)?;
                self.expr(value, LOAD)
            }
            StmtKind::AugAssign { target, value, .. } => {
                self.expr(target, STORE)?;
                self.expr(value, LOAD)
            }
            StmtKind::AnnAssign { target, annotation, value, simple } => {
                if *simple != 0 && !matches!(target.node, ExprKind::Name { .. }) {
                    return terr("AnnAssign with simple non-Name target");
                }
                self.expr(target, STORE)?;
                self.opt(value, LOAD)?;
                self.expr(annotation, LOAD)
            }
            StmtKind::For(f) | StmtKind::AsyncFor(f) => {
                let owner = if matches!(s.node, StmtKind::For(_)) { "For" } else { "AsyncFor" };
                self.expr(&f.target, STORE)?;
                self.expr(&f.iter, LOAD)?;
                self.body(&f.body, owner)?;
                self.stmts(&f.orelse)
            }
            StmtKind::While { test, body, orelse } | StmtKind::If { test, body, orelse } => {
                let owner = if matches!(s.node, StmtKind::While { .. }) { "While" } else { "If" };
                self.expr(test, LOAD)?;
                self.body(body, owner)?;
                self.stmts(orelse)
            }
            StmtKind::With(w) | StmtKind::AsyncWith(w) => {
                let owner = if matches!(s.node, StmtKind::With(_)) { "With" } else { "AsyncWith" };
                if w.items.is_empty() {
                    return verr(format!("empty items on {}", owner));
                }
                for it in &w.items {
                    self.expr(&it.context_expr, LOAD)?;
                    self.opt(&it.optional_vars, STORE)?;
                }
                self.body(&w.body, owner)
            }
            StmtKind::Match { subject, cases } => {
                self.expr(subject, LOAD)?;
                if cases.is_empty() {
                    return verr("empty cases on Match");
                }
                for c in cases {
                    self.pattern(&c.pattern, false)?;
                    self.opt(&c.guard, LOAD)?;
                    self.body(&c.body, "match_case")?;
                }
                Ok(())
            }
            StmtKind::Raise { exc, cause } => {
                match exc {
                    Some(e) => {
                        self.expr(e, LOAD)?;
                        self.opt(cause, LOAD)
                    }
                    None => {
                        if cause.is_some() {
                            return verr("Raise with cause but no exception");
                        }
                        Ok(())
                    }
                }
            }
            StmtKind::Try(t) | StmtKind::TryStar(t) => {
                let owner = if matches!(s.node, StmtKind::Try(_)) { "Try" } else { "TryStar" };
                self.body(&t.body, owner)?;
                if t.handlers.is_empty() && t.finalbody.is_empty() {
                    return verr(format!("{} has neither except handlers nor finalbody", owner));
                }
                if t.handlers.is_empty() && !t.orelse.is_empty() {
                    return verr(format!("{} has orelse but no except handlers", owner));
                }
                for h in &t.handlers {
                    check(&h.loc)?;
                    if let Some(n) = &h.name {
                        validate_name(n)?;
                    }
                    self.opt(&h.type_, LOAD)?;
                    self.body(&h.body, "ExceptHandler")?;
                }
                self.stmts(&t.finalbody)?;
                self.stmts(&t.orelse)
            }
            StmtKind::Assert { test, msg } => {
                self.expr(test, LOAD)?;
                self.opt(msg, LOAD)
            }
            StmtKind::Import { names, .. } => self.aliases(names, "Import"),
            StmtKind::ImportFrom { names, level, .. } => {
                if *level < 0 {
                    return verr("Negative ImportFrom level");
                }
                self.aliases(names, "ImportFrom")
            }
            StmtKind::Global { names } | StmtKind::Nonlocal { names } => {
                let owner = if matches!(s.node, StmtKind::Global { .. }) { "Global" } else { "Nonlocal" };
                if names.is_empty() {
                    return verr(format!("empty names on {}", owner));
                }
                for n in names {
                    validate_name(n)?;
                }
                Ok(())
            }
            StmtKind::Expr { value } => self.expr(value, LOAD),
            StmtKind::Pass | StmtKind::Break | StmtKind::Continue => Ok(()),
        }
    }

    fn capture(&self, name: &str) -> R {
        if name == "_" {
            return verr("can't capture name '_' in patterns");
        }
        validate_name(name)
    }

    fn match_value(&mut self, e: &Expr) -> R {
        self.expr(e, LOAD)?;
        match &e.node {
            ExprKind::Constant { value, .. } => match value {
                Constant::Int(_) | Constant::Float(_) | Constant::Bytes(_) | Constant::Complex { .. } | Constant::Str(_) => Ok(()),
                _ => verr("unexpected constant inside of a literal pattern"),
            },
            ExprKind::Attribute { .. } => Ok(()),
            ExprKind::UnaryOp { op: UnaryOp::USub, .. } => Ok(()),
            ExprKind::BinOp { op: Operator::Add | Operator::Sub, .. } => Ok(()),
            ExprKind::JoinedStr { .. } => Ok(()),
            _ => verr("patterns may only match literals and attribute lookups"),
        }
    }

    fn patterns(&mut self, ps: &[Pattern], star_ok: bool) -> R {
        for p in ps {
            self.pattern(p, star_ok)?;
        }
        Ok(())
    }

    fn pattern(&mut self, p: &Pattern, star_ok: bool) -> R {
        check(&p.loc)?;
        self.enter()?;
        let r = self.pattern_inner(p, star_ok);
        self.depth -= 1;
        r
    }
    fn pattern_inner(&mut self, p: &Pattern, star_ok: bool) -> R {
        match &p.node {
            PatternKind::MatchValue { value } => self.match_value(value),
            PatternKind::MatchSingleton { value } => match value {
                Constant::None | Constant::Bool(_) => Ok(()),
                _ => verr("MatchSingleton can only contain True, False and None"),
            },
            PatternKind::MatchSequence { patterns } => self.patterns(patterns, true),
            PatternKind::MatchMapping { keys, patterns, rest } => {
                if keys.len() != patterns.len() {
                    return verr("MatchMapping doesn't have the same number of keys as patterns");
                }
                if let Some(r) = rest {
                    self.capture(r)?;
                }
                for k in keys {
                    if let ExprKind::Constant { value: Constant::None | Constant::Bool(_), .. } = &k.node {
                        continue;
                    }
                    self.match_value(k)?;
                }
                self.patterns(patterns, false)
            }
            PatternKind::MatchClass { cls, patterns, kwd_attrs, kwd_patterns } => {
                let mut c: &Expr = cls;
                loop {
                    match &c.node {
                        ExprKind::Name { .. } => break,
                        ExprKind::Attribute { value, .. } => c = value,
                        _ => return verr("MatchClass cls field can only contain Name or Attribute nodes."),
                    }
                }
                self.expr(cls, LOAD)?;
                if kwd_attrs.len() != kwd_patterns.len() {
                    return verr("MatchClass doesn't have the same number of keyword attributes as patterns");
                }
                for a in kwd_attrs {
                    validate_name(a)?;
                }
                self.patterns(patterns, false)?;
                self.patterns(kwd_patterns, false)
            }
            PatternKind::MatchStar { name } => {
                if !star_ok {
                    return verr("can't use MatchStar here");
                }
                match name {
                    Some(n) => self.capture(n),
                    None => Ok(()),
                }
            }
            PatternKind::MatchAs { pattern, name } => {
                if let Some(n) = name {
                    self.capture(n)?;
                }
                match pattern {
                    None => Ok(()),
                    Some(q) => {
                        if name.is_none() {
                            return verr("MatchAs must specify a target name if a pattern is given");
                        }
                        self.pattern(q, false)
                    }
                }
            }
            PatternKind::MatchOr { patterns } => {
                if patterns.len() < 2 {
                    return verr("MatchOr requires at least 2 patterns");
                }
                self.patterns(patterns, false)
            }
        }
    }

    fn expr(&mut self, e: &Expr, ctx: ExprContext) -> R {
        check(&e.loc)?;
        self.enter()?;
        let r = self.expr_inner(e, ctx);
        self.depth -= 1;
        r
    }
    fn comps(&mut self, gens: &[Comprehension]) -> R {
        if gens.is_empty() {
            return verr("comprehension with no generators");
        }
        for g in gens {
            self.expr(&g.target, STORE)?;
            self.expr(&g.iter, LOAD)?;
            self.exprs(&g.ifs, LOAD)?;
        }
        Ok(())
    }
    fn expr_inner(&mut self, e: &Expr, ctx: ExprContext) -> R {
        let actual = match &e.node {
            ExprKind::Attribute { ctx, .. }
            | ExprKind::Subscript { ctx, .. }
            | ExprKind::Starred { ctx, .. }
            | ExprKind::Name { ctx, .. }
            | ExprKind::List { ctx, .. }
            | ExprKind::Tuple { ctx, .. } => Some(*ctx),
            _ => None,
        };
        match actual {
            Some(a) => {
                if a != ctx {
                    return verr(format!("expression must have {} context but has {} instead", ctx_name(ctx), ctx_name(a)));
                }
            }
            None => {
                if ctx != LOAD {
                    return verr(format!("expression which can't be assigned to in {} context", ctx_name(ctx)));
                }
            }
        }
        match &e.node {
            ExprKind::BoolOp { values, .. } => {
                if values.len() < 2 {
                    return verr("BoolOp with less than 2 values");
                }
                self.exprs(values, LOAD)
            }
            ExprKind::NamedExpr { target, value } => {
                if !matches!(target.node, ExprKind::Name { .. }) {
                    return terr("NamedExpr target must be a Name");
                }
                self.expr(value, LOAD)
            }
            ExprKind::BinOp { left, right, .. } => {
                self.expr(left, LOAD)?;
                self.expr(right, LOAD)
            }
            ExprKind::UnaryOp { operand, .. } => self.expr(operand, LOAD),
            ExprKind::Lambda { args, body } => {
                self.args(args)?;
                self.expr(body, LOAD)
            }
            ExprKind::IfExp { test, body, orelse } => {
                self.expr(test, LOAD)?;
                self.expr(body, LOAD)?;
                self.expr(orelse, LOAD)
            }
            ExprKind::Dict { keys, values } => {
                if keys.len() != values.len() {
                    return verr("Dict doesn't have the same number of keys as values");
                }
                for k in keys.iter().flatten() {
                    self.expr(k, LOAD)?;
                }
                self.exprs(values, LOAD)
            }
            ExprKind::Set { elts } => self.exprs(elts, LOAD),
            ExprKind::ListComp { elt, generators } | ExprKind::SetComp { elt, generators } | ExprKind::GeneratorExp { elt, generators } => {
                self.comps(generators)?;
                self.expr(elt, LOAD)
            }
            ExprKind::DictComp { key, value, generators } => {
                self.comps(generators)?;
                self.expr(key, LOAD)?;
                self.opt(value, LOAD)
            }
            ExprKind::Await { value } | ExprKind::YieldFrom { value } => self.expr(value, LOAD),
            ExprKind::Yield { value } => self.opt(value, LOAD),
            ExprKind::Compare { left, ops, comparators } => {
                if comparators.is_empty() {
                    return verr("Compare with no comparators");
                }
                if ops.len() != comparators.len() {
                    return verr("Compare has a different number of comparators and operands");
                }
                self.exprs(comparators, LOAD)?;
                self.expr(left, LOAD)
            }
            ExprKind::Call { func, args, keywords } => {
                self.expr(func, LOAD)?;
                self.exprs(args, LOAD)?;
                self.keywords(keywords)
            }
            ExprKind::FormattedValue { value, format_spec, .. } | ExprKind::Interpolation { value, format_spec, .. } => {
                self.expr(value, LOAD)?;
                self.opt(format_spec, LOAD)
            }
            ExprKind::JoinedStr { values } | ExprKind::TemplateStr { values } => self.exprs(values, LOAD),
            ExprKind::Constant { .. } => Ok(()),
            ExprKind::Attribute { value, .. } => self.expr(value, LOAD),
            ExprKind::Subscript { value, slice, .. } => {
                self.expr(slice, LOAD)?;
                self.expr(value, LOAD)
            }
            ExprKind::Starred { value, .. } => self.expr(value, ctx),
            ExprKind::Slice { lower, upper, step } => {
                self.opt(lower, LOAD)?;
                self.opt(upper, LOAD)?;
                self.opt(step, LOAD)
            }
            ExprKind::List { elts, .. } | ExprKind::Tuple { elts, .. } => self.exprs(elts, ctx),
            ExprKind::Name { id, .. } => validate_name(id),
        }
    }
}
