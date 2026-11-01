//! Per-construct checks of the generated code units.
mod common;
use common::*;
use rpy_compiler::bytecode::*;
use rpy_compiler::{compile, CompileOptions, Mode};

fn has(c: &CodeUnit, op: Op) -> bool {
    ops(c).contains(&op)
}

fn consts_have(c: &CodeUnit, k: &Constant) -> bool {
    c.consts.iter().any(|x| x == k)
}

fn s(x: &str) -> Constant {
    Constant::Str(x.chars().map(|c| c as u32).collect())
}

#[test]
fn module_basics() {
    let c = comp("x = 1\n");
    assert_eq!(c.name, "<module>");
    assert_eq!(ops(&c), vec![Op::Resume, Op::LoadConst, Op::StoreName, Op::LoadConst, Op::ReturnValue]);
    assert_eq!(c.locations.len(), c.code.len());
    assert_eq!(c.locations[1].lineno, 1);
    assert!(c.stacksize >= 1);
}

#[test]
fn eval_and_single_modes() {
    let m = parse_src("1 + x", Mode::Eval);
    let c = compile(&m, &CompileOptions::new("<e>", Mode::Eval)).unwrap().code;
    assert_eq!(*ops(&c).last().unwrap(), Op::ReturnValue);
    assert!(!has(&c, Op::PopTop));
    let m = parse_src("x\n", Mode::Single);
    let c = compile(&m, &CompileOptions::new("<s>", Mode::Single)).unwrap().code;
    assert!(c.code.iter().any(|&w| decode(w) == (Op::CallIntrinsic1, Intrinsic1::Print as u32)));
}

#[test]
fn constant_folding() {
    let c = comp("a = -5\nb = 2 ** 8\nc = 'ab' + 'cd'\nd = (1, 2, (3, 4))\ne = 1 + 2j\nf = 2 ** 200\ng = 'x' * 5000\n");
    assert!(consts_have(&c, &Constant::Int(-5)));
    assert!(consts_have(&c, &Constant::Int(256)));
    assert!(consts_have(&c, &s("abcd")));
    assert!(consts_have(&c, &Constant::Tuple(vec![Constant::Int(1), Constant::Int(2), Constant::Tuple(vec![Constant::Int(3), Constant::Int(4)])])));
    assert!(consts_have(&c, &Constant::Complex { real: 1.0, imag: 2.0 }));
    // too big: not folded
    assert!(has(&c, Op::BinaryOp));
    let c = comp("if x in [1, 2, 3]: pass\nfor y in {4, 5}: pass\n");
    assert!(c.consts.iter().any(|k| matches!(k, Constant::Tuple(v) if v.len() == 3)));
    assert!(c.consts.iter().any(|k| matches!(k, Constant::FrozenSet(v) if v.len() == 2)));
}

#[test]
fn debug_and_asserts() {
    let m = parse_src("assert x\nif __debug__: y()\n", Mode::Exec);
    let c0 = compile(&m, &CompileOptions::new("<a>", Mode::Exec)).unwrap().code;
    assert!(has(&c0, Op::RaiseVarargs));
    let mut o = CompileOptions::new("<a>", Mode::Exec);
    o.optimize = 1;
    let c1 = compile(&m, &o).unwrap().code;
    assert!(!has(&c1, Op::RaiseVarargs));
    assert!(!has(&c1, Op::Call));
}

#[test]
fn function_flags_and_varnames() {
    let c = comp("def f(a, /, b, *args, c, d=1, **kw):\n    '''doc'''\n    e = a\n    return e\n");
    let f = find_code(&c, "f").unwrap();
    assert_eq!((f.argcount, f.posonlyargcount, f.kwonlyargcount), (2, 1, 2));
    assert_eq!(f.localsplusnames, ["a", "b", "c", "d", "args", "kw", "e"]);
    assert_eq!(f.flags & (co::OPTIMIZED | co::NEWLOCALS | co::VARARGS | co::VARKEYWORDS | co::HAS_DOCSTRING), co::OPTIMIZED | co::NEWLOCALS | co::VARARGS | co::VARKEYWORDS | co::HAS_DOCSTRING);
    assert_eq!(f.consts[0], s("doc"));
    assert_eq!(f.localspluskinds[0], fast::LOCAL | fast::ARG_POS);
    assert_eq!(f.localspluskinds[1], fast::LOCAL | fast::ARG_POS | fast::ARG_KW);
    assert_eq!(f.localspluskinds[2], fast::LOCAL | fast::ARG_KW);
    assert_eq!(f.localspluskinds[4], fast::LOCAL | fast::ARG_VAR | fast::ARG_POS);
    assert_eq!(f.localspluskinds[5], fast::LOCAL | fast::ARG_VAR | fast::ARG_KW);
    // parameters and stored locals are loaded without checks
    assert!(!has(f, Op::LoadFastCheck));
    // kwdefaults dict and defaults: SetFunctionAttribute order KWDEFAULTS then DEFAULTS
    let attrs: Vec<u32> = c.code.iter().map(|&w| decode(w)).filter(|(o, _)| *o == Op::SetFunctionAttribute).map(|(_, a)| a).collect();
    assert_eq!(attrs, vec![make_fn::KWDEFAULTS]);
}

