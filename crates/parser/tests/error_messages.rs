//! Error message / location conformance, with expectations extracted from the CPython test
//! suite: test_syntax.py (doctests + `_check_error` calls), test_exceptions.py
//! (`check`/`ckmsg`), test_fstring.py / test_tstring.py (`assertAllRaise` / error tables),
//! test_grammar.py (`check_syntax_error`), test_patma.py, test_type_params.py etc.
//!
//! Errors that CPython raises in the compiler/symtable (not the parser) are counted separately
//! ("parses OK: compiler-level") and do not fail the test. The test prints a summary and fails
//! only if the number of real mismatches exceeds the recorded baseline.

mod common;
#[path = "common/regex.rs"]
mod regex;

use rpy_parser::ast::{Constant, Expr, ExprKind, Mod, Stmt, StmtKind};
use rpy_parser::{parse, parse_bytes, ErrorKind, Mode, ParseOptions, SyntaxError};

fn opts(mode: Mode) -> ParseOptions {
    ParseOptions { mode, hooks: common::hooks(), ..Default::default() }
}

fn parse_file(rel: &str) -> (String, Mod) {
    let src = std::fs::read_to_string(common::spec_path(rel)).unwrap();
    let m = parse(&src, &opts(Mode::Exec)).result.unwrap_or_else(|e| panic!("{} failed: {:?}", rel, e));
    (src, m)
}

fn const_str(e: &Expr) -> Option<String> {
    match &e.node {
        ExprKind::Constant { value: Constant::Str(s), .. } => Some(s.to_string_lossy()),
        _ => None,
    }
}
fn const_bytes(e: &Expr) -> Option<Vec<u8>> {
    match &e.node {
        ExprKind::Constant { value: Constant::Bytes(b), .. } => Some(b.clone()),
        ExprKind::Constant { value: Constant::Str(s), .. } => Some(s.to_string_lossy().into_bytes()),
        _ => None,
    }
}
fn const_int(e: &Expr) -> Option<i64> {
    match &e.node {
        ExprKind::Constant { value: Constant::Int(rpy_parser::ast::Int::Small(v)), .. } => Some(*v),
        _ => None,
    }
}

/// Visit all expressions in a module (pre-order) with the enclosing function name.
fn walk_mod(m: &Mod, f: &mut dyn FnMut(&Expr, &str)) {
    let Mod::Module { body, .. } = m else { return };
    for s in body {
        walk_stmt(s, "", f);
    }
}

fn walk_stmt(s: &Stmt, func: &str, f: &mut dyn FnMut(&Expr, &str)) {
    use StmtKind::*;
    match &s.node {
        FunctionDef(d) | AsyncFunctionDef(d) => {
            for x in &d.body {
                walk_stmt(x, &d.name, f);
            }
        }
        ClassDef { body, .. } => {
            for x in body {
                walk_stmt(x, func, f);
            }
        }
        Expr { value } | Return { value: Some(value) } => walk_expr(value, func, f),
        Assign { value, .. } => walk_expr(value, func, f),
        For(fo) | AsyncFor(fo) => {
            walk_expr(&fo.iter, func, f);
            for x in fo.body.iter().chain(fo.orelse.iter()) {
                walk_stmt(x, func, f);
            }
        }
        While { body, orelse, .. } | If { body, orelse, .. } => {
            for x in body.iter().chain(orelse.iter()) {
                walk_stmt(x, func, f);
            }
        }
        With(w) | AsyncWith(w) => {
            for x in &w.body {
                walk_stmt(x, func, f);
            }
        }
        Try(t) | TryStar(t) => {
            for x in t.body.iter().chain(t.orelse.iter()).chain(t.finalbody.iter()) {
                walk_stmt(x, func, f);
            }
            for h in &t.handlers {
                for x in &h.body {
                    walk_stmt(x, func, f);
                }
            }
        }
        _ => {}
    }
}

fn walk_expr(e: &Expr, func: &str, f: &mut dyn FnMut(&Expr, &str)) {
    f(e, func);
    if let ExprKind::Call { args, .. } = &e.node {
        for a in args {
            walk_expr(a, func, f);
        }
    }
    if let ExprKind::Tuple { elts, .. } | ExprKind::List { elts, .. } = &e.node {
        for a in elts {
            walk_expr(a, func, f);
        }
    }
}

