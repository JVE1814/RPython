//! Mirrors the assertions of spec/Lib/test/test_symtable.py on the Rust data structure.
mod common;
use common::parse_src;
use rpy_compiler::symtable::*;
use rpy_compiler::{CompileOptions, Mode};

const TEST_CODE: &str = r#"
import sys

glob = 42
some_var = 12
some_non_assigned_global_var: int
some_assigned_global_var = 11

class Mine:
    instance_var = 24
    def a_method(p1, p2):
        pass

def spam(a, b, *var, **kw):
    global bar
    global some_assigned_global_var
    some_assigned_global_var = 12
    bar = 47
    some_var = 10
    x = 23
    glob
    def internal():
        return x
    def other_internal():
        nonlocal some_var
        some_var = 3
        return some_var
    return internal

def foo():
    pass

def namespace_test(): pass
def namespace_test(): pass

type Alias = int
type GenericAlias[T] = list[T]

def generic_spam[T](a):
    pass

class GenericMine[T: int, U: (int, str) = int]:
    pass
"#;

fn st(src: &str) -> SymbolTable {
    rpy_compiler::symtable(&parse_src(src, Mode::Exec), &CompileOptions::new("?", Mode::Exec)).unwrap()
}

fn find(t: &SymbolTable, sid: ScopeId, name: &str) -> ScopeId {
    *t.scopes[sid].children.iter().find(|&&c| t.scopes[c].name == name).unwrap_or_else(|| panic!("no child {name}"))
}

fn names_where(t: &SymbolTable, sid: ScopeId, f: impl Fn(&Symbol) -> bool) -> Vec<String> {
    let mut v: Vec<String> = t.scopes[sid].symbols.iter().filter(|s| f(s)).map(|s| s.name.clone()).collect();
    v.sort();
    v
}

fn flag_names(s: &Symbol) -> String {
    let names = [
        (DEF_GLOBAL, "DEF_GLOBAL"),
        (DEF_LOCAL, "DEF_LOCAL"),
        (DEF_PARAM, "DEF_PARAM"),
        (DEF_NONLOCAL, "DEF_NONLOCAL"),
        (USE, "USE"),
        (DEF_FREE, "DEF_FREE"),
        (DEF_FREE_CLASS, "DEF_FREE_CLASS"),
        (DEF_IMPORT, "DEF_IMPORT"),
        (DEF_ANNOT, "DEF_ANNOT"),
        (DEF_COMP_ITER, "DEF_COMP_ITER"),
        (DEF_TYPE_PARAM, "DEF_TYPE_PARAM"),
        (DEF_COMP_CELL, "DEF_COMP_CELL"),
    ];
    let scope = match s.scope {
        SymScope::Local => "LOCAL",
        SymScope::GlobalExplicit => "GLOBAL_EXPLICIT",
        SymScope::GlobalImplicit => "GLOBAL_IMPLICIT",
        SymScope::Free => "FREE",
        SymScope::Cell => "CELL",
        SymScope::Unknown => "UNKNOWN",
    };
    let fl: Vec<&str> = names.iter().filter(|(b, _)| s.flags & b != 0).map(|(_, n)| *n).collect();
    format!("{}, {}", scope, fl.join("|"))
}

