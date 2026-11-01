//! Python `ast` objects -> Rust AST (CPython's obj2ast), with CPython's error messages.
//!
//! Lists may not contain None where the Rust AST has no hole; those cases raise the
//! validator's "None disallowed in ... list" ValueError directly.

use super::{K, State, state};
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::{bool_, bytes, int, list, none, str as pystr, tuple};
use crate::{name, raise};
use rpy_parser::ast::*;

const MAX_DEPTH: usize = 6000;
const MAX_STACK: usize = 6 << 20;

#[inline(never)]
fn sp() -> usize {
    let x = 0u8;
    &x as *const u8 as usize
}

struct C {
    s: &'static State,
    depth: usize,
    stack_base: usize,
}

fn fname(n: &PyObject) -> String {
    pystr::to_string_lossy(n)
}

pub fn py_str_to_pystr(o: &PyObject) -> PyStr {
    match pystr::to_string_strict(o) {
        Some(s) => PyStr::Utf8(s),
        None => {
            let d = pystr::str_data(o);
            PyStr::CodePoints(d.to_vec())
        }
    }
}

/// Convert a Python constant (exact builtin types only, like CPython's validate_constant).
pub fn py_to_constant(o: &PyObject) -> PyResult<Constant> {
    if o.is(none::none()) {
        return Ok(Constant::None);
    }
    if o.is(none::ellipsis()) {
        return Ok(Constant::Ellipsis);
    }
    if bool_::is_bool(o) {
        return Ok(Constant::Bool(o.is(bool_::true_obj())));
    }
    if int::is_int_exact(o) {
        if let Some(v) = int::as_i64(o) {
            return Ok(Constant::Int(Int::Small(v)));
        }
        let mag = int::int_methods_mag_string(o, 10);
        let s = if int::sign(o) < 0 { format!("-{}", mag) } else { mag };
        return Ok(Constant::Int(Int::Big(s)));
    }
    if crate::types::float::is_float_exact(o) {
        return Ok(Constant::Float(crate::types::float::float_value(o)));
    }
    if crate::types::complex::is_complex_exact(o) {
        let c = crate::types::complex::complex_value(o);
        return Ok(Constant::Complex { real: c.real, imag: c.imag });
    }
    if pystr::is_str_exact(o) {
        return Ok(Constant::Str(py_str_to_pystr(o)));
    }
    if bytes::is_bytes_exact(o) {
        return Ok(Constant::Bytes(bytes::bytes_data(o).to_vec()));
    }
    if tuple::is_tuple_exact(o) {
        let mut v = Vec::new();
        for it in tuple::tuple_items(o) {
            v.push(py_to_constant(it)?);
        }
        return Ok(Constant::Tuple(v));
    }
    if o.is_type(crate::types::set::frozenset_type()) {
        let mut v = Vec::new();
        for it in crate::types::set::set_items(o) {
            v.push(py_to_constant(&it)?);
        }
        return Ok(Constant::FrozenSet(v));
    }
    raise!(TypeError, "got an invalid type in Constant: {}", crate::types::typeobject::type_name(o.ty()));
}

fn to_i32(v: &PyObject) -> PyResult<i32> {
    if !int::is_int(v) {
        raise!(ValueError, "invalid integer value: {}", ab::repr_string(v)?);
    }
    match int::as_i64(v) {
        Some(x) if x >= i32::MIN as i64 && x <= i32::MAX as i64 => Ok(x as i32),
        _ => raise!(OverflowError, "Python int too large to convert to C int"),
    }
}

