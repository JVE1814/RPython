//! Expression unparser (CPython `ast_unparse.c` semantics), used for
//! `from __future__ import annotations` string annotations and error messages.

use crate::ast::*;

const PR_TUPLE: u8 = 0;
const PR_TEST: u8 = 1;
const PR_OR: u8 = 2;
const PR_AND: u8 = 3;
const PR_NOT: u8 = 4;
const PR_CMP: u8 = 5;
const PR_BOR: u8 = 6;
const PR_BXOR: u8 = 7;
const PR_BAND: u8 = 8;
const PR_SHIFT: u8 = 9;
const PR_ARITH: u8 = 10;
const PR_TERM: u8 = 11;
const PR_FACTOR: u8 = 12;
const PR_POWER: u8 = 13;
const PR_AWAIT: u8 = 14;
const PR_ATOM: u8 = 15;

/// Unparse an annotation as a str constant value (code points).
pub fn unparse_annotation(e: &Expr) -> Vec<u32> {
    unparse_expr(e).chars().map(|c| c as u32).collect()
}

/// Unparse an expression (at "test" level, like CPython's annotation stringification).
pub fn unparse_expr(e: &Expr) -> String {
    let mut s = String::new();
    expr(&mut s, e, PR_TEST);
    s
}

fn binop_prec(op: Operator) -> u8 {
    match op {
        Operator::Add | Operator::Sub => PR_ARITH,
        Operator::Mult | Operator::MatMult | Operator::Div | Operator::Mod | Operator::FloorDiv => PR_TERM,
        Operator::LShift | Operator::RShift => PR_SHIFT,
        Operator::BitOr => PR_BOR,
        Operator::BitXor => PR_BXOR,
        Operator::BitAnd => PR_BAND,
        Operator::Pow => PR_POWER,
    }
}

fn cmp_str(op: CmpOp) -> &'static str {
    match op {
        CmpOp::Eq => "==",
        CmpOp::NotEq => "!=",
        CmpOp::Lt => "<",
        CmpOp::LtE => "<=",
        CmpOp::Gt => ">",
        CmpOp::GtE => ">=",
        CmpOp::Is => "is",
        CmpOp::IsNot => "is not",
        CmpOp::In => "in",
        CmpOp::NotIn => "not in",
    }
}

fn join<'e>(s: &mut String, items: impl Iterator<Item = &'e Expr>, level: u8) {
    for (i, x) in items.enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        expr(s, x, level);
    }
}

fn comprehensions(s: &mut String, gens: &[Comprehension]) {
    for g in gens {
        s.push_str(if g.is_async != 0 { " async for " } else { " for " });
        expr(s, &g.target, PR_TUPLE);
        s.push_str(" in ");
        expr(s, &g.iter, PR_TEST + 1);
        for i in &g.ifs {
            s.push_str(" if ");
            expr(s, i, PR_TEST + 1);
        }
    }
}

fn args(s: &mut String, a: &Arguments) {
    let mut first = true;
    let mut sep = |s: &mut String| {
        if !first {
            s.push_str(", ");
        }
        first = false;
    };
    let npos = a.posonlyargs.len() + a.args.len();
    let ndef = a.defaults.len();
    for (i, arg) in a.posonlyargs.iter().chain(a.args.iter()).enumerate() {
        sep(s);
        s.push_str(&arg.arg);
        if let Some(ann) = &arg.annotation {
            s.push_str(": ");
            expr(s, ann, PR_TEST);
        }
        if i + ndef >= npos {
            s.push('=');
            expr(s, &a.defaults[i + ndef - npos], PR_TEST);
        }
        if i + 1 == a.posonlyargs.len() {
            s.push_str(", /");
        }
    }
    if a.vararg.is_some() || !a.kwonlyargs.is_empty() {
        sep(s);
        s.push('*');
        if let Some(v) = &a.vararg {
            s.push_str(&v.arg);
            if let Some(ann) = &v.annotation {
                s.push_str(": ");
                expr(s, ann, PR_TEST);
            }
        }
    }
    for (arg, d) in a.kwonlyargs.iter().zip(a.kw_defaults.iter()) {
        sep(s);
        s.push_str(&arg.arg);
        if let Some(ann) = &arg.annotation {
            s.push_str(": ");
            expr(s, ann, PR_TEST);
        }
        if let Some(d) = d {
            s.push('=');
            expr(s, d, PR_TEST);
        }
    }
    if let Some(k) = &a.kwarg {
        sep(s);
        s.push_str("**");
        s.push_str(&k.arg);
        if let Some(ann) = &k.annotation {
            s.push_str(": ");
            expr(s, ann, PR_TEST);
        }
    }
}