#[test]
fn docstring_cleaning_and_oo() {
    let src = "def f():\n    '''First.\n\n        Indented.\n    Body.\n    '''\n";
    let c = comp(src);
    let f = find_code(&c, "f").unwrap();
    assert_eq!(f.consts[0], s("First.\n\n    Indented.\nBody.\n"));
    let mut o = CompileOptions::new("<d>", Mode::Exec);
    o.optimize = 2;
    let c = compile(&parse_src(src, Mode::Exec), &o).unwrap().code;
    assert_eq!(find_code(&c, "f").unwrap().flags & co::HAS_DOCSTRING, 0);
}

#[test]
fn unbound_local_checks() {
    let c = comp("def f(x):\n    if x:\n        y = 1\n    return y\n");
    assert!(has(find_code(&c, "f").unwrap(), Op::LoadFastCheck));
    let c = comp("def f(x):\n    y = 1\n    del y\n    return y\n");
    assert!(has(find_code(&c, "f").unwrap(), Op::LoadFastCheck));
    let c = comp("def f(x):\n    y = 1\n    return y\n");
    assert!(!has(find_code(&c, "f").unwrap(), Op::LoadFastCheck));
}

#[test]
fn closures_and_cells() {
    let c = comp("def f(a):\n    b = 1\n    def g():\n        return a + b\n    return g\n");
    let f = find_code(&c, "f").unwrap();
    let g = find_code(&c, "g").unwrap();
    // a is a cell that is also an argument (shares its slot); b is a separate cell
    assert_eq!(f.localspluskinds[0], fast::LOCAL | fast::CELL | fast::ARG_POS | fast::ARG_KW);
    assert!(f.localsplusnames.contains(&"b".to_string()));
    assert_eq!(ops(f)[0], Op::MakeCell);
    assert_eq!(ops(g)[0], Op::CopyFreeVars);
    assert_eq!(g.localsplusnames, ["a", "b"]);
    assert!(g.localspluskinds.iter().all(|k| *k == fast::FREE));
    assert!(has(f, Op::SetFunctionAttribute));
}

#[test]
fn class_body() {
    let c = comp("class C(B):\n    '''d'''\n    def m(self):\n        self.z = 1\n        return super().m()\n");
    let body = find_code(&c, "C").unwrap();
    for n in ["__module__", "__qualname__", "__firstlineno__", "__doc__", "__static_attributes__", "__classcell__"] {
        assert!(body.names.contains(&n.to_string()), "{}", n);
    }
    assert!(consts_have(body, &Constant::Tuple(vec![s("z")])));
    assert_eq!(body.flags & co::OPTIMIZED, 0);
    let m = find_code(&c, "m").unwrap();
    assert_eq!(m.qualname, "C.m");
    assert!(m.flags & co::METHOD != 0);
    assert!(has(m, Op::LoadSuperAttr));
    assert!(m.localsplusnames.contains(&"__class__".to_string()));
}

#[test]
fn qualnames() {
    let c = comp("def f():\n    def g():\n        return lambda: 1\n    class K:\n        def h(self): pass\n    return g, K\n");
    assert_eq!(find_code(&c, "g").unwrap().qualname, "f.<locals>.g");
    assert_eq!(find_code(&c, "<lambda>").unwrap().qualname, "f.<locals>.g.<locals>.<lambda>");
    assert_eq!(find_code(&c, "h").unwrap().qualname, "f.<locals>.K.h");
    let c = comp("def f():\n    global g\n    def g(): pass\n");
    assert_eq!(find_code(&c, "g").unwrap().qualname, "g");
}

