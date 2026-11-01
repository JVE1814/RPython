//! Compare `to_tuple` dumps against every expected entry in Lib/test/test_ast/snippets.py.

use rpy_parser::ast::{Constant, ExprKind, Mod, StmtKind};
use rpy_parser::{dump, parse, Mode, ParseOptions};

fn spec_path(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/Lib/test").join(rel)
}

/// Extract `name = [ ... ]` list of string constants using our own parser.
fn string_list(m: &Mod, name: &str) -> Vec<String> {
    let Mod::Module { body, .. } = m else { panic!() };
    for s in body {
        if let StmtKind::Assign { targets, value, .. } = &s.node {
            if let ExprKind::Name { id, .. } = &targets[0].node {
                if id == name {
                    if let ExprKind::List { elts, .. } = &value.node {
                        return elts
                            .iter()
                            .map(|e| match &e.node {
                                ExprKind::Constant { value: Constant::Str(s), .. } => s.as_str().unwrap().to_string(),
                                _ => panic!("non-string entry"),
                            })
                            .collect();
                    }
                }
            }
        }
    }
    panic!("list {} not found", name)
}

/// Expected result lines: the text lines between `name = [` and `]`.
fn result_lines(src: &str, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in src.lines() {
        if line == format!("{} = [", name) {
            inside = true;
            continue;
        }
        if inside {
            if line == "]" {
                break;
            }
            out.push(line.trim_end_matches(',').to_string());
        }
    }
    out
}

#[test]
fn snippets() {
    let src = std::fs::read_to_string(spec_path("test_ast/snippets.py")).unwrap();
    let m = parse(&src, &ParseOptions::default()).result.expect("snippets.py must parse");
    let mut total = 0;
    let mut failed = 0;
    for (kind, mode) in [("exec", Mode::Exec), ("single", Mode::Single), ("eval", Mode::Eval)] {
        let tests = string_list(&m, &format!("{}_tests", kind));
        let results = result_lines(&src, &format!("{}_results", kind));
        assert_eq!(tests.len(), results.len(), "{} count mismatch", kind);
        for (t, expected) in tests.iter().zip(results.iter()) {
            total += 1;
            let got = match parse(t, &ParseOptions::new(mode)).result {
                Ok(m) => dump::to_tuple(&m),
                Err(e) => format!("ERROR {:?}", e),
            };
            if &got != expected {
                failed += 1;
                eprintln!("MISMATCH ({}) {:?}\n  expected: {}\n  got:      {}", kind, t, expected, got);
            }
        }
    }
    eprintln!("snippets: {}/{} passed", total - failed, total);
    assert_eq!(failed, 0, "{} of {} snippets mismatched", failed, total);
}
