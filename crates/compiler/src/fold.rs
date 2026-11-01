//! Constant folding and AST-level checks.
//!
//! [`fold_module`] runs on a private copy of the AST before symbol-table construction and code
//! generation. It folds what CPython folds observably (unary/binary operations on constants
//! with CPython's size limits, constant tuples, `in <constant list/set>`, `for x in <constant
//! list/set>`, `__debug__`, match-pattern values) and emits the SyntaxWarnings that CPython's
//! AST preprocessing and code generator emit (`is` with a literal, calling/subscripting a
//! literal, `assert` on a tuple, PEP 765 control flow in `finally`).
//!
//! [`optimize_ast`] is the `ast.parse(..., optimize=N)` variant.

use crate::ast::*;
use crate::error::{CompileError, Warning};

const MAX_INT_BITS: u32 = 128;
const MAX_COLLECTION_SIZE: usize = 256;
const MAX_STR_SIZE: usize = 4096;
const MAX_TOTAL_ITEMS: usize = 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctx {
    Loop,
    Finally,
}

struct Folder<'w> {
    optimize: i32,
    /// full folding for compilation (false: only the `ast.parse(optimize=N)` subset)
    full: bool,
    warnings: Option<&'w mut Vec<Warning>>,
    depth: u32,
    max_depth: u32,
    ctx: Vec<Ctx>,
    /// `from __future__ import annotations`: annotations are unparsed later and must keep
    /// their source form (no folding).
    string_annotations: bool,
}

type R = Result<(), CompileError>;

pub fn fold_module(m: &mut Mod, optimize: i32, future: u32, warnings: &mut Vec<Warning>, max_depth: u32) -> R {
    let string_annotations = future & crate::cf::CO_FUTURE_ANNOTATIONS != 0;
    let mut f = Folder { optimize, full: true, warnings: Some(warnings), depth: 0, max_depth, ctx: Vec::new(), string_annotations };
    f.fold_mod(m)
}

pub fn optimize_ast(m: &mut Mod, optimize: i32, _flags: u32) -> R {
    if optimize <= 0 {
        return Ok(());
    }
    let mut f = Folder { optimize, full: false, warnings: None, depth: 0, max_depth: crate::DEFAULT_MAX_DEPTH, ctx: Vec::new(), string_annotations: false };
    f.fold_mod(m)?;
    if optimize >= 2 {
        strip_docstrings_mod(m);
    }
    Ok(())
}

fn strip_docstrings_mod(m: &mut Mod) {
    if let Mod::Module { body, .. } = m {
        strip_docstring(body);
        for s in body.iter_mut() {
            strip_docstrings_stmt(s);
        }
    }
}

fn strip_docstring(body: &mut Vec<Stmt>) {
    if let Some(first) = body.first() {
        if let StmtKind::Expr { value } = &first.node {
            if let ExprKind::Constant { value: Constant::Str(_), .. } = value.node {
                let loc = first.loc;
                if body.len() == 1 {
                    body[0] = Stmt::new(StmtKind::Pass, loc);
                } else {
                    body.remove(0);
                }
            }
        }
    }
}

fn strip_docstrings_stmt(s: &mut Stmt) {
    match &mut s.node {
        StmtKind::FunctionDef(f) | StmtKind::AsyncFunctionDef(f) => {
            strip_docstring(&mut f.body);
            for x in f.body.iter_mut() {
                strip_docstrings_stmt(x);
            }
        }
        StmtKind::ClassDef { body, .. } => {
            strip_docstring(body);
            for x in body.iter_mut() {
                strip_docstrings_stmt(x);
            }
        }
        _ => {}
    }
}

fn type_name_of_const(c: &Constant) -> &'static str {
    match c {
        Constant::None => "NoneType",
        Constant::Bool(_) => "bool",
        Constant::Ellipsis => "ellipsis",
        Constant::Int(_) => "int",
        Constant::Float(_) => "float",
        Constant::Complex { .. } => "complex",
        Constant::Str(_) => "str",
        Constant::Bytes(_) => "bytes",
        Constant::Tuple(_) => "tuple",
        Constant::FrozenSet(_) => "frozenset",
    }
}

/// CPython's infer_type for literal expressions.
fn infer_type(e: &Expr) -> Option<&'static str> {
    Some(match &e.node {
        ExprKind::Tuple { .. } => "tuple",
        ExprKind::List { .. } | ExprKind::ListComp { .. } => "list",
        ExprKind::Dict { .. } | ExprKind::DictComp { .. } => "dict",
        ExprKind::Set { .. } | ExprKind::SetComp { .. } => "set",
        ExprKind::GeneratorExp { .. } => "generator",
        ExprKind::Lambda { .. } => "function",
        ExprKind::JoinedStr { .. } | ExprKind::FormattedValue { .. } => "str",
        ExprKind::TemplateStr { .. } => "string.templatelib.Template",
        ExprKind::Interpolation { .. } => "string.templatelib.Interpolation",
        ExprKind::Constant { value, .. } => type_name_of_const(value),
        ExprKind::Slice { .. } => "slice",
        _ => return None,
    })
}