#[test]
fn generators_and_coroutines() {
    let c = comp("def g():\n    yield 1\nasync def a():\n    await x\nasync def ag():\n    yield 1\n");
    let g = find_code(&c, "g").unwrap();
    assert!(g.flags & co::GENERATOR != 0);
    assert_eq!(g.exception_table.len(), 1);
    assert!(g.exception_table[0].lasti);
    let a = find_code(&c, "a").unwrap();
    assert!(a.flags & co::COROUTINE != 0);
    assert!(has(a, Op::Send) && has(a, Op::CleanupThrow) && has(a, Op::GetAwaitable));
    let ag = find_code(&c, "ag").unwrap();
    assert!(ag.flags & co::ASYNC_GENERATOR != 0);
    assert!(ag.code.iter().any(|&w| decode(w) == (Op::CallIntrinsic1, Intrinsic1::AsyncGenWrap as u32)));
}

#[test]
fn comprehension_inlining() {
    let c = comp("def f(xs):\n    return [x for x in xs]\n");
    let f = find_code(&c, "f").unwrap();
    assert!(has(f, Op::LoadFastAndClear) && has(f, Op::ListAppend));
    assert!(find_code(&c, "<listcomp>").is_none());
    let c = comp("g = (x for x in y)\n");
    let ge = find_code(&c, "<genexpr>").unwrap();
    assert_eq!(ge.argcount, 1);
    assert_eq!(ge.localsplusnames[0], ".0");
    // module-level comprehension variable is a hidden fast local
    let c = comp("[x for x in y]\n");
    let i = c.localsplusnames.iter().position(|n| n == "x").unwrap();
    assert!(c.localspluskinds[i] & fast::HIDDEN != 0);
}

#[test]
fn exception_tables() {
    let c = comp("try:\n    a()\nexcept E as e:\n    b()\nfinally:\n    c()\n");
    assert!(c.exception_table.len() >= 3);
    for e in &c.exception_table {
        assert!(e.start < e.end && (e.target as usize) < c.code.len());
    }
    // ranges never overlap
    let mut v: Vec<(u32, u32)> = c.exception_table.iter().map(|e| (e.start, e.end)).collect();
    v.sort();
    for w in v.windows(2) {
        assert!(w[0].1 <= w[1].0);
    }
    let c = comp("with a as b:\n    pass\n");
    assert!(has(&c, Op::BeforeWith) && has(&c, Op::WithExceptStart));
}

#[test]
fn calls() {
    let c = comp("f(1, k=2)\no.m(1)\nf(*a, **k)\n");
    assert!(has(&c, Op::CallKw) && has(&c, Op::LoadMethod) && has(&c, Op::CallFunctionEx));
    assert!(consts_have(&c, &Constant::Tuple(vec![s("k")])));
}

#[test]
fn annotations() {
    let c = comp("x: int = 1\ndef f(a: int) -> str: pass\nclass C:\n    y: str\n");
    assert!(c.names.contains(&"__annotate__".to_string()));
    assert!(c.names.contains(&"__conditional_annotations__".to_string()));
    let anns: Vec<&CodeUnit> = c.consts.iter().filter_map(|k| if let Constant::Code(x) = k { Some(&**x) } else { None }).filter(|x| x.name == "__annotate__").collect();
    assert!(!anns.is_empty());
    for a in &anns {
        assert_eq!(a.localsplusnames[0], "format");
        assert_eq!((a.argcount, a.posonlyargcount), (1, 1));
    }
    let f_ann = find_code(&c, "C").unwrap();
    assert!(f_ann.names.contains(&"__annotate_func__".to_string()));
    let c = comp("from __future__ import annotations\nx: int\n");
    assert!(has(&c, Op::SetupAnnotations));
    assert!(consts_have(&c, &s("int")));
    assert!(c.flags & rpy_compiler::cf::CO_FUTURE_ANNOTATIONS != 0);
}

#[test]
fn generics() {
    let c = comp("def f[T](x: T) -> T: return x\nclass C[T]: pass\ntype A = int\n");
    assert!(find_code(&c, "<generic parameters of f>").is_some());
    assert!(find_code(&c, "<generic parameters of C>").is_some());
    let all = rpy_compiler::dis::disassemble(&c);
    assert!(all.contains("CallIntrinsic1"));
}

