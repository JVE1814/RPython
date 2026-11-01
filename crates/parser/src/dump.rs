//! Render an AST in the format of `test.test_ast.utils.to_tuple` (Python repr of nested tuples).

use crate::ast::*;

/// Python `repr()` of a str.
pub fn repr_str(s: &PyStr) -> String {
    let cps: Vec<u32> = s.code_points().collect();
    let has_sq = cps.contains(&('\'' as u32));
    let has_dq = cps.contains(&('"' as u32));
    let q = if has_sq && !has_dq { '"' } else { '\'' };
    let mut out = String::new();
    out.push(q);
    for cp in cps {
        match cp {
            0x5C => out.push_str("\\\\"),
            0x0A => out.push_str("\\n"),
            0x0D => out.push_str("\\r"),
            0x09 => out.push_str("\\t"),
            c if c == q as u32 => {
                out.push('\\');
                out.push(q);
            }
            c if c < 0x20 || c == 0x7F => out.push_str(&format!("\\x{:02x}", c)),
            c if (0xD800..0xE000).contains(&c) => out.push_str(&format!("\\u{:04x}", c)),
            c => {
                let ch = char::from_u32(c).unwrap();
                if (0x80..0xA1).contains(&c) || c == 0xAD {
                    out.push_str(&format!("\\x{:02x}", c));
                } else if !is_printable_approx(ch) {
                    if c <= 0xFFFF {
                        out.push_str(&format!("\\u{:04x}", c));
                    } else {
                        out.push_str(&format!("\\U{:08x}", c));
                    }
                } else {
                    out.push(ch);
                }
            }
        }
    }
    out.push(q);
    out
}

fn is_printable_approx(c: char) -> bool {
    !(c.is_control() || (c.is_whitespace() && c != ' ') || matches!(c as u32, 0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x2064 | 0xFEFF))
}

/// Python `repr()` of bytes.
pub fn repr_bytes(b: &[u8]) -> String {
    let has_sq = b.contains(&b'\'');
    let has_dq = b.contains(&b'"');
    let q = if has_sq && !has_dq { b'"' } else { b'\'' };
    let mut out = String::from("b");
    out.push(q as char);
    for &c in b {
        match c {
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(q as char);
            }
            c if !(0x20..0x7F).contains(&c) => out.push_str(&format!("\\x{:02x}", c)),
            c => out.push(c as char),
        }
    }
    out.push(q as char);
    out
}

/// Python `repr()` of a float.
pub fn repr_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    // shortest round-trip digits via Rust's {:e}
    let s = format!("{:e}", f);
    let (mant, exp) = s.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let neg = mant.starts_with('-');
    let mant = mant.trim_start_matches('-');
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if !(-4..16).contains(&exp) {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        if exp < 0 {
            out.push('-');
        } else {
            out.push('+');
        }
        out.push_str(&format!("{:02}", exp.abs()));
        return out;
    }
    if exp >= 0 {
        let e = exp as usize;
        if digits.len() <= e + 1 {
            out.push_str(&digits);
            for _ in 0..(e + 1 - digits.len()) {
                out.push('0');
            }
            out.push_str(".0");
        } else {
            out.push_str(&digits[..e + 1]);
            out.push('.');
            out.push_str(&digits[e + 1..]);
        }
    } else {
        out.push_str("0.");
        for _ in 0..(-exp - 1) {
            out.push('0');
        }
        out.push_str(&digits);
    }
    out
}

fn repr_imag(f: f64) -> String {
    let r = repr_float(f);
    let r = r.strip_suffix(".0").map(|s| s.to_string()).unwrap_or(r);
    format!("{}j", r)
}

pub fn repr_constant(c: &Constant) -> String {
    match c {
        Constant::None => "None".into(),
        Constant::Bool(true) => "True".into(),
        Constant::Bool(false) => "False".into(),
        Constant::Ellipsis => "Ellipsis".into(),
        Constant::Int(Int::Small(v)) => v.to_string(),
        Constant::Int(Int::Big(s)) => s.clone(),
        Constant::Float(f) => repr_float(*f),
        Constant::Complex { real, imag } => {
            if *real == 0.0 && !real.is_sign_negative() {
                repr_imag(*imag)
            } else {
                let im = repr_imag(*imag);
                let sign = if im.starts_with('-') { "" } else { "+" };
                let re = repr_float(*real);
                let re = re.strip_suffix(".0").map(|s| s.to_string()).unwrap_or(re);
                format!("({}{}{})", re, sign, im)
            }
        }
        Constant::Str(s) => repr_str(s),
        Constant::Bytes(b) => repr_bytes(b),
        Constant::Tuple(v) => {
            let parts: Vec<String> = v.iter().map(repr_constant).collect();
            if parts.len() == 1 {
                format!("({},)", parts[0])
            } else {
                format!("({})", parts.join(", "))
            }
        }
        Constant::FrozenSet(v) => {
            if v.is_empty() {
                "frozenset()".into()
            } else {
                let parts: Vec<String> = v.iter().map(repr_constant).collect();
                format!("frozenset({{{}}})", parts.join(", "))
            }
        }
    }
}