// ------------------------------------------------------------------ numeric helpers

#[derive(Clone, Copy, Debug)]
enum Num {
    I(i128),
    F(f64),
    C(f64, f64),
}

fn int_value(i: &Int) -> Option<i128> {
    match i {
        Int::Small(v) => Some(*v as i128),
        Int::Big(s) => s.parse::<i128>().ok(),
    }
}

fn make_int(v: i128) -> Constant {
    if v >= i64::MIN as i128 && v <= i64::MAX as i128 {
        Constant::Int(Int::Small(v as i64))
    } else {
        Constant::Int(Int::Big(v.to_string()))
    }
}

fn num_of(c: &Constant) -> Option<Num> {
    match c {
        Constant::Bool(b) => Some(Num::I(*b as i128)),
        Constant::Int(i) => int_value(i).map(Num::I),
        Constant::Float(f) => Some(Num::F(*f)),
        Constant::Complex { real, imag } => Some(Num::C(*real, *imag)),
        _ => None,
    }
}

fn bits(v: i128) -> u32 {
    128 - v.unsigned_abs().leading_zeros()
}

fn int_to_f64(v: i128) -> Option<f64> {
    if v.unsigned_abs() <= (1u128 << 53) {
        Some(v as f64)
    } else {
        None
    }
}

fn py_floor_divmod(a: f64, b: f64) -> (f64, f64) {
    let mut m = a % b;
    let mut div = (a - m) / b;
    if m != 0.0 {
        if (b < 0.0) != (m < 0.0) {
            m += b;
            div -= 1.0;
        }
    } else {
        m = 0.0f64.copysign(b);
    }
    let fd = if div != 0.0 {
        let mut f = div.floor();
        if div - f > 0.5 {
            f += 1.0;
        }
        f
    } else {
        0.0f64.copysign(a / b)
    };
    (fd, m)
}

fn fold_num_binop(op: Operator, a: Num, b: Num) -> Option<Constant> {
    use Num::*;
    match (a, b) {
        (I(x), I(y)) => {
            let r = match op {
                Operator::Add => x.checked_add(y)?,
                Operator::Sub => x.checked_sub(y)?,
                Operator::Mult => {
                    if x != 0 && y != 0 && bits(x) + bits(y) > MAX_INT_BITS {
                        return None;
                    }
                    x.checked_mul(y)?
                }
                Operator::FloorDiv => {
                    if y == 0 {
                        return None;
                    }
                    let q = x.checked_div(y)?;
                    if (x % y != 0) && ((x < 0) != (y < 0)) {
                        q - 1
                    } else {
                        q
                    }
                }
                Operator::Mod => {
                    if y == 0 {
                        return None;
                    }
                    let r = x.checked_rem(y)?;
                    if r != 0 && ((r < 0) != (y < 0)) {
                        r + y
                    } else {
                        r
                    }
                }
                Operator::Pow => {
                    if y < 0 {
                        return None;
                    }
                    if x != 0 && x != 1 && x != -1 && (bits(x) as i128) * y > MAX_INT_BITS as i128 {
                        return None;
                    }
                    let mut acc: i128 = 1;
                    let mut base = x;
                    let mut e = y;
                    while e > 0 {
                        if e & 1 == 1 {
                            acc = acc.checked_mul(base)?;
                        }
                        e >>= 1;
                        if e > 0 {
                            base = base.checked_mul(base)?;
                        }
                    }
                    acc
                }
                Operator::LShift => {
                    if y < 0 {
                        return None;
                    }
                    if x == 0 {
                        0
                    } else {
                        if y > MAX_INT_BITS as i128 || bits(x) as i128 + y > MAX_INT_BITS as i128 - 1 {
                            return None;
                        }
                        x.checked_shl(y as u32)?
                    }
                }
                Operator::RShift => {
                    if y < 0 {
                        return None;
                    }
                    if y >= 127 {
                        if x < 0 {
                            -1
                        } else {
                            0
                        }
                    } else {
                        x >> y
                    }
                }
                Operator::BitAnd => x & y,
                Operator::BitOr => x | y,
                Operator::BitXor => x ^ y,
                Operator::Div => {
                    if y == 0 {
                        return None;
                    }
                    let (fx, fy) = (int_to_f64(x)?, int_to_f64(y)?);
                    return Some(Constant::Float(fx / fy));
                }
                Operator::MatMult => return None,
            };
            Some(make_int(r))
        }
        (I(_), F(_)) | (F(_), I(_)) | (F(_), F(_)) => {
            let fx = match a {
                I(x) => int_to_f64(x)?,
                F(x) => x,
                _ => unreachable!(),
            };
            let fy = match b {
                I(y) => int_to_f64(y)?,
                F(y) => y,
                _ => unreachable!(),
            };
            let r = match op {
                Operator::Add => fx + fy,
                Operator::Sub => fx - fy,
                Operator::Mult => fx * fy,
                Operator::Div => {
                    if fy == 0.0 {
                        return None;
                    }
                    fx / fy
                }
                Operator::FloorDiv | Operator::Mod => {
                    if fy == 0.0 || !fx.is_finite() || !fy.is_finite() {
                        return None;
                    }
                    let (d, m) = py_floor_divmod(fx, fy);
                    if op == Operator::FloorDiv {
                        d
                    } else {
                        m
                    }
                }
                _ => return None,
            };
            Some(Constant::Float(r))
        }
        _ => {
            // complex arithmetic
            let to_c = |n: Num| -> Option<(f64, f64)> {
                match n {
                    I(x) => Some((int_to_f64(x)?, 0.0)),
                    F(x) => Some((x, 0.0)),
                    C(r, i) => Some((r, i)),
                }
            };
            let (ar, ai) = to_c(a)?;
            let (br, bi) = to_c(b)?;
            let a_is_c = matches!(a, C(..));
            let b_is_c = matches!(b, C(..));
            let (r, i) = match op {
                Operator::Add => {
                    // real + complex keeps the sign of the complex imaginary part
                    if !a_is_c {
                        (ar + br, bi)
                    } else if !b_is_c {
                        (ar + br, ai)
                    } else {
                        (ar + br, ai + bi)
                    }
                }
                Operator::Sub => {
                    if !a_is_c {
                        (ar - br, -bi)
                    } else if !b_is_c {
                        (ar - br, ai)
                    } else {
                        (ar - br, ai - bi)
                    }
                }
                Operator::Mult => {
                    if !a_is_c {
                        (ar * br, ar * bi)
                    } else if !b_is_c {
                        (ar * br, ai * br)
                    } else {
                        (ar * br - ai * bi, ar * bi + ai * br)
                    }
                }
                _ => return None,
            };
            Some(Constant::Complex { real: r, imag: i })
        }
    }
}