#[test]
fn errors() {
    let cases = [
        ("return 1\n", "'return' outside function"),
        ("break\n", "'break' outside loop"),
        ("continue\n", "'continue' not properly in loop"),
        ("def f():\n    x = 1\n    global x\n", "name 'x' is assigned to before global declaration"),
        ("def f():\n    print(x)\n    global x\n", "name 'x' is used prior to global declaration"),
        ("def f():\n    nonlocal x\n", "no binding for nonlocal 'x' found"),
        ("nonlocal x\n", "nonlocal declaration not allowed at module level"),
        ("def f(x):\n    nonlocal x\n", "name 'x' is parameter and nonlocal"),
        ("def f():\n    [i := 0 for i in x]\n", "assignment expression cannot rebind comprehension iteration variable 'i'"),
        ("def f(): yield from x\nclass C: [(y := 1) for i in x]\n", "assignment expression within a comprehension cannot be used in a class body"),
        ("await x\n", "'await' outside function"),
        ("def f():\n    await x\n", "'await' outside async function"),
        ("yield 1\n", "'yield' outside function"),
        ("def f():\n    def g(): pass\n    from m import *\n", "import * only allowed at module level"),
        ("async def f():\n    yield 1\n    return 2\n", "'return' with value in async generator"),
        ("def f():\n    lazy import os\n", "lazy import not allowed inside functions"),
        ("try:\n    lazy import os\nexcept: pass\n", "lazy import not allowed inside try/except blocks"),
        ("def f(x: (yield)): pass\n", "yield expression cannot be used within an annotation"),
        ("type A[T] = (yield)\n", "yield expression cannot be used within a type alias"),
        ("match x:\n    case a: pass\n    case b: pass\n", "name capture 'a' makes remaining patterns unreachable"),
        ("match x:\n    case [a, a]: pass\n", "multiple assignments to name 'a' in pattern"),
        ("match x:\n    case [a] | [b]: pass\n", "alternative patterns bind different names"),
        ("from __future__ import braces\n", "not a chance"),
        ("x = 1\nfrom __future__ import annotations\n", "from __future__ imports must occur at the beginning of the file"),
        ("for x in y:\n    try: pass\n    except* E:\n        break\n", "'break', 'continue' and 'return' cannot appear in an except* block"),
        ("def f[T, U=int, V](): pass\n", "non-default type parameter 'V' follows default type parameter"),
        ("a, *b, *c = d\n", "multiple starred expressions in assignment"),
        ("f(a=1, a=2)\n", "keyword argument repeated: a"),
    ];
    for (src, msg) in cases {
        let e = comp_err(src);
        assert_eq!(e.msg, msg, "for {:?}", src);
    }
    let e = comp_err("def f():\n    x = 1\n    global x\n");
    assert_eq!(e.lineno, 3);
}

#[test]
fn warnings() {
    let src = "x is 1\nassert (x, 'm')\n(1, 2)(3)\ndef f():\n    try: pass\n    finally: return 1\n'abc'[1.5]\n";
    let out = try_compile(src).unwrap();
    let msgs: Vec<&str> = out.warnings.iter().map(|w| w.msg.as_str()).collect();
    assert!(msgs.contains(&"\"is\" with 'int' literal. Did you mean \"==\"?"));
    assert!(msgs.contains(&"assertion is always true, perhaps remove parentheses?"));
    assert!(msgs.contains(&"'tuple' object is not callable; perhaps you missed a comma?"));
    assert!(msgs.contains(&"'return' in a 'finally' block"));
    assert!(msgs.contains(&"str indices must be integers or slices, not float; perhaps you missed a comma?"));
    // dedup: the finally body is compiled twice
    let out = try_compile("try:\n    pass\nfinally:\n    1 is 1\n").unwrap();
    assert_eq!(out.warnings.len(), 1);
}

#[test]
fn match_statement() {
    let c = comp("match x:\n    case [1, *r]: pass\n    case {'k': v, **kw}: pass\n    case P(a, b=2): pass\n    case None: pass\n    case _: pass\n");
    for op in [Op::MatchSequence, Op::MatchMapping, Op::MatchKeys, Op::MatchClass, Op::GetLen] {
        assert!(has(&c, op), "{:?}", op);
    }
}

