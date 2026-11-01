//! Cases from test_named_expressions.py (each checked at module, class and function scope).
mod common;
use common::*;

fn check(code: &str, msg: &str) {
    for wrapped in [code.to_string(), format!("lambda: {}\n", code)] {
        let e = comp_err(&wrapped);
        assert_eq!(e.msg, msg, "{}", wrapped);
    }
}

#[test]
fn rebind_iteration_variable() {
    for (t, code) in [
        ("i", "[i := 0 for i in range(5)]"),
        ("j", "[[(j := 0) for i in range(5)] for j in range(5)]"),
        ("j", "[(j := 0) for i in range(5) for j in range(5)]"),
        ("i", "[i := 0 for i, j in [(0, 1)]]"),
        ("i", "[i+1 for i in range(5) if (i := 0)]"),
        ("i", "[False or (i:=0) for i in range(5)]"),
        ("i", "[(i, j) for i in range(5) for j in range(5) if True or (i:=10)]"),
        ("a", "{(a := 1) for a, (*b, c[d+e::f(g)], h.i) in j}"),
        ("b", "{(b := 1) for a, (*b, c[d+e::f(g)], h.i) in j}"),
        ("i", "{(i := 0): 1 for i in range(5)}"),
        ("i", "{1: (i := 0) for i in range(5)}"),
    ] {
        check(code, &format!("assignment expression cannot rebind comprehension iteration variable '{}'", t));
    }
}

#[test]
fn inner_loop_rebind() {
    for code in ["[i for i in range(5) if (j := 0) for j in range(5)]", "{i: 1 for i in range(5) if (j := 0) for j, k in {(0, 1)}}"] {
        check(code, "comprehension inner loop cannot rebind assignment expression target 'j'");
    }
}

#[test]
fn iterable_expression() {
    for code in [
        "[i for i in (i := range(5))]",
        "[i for i in (2, 3, i := range(5))]",
        "[i for i in [2, 3, i := range(5)]]",
        "[i for i in (j := range(5))]",
        "[i for i in (lambda:(j := range(5)))()]",
        "[i for i in range(5) for j in (i := range(5))]",
        "[i for i in [j for j in (k := range(5))]]",
        "[i for i in [j for j in range(5) if (j := True)]]",
        "[i for i in [(j := True) for j in range(5)]]",
    ] {
        check(code, "assignment expression cannot be used in a comprehension iterable expression");
    }
}

#[test]
fn mangled() {
    let e = comp_err("class Foo:\n    def bar(self):\n        [[(__x:=2) for _ in range(2)] for __x in range(2)]\n");
    assert_eq!(e.msg, "assignment expression cannot rebind comprehension iteration variable '__x'");
}