fn fstring_body(s: &mut String, values: &[Expr]) {
    for v in values {
        match &v.node {
            ExprKind::Constant { value: Constant::Str(t), .. } => {
                for c in t.to_string_lossy().chars() {
                    match c {
                        '{' => s.push_str("{{"),
                        '}' => s.push_str("}}"),
                        _ => s.push(c),
                    }
                }
            }
            ExprKind::FormattedValue { value, conversion, format_spec } | ExprKind::Interpolation { value, conversion, format_spec, .. } => {
                let mut inner = String::new();
                match &v.node {
                    // t-strings keep the source text of the expression
                    ExprKind::Interpolation { str: text, .. } => inner.push_str(&text.to_string_lossy()),
                    _ => expr(&mut inner, value, PR_TEST + 1),
                }
                s.push('{');
                if inner.starts_with('{') {
                    s.push(' ');
                }
                s.push_str(&inner);
                if *conversion > 0 {
                    s.push('!');
                    s.push(char::from_u32(*conversion as u32).unwrap_or('r'));
                }
                if let Some(spec) = format_spec {
                    s.push(':');
                    if let ExprKind::JoinedStr { values } = &spec.node {
                        fstring_body(s, values);
                    }
                }
                s.push('}');
            }
            _ => {}
        }
    }
}

