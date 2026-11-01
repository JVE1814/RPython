//! Incomplete-input detection as used by codeop: replays the av/ai calls of test_codeop.py,
//! emulating `codeop._maybe_compile` on top of `allow_incomplete_input`/`dont_imply_dedent`.

mod common;

use rpy_parser::ast::{Constant, ExprKind, Mod, Stmt, StmtKind};
use rpy_parser::{parse, ErrorKind, Mode, ParseOptions};

fn opts(mode: Mode, incomplete: bool) -> ParseOptions {
    ParseOptions { mode, allow_incomplete_input: incomplete, dont_imply_dedent: incomplete, hooks: common::hooks(), ..Default::default() }
}

/// codeop.compile_command: Ok(true) = compiled, Ok(false) = incomplete (None), Err = SyntaxError
fn compile_command(source: &str, mode: Mode) -> Result<bool, String> {
    let mut src = source.to_string();
    let only_blank = source.split('\n').all(|l| {
        let l = l.trim();
        l.is_empty() || l.starts_with('#')
    });
    if only_blank && mode != Mode::Eval {
        src = "pass".into();
    }
    match parse(&src, &opts(mode, true)).result {
        Ok(_) => return Ok(true),
        Err(_) => match parse(&format!("{}\n", src), &opts(mode, true)).result {
            Ok(_) => return Ok(false),
            Err(e) if e.kind == ErrorKind::IncompleteInput => return Ok(false),
            Err(_) => {}
        },
    }
    match parse(&src, &opts(mode, false)).result {
        Ok(_) => Ok(true),
        Err(e) => Err(e.msg),
    }
}

fn collect(stmts: &[Stmt], func: &str, out: &mut Vec<(String, String, String, Mode)>) {
    for s in stmts {
        match &s.node {
            StmtKind::FunctionDef(f) => collect(&f.body, &f.name, out),
            StmtKind::ClassDef { body, .. } => collect(body, func, out),
            StmtKind::Expr { value } => {
                if let ExprKind::Call { func: f, args, keywords } = &value.node {
                    if let ExprKind::Name { id, .. } = &f.node {
                        if (id == "av" || id == "ai") && !args.is_empty() {
                            if let ExprKind::Constant { value: Constant::Str(src), .. } = &args[0].node {
                                let mut mode = Mode::Single;
                                let sym = args.get(1).map(|a| &a.node).or(keywords.iter().find(|k| k.arg.as_deref() == Some("symbol")).map(|k| &k.value.node));
                                if let Some(ExprKind::Constant { value: Constant::Str(m), .. }) = sym {
                                    mode = match m.as_str().unwrap() {
                                        "eval" => Mode::Eval,
                                        "exec" => Mode::Exec,
                                        _ => Mode::Single,
                                    };
                                }
                                out.push((func.to_string(), id.clone(), src.to_string_lossy(), mode));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

#[test]
fn codeop() {
    let text = std::fs::read_to_string(common::spec_path("test_codeop.py")).unwrap();
    let m = parse(&text, &ParseOptions::default()).result.unwrap();
    let Mod::Module { body, .. } = m else { panic!() };
    let mut cases = Vec::new();
    collect(&body, "", &mut cases);
    let mut fails = 0;
    for (func, _kind, src, mode) in &cases {
        let r = compile_command(src, *mode);
        let ok = match func.as_str() {
            "test_valid" => {
                // must compile both through codeop and with plain compile(..., DONT_IMPLY_DEDENT)
                let plain = parse(src, &ParseOptions { mode: *mode, dont_imply_dedent: true, hooks: common::hooks(), ..Default::default() }).result;
                r == Ok(true) && plain.is_ok()
            }
            "test_incomplete" => r == Ok(false),
            "test_invalid" | "test_invalid_exec" => r.is_err() || src.starts_with("return"), // return outside function: compiler error
            _ => true,
        };
        if !ok {
            fails += 1;
            eprintln!("FAIL {} {:?} ({:?}): {:?}", func, src, mode, r);
        }
    }
    eprintln!("codeop: {}/{} cases pass", cases.len() - fails, cases.len());
    assert_eq!(fails, 0);
}
