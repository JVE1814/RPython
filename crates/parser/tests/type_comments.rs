//! Port of the checks in Lib/test/test_type_comments.py (sources extracted from that file).

mod common;

use rpy_parser::ast::*;
use rpy_parser::{parse, Mode, ParseOptions};

fn sources() -> std::collections::HashMap<String, String> {
    let src = std::fs::read_to_string(common::spec_path("test_type_comments.py")).unwrap();
    let m = parse(&src, &ParseOptions::default()).result.unwrap();
    let Mod::Module { body, .. } = m else { panic!() };
    let mut out = std::collections::HashMap::new();
    for s in body {
        if let StmtKind::Assign { targets, value, .. } = &s.node {
            if let (ExprKind::Name { id, .. }, ExprKind::Constant { value: Constant::Str(v), .. }) = (&targets[0].node, &value.node) {
                out.insert(id.clone(), v.to_string_lossy());
            }
        }
    }
    out
}

fn tc_parse(src: &str, fv: Option<u32>) -> Result<Mod, rpy_parser::SyntaxError> {
    parse(src, &ParseOptions { type_comments: true, feature_version: fv, hooks: common::hooks(), ..Default::default() }).result
}

fn body(m: &Mod) -> &Vec<Stmt> {
    match m {
        Mod::Module { body, .. } => body,
        _ => panic!(),
    }
}

fn fdef(s: &Stmt) -> &FunctionDef {
    match &s.node {
        StmtKind::FunctionDef(f) | StmtKind::AsyncFunctionDef(f) => f,
        _ => panic!("not a def"),
    }
}

/// parse_all: versions in [minver, maxver] must parse, others must fail.
fn parse_all(src: &str, minver: u32, maxver: u32) -> Vec<Mod> {
    let mut v = Vec::new();
    for ver in 4..=15 {
        let r = tc_parse(src, Some(ver));
        if minver <= ver && ver <= maxver {
            v.push(r.unwrap_or_else(|e| panic!("version {}: {:?}", ver, e)));
        } else {
            assert!(r.is_err(), "version {} should fail for {:?}", ver, src);
        }
    }
    v
}