#[test]
fn imports() {
    let c = comp("import a.b.c as d\nfrom . import e\nlazy import f\n");
    let lazy = c.code.iter().map(|&w| decode(w)).filter(|(o, _)| *o == Op::ImportName).any(|(_, a)| a >> 23 == 1);
    assert!(lazy);
    assert!(has(&c, Op::ImportFrom));
}

#[test]
fn line_numbers() {
    let c = comp("a = 1\n\nb = 2\nif a:\n    c = 3\n");
    let lines: std::collections::BTreeSet<u32> = c.locations.iter().map(|l| l.lineno).filter(|&l| l > 0).collect();
    assert_eq!(lines.into_iter().collect::<Vec<_>>(), vec![1, 3, 4, 5]);
    // `pass` keeps a NOP for its line
    let c = comp("def f():\n    pass\n");
    let f = find_code(&c, "f").unwrap();
    assert!(f.locations.iter().any(|l| l.lineno == 2));
}

#[test]
fn extended_arg() {
    let mut src = String::new();
    for i in 0..300 {
        src.push_str(&format!("x{} = {}\n", i, i));
    }
    let c = comp(&src);
    assert_eq!(c.locations.len(), c.code.len());
}

#[test]
fn percent_format_folding() {
    let mut m = parse_src("'%s' % (a,)\n'%-5r|%%|%.2a' % (b, c)\n'%d' % (d,)\n", Mode::Exec);
    rpy_compiler::optimize_ast(&mut m, 1, 0).unwrap();
    let d = format!("{:?}", m);
    assert!(d.contains("JoinedStr"));
    assert!(d.matches("FormattedValue").count() == 3, "{}", d);
    assert!(d.contains("BinOp")); // %d stays
    let c = comp("x = '%s=%r' % (a, b)\n");
    assert!(has(&c, Op::FormatSimple) && has(&c, Op::BuildString));
}

#[test]
fn unused_consts_removed() {
    let c = comp("l = lambda: 'this is the only const'\n");
    assert_eq!(find_code(&c, "<lambda>").unwrap().consts, vec![s("this is the only const")]);
    let c = comp("def f():\n    'doc'\n    return 42\n");
    assert_eq!(find_code(&c, "f").unwrap().consts, vec![s("doc"), Constant::Int(42)]);
    let c = comp("def f():\n    if True:\n        return 'used'\n    else:\n        return 'unused'\n");
    assert!(!find_code(&c, "f").unwrap().consts.contains(&s("unused")));
}

#[test]
fn position_validation() {
    use rpy_parser::ast::*;
    let mut m = parse_src("a = 1\n", Mode::Exec);
    assert!(rpy_compiler::validate_ast(&m).is_ok());
    for (l, el) in [(10, 1), (-10, -11), (10, -11), (-5, -2), (-5, 1)] {
        if let Mod::Module { body, .. } = &mut m {
            body[0].loc.lineno = l;
            body[0].loc.end_lineno = el;
        }
        let e = rpy_compiler::validate_ast(&m).unwrap_err();
        assert_eq!(e.kind, rpy_compiler::ErrorKind::ValueError);
    }
    // compiling odd positions must not panic
    let mut m = parse_src("import traceback\n", Mode::Exec);
    if let Mod::Module { body, .. } = &mut m {
        body[0].loc = Loc::new(-2, -2, -2, -2);
    }
    compile(&m, &CompileOptions::new("<t>", Mode::Exec)).unwrap();
}

#[test]
fn optimize_ast_docstrings() {
    use rpy_parser::ast::*;
    let mut m = parse_src("class A:\n    \"\"\"\n    Docstring\"\"\"\n", Mode::Exec);
    rpy_compiler::optimize_ast(&mut m, 2, 0).unwrap();
    if let Mod::Module { body, .. } = &m {
        if let StmtKind::ClassDef { body, .. } = &body[0].node {
            assert!(matches!(body[0].node, StmtKind::Pass));
            assert_eq!(body[0].loc, Loc::new(2, 4, 2, 8));
        } else {
            panic!()
        }
    }
    let mut m = parse_src("__debug__\n", Mode::Exec);
    rpy_compiler::optimize_ast(&mut m, 1, 0).unwrap();
    assert!(format!("{:?}", m).contains("Bool(false)"));
}