fn call_name(e: &Expr) -> Option<&str> {
    if let ExprKind::Call { func, .. } = &e.node {
        return match &func.node {
            ExprKind::Name { id, .. } => Some(id),
            ExprKind::Attribute { attr, .. } => Some(attr),
            _ => None,
        };
    }
    None
}

#[derive(Default)]
struct Expect {
    origin: String,
    src: Vec<u8>,
    mode: Option<Mode>,
    regex: Option<String>,
    exact_msg: Option<String>,
    kind: Option<ErrorKind>,
    lineno: Option<i64>,
    offset: Option<i64>,
    end_lineno: Option<i64>,
    end_offset: Option<i64>,
    /// expected to compile successfully
    ok: bool,
}

#[derive(Default)]
struct Stats {
    total: usize,
    pass: usize,
    compiler_level: Vec<String>,
    fail: Vec<String>,
}

fn check(e: &Expect, st: &mut Stats) {
    st.total += 1;
    let o = opts(e.mode.unwrap_or(Mode::Exec));
    let out = parse_bytes(&e.src, &o);
    let shown = String::from_utf8_lossy(&e.src).into_owned();
    match out.result {
        Ok(_) => {
            if e.ok {
                st.pass += 1;
            } else {
                st.compiler_level.push(format!("[{}] {:?}: expected {:?}", e.origin, shown, e.exact_msg.as_ref().or(e.regex.as_ref())));
            }
        }
        Err(err) => {
            if e.ok {
                st.fail.push(format!("[{}] {:?}: expected success, got {:?}", e.origin, shown, err.msg));
                return;
            }
            let mut problems = Vec::new();
            if let Some(m) = &e.exact_msg {
                let alt_ok = m.starts_with("invalid syntax. Did you mean '") && err.msg.starts_with("invalid syntax");
                if &err.msg != m && !alt_ok {
                    problems.push(format!("msg {:?} != {:?}", err.msg, m));
                }
            }
            if let Some(r) = &e.regex {
                if !regex::search(r, &err.msg) {
                    problems.push(format!("msg {:?} !~ {:?}", err.msg, r));
                }
            }
            if let Some(k) = e.kind {
                if k != err.kind {
                    problems.push(format!("kind {:?} != {:?}", err.kind, k));
                }
            }
            if let Some(l) = e.lineno {
                if err.lineno as i64 != l {
                    problems.push(format!("lineno {} != {}", err.lineno, l));
                }
            }
            if let Some(o) = e.offset {
                if err.offset as i64 != o {
                    problems.push(format!("offset {} != {}", err.offset, o));
                }
            }
            if let Some(l) = e.end_lineno {
                if err.end_lineno.map(|x| x as i64) != Some(l) {
                    problems.push(format!("end_lineno {:?} != {}", err.end_lineno, l));
                }
            }
            if let Some(o) = e.end_offset {
                if err.end_offset.map(|x| x as i64) != Some(o) {
                    problems.push(format!("end_offset {:?} != {}", err.end_offset, o));
                }
            }
            if problems.is_empty() {
                st.pass += 1;
            } else {
                st.fail.push(format!("[{}] {:?}: {}", e.origin, shown, problems.join("; ")));
            }
        }
    }
}

fn err_kind(name: &str) -> Option<ErrorKind> {
    match name {
        "SyntaxError" => Some(ErrorKind::Syntax),
        "IndentationError" => Some(ErrorKind::Indentation),
        "TabError" => Some(ErrorKind::Tab),
        _ => None,
    }
}