#[test]
fn type_comments() {
    let s = sources();
    for t in parse_all(&s["funcdef"], 4, 15) {
        assert_eq!(fdef(&body(&t)[0]).type_comment.as_deref(), Some("() -> int"));
        assert_eq!(fdef(&body(&t)[1]).type_comment.as_deref(), Some("() -> None"));
    }
    let t = parse(&s["funcdef"], &ParseOptions::default()).result.unwrap();
    assert_eq!(fdef(&body(&t)[0]).type_comment, None);
    for t in parse_all(&s["asyncdef"], 5, 15) {
        assert_eq!(fdef(&body(&t)[0]).type_comment.as_deref(), Some("() -> int"));
        assert_eq!(fdef(&body(&t)[1]).type_comment.as_deref(), Some("() -> int"));
    }
    assert!(parse(&s["asyncvar"], &ParseOptions::default()).result.is_err());
    parse_all(&s["asynccomp"], 6, 15);
    parse_all(&s["matmul"], 5, 15);
    parse_all(&s["fstring"], 4, 15);
    parse_all(&s["underscorednumber"], 6, 15);
    for v in 4..=15 {
        let e = tc_parse(&s["redundantdef"], Some(v)).unwrap_err();
        assert!(e.msg.starts_with("Cannot have two type comments on def"), "{:?}", e);
    }
    for t in parse_all(&s["nonasciidef"], 4, 15) {
        assert_eq!(fdef(&body(&t)[0]).type_comment.as_deref(), Some("() -> àçčéñt"));
    }
    for t in parse_all(&s["forstmt"], 4, 15) {
        let StmtKind::For(f) = &body(&t)[0].node else { panic!() };
        assert_eq!(f.type_comment.as_deref(), Some("int"));
    }
    for name in ["withstmt", "parenthesized_withstmt"] {
        for t in parse_all(&s[name], 4, 15) {
            for st in body(&t) {
                let StmtKind::With(w) = &st.node else { panic!() };
                assert_eq!(w.type_comment.as_deref(), Some("int"), "{}", name);
            }
        }
    }
    for t in parse_all(&s["vardecl"], 4, 15) {
        let StmtKind::Assign { type_comment, .. } = &body(&t)[0].node else { panic!() };
        assert_eq!(type_comment.as_deref(), Some("int"));
    }
    for t in parse_all(&s["ignores"], 4, 15) {
        let Mod::Module { type_ignores, .. } = &t else { panic!() };
        let got: Vec<(i32, &str)> = type_ignores.iter().map(|t| (t.lineno, t.tag.as_str())).collect();
        assert_eq!(got, vec![(2, ""), (5, ""), (8, "[excuse]"), (9, "=excuse"), (10, " [excuse]"), (11, " whatever")]);
    }
    let t = parse(&s["ignores"], &ParseOptions::default()).result.unwrap();
    let Mod::Module { type_ignores, .. } = &t else { panic!() };
    assert!(type_ignores.is_empty());
    for t in parse_all(&s["longargs"], 8, 15) {
        for st in body(&t) {
            let f = fdef(st);
            let todo: Vec<char> = f.name[1..].chars().collect();
            let a = &f.args;
            assert_eq!(a.args.len() + a.posonlyargs.len(), todo.len() - a.vararg.is_some() as usize - a.kwarg.is_some() as usize, "{}", f.name);
            for (index, c) in todo.iter().enumerate() {
                let arg = match c {
                    'v' => a.vararg.as_deref().unwrap(),
                    'k' => a.kwarg.as_deref().unwrap(),
                    _ => {
                        let i = (*c as u8 - b'a') as usize;
                        if index < a.posonlyargs.len() {
                            &a.posonlyargs[i]
                        } else {
                            &a.args[i - a.posonlyargs.len()]
                        }
                    }
                };
                assert_eq!(arg.arg, c.to_string());
                assert_eq!(arg.type_comment.as_deref(), Some(c.to_ascii_uppercase().to_string().as_str()), "{} {}", f.name, c);
            }
        }
    }
    // inappropriate type comments: ignored without type_comments, errors with them
    for src in [
        "pass  # type: int\n",
        "foo()  # type: int\n",
        "x += 1  # type: int\n",
        "while True:  # type: int\n  continue\n",
        "while True:\n  continue  # type: int\n",
        "try:  # type: int\n  pass\nfinally:\n  pass\n",
        "try:\n  pass\nfinally:  # type: int\n  pass\n",
        "pass  # type: ignorewhatever\n",
        "pass  # type: ignoreé\n",
    ] {
        assert!(parse(src, &ParseOptions::default()).result.is_ok(), "{:?}", src);
        for v in 4..=15 {
            assert!(tc_parse(src, Some(v)).is_err(), "{:?} v{}", src, v);
        }
    }
    // func_type input
    let ft = |s: &str| parse(s, &ParseOptions::new(Mode::FuncType)).result;
    let Mod::FunctionType { argtypes, returns } = ft("() -> int").unwrap() else { panic!() };
    assert!(argtypes.is_empty());
    assert!(matches!(&returns.node, ExprKind::Name { id, .. } if id == "int"));
    let Mod::FunctionType { argtypes, .. } = ft("(int, *str, **Any) -> float").unwrap() else { panic!() };
    assert_eq!(argtypes.len(), 3);
    for s in ["(*int) -> None", "(**int) -> None", "(*int, **str) -> None", "(int) -> List[str]"] {
        assert!(ft(s).is_ok(), "{}", s);
    }
    assert!(ft("(int, *str, *Any) -> float").is_err());
    assert!(ft("(int, **str, Any) -> float").is_err());
}