impl C {
    fn enter(&mut self) -> PyResult<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH || self.stack_base.saturating_sub(sp()) > MAX_STACK {
            self.depth -= 1;
            return Err(err!(RecursionError, "maximum recursion depth exceeded during ast construction"));
        }
        Ok(())
    }

    // ---- raw field access
    fn get(&self, o: &PyObject, n: &PyObject) -> PyResult<Option<PyRef>> {
        ab::getattr_opt(o, n)
    }
    fn req(&self, o: &PyObject, n: &PyObject, owner: &str) -> PyResult<PyRef> {
        match self.get(o, n)? {
            Some(v) => Ok(v),
            None => raise!(TypeError, "required field \"{}\" missing from {}", fname(n), owner),
        }
    }
    fn int(&self, o: &PyObject, n: &PyObject, owner: &str) -> PyResult<i32> {
        let v = self.req(o, n, owner)?;
        to_i32(&v)
    }
    fn opt_int(&self, o: &PyObject, n: &PyObject, default: i32) -> PyResult<i32> {
        match self.get(o, n)? {
            Some(v) if !v.is(none::none()) => to_i32(&v),
            _ => Ok(default),
        }
    }
    fn ident_value(&self, v: &PyObject) -> PyResult<Option<String>> {
        if v.is(none::none()) {
            return Ok(None);
        }
        if !pystr::is_str_exact(v) {
            raise!(TypeError, "AST identifier must be of type str");
        }
        Ok(Some(pystr::to_string_lossy(v)))
    }
    fn ident(&self, o: &PyObject, n: &PyObject, owner: &str) -> PyResult<String> {
        let v = self.req(o, n, owner)?;
        match self.ident_value(&v)? {
            Some(s) => Ok(s),
            None => raise!(ValueError, "field '{}' is required for {}", fname(n), owner),
        }
    }
    fn opt_ident(&self, o: &PyObject, n: &PyObject) -> PyResult<Option<String>> {
        match self.get(o, n)? {
            Some(v) => self.ident_value(&v),
            None => Ok(None),
        }
    }
    fn opt_string(&self, o: &PyObject, n: &PyObject) -> PyResult<Option<String>> {
        match self.get(o, n)? {
            Some(v) if !v.is(none::none()) => {
                if pystr::is_str_exact(&v) {
                    Ok(Some(pystr::to_string_lossy(&v)))
                } else if bytes::is_bytes_exact(&v) {
                    Ok(Some(String::from_utf8_lossy(bytes::bytes_data(&v)).into_owned()))
                } else {
                    raise!(TypeError, "AST string must be of type str")
                }
            }
            _ => Ok(None),
        }
    }
    fn string(&self, o: &PyObject, n: &PyObject, owner: &str) -> PyResult<String> {
        match self.opt_string(o, n)? {
            Some(s) => Ok(s),
            None => {
                self.req(o, n, owner)?;
                raise!(ValueError, "field '{}' is required for {}", fname(n), owner)
            }
        }
    }
    fn constant(&self, o: &PyObject, n: &PyObject, owner: &str) -> PyResult<Constant> {
        let v = self.req(o, n, owner)?;
        py_to_constant(&v)
    }

    /// Items of a list field (missing -> empty list). Returns the list object and a snapshot.
    fn list_items(&self, o: &PyObject, n: &PyObject, owner: &str) -> PyResult<(Option<PyRef>, Vec<PyRef>)> {
        let v = match self.get(o, n)? {
            Some(v) => v,
            None => return Ok((None, Vec::new())),
        };
        if !list::is_list(&v) {
            raise!(TypeError, "{} field \"{}\" must be a list, not a {}", owner, fname(n), crate::types::typeobject::type_name(v.ty()));
        }
        let items = list::list_to_vec(&v);
        Ok((Some(v), items))
    }
    fn check_size(&self, l: &Option<PyRef>, n0: usize, n: &PyObject, owner: &str) -> PyResult<()> {
        if let Some(l) = l {
            if list::list_len(l) != n0 {
                raise!(RuntimeError, "{} field \"{}\" changed size during iteration", owner, fname(n));
            }
        }
        Ok(())
    }
    fn seq<T>(&mut self, o: &PyObject, n: &PyObject, owner: &str, mut f: impl FnMut(&mut Self, &PyObject) -> PyResult<T>) -> PyResult<Vec<T>> {
        let (l, items) = self.list_items(o, n, owner)?;
        let mut out = Vec::with_capacity(items.len());
        for it in &items {
            out.push(f(self, it)?);
            self.check_size(&l, items.len(), n, owner)?;
        }
        Ok(out)
    }
    fn exprs(&mut self, o: &PyObject, n: &PyObject, owner: &str) -> PyResult<Vec<Expr>> {
        self.seq(o, n, owner, |c, it| match c.expr(it)? {
            Some(e) => Ok(e),
            None => raise!(ValueError, "None disallowed in expression list"),
        })
    }
    fn opt_exprs(&mut self, o: &PyObject, n: &PyObject, owner: &str) -> PyResult<Vec<Option<Expr>>> {
        self.seq(o, n, owner, |c, it| c.expr(it))
    }
    fn stmts(&mut self, o: &PyObject, n: &PyObject, owner: &str) -> PyResult<Vec<Stmt>> {
        self.seq(o, n, owner, |c, it| match c.stmt(it)? {
            Some(s) => Ok(s),
            None => raise!(ValueError, "None disallowed in statement list"),
        })
    }
    fn idents(&mut self, o: &PyObject, n: &PyObject, owner: &str) -> PyResult<Vec<String>> {
        self.seq(o, n, owner, |c, it| match c.ident_value(it)? {
            Some(s) => Ok(s),
            None => raise!(ValueError, "None disallowed in identifier list"),
        })
    }
    fn patterns(&mut self, o: &PyObject, n: &PyObject, owner: &str) -> PyResult<Vec<Pattern>> {
        self.seq(o, n, owner, |c, it| match c.pattern(it)? {
            Some(p) => Ok(p),
            None => raise!(ValueError, "None disallowed in pattern list"),
        })
    }
    fn expr_req(&mut self, o: &PyObject, n: &PyObject, owner: &str) -> PyResult<Box<Expr>> {
        let v = self.req(o, n, owner)?;
        match self.expr(&v)? {
            Some(e) => Ok(Box::new(e)),
            None => raise!(ValueError, "field '{}' is required for {}", fname(n), owner),
        }
    }
    fn expr_opt(&mut self, o: &PyObject, n: &PyObject) -> PyResult<Option<Box<Expr>>> {
        match self.get(o, n)? {
            Some(v) => Ok(self.expr(&v)?.map(Box::new)),
            None => Ok(None),
        }
    }

    fn loc(&self, o: &PyObject, owner: &str) -> PyResult<Loc> {
        let lineno = self.int(o, name!("lineno"), owner)?;
        let col = self.int(o, name!("col_offset"), owner)?;
        let end_lineno = self.opt_int(o, name!("end_lineno"), lineno)?;
        let end_col = self.opt_int(o, name!("end_col_offset"), col)?;
        Ok(Loc { lineno, col_offset: col, end_lineno, end_col_offset: end_col })
    }
    fn loc_required(&self, o: &PyObject, owner: &str) -> PyResult<Loc> {
        let lineno = self.int(o, name!("lineno"), owner)?;
        let col = self.int(o, name!("col_offset"), owner)?;
        let end_lineno = self.int(o, name!("end_lineno"), owner)?;
        let end_col = self.int(o, name!("end_col_offset"), owner)?;
        Ok(Loc { lineno, col_offset: col, end_lineno, end_col_offset: end_col })
    }

    /// Which of the kinds `ks` is `o` an instance of (first match in order).
    fn which(&self, o: &PyObject, ks: &[K]) -> PyResult<Option<K>> {
        if let Some(k) = self.s.kind_of_exact(o) {
            if ks.contains(&k) {
                return Ok(Some(k));
            }
        }
        for &k in ks {
            if self.s.isinstance(o, k)? {
                return Ok(Some(k));
            }
        }
        Ok(None)
    }
    fn expected(&self, what: &str, o: &PyObject) -> PyErr {
        match ab::repr_string(o) {
            Ok(r) => err!(TypeError, "expected some sort of {}, but got {}", what, r),
            Err(e) => e,
        }
    }

    // ---- simple sums
    fn ctx(&self, o: &PyObject, owner: &str) -> PyResult<ExprContext> {
        let v = self.req(o, name!("ctx"), owner)?;
        match self.which(&v, &[K::Load, K::Store, K::Del])? {
            Some(K::Load) => Ok(ExprContext::Load),
            Some(K::Store) => Ok(ExprContext::Store),
            Some(_) => Ok(ExprContext::Del),
            None => Err(self.expected("expr_context", &v)),
        }
    }
    fn operator_value(&self, v: &PyObject) -> PyResult<Operator> {
        use Operator::*;
        const KS: [K; 13] = [K::Add, K::Sub, K::Mult, K::MatMult, K::Div, K::ModOp, K::Pow, K::LShift, K::RShift, K::BitOr, K::BitXor, K::BitAnd, K::FloorDiv];
        const OPS: [Operator; 13] = [Add, Sub, Mult, MatMult, Div, Mod, Pow, LShift, RShift, BitOr, BitXor, BitAnd, FloorDiv];
        match self.which(v, &KS)? {
            Some(k) => Ok(OPS[KS.iter().position(|x| *x == k).unwrap()]),
            None => Err(self.expected("operator", v)),
        }
    }
    fn operator(&self, o: &PyObject, owner: &str) -> PyResult<Operator> {
        let v = self.req(o, name!("op"), owner)?;
        self.operator_value(&v)
    }
    fn cmpop_value(&self, v: &PyObject) -> PyResult<CmpOp> {
        use CmpOp::*;
        const KS: [K; 10] = [K::Eq, K::NotEq, K::Lt, K::LtE, K::Gt, K::GtE, K::Is, K::IsNot, K::In, K::NotIn];
        const OPS: [CmpOp; 10] = [Eq, NotEq, Lt, LtE, Gt, GtE, Is, IsNot, In, NotIn];
        match self.which(v, &KS)? {
            Some(k) => Ok(OPS[KS.iter().position(|x| *x == k).unwrap()]),
            None => Err(self.expected("cmpop", v)),
        }
    }

    // ---- mod
    fn module(&mut self, o: &PyObject) -> PyResult<Mod> {
        match self.which(o, &[K::Module, K::Interactive, K::Expression, K::FunctionType])? {
            Some(K::Module) => {
                let body = self.stmts(o, name!("body"), "Module")?;
                let type_ignores = self.seq(o, name!("type_ignores"), "Module", |c, it| {
                    match c.which(it, &[K::TypeIgnore])? {
                        Some(_) => {
                            let lineno = c.int(it, name!("lineno"), "TypeIgnore")?;
                            let tag = c.string(it, name!("tag"), "TypeIgnore")?;
                            Ok(TypeIgnore { lineno, tag })
                        }
                        None => Err(c.expected("type_ignore", it)),
                    }
                })?;
                Ok(Mod::Module { body, type_ignores })
            }
            Some(K::Interactive) => Ok(Mod::Interactive { body: self.stmts(o, name!("body"), "Interactive")? }),
            Some(K::Expression) => Ok(Mod::Expression { body: self.expr_req(o, name!("body"), "Expression")? }),
            Some(_) => {
                let argtypes = self.exprs(o, name!("argtypes"), "FunctionType")?;
                let returns = self.expr_req(o, name!("returns"), "FunctionType")?;
                Ok(Mod::FunctionType { argtypes, returns })
            }
            None => Err(self.expected("mod", o)),
        }
    }

    // ---- products
    fn arg(&mut self, o: &PyObject) -> PyResult<Arg> {
        let loc = self.loc(o, "arg")?;
        let arg = self.ident(o, name!("arg"), "arg")?;
        let annotation = self.expr_opt(o, name!("annotation"))?;
        let type_comment = self.opt_string(o, name!("type_comment"))?;
        Ok(Arg { arg, annotation, type_comment, loc })
    }
    fn opt_arg(&mut self, o: &PyObject, n: &PyObject) -> PyResult<Option<Box<Arg>>> {
        match self.get(o, n)? {
            Some(v) if !v.is(none::none()) => Ok(Some(Box::new(self.arg(&v)?))),
            _ => Ok(None),
        }
    }
    fn args_list(&mut self, o: &PyObject, n: &PyObject) -> PyResult<Vec<Arg>> {
        self.seq(o, n, "arguments", |c, it| c.arg(it))
    }
    fn arguments(&mut self, o: &PyObject) -> PyResult<Arguments> {
        let posonlyargs = self.args_list(o, name!("posonlyargs"))?;
        let args = self.args_list(o, name!("args"))?;
        let vararg = self.opt_arg(o, name!("vararg"))?;
        let kwonlyargs = self.args_list(o, name!("kwonlyargs"))?;
        let kw_defaults = self.opt_exprs(o, name!("kw_defaults"), "arguments")?;
        let kwarg = self.opt_arg(o, name!("kwarg"))?;
        let defaults = self.exprs(o, name!("defaults"), "arguments")?;
        Ok(Arguments { posonlyargs, args, vararg, kwonlyargs, kw_defaults, kwarg, defaults })
    }
    fn arguments_field(&mut self, o: &PyObject, owner: &str) -> PyResult<Box<Arguments>> {
        let v = self.req(o, name!("args"), owner)?;
        if v.is(none::none()) {
            raise!(ValueError, "field 'args' is required for {}", owner);
        }
        Ok(Box::new(self.arguments(&v)?))
    }
    fn keyword(&mut self, o: &PyObject) -> PyResult<Keyword> {
        let loc = self.loc(o, "keyword")?;
        let arg = self.opt_ident(o, name!("arg"))?;
        let value = *self.expr_req(o, name!("value"), "keyword")?;
        Ok(Keyword { arg, value, loc })
    }
    fn keywords(&mut self, o: &PyObject, owner: &str) -> PyResult<Vec<Keyword>> {
        self.seq(o, name!("keywords"), owner, |c, it| c.keyword(it))
    }
    fn alias(&mut self, o: &PyObject) -> PyResult<Alias> {
        let loc = self.loc(o, "alias")?;
        let name = self.ident(o, name!("name"), "alias")?;
        let asname = self.opt_ident(o, name!("asname"))?;
        Ok(Alias { name, asname, loc })
    }
    fn aliases(&mut self, o: &PyObject, owner: &str) -> PyResult<Vec<Alias>> {
        self.seq(o, name!("names"), owner, |c, it| c.alias(it))
    }
    fn comprehension(&mut self, o: &PyObject) -> PyResult<Comprehension> {
        let target = *self.expr_req(o, name!("target"), "comprehension")?;
        let iter = *self.expr_req(o, name!("iter"), "comprehension")?;
        let ifs = self.exprs(o, name!("ifs"), "comprehension")?;
        let is_async = self.int(o, name!("is_async"), "comprehension")?;
        Ok(Comprehension { target, iter, ifs, is_async })
    }
    fn generators(&mut self, o: &PyObject, owner: &str) -> PyResult<Vec<Comprehension>> {
        self.seq(o, name!("generators"), owner, |c, it| c.comprehension(it))
    }
    fn withitem(&mut self, o: &PyObject) -> PyResult<WithItem> {
        let context_expr = *self.expr_req(o, name!("context_expr"), "withitem")?;
        let optional_vars = self.expr_opt(o, name!("optional_vars"))?;
        Ok(WithItem { context_expr, optional_vars })
    }
    fn handler(&mut self, o: &PyObject) -> PyResult<ExceptHandler> {
        let loc = self.loc(o, "excepthandler")?;
        if self.which(o, &[K::ExceptHandler])?.is_none() {
            return Err(self.expected("excepthandler", o));
        }
        let type_ = self.expr_opt(o, name!("type"))?;
        let name = self.opt_ident(o, name!("name"))?;
        let body = self.stmts(o, name!("body"), "ExceptHandler")?;
        Ok(ExceptHandler { type_, name, body, loc })
    }
    fn match_case(&mut self, o: &PyObject) -> PyResult<MatchCase> {
        let pv = self.req(o, name!("pattern"), "match_case")?;
        let pattern = match self.pattern(&pv)? {
            Some(p) => p,
            None => raise!(ValueError, "field 'pattern' is required for match_case"),
        };
        let guard = self.expr_opt(o, name!("guard"))?;
        let body = self.stmts(o, name!("body"), "match_case")?;
        Ok(MatchCase { pattern, guard, body })
    }
    fn type_params(&mut self, o: &PyObject, owner: &str) -> PyResult<Vec<TypeParam>> {
        self.seq(o, name!("type_params"), owner, |c, it| {
            if it.is(none::none()) {
                raise!(ValueError, "None disallowed in type_param list");
            }
            let loc = c.loc_required(it, "type_param")?;
            let node = match c.which(it, &[K::TypeVar, K::ParamSpec, K::TypeVarTuple])? {
                Some(K::TypeVar) => TypeParamKind::TypeVar {
                    name: c.ident(it, name!("name"), "TypeVar")?,
                    bound: c.expr_opt(it, name!("bound"))?,
                    default_value: c.expr_opt(it, name!("default_value"))?,
                },
                Some(K::ParamSpec) => TypeParamKind::ParamSpec {
                    name: c.ident(it, name!("name"), "ParamSpec")?,
                    default_value: c.expr_opt(it, name!("default_value"))?,
                },
                Some(_) => TypeParamKind::TypeVarTuple {
                    name: c.ident(it, name!("name"), "TypeVarTuple")?,
                    default_value: c.expr_opt(it, name!("default_value"))?,
                },
                None => return Err(c.expected("type_param", it)),
            };
            Ok(TypeParam { node, loc })
        })
    }

    // ---- stmt
    fn stmt(&mut self, o: &PyObject) -> PyResult<Option<Stmt>> {
        if o.is(none::none()) {
            return Ok(None);
        }
        self.enter()?;
        let r = self.stmt_inner(o);
        self.depth -= 1;
        r.map(Some)
    }
    fn stmt_inner(&mut self, o: &PyObject) -> PyResult<Stmt> {
        use StmtKind as S;
        let loc = self.loc(o, "stmt")?;
        const KS: [K; 28] = [
            K::FunctionDef, K::AsyncFunctionDef, K::ClassDef, K::Return, K::Delete, K::Assign, K::TypeAlias, K::AugAssign,
            K::AnnAssign, K::For, K::AsyncFor, K::While, K::If, K::With, K::AsyncWith, K::Match, K::Raise, K::Try, K::TryStar,
            K::Assert, K::Import, K::ImportFrom, K::Global, K::Nonlocal, K::ExprStmt, K::Pass, K::Break, K::Continue,
        ];
        let k = match self.which(o, &KS)? {
            Some(k) => k,
            None => return Err(self.expected("stmt", o)),
        };
        let node = match k {
            K::FunctionDef | K::AsyncFunctionDef => {
                let owner = if k == K::FunctionDef { "FunctionDef" } else { "AsyncFunctionDef" };
                let fd = FunctionDef {
                    name: self.ident(o, name!("name"), owner)?,
                    args: self.arguments_field(o, owner)?,
                    body: self.stmts(o, name!("body"), owner)?,
                    decorator_list: self.exprs(o, name!("decorator_list"), owner)?,
                    returns: self.expr_opt(o, name!("returns"))?,
                    type_comment: self.opt_string(o, name!("type_comment"))?,
                    type_params: self.type_params(o, owner)?,
                };
                if k == K::FunctionDef { S::FunctionDef(fd) } else { S::AsyncFunctionDef(fd) }
            }
            K::ClassDef => S::ClassDef {
                name: self.ident(o, name!("name"), "ClassDef")?,
                bases: self.exprs(o, name!("bases"), "ClassDef")?,
                keywords: self.keywords(o, "ClassDef")?,
                body: self.stmts(o, name!("body"), "ClassDef")?,
                decorator_list: self.exprs(o, name!("decorator_list"), "ClassDef")?,
                type_params: self.type_params(o, "ClassDef")?,
            },
            K::Return => S::Return { value: self.expr_opt(o, name!("value"))? },
            K::Delete => S::Delete { targets: self.exprs(o, name!("targets"), "Delete")? },
            K::Assign => S::Assign {
                targets: self.exprs(o, name!("targets"), "Assign")?,
                value: self.expr_req(o, name!("value"), "Assign")?,
                type_comment: self.opt_string(o, name!("type_comment"))?,
            },
            K::TypeAlias => S::TypeAlias {
                name: self.expr_req(o, name!("name"), "TypeAlias")?,
                type_params: self.type_params(o, "TypeAlias")?,
                value: self.expr_req(o, name!("value"), "TypeAlias")?,
            },
            K::AugAssign => S::AugAssign {
                target: self.expr_req(o, name!("target"), "AugAssign")?,
                op: self.operator(o, "AugAssign")?,
                value: self.expr_req(o, name!("value"), "AugAssign")?,
            },
            K::AnnAssign => S::AnnAssign {
                target: self.expr_req(o, name!("target"), "AnnAssign")?,
                annotation: self.expr_req(o, name!("annotation"), "AnnAssign")?,
                value: self.expr_opt(o, name!("value"))?,
                simple: self.int(o, name!("simple"), "AnnAssign")?,
            },
            K::For | K::AsyncFor => {
                let owner = if k == K::For { "For" } else { "AsyncFor" };
                let f = For {
                    target: self.expr_req(o, name!("target"), owner)?,
                    iter: self.expr_req(o, name!("iter"), owner)?,
                    body: self.stmts(o, name!("body"), owner)?,
                    orelse: self.stmts(o, name!("orelse"), owner)?,
                    type_comment: self.opt_string(o, name!("type_comment"))?,
                };
                if k == K::For { S::For(f) } else { S::AsyncFor(f) }
            }
            K::While | K::If => {
                let owner = if k == K::While { "While" } else { "If" };
                let test = self.expr_req(o, name!("test"), owner)?;
                let body = self.stmts(o, name!("body"), owner)?;
                let orelse = self.stmts(o, name!("orelse"), owner)?;
                if k == K::While { S::While { test, body, orelse } } else { S::If { test, body, orelse } }
            }
            K::With | K::AsyncWith => {
                let owner = if k == K::With { "With" } else { "AsyncWith" };
                let w = With {
                    items: self.seq(o, name!("items"), owner, |c, it| c.withitem(it))?,
                    body: self.stmts(o, name!("body"), owner)?,
                    type_comment: self.opt_string(o, name!("type_comment"))?,
                };
                if k == K::With { S::With(w) } else { S::AsyncWith(w) }
            }
            K::Match => S::Match {
                subject: self.expr_req(o, name!("subject"), "Match")?,
                cases: self.seq(o, name!("cases"), "Match", |c, it| c.match_case(it))?,
            },
            K::Raise => S::Raise { exc: self.expr_opt(o, name!("exc"))?, cause: self.expr_opt(o, name!("cause"))? },
            K::Try | K::TryStar => {
                let owner = if k == K::Try { "Try" } else { "TryStar" };
                let t = Try {
                    body: self.stmts(o, name!("body"), owner)?,
                    handlers: self.seq(o, name!("handlers"), owner, |c, it| c.handler(it))?,
                    orelse: self.stmts(o, name!("orelse"), owner)?,
                    finalbody: self.stmts(o, name!("finalbody"), owner)?,
                };
                if k == K::Try { S::Try(t) } else { S::TryStar(t) }
            }
            K::Assert => S::Assert { test: self.expr_req(o, name!("test"), "Assert")?, msg: self.expr_opt(o, name!("msg"))? },
            K::Import => S::Import { names: self.aliases(o, "Import")?, is_lazy: self.opt_int(o, name!("is_lazy"), 0)? },
            K::ImportFrom => S::ImportFrom {
                module: self.opt_ident(o, name!("module"))?,
                names: self.aliases(o, "ImportFrom")?,
                level: self.opt_int(o, name!("level"), 0)?,
                is_lazy: self.opt_int(o, name!("is_lazy"), 0)?,
            },
            K::Global => S::Global { names: self.idents(o, name!("names"), "Global")? },
            K::Nonlocal => S::Nonlocal { names: self.idents(o, name!("names"), "Nonlocal")? },
            K::ExprStmt => S::Expr { value: self.expr_req(o, name!("value"), "Expr")? },
            K::Pass => S::Pass,
            K::Break => S::Break,
            _ => S::Continue,
        };
        Ok(Stmt { node, loc })
    }

    // ---- expr
    fn expr(&mut self, o: &PyObject) -> PyResult<Option<Expr>> {
        if o.is(none::none()) {
            return Ok(None);
        }
        self.enter()?;
        let r = self.expr_inner(o);
        self.depth -= 1;
        r.map(Some)
    }
    fn expr_inner(&mut self, o: &PyObject) -> PyResult<Expr> {
        use ExprKind as E;
        let loc = self.loc(o, "expr")?;
        const KS: [K; 29] = [
            K::BoolOp, K::NamedExpr, K::BinOp, K::UnaryOp, K::Lambda, K::IfExp, K::Dict, K::Set, K::ListComp, K::SetComp,
            K::DictComp, K::GeneratorExp, K::Await, K::Yield, K::YieldFrom, K::Compare, K::Call, K::FormattedValue,
            K::Interpolation, K::JoinedStr, K::TemplateStr, K::Constant, K::Attribute, K::Subscript, K::Starred, K::Name,
            K::List, K::Tuple, K::Slice,
        ];
        let k = match self.which(o, &KS)? {
            Some(k) => k,
            None => return Err(self.expected("expr", o)),
        };
        let node = match k {
            K::BoolOp => {
                let v = self.req(o, name!("op"), "BoolOp")?;
                let op = match self.which(&v, &[K::And, K::Or])? {
                    Some(K::And) => BoolOp::And,
                    Some(_) => BoolOp::Or,
                    None => return Err(self.expected("boolop", &v)),
                };
                E::BoolOp { op, values: self.exprs(o, name!("values"), "BoolOp")? }
            }
            K::NamedExpr => E::NamedExpr { target: self.expr_req(o, name!("target"), "NamedExpr")?, value: self.expr_req(o, name!("value"), "NamedExpr")? },
            K::BinOp => E::BinOp {
                left: self.expr_req(o, name!("left"), "BinOp")?,
                op: self.operator(o, "BinOp")?,
                right: self.expr_req(o, name!("right"), "BinOp")?,
            },
            K::UnaryOp => {
                let v = self.req(o, name!("op"), "UnaryOp")?;
                let op = match self.which(&v, &[K::Invert, K::Not, K::UAdd, K::USub])? {
                    Some(K::Invert) => UnaryOp::Invert,
                    Some(K::Not) => UnaryOp::Not,
                    Some(K::UAdd) => UnaryOp::UAdd,
                    Some(_) => UnaryOp::USub,
                    None => return Err(self.expected("unaryop", &v)),
                };
                E::UnaryOp { op, operand: self.expr_req(o, name!("operand"), "UnaryOp")? }
            }
            K::Lambda => E::Lambda { args: self.arguments_field(o, "Lambda")?, body: self.expr_req(o, name!("body"), "Lambda")? },
            K::IfExp => E::IfExp {
                test: self.expr_req(o, name!("test"), "IfExp")?,
                body: self.expr_req(o, name!("body"), "IfExp")?,
                orelse: self.expr_req(o, name!("orelse"), "IfExp")?,
            },
            K::Dict => E::Dict { keys: self.opt_exprs(o, name!("keys"), "Dict")?, values: self.exprs(o, name!("values"), "Dict")? },
            K::Set => E::Set { elts: self.exprs(o, name!("elts"), "Set")? },
            K::ListComp => E::ListComp { elt: self.expr_req(o, name!("elt"), "ListComp")?, generators: self.generators(o, "ListComp")? },
            K::SetComp => E::SetComp { elt: self.expr_req(o, name!("elt"), "SetComp")?, generators: self.generators(o, "SetComp")? },
            K::GeneratorExp => {
                E::GeneratorExp { elt: self.expr_req(o, name!("elt"), "GeneratorExp")?, generators: self.generators(o, "GeneratorExp")? }
            }
            K::DictComp => E::DictComp {
                key: self.expr_req(o, name!("key"), "DictComp")?,
                value: self.expr_opt(o, name!("value"))?,
                generators: self.generators(o, "DictComp")?,
            },
            K::Await => E::Await { value: self.expr_req(o, name!("value"), "Await")? },
            K::Yield => E::Yield { value: self.expr_opt(o, name!("value"))? },
            K::YieldFrom => E::YieldFrom { value: self.expr_req(o, name!("value"), "YieldFrom")? },
            K::Compare => {
                let left = self.expr_req(o, name!("left"), "Compare")?;
                let ops = self.seq(o, name!("ops"), "Compare", |c, it| c.cmpop_value(it))?;
                let comparators = self.exprs(o, name!("comparators"), "Compare")?;
                E::Compare { left, ops, comparators }
            }
            K::Call => E::Call {
                func: self.expr_req(o, name!("func"), "Call")?,
                args: self.exprs(o, name!("args"), "Call")?,
                keywords: self.keywords(o, "Call")?,
            },
            K::FormattedValue => E::FormattedValue {
                value: self.expr_req(o, name!("value"), "FormattedValue")?,
                conversion: self.int(o, name!("conversion"), "FormattedValue")?,
                format_spec: self.expr_opt(o, name!("format_spec"))?,
            },
            K::Interpolation => {
                let value = self.expr_req(o, name!("value"), "Interpolation")?;
                let sv = self.req(o, name!("str"), "Interpolation")?;
                let s = match py_to_constant(&sv)? {
                    Constant::Str(s) => s,
                    _ => PyStr::Utf8(pystr::to_string_lossy(&*ab::str(&sv)?)),
                };
                E::Interpolation {
                    value,
                    str: s,
                    conversion: self.int(o, name!("conversion"), "Interpolation")?,
                    format_spec: self.expr_opt(o, name!("format_spec"))?,
                }
            }
            K::JoinedStr => E::JoinedStr { values: self.exprs(o, name!("values"), "JoinedStr")? },
            K::TemplateStr => E::TemplateStr { values: self.exprs(o, name!("values"), "TemplateStr")? },
            K::Constant => E::Constant { value: self.constant(o, name!("value"), "Constant")?, kind: self.opt_string(o, name!("kind"))? },
            K::Attribute => E::Attribute {
                value: self.expr_req(o, name!("value"), "Attribute")?,
                attr: self.ident(o, name!("attr"), "Attribute")?,
                ctx: self.ctx(o, "Attribute")?,
            },
            K::Subscript => E::Subscript {
                value: self.expr_req(o, name!("value"), "Subscript")?,
                slice: self.expr_req(o, name!("slice"), "Subscript")?,
                ctx: self.ctx(o, "Subscript")?,
            },
            K::Starred => E::Starred { value: self.expr_req(o, name!("value"), "Starred")?, ctx: self.ctx(o, "Starred")? },
            K::Name => E::Name { id: self.ident(o, name!("id"), "Name")?, ctx: self.ctx(o, "Name")? },
            K::List => E::List { elts: self.exprs(o, name!("elts"), "List")?, ctx: self.ctx(o, "List")? },
            K::Tuple => E::Tuple { elts: self.exprs(o, name!("elts"), "Tuple")?, ctx: self.ctx(o, "Tuple")? },
            _ => E::Slice { lower: self.expr_opt(o, name!("lower"))?, upper: self.expr_opt(o, name!("upper"))?, step: self.expr_opt(o, name!("step"))? },
        };
        Ok(Expr { node, loc })
    }

    // ---- pattern
    fn pattern(&mut self, o: &PyObject) -> PyResult<Option<Pattern>> {
        if o.is(none::none()) {
            return Ok(None);
        }
        self.enter()?;
        let r = self.pattern_inner(o);
        self.depth -= 1;
        r.map(Some)
    }
    fn pattern_inner(&mut self, o: &PyObject) -> PyResult<Pattern> {
        use PatternKind as P;
        let loc = self.loc_required(o, "pattern")?;
        const KS: [K; 8] = [K::MatchValue, K::MatchSingleton, K::MatchSequence, K::MatchMapping, K::MatchClass, K::MatchStar, K::MatchAs, K::MatchOr];
        let k = match self.which(o, &KS)? {
            Some(k) => k,
            None => return Err(self.expected("pattern", o)),
        };
        let node = match k {
            K::MatchValue => P::MatchValue { value: self.expr_req(o, name!("value"), "MatchValue")? },
            K::MatchSingleton => P::MatchSingleton { value: self.constant(o, name!("value"), "MatchSingleton")? },
            K::MatchSequence => P::MatchSequence { patterns: self.patterns(o, name!("patterns"), "MatchSequence")? },
            K::MatchMapping => P::MatchMapping {
                keys: self.exprs(o, name!("keys"), "MatchMapping")?,
                patterns: self.patterns(o, name!("patterns"), "MatchMapping")?,
                rest: self.opt_ident(o, name!("rest"))?,
            },
            K::MatchClass => P::MatchClass {
                cls: self.expr_req(o, name!("cls"), "MatchClass")?,
                patterns: self.patterns(o, name!("patterns"), "MatchClass")?,
                kwd_attrs: self.idents(o, name!("kwd_attrs"), "MatchClass")?,
                kwd_patterns: self.patterns(o, name!("kwd_patterns"), "MatchClass")?,
            },
            K::MatchStar => P::MatchStar { name: self.opt_ident(o, name!("name"))? },
            K::MatchAs => {
                let pattern = match self.get(o, name!("pattern"))? {
                    Some(v) => self.pattern(&v)?.map(Box::new),
                    None => None,
                };
                P::MatchAs { pattern, name: self.opt_ident(o, name!("name"))? }
            }
            _ => P::MatchOr { patterns: self.patterns(o, name!("patterns"), "MatchOr")? },
        };
        Ok(Pattern { node, loc })
    }
}