fn expr(s: &mut String, e: &Expr, level: u8) {
    match &e.node {
        ExprKind::BoolOp { op, values } => {
            let pr = if *op == BoolOp::And { PR_AND } else { PR_OR };
            if level > pr {
                s.push('(');
            }
            for (i, v) in values.iter().enumerate() {
                if i > 0 {
                    s.push_str(if *op == BoolOp::And { " and " } else { " or " });
                }
                expr(s, v, pr + 1);
            }
            if level > pr {
                s.push(')');
            }
        }
        ExprKind::NamedExpr { target, value } => {
            if level > PR_TUPLE {
                s.push('(');
            }
            expr(s, target, PR_ATOM);
            s.push_str(" := ");
            expr(s, value, PR_ATOM);
            if level > PR_TUPLE {
                s.push(')');
            }
        }
        ExprKind::BinOp { left, op, right } => {
            let pr = binop_prec(*op);
            let rassoc = (*op == Operator::Pow) as u8;
            if level > pr {
                s.push('(');
            }
            expr(s, left, pr + rassoc);
            s.push(' ');
            s.push_str(op.symbol());
            s.push(' ');
            expr(s, right, pr + 1 - rassoc);
            if level > pr {
                s.push(')');
            }
        }
        ExprKind::UnaryOp { op, operand } => {
            let (pr, txt) = match op {
                UnaryOp::Not => (PR_NOT, "not "),
                UnaryOp::Invert => (PR_FACTOR, "~"),
                UnaryOp::UAdd => (PR_FACTOR, "+"),
                UnaryOp::USub => (PR_FACTOR, "-"),
            };
            if level > pr {
                s.push('(');
            }
            s.push_str(txt);
            expr(s, operand, pr);
            if level > pr {
                s.push(')');
            }
        }
        ExprKind::Lambda { args: a, body } => {
            if level > PR_TEST {
                s.push('(');
            }
            let n = a.posonlyargs.len() + a.args.len() + a.kwonlyargs.len() + a.vararg.is_some() as usize + a.kwarg.is_some() as usize;
            s.push_str(if n > 0 { "lambda " } else { "lambda" });
            args(s, a);
            s.push_str(": ");
            expr(s, body, PR_TEST);
            if level > PR_TEST {
                s.push(')');
            }
        }
        ExprKind::IfExp { test, body, orelse } => {
            if level > PR_TEST {
                s.push('(');
            }
            expr(s, body, PR_TEST + 1);
            s.push_str(" if ");
            expr(s, test, PR_TEST + 1);
            s.push_str(" else ");
            expr(s, orelse, PR_TEST);
            if level > PR_TEST {
                s.push(')');
            }
        }
        ExprKind::Dict { keys, values } => {
            s.push('{');
            for (i, (k, v)) in keys.iter().zip(values.iter()).enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                match k {
                    Some(k) => {
                        expr(s, k, PR_TEST);
                        s.push_str(": ");
                        expr(s, v, PR_TEST);
                    }
                    None => {
                        s.push_str("**");
                        expr(s, v, PR_EXPR);
                    }
                }
            }
            s.push('}');
        }
        ExprKind::Set { elts } => {
            if elts.is_empty() {
                s.push_str("{*()}");
            } else {
                s.push('{');
                join(s, elts.iter(), PR_TEST);
                s.push('}');
            }
        }
        ExprKind::ListComp { elt, generators } => {
            s.push('[');
            expr(s, elt, PR_TEST);
            comprehensions(s, generators);
            s.push(']');
        }
        ExprKind::SetComp { elt, generators } => {
            s.push('{');
            expr(s, elt, PR_TEST);
            comprehensions(s, generators);
            s.push('}');
        }
        ExprKind::DictComp { key, value, generators } => {
            s.push('{');
            match value {
                Some(v) => {
                    expr(s, key, PR_TEST);
                    s.push_str(": ");
                    expr(s, v, PR_TEST);
                }
                None => {
                    s.push_str("**");
                    expr(s, key, PR_EXPR);
                }
            }
            comprehensions(s, generators);
            s.push('}');
        }
        ExprKind::GeneratorExp { elt, generators } => {
            s.push('(');
            expr(s, elt, PR_TEST);
            comprehensions(s, generators);
            s.push(')');
        }
        ExprKind::Await { value } => {
            if level > PR_AWAIT {
                s.push('(');
            }
            s.push_str("await ");
            expr(s, value, PR_ATOM);
            if level > PR_AWAIT {
                s.push(')');
            }
        }
        ExprKind::Yield { value } => {
            s.push_str("(yield");
            if let Some(v) = value {
                s.push(' ');
                expr(s, v, PR_TEST);
            }
            s.push(')');
        }
        ExprKind::YieldFrom { value } => {
            s.push_str("(yield from ");
            expr(s, value, PR_TEST);
            s.push(')');
        }
        ExprKind::Compare { left, ops, comparators } => {
            if level > PR_CMP {
                s.push('(');
            }
            expr(s, left, PR_CMP + 1);
            for (op, c) in ops.iter().zip(comparators.iter()) {
                s.push(' ');
                s.push_str(cmp_str(*op));
                s.push(' ');
                expr(s, c, PR_CMP + 1);
            }
            if level > PR_CMP {
                s.push(')');
            }
        }
        ExprKind::Call { func, args: a, keywords } => {
            expr(s, func, PR_ATOM);
            if a.len() == 1 && keywords.is_empty() {
                if let ExprKind::GeneratorExp { .. } = a[0].node {
                    expr(s, &a[0], PR_TEST);
                    return;
                }
            }
            s.push('(');
            let mut first = true;
            for x in a {
                if !first {
                    s.push_str(", ");
                }
                first = false;
                expr(s, x, PR_TEST);
            }
            for k in keywords {
                if !first {
                    s.push_str(", ");
                }
                first = false;
                match &k.arg {
                    Some(n) => {
                        s.push_str(n);
                        s.push('=');
                    }
                    None => s.push_str("**"),
                }
                expr(s, &k.value, PR_TEST);
            }
            s.push(')');
        }
        ExprKind::FormattedValue { .. } | ExprKind::JoinedStr { .. } => {
            let mut body = String::new();
            match &e.node {
                ExprKind::JoinedStr { values } => fstring_body(&mut body, values),
                _ => fstring_body(&mut body, std::slice::from_ref(e)),
            }
            s.push('f');
            s.push_str(&str_repr(&body.chars().map(|c| c as u32).collect::<Vec<_>>()));
        }
        ExprKind::TemplateStr { values } => {
            let mut body = String::new();
            fstring_body(&mut body, values);
            s.push('t');
            s.push_str(&str_repr(&body.chars().map(|c| c as u32).collect::<Vec<_>>()));
        }
        ExprKind::Interpolation { .. } => {
            let mut body = String::new();
            fstring_body(&mut body, std::slice::from_ref(e));
            s.push('t');
            s.push_str(&str_repr(&body.chars().map(|c| c as u32).collect::<Vec<_>>()));
        }
        ExprKind::Constant { value, kind } => {
            if kind.as_deref() == Some("u") {
                s.push('u');
            }
            s.push_str(&const_repr_inf(value));
        }
        ExprKind::Attribute { value, attr, .. } => {
            expr(s, value, PR_ATOM);
            let is_int = matches!(&value.node, ExprKind::Constant { value: Constant::Int(_), .. });
            s.push_str(if is_int { " ." } else { "." });
            s.push_str(attr);
        }
        ExprKind::Subscript { value, slice, .. } => {
            expr(s, value, PR_ATOM);
            s.push('[');
            match &slice.node {
                ExprKind::Tuple { elts, .. } if !elts.is_empty() => {
                    join(s, elts.iter(), PR_TEST);
                    if elts.len() == 1 {
                        s.push(',');
                    }
                }
                _ => expr(s, slice, PR_TUPLE),
            }
            s.push(']');
        }
        ExprKind::Starred { value, .. } => {
            s.push('*');
            expr(s, value, PR_EXPR);
        }
        ExprKind::Name { id, .. } => s.push_str(id),
        ExprKind::List { elts, .. } => {
            s.push('[');
            join(s, elts.iter(), PR_TEST);
            s.push(']');
        }
        ExprKind::Tuple { elts, .. } => {
            if elts.is_empty() {
                s.push_str("()");
                return;
            }
            if level > PR_TUPLE {
                s.push('(');
            }
            join(s, elts.iter(), PR_TEST);
            if elts.len() == 1 {
                s.push(',');
            }
            if level > PR_TUPLE {
                s.push(')');
            }
        }
        ExprKind::Slice { lower, upper, step } => {
            if let Some(l) = lower {
                expr(s, l, PR_TEST);
            }
            s.push(':');
            if let Some(u) = upper {
                expr(s, u, PR_TEST);
            }
            if let Some(st) = step {
                s.push(':');
                expr(s, st, PR_TEST);
            }
        }
    }
}

