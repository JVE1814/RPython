//! Assorted checks taken from test_compile.py / test_grammar.py / test_ast.py.

mod common;

use rpy_parser::ast::*;
use rpy_parser::{dump, parse, Mode, ParseOptions};

fn ok(src: &str, mode: Mode) -> bool {
    parse(src, &ParseOptions { mode, hooks: common::hooks(), ..Default::default() }).result.is_ok()
}

fn eval_const(src: &str) -> String {
    let m = parse(src, &ParseOptions { mode: Mode::Eval, ..Default::default() }).result.unwrap();
    let Mod::Expression { body } = m else { panic!() };
    dump::expr_to_tuple(&body)
}

#[test]
fn literals_with_leading_zeroes() {
    for arg in [
        "077787", "0xj", "0x.", "0e", "090000000000000", "080000000000000", "000000000000009", "000000000000008", "0b42",
        "0BADCAFE", "0o123456789", "0b1.1", "0o4.2", "0b101j", "0o153j", "0b100e1", "0o777e1", "0777", "000777",
        "000000000000007",
    ] {
        assert!(!ok(arg, Mode::Eval), "{} should fail", arg);
    }
    for (src, want) in [
        ("0xff", "255"),
        ("0777.", "777.0"),
        ("0777e1", "7770.0"),
        ("0e0", "0.0"),
        ("0000e-012", "0.0"),
        ("09.5", "9.5"),
        ("0777j", "777j"),
        ("000", "0"),
        ("00j", "0j"),
        ("090000000000000.", "90000000000000.0"),
        ("090000000000000e-0", "90000000000000.0"),
        ("000000000000009.", "9.0"),
        ("0b101010", "42"),
        ("0o777", "511"),
        ("0xffffffffffffffffffff", "1208925819614629174706175"),
        ("1_000_000", "1000000"),
        ("1e1000", "inf"),
        ("2.5e-5", "2.5e-05"),
        ("1e16", "1e+16"),
        ("123456789012345678901234567890", "123456789012345678901234567890"),
    ] {
        let got = eval_const(src);
        assert!(got.ends_with(&format!(", {}, None)", want)), "{} -> {}", src, got);
    }
    for src in ["2e", "2.0e+", "1e-", "3-4e/21", "1+*3", "f(None=1)", "def f(a=1, b): pass"] {
        assert!(!ok(src, Mode::Exec), "{}", src);
    }
}

#[test]
fn none_keyword() {
    for stmt in [
        "None = 0",
        "None += 0",
        "__builtins__.None = 0",
        "def None(): pass",
        "class None: pass",
        "(a, None) = 0, 0",
        "for None in range(10): pass",
        "def f(None): pass",
        "import None",
        "import x as None",
        "from x import None",
        "from x import y as None",
    ] {
        let s = format!("{}\n", stmt);
        assert!(!ok(&s, Mode::Single), "{}", stmt);
        assert!(!ok(&s, Mode::Exec), "{}", stmt);
    }
}

#[test]
fn imports() {
    for s in [
        "import sys",
        "import os, sys",
        "import os as bar",
        "import os.path as bar",
        "from __future__ import nested_scopes, generators",
        "from __future__ import (nested_scopes,\ngenerators)",
        "from __future__ import (nested_scopes,\ngenerators,)",
        "from sys import stdin, stderr, stdout",
        "from sys import (stdin, stderr,\nstdout)",
        "from sys import (stdin, stderr,\nstdout,)",
        "from sys import (stdin\n, stderr, stdout)",
        "from sys import (stdin\n, stderr, stdout,)",
        "from sys import stdin as si, stdout as so, stderr as se",
        "from sys import (stdin as si, stdout as so, stderr as se)",
        "from sys import (stdin as si, stdout as so, stderr as se,)",
    ] {
        assert!(ok(s, Mode::Exec), "{}", s);
    }
    for s in [
        "import (os, sys)",
        "import (os), (sys)",
        "import ((os), (sys))",
        "import (sys",
        "import sys)",
        "import (os,)",
        "import os As bar",
        "import os.path a bar",
        "from sys import stdin As stdout",
        "from sys import stdin a stdout",
        "from (sys) import stdin",
        "from __future__ import (nested_scopes",
        "from __future__ import nested_scopes)",
        "from __future__ import nested_scopes,\ngenerators",
        "from sys import (stdin",
        "from sys import stdin)",
        "from sys import stdin, stdout,\nstderr",
        "from sys import stdin si",
        "from sys import stdin,",
        "from sys import (*)",
        "from sys import (stdin,, stdout, stderr)",
        "from sys import (stdin, stdout),",
    ] {
        assert!(!ok(s, Mode::Exec), "{}", s);
    }
}