#[test]
fn test_symtable_mirror() {
    let t = st(TEST_CODE);
    let top = 0;
    let mine = find(&t, top, "Mine");
    let a_method = find(&t, mine, "a_method");
    let spam = find(&t, top, "spam");
    let internal = find(&t, spam, "internal");
    let other_internal = find(&t, spam, "other_internal");
    let foo = find(&t, top, "foo");
    let alias = find(&t, top, "Alias");
    let galias = find(&t, top, "GenericAlias");
    let galias_inner = find(&t, galias, "GenericAlias");
    let gspam = find(&t, top, "generic_spam");
    let gspam_inner = find(&t, gspam, "generic_spam");
    let gmine = find(&t, top, "GenericMine");
    let gmine_inner = find(&t, gmine, "GenericMine");
    let tv = find(&t, gmine, "T");
    let uv = find(&t, gmine, "U");
    let ty = |s: ScopeId| t.scopes[s].kind.type_name();
    assert_eq!(ty(top), "module");
    assert_eq!(ty(mine), "class");
    assert_eq!(ty(a_method), "function");
    assert_eq!(ty(spam), "function");
    assert_eq!(ty(internal), "function");
    assert_eq!(ty(foo), "function");
    assert_eq!(ty(alias), "type alias");
    assert_eq!(ty(galias), "type parameters");
    assert_eq!(ty(galias_inner), "type alias");
    assert_eq!(ty(gspam), "type parameters");
    assert_eq!(ty(gspam_inner), "function");
    assert_eq!(ty(gmine), "type parameters");
    assert_eq!(ty(gmine_inner), "class");
    assert_eq!(ty(tv), "type variable");
    assert_eq!(ty(uv), "type variable");
    assert!(!t.scopes[top].is_optimized());
    assert!(t.scopes[spam].is_optimized());
    assert!(!t.scopes[top].nested && !t.scopes[mine].nested && !t.scopes[spam].nested);
    assert!(t.scopes[internal].nested);
    assert!(t.scopes[foo].children.is_empty());
    assert_eq!(t.scopes[top].lineno, 0);
    assert_eq!(t.scopes[spam].lineno, 14);
    let mut params = t.scopes[spam].varnames.clone();
    params.sort();
    assert_eq!(params, ["a", "b", "kw", "var"]);
    let locals = names_where(&t, spam, |s| matches!(s.scope, SymScope::Local | SymScope::Cell));
    assert_eq!(locals, ["a", "b", "internal", "kw", "other_internal", "some_var", "var", "x"]);
    let globals = names_where(&t, spam, |s| matches!(s.scope, SymScope::GlobalExplicit | SymScope::GlobalImplicit));
    assert_eq!(globals, ["bar", "glob", "some_assigned_global_var"]);
    assert_eq!(names_where(&t, internal, |s| s.scope == SymScope::Free), ["x"]);
    assert_eq!(names_where(&t, spam, |s| s.scope == SymScope::Cell), ["some_var", "x"]);
    let sym = |s: ScopeId, n: &str| t.scopes[s].lookup(n).unwrap_or_else(|| panic!("no symbol {n}")).clone();
    assert_eq!(flag_names(&sym(spam, "glob")), "GLOBAL_IMPLICIT, USE");
    assert_eq!(flag_names(&sym(spam, "bar")), "GLOBAL_EXPLICIT, DEF_GLOBAL|DEF_LOCAL");
    assert_eq!(flag_names(&sym(spam, "a")), "LOCAL, DEF_PARAM");
    assert_eq!(flag_names(&sym(spam, "internal")), "LOCAL, DEF_LOCAL|USE");
    assert_eq!(flag_names(&sym(spam, "other_internal")), "LOCAL, DEF_LOCAL");
    assert_eq!(flag_names(&sym(internal, "x")), "FREE, USE");
    assert_eq!(flag_names(&sym(other_internal, "some_var")), "FREE, DEF_LOCAL|DEF_NONLOCAL|USE");
    assert_eq!(flag_names(&sym(gmine, "T")), "LOCAL, DEF_LOCAL|DEF_TYPE_PARAM");
    // module-scope globals
    assert!(sym(top, "some_assigned_global_var").flags & DEF_GLOBAL != 0);
    assert!(sym(top, "sys").flags & DEF_IMPORT != 0);

    let t1 = st("[x for x in [1]]");
    assert_eq!(flag_names(t1.scopes[0].lookup("x").unwrap()), "LOCAL, DEF_LOCAL|USE|DEF_COMP_ITER");
    let t2 = st("[(lambda: x) for x in [1]]");
    assert_eq!(flag_names(t2.scopes[0].lookup("x").unwrap()), "CELL, DEF_LOCAL|DEF_COMP_ITER|DEF_COMP_CELL");
    let t3 = st("def f():\n   x = 1\n   class A:\n       x = 2\n       def method():\n           return x\n");
    let f = t3.scopes[0].children[1];
    assert_eq!(t3.scopes[f].name, "f");
    let a = t3.scopes[f].children[0];
    assert_eq!(flag_names(t3.scopes[a].lookup("x").unwrap()), "LOCAL, DEF_LOCAL|DEF_FREE_CLASS");

    // lambda / genexpr children
    let tl = st("lambda x: lambda y=x: y");
    assert_eq!(tl.scopes[0].children.len(), 1);
    let l1 = tl.scopes[0].children[0];
    assert_eq!(tl.scopes[l1].name, "<lambda>");
    assert!(!tl.scopes[l1].nested);
    assert_eq!(tl.scopes[l1].children.len(), 1);
    assert!(tl.scopes[tl.scopes[l1].children[0]].nested);
    let tg = st("((y for y in x) for x in a)");
    let g1 = tg.scopes[0].children[0];
    assert_eq!(tg.scopes[g1].name, "<genexpr>");
    let mut ids: Vec<&str> = tg.scopes[g1].symbols.iter().map(|s| s.name.as_str()).collect();
    ids.sort();
    assert_eq!(ids, [".0", "x"]);

    // comprehension children are spliced
    let tc = st("def f(xs, k):\n    local = 1\n    return [x + k + local for x in xs]\n");
    let f = find(&tc, 0, "f");
    assert!(tc.scopes[f].children.is_empty());
    assert!(tc.scopes[f].lookup("k").unwrap().flags & USE != 0);

    // annotation children
    let ta = st("def f():\n    x: int\n");
    assert_eq!(ta.scopes[ta.scopes[0].children[1]].kind.type_name(), "function");
    let tf = ta.scopes[0].children[1];
    let x = ta.scopes[tf].lookup("x").unwrap();
    assert_eq!(x.scope, SymScope::Local);
    assert!(x.flags & DEF_ANNOT != 0);
    let t5 = st("global x\nx: int");
    assert!(t5.scopes[0].lookup("x").unwrap().scope == SymScope::GlobalExplicit);

    // errors
    let e = rpy_compiler::symtable(&parse_src("def f(x): global x", Mode::Exec), &CompileOptions::new("spam", Mode::Exec)).unwrap_err();
    assert_eq!(e.msg, "name 'x' is parameter and global");
    assert_eq!((e.lineno, e.col_offset + 1), (1, 11));
}