struct W {
    out: String,
}

impl W {
    fn s(&mut self, x: &str) {
        self.out.push_str(x);
    }
    fn ident(&mut self, x: &str) {
        let r = repr_str(&PyStr::Utf8(x.to_string()));
        self.out.push_str(&r);
    }
    fn opt_ident(&mut self, x: &Option<String>) {
        match x {
            Some(v) => self.ident(v),
            None => self.s("None"),
        }
    }
    fn loc(&mut self, l: &Loc) {
        self.out.push_str(&format!("({}, {}, {}, {})", l.lineno, l.col_offset, l.end_lineno, l.end_col_offset));
    }
    fn begin(&mut self, name: &str, loc: Option<&Loc>) {
        self.s("('");
        self.s(name);
        self.s("'");
        if let Some(l) = loc {
            self.s(", ");
            self.loc(l);
        }
    }
    fn sep(&mut self) {
        self.s(", ");
    }
    fn end(&mut self, nfields: usize, has_loc: bool) {
        if nfields == 0 && !has_loc {
            self.s(",");
        }
        self.s(")");
    }
    fn unit(&mut self, name: &str) {
        self.s("('");
        self.s(name);
        self.s("',)");
    }
    fn list<T>(&mut self, v: &[T], f: impl Fn(&mut Self, &T)) {
        self.s("[");
        for (i, x) in v.iter().enumerate() {
            if i > 0 {
                self.s(", ");
            }
            f(self, x);
        }
        self.s("]");
    }
    fn opt_expr(&mut self, e: &Option<Box<Expr>>) {
        match e {
            Some(e) => self.expr(e),
            None => self.s("None"),
        }
    }
    fn int(&mut self, v: i32) {
        self.s(&v.to_string());
    }

    fn mod_(&mut self, m: &Mod) {
        match m {
            Mod::Module { body, type_ignores } => {
                self.begin("Module", None);
                self.sep();
                self.list(body, |w, s| w.stmt(s));
                self.sep();
                self.list(type_ignores, |w, t| {
                    w.begin("TypeIgnore", None);
                    w.sep();
                    w.int(t.lineno);
                    w.sep();
                    w.ident(&t.tag);
                    w.end(2, false);
                });
                self.end(2, false);
            }
            Mod::Interactive { body } => {
                self.begin("Interactive", None);
                self.sep();
                self.list(body, |w, s| w.stmt(s));
                self.end(1, false);
            }
            Mod::Expression { body } => {
                self.begin("Expression", None);
                self.sep();
                self.expr(body);
                self.end(1, false);
            }
            Mod::FunctionType { argtypes, returns } => {
                self.begin("FunctionType", None);
                self.sep();
                self.list(argtypes, |w, e| w.expr(e));
                self.sep();
                self.expr(returns);
                self.end(2, false);
            }
        }
    }

    fn stmts(&mut self, v: &[Stmt]) {
        self.list(v, |w, s| w.stmt(s));
    }
    fn exprs(&mut self, v: &[Expr]) {
        self.list(v, |w, e| w.expr(e));
    }

    fn funcdef(&mut self, name: &str, f: &FunctionDef, loc: &Loc) {
        self.begin(name, Some(loc));
        self.sep();
        self.ident(&f.name);
        self.sep();
        self.arguments(&f.args);
        self.sep();
        self.stmts(&f.body);
        self.sep();
        self.exprs(&f.decorator_list);
        self.sep();
        self.opt_expr(&f.returns);
        self.sep();
        self.opt_ident(&f.type_comment);
        self.sep();
        self.list(&f.type_params, |w, t| w.type_param(t));
        self.s(")");
    }

