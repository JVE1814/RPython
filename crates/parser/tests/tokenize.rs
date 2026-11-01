//! Tokenizer conformance: replay every `self.check_tokenize(src, expected)` call with constant
//! arguments in test_tokenize.py (TokenizeTest: `tokenize` module stream with extra tokens;
//! CTokenizeTest: raw C-tokenizer stream) and compare the formatted token tables.

mod common;

use rpy_parser::ast::{Constant, Expr, ExprKind, Mod, Stmt, StmtKind};
use rpy_parser::dump::repr_str;
use rpy_parser::token::TokenType;
use rpy_parser::tokenizer::TokenIter;
use rpy_parser::{parse, ParseOptions};

fn const_str(e: &Expr) -> Option<String> {
    match &e.node {
        ExprKind::Constant { value: Constant::Str(s), .. } => Some(s.to_string_lossy()),
        _ => None,
    }
}

fn collect_calls(stmts: &[Stmt], out: &mut Vec<(String, String, String)>, method: &str) {
    for s in stmts {
        match &s.node {
            StmtKind::FunctionDef(f) => collect_calls(&f.body, out, &f.name),
            StmtKind::Expr { value } => {
                if let ExprKind::Call { func, args, .. } = &value.node {
                    if let ExprKind::Attribute { attr, .. } = &func.node {
                        if attr == "check_tokenize" && args.len() == 2 {
                            if let (Some(a), Some(b)) = (const_str(&args[0]), const_str(&args[1])) {
                                out.push((method.to_string(), a, b));
                            }
                        }
                    }
                }
            }
            StmtKind::For(f) => collect_calls(&f.body, out, method),
            StmtKind::With(w) => collect_calls(&w.body, out, method),
            StmtKind::If { body, orelse, .. } => {
                collect_calls(body, out, method);
                collect_calls(orelse, out, method);
            }
            _ => {}
        }
    }
}

fn format_tokens(src: &str, extra: bool) -> Result<Vec<String>, String> {
    let num_lines = src.lines().count() as u32;
    let missing_trailing_nl = !src.ends_with('\n') && !src.ends_with('\r');
    let mut out = Vec::new();
    for t in TokenIter::new(src, extra, common::hooks()) {
        let t = t.map_err(|e| format!("{:?}", e))?;
        if t.exact_type == TokenType::ENDMARKER {
            break;
        }
        if missing_trailing_nl && t.ty == TokenType::NEWLINE && t.end.0 == num_lines {
            continue;
        }
        let name = t.ty.name();
        let r = repr_str(&rpy_parser::ast::PyStr::Utf8(t.string.clone()));
        out.push(format!("    {:10} {:13} ({}, {}) ({}, {})", name, r, t.start.0, t.start.1, t.end.0, t.end.1));
    }
    Ok(out)
}

#[test]
fn tokenize_conformance() {
    let src = std::fs::read_to_string(common::spec_path("test_tokenize.py")).unwrap();
    let m = parse(&src, &ParseOptions { hooks: common::hooks(), ..Default::default() }).result.unwrap();
    let Mod::Module { body, .. } = m else { panic!() };
    let mut total = 0;
    let mut failed = 0;
    for s in &body {
        if let StmtKind::ClassDef { name, body, .. } = &s.node {
            let extra = match name.as_str() {
                "TokenizeTest" => true,
                "CTokenizeTest" => false,
                _ => continue,
            };
            let mut calls = Vec::new();
            collect_calls(body, &mut calls, "");
            for (method, source, expected) in calls {
                total += 1;
                let exp: Vec<String> = expected.trim_end().lines().map(|l| l.to_string()).collect();
                // the test strips leading spaces differently: compare trimmed lines
                let exp_t: Vec<String> = exp.iter().map(|l| l.trim().to_string()).collect();
                match format_tokens(&source, extra) {
                    Ok(got) => {
                        let got_t: Vec<String> = got.iter().map(|l| l.trim().to_string()).collect();
                        if got_t != exp_t {
                            failed += 1;
                            eprintln!("MISMATCH {}::{} {:?}", name, method, source);
                            for (i, (g, e)) in got_t.iter().zip(exp_t.iter()).enumerate() {
                                if g != e {
                                    eprintln!("  first diff at {}: got {:?}\n                 exp {:?}", i, g, e);
                                    break;
                                }
                            }
                            if got_t.len() != exp_t.len() {
                                eprintln!("  len got {} exp {}", got_t.len(), exp_t.len());
                            }
                        }
                    }
                    Err(e) => {
                        failed += 1;
                        eprintln!("ERROR {}::{} {:?}: {}", name, method, source, e);
                    }
                }
            }
        }
    }
    eprintln!("tokenize: {}/{} check_tokenize cases pass", total - failed, total);
    assert_eq!(failed, 0);
}