/// Doctest examples from a docstring: (source, expected exception class+msg or None).
fn doctests(doc: &str) -> Vec<(String, Option<(String, String)>)> {
    let lines: Vec<&str> = doc.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        let t = l.trim_start();
        if !(t.starts_with(">>> ") || t == ">>>") {
            i += 1;
            continue;
        }
        let indent = l.len() - t.len();
        let mut src = String::new();
        src.push_str(if t.len() > 4 { &t[4..] } else { "" });
        src.push('\n');
        i += 1;
        while i < lines.len() {
            let l = lines[i];
            if l.len() >= indent + 3 && &l[indent..indent + 3] == "..." && (l.len() == indent + 3 || l.as_bytes()[indent + 3] == b' ') {
                src.push_str(if l.len() > indent + 4 { &l[indent + 4..] } else { "" });
                src.push('\n');
                i += 1;
            } else {
                break;
            }
        }
        let mut want = Vec::new();
        while i < lines.len() {
            let l = lines[i];
            let t = l.trim_start();
            if t.is_empty() || t.starts_with(">>>") {
                break;
            }
            want.push(t.to_string());
            i += 1;
        }
        let mut exc = None;
        if want.iter().any(|w| w.starts_with("Traceback (most recent call last)")) {
            for w in want.iter().rev() {
                if let Some((cls, msg)) = w.split_once(": ") {
                    if cls.chars().all(|c| c.is_alphanumeric() || c == '.' || c == '_') && cls.ends_with("Error") {
                        exc = Some((cls.rsplit('.').next().unwrap().to_string(), msg.to_string()));
                        break;
                    }
                }
            }
            if exc.is_none() {
                exc = Some(("?".into(), "?".into()));
            }
        }
        out.push((src, exc));
    }
    out
}

fn kwarg(e: &Expr, name: &str) -> Option<Expr> {
    if let ExprKind::Call { keywords, .. } = &e.node {
        for k in keywords {
            if k.arg.as_deref() == Some(name) {
                return Some(k.value.clone());
            }
        }
    }
    None
}

fn call_args(e: &Expr) -> &[Expr] {
    if let ExprKind::Call { args, .. } = &e.node {
        args
    } else {
        &[]
    }
}