    fn stmt(&mut self, s: &Stmt) {
        let l = &s.loc;
        use StmtKind as K;
        match &s.node {
            K::FunctionDef(f) => self.funcdef("FunctionDef", f, l),
            K::AsyncFunctionDef(f) => self.funcdef("AsyncFunctionDef", f, l),
            K::ClassDef { name, bases, keywords, body, decorator_list, type_params } => {
                self.begin("ClassDef", Some(l));
                self.sep();
                self.ident(name);
                self.sep();
                self.exprs(bases);
                self.sep();
                self.list(keywords, |w, k| w.keyword(k));
                self.sep();
                self.stmts(body);
                self.sep();
                self.exprs(decorator_list);
                self.sep();
                self.list(type_params, |w, t| w.type_param(t));
                self.s(")");
            }
            K::Return { value } => {
                self.begin("Return", Some(l));
                self.sep();
                self.opt_expr(value);
                self.s(")");
            }
            K::Delete { targets } => {
                self.begin("Delete", Some(l));
                self.sep();
                self.exprs(targets);
                self.s(")");
            }
            K::Assign { targets, value, type_comment } => {
                self.begin("Assign", Some(l));
                self.sep();
                self.exprs(targets);
                self.sep();
                self.expr(value);
                self.sep();
                self.opt_ident(type_comment);
                self.s(")");
            }
            K::TypeAlias { name, type_params, value } => {
                self.begin("TypeAlias", Some(l));
                self.sep();
                self.expr(name);
                self.sep();
                self.list(type_params, |w, t| w.type_param(t));
                self.sep();
                self.expr(value);
                self.s(")");
            }
            K::AugAssign { target, op, value } => {
                self.begin("AugAssign", Some(l));
                self.sep();
                self.expr(target);
                self.sep();
                self.unit(op.name());
                self.sep();
                self.expr(value);
                self.s(")");
            }
            K::AnnAssign { target, annotation, value, simple } => {
                self.begin("AnnAssign", Some(l));
                self.sep();
                self.expr(target);
                self.sep();
                self.expr(annotation);
                self.sep();
                self.opt_expr(value);
                self.sep();
                self.int(*simple);
                self.s(")");
            }
            K::For(f) | K::AsyncFor(f) => {
                self.begin(if matches!(s.node, K::For(_)) { "For" } else { "AsyncFor" }, Some(l));
                self.sep();
                self.expr(&f.target);
                self.sep();
                self.expr(&f.iter);
                self.sep();
                self.stmts(&f.body);
                self.sep();
                self.stmts(&f.orelse);
                self.sep();
                self.opt_ident(&f.type_comment);
                self.s(")");
            }
            K::While { test, body, orelse } | K::If { test, body, orelse } => {
                self.begin(if matches!(s.node, K::While { .. }) { "While" } else { "If" }, Some(l));
                self.sep();
                self.expr(test);
                self.sep();
                self.stmts(body);
                self.sep();
                self.stmts(orelse);
                self.s(")");
            }
            K::With(w) | K::AsyncWith(w) => {
                self.begin(if matches!(s.node, K::With(_)) { "With" } else { "AsyncWith" }, Some(l));
                self.sep();
                self.list(&w.items, |w, it| {
                    w.begin("withitem", None);
                    w.sep();
                    w.expr(&it.context_expr);
                    w.sep();
                    w.opt_expr(&it.optional_vars);
                    w.s(")");
                });
                self.sep();
                self.stmts(&w.body);
                self.sep();
                self.opt_ident(&w.type_comment);
                self.s(")");
            }
            K::Match { subject, cases } => {
                self.begin("Match", Some(l));
                self.sep();
                self.expr(subject);
                self.sep();
                self.list(cases, |w, c| {
                    w.begin("match_case", None);
                    w.sep();
                    w.pattern(&c.pattern);
                    w.sep();
                    w.opt_expr(&c.guard);
                    w.sep();
                    w.stmts(&c.body);
                    w.s(")");
                });
                self.s(")");
            }
            K::Raise { exc, cause } => {
                self.begin("Raise", Some(l));
                self.sep();
                self.opt_expr(exc);
                self.sep();
                self.opt_expr(cause);
                self.s(")");
            }
            K::Try(t) | K::TryStar(t) => {
                self.begin(if matches!(s.node, K::Try(_)) { "Try" } else { "TryStar" }, Some(l));
                self.sep();
                self.stmts(&t.body);
                self.sep();
                self.list(&t.handlers, |w, h| {
                    w.begin("ExceptHandler", Some(&h.loc));
                    w.sep();
                    w.opt_expr(&h.type_);
                    w.sep();
                    w.opt_ident(&h.name);
                    w.sep();
                    w.stmts(&h.body);
                    w.s(")");
                });
                self.sep();
                self.stmts(&t.orelse);
                self.sep();
                self.stmts(&t.finalbody);
                self.s(")");
            }
            K::Assert { test, msg } => {
                self.begin("Assert", Some(l));
                self.sep();
                self.expr(test);
                self.sep();
                self.opt_expr(msg);
                self.s(")");
            }
            K::Import { names, is_lazy } => {
                self.begin("Import", Some(l));
                self.sep();
                self.list(names, |w, a| w.alias(a));
                self.sep();
                self.int(*is_lazy);
                self.s(")");
            }
            K::ImportFrom { module, names, level, is_lazy } => {
                self.begin("ImportFrom", Some(l));
                self.sep();
                self.opt_ident(module);
                self.sep();
                self.list(names, |w, a| w.alias(a));
                self.sep();
                self.int(*level);
                self.sep();
                self.int(*is_lazy);
                self.s(")");
            }
            K::Global { names } | K::Nonlocal { names } => {
                self.begin(if matches!(s.node, K::Global { .. }) { "Global" } else { "Nonlocal" }, Some(l));
                self.sep();
                self.list(names, |w, n| w.ident(n));
                self.s(")");
            }
            K::Expr { value } => {
                self.begin("Expr", Some(l));
                self.sep();
                self.expr(value);
                self.s(")");
            }
            K::Pass => {
                self.begin("Pass", Some(l));
                self.s(")");
            }
            K::Break => {
                self.begin("Break", Some(l));
                self.s(")");
            }
            K::Continue => {
                self.begin("Continue", Some(l));
                self.s(")");
            }
        }
    }