#[test]
fn int_max_str_digits() {
    let n = 3000;
    let source = format!("a = 1\nb = 2\nc = {}\nd = 4", "3".repeat(n));
    let o = |d| ParseOptions { int_max_str_digits: d, ..Default::default() };
    assert!(parse(&source, &o(n)).result.is_ok());
    let e = parse(&source, &o(n - 1)).result.unwrap_err();
    assert_eq!(e.lineno, 3);
    assert!(e.msg.contains("Exceeds the limit ") && e.msg.contains(" Consider hexadecimal "), "{}", e.msg);
    assert!(parse(&source, &o(0)).result.is_ok());
}

#[test]
fn max_level_parens() {
    let ok200 = format!("{}{}", "(".repeat(200), ")".repeat(200));
    assert!(ok(&ok200, Mode::Eval));
    let bad = format!("{}{}", "(".repeat(201), ")".repeat(201));
    let e = parse(&bad, &ParseOptions::new(Mode::Eval)).result.unwrap_err();
    assert!(e.msg.starts_with("too many nested parentheses"), "{}", e.msg);
}

#[test]
fn recursion_limits() {
    // test_ast.test_ast_recursion_limit / test_compile.test_compiler_recursion_limit
    for (prefix, rep) in [("a", "()"), ("a", ".b"), ("a", "[0]"), ("a", "*a")] {
        let good = format!("{}{}", prefix, rep.repeat(200));
        assert!(ok(&good, Mode::Exec));
        let bad = format!("{}{}", prefix, rep.repeat(500_000));
        let e = parse(&bad, &ParseOptions::default()).result.unwrap_err();
        assert_eq!(e.kind, rpy_parser::ErrorKind::RecursionError);
    }
    // test_syntax.test_error_on_parser_stack_overflow
    let src = format!("{}4", "-".repeat(100_000));
    for mode in [Mode::Exec, Mode::Eval, Mode::Single] {
        let e = parse(&src, &ParseOptions::new(mode)).result.unwrap_err();
        assert_eq!(e.kind, rpy_parser::ErrorKind::MemoryError);
        assert!(e.msg.contains("too complex"));
    }
    // test_compile.test_stack_overflow
    assert!(ok(&"if a: b\n".repeat(200_000), Mode::Exec));
}

#[test]
fn node_classes() {
    let classes = rpy_parser::ast::node_classes();
    let names: Vec<&str> = classes.iter().map(|c| c.name).collect();
    for n in ["Module", "TemplateStr", "Interpolation", "TypeVar", "MatchAs", "comprehension", "arguments", "Load", "cmpop"] {
        assert!(names.contains(&n), "{}", n);
    }
    let imp = rpy_parser::ast::node_class("ImportFrom").unwrap();
    assert_eq!(imp.fields.iter().map(|f| f.name).collect::<Vec<_>>(), ["module", "names", "level", "is_lazy"]);
    let interp = rpy_parser::ast::node_class("Interpolation").unwrap();
    assert_eq!(interp.fields.iter().map(|f| f.name).collect::<Vec<_>>(), ["value", "str", "conversion", "format_spec"]);
}