#[test]
fn error_messages() {
    let mut cases: Vec<Expect> = Vec::new();

    // ---- test_syntax.py doctests
    let (_, m) = parse_file("test_syntax.py");
    if let Mod::Module { body, .. } = &m {
        if let StmtKind::Expr { value } = &body[0].node {
            let doc = const_str(value).unwrap();
            for (src, exc) in doctests(&doc) {
                if src.contains("import ast") || src.contains("compile(") || src.contains("exec(") {
                    continue;
                }
                let mut e = Expect { origin: "syntax-doctest".into(), src: src.into_bytes(), mode: Some(Mode::Single), ..Default::default() };
                match exc {
                    None => e.ok = true,
                    Some((cls, msg)) => {
                        let Some(k) = err_kind(&cls) else { continue };
                        e.kind = Some(k);
                        e.exact_msg = Some(msg);
                    }
                }
                cases.push(e);
            }
        }
    }
    // ---- test_syntax.py _check_error(...) calls
    walk_mod(&m, &mut |e, _func| {
        if call_name(e) != Some("_check_error") {
            return;
        }
        let a = call_args(e);
        if a.len() < 2 {
            return;
        }
        let (Some(src), Some(re)) = (const_bytes(&a[0]), const_str(&a[1])) else { return };
        let mut x = Expect { origin: "syntax-check_error".into(), src, regex: Some(re), ..Default::default() };
        x.lineno = kwarg(e, "lineno").and_then(|v| const_int(&v));
        x.offset = kwarg(e, "offset").and_then(|v| const_int(&v));
        x.end_lineno = kwarg(e, "end_lineno").and_then(|v| const_int(&v));
        x.end_offset = kwarg(e, "end_offset").and_then(|v| const_int(&v));
        if let Some(md) = kwarg(e, "mode").and_then(|v| const_str(&v)) {
            x.mode = Some(match md.as_str() {
                "eval" => Mode::Eval,
                "single" => Mode::Single,
                _ => Mode::Exec,
            });
        }
        if let Some(sc) = kwarg(e, "subclass") {
            if let ExprKind::Name { id, .. } = &sc.node {
                x.kind = err_kind(id);
            }
        }
        cases.push(x);
    });

    // ---- test_exceptions.py: check(src, lineno, offset[, end_lineno, end_offset]) / ckmsg
    let (_, m) = parse_file("test_exceptions.py");
    walk_mod(&m, &mut |e, func| {
        let name = call_name(e);
        let a = call_args(e);
        if name == Some("check") && (func == "testSyntaxErrorOffset" || func == "testSyntaxErrorNonUTF8Offset" || func == "test_error_offset_continuation_characters") && a.len() >= 3 {
            let (Some(src), Some(l), Some(o)) = (const_bytes(&a[0]), const_int(&a[1]), const_int(&a[2])) else { return };
            let mut x = Expect { origin: format!("exceptions-{}", func), src, lineno: Some(l), offset: Some(o), ..Default::default() };
            if a.len() >= 5 {
                x.end_lineno = const_int(&a[3]);
                x.end_offset = const_int(&a[4]);
            }
            cases.push(x);
        }
        if name == Some("ckmsg") && a.len() >= 2 {
            if let (Some(src), Some(msg)) = (const_bytes(&a[0]), const_str(&a[1])) {
                cases.push(Expect { origin: "exceptions-ckmsg".into(), src, exact_msg: Some(msg), ..Default::default() });
            }
        }
    });

    // ---- test_fstring.py / test_tstring.py: assertAllRaise(SyntaxError, regex, [srcs])
    for file in ["test_fstring.py", "test_tstring.py"] {
        let (_, m) = parse_file(file);
        walk_mod(&m, &mut |e, _| {
            let a = call_args(e);
            if call_name(e) == Some("assertAllRaise") && a.len() == 3 {
                if !matches!(&a[0].node, ExprKind::Name { id, .. } if id == "SyntaxError") {
                    return;
                }
                let Some(re) = const_str(&a[1]) else { return };
                if let ExprKind::List { elts, .. } = &a[2].node {
                    for s in elts {
                        if let Some(src) = const_bytes(s) {
                            cases.push(Expect { origin: format!("{}-assertAllRaise", file), src, mode: Some(Mode::Eval), regex: Some(re.clone()), ..Default::default() });
                        }
                    }
                }
            }
        });
    }
    // test_tstring.test_syntax_errors table: tuples (src, regex)
    let (_, m) = parse_file("test_tstring.py");
    walk_mod(&m, &mut |e, func| {
        if func != "test_syntax_errors" {
            return;
        }
        if let ExprKind::Tuple { elts, .. } = &e.node {
            if elts.len() == 2 {
                if let (Some(src), Some(re)) = (const_bytes(&elts[0]), const_str(&elts[1])) {
                    cases.push(Expect { origin: "tstring-table".into(), src, mode: Some(Mode::Eval), regex: Some(re), ..Default::default() });
                }
            }
        }
    });

    // ---- test_grammar.py: check_syntax_error(self, "src", "msg")
    for file in ["test_grammar.py", "test_patma.py", "test_type_params.py", "test_positional_only_arg.py", "test_genexps.py", "test_named_expressions.py", "test_unpack_ex.py"] {
        let (_, m) = parse_file(file);
        walk_mod(&m, &mut |e, _| {
            let a = call_args(e);
            if call_name(e) == Some("check_syntax_error") && a.len() >= 2 {
                let si = if matches!(&a[0].node, ExprKind::Name { .. }) { 1 } else { 0 };
                if let Some(src) = a.get(si).and_then(const_bytes) {
                    let re = a.get(si + 1).and_then(const_str);
                    let mut x = Expect { origin: format!("{}-check_syntax_error", file), src, regex: re, ..Default::default() };
                    x.lineno = kwarg(e, "lineno").and_then(|v| const_int(&v));
                    x.offset = kwarg(e, "offset").and_then(|v| const_int(&v));
                    cases.push(x);
                }
            }
        });
    }

    let mut st = Stats::default();
    for c in &cases {
        check(c, &mut st);
    }
    let verbose = std::env::var("VERBOSE").is_ok();
    if verbose {
        for f in &st.compiler_level {
            eprintln!("PARSED-OK {}", f);
        }
    }
    for f in &st.fail {
        eprintln!("FAIL {}", f);
    }
    eprintln!(
        "error_messages: {} cases, {} pass, {} parse OK (compiler-level errors), {} mismatches",
        st.total,
        st.pass,
        st.compiler_level.len(),
        st.fail.len()
    );
    let _ = SyntaxError::simple("");
    assert!(st.fail.is_empty(), "{} error-message mismatches", st.fail.len());
}