    fn alias(&mut self, a: &Alias) {
        self.begin("alias", Some(&a.loc));
        self.sep();
        self.ident(&a.name);
        self.sep();
        self.opt_ident(&a.asname);
        self.s(")");
    }

    fn keyword(&mut self, k: &Keyword) {
        self.begin("keyword", Some(&k.loc));
        self.sep();
        self.opt_ident(&k.arg);
        self.sep();
        self.expr(&k.value);
        self.s(")");
    }

    fn arg(&mut self, a: &Arg) {
        self.begin("arg", Some(&a.loc));
        self.sep();
        self.ident(&a.arg);
        self.sep();
        self.opt_expr(&a.annotation);
        self.sep();
        self.opt_ident(&a.type_comment);
        self.s(")");
    }

    fn arguments(&mut self, a: &Arguments) {
        self.begin("arguments", None);
        self.sep();
        self.list(&a.posonlyargs, |w, x| w.arg(x));
        self.sep();
        self.list(&a.args, |w, x| w.arg(x));
        self.sep();
        match &a.vararg {
            Some(x) => self.arg(x),
            None => self.s("None"),
        }
        self.sep();
        self.list(&a.kwonlyargs, |w, x| w.arg(x));
        self.sep();
        self.list(&a.kw_defaults, |w, x| match x {
            Some(e) => w.expr(e),
            None => w.s("None"),
        });
        self.sep();
        match &a.kwarg {
            Some(x) => self.arg(x),
            None => self.s("None"),
        }
        self.sep();
        self.exprs(&a.defaults);
        self.s(")");
    }

    fn type_param(&mut self, t: &TypeParam) {
        match &t.node {
            TypeParamKind::TypeVar { name, bound, default_value } => {
                self.begin("TypeVar", Some(&t.loc));
                self.sep();
                self.ident(name);
                self.sep();
                self.opt_expr(bound);
                self.sep();
                self.opt_expr(default_value);
                self.s(")");
            }
            TypeParamKind::ParamSpec { name, default_value } | TypeParamKind::TypeVarTuple { name, default_value } => {
                let n = if matches!(t.node, TypeParamKind::ParamSpec { .. }) { "ParamSpec" } else { "TypeVarTuple" };
                self.begin(n, Some(&t.loc));
                self.sep();
                self.ident(name);
                self.sep();
                self.opt_expr(default_value);
                self.s(")");
            }
        }
    }