const PR_EXPR: u8 = PR_BOR;

/// repr() of a str value.
pub fn str_repr(cps: &[u32]) -> String {
    let has_sq = cps.contains(&('\'' as u32));
    let has_dq = cps.contains(&('"' as u32));
    let q = if has_sq && !has_dq { '"' } else { '\'' };
    let mut s = String::new();
    s.push(q);
    for &c in cps {
        match c {
            0x5c => s.push_str("\\\\"),
            0x0a => s.push_str("\\n"),
            0x0d => s.push_str("\\r"),
            0x09 => s.push_str("\\t"),
            _ if c == q as u32 => {
                s.push('\\');
                s.push(q);
            }
            _ if c < 0x20 || c == 0x7f => s.push_str(&format!("\\x{:02x}", c)),
            0x80..=0xa0 | 0xad => s.push_str(&format!("\\x{:02x}", c)),
            0xd800..=0xdfff => s.push_str(&format!("\\u{:04x}", c)),
            _ => match char::from_u32(c) {
                Some(ch) => s.push(ch),
                None => s.push_str(&format!("\\U{:08x}", c)),
            },
        }
    }
    s.push(q);
    s
}

pub fn bytes_repr(b: &[u8]) -> String {
    let has_sq = b.contains(&b'\'');
    let has_dq = b.contains(&b'"');
    let q = if has_sq && !has_dq { b'"' } else { b'\'' };
    let mut s = String::from("b");
    s.push(q as char);
    for &c in b {
        match c {
            b'\\' => s.push_str("\\\\"),
            b'\n' => s.push_str("\\n"),
            b'\r' => s.push_str("\\r"),
            b'\t' => s.push_str("\\t"),
            _ if c == q => {
                s.push('\\');
                s.push(q as char);
            }
            0x20..=0x7e => s.push(c as char),
            _ => s.push_str(&format!("\\x{:02x}", c)),
        }
    }
    s.push(q as char);
    s
}