fn str_len(s: &PyStr) -> usize {
    s.char_len()
}

fn const_size(c: &Constant) -> usize {
    match c {
        Constant::Tuple(v) | Constant::FrozenSet(v) => v.iter().map(const_size).sum::<usize>() + 1,
        _ => 1,
    }
}

fn fold_binop(op: Operator, a: &Constant, b: &Constant) -> Option<Constant> {
    if let (Some(x), Some(y)) = (num_of(a), num_of(b)) {
        // bool & bool, bool | bool, bool ^ bool stay bool
        if let (Constant::Bool(p), Constant::Bool(q)) = (a, b) {
            match op {
                Operator::BitAnd => return Some(Constant::Bool(*p & *q)),
                Operator::BitOr => return Some(Constant::Bool(*p | *q)),
                Operator::BitXor => return Some(Constant::Bool(*p ^ *q)),
                _ => {}
            }
        }
        return fold_num_binop(op, x, y);
    }
    let rep = |n: &Constant| -> Option<i128> {
        match n {
            Constant::Int(i) => int_value(i),
            Constant::Bool(b) => Some(*b as i128),
            _ => None,
        }
    };
    match (op, a, b) {
        (Operator::Add, Constant::Str(x), Constant::Str(y)) => {
            if str_len(x) + str_len(y) > MAX_STR_SIZE {
                return None;
            }
            let mut s = x.clone();
            s.push_pystr(y);
            Some(Constant::Str(s))
        }
        (Operator::Add, Constant::Bytes(x), Constant::Bytes(y)) => {
            if x.len() + y.len() > MAX_STR_SIZE {
                return None;
            }
            let mut v = x.clone();
            v.extend_from_slice(y);
            Some(Constant::Bytes(v))
        }
        (Operator::Add, Constant::Tuple(x), Constant::Tuple(y)) => {
            if x.len() + y.len() > MAX_COLLECTION_SIZE {
                return None;
            }
            let mut v = x.clone();
            v.extend(y.iter().cloned());
            Some(Constant::Tuple(v))
        }
        (Operator::Mult, Constant::Str(_) | Constant::Bytes(_) | Constant::Tuple(_), _) => repeat(a, rep(b)?),
        (Operator::Mult, _, Constant::Str(_) | Constant::Bytes(_) | Constant::Tuple(_)) => repeat(b, rep(a)?),
        _ => None,
    }
}