    fn comprehension(&mut self, c: &Comprehension) {
        self.begin("comprehension", None);
        self.sep();
        self.expr(&c.target);
        self.sep();
        self.expr(&c.iter);
        self.sep();
        self.exprs(&c.ifs);
        self.sep();
        self.int(c.is_async);
        self.s(")");
    }

    fn pattern(&mut self, p: &Pattern) {
        let l = &p.loc;
        match &p.node {
            PatternKind::MatchValue { value } => {
                self.begin("MatchValue", Some(l));
                self.sep();
                self.expr(value);
                self.s(")");
            }
            PatternKind::MatchSingleton { value } => {
                self.begin("MatchSingleton", Some(l));
                self.sep();
                self.s(&repr_constant(value));
                self.s(")");
            }
            PatternKind::MatchSequence { patterns } => {
                self.begin("MatchSequence", Some(l));
                self.sep();
                self.list(patterns, |w, x| w.pattern(x));
                self.s(")");
            }
            PatternKind::MatchMapping { keys, patterns, rest } => {
                self.begin("MatchMapping", Some(l));
                self.sep();
                self.exprs(keys);
                self.sep();
                self.list(patterns, |w, x| w.pattern(x));
                self.sep();
                self.opt_ident(rest);
                self.s(")");
            }
            PatternKind::MatchClass { cls, patterns, kwd_attrs, kwd_patterns } => {
                self.begin("MatchClass", Some(l));
                self.sep();
                self.expr(cls);
                self.sep();
                self.list(patterns, |w, x| w.pattern(x));
                self.sep();
                self.list(kwd_attrs, |w, x| w.ident(x));
                self.sep();
                self.list(kwd_patterns, |w, x| w.pattern(x));
                self.s(")");
            }
            PatternKind::MatchStar { name } => {
                self.begin("MatchStar", Some(l));
                self.sep();
                self.opt_ident(name);
                self.s(")");
            }
            PatternKind::MatchAs { pattern, name } => {
                self.begin("MatchAs", Some(l));
                self.sep();
                match pattern {
                    Some(p) => self.pattern(p),
                    None => self.s("None"),
                }
                self.sep();
                self.opt_ident(name);
                self.s(")");
            }
            PatternKind::MatchOr { patterns } => {
                self.begin("MatchOr", Some(l));
                self.sep();
                self.list(patterns, |w, x| w.pattern(x));
                self.s(")");
            }
        }
    }