#[test]
fn comprehension_references() {
    let t = st("def outer(k):\n    def f(xs):\n        nonlocal k\n        return [x + k for x in xs]\n");
    let f = find(&t, find(&t, 0, "outer"), "f");
    let k = t.scopes[f].lookup("k").unwrap();
    assert!(k.flags & USE != 0 && k.flags & DEF_NONLOCAL != 0);
    for comp in ["[k for k in xs]", "[[k for _ in ()] for k in xs]", "[(lambda: k) for x in xs]", "(x + k for x in xs)"] {
        let t = st(&format!("def f(xs, k):\n    return {}\n", comp));
        let f = find(&t, 0, "f");
        assert!(t.scopes[f].lookup("k").unwrap().flags & USE == 0, "{}", comp);
    }
    let t = st("def outer(k):\n    class C:\n        k = 1\n        values = [k for _ in ()]\n");
    let c = find(&t, find(&t, 0, "outer"), "C");
    assert!(t.scopes[c].lookup("k").unwrap().flags & USE == 0);
    for comp in ["[x for x in [1]]", "{x for x in [1]}", "{x:x*x for x in [1]}"] {
        let t = st(comp);
        let n: usize = t.scopes.iter().filter(|s| s.comp_inlined == false).map(|s| s.symbols.iter().filter(|y| y.name == "x").count()).sum();
        assert_eq!(n, 1, "{}", comp);
    }
}