fn repeat(seq: &Constant, n: i128) -> Option<Constant> {
    let n = if n < 0 { 0 } else { n as usize };
    match seq {
        Constant::Str(s) => {
            let l = str_len(s);
            if l > 0 && n > MAX_STR_SIZE / l {
                return None;
            }
            let mut out = PyStr::new();
            for _ in 0..n {
                out.push_pystr(s);
            }
            Some(Constant::Str(out))
        }
        Constant::Bytes(b) => {
            if !b.is_empty() && n > MAX_STR_SIZE / b.len() {
                return None;
            }
            Some(Constant::Bytes(b.repeat(n)))
        }
        Constant::Tuple(t) => {
            if !t.is_empty() && n > MAX_COLLECTION_SIZE / t.len() {
                return None;
            }
            let total: usize = t.iter().map(const_size).sum::<usize>() * n;
            if total > MAX_TOTAL_ITEMS {
                return None;
            }
            let mut v = Vec::with_capacity(t.len() * n);
            for _ in 0..n {
                v.extend(t.iter().cloned());
            }
            Some(Constant::Tuple(v))
        }
        _ => None,
    }
}

fn fold_unary(op: UnaryOp, c: &Constant) -> Option<Constant> {
    match op {
        UnaryOp::Not => crate::codegen::const_truth(c).map(|t| Constant::Bool(!t)),
        UnaryOp::USub => match c {
            Constant::Bool(b) => Some(make_int(-(*b as i128))),
            Constant::Int(i) => Some(make_int(-int_value(i)?)),
            Constant::Float(f) => Some(Constant::Float(-f)),
            Constant::Complex { real, imag } => Some(Constant::Complex { real: -real, imag: -imag }),
            _ => None,
        },
        UnaryOp::UAdd => match c {
            Constant::Bool(b) => Some(make_int(*b as i128)),
            Constant::Int(_) | Constant::Float(_) | Constant::Complex { .. } => Some(c.clone()),
            _ => None,
        },
        UnaryOp::Invert => match c {
            Constant::Int(i) => Some(make_int(!int_value(i)?)),
            _ => None,
        },
    }
}

fn as_const(e: &Expr) -> Option<&Constant> {
    match &e.node {
        ExprKind::Constant { value, .. } => Some(value),
        _ => None,
    }
}

fn mk_const(c: Constant, loc: Loc) -> Expr {
    Expr::new(ExprKind::Constant { value: c, kind: None }, loc)
}

impl<'w> Folder<'w> {
    fn warn(&mut self, msg: String, loc: Loc) {
        if let Some(w) = &mut self.warnings {
            w.push(Warning::syntax(msg, loc));
        }
    }

    fn enter(&mut self) -> R {
        self.depth += 1;
        if self.depth > self.max_depth {
            return Err(CompileError::recursion());
        }
        Ok(())
    }

    fn fold_mod(&mut self, m: &mut Mod) -> R {
        match m {
            Mod::Module { body, .. } | Mod::Interactive { body } => self.fold_body(body),
            Mod::Expression { body } => self.fold_expr(body),
            Mod::FunctionType { argtypes, returns } => {
                for e in argtypes.iter_mut() {
                    self.fold_expr(e)?;
                }
                self.fold_expr(returns)
            }
        }
    }

    fn fold_body(&mut self, body: &mut [Stmt]) -> R {
        for s in body.iter_mut() {
            self.fold_stmt(s)?;
        }
        Ok(())
    }

    fn fold_function_body(&mut self, body: &mut [Stmt]) -> R {
        let saved = std::mem::take(&mut self.ctx);
        let r = self.fold_body(body);
        self.ctx = saved;
        r
    }

    fn fold_annotation(&mut self, e: &mut Expr) -> R {
        if self.string_annotations {
            let full = self.full;
            self.full = false;
            let r = self.fold_expr(e);
            self.full = full;
            r
        } else {
            self.fold_expr(e)
        }
    }

    fn fold_args(&mut self, a: &mut Arguments) -> R {
        for d in a.defaults.iter_mut() {
            self.fold_expr(d)?;
        }
        for d in a.kw_defaults.iter_mut().flatten() {
            self.fold_expr(d)?;
        }
        for arg in a.posonlyargs.iter_mut().chain(a.args.iter_mut()).chain(a.kwonlyargs.iter_mut()) {
            if let Some(ann) = &mut arg.annotation {
                self.fold_annotation(ann)?;
            }
        }
        for arg in a.vararg.iter_mut().chain(a.kwarg.iter_mut()) {
            if let Some(ann) = &mut arg.annotation {
                self.fold_annotation(ann)?;
            }
        }
        Ok(())
    }

