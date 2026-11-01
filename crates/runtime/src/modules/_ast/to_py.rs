//! Rust AST -> Python `ast` objects (CPython's ast2obj).

use super::{K, State, new_node, state};
use crate::errors::*;
use crate::name;
use crate::ffi::*;
use crate::object::*;
use crate::types::{bytes, dict, int, list, none, str as pystr, tuple};
use rpy_parser::ast::*;

const MAX_DEPTH: usize = 6000;
const MAX_STACK: usize = 6 << 20;

struct B {
    s: &'static State,
    depth: usize,
    stack_base: usize,
}

type Fields = Vec<(&'static PyObject, PyRef)>;

#[inline(never)]
fn sp() -> usize {
    let x = 0u8;
    &x as *const u8 as usize
}

fn ident(s: &str) -> PyRef {
    pystr::intern_str(s)
}
fn opt_ident(s: &Option<String>) -> PyRef {
    match s {
        Some(s) => ident(s),
        None => none::none_ref(),
    }
}
fn opt_string(s: &Option<String>) -> PyRef {
    match s {
        Some(s) => pystr::new_str(s),
        None => none::none_ref(),
    }
}
fn pyint(v: i32) -> PyRef {
    int::new_int(v as i64)
}

pub fn pystr_obj(s: &PyStr) -> PyRef {
    match s {
        PyStr::Utf8(s) => pystr::new_str(s),
        PyStr::CodePoints(v) => pystr::from_codepoints(v),
    }
}

pub fn constant_to_py(c: &Constant) -> PyResult {
    Ok(match c {
        Constant::None => none::none_ref(),
        Constant::Bool(b) => crate::types::bool_::new_bool(*b),
        Constant::Ellipsis => none::ellipsis().to_ref(),
        Constant::Int(Int::Small(v)) => int::new_int(*v),
        Constant::Int(Int::Big(s)) => {
            let (neg, digits) = match s.strip_prefix('-') {
                Some(d) => (true, d),
                None => (false, s.as_str()),
            };
            crate::types::int_methods::from_decimal_str(neg, digits)?
        }
        Constant::Float(f) => crate::types::float::new_float(*f),
        Constant::Complex { real, imag } => crate::types::complex::new_complex(*real, *imag),
        Constant::Str(s) => pystr_obj(s),
        Constant::Bytes(b) => bytes::new_bytes(b),
        Constant::Tuple(items) => {
            let mut v = Vec::with_capacity(items.len());
            for it in items {
                v.push(constant_to_py(it)?);
            }
            tuple::new_tuple(v)
        }
        Constant::FrozenSet(items) => {
            let mut v = Vec::with_capacity(items.len());
            for it in items {
                v.push(constant_to_py(it)?);
            }
            crate::types::set::new_frozenset_from_vec(v)?
        }
    })
}

impl B {
    fn enter(&mut self) -> PyResult<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH || self.stack_base.saturating_sub(sp()) > MAX_STACK {
            self.depth -= 1;
            return Err(err!(RecursionError, "maximum recursion depth exceeded during ast construction"));
        }
        Ok(())
    }
    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn mk(&self, k: K, fields: Fields, loc: Option<&Loc>) -> PyResult {
        let o = new_node(self.s.tp(k))?;
        let d = crate::types::typeobject::instance_dict(&o).ok_or_else(|| err!(SystemError, "AST node without __dict__"))?;
        for (n, v) in fields {
            dict::set_item(&d, n, &v)?;
        }
        if let Some(l) = loc {
            dict::set_item(&d, name!("lineno"), &pyint(l.lineno))?;
            dict::set_item(&d, name!("col_offset"), &pyint(l.col_offset))?;
            dict::set_item(&d, name!("end_lineno"), &pyint(l.end_lineno))?;
            dict::set_item(&d, name!("end_col_offset"), &pyint(l.end_col_offset))?;
        }
        Ok(o)
    }

    fn list<T>(&mut self, items: &[T], f: impl Fn(&mut Self, &T) -> PyResult) -> PyResult {
        let mut v = Vec::with_capacity(items.len());
        for it in items {
            v.push(f(self, it)?);
        }
        Ok(list::new_list(v))
    }
    fn exprs(&mut self, items: &[Expr]) -> PyResult {
        self.list(items, |b, e| b.expr(e))
    }
    fn stmts(&mut self, items: &[Stmt]) -> PyResult {
        self.list(items, |b, s| b.stmt(s))
    }
    fn opt_expr(&mut self, e: &Option<Box<Expr>>) -> PyResult {
        match e {
            Some(e) => self.expr(e),
            None => Ok(none::none_ref()),
        }
    }
    fn idents(&self, v: &[String]) -> PyRef {
        list::new_list(v.iter().map(|s| ident(s)).collect())
    }

    fn module(&mut self, m: &Mod) -> PyResult {
        match m {
            Mod::Module { body, type_ignores } => {
                let b = self.stmts(body)?;
                let ti = self.list(type_ignores, |b, t| {
                    b.mk(K::TypeIgnore, vec![(name!("lineno"), pyint(t.lineno)), (name!("tag"), pystr::new_str(&t.tag))], None)
                })?;
                self.mk(K::Module, vec![(name!("body"), b), (name!("type_ignores"), ti)], None)
            }
            Mod::Interactive { body } => {
                let b = self.stmts(body)?;
                self.mk(K::Interactive, vec![(name!("body"), b)], None)
            }
            Mod::Expression { body } => {
                let b = self.expr(body)?;
                self.mk(K::Expression, vec![(name!("body"), b)], None)
            }
            Mod::FunctionType { argtypes, returns } => {
                let a = self.exprs(argtypes)?;
                let r = self.expr(returns)?;
                self.mk(K::FunctionType, vec![(name!("argtypes"), a), (name!("returns"), r)], None)
            }
        }
    }

    fn type_params(&mut self, tps: &[TypeParam]) -> PyResult {
        self.list(tps, |b, tp| {
            let (k, f) = match &tp.node {
                TypeParamKind::TypeVar { name, bound, default_value } => (
                    K::TypeVar,
                    vec![(name!("name"), ident(name)), (name!("bound"), b.opt_expr(bound)?), (name!("default_value"), b.opt_expr(default_value)?)],
                ),
                TypeParamKind::ParamSpec { name, default_value } => {
                    (K::ParamSpec, vec![(name!("name"), ident(name)), (name!("default_value"), b.opt_expr(default_value)?)])
                }
                TypeParamKind::TypeVarTuple { name, default_value } => {
                    (K::TypeVarTuple, vec![(name!("name"), ident(name)), (name!("default_value"), b.opt_expr(default_value)?)])
                }
            };
            b.mk(k, f, Some(&tp.loc))
        })
    }

    fn arg(&mut self, a: &Arg) -> PyResult {
        let ann = self.opt_expr(&a.annotation)?;
        self.mk(K::Arg, vec![(name!("arg"), ident(&a.arg)), (name!("annotation"), ann), (name!("type_comment"), opt_string(&a.type_comment))], Some(&a.loc))
    }
    fn opt_arg(&mut self, a: &Option<Box<Arg>>) -> PyResult {
        match a {
            Some(a) => self.arg(a),
            None => Ok(none::none_ref()),
        }
    }
    fn arguments(&mut self, a: &Arguments) -> PyResult {
        let posonly = self.list(&a.posonlyargs, |b, x| b.arg(x))?;
        let args = self.list(&a.args, |b, x| b.arg(x))?;
        let vararg = self.opt_arg(&a.vararg)?;
        let kwonly = self.list(&a.kwonlyargs, |b, x| b.arg(x))?;
        let kwd = self.list(&a.kw_defaults, |b, x| match x {
            Some(e) => b.expr(e),
            None => Ok(none::none_ref()),
        })?;
        let kwarg = self.opt_arg(&a.kwarg)?;
        let defaults = self.exprs(&a.defaults)?;
        self.mk(
            K::Arguments,
            vec![
                (name!("posonlyargs"), posonly),
                (name!("args"), args),
                (name!("vararg"), vararg),
                (name!("kwonlyargs"), kwonly),
                (name!("kw_defaults"), kwd),
                (name!("kwarg"), kwarg),
                (name!("defaults"), defaults),
            ],
            None,
        )
    }
    fn keywords(&mut self, kws: &[Keyword]) -> PyResult {
        self.list(kws, |b, k| {
            let v = b.expr(&k.value)?;
            b.mk(K::Keyword, vec![(name!("arg"), opt_ident(&k.arg)), (name!("value"), v)], Some(&k.loc))
        })
    }
    fn aliases(&mut self, names: &[Alias]) -> PyResult {
        self.list(names, |b, a| b.mk(K::Alias, vec![(name!("name"), ident(&a.name)), (name!("asname"), opt_ident(&a.asname))], Some(&a.loc)))
    }
    fn handlers(&mut self, hs: &[ExceptHandler]) -> PyResult {
        self.list(hs, |b, h| {
            let t = b.opt_expr(&h.type_)?;
            let body = b.stmts(&h.body)?;
            b.mk(K::ExceptHandler, vec![(name!("type"), t), (name!("name"), opt_ident(&h.name)), (name!("body"), body)], Some(&h.loc))
        })
    }
    fn comprehensions(&mut self, gens: &[Comprehension]) -> PyResult {
        self.list(gens, |b, g| {
            let t = b.expr(&g.target)?;
            let i = b.expr(&g.iter)?;
            let ifs = b.exprs(&g.ifs)?;
            b.mk(K::Comprehension, vec![(name!("target"), t), (name!("iter"), i), (name!("ifs"), ifs), (name!("is_async"), pyint(g.is_async))], None)
        })
    }
    fn withitems(&mut self, items: &[WithItem]) -> PyResult {
        self.list(items, |b, w| {
            let c = b.expr(&w.context_expr)?;
            let v = b.opt_expr(&w.optional_vars)?;
            b.mk(K::WithItem, vec![(name!("context_expr"), c), (name!("optional_vars"), v)], None)
        })
    }

    fn stmt(&mut self, s: &Stmt) -> PyResult {
        self.enter()?;
        let r = self.stmt_inner(s);
        self.leave();
        r
    }
    fn stmt_inner(&mut self, s: &Stmt) -> PyResult {
        use StmtKind as S;
        let (k, f): (K, Fields) = match &s.node {
            S::FunctionDef(fd) | S::AsyncFunctionDef(fd) => {
                let k = if matches!(s.node, S::FunctionDef(_)) { K::FunctionDef } else { K::AsyncFunctionDef };
                let args = self.arguments(&fd.args)?;
                let body = self.stmts(&fd.body)?;
                let decos = self.exprs(&fd.decorator_list)?;
                let returns = self.opt_expr(&fd.returns)?;
                let tps = self.type_params(&fd.type_params)?;
                (
                    k,
                    vec![
                        (name!("name"), ident(&fd.name)),
                        (name!("args"), args),
                        (name!("body"), body),
                        (name!("decorator_list"), decos),
                        (name!("returns"), returns),
                        (name!("type_comment"), opt_string(&fd.type_comment)),
                        (name!("type_params"), tps),
                    ],
                )
            }
            S::ClassDef { name, bases, keywords, body, decorator_list, type_params } => {
                let bases = self.exprs(bases)?;
                let kws = self.keywords(keywords)?;
                let body = self.stmts(body)?;
                let decos = self.exprs(decorator_list)?;
                let tps = self.type_params(type_params)?;
                (
                    K::ClassDef,
                    vec![
                        (name!("name"), ident(name)),
                        (name!("bases"), bases),
                        (name!("keywords"), kws),
                        (name!("body"), body),
                        (name!("decorator_list"), decos),
                        (name!("type_params"), tps),
                    ],
                )
            }
            S::Return { value } => (K::Return, vec![(name!("value"), self.opt_expr(value)?)]),
            S::Delete { targets } => (K::Delete, vec![(name!("targets"), self.exprs(targets)?)]),
            S::Assign { targets, value, type_comment } => {
                let t = self.exprs(targets)?;
                let v = self.expr(value)?;
                (K::Assign, vec![(name!("targets"), t), (name!("value"), v), (name!("type_comment"), opt_string(type_comment))])
            }
            S::TypeAlias { name, type_params, value } => {
                let n = self.expr(name)?;
                let tps = self.type_params(type_params)?;
                let v = self.expr(value)?;
                (K::TypeAlias, vec![(name!("name"), n), (name!("type_params"), tps), (name!("value"), v)])
            }
            S::AugAssign { target, op, value } => {
                let t = self.expr(target)?;
                let o = self.operator(*op)?;
                let v = self.expr(value)?;
                (K::AugAssign, vec![(name!("target"), t), (name!("op"), o), (name!("value"), v)])
            }
            S::AnnAssign { target, annotation, value, simple } => {
                let t = self.expr(target)?;
                let a = self.expr(annotation)?;
                let v = self.opt_expr(value)?;
                (K::AnnAssign, vec![(name!("target"), t), (name!("annotation"), a), (name!("value"), v), (name!("simple"), pyint(*simple))])
            }
            S::For(fr) | S::AsyncFor(fr) => {
                let k = if matches!(s.node, S::For(_)) { K::For } else { K::AsyncFor };
                let t = self.expr(&fr.target)?;
                let i = self.expr(&fr.iter)?;
                let body = self.stmts(&fr.body)?;
                let orelse = self.stmts(&fr.orelse)?;
                (
                    k,
                    vec![
                        (name!("target"), t),
                        (name!("iter"), i),
                        (name!("body"), body),
                        (name!("orelse"), orelse),
                        (name!("type_comment"), opt_string(&fr.type_comment)),
                    ],
                )
            }
            S::While { test, body, orelse } | S::If { test, body, orelse } => {
                let k = if matches!(s.node, S::While { .. }) { K::While } else { K::If };
                let t = self.expr(test)?;
                let b = self.stmts(body)?;
                let o = self.stmts(orelse)?;
                (k, vec![(name!("test"), t), (name!("body"), b), (name!("orelse"), o)])
            }
            S::With(w) | S::AsyncWith(w) => {
                let k = if matches!(s.node, S::With(_)) { K::With } else { K::AsyncWith };
                let items = self.withitems(&w.items)?;
                let body = self.stmts(&w.body)?;
                (k, vec![(name!("items"), items), (name!("body"), body), (name!("type_comment"), opt_string(&w.type_comment))])
            }
            S::Match { subject, cases } => {
                let subj = self.expr(subject)?;
                let cs = self.list(cases, |b, c| {
                    let p = b.pattern(&c.pattern)?;
                    let g = b.opt_expr(&c.guard)?;
                    let body = b.stmts(&c.body)?;
                    b.mk(K::MatchCase, vec![(name!("pattern"), p), (name!("guard"), g), (name!("body"), body)], None)
                })?;
                (K::Match, vec![(name!("subject"), subj), (name!("cases"), cs)])
            }
            S::Raise { exc, cause } => {
                let e = self.opt_expr(exc)?;
                let c = self.opt_expr(cause)?;
                (K::Raise, vec![(name!("exc"), e), (name!("cause"), c)])
            }
            S::Try(t) | S::TryStar(t) => {
                let k = if matches!(s.node, S::Try(_)) { K::Try } else { K::TryStar };
                let body = self.stmts(&t.body)?;
                let hs = self.handlers(&t.handlers)?;
                let orelse = self.stmts(&t.orelse)?;
                let fb = self.stmts(&t.finalbody)?;
                (k, vec![(name!("body"), body), (name!("handlers"), hs), (name!("orelse"), orelse), (name!("finalbody"), fb)])
            }
            S::Assert { test, msg } => {
                let t = self.expr(test)?;
                let m = self.opt_expr(msg)?;
                (K::Assert, vec![(name!("test"), t), (name!("msg"), m)])
            }
            S::Import { names, is_lazy } => (K::Import, vec![(name!("names"), self.aliases(names)?), (name!("is_lazy"), pyint(*is_lazy))]),
            S::ImportFrom { module, names, level, is_lazy } => (
                K::ImportFrom,
                vec![
                    (name!("module"), opt_ident(module)),
                    (name!("names"), self.aliases(names)?),
                    (name!("level"), pyint(*level)),
                    (name!("is_lazy"), pyint(*is_lazy)),
                ],
            ),
            S::Global { names } => (K::Global, vec![(name!("names"), self.idents(names))]),
            S::Nonlocal { names } => (K::Nonlocal, vec![(name!("names"), self.idents(names))]),
            S::Expr { value } => (K::ExprStmt, vec![(name!("value"), self.expr(value)?)]),
            S::Pass => (K::Pass, vec![]),
            S::Break => (K::Break, vec![]),
            S::Continue => (K::Continue, vec![]),
        };
        self.mk(k, f, Some(&s.loc))
    }

    fn ctx(&self, c: ExprContext) -> PyResult {
        self.s.singleton(match c {
            ExprContext::Load => K::Load,
            ExprContext::Store => K::Store,
            ExprContext::Del => K::Del,
        })
    }
    fn operator(&self, op: Operator) -> PyResult {
        use Operator::*;
        self.s.singleton(match op {
            Add => K::Add,
            Sub => K::Sub,
            Mult => K::Mult,
            MatMult => K::MatMult,
            Div => K::Div,
            Mod => K::ModOp,
            Pow => K::Pow,
            LShift => K::LShift,
            RShift => K::RShift,
            BitOr => K::BitOr,
            BitXor => K::BitXor,
            BitAnd => K::BitAnd,
            FloorDiv => K::FloorDiv,
        })
    }
    fn cmpop(&self, op: CmpOp) -> PyResult {
        use CmpOp::*;
        self.s.singleton(match op {
            Eq => K::Eq,
            NotEq => K::NotEq,
            Lt => K::Lt,
            LtE => K::LtE,
            Gt => K::Gt,
            GtE => K::GtE,
            Is => K::Is,
            IsNot => K::IsNot,
            In => K::In,
            NotIn => K::NotIn,
        })
    }

    fn expr(&mut self, e: &Expr) -> PyResult {
        self.enter()?;
        let r = self.expr_inner(e);
        self.leave();
        r
    }
    fn expr_inner(&mut self, e: &Expr) -> PyResult {
        use ExprKind as E;
        let (k, f): (K, Fields) = match &e.node {
            E::BoolOp { op, values } => {
                let o = self.s.singleton(match op {
                    BoolOp::And => K::And,
                    BoolOp::Or => K::Or,
                })?;
                (K::BoolOp, vec![(name!("op"), o), (name!("values"), self.exprs(values)?)])
            }
            E::NamedExpr { target, value } => {
                let t = self.expr(target)?;
                let v = self.expr(value)?;
                (K::NamedExpr, vec![(name!("target"), t), (name!("value"), v)])
            }
            E::BinOp { left, op, right } => {
                let l = self.expr(left)?;
                let o = self.operator(*op)?;
                let r = self.expr(right)?;
                (K::BinOp, vec![(name!("left"), l), (name!("op"), o), (name!("right"), r)])
            }
            E::UnaryOp { op, operand } => {
                let o = self.s.singleton(match op {
                    UnaryOp::Invert => K::Invert,
                    UnaryOp::Not => K::Not,
                    UnaryOp::UAdd => K::UAdd,
                    UnaryOp::USub => K::USub,
                })?;
                (K::UnaryOp, vec![(name!("op"), o), (name!("operand"), self.expr(operand)?)])
            }
            E::Lambda { args, body } => {
                let a = self.arguments(args)?;
                let b = self.expr(body)?;
                (K::Lambda, vec![(name!("args"), a), (name!("body"), b)])
            }
            E::IfExp { test, body, orelse } => {
                let t = self.expr(test)?;
                let b = self.expr(body)?;
                let o = self.expr(orelse)?;
                (K::IfExp, vec![(name!("test"), t), (name!("body"), b), (name!("orelse"), o)])
            }
            E::Dict { keys, values } => {
                let ks = self.list(keys, |b, k| match k {
                    Some(k) => b.expr(k),
                    None => Ok(none::none_ref()),
                })?;
                let vs = self.exprs(values)?;
                (K::Dict, vec![(name!("keys"), ks), (name!("values"), vs)])
            }
            E::Set { elts } => (K::Set, vec![(name!("elts"), self.exprs(elts)?)]),
            E::ListComp { elt, generators } | E::SetComp { elt, generators } | E::GeneratorExp { elt, generators } => {
                let k = match &e.node {
                    E::ListComp { .. } => K::ListComp,
                    E::SetComp { .. } => K::SetComp,
                    _ => K::GeneratorExp,
                };
                let el = self.expr(elt)?;
                let g = self.comprehensions(generators)?;
                (k, vec![(name!("elt"), el), (name!("generators"), g)])
            }
            E::DictComp { key, value, generators } => {
                let k = self.expr(key)?;
                let v = self.opt_expr(value)?;
                let g = self.comprehensions(generators)?;
                (K::DictComp, vec![(name!("key"), k), (name!("value"), v), (name!("generators"), g)])
            }
            E::Await { value } => (K::Await, vec![(name!("value"), self.expr(value)?)]),
            E::Yield { value } => (K::Yield, vec![(name!("value"), self.opt_expr(value)?)]),
            E::YieldFrom { value } => (K::YieldFrom, vec![(name!("value"), self.expr(value)?)]),
            E::Compare { left, ops, comparators } => {
                let l = self.expr(left)?;
                let mut ov = Vec::with_capacity(ops.len());
                for o in ops {
                    ov.push(self.cmpop(*o)?);
                }
                let c = self.exprs(comparators)?;
                (K::Compare, vec![(name!("left"), l), (name!("ops"), list::new_list(ov)), (name!("comparators"), c)])
            }
            E::Call { func, args, keywords } => {
                let fu = self.expr(func)?;
                let a = self.exprs(args)?;
                let k = self.keywords(keywords)?;
                (K::Call, vec![(name!("func"), fu), (name!("args"), a), (name!("keywords"), k)])
            }
            E::FormattedValue { value, conversion, format_spec } => {
                let v = self.expr(value)?;
                let fs = self.opt_expr(format_spec)?;
                (K::FormattedValue, vec![(name!("value"), v), (name!("conversion"), pyint(*conversion)), (name!("format_spec"), fs)])
            }
            E::Interpolation { value, str, conversion, format_spec } => {
                let v = self.expr(value)?;
                let fs = self.opt_expr(format_spec)?;
                (
                    K::Interpolation,
                    vec![(name!("value"), v), (name!("str"), pystr_obj(str)), (name!("conversion"), pyint(*conversion)), (name!("format_spec"), fs)],
                )
            }
            E::JoinedStr { values } => (K::JoinedStr, vec![(name!("values"), self.exprs(values)?)]),
            E::TemplateStr { values } => (K::TemplateStr, vec![(name!("values"), self.exprs(values)?)]),
            E::Constant { value, kind } => (K::Constant, vec![(name!("value"), constant_to_py(value)?), (name!("kind"), opt_string(kind))]),
            E::Attribute { value, attr, ctx } => {
                let v = self.expr(value)?;
                (K::Attribute, vec![(name!("value"), v), (name!("attr"), ident(attr)), (name!("ctx"), self.ctx(*ctx)?)])
            }
            E::Subscript { value, slice, ctx } => {
                let v = self.expr(value)?;
                let s = self.expr(slice)?;
                (K::Subscript, vec![(name!("value"), v), (name!("slice"), s), (name!("ctx"), self.ctx(*ctx)?)])
            }
            E::Starred { value, ctx } => {
                let v = self.expr(value)?;
                (K::Starred, vec![(name!("value"), v), (name!("ctx"), self.ctx(*ctx)?)])
            }
            E::Name { id, ctx } => (K::Name, vec![(name!("id"), ident(id)), (name!("ctx"), self.ctx(*ctx)?)]),
            E::List { elts, ctx } => (K::List, vec![(name!("elts"), self.exprs(elts)?), (name!("ctx"), self.ctx(*ctx)?)]),
            E::Tuple { elts, ctx } => (K::Tuple, vec![(name!("elts"), self.exprs(elts)?), (name!("ctx"), self.ctx(*ctx)?)]),
            E::Slice { lower, upper, step } => {
                let l = self.opt_expr(lower)?;
                let u = self.opt_expr(upper)?;
                let s = self.opt_expr(step)?;
                (K::Slice, vec![(name!("lower"), l), (name!("upper"), u), (name!("step"), s)])
            }
        };
        self.mk(k, f, Some(&e.loc))
    }

    fn patterns(&mut self, ps: &[Pattern]) -> PyResult {
        self.list(ps, |b, p| b.pattern(p))
    }
    fn pattern(&mut self, p: &Pattern) -> PyResult {
        self.enter()?;
        let r = self.pattern_inner(p);
        self.leave();
        r
    }
    fn pattern_inner(&mut self, p: &Pattern) -> PyResult {
        use PatternKind as P;
        let (k, f): (K, Fields) = match &p.node {
            P::MatchValue { value } => (K::MatchValue, vec![(name!("value"), self.expr(value)?)]),
            P::MatchSingleton { value } => (K::MatchSingleton, vec![(name!("value"), constant_to_py(value)?)]),
            P::MatchSequence { patterns } => (K::MatchSequence, vec![(name!("patterns"), self.patterns(patterns)?)]),
            P::MatchMapping { keys, patterns, rest } => {
                let ks = self.exprs(keys)?;
                let ps = self.patterns(patterns)?;
                (K::MatchMapping, vec![(name!("keys"), ks), (name!("patterns"), ps), (name!("rest"), opt_ident(rest))])
            }
            P::MatchClass { cls, patterns, kwd_attrs, kwd_patterns } => {
                let c = self.expr(cls)?;
                let ps = self.patterns(patterns)?;
                let ka = self.idents(kwd_attrs);
                let kp = self.patterns(kwd_patterns)?;
                (K::MatchClass, vec![(name!("cls"), c), (name!("patterns"), ps), (name!("kwd_attrs"), ka), (name!("kwd_patterns"), kp)])
            }
            P::MatchStar { name } => (K::MatchStar, vec![(name!("name"), opt_ident(name))]),
            P::MatchAs { pattern, name } => {
                let pt = match pattern {
                    Some(q) => self.pattern(q)?,
                    None => none::none_ref(),
                };
                (K::MatchAs, vec![(name!("pattern"), pt), (name!("name"), opt_ident(name))])
            }
            P::MatchOr { patterns } => (K::MatchOr, vec![(name!("patterns"), self.patterns(patterns)?)]),
        };
        self.mk(k, f, Some(&p.loc))
    }
}

/// Convert a Rust module AST to Python `ast` objects.
pub fn mod_to_py(m: &Mod) -> PyResult {
    let s = state()?;
    let mut b = B { s, depth: 0, stack_base: sp() };
    b.module(m)
}

#[allow(dead_code)]
fn _unused(_: *mut PyTypeObject) {}