    fn expr(&mut self, e: &Expr) {
        let l = &e.loc;
        use ExprKind as K;
        match &e.node {
            K::BoolOp { op, values } => {
                self.begin("BoolOp", Some(l));
                self.sep();
                self.unit(op.name());
                self.sep();
                self.exprs(values);
                self.s(")");
            }
            K::NamedExpr { target, value } => {
                self.begin("NamedExpr", Some(l));
                self.sep();
                self.expr(target);
                self.sep();
                self.expr(value);
                self.s(")");
            }
            K::BinOp { left, op, right } => {
                self.begin("BinOp", Some(l));
                self.sep();
                self.expr(left);
                self.sep();
                self.unit(op.name());
                self.sep();
                self.expr(right);
                self.s(")");
            }
            K::UnaryOp { op, operand } => {
                self.begin("UnaryOp", Some(l));
                self.sep();
                self.unit(op.name());
                self.sep();
                self.expr(operand);
                self.s(")");
            }
            K::Lambda { args, body } => {
                self.begin("Lambda", Some(l));
                self.sep();
                self.arguments(args);
                self.sep();
                self.expr(body);
                self.s(")");
            }
            K::IfExp { test, body, orelse } => {
                self.begin("IfExp", Some(l));
                self.sep();
                self.expr(test);
                self.sep();
                self.expr(body);
                self.sep();
                self.expr(orelse);
                self.s(")");
            }
            K::Dict { keys, values } => {
                self.begin("Dict", Some(l));
                self.sep();
                self.list(keys, |w, k| match k {
                    Some(k) => w.expr(k),
                    None => w.s("None"),
                });
                self.sep();
                self.exprs(values);
                self.s(")");
            }
            K::Set { elts } => {
                self.begin("Set", Some(l));
                self.sep();
                self.exprs(elts);
                self.s(")");
            }
            K::ListComp { elt, generators } | K::SetComp { elt, generators } | K::GeneratorExp { elt, generators } => {
                let n = match &e.node {
                    K::ListComp { .. } => "ListComp",
                    K::SetComp { .. } => "SetComp",
                    _ => "GeneratorExp",
                };
                self.begin(n, Some(l));
                self.sep();
                self.expr(elt);
                self.sep();
                self.list(generators, |w, c| w.comprehension(c));
                self.s(")");
            }
            K::DictComp { key, value, generators } => {
                self.begin("DictComp", Some(l));
                self.sep();
                self.expr(key);
                self.sep();
                self.opt_expr(value);
                self.sep();
                self.list(generators, |w, c| w.comprehension(c));
                self.s(")");
            }
            K::Await { value } | K::YieldFrom { value } => {
                self.begin(if matches!(e.node, K::Await { .. }) { "Await" } else { "YieldFrom" }, Some(l));
                self.sep();
                self.expr(value);
                self.s(")");
            }
            K::Yield { value } => {
                self.begin("Yield", Some(l));
                self.sep();
                self.opt_expr(value);
                self.s(")");
            }
            K::Compare { left, ops, comparators } => {
                self.begin("Compare", Some(l));
                self.sep();
                self.expr(left);
                self.sep();
                self.list(ops, |w, o| w.unit(o.name()));
                self.sep();
                self.exprs(comparators);
                self.s(")");
            }
            K::Call { func, args, keywords } => {
                self.begin("Call", Some(l));
                self.sep();
                self.expr(func);
                self.sep();
                self.exprs(args);
                self.sep();
                self.list(keywords, |w, k| w.keyword(k));
                self.s(")");
            }
            K::FormattedValue { value, conversion, format_spec } => {
                self.begin("FormattedValue", Some(l));
                self.sep();
                self.expr(value);
                self.sep();
                self.int(*conversion);
                self.sep();
                self.opt_expr(format_spec);
                self.s(")");
            }
            K::Interpolation { value, str, conversion, format_spec } => {
                self.begin("Interpolation", Some(l));
                self.sep();
                self.expr(value);
                self.sep();
                self.s(&repr_str(str));
                self.sep();
                self.int(*conversion);
                self.sep();
                self.opt_expr(format_spec);
                self.s(")");
            }
            K::JoinedStr { values } | K::TemplateStr { values } => {
                self.begin(if matches!(e.node, K::JoinedStr { .. }) { "JoinedStr" } else { "TemplateStr" }, Some(l));
                self.sep();
                self.exprs(values);
                self.s(")");
            }
            K::Constant { value, kind } => {
                self.begin("Constant", Some(l));
                self.sep();
                self.s(&repr_constant(value));
                self.sep();
                self.opt_ident(kind);
                self.s(")");
            }
            K::Attribute { value, attr, ctx } => {
                self.begin("Attribute", Some(l));
                self.sep();
                self.expr(value);
                self.sep();
                self.ident(attr);
                self.sep();
                self.unit(ctx.name());
                self.s(")");
            }
            K::Subscript { value, slice, ctx } => {
                self.begin("Subscript", Some(l));
                self.sep();
                self.expr(value);
                self.sep();
                self.expr(slice);
                self.sep();
                self.unit(ctx.name());
                self.s(")");
            }
            K::Starred { value, ctx } => {
                self.begin("Starred", Some(l));
                self.sep();
                self.expr(value);
                self.sep();
                self.unit(ctx.name());
                self.s(")");
            }
            K::Name { id, ctx } => {
                self.begin("Name", Some(l));
                self.sep();
                self.ident(id);
                self.sep();
                self.unit(ctx.name());
                self.s(")");
            }
            K::List { elts, ctx } | K::Tuple { elts, ctx } => {
                self.begin(if matches!(e.node, K::List { .. }) { "List" } else { "Tuple" }, Some(l));
                self.sep();
                self.exprs(elts);
                self.sep();
                self.unit(ctx.name());
                self.s(")");
            }
            K::Slice { lower, upper, step } => {
                self.begin("Slice", Some(l));
                self.sep();
                self.opt_expr(lower);
                self.sep();
                self.opt_expr(upper);
                self.sep();
                self.opt_expr(step);
                self.s(")");
            }
        }
    }
}

/// `to_tuple(tree)` as in `Lib/test/test_ast/utils.py`, rendered with Python's `repr`.
pub fn to_tuple(m: &Mod) -> String {
    let mut w = W { out: String::new() };
    w.mod_(m);
    let _ = W::end;
    w.out
}

/// `to_tuple` of a single expression.
pub fn expr_to_tuple(e: &Expr) -> String {
    let mut w = W { out: String::new() };
    w.expr(e);
    w.out
}