/// Python float repr (shortest round-trip, Python exponent rules).
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".to_string() } else { "-inf".to_string() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0".to_string() } else { "0.0".to_string() };
    }
    let e = format!("{:e}", f); // e.g. "-1.2345e-7"
    let (mant, exp) = e.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let neg = mant.starts_with('-');
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
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
        out.push(if exp < 0 { '-' } else { '+' });
        out.push_str(&format!("{:02}", exp.abs()));
    } else if exp >= 0 {
        let p = exp as usize + 1;
        if digits.len() <= p {
            out.push_str(&digits);
            for _ in digits.len()..p {
                out.push('0');
            }
            out.push_str(".0");
        } else {
            out.push_str(&digits[..p]);
            out.push('.');
            out.push_str(&digits[p..]);
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

/// repr() of a constant (as Python would print it).
pub fn const_repr(c: &Constant) -> String {
    match c {
        Constant::None => "None".into(),
        Constant::Bool(b) => if *b { "True" } else { "False" }.into(),
        Constant::Ellipsis => "Ellipsis".into(),
        Constant::Int(Int::Small(i)) => i.to_string(),
        Constant::Int(Int::Big(s)) => s.clone(),
        Constant::Float(f) => float_repr(*f),
        Constant::Complex { real, imag } => complex_repr(*real, *imag),
        Constant::Str(s) => str_repr(&s.code_points().collect::<Vec<_>>()),
        Constant::Bytes(b) => bytes_repr(b),
        Constant::Tuple(v) => {
            let mut s = String::from("(");
            for (i, x) in v.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&const_repr(x));
            }
            if v.len() == 1 {
                s.push(',');
            }
            s.push(')');
            s
        }
        Constant::FrozenSet(v) => {
            let mut s = String::from("frozenset({");
            for (i, x) in v.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&const_repr(x));
            }
            s.push_str("})");
            s
        }
    }
}

fn complex_repr(r: f64, i: f64) -> String {
    let fmt = |x: f64| -> String {
        let s = float_repr(x);
        s.strip_suffix(".0").map(|t| t.to_string()).unwrap_or(s)
    };
    if r == 0.0 && r.is_sign_positive() {
        format!("{}j", fmt(i))
    } else {
        let im = fmt(i);
        let sign = if im.starts_with('-') || i.is_nan() && im.starts_with('-') { "" } else { "+" };
        format!("({}{}{}j)", fmt(r), sign, im)
    }
}

/// Like const_repr but with CPython's unparse spelling of infinities (`1e309`).
fn const_repr_inf(c: &Constant) -> String {
    match c {
        Constant::Ellipsis => "...".into(),
        Constant::Float(f) if f.is_infinite() => if *f > 0.0 { "1e309" } else { "-1e309" }.into(),
        Constant::Complex { real, imag } if real.is_infinite() || imag.is_infinite() => {
            let f = |x: f64| -> String {
                if x.is_infinite() {
                    if x > 0.0 { "1e309".into() } else { "-1e309".into() }
                } else {
                    float_repr(x)
                }
            };
            if *real == 0.0 {
                format!("{}j", f(*imag))
            } else {
                format!("({} + {}j)", f(*real), f(*imag))
            }
        }
        Constant::Tuple(v) => {
            let mut s = String::from("(");
            for (i, x) in v.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&const_repr_inf(x));
            }
            if v.len() == 1 {
                s.push(',');
            }
            s.push(')');
            s
        }
        _ => const_repr(c),
    }
}
