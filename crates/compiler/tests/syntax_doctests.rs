//! Checks the `SyntaxError` doctests of test_syntax.py & friends: for every example the parser
//! accepts, the compiler must raise exactly the expected SyntaxError message.

use rpy_compiler::{compile, CompileOptions, Mode};
use rpy_parser::{parse, Mode as PMode, ParseOptions};

fn cases(src: &str) -> Vec<(String, String, usize)> {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i].trim_start();
        if let Some(code) = l.strip_prefix(">>> ") {
            let start = i;
            let indent = lines[i].len() - l.len();
            let mut code_lines = vec![code.to_string()];
            i += 1;
            while i < lines.len() {
                let t = lines[i].trim_start();
                if let Some(c) = t.strip_prefix("...") {
                    let c = c.strip_prefix(' ').unwrap_or(c);
                    code_lines.push(c.to_string());
                    i += 1;
                } else {
                    break;
                }
            }
            // find "SyntaxError: msg" before the next >>>
            let mut j = i;
            let mut msg = None;
            while j < lines.len() && !lines[j].trim_start().starts_with(">>>") {
                let t = lines[j].trim_start();
                if lines[j].len() - t.len() < indent {
                    break;
                }
                if let Some(m) = t.strip_prefix("SyntaxError: ") {
                    msg = Some(m.to_string());
                    break;
                }
                j += 1;
            }
            if let Some(m) = msg {
                let code = code_lines.join("\n") + "\n";
                out.push((code.replace("\\\\", "\\"), m, start + 1));
            }
        } else {
            i += 1;
        }
    }
    out
}

#[test]
fn syntax_doctests() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/Lib/test");
    let mut checked = 0;
    let mut bad = Vec::new();
    for file in ["test_syntax.py", "test_unpack_ex.py", "test_genexps.py", "test_generators.py", "test_listcomps.py", "test_setcomps.py", "test_named_expressions.py"] {
        let src = std::fs::read_to_string(root.join(file)).unwrap();
        for (code, msg, line) in cases(&src) {
            if msg.contains("...") || msg.is_empty() || code.contains("compile(") || code.contains("ast.parse(") {
                continue;
            }
            for mode in [PMode::Single, PMode::Exec] {
                let m = match parse(&code, &ParseOptions::new(mode)).result {
                    Ok(m) => m,
                    Err(_) => break,
                };
                checked += 1;
                let cm = if mode == PMode::Single { Mode::Single } else { Mode::Exec };
                match compile(&m, &CompileOptions::new("<doctest>", cm)) {
                    Ok(_) => bad.push(format!("{}:{}: no error, expected {:?}\n{}", file, line, msg, code)),
                    Err(e) => {
                        if e.msg != msg {
                            bad.push(format!("{}:{}: got {:?}, expected {:?}\n{}", file, line, e.msg, msg, code));
                        }
                    }
                }
                break;
            }
        }
    }
    eprintln!("checked {} doctests compiled by the compiler, {} mismatches", checked, bad.len());
    for b in &bad {
        eprintln!("{}", b);
    }
    assert!(bad.is_empty());
}