    fn fold_type_params(&mut self, tps: &mut [TypeParam]) -> R {
        for tp in tps.iter_mut() {
            match &mut tp.node {
                TypeParamKind::TypeVar { bound, default_value, .. } => {
                    if let Some(b) = bound {
                        self.fold_expr(b)?;
                    }
                    if let Some(d) = default_value {
                        self.fold_expr(d)?;
                    }
                }
                TypeParamKind::ParamSpec { default_value, .. } | TypeParamKind::TypeVarTuple { default_value, .. } => {
                    if let Some(d) = default_value {
                        self.fold_expr(d)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn fold_stmt(&mut self, s: &mut Stmt) -> R {
        self.enter()?;
        let r = self.fold_stmt_inner(s);
        self.depth -= 1;
        r
    }

    fn fold_iter(&mut self, iter: &mut Expr) -> R {
        self.fold_expr(iter)?;
        if self.full {
            // for x in [1, 2, 3]  ->  for x in (1, 2, 3);  {1, 2} -> frozenset
            self.fold_iter_target(iter);
        }
        Ok(())
    }

    fn fold_iter_target(&mut self, e: &mut Expr) {
        match &e.node {
            ExprKind::List { elts, .. } if elts.iter().all(|x| as_const(x).is_some()) => {
                let v: Vec<Constant> = elts.iter().map(|x| as_const(x).unwrap().clone()).collect();
                *e = mk_const(Constant::Tuple(v), e.loc);
            }
            ExprKind::Set { elts } if elts.iter().all(|x| as_const(x).is_some()) => {
                let v: Vec<Constant> = elts.iter().map(|x| as_const(x).unwrap().clone()).collect();
                *e = mk_const(Constant::FrozenSet(v), e.loc);
            }
            _ => {}
        }
    }

    fn fold_stmt_inner(&mut self, s: &mut Stmt) -> R {
        let loc = s.loc;
        match &mut s.node {
            StmtKind::FunctionDef(f) | StmtKind::AsyncFunctionDef(f) => {
                for d in f.decorator_list.iter_mut() {
                    self.fold_expr(d)?;
                }
                self.fold_type_params(&mut f.type_params)?;
                self.fold_args(&mut f.args)?;
                if let Some(r) = &mut f.returns {
                    self.fold_annotation(r)?;
                }
                self.fold_function_body(&mut f.body)?;
            }
            StmtKind::ClassDef { bases, keywords, body, decorator_list, type_params, .. } => {
                for d in decorator_list.iter_mut() {
                    self.fold_expr(d)?;
                }
                self.fold_type_params(type_params)?;
                for b in bases.iter_mut() {
                    self.fold_expr(b)?;
                }
                for k in keywords.iter_mut() {
                    self.fold_expr(&mut k.value)?;
                }
                self.fold_function_body(body)?;
            }
            StmtKind::Return { value } => {
                if self.ctx.contains(&Ctx::Finally) {
                    self.warn("'return' in a 'finally' block".to_string(), loc);
                }
                if let Some(v) = value {
                    self.fold_expr(v)?;
                }
            }
            StmtKind::Delete { targets } => {
                for t in targets.iter_mut() {
                    self.fold_expr(t)?;
                }
            }
            StmtKind::Assign { targets, value, .. } => {
                for t in targets.iter_mut() {
                    self.fold_expr(t)?;
                }
                self.fold_expr(value)?;
            }
            StmtKind::TypeAlias { name, type_params, value } => {
                self.fold_expr(name)?;
                self.fold_type_params(type_params)?;
                self.fold_expr(value)?;
            }
            StmtKind::AugAssign { target, value, .. } => {
                self.fold_expr(target)?;
                self.fold_expr(value)?;
            }
            StmtKind::AnnAssign { target, annotation, value, .. } => {
                self.fold_expr(target)?;
                self.fold_annotation(annotation)?;
                if let Some(v) = value {
                    self.fold_expr(v)?;
                }
            }
            StmtKind::For(f) | StmtKind::AsyncFor(f) => {
                let is_async = matches!(s.node, StmtKind::AsyncFor(_));
                let f = match &mut s.node {
                    StmtKind::For(f) | StmtKind::AsyncFor(f) => f,
                    _ => unreachable!(),
                };
                self.fold_expr(&mut f.target)?;
                if is_async {
                    self.fold_expr(&mut f.iter)?;
                } else {
                    self.fold_iter(&mut f.iter)?;
                }
                self.ctx.push(Ctx::Loop);
                let r = self.fold_body(&mut f.body);
                self.ctx.pop();
                r?;
                self.fold_body(&mut f.orelse)?;
            }
            StmtKind::While { test, body, orelse } => {
                self.fold_expr(test)?;
                self.ctx.push(Ctx::Loop);
                let r = self.fold_body(body);
                self.ctx.pop();
                r?;
                self.fold_body(orelse)?;
            }
            StmtKind::If { test, body, orelse } => {
                self.fold_expr(test)?;
                self.fold_body(body)?;
                self.fold_body(orelse)?;
            }
            StmtKind::With(w) | StmtKind::AsyncWith(w) => {
                for it in w.items.iter_mut() {
                    self.fold_expr(&mut it.context_expr)?;
                    if let Some(v) = &mut it.optional_vars {
                        self.fold_expr(v)?;
                    }
                }
                self.fold_body(&mut w.body)?;
            }
            StmtKind::Match { subject, cases } => {
                self.fold_expr(subject)?;
                for c in cases.iter_mut() {
                    self.fold_pattern(&mut c.pattern)?;
                    if let Some(g) = &mut c.guard {
                        self.fold_expr(g)?;
                    }
                    self.fold_body(&mut c.body)?;
                }
            }
            StmtKind::Raise { exc, cause } => {
                if let Some(e) = exc {
                    self.fold_expr(e)?;
                }
                if let Some(c) = cause {
                    self.fold_expr(c)?;
                }
            }
            StmtKind::Try(t) | StmtKind::TryStar(t) => {
                self.fold_body(&mut t.body)?;
                for h in t.handlers.iter_mut() {
                    if let Some(ty) = &mut h.type_ {
                        self.fold_expr(ty)?;
                    }
                    self.fold_body(&mut h.body)?;
                }
                self.fold_body(&mut t.orelse)?;
                self.ctx.push(Ctx::Finally);
                let r = self.fold_body(&mut t.finalbody);
                self.ctx.pop();
                r?;
            }
            StmtKind::Assert { test, msg } => {
                if let ExprKind::Tuple { elts, .. } = &test.node {
                    if !elts.is_empty() {
                        self.warn("assertion is always true, perhaps remove parentheses?".to_string(), loc);
                    }
                }
                self.fold_expr(test)?;
                if let Some(m) = msg {
                    self.fold_expr(m)?;
                }
            }
            StmtKind::Expr { value } => self.fold_expr(value)?,
            StmtKind::Break | StmtKind::Continue => {
                let kw = if matches!(s.node, StmtKind::Break) { "break" } else { "continue" };
                for c in self.ctx.iter().rev() {
                    match c {
                        Ctx::Loop => break,
                        Ctx::Finally => {
                            self.warn(format!("'{}' in a 'finally' block", kw), loc);
                            break;
                        }
                    }
                }
            }
            StmtKind::Import { .. }
            | StmtKind::ImportFrom { .. }
            | StmtKind::Global { .. }
            | StmtKind::Nonlocal { .. }
            | StmtKind::Pass => {}
        }
        Ok(())
    }

    fn fold_pattern(&mut self, p: &mut Pattern) -> R {
        self.enter()?;
        let r = self.fold_pattern_inner(p);
        self.depth -= 1;
        r
    }

    fn fold_pattern_inner(&mut self, p: &mut Pattern) -> R {
        match &mut p.node {
            PatternKind::MatchValue { value } => self.fold_match_value(value)?,
            PatternKind::MatchSingleton { .. } | PatternKind::MatchStar { .. } => {}
            PatternKind::MatchSequence { patterns } | PatternKind::MatchOr { patterns } => {
                for q in patterns.iter_mut() {
                    self.fold_pattern(q)?;
                }
            }
            PatternKind::MatchMapping { keys, patterns, .. } => {
                for k in keys.iter_mut() {
                    self.fold_match_value(k)?;
                }
                for q in patterns.iter_mut() {
                    self.fold_pattern(q)?;
                }
            }
            PatternKind::MatchClass { cls, patterns, kwd_patterns, .. } => {
                self.fold_expr(cls)?;
                for q in patterns.iter_mut().chain(kwd_patterns.iter_mut()) {
                    self.fold_pattern(q)?;
                }
            }
            PatternKind::MatchAs { pattern, .. } => {
                if let Some(q) = pattern {
                    self.fold_pattern(q)?;
                }
            }
        }
        Ok(())
    }

    /// Match values (`-1`, `1 + 2j`, ...) are always folded, even in the optimize=1 AST.
    fn fold_match_value(&mut self, e: &mut Expr) -> R {
        let full = self.full;
        self.full = true;
        let r = self.fold_expr(e);
        self.full = full;
        r
    }

    fn fold_expr(&mut self, e: &mut Expr) -> R {
        self.enter()?;
        let r = self.fold_expr_inner(e);
        self.depth -= 1;
        r
    }

    fn fold_comprehensions(&mut self, gens: &mut [Comprehension]) -> R {
        for g in gens.iter_mut() {
            self.fold_expr(&mut g.target)?;
            self.fold_iter(&mut g.iter)?;
            for i in g.ifs.iter_mut() {
                self.fold_expr(i)?;
            }
        }
        Ok(())
    }

    fn check_compare(&mut self, e: &Expr) {
        if let ExprKind::Compare { left, ops, comparators } = &e.node {
            let is_arg = |x: &Expr| -> bool {
                match &x.node {
                    ExprKind::Constant { value, .. } => {
                        matches!(value, Constant::None | Constant::Bool(_) | Constant::Ellipsis)
                    }
                    _ => true,
                }
            };
            let mut left_ok = is_arg(left);
            let mut left_expr: &Expr = left;
            for (op, right) in ops.iter().zip(comparators.iter()) {
                let right_ok = is_arg(right);
                if matches!(op, CmpOp::Is | CmpOp::IsNot) && (!right_ok || !left_ok) {
                    let lit = if !left_ok { left_expr } else { right };
                    let tn = infer_type(lit).unwrap_or("object");
                    let msg = if *op == CmpOp::Is {
                        format!("\"is\" with '{}' literal. Did you mean \"==\"?", tn)
                    } else {
                        format!("\"is not\" with '{}' literal. Did you mean \"!=\"?", tn)
                    };
                    self.warn(msg, e.loc);
                    return;
                }
                left_ok = right_ok;
                left_expr = right;
            }
        }
    }

    fn check_call(&mut self, e: &Expr) {
        if let ExprKind::Call { func, .. } = &e.node {
            let lit = matches!(
                func.node,
                ExprKind::Constant { .. }
                    | ExprKind::Tuple { .. }
                    | ExprKind::List { .. }
                    | ExprKind::ListComp { .. }
                    | ExprKind::Dict { .. }
                    | ExprKind::DictComp { .. }
                    | ExprKind::Set { .. }
                    | ExprKind::SetComp { .. }
                    | ExprKind::GeneratorExp { .. }
                    | ExprKind::JoinedStr { .. }
                    | ExprKind::TemplateStr { .. }
                    | ExprKind::FormattedValue { .. }
                    | ExprKind::Interpolation { .. }
            );
            if lit {
                let tn = infer_type(func).unwrap_or("object");
                self.warn(format!("'{}' object is not callable; perhaps you missed a comma?", tn), func.loc);
            }
        }
    }

    fn check_subscript(&mut self, e: &Expr) {
        if let ExprKind::Subscript { value, slice, ctx: ExprContext::Load } = &e.node {
            // check_subscripter
            let bad = match &value.node {
                ExprKind::Constant { value: c, .. } => matches!(
                    c,
                    Constant::None | Constant::Ellipsis | Constant::Int(_) | Constant::Bool(_) | Constant::Float(_) | Constant::Complex { .. } | Constant::FrozenSet(_)
                ),
                ExprKind::Set { .. }
                | ExprKind::SetComp { .. }
                | ExprKind::GeneratorExp { .. }
                | ExprKind::TemplateStr { .. }
                | ExprKind::Interpolation { .. }
                | ExprKind::Lambda { .. } => true,
                _ => false,
            };
            if bad {
                let tn = infer_type(value).unwrap_or("object");
                self.warn(format!("'{}' object is not subscriptable; perhaps you missed a comma?", tn), value.loc);
                return;
            }
            // check_index
            let it = match infer_type(slice) {
                None => return,
                Some(t) => t,
            };
            if matches!(it, "int" | "bool" | "slice") {
                return;
            }
            let seq = match &value.node {
                ExprKind::Constant { value: c, .. } => matches!(c, Constant::Str(_) | Constant::Bytes(_) | Constant::Tuple(_)),
                ExprKind::Tuple { .. } | ExprKind::List { .. } | ExprKind::ListComp { .. } | ExprKind::JoinedStr { .. } | ExprKind::FormattedValue { .. } => true,
                _ => false,
            };
            if seq {
                let tn = infer_type(value).unwrap_or("object");
                self.warn(format!("{} indices must be integers or slices, not {}; perhaps you missed a comma?", tn, it), value.loc);
            }
        }
    }

    fn fold_expr_inner(&mut self, e: &mut Expr) -> R {
        let loc = e.loc;
        // warnings on the unfolded expression
        match &e.node {
            ExprKind::Compare { .. } => self.check_compare(e),
            ExprKind::Call { .. } => self.check_call(e),
            ExprKind::Subscript { .. } => self.check_subscript(e),
            _ => {}
        }
        let mut replace: Option<Expr> = None;
        match &mut e.node {
            ExprKind::BoolOp { values, .. } => {
                for v in values.iter_mut() {
                    self.fold_expr(v)?;
                }
            }
            ExprKind::NamedExpr { target, value } => {
                self.fold_expr(target)?;
                self.fold_expr(value)?;
            }
            ExprKind::BinOp { left, op, right } => {
                // iterative descent for long left-nested chains
                self.fold_expr(left)?;
                self.fold_expr(right)?;
                if self.full {
                    if let (Some(a), Some(b)) = (as_const(left), as_const(right)) {
                        if let Some(c) = fold_binop(*op, a, b) {
                            replace = Some(mk_const(c, loc));
                        }
                    }
                }
            }
            ExprKind::UnaryOp { op, operand } => {
                self.fold_expr(operand)?;
                if self.full {
                    if let Some(c) = as_const(operand) {
                        if let Some(r) = fold_unary(*op, c) {
                            replace = Some(mk_const(r, loc));
                        }
                    }
                }
            }
            ExprKind::Lambda { args, body } => {
                self.fold_args(args)?;
                self.fold_expr(body)?;
            }
            ExprKind::IfExp { test, body, orelse } => {
                self.fold_expr(test)?;
                self.fold_expr(body)?;
                self.fold_expr(orelse)?;
            }
            ExprKind::Dict { keys, values } => {
                for k in keys.iter_mut().flatten() {
                    self.fold_expr(k)?;
                }
                for v in values.iter_mut() {
                    self.fold_expr(v)?;
                }
            }
            ExprKind::Set { elts } => {
                for v in elts.iter_mut() {
                    self.fold_expr(v)?;
                }
            }
            ExprKind::ListComp { elt, generators } | ExprKind::SetComp { elt, generators } | ExprKind::GeneratorExp { elt, generators } => {
                self.fold_comprehensions(generators)?;
                self.fold_expr(elt)?;
            }
            ExprKind::DictComp { key, value, generators } => {
                self.fold_comprehensions(generators)?;
                self.fold_expr(key)?;
                if let Some(v) = value {
                    self.fold_expr(v)?;
                }
            }
            ExprKind::Await { value } | ExprKind::YieldFrom { value } => self.fold_expr(value)?,
            ExprKind::Yield { value } => {
                if let Some(v) = value {
                    self.fold_expr(v)?;
                }
            }
            ExprKind::Compare { left, ops, comparators } => {
                self.fold_expr(left)?;
                for c in comparators.iter_mut() {
                    self.fold_expr(c)?;
                }
                if self.full {
                    // x in [1, 2] -> x in (1, 2); x in {1, 2} -> frozenset
                    if let (Some(CmpOp::In | CmpOp::NotIn), Some(last)) = (ops.last(), comparators.last_mut()) {
                        self.fold_iter_target(last);
                    }
                }
            }
            ExprKind::Call { func, args, keywords } => {
                self.fold_expr(func)?;
                for a in args.iter_mut() {
                    self.fold_expr(a)?;
                }
                for k in keywords.iter_mut() {
                    self.fold_expr(&mut k.value)?;
                }
            }
            ExprKind::FormattedValue { value, format_spec, .. } | ExprKind::Interpolation { value, format_spec, .. } => {
                self.fold_expr(value)?;
                if let Some(f) = format_spec {
                    self.fold_expr(f)?;
                }
            }
            ExprKind::JoinedStr { values } | ExprKind::TemplateStr { values } => {
                for v in values.iter_mut() {
                    self.fold_expr(v)?;
                }
            }
            ExprKind::Constant { .. } => {}
            ExprKind::Attribute { value, .. } => self.fold_expr(value)?,
            ExprKind::Subscript { value, slice, .. } => {
                self.fold_expr(value)?;
                self.fold_expr(slice)?;
            }
            ExprKind::Starred { value, .. } => self.fold_expr(value)?,
            ExprKind::Name { id, ctx } => {
                if *ctx == ExprContext::Load && id == "__debug__" && (self.full || self.optimize > 0) {
                    replace = Some(mk_const(Constant::Bool(self.optimize <= 0), loc));
                }
            }
            ExprKind::List { elts, .. } => {
                for v in elts.iter_mut() {
                    self.fold_expr(v)?;
                }
            }
            ExprKind::Tuple { elts, ctx } => {
                for v in elts.iter_mut() {
                    self.fold_expr(v)?;
                }
                if self.full && *ctx == ExprContext::Load && elts.iter().all(|x| as_const(x).is_some()) {
                    let v: Vec<Constant> = elts.iter().map(|x| as_const(x).unwrap().clone()).collect();
                    replace = Some(mk_const(Constant::Tuple(v), loc));
                }
            }
            ExprKind::Slice { lower, upper, step } => {
                for x in [lower, upper, step].into_iter().flatten() {
                    self.fold_expr(x)?;
                }
            }
        }
        if let Some(r) = replace {
            *e = r;
        }
        Ok(())
    }
}