/// Convert a Python AST (root must match `mode`) to the Rust AST. `mode`: 0 exec, 1 eval,
/// 2 single, 3 func_type.
pub fn py_to_mod(o: &PyObject, mode: crate::compile::Mode) -> PyResult<Mod> {
    let s = state()?;
    let (k, nm) = match mode {
        crate::compile::Mode::Exec => (K::Module, "Module"),
        crate::compile::Mode::Eval => (K::Expression, "Expression"),
        crate::compile::Mode::Single => (K::Interactive, "Interactive"),
        crate::compile::Mode::FuncType => (K::FunctionType, "FunctionType"),
    };
    if !s.isinstance(o, k)? {
        raise!(TypeError, "expected {} node, got {}", nm, crate::types::typeobject::type_name(o.ty()));
    }
    let mut c = C { s, depth: 0, stack_base: sp() };
    c.module(o)
}

/// Check that `o` is an AST root of the class required by `mode` (for compile(ast, flags=ONLY_AST)).
pub fn check_mod_type(o: &PyObject, mode: crate::compile::Mode) -> PyResult<()> {
    let s = state()?;
    let (k, nm) = match mode {
        crate::compile::Mode::Exec => (K::Module, "Module"),
        crate::compile::Mode::Eval => (K::Expression, "Expression"),
        crate::compile::Mode::Single => (K::Interactive, "Interactive"),
        crate::compile::Mode::FuncType => (K::FunctionType, "FunctionType"),
    };
    if !s.isinstance(o, k)? {
        raise!(TypeError, "expected {} node, got {}", nm, crate::types::typeobject::type_name(o.ty()));
    }
    Ok(())
}

#[allow(dead_code)]
fn _unused(_: *mut PyTypeObject) {}
