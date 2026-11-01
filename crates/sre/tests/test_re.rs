//! Port of spec/Lib/test/test_re.py (the parts that exercise the engine).

mod common;
use common::*;
use rpy_sre::flags::*;
use rpy_sre::*;

fn sp(m: Option<M>) -> (isize, isize) {
    m.expect("expected a match").span()
}

#[test]
fn test_search_star_plus() {
    assert_eq!(sp(re_search("x*", "axx")), (0, 0));
    assert_eq!(sp(re_search("x+", "axx")), (1, 3));
    assert!(re_search("x", "aaa").is_none());
    assert_eq!(sp(re_match("a*", "xxx")), (0, 0));
    assert_eq!(sp(re_match("x*", "xxxa")), (0, 3));
    assert!(re_match("a+", "xxx").is_none());
}

#[test]
fn test_branching() {
    assert_eq!(sp(re_match("(ab|ba)", "ab")), (0, 2));
    assert_eq!(sp(re_match("(ab|ba)", "ba")), (0, 2));
    assert_eq!(sp(re_match("(abc|bac|ca|cb)", "abc")), (0, 3));
    assert_eq!(sp(re_match("(abc|bac|ca|cb)", "bac")), (0, 3));
    assert_eq!(sp(re_match("(abc|bac|ca|cb)", "ca")), (0, 2));
    assert_eq!(sp(re_match("(abc|bac|ca|cb)", "cb")), (0, 2));
    assert_eq!(sp(re_match("((a)|(b)|(c))", "a")), (0, 1));
    assert_eq!(sp(re_match("((a)|(b)|(c))", "b")), (0, 1));
    assert_eq!(sp(re_match("((a)|(b)|(c))", "c")), (0, 1));
}

fn bump_num(m: &M) -> String {
    let v: i64 = m.g0().parse().unwrap();
    (v + 1).to_string()
}

#[test]
fn test_basic_re_sub() {
    assert_eq!(sub("y", "a", "xyz"), "xaz");
    for y in ["\u{e0}", "\u{430}", "\u{1d49c}"] {
        assert_eq!(sub(y, "a", &format!("x{y}z")), "xaz");
    }
    assert_eq!(sub("(?i)b+", "x", "bbbb BBBB"), "x x");
    assert_eq!(sub_fn(r"\d+", bump_num, "08.2 -2 23x99y", 0), "9.3 -3 24x100y");
    assert_eq!(sub_fn(r"\d+", bump_num, "08.2 -2 23x99y", 3), "9.3 -3 23x99y");
    assert_eq!(sub_fn(".", |_| r"\n".to_string(), "x", 0), "\\n");
    assert_eq!(sub(".", r"\n", "x"), "\n");
    let s = r"\1\1";
    assert_eq!(sub("(.)", s, "x"), "xx");
    assert_eq!(sub("(.)", &s.replace('\\', r"\\"), "x"), s);
    assert_eq!(sub("(?P<a>x)", r"\g<a>\g<a>", "xx"), "xxxx");
    assert_eq!(sub("(?P<a>x)", r"\g<a>\g<1>", "xx"), "xxxx");
    assert_eq!(sub("(?P<unk>x)", r"\g<unk>\g<unk>", "xx"), "xxxx");
    assert_eq!(sub("(?P<unk>x)", r"\g<1>\g<1>", "xx"), "xxxx");
    assert_eq!(sub("()x", r"\g<0>\g<0>", "xx"), "xxxx");
    assert_eq!(sub("a", r"\t\n\v\r\f\a\b", "a"), "\t\n\x0b\r\x0c\x07\x08");
    assert_eq!(sub("a", "\t\n\x0b\r\x0c\x07\x08", "a"), "\t\n\x0b\r\x0c\x07\x08");
    for ch in "cdehijklmopqsuwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ".chars() {
        let r = subn_pat(&c("a"), &format!("\\{ch}"), "a", 0);
        assert!(matches!(r, Err(TemplateError::Pattern { .. })), "\\{ch}");
    }
    assert_eq!(sub(r"^\s*", "X", "test"), "Xtest");
}

#[test]
fn test_bug_449964() {
    assert_eq!(sub(r"(?P<unk>x)", r"\g<1>\g<1>\b", "xx"), "xx\x08xx\x08");
}

#[test]
fn test_bug_449000() {
    assert_eq!(sub(r"\r\n", r"\n", "abc\r\ndef\r\n"), "abc\ndef\n");
    assert_eq!(sub("\r\n", r"\n", "abc\r\ndef\r\n"), "abc\ndef\n");
    assert_eq!(sub(r"\r\n", "\n", "abc\r\ndef\r\n"), "abc\ndef\n");
    assert_eq!(sub("\r\n", "\n", "abc\r\ndef\r\n"), "abc\ndef\n");
}

#[test]
fn test_bug_3629() {
    c("(?P<quote>)(?(quote))");
}

#[test]
fn test_sub_template_numeric_escape() {
    assert_eq!(sub("x", r"\0", "x"), "\0");
    assert_eq!(sub("x", r"\000", "x"), "\0");
    assert_eq!(sub("x", r"\001", "x"), "\x01");
    assert_eq!(sub("x", r"\008", "x"), "\x008");
    assert_eq!(sub("x", r"\009", "x"), "\x009");
    assert_eq!(sub("x", r"\111", "x"), "\x49");
    assert_eq!(sub("x", r"\117", "x"), "\x4f");
    assert_eq!(sub("x", r"\377", "x"), "\u{ff}");
    assert_eq!(sub("x", r"\1111", "x"), "\x491");
    assert_eq!(sub("x", r"\00", "x"), "\x00");
    assert_eq!(sub("x", r"\07", "x"), "\x07");
    assert_eq!(sub("x", r"\08", "x"), "\x008");
    assert_eq!(sub("x", r"\09", "x"), "\x009");
    assert_eq!(sub("x", r"\0a", "x"), "\x00a");

    check_template_error("x", r"\400", "x", r"octal escape value \400 outside of range 0-0o377", Some(0));
    check_template_error("x", r"\777", "x", r"octal escape value \777 outside of range 0-0o377", Some(0));
    check_template_error("x", r"\1", "x", "invalid group reference 1", Some(1));
    check_template_error("x", r"\8", "x", "invalid group reference 8", Some(1));
    check_template_error("x", r"\9", "x", "invalid group reference 9", Some(1));
    check_template_error("x", r"\11", "x", "invalid group reference 11", Some(1));
    check_template_error("x", r"\18", "x", "invalid group reference 18", Some(1));
    check_template_error("x", r"\1a", "x", "invalid group reference 1", Some(1));
    check_template_error("x", r"\90", "x", "invalid group reference 90", Some(1));
    check_template_error("x", r"\99", "x", "invalid group reference 99", Some(1));
    check_template_error("x", r"\118", "x", "invalid group reference 11", Some(1));
    check_template_error("x", r"\11a", "x", "invalid group reference 11", Some(1));
    check_template_error("x", r"\181", "x", "invalid group reference 18", Some(1));
    check_template_error("x", r"\800", "x", "invalid group reference 80", Some(1));
    check_template_error("x", r"\8", "", "invalid group reference 8", Some(1));

    assert_eq!(sub("(((((((((((x)))))))))))", r"\11", "x"), "x");
    assert_eq!(sub("((((((((((y))))))))))(.)", r"\118", "xyz"), "xz8");
    assert_eq!(sub("((((((((((y))))))))))(.)", r"\11a", "xyz"), "xza");
}

#[test]
fn test_qualified_re_sub() {
    assert_eq!(sub("a", "b", "aaaaa"), "bbbbb");
    assert_eq!(subn("a", "b", "aaaaa", 1).0, "baaaa");
}

#[test]
fn test_bug_114660() {
    assert_eq!(sub(r"(\S)\s+(\S)", r"\1 \2", "hello  there"), "hello there");
}

#[test]
fn test_symbolic_groups() {
    c(r"(?P<a>x)(?P=a)(?(a)y)");
    c(r"(?P<a1>x)(?P=a1)(?(a1)y)");
    c(r"(?P<a1>x)\1(?(1)y)");
    cb(b"(?P<a1>x)(?P=a1)(?(a1)y)", 0);
    c("(?P<\u{b5}>x)(?P=\u{b5})(?(\u{b5})y)");
    c("(?P<𝔘𝔫𝔦𝔠𝔬𝔡𝔢>x)(?P=𝔘𝔫𝔦𝔠𝔬𝔡𝔢)(?(𝔘𝔫𝔦𝔠𝔬𝔡𝔢)y)");
    let pat: Vec<String> = (1..=200).map(|i| format!("x(?P<a{i}>{i:x})y")).collect();
    let pat = format!("(?:{})(?(200)z|t)", pat.join("|"));
    assert_eq!(sp(re_match(&pat, "xc8yz")), (0, 5));
}

#[test]
fn test_symbolic_groups_errors() {
    check_pattern_error(r"(?P<a>)(?P<a>)", "redefinition of group name 'a' as group 2; was group 1", None);
    check_pattern_error(r"(?P<a>(?P=a))", "cannot refer to an open group", Some(10));
    check_pattern_error(r"(?Pxy)", "unknown extension ?Px", None);
    check_pattern_error(r"(?P<a>)(?P=a", "missing ), unterminated name", Some(11));
    check_pattern_error(r"(?P=", "missing group name", Some(4));
    check_pattern_error(r"(?P=)", "missing group name", Some(4));
    check_pattern_error(r"(?P=1)", "bad character in group name '1'", Some(4));
    check_pattern_error(r"(?P=a)", "unknown group name 'a'", None);
    check_pattern_error(r"(?P=a1)", "unknown group name 'a1'", None);
    check_pattern_error(r"(?P=a.)", "bad character in group name 'a.'", Some(4));
    check_pattern_error(r"(?P<)", "missing >, unterminated name", Some(4));
    check_pattern_error(r"(?P<a", "missing >, unterminated name", Some(4));
    check_pattern_error(r"(?P<", "missing group name", Some(4));
    check_pattern_error(r"(?P<>)", "missing group name", Some(4));
    check_pattern_error(r"(?P<1>)", "bad character in group name '1'", Some(4));
    check_pattern_error(r"(?P<a.>)", "bad character in group name 'a.'", Some(4));
    check_pattern_error(r"(?(", "missing group name", Some(3));
    check_pattern_error(r"(?())", "missing group name", Some(3));
    check_pattern_error(r"(?(a))", "unknown group name 'a'", Some(3));
    check_pattern_error(r"(?(-1))", "bad character in group name '-1'", Some(3));
    check_pattern_error(r"(?(1a))", "bad character in group name '1a'", Some(3));
    check_pattern_error(r"(?(a.))", "bad character in group name 'a.'", Some(3));
    check_pattern_error("(?P<\u{a9}>x)", "bad character in group name '\u{a9}'", Some(4));
    check_pattern_error("(?P=\u{a9})", "bad character in group name '\u{a9}'", Some(4));
    check_pattern_error("(?(\u{a9})y)", "bad character in group name '\u{a9}'", Some(3));
    check_pattern_error_b(b"(?P<\xc2\xb5>x)", r"bad character in group name '\xc2\xb5'", Some(4));
    check_pattern_error_b(b"(?P=\xc2\xb5)", r"bad character in group name '\xc2\xb5'", Some(4));
    check_pattern_error_b(b"(?(\xc2\xb5)y)", r"bad character in group name '\xc2\xb5'", Some(3));
}

#[test]
fn test_symbolic_refs() {
    assert_eq!(sub("(?P<a>x)|(?P<b>y)", r"\g<b>", "xx"), "");
    assert_eq!(sub("(?P<a>x)|(?P<b>y)", r"\2", "xx"), "");
    assert_eq!(sub("(?P<\u{b5}>x)", "\\g<\u{b5}>", "xx"), "xx");
    assert_eq!(sub("(?P<𝔘𝔫𝔦𝔠𝔬𝔡𝔢>x)", r"\g<𝔘𝔫𝔦𝔠𝔬𝔡𝔢>", "xx"), "xx");
    let pat: Vec<String> = (1..=200).map(|i| format!("x(?P<a{i}>{i:x})y")).collect();
    assert_eq!(sub(&pat.join("|"), r"\g<200>", "xc8yzxc8y"), "c8zc8");
    // bytes template
    let p = cb(b"(?P<a1>x)", 0);
    let t = p.parse_template(&[b'\\' as u32, b'g' as u32, b'<' as u32, b'a' as u32, b'1' as u32, b'>' as u32]).unwrap();
    assert_eq!(t, vec![TemplateItem::Group(1)]);
}

#[test]
fn test_symbolic_refs_errors() {
    check_template_error("(?P<a>x)", r"\g<a", "xx", "missing >, unterminated name", Some(3));
    check_template_error("(?P<a>x)", r"\g<", "xx", "missing group name", Some(3));
    check_template_error("(?P<a>x)", r"\g", "xx", "missing <", Some(2));
    check_template_error("(?P<a>x)", r"\g<a a>", "xx", "bad character in group name 'a a'", Some(3));
    check_template_error("(?P<a>x)", r"\g<>", "xx", "missing group name", Some(3));
    check_template_error("(?P<a>x)", r"\g<1a1>", "xx", "bad character in group name '1a1'", Some(3));
    check_template_error("(?P<a>x)", r"\g<2>", "xx", "invalid group reference 2", Some(3));
    check_template_error("(?P<a>x)", r"\2", "xx", "invalid group reference 2", Some(1));
    assert_eq!(sub_err("(?P<a>x)", r"\g<ab>", "xx"), TemplateError::Index("unknown group name 'ab'".into()));
    check_template_error("(?P<a>x)", r"\g<-1>", "xx", "bad character in group name '-1'", Some(3));
    check_template_error("(?P<a>x)", r"\g<+1>", "xx", "bad character in group name '+1'", Some(3));
    check_template_error(&"()".repeat(10), r"\g<1_0>", "xx", "bad character in group name '1_0'", Some(3));
    check_template_error("(?P<a>x)", r"\g< 1 >", "xx", "bad character in group name ' 1 '", Some(3));
    check_template_error("(?P<a>x)", "\\g<\u{a9}>", "xx", "bad character in group name '\u{a9}'", Some(3));
    check_template_error("(?P<a>x)", "\\g<\u{3280}>", "xx", "bad character in group name '\u{3280}'", Some(3));
    check_template_error("(?P<a>x)", "\\g<\u{b9}>", "xx", "bad character in group name '\u{b9}'", Some(3));
    check_template_error("(?P<a>x)", "\\g<\u{967}>", "xx", "bad character in group name '\u{967}'", Some(3));
    // bytes
    let p = cb(b"(?P<a>x)", 0);
    let repl: Vec<u32> = b"\\g<\xc2\xb5>".iter().map(|&b| b as u32).collect();
    assert_eq!(
        p.parse_template(&repl).unwrap_err(),
        TemplateError::Pattern { msg: r"bad character in group name '\xc2\xb5'".into(), pos: 3 }
    );
}

#[test]
fn test_re_subn() {
    assert_eq!(subn("(?i)b+", "x", "bbbb BBBB", 0), ("x x".into(), 2));
    assert_eq!(subn("b+", "x", "bbbb BBBB", 0), ("x BBBB".into(), 1));
    assert_eq!(subn("b+", "x", "xyz", 0), ("xyz".into(), 0));
    assert_eq!(subn("b*", "x", "xyz", 0), ("xxxyxzx".into(), 4));
    assert_eq!(subn("b*", "x", "xyz", 2), ("xxxyz".into(), 2));
}

#[test]
fn test_re_split() {
    let s = ":a:b::c";
    assert_eq!(split(":", s), somes(&["", "a", "b", "", "c"]));
    assert_eq!(split(":+", s), somes(&["", "a", "b", "c"]));
    assert_eq!(split("(:+)", s), somes(&["", ":", "a", ":", "b", "::", "c"]));
    for (a, b, cc) in [("\u{e0}", "\u{df}", "\u{e7}"), ("\u{430}", "\u{431}", "\u{432}"), ("\u{1d49c}", "\u{1d49e}", "\u{1d4b5}")] {
        let string = format!(":{a}:{b}::{cc}");
        assert_eq!(split(":", &string), somes(&["", a, b, "", cc]));
        assert_eq!(split(":+", &string), somes(&["", a, b, cc]));
        assert_eq!(split("(:+)", &string), somes(&["", ":", a, ":", b, "::", cc]));
    }
    assert_eq!(split("(?::+)", ":a:b::c"), somes(&["", "a", "b", "c"]));
    assert_eq!(split("(:)+", ":a:b::c"), somes(&["", ":", "a", ":", "b", ":", "c"]));
    assert_eq!(split("([b:]+)", ":a:b::c"), somes(&["", ":", "a", ":b::", "c"]));
    let n: Option<String> = None;
    let sm = |x: &str| Some(x.to_string());
    assert_eq!(
        split("(b)|(:+)", ":a:b::c"),
        vec![sm(""), n.clone(), sm(":"), sm("a"), n.clone(), sm(":"), sm(""), sm("b"), n.clone(), sm(""), n.clone(), sm("::"), sm("c")]
    );
    assert_eq!(split("(?:b)|(?::+)", ":a:b::c"), somes(&["", "a", "", "", "c"]));
    assert_eq!(split(":*", ":a:b::c"), somes(&["", "", "a", "", "b", "", "c", ""]));
    assert_eq!(split("(?::*)", ":a:b::c"), somes(&["", "", "a", "", "b", "", "c", ""]));
    assert_eq!(
        split("(:*)", ":a:b::c"),
        somes(&["", ":", "", "", "a", ":", "", "", "b", "::", "", "", "c", "", ""])
    );
    assert_eq!(
        split("(:)*", ":a:b::c"),
        vec![sm(""), sm(":"), sm(""), n.clone(), sm("a"), sm(":"), sm(""), n.clone(), sm("b"), sm(":"), sm(""), n.clone(), sm("c"), n.clone(), sm("")]
    );
    assert_eq!(split("", ":a:b::c"), somes(&["", ":", "a", ":", "b", ":", ":", "c", ""]));
    assert_eq!(split(r"\b", ":a:b::c"), somes(&[":", "a", ":", "b", "::", "c", ""]));
    assert_eq!(split(r"(?=:)", ":a:b::c"), somes(&["", ":a", ":b", ":", ":c"]));
    assert_eq!(split(r"(?<=:)", ":a:b::c"), somes(&[":", "a:", "b:", ":", "c"]));
}

#[test]
fn test_qualified_re_split() {
    assert_eq!(split_max(":", ":a:b::c", 2), somes(&["", "a", "b::c"]));
    assert_eq!(split_max(":", "a:b:c:d", 2), somes(&["a", "b", "c:d"]));
    assert_eq!(split_max("(:)", ":a:b::c", 2), somes(&["", ":", "a", ":", "b::c"]));
    assert_eq!(split_max("(:+)", ":a:b::c", 2), somes(&["", ":", "a", ":", "b::c"]));
    assert_eq!(split_max("(:*)", ":a:b::c", 2), somes(&["", ":", "", "", "a:b::c"]));
}

#[test]
fn test_re_findall() {
    assert!(findall(":+", "abc").is_empty());
    let s = "a:b::c:::d";
    assert_eq!(findall(":+", s), strs(&[":", "::", ":::"]));
    assert_eq!(findall("(:+)", s), strs(&[":", "::", ":::"]));
    assert_eq!(
        findall_pat(&c("(:)(:*)"), s),
        vec![strs(&[":", ""]), strs(&[":", ":"]), strs(&[":", "::"])]
    );
    for x in ["\u{e0}", "\u{430}", "\u{1d49c}"] {
        let xx = x.repeat(2);
        let xxx = x.repeat(3);
        let string = format!("a{x}b{xx}c{xxx}d");
        assert_eq!(findall(&format!("{x}+"), &string), strs(&[x, &xx, &xxx]));
        assert_eq!(findall(&format!("({x}+)"), &string), strs(&[x, &xx, &xxx]));
        assert_eq!(
            findall_pat(&c(&format!("({x})({x}*)")), &string),
            vec![strs(&[x, ""]), strs(&[x, x]), strs(&[x, &xx])]
        );
    }
}

#[test]
fn test_bug_117612() {
    assert_eq!(
        findall_pat(&c(r"(a|(b))"), "aba"),
        vec![strs(&["a", ""]), strs(&["b", "b"]), strs(&["a", ""])]
    );
}

#[test]
fn test_re_match() {
    assert_eq!(re_match("a", "a").unwrap().groups(), vec![]);
    assert_eq!(re_match("(a)", "a").unwrap().groups(), vec![Some("a".into())]);
    assert_eq!(re_match("(a)", "a").unwrap().g0(), "a");
    for a in ["\u{e0}", "\u{430}", "\u{1d49c}"] {
        assert!(re_match(a, a).unwrap().groups().is_empty());
        assert_eq!(re_match(&format!("({a})"), a).unwrap().g(1), Some(a.into()));
    }
    let pat = c("((a)|(b))(c)?");
    let g = |s: &str| {
        let v = cp(s);
        let m = pat.match_at(&v, 0, usize::MAX).unwrap();
        (1..=4).map(|i| group_of(&v, &m, i)).collect::<Vec<_>>()
    };
    let sm = |x: &str| Some(x.to_string());
    assert_eq!(g("a"), vec![sm("a"), sm("a"), None, None]);
    assert_eq!(g("b"), vec![sm("b"), None, sm("b"), None]);
    assert_eq!(g("ac"), vec![sm("a"), sm("a"), None, sm("c")]);
    assert_eq!(g("bc"), vec![sm("b"), None, sm("b"), sm("c")]);

    let m = re_match("(?:(?P<a1>a)|(?P<b2>b))(?P<c3>c)?", "a").unwrap();
    assert_eq!((m.g(1), m.g(2), m.g(3)), (sm("a"), None, None));
    let m = re_match("(?:(?P<a1>a)|(?P<b2>b))(?P<c3>c)?", "b").unwrap();
    assert_eq!((m.named("a1"), m.named("b2"), m.named("c3")), (None, sm("b"), None));
    let m = re_match("(?:(?P<a1>a)|(?P<b2>b))(?P<c3>c)?", "ac").unwrap();
    assert_eq!((m.g(1), m.named("b2"), m.g(3)), (sm("a"), None, sm("c")));
}

#[test]
fn test_re_fullmatch() {
    assert_eq!(sp(re_fullmatch(r"a", "a")), (0, 1));
    assert_eq!(sp(re_fullmatch(r"a|ab", "ab")), (0, 2));
    assert_eq!(b_fullmatch(br"a|ab", b"ab", 0).unwrap().spans[0], (0, 2));
    for (a, b) in [("\u{e0}", "\u{df}"), ("\u{430}", "\u{431}"), ("\u{1d49c}", "\u{1d49e}")] {
        let r = format!("{a}|{a}{b}");
        assert_eq!(sp(re_fullmatch(&r, &format!("{a}{b}"))), (0, 2));
    }
    assert_eq!(sp(re_fullmatch(r".*?$", "abc")), (0, 3));
    assert_eq!(sp(re_fullmatch(r".*?", "abc")), (0, 3));
    assert_eq!(sp(re_fullmatch(r"a.*?b", "ab")), (0, 2));
    assert_eq!(sp(re_fullmatch(r"a.*?b", "abb")), (0, 3));
    assert_eq!(sp(re_fullmatch(r"a.*?b", "axxb")), (0, 4));
    assert!(re_fullmatch(r"a+", "ab").is_none());
    assert!(re_fullmatch(r"abc$", "abc\n").is_none());
    assert!(re_fullmatch(r"abc\z", "abc\n").is_none());
    assert!(re_fullmatch(r"abc\Z", "abc\n").is_none());
    assert!(re_fullmatch(r"(?m)abc$", "abc\n").is_none());
    assert_eq!(sp(re_fullmatch(r"ab(?=c)cd", "abcd")), (0, 4));
    assert_eq!(sp(re_fullmatch(r"ab(?<=b)cd", "abcd")), (0, 4));
    assert_eq!(sp(re_fullmatch(r"(?=a|ab)ab", "ab")), (0, 2));
    let v = cp("abcd");
    assert_eq!(c(r"bc").fullmatch(&v, 1, 3).unwrap().spans[0], (1, 3));
    assert_eq!(c(r".*?$").fullmatch(&v, 1, 3).unwrap().spans[0], (1, 3));
    assert_eq!(c(r".*?").fullmatch(&v, 1, 3).unwrap().spans[0], (1, 3));
}

#[test]
fn test_re_groupref_exists() {
    let sm = |x: &str| Some(x.to_string());
    assert_eq!(re_match(r"^(\()?([^()]+)(?(1)\))$", "(a)").unwrap().groups(), vec![sm("("), sm("a")]);
    assert_eq!(re_match(r"^(\()?([^()]+)(?(1)\))$", "a").unwrap().groups(), vec![None, sm("a")]);
    assert!(re_match(r"^(\()?([^()]+)(?(1)\))$", "a)").is_none());
    assert!(re_match(r"^(\()?([^()]+)(?(1)\))$", "(a").is_none());
    assert_eq!(re_match("^(?:(a)|c)((?(1)b|d))$", "ab").unwrap().groups(), vec![sm("a"), sm("b")]);
    assert_eq!(re_match(r"^(?:(a)|c)((?(1)b|d))$", "cd").unwrap().groups(), vec![None, sm("d")]);
    assert_eq!(re_match(r"^(?:(a)|c)((?(1)|d))$", "cd").unwrap().groups(), vec![None, sm("d")]);
    assert_eq!(re_match(r"^(?:(a)|c)((?(1)|d))$", "a").unwrap().groups(), vec![sm("a"), sm("")]);

    let p = c("(?P<g1>a)(?P<g2>b)?((?(g2)c|d))");
    let g = |s: &str| {
        let v = cp(s);
        p.match_at(&v, 0, usize::MAX).map(|m| (1..=3).map(|i| group_of(&v, &m, i)).collect::<Vec<_>>())
    };
    assert_eq!(g("abc"), Some(vec![sm("a"), sm("b"), sm("c")]));
    assert_eq!(g("ad"), Some(vec![sm("a"), None, sm("d")]));
    assert_eq!(g("abd"), None);
    assert_eq!(g("ac"), None);

    let pat: Vec<String> = (1..=200).map(|i| format!("x(?P<a{i}>{i:x})y")).collect();
    let pat = format!("(?:{})(?(200)z)", pat.join("|"));
    assert_eq!(sp(re_match(&pat, "xc8yz")), (0, 5));
}

#[test]
fn test_re_groupref_exists_errors() {
    check_pattern_error(r"(?P<a>)(?(0)a|b)", "bad group number", Some(10));
    check_pattern_error(r"()(?(-1)a|b)", "bad character in group name '-1'", Some(5));
    check_pattern_error(r"()(?(+1)a|b)", "bad character in group name '+1'", Some(5));
    check_pattern_error(&format!("{}(?(1_0)a|b)", "()".repeat(10)), "bad character in group name '1_0'", Some(23));
    check_pattern_error(r"()(?( 1 )a|b)", "bad character in group name ' 1 '", Some(5));
    check_pattern_error("()(?(\u{3280})a|b)", "bad character in group name '\u{3280}'", Some(5));
    check_pattern_error("()(?(\u{b9})a|b)", "bad character in group name '\u{b9}'", Some(5));
    check_pattern_error("()(?(\u{967})a|b)", "bad character in group name '\u{967}'", Some(5));
    check_pattern_error(r"()(?(1", "missing ), unterminated name", Some(5));
    check_pattern_error(r"()(?(1)a", "missing ), unterminated subpattern", Some(2));
    check_pattern_error(r"()(?(1)a|b", "missing ), unterminated subpattern", Some(2));
    check_pattern_error(r"()(?(1)a|b|c", "conditional backref with more than two branches", Some(10));
    check_pattern_error(r"()(?(1)a|b|c)", "conditional backref with more than two branches", Some(10));
    check_pattern_error(r"()(?(2)a)", "invalid group reference 2", Some(5));
}

#[test]
fn test_re_groupref_exists_validation_bug() {
    for i in 0..256 {
        c(&format!(r"()(?(1)\x{i:02x}?)"));
    }
}

#[test]
fn test_re_groupref_overflow() {
    check_template_error("()", &format!(r"\g<{MAXGROUPS}>"), "xx", &format!("invalid group reference {MAXGROUPS}"), Some(3));
    check_pattern_error(&format!(r"(?P<a>)(?({MAXGROUPS}))"), &format!("invalid group reference {MAXGROUPS}"), Some(10));
}

#[test]
fn test_re_groupref() {
    let sm = |x: &str| Some(x.to_string());
    assert_eq!(re_match(r"^(\|)?([^()]+)\1$", "|a|").unwrap().groups(), vec![sm("|"), sm("a")]);
    assert_eq!(re_match(r"^(\|)?([^()]+)\1?$", "a").unwrap().groups(), vec![None, sm("a")]);
    assert!(re_match(r"^(\|)?([^()]+)\1$", "a|").is_none());
    assert!(re_match(r"^(\|)?([^()]+)\1$", "|a").is_none());
    assert_eq!(re_match(r"^(?:(a)|c)(\1)$", "aa").unwrap().groups(), vec![sm("a"), sm("a")]);
    assert_eq!(re_match(r"^(?:(a)|c)(\1)?$", "c").unwrap().groups(), vec![None, None]);
    check_pattern_error(r"(abc\1)", "cannot refer to an open group", Some(4));
}

#[test]
fn test_groupdict_expand() {
    let m = re_match("(?P<first>first) (?P<second>second)", "first second").unwrap();
    assert_eq!(m.named("first"), Some("first".into()));
    assert_eq!(m.named("second"), Some("second".into()));
    let t = m.pat.parse_template(&cp(r"\2 \1 \g<second> \g<first>")).unwrap();
    let mut out = Vec::new();
    expand_template(&t, |g| m.caps.span(g).map(|(a, b)| &m.s[a..b]), &mut out);
    assert_eq!(st(&out), "second first second first");
    let m = re_match("(?P<first>first)|(?P<second>second)", "first").unwrap();
    let t = m.pat.parse_template(&cp(r"\2 \g<second>")).unwrap();
    let mut out = Vec::new();
    expand_template(&t, |g| m.caps.span(g).map(|(a, b)| &m.s[a..b]), &mut out);
    assert_eq!(st(&out), " ");
}

#[test]
fn test_repeat_minmax() {
    assert!(re_match(r"^(\w){1}$", "abc").is_none());
    assert!(re_match(r"^(\w){1}?$", "abc").is_none());
    assert!(re_match(r"^(\w){1,2}$", "abc").is_none());
    assert!(re_match(r"^(\w){1,2}?$", "abc").is_none());
    for p in [r"^(\w){3}$", r"^(\w){1,3}$", r"^(\w){1,4}$", r"^(\w){3,4}?$", r"^(\w){3}?$", r"^(\w){1,3}?$", r"^(\w){1,4}?$"] {
        assert_eq!(re_match(p, "abc").unwrap().g(1), Some("c".into()), "{p}");
    }
    assert!(re_match(r"^x{1}$", "xxx").is_none());
    assert!(re_match(r"^x{1}?$", "xxx").is_none());
    assert!(re_match(r"^x{1,2}$", "xxx").is_none());
    assert!(re_match(r"^x{1,2}?$", "xxx").is_none());
    for p in [r"^x{3}$", r"^x{1,3}$", r"^x{3,3}$", r"^x{1,4}$", r"^x{3,4}?$", r"^x{3}?$", r"^x{1,3}?$", r"^x{1,4}?$"] {
        assert!(re_match(p, "xxx").is_some(), "{p}");
    }
    assert!(re_match(r"^x{}$", "xxx").is_none());
    assert!(re_match(r"^x{}$", "x{}").is_some());
    check_pattern_error(r"x{2,1}", "min repeat greater than max repeat", Some(2));
}

#[test]
fn test_getattr() {
    let p = c("(?i)(a)(b)");
    assert_eq!(p.flags(), IGNORECASE | UNICODE);
    assert_eq!(p.groups(), 2);
    assert!(p.group_index().is_empty());
    let p = c("(?i)(?P<first>a)(?P<other>b)");
    assert_eq!(p.group_index(), &[("first".to_string(), 1), ("other".to_string(), 2)]);
    let m = re_match("(a)", "a").unwrap();
    assert_eq!(m.caps.spans, vec![(0, 1), (0, 1)]);
}

#[test]
fn test_special_escapes() {
    assert_eq!(re_search(r"\b(b.)\b", "abcd abc bcd bx").unwrap().g(1), Some("bx".into()));
    assert_eq!(re_search(r"\B(b.)\B", "abc bcd bc abxd").unwrap().g(1), Some("bx".into()));
    assert_eq!(re_search_f(r"\b(b.)\b", "abcd abc bcd bx", ASCII).unwrap().g(1), Some("bx".into()));
    assert_eq!(re_search_f(r"\B(b.)\B", "abc bcd bc abxd", ASCII).unwrap().g(1), Some("bx".into()));
    assert_eq!(re_search_f(r"^abc$", "\nabc\n", MULTILINE).unwrap().g0(), "abc");
    assert_eq!(re_search_f(r"^\Aabc\z$", "abc", MULTILINE).unwrap().g0(), "abc");
    assert!(re_search_f(r"^\Aabc\z$", "\nabc\n", MULTILINE).is_none());
    assert_eq!(re_search_f(r"^\Aabc\Z$", "abc", MULTILINE).unwrap().g0(), "abc");
    assert!(re_search_f(r"^\Aabc\Z$", "\nabc\n", MULTILINE).is_none());
    let bs = |p: &[u8], s: &[u8], f: u32, g: usize| b_search(p, s, f).map(|c| c.spans[g]);
    assert_eq!(bs(br"\b(b.)\b", b"abcd abc bcd bx", 0, 1), Some((13, 15)));
    assert_eq!(bs(br"\B(b.)\B", b"abc bcd bc abxd", 0, 1), Some((12, 14)));
    assert_eq!(bs(br"\b(b.)\b", b"abcd abc bcd bx", LOCALE, 1), Some((13, 15)));
    assert_eq!(bs(br"\B(b.)\B", b"abc bcd bc abxd", LOCALE, 1), Some((12, 14)));
    assert_eq!(bs(br"^abc$", b"\nabc\n", MULTILINE, 0), Some((1, 4)));
    assert_eq!(bs(br"^\Aabc\z$", b"abc", MULTILINE, 0), Some((0, 3)));
    assert_eq!(bs(br"^\Aabc\z$", b"\nabc\n", MULTILINE, 0), None);
    assert_eq!(bs(br"^\Aabc\Z$", b"abc", MULTILINE, 0), Some((0, 3)));
    assert_eq!(bs(br"^\Aabc\Z$", b"\nabc\n", MULTILINE, 0), None);
    assert_eq!(re_search(r"\d\D\w\W\s\S", "1aa! a").unwrap().g0(), "1aa! a");
    assert_eq!(bs(br"\d\D\w\W\s\S", b"1aa! a", 0, 0), Some((0, 6)));
    assert_eq!(re_search_f(r"\d\D\w\W\s\S", "1aa! a", ASCII).unwrap().g0(), "1aa! a");
    assert_eq!(bs(br"\d\D\w\W\s\S", b"1aa! a", LOCALE, 0), Some((0, 6)));
}

#[test]
fn test_other_escapes() {
    check_pattern_error("\\", "bad escape (end of pattern)", Some(0));
    assert_eq!(re_match(r"\(", "(").unwrap().g0(), "(");
    assert!(re_match(r"\(", ")").is_none());
    assert_eq!(re_match(r"\\", "\\").unwrap().g0(), "\\");
    assert_eq!(re_match(r"[\]]", "]").unwrap().g0(), "]");
    assert!(re_match(r"[\]]", "[").is_none());
    assert_eq!(re_match(r"[a\-c]", "-").unwrap().g0(), "-");
    assert!(re_match(r"[a\-c]", "b").is_none());
    assert_eq!(re_match(r"[\^a]+", "a^").unwrap().g0(), "a^");
    assert!(re_match(r"[\^a]+", "b").is_none());
    for ch in "ceghijklmopqyCEFGHIJKLMNOPQRTVXY".chars() {
        assert!(compile_str(&format!("\\{ch}"), 0).is_err(), "\\{ch}");
    }
    for ch in "ceghijklmopqyzABCEFGHIJKLMNOPQRTVXYZ".chars() {
        assert!(compile_str(&format!("[\\{ch}]"), 0).is_err(), "[\\{ch}]");
    }
}

#[test]
fn test_named_unicode_escapes() {
    assert!(re_match(r"\N{LESS-THAN SIGN}", "<").is_some());
    assert!(re_match(r"\N{less-than sign}", "<").is_some());
    assert!(re_match(r"\N{LESS-THAN SIGN}", ">").is_none());
    assert!(re_match(r"\N{SNAKE}", "\u{1f40d}").is_some());
    assert!(re_match(
        r"\N{ARABIC LIGATURE UIGHUR KIRGHIZ YEH WITH HAMZA ABOVE WITH ALEF MAKSURA ISOLATED FORM}",
        "\u{fbf9}"
    )
    .is_some());
    assert!(re_match(r"[\N{LESS-THAN SIGN}-\N{GREATER-THAN SIGN}]", "=").is_some());
    assert!(re_match(r"[\N{LESS-THAN SIGN}-\N{GREATER-THAN SIGN}]", ";").is_none());

    check_pattern_error(r"\N", "missing {", Some(2));
    check_pattern_error(r"[\N]", "missing {", Some(3));
    check_pattern_error(r"\N{", "missing character name", Some(3));
    check_pattern_error(r"[\N{", "missing character name", Some(4));
    check_pattern_error(r"\N{}", "missing character name", Some(3));
    check_pattern_error(r"[\N{}]", "missing character name", Some(4));
    check_pattern_error(r"\NSNAKE}", "missing {", Some(2));
    check_pattern_error(r"[\NSNAKE}]", "missing {", Some(3));
    check_pattern_error(r"\N{SNAKE", "missing }, unterminated name", Some(3));
    check_pattern_error(r"[\N{SNAKE]", "missing }, unterminated name", Some(4));
    check_pattern_error(r"[\N{SNAKE]}", "undefined character name 'SNAKE]'", Some(1));
    check_pattern_error(r"\N{SPAM}", "undefined character name 'SPAM'", Some(0));
    check_pattern_error(r"[\N{SPAM}]", "undefined character name 'SPAM'", Some(1));
    check_pattern_error(r"\N{KEYCAP NUMBER SIGN}", "undefined character name 'KEYCAP NUMBER SIGN'", Some(0));
    check_pattern_error(r"[\N{KEYCAP NUMBER SIGN}]", "undefined character name 'KEYCAP NUMBER SIGN'", Some(1));
    check_pattern_error_b(br"\N{LESS-THAN SIGN}", r"bad escape \N", Some(0));
    check_pattern_error_b(br"[\N{LESS-THAN SIGN}]", r"bad escape \N", Some(1));
}

#[test]
fn test_word_boundaries() {
    let s = |p: &str, x: &str, f: u32| re_search_f(p, x, f);
    let m = |p: &str, x: &str, f: u32| re_match_f(p, x, f).is_some();
    let fm = |p: &str, x: &str, f: u32| re_fullmatch_f(p, x, f).is_some();
    let bm = |p: &[u8], x: &[u8], f: u32| b_match(p, x, f).is_some();
    let bfm = |p: &[u8], x: &[u8], f: u32| b_fullmatch(p, x, f).is_some();
    assert_eq!(s(r"\b(abc)\b", "abc", 0).unwrap().g(1), Some("abc".into()));
    assert_eq!(s(r"\b(abc)\b", "abc", ASCII).unwrap().g(1), Some("abc".into()));
    assert!(b_search(br"\b(abc)\b", b"abc", 0).is_some());
    assert!(b_search(br"\b(abc)\b", b"abc", LOCALE).is_some());
    assert_eq!(s(r"\b(ьюя)\b", "ьюя", 0).unwrap().g(1), Some("ьюя".into()));
    assert!(s(r"\b(ьюя)\b", "ьюя", ASCII).is_none());
    assert!(m(r".\b", "a=", 0));
    assert!(m(r".\b", "a=", ASCII));
    assert!(bm(br".\b", b"a=", 0));
    assert!(bm(br".\b", b"a=", LOCALE));
    assert!(m(r".\b", "я=", 0));
    assert!(!m(r".\b", "я=", ASCII));
    assert!(m(r".\b", "=a", 0));
    assert!(m(r".\b", "=a", ASCII));
    assert!(bm(br".\b", b"=a", 0));
    assert!(bm(br".\b", b"=a", LOCALE));
    assert!(m(r".\b", "=я", 0));
    assert!(!m(r".\b", "=я", ASCII));
    assert!(!m(r".\b", "ab", 0));
    assert!(!m(r".\b", "ab", ASCII));
    assert!(!bm(br".\b", b"ab", 0));
    assert!(!bm(br".\b", b"ab", LOCALE));
    assert!(!m(r".\b", "юя", 0));
    assert!(!m(r".\b", "юя", ASCII));
    assert!(!m(r".\b", "=-", 0));
    assert!(!m(r".\b", "=-", ASCII));
    assert!(!bm(br".\b", b"=-", 0));
    assert!(!bm(br".\b", b"=-", LOCALE));
    assert!(!m(r".\B", "a=", 0));
    assert!(!m(r".\B", "a=", ASCII));
    assert!(!bm(br".\B", b"a=", 0));
    assert!(!bm(br".\B", b"a=", LOCALE));
    assert!(!m(r".\B", "я=", 0));
    assert!(m(r".\B", "я=", ASCII));
    assert!(!m(r".\B", "=a", 0));
    assert!(!m(r".\B", "=a", ASCII));
    assert!(!bm(br".\B", b"=a", 0));
    assert!(!bm(br".\B", b"=a", LOCALE));
    assert!(!m(r".\B", "=я", 0));
    assert!(m(r".\B", "=я", ASCII));
    assert!(m(r".\B", "ab", 0));
    assert!(m(r".\B", "ab", ASCII));
    assert!(bm(br".\B", b"ab", 0));
    assert!(bm(br".\B", b"ab", LOCALE));
    assert!(m(r".\B", "юя", 0));
    assert!(m(r".\B", "юя", ASCII));
    assert!(m(r".\B", "=-", 0));
    assert!(m(r".\B", "=-", ASCII));
    assert!(bm(br".\B", b"=-", 0));
    assert!(bm(br".\B", b"=-", LOCALE));
    assert!(m(r"\b", "abc", 0));
    assert!(m(r"\b", "abc", ASCII));
    assert!(bm(br"\b", b"abc", 0));
    assert!(bm(br"\b", b"abc", LOCALE));
    assert!(m(r"\b", "ьюя", 0));
    assert!(!m(r"\b", "ьюя", ASCII));
    assert!(fm(r".+\b", "abc", 0));
    assert!(fm(r".+\b", "abc", ASCII));
    assert!(bfm(br".+\b", b"abc", 0));
    assert!(bfm(br".+\b", b"abc", LOCALE));
    assert!(fm(r".+\b", "ьюя", 0));
    assert!(s(r"\b", "ьюя", ASCII).is_none());
    assert_eq!(s(r"\B", "abc", 0).unwrap().span(), (1, 1));
    assert_eq!(s(r"\B", "abc", ASCII).unwrap().span(), (1, 1));
    assert_eq!(b_search(br"\B", b"abc", 0).unwrap().spans[0], (1, 1));
    assert_eq!(b_search(br"\B", b"abc", LOCALE).unwrap().spans[0], (1, 1));
    assert_eq!(s(r"\B", "ьюя", 0).unwrap().span(), (1, 1));
    assert_eq!(s(r"\B", "ьюя", ASCII).unwrap().span(), (0, 0));
    assert!(!m(r"\B", "abc", 0));
    assert!(!m(r"\B", "abc", ASCII));
    assert!(!bm(br"\B", b"abc", 0));
    assert!(!bm(br"\B", b"abc", LOCALE));
    assert!(!m(r"\B", "ьюя", 0));
    assert!(m(r"\B", "ьюя", ASCII));
    assert!(!fm(r".+\B", "abc", 0));
    assert!(!fm(r".+\B", "abc", ASCII));
    assert!(!bfm(br".+\B", b"abc", 0));
    assert!(!bfm(br".+\B", b"abc", LOCALE));
    assert!(!fm(r".+\B", "ьюя", 0));
    assert!(fm(r".+\B", "ьюя", ASCII));
    assert!(s(r"\b", "", 0).is_none());
    assert!(s(r"\b", "", ASCII).is_none());
    assert!(b_search(br"\b", b"", 0).is_none());
    assert!(b_search(br"\b", b"", LOCALE).is_none());
    assert!(s(r"\B", "", 0).is_some());
    assert!(s(r"\B", "", ASCII).is_some());
    assert!(b_search(br"\B", b"", 0).is_some());
    assert!(b_search(br"\B", b"", LOCALE).is_some());
    let fa = |p: &str, x: &str, f: u32| findall_f(p, x, f).len();
    let bfa = |p: &[u8], x: &[u8], f: u32| {
        let pat = cb(p, f);
        pat.scanner(x, 0, usize::MAX).count()
    };
    assert_eq!(fa(r"\b", "a", 0), 2);
    assert_eq!(fa(r"\b", "a", ASCII), 2);
    assert_eq!(bfa(br"\b", b"a", 0), 2);
    assert_eq!(bfa(br"\b", b"a", LOCALE), 2);
    assert_eq!(fa(r"\B", "a", 0), 0);
    assert_eq!(fa(r"\B", "a", ASCII), 0);
    assert_eq!(bfa(br"\B", b"a", 0), 0);
    assert_eq!(bfa(br"\B", b"a", LOCALE), 0);
    assert_eq!(fa(r"\b", " ", 0), 0);
    assert_eq!(fa(r"\b", " ", ASCII), 0);
    assert_eq!(bfa(br"\b", b" ", 0), 0);
    assert_eq!(bfa(br"\b", b" ", LOCALE), 0);
    assert_eq!(fa(r"\b", "   ", 0), 0);
    assert_eq!(fa(r"\b", "   ", ASCII), 0);
    assert_eq!(bfa(br"\b", b"   ", 0), 0);
    assert_eq!(bfa(br"\b", b"   ", LOCALE), 0);
    assert_eq!(fa(r"\B", " ", 0), 2);
    assert_eq!(fa(r"\B", " ", ASCII), 2);
    assert_eq!(bfa(br"\B", b" ", 0), 2);
    assert_eq!(bfa(br"\B", b" ", LOCALE), 2);
}

#[test]
fn test_bigcharset() {
    assert_eq!(re_match("([\u{2222}\u{2223}])", "\u{2222}").unwrap().g(1), Some("\u{2222}".into()));
    let r: String = (256..65536).step_by(255).map(|i| char::from_u32(i).unwrap_or('x')).collect();
    // (the Python test builds chr() for every value; surrogates cannot be in a
    // Rust String, so build the pattern from code points directly)
    let mut pv: Vec<u32> = vec!['[' as u32];
    pv.extend((256..65536u32).step_by(255));
    pv.push(']' as u32);
    let _ = r;
    let p = compile(PatternSource::Ucs4(&pv), 0).unwrap();
    assert!(p.match_at(&[0xff01u32], 0, usize::MAX).is_some());
}

#[test]
fn test_big_codesize() {
    let pat: Vec<String> = (0..10000).map(|x| x.to_string()).collect();
    let r = c(&pat.join("|"));
    assert!(r.match_at(&cp("1000"), 0, usize::MAX).is_some());
    assert!(r.match_at(&cp("9999"), 0, usize::MAX).is_some());
}

#[test]
fn test_anyall() {
    assert_eq!(re_match_f("a.b", "a\nb", DOTALL).unwrap().g0(), "a\nb");
    assert_eq!(re_match_f("a.*b", "a\n\nb", DOTALL).unwrap().g0(), "a\n\nb");
}

#[test]
fn test_lookahead() {
    let g1 = |p: &str, s: &str| re_match(p, s).unwrap().g(1);
    let a = Some("a".to_string());
    assert_eq!(g1(r"(a(?=\s[^a]))", "a b"), a);
    assert_eq!(g1(r"(a(?=\s[^a]*))", "a b"), a);
    assert_eq!(g1(r"(a(?=\s[abc]))", "a b"), a);
    assert_eq!(g1(r"(a(?=\s[abc]*))", "a bc"), a);
    assert_eq!(g1(r"(a)(?=\s\1)", "a a"), a);
    assert_eq!(g1(r"(a)(?=\s\1*)", "a aa"), a);
    assert_eq!(g1(r"(a)(?=\s(abc|a))", "a a"), a);
    assert_eq!(g1(r"(a(?!\s[^a]))", "a a"), a);
    assert_eq!(g1(r"(a(?!\s[abc]))", "a d"), a);
    assert_eq!(g1(r"(a)(?!\s\1)", "a b"), a);
    assert_eq!(g1(r"(a)(?!\s(abc|a))", "a b"), a);
    assert!(re_match(r"(a)b(?=\1)a", "aba").is_some());
    assert!(re_match(r"(a)b(?=\1)c", "abac").is_none());
    assert!(re_match(r"(?:(a)|(x))b(?=(?(2)x|c))c", "abc").is_some());
    assert!(re_match(r"(?:(a)|(x))b(?=(?(2)c|x))c", "abc").is_none());
    assert!(re_match(r"(?:(a)|(x))b(?=(?(2)x|c))c", "abc").is_some());
    assert!(re_match(r"(?:(a)|(x))b(?=(?(1)b|x))c", "abc").is_none());
    assert!(re_match(r"(?:(a)|(x))b(?=(?(1)c|x))c", "abc").is_some());
    assert!(re_match(r"(a)b(?=(?(2)x|c))(c)", "abc").is_some());
    assert!(re_match(r"(a)b(?=(?(2)b|x))(c)", "abc").is_none());
    assert!(re_match(r"(a)b(?=(?(1)c|x))(c)", "abc").is_some());
}

#[test]
fn test_lookbehind() {
    assert!(re_match(r"ab(?<=b)c", "abc").is_some());
    assert!(re_match(r"ab(?<=c)c", "abc").is_none());
    assert!(re_match(r"ab(?<!b)c", "abc").is_none());
    assert!(re_match(r"ab(?<!c)c", "abc").is_some());
    assert!(re_match(r"(a)a(?<=\1)c", "aac").is_some());
    assert!(re_match(r"(a)b(?<=\1)a", "abaa").is_none());
    assert!(re_match(r"(a)a(?<!\1)c", "aac").is_none());
    assert!(re_match(r"(a)b(?<!\1)a", "abaa").is_some());
    assert!(re_match(r"(?:(a)|(x))b(?<=(?(2)x|c))c", "abc").is_none());
    assert!(re_match(r"(?:(a)|(x))b(?<=(?(2)b|x))c", "abc").is_none());
    assert!(re_match(r"(?:(a)|(x))b(?<=(?(2)x|b))c", "abc").is_some());
    assert!(re_match(r"(?:(a)|(x))b(?<=(?(1)c|x))c", "abc").is_none());
    assert!(re_match(r"(?:(a)|(x))b(?<=(?(1)b|x))c", "abc").is_some());
    assert!(compile_str(r"(a)b(?<=(?(2)b|x))(c)", 0).is_err());
    assert!(re_match(r"(a)b(?<=(?(1)c|x))(c)", "abc").is_none());
    assert!(re_match(r"(a)b(?<=(?(1)b|x))(c)", "abc").is_some());
    assert!(compile_str(r"(a)b(?<=(.)\2)(c)", 0).is_err());
    assert!(compile_str(r"(a)b(?<=(?P<a>.)(?P=a))(c)", 0).is_err());
    assert!(compile_str(r"(a)b(?<=(a)(?(2)b|x))(c)", 0).is_err());
    assert!(compile_str(r"(a)b(?<=(.)(?<=\2))(c)", 0).is_err());
    // variable width
    assert_eq!(perr(r"(?<=a|bc)").0, "look-behind requires fixed-width pattern");
    assert!(re_search(r"(?<=ab|cd)x", "cdx").is_some());
}

#[test]
fn test_ignore_case() {
    assert_eq!(re_match_f("abc", "ABC", IGNORECASE).unwrap().g0(), "ABC");
    assert_eq!(b_match(b"abc", b"ABC", IGNORECASE).unwrap().spans[0], (0, 3));
    let g1 = |p: &str, s: &str| re_match_f(p, s, IGNORECASE).unwrap().g(1).unwrap();
    assert_eq!(g1(r"(a\s[^a])", "a b"), "a b");
    assert_eq!(g1(r"(a\s[^a]*)", "a bb"), "a bb");
    assert_eq!(g1(r"(a\s[abc])", "a b"), "a b");
    assert_eq!(g1(r"(a\s[abc]*)", "a bb"), "a bb");
    assert_eq!(g1(r"((a)\s\2)", "a a"), "a a");
    assert_eq!(g1(r"((a)\s\2*)", "a aa"), "a aa");
    assert_eq!(g1(r"((a)\s(abc|a))", "a a"), "a a");
    assert_eq!(g1(r"((a)\s(abc|a)*)", "a aa"), "a aa");
    let mi = |p: &str, s: &str| re_match_f(p, s, IGNORECASE).is_some();
    assert!(mi("K", "\u{212a}"));
    assert!(mi("k", "\u{212a}"));
    assert!(mi("\u{212a}", "K"));
    assert!(mi("\u{212a}", "k"));
    assert!(mi("S", "\u{17f}"));
    assert!(mi("s", "\u{17f}"));
    assert!(mi("\u{17f}", "S"));
    assert!(mi("\u{17f}", "s"));
    assert!(mi("\u{412}", "\u{432}"));
    assert!(mi("\u{412}", "\u{1c80}"));
    assert!(mi("\u{432}", "\u{412}"));
    assert!(mi("\u{432}", "\u{1c80}"));
    assert!(mi("\u{1c80}", "\u{412}"));
    assert!(mi("\u{1c80}", "\u{432}"));
    assert!(mi("\u{fb05}", "\u{fb06}"));
    assert!(mi("\u{fb06}", "\u{fb05}"));
}

#[test]
fn test_ignore_case_set() {
    let mi = |p: &str, s: &str, f: u32| re_match_f(p, s, IGNORECASE | f).is_some();
    for (p, s, f, exp) in [
        ("[19A]", "A", 0, true), ("[19a]", "a", 0, true), ("[19a]", "A", 0, true), ("[19A]", "a", 0, true),
        ("[19\u{c7}]", "\u{c7}", 0, true), ("[19\u{c7}]", "\u{e7}", 0, true), ("[19\u{e7}]", "\u{c7}", 0, true), ("[19\u{e7}]", "\u{e7}", 0, true),
        ("[19\u{400}]", "\u{400}", 0, true), ("[19\u{400}]", "\u{450}", 0, true), ("[19\u{450}]", "\u{400}", 0, true), ("[19\u{450}]", "\u{450}", 0, true),
        ("[19\u{10400}]", "\u{10400}", 0, true), ("[19\u{10400}]", "\u{10428}", 0, true), ("[19\u{10428}]", "\u{10400}", 0, true), ("[19\u{10428}]", "\u{10428}", 0, true),
        ("[19A]", "A", ASCII, true), ("[19a]", "a", ASCII, true), ("[19a]", "A", ASCII, true), ("[19A]", "a", ASCII, true),
        ("[19\u{c7}]", "\u{c7}", ASCII, true), ("[19\u{c7}]", "\u{e7}", ASCII, false), ("[19\u{e7}]", "\u{c7}", ASCII, false), ("[19\u{e7}]", "\u{e7}", ASCII, true),
        ("[19\u{400}]", "\u{400}", ASCII, true), ("[19\u{400}]", "\u{450}", ASCII, false), ("[19\u{450}]", "\u{400}", ASCII, false), ("[19\u{450}]", "\u{450}", ASCII, true),
        ("[19\u{10400}]", "\u{10400}", ASCII, true), ("[19\u{10400}]", "\u{10428}", ASCII, false), ("[19\u{10428}]", "\u{10400}", ASCII, false), ("[19\u{10428}]", "\u{10428}", ASCII, true),
        ("[19K]", "\u{212a}", 0, true), ("[19k]", "\u{212a}", 0, true), ("[19\u{212a}]", "K", 0, true), ("[19\u{212a}]", "k", 0, true),
        ("[19S]", "\u{17f}", 0, true), ("[19s]", "\u{17f}", 0, true), ("[19\u{17f}]", "S", 0, true), ("[19\u{17f}]", "s", 0, true),
        ("[19\u{412}]", "\u{432}", 0, true), ("[19\u{412}]", "\u{1c80}", 0, true), ("[19\u{432}]", "\u{412}", 0, true), ("[19\u{432}]", "\u{1c80}", 0, true),
        ("[19\u{1c80}]", "\u{412}", 0, true), ("[19\u{1c80}]", "\u{432}", 0, true),
        ("[19\u{fb05}]", "\u{fb06}", 0, true), ("[19\u{fb06}]", "\u{fb05}", 0, true),
    ] {
        assert_eq!(mi(p, s, f), exp, "{p:?} {s:?} {f}");
    }
    for (p, s) in [(&b"[19A]"[..], &b"A"[..]), (b"[19a]", b"a"), (b"[19a]", b"A"), (b"[19A]", b"a")] {
        assert!(b_match(p, s, IGNORECASE).is_some());
    }
}

#[test]
fn test_ignore_case_range() {
    let mi = |p: &str, s: &str, f: u32| re_match_f(p, s, IGNORECASE | f).is_some();
    for (p, s, f, exp) in [
        (r"[9-a]", "_", 0, true), (r"[9-A]", "_", 0, false),
        ("[\u{c0}-\u{de}]", "\u{d7}", 0, true), ("[\u{c0}-\u{de}]", "\u{e7}", 0, true), ("[\u{c0}-\u{de}]", "\u{f7}", 0, false),
        ("[\u{e0}-\u{fe}]", "\u{f7}", 0, true), ("[\u{e0}-\u{fe}]", "\u{c7}", 0, true), ("[\u{e0}-\u{fe}]", "\u{d7}", 0, false),
        ("[\u{430}-\u{45f}]", "\u{450}", 0, true), ("[\u{430}-\u{45f}]", "\u{400}", 0, true),
        ("[\u{400}-\u{42f}]", "\u{450}", 0, true), ("[\u{400}-\u{42f}]", "\u{400}", 0, true),
        ("[\u{10428}-\u{1044f}]", "\u{10428}", 0, true), ("[\u{10428}-\u{1044f}]", "\u{10400}", 0, true),
        ("[\u{10400}-\u{10427}]", "\u{10428}", 0, true), ("[\u{10400}-\u{10427}]", "\u{10400}", 0, true),
        ("[\u{c0}-\u{de}]", "\u{d7}", ASCII, true), ("[\u{c0}-\u{de}]", "\u{e7}", ASCII, false),
        ("[\u{e0}-\u{fe}]", "\u{f7}", ASCII, true), ("[\u{e0}-\u{fe}]", "\u{c7}", ASCII, false),
        ("[\u{430}-\u{45f}]", "\u{450}", ASCII, true), ("[\u{430}-\u{45f}]", "\u{400}", ASCII, false),
        ("[\u{400}-\u{42f}]", "\u{450}", ASCII, false), ("[\u{400}-\u{42f}]", "\u{400}", ASCII, true),
        ("[\u{10428}-\u{1044f}]", "\u{10428}", ASCII, true), ("[\u{10428}-\u{1044f}]", "\u{10400}", ASCII, false),
        ("[\u{10400}-\u{10427}]", "\u{10428}", ASCII, false), ("[\u{10400}-\u{10427}]", "\u{10400}", ASCII, true),
        (r"[N-\x7f]", "A", ASCII, true), (r"[n-\x7f]", "Z", ASCII, true),
        (r"[N-\uffff]", "A", ASCII, true), (r"[n-\uffff]", "Z", ASCII, true),
        (r"[N-\U00010000]", "A", ASCII, true), (r"[n-\U00010000]", "Z", ASCII, true),
        (r"[J-M]", "\u{212a}", 0, true), (r"[j-m]", "\u{212a}", 0, true),
        ("[\u{2129}-\u{212b}]", "K", 0, true), ("[\u{2129}-\u{212b}]", "k", 0, true),
        (r"[R-T]", "\u{17f}", 0, true), (r"[r-t]", "\u{17f}", 0, true),
        ("[\u{17e}-\u{180}]", "S", 0, true), ("[\u{17e}-\u{180}]", "s", 0, true),
        ("[\u{411}-\u{413}]", "\u{432}", 0, true), ("[\u{411}-\u{413}]", "\u{1c80}", 0, true),
        ("[\u{431}-\u{433}]", "\u{412}", 0, true), ("[\u{431}-\u{433}]", "\u{1c80}", 0, true),
        ("[\u{1c80}-\u{1c82}]", "\u{412}", 0, true), ("[\u{1c80}-\u{1c82}]", "\u{432}", 0, true),
        ("[\u{fb04}-\u{fb05}]", "\u{fb06}", 0, true), ("[\u{fb06}-\u{fb07}]", "\u{fb05}", 0, true),
    ] {
        assert_eq!(mi(p, s, f), exp, "{p:?} {s:?} {f}");
    }
    assert!(b_match(br"[9-a]", b"_", IGNORECASE).is_some());
    assert!(b_match(br"[9-A]", b"_", IGNORECASE).is_none());
}

#[test]
fn test_category_and_not_literal() {
    assert_eq!(re_match(r"(\s)", " ").unwrap().g(1), Some(" ".into()));
    assert_eq!(re_search(r"\s([^a])", " b").unwrap().g(1), Some("b".into()));
    assert_eq!(re_search(r"\s([^a]*)", " bb").unwrap().g(1), Some("bb".into()));
}

fn warns(p: &str) -> Vec<String> {
    c(p).warnings().iter().map(|w| {
        assert_eq!(w.kind, WarningKind::FutureWarning);
        w.message.clone()
    }).collect()
}

#[test]
fn test_possible_set_operations() {
    let s: String = (0u8..128).map(|b| b as char).collect();
    let fa = |p: &str| findall(p, &s).concat();
    assert!(warns(r"[0-9--1]")[0].starts_with("Possible set difference"));
    assert_eq!(fa(r"[0-9--1]"), "-./0123456789");
    assert!(warns(r"[0-9--2]")[0].starts_with("Possible set difference"));
    assert_eq!(fa(r"[0-9--2]"), "-./0123456789");
    assert_eq!(fa(r"[--1]"), "-./01");
    assert!(warns(r"[%--1]")[0].starts_with("Possible set difference"));
    assert_eq!(fa(r"[%--1]"), "%&'()*+,-1");
    assert!(warns(r"[%--]")[0].starts_with("Possible set difference "));
    assert_eq!(fa(r"[%--]"), "%&'()*+,-");
    assert!(warns(r"[0-9&&1]")[0].starts_with("Possible set intersection "));
    assert_eq!(fa(r"[0-9&&1]"), "&0123456789");
    assert_eq!(fa(r"[0-8&&1]"), "&012345678");
    assert!(warns(r"[\d&&1]")[0].starts_with("Possible set intersection "));
    assert_eq!(fa(r"[\d&&1]"), "&0123456789");
    assert_eq!(fa(r"[&&1]"), "&1");
    assert!(warns(r"[0-9||a]")[0].starts_with("Possible set union "));
    assert_eq!(fa(r"[0-9||a]"), "0123456789a|");
    assert!(warns(r"[\d||a]")[0].starts_with("Possible set union "));
    assert_eq!(fa(r"[\d||a]"), "0123456789a|");
    assert_eq!(fa(r"[||1]"), "1|");
    assert!(warns(r"[0-9~~1]")[0].starts_with("Possible set symmetric difference "));
    assert_eq!(fa(r"[0-9~~1]"), "0123456789~");
    assert!(warns(r"[\d~~1]")[0].starts_with("Possible set symmetric difference "));
    assert_eq!(fa(r"[\d~~1]"), "0123456789~");
    assert_eq!(fa(r"[~~1]"), "1~");
    assert!(warns(r"[[0-9]|]")[0].starts_with("Possible nested set "));
    assert_eq!(fa(r"[[0-9]|]"), "0123456789[]");
    assert_eq!(fa(r"[[0-8]|]"), "012345678[]");
    assert!(warns(r"[[:digit:]|]")[0].starts_with("Possible nested set "));
    assert_eq!(fa(r"[[:digit:]|]"), ":[]dgit");
    assert_eq!(warns(r"[0-9--1]")[0], "Possible set difference at position 4");
    assert_eq!(warns(r"[[0-9]|]")[0], "Possible nested set at position 1");
    assert!(warns(r"[--1]").is_empty());
}

#[test]
fn test_search_coverage() {
    assert_eq!(re_search(r"\s(b)", " b").unwrap().g(1), Some("b".into()));
    assert_eq!(re_search(r"a\s", "a ").unwrap().g0(), "a ");
}

/// re.escape as defined by CPython.
fn escape(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if "()[]{}?*+-|^$\\.&~# \t\n\r\x0b\x0c".contains(ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn assert_match(p: &str, text: &str) {
    let m = re_fullmatch(p, text).unwrap_or_else(|| panic!("{p:?} !~ {text:?}"));
    assert_eq!(m.g0(), text);
}

#[test]
fn test_re_escape() {
    let p: String = (0u32..256).map(|i| char::from_u32(i).unwrap()).collect();
    for ch in p.chars() {
        let s = ch.to_string();
        assert_match(&escape(&s), &s);
        assert_match(&format!("[{}]", escape(&s)), &s);
        assert_match(&format!("(?x){}", escape(&s)), &s);
    }
    assert_match(&escape(&p), &p);
    let s = "xxx\u{2620}\u{2620}\u{2620}xxx";
    assert_match(&escape(s), s);
    let m = re_search(&format!(".{}+.", escape("\u{2620}")), s).unwrap();
    assert_eq!(m.span(), (2, 7));
}

#[test]
fn test_re_escape_bytes() {
    for i in 0u8..=255 {
        let b = [i];
        let mut e = Vec::new();
        if b"()[]{}?*+-|^$\\.&~# \t\n\r\x0b\x0c".contains(&i) {
            e.push(b'\\');
        }
        e.push(i);
        assert!(b_fullmatch(&e, &b, 0).is_some(), "{i}");
        let mut set = vec![b'['];
        set.extend_from_slice(&e);
        set.push(b']');
        assert!(b_fullmatch(&set, &b, 0).is_some(), "[{i}]");
        let mut x = b"(?x)".to_vec();
        x.extend_from_slice(&e);
        assert!(b_fullmatch(&x, &b, 0).is_some(), "(?x){i}");
    }
}

#[test]
fn test_flags() {
    for f in [IGNORECASE, MULTILINE, VERBOSE, DOTALL, ASCII, UNICODE] {
        cf("^pattern$", f);
    }
    for f in [IGNORECASE, MULTILINE, VERBOSE, DOTALL, ASCII, LOCALE] {
        cb(b"^pattern$", f);
    }
}

#[test]
fn test_sre_character_literals() {
    for i in [0u32, 8, 16, 32, 64, 127, 128, 255, 256, 0xFFFF, 0x10000, 0x10FFFF] {
        let ch = char::from_u32(i).unwrap().to_string();
        if i < 256 {
            assert!(re_match(&format!(r"\{i:03o}"), &ch).is_some());
            assert!(re_match(&format!(r"\{i:03o}0"), &format!("{ch}0")).is_some());
            assert!(re_match(&format!(r"\{i:03o}8"), &format!("{ch}8")).is_some());
            assert!(re_match(&format!(r"\x{i:02x}"), &ch).is_some());
            assert!(re_match(&format!(r"\x{i:02x}0"), &format!("{ch}0")).is_some());
            assert!(re_match(&format!(r"\x{i:02x}z"), &format!("{ch}z")).is_some());
        }
        if i < 0x10000 {
            assert!(re_match(&format!(r"\u{i:04x}"), &ch).is_some());
            assert!(re_match(&format!(r"\u{i:04x}0"), &format!("{ch}0")).is_some());
            assert!(re_match(&format!(r"\u{i:04x}z"), &format!("{ch}z")).is_some());
        }
        assert!(re_match(&format!(r"\U{i:08x}"), &ch).is_some());
        assert!(re_match(&format!(r"\U{i:08x}0"), &format!("{ch}0")).is_some());
        assert!(re_match(&format!(r"\U{i:08x}z"), &format!("{ch}z")).is_some());
    }
    assert!(re_match(r"\0", "\0").is_some());
    assert!(re_match(r"\08", "\x008").is_some());
    assert!(re_match(r"\01", "\x01").is_some());
    assert!(re_match(r"\018", "\x018").is_some());
    check_pattern_error(r"\567", r"octal escape value \567 outside of range 0-0o377", Some(0));
    check_pattern_error(r"\911", "invalid group reference 91", Some(1));
    check_pattern_error(r"\x1", r"incomplete escape \x1", Some(0));
    check_pattern_error(r"\x1z", r"incomplete escape \x1", Some(0));
    check_pattern_error(r"\u123", r"incomplete escape \u123", Some(0));
    check_pattern_error(r"\u123z", r"incomplete escape \u123", Some(0));
    check_pattern_error(r"\U0001234", r"incomplete escape \U0001234", Some(0));
    check_pattern_error(r"\U0001234z", r"incomplete escape \U0001234", Some(0));
    check_pattern_error(r"\U00110000", r"bad escape \U00110000", Some(0));
}

#[test]
fn test_sre_character_class_literals() {
    for i in [0u32, 8, 16, 32, 64, 127, 128, 255, 256, 0xFFFF, 0x10000, 0x10FFFF] {
        let ch = char::from_u32(i).unwrap().to_string();
        if i < 256 {
            assert!(re_match(&format!(r"[\{i:o}]"), &ch).is_some());
            assert!(re_match(&format!(r"[\{i:o}8]"), &ch).is_some());
            assert!(re_match(&format!(r"[\{i:03o}]"), &ch).is_some());
            assert!(re_match(&format!(r"[\{i:03o}0]"), &ch).is_some());
            assert!(re_match(&format!(r"[\{i:03o}8]"), &ch).is_some());
            assert!(re_match(&format!(r"[\x{i:02x}]"), &ch).is_some());
            assert!(re_match(&format!(r"[\x{i:02x}0]"), &ch).is_some());
            assert!(re_match(&format!(r"[\x{i:02x}z]"), &ch).is_some());
        }
        if i < 0x10000 {
            assert!(re_match(&format!(r"[\u{i:04x}]"), &ch).is_some());
            assert!(re_match(&format!(r"[\u{i:04x}0]"), &ch).is_some());
            assert!(re_match(&format!(r"[\u{i:04x}z]"), &ch).is_some());
        }
        assert!(re_match(&format!(r"[\U{i:08x}]"), &ch).is_some());
        assert!(re_match(&format!(r"[\U{i:08x}0]"), &format!("{ch}0")).is_some());
        assert!(re_match(&format!(r"[\U{i:08x}z]"), &format!("{ch}z")).is_some());
    }
    check_pattern_error(r"[\567]", r"octal escape value \567 outside of range 0-0o377", Some(1));
    check_pattern_error(r"[\911]", r"bad escape \9", Some(1));
    check_pattern_error(r"[\x1z]", r"incomplete escape \x1", Some(1));
    check_pattern_error(r"[\u123z]", r"incomplete escape \u123", Some(1));
    check_pattern_error(r"[\U0001234z]", r"incomplete escape \U0001234", Some(1));
    check_pattern_error(r"[\U00110000]", r"bad escape \U00110000", Some(1));
    assert!(re_match(r"[\U0001d49c-\U0001d4b5]", "\u{1d49e}").is_some());
}

#[test]
fn test_sre_byte_literals() {
    for i in [0u8, 8, 16, 32, 64, 127, 128, 255] {
        let m = |p: String, s: Vec<u8>| assert!(b_match(p.as_bytes(), &s, 0).is_some(), "{p}");
        m(format!(r"\{i:03o}"), vec![i]);
        m(format!(r"\{i:03o}0"), vec![i, b'0']);
        m(format!(r"\{i:03o}8"), vec![i, b'8']);
        m(format!(r"\x{i:02x}"), vec![i]);
        m(format!(r"\x{i:02x}0"), vec![i, b'0']);
        m(format!(r"\x{i:02x}z"), vec![i, b'z']);
        m(format!(r"[\{i:o}]"), vec![i]);
        m(format!(r"[\{i:o}8]"), vec![i]);
        m(format!(r"[\{i:03o}]"), vec![i]);
        m(format!(r"[\{i:03o}0]"), vec![i]);
        m(format!(r"[\{i:03o}8]"), vec![i]);
        m(format!(r"[\x{i:02x}]"), vec![i]);
        m(format!(r"[\x{i:02x}0]"), vec![i]);
        m(format!(r"[\x{i:02x}z]"), vec![i]);
    }
    assert!(compile_bytes(br"\u1234", 0).is_err());
    assert!(compile_bytes(br"\U00012345", 0).is_err());
    assert!(compile_bytes(br"[\u1234]", 0).is_err());
    assert!(compile_bytes(br"[\U00012345]", 0).is_err());
    assert!(b_match(br"\0", b"\x00", 0).is_some());
    assert!(b_match(br"\08", b"\x008", 0).is_some());
    assert!(b_match(br"\01", b"\x01", 0).is_some());
    assert!(b_match(br"\018", b"\x018", 0).is_some());
    check_pattern_error_b(br"\567", r"octal escape value \567 outside of range 0-0o377", Some(0));
    check_pattern_error_b(br"\911", "invalid group reference 91", Some(1));
    check_pattern_error_b(br"\x1", r"incomplete escape \x1", Some(0));
    check_pattern_error_b(br"\x1z", r"incomplete escape \x1", Some(0));
    check_pattern_error_b(br"[\567]", r"octal escape value \567 outside of range 0-0o377", Some(1));
    check_pattern_error_b(br"[\911]", r"bad escape \9", Some(1));
    check_pattern_error_b(br"[\x1z]", r"incomplete escape \x1", Some(1));
}

#[test]
fn test_character_set_errors() {
    check_pattern_error(r"[", "unterminated character set", Some(0));
    check_pattern_error(r"[^", "unterminated character set", Some(0));
    check_pattern_error(r"[a", "unterminated character set", Some(0));
    check_pattern_error(r"[a-", "unterminated character set", Some(0));
    check_pattern_error(r"[\w-b]", r"bad character range \w-b", Some(1));
    check_pattern_error(r"[a-\w]", r"bad character range a-\w", Some(1));
    check_pattern_error(r"[b-a]", "bad character range b-a", Some(1));
}

#[test]
fn test_bug_113254() {
    let m = re_match(r"(a)|(b)", "b").unwrap();
    assert_eq!(m.span_g(1), (-1, -1));
}

#[test]
fn test_bug_527371() {
    assert_eq!(re_match(r"(a)?a", "a").unwrap().lastindex(), None);
    assert_eq!(re_match(r"(a)(b)?b", "ab").unwrap().lastindex(), Some(1));
    assert_eq!(re_match(r"(?P<a>a)(?P<b>b)?b", "ab").unwrap().lastgroup(), Some("a".into()));
    assert_eq!(re_match(r"(?P<a>a(b))", "ab").unwrap().lastgroup(), Some("a".into()));
    assert_eq!(re_match(r"((a))", "a").unwrap().lastindex(), Some(1));
}

#[test]
fn test_bug_418626() {
    assert_eq!(re_match(".*?c", &("ab".repeat(10000) + "cd")).unwrap().span().1, 20001);
    assert_eq!(re_match(".*?cd", &("ab".repeat(5000) + "c" + &"ab".repeat(5000) + "cde")).unwrap().span().1, 20003);
    assert_eq!(re_match(".*?cd", &("abc".repeat(20000) + "de")).unwrap().span().1, 60001);
    assert_eq!(re_search("(a|b)*?c", &("ab".repeat(10000) + "cd")).unwrap().span().1, 20001);
}

#[test]
fn test_bug_612074() {
    c(&format!("[{}]", escape("\u{2039}")));
}

#[test]
fn test_stack_overflow() {
    assert_eq!(re_match("(x)*", &"x".repeat(50000)).unwrap().g(1), Some("x".into()));
    assert_eq!(re_match("(x)*y", &("x".repeat(50000) + "y")).unwrap().g(1), Some("x".into()));
    assert_eq!(re_match("(x)*?y", &("x".repeat(50000) + "y")).unwrap().g(1), Some("x".into()));
}

#[test]
fn test_nothing_to_repeat() {
    for reps in ["*", "+", "?", "{1,2}"] {
        for m in ["", "?"] {
            check_pattern_error(&format!("{reps}{m}"), "nothing to repeat", Some(0));
            check_pattern_error(&format!("(?:{reps}{m})"), "nothing to repeat", Some(3));
        }
    }
}

#[test]
fn test_multiple_repeat() {
    for outer_reps in ["*", "+", "?", "{1,2}"] {
        for outer_mod in ["", "?", "+"] {
            let outer_op = format!("{outer_reps}{outer_mod}");
            for inner_reps in ["*", "+", "?", "{1,2}"] {
                for inner_mod in ["", "?", "+"] {
                    let x = format!("{inner_mod}{outer_reps}");
                    if x == "?" || x == "+" {
                        continue;
                    }
                    let inner_op = format!("{inner_reps}{inner_mod}");
                    check_pattern_error(&format!("x{inner_op}{outer_op}"), "multiple repeat", Some(1 + inner_op.len()));
                }
            }
        }
    }
}

#[test]
fn test_unlimited_zero_width_repeat() {
    assert!(re_match(r"(?:a?)*y", "z").is_none());
    assert!(re_match(r"(?:a?)+y", "z").is_none());
    assert!(re_match(r"(?:a?){2,}y", "z").is_none());
    assert!(re_match(r"(?:a?)*?y", "z").is_none());
    assert!(re_match(r"(?:a?)+?y", "z").is_none());
    assert!(re_match(r"(?:a?){2,}?y", "z").is_none());
}

#[test]
fn test_scanner_like() {
    // re.Scanner equivalent: alternation of capturing groups + lastindex
    let p = c(r"([a-zA-Z_]\w*)|(\d+\.\d*)|(\d+)|(=|\+|-|\*|/)|(\s+)");
    let s = cp("sum = 3*foo + 312.50 + bar");
    let mut pos = 0;
    let mut out = Vec::new();
    while let Some(m) = p.match_at(&s, pos, usize::MAX) {
        let e = m.end();
        if e == pos {
            break;
        }
        let li = m.lastindex.unwrap();
        if li != 5 {
            out.push(format!("{li}:{}", st(&s[pos..e])));
        }
        pos = e;
    }
    assert_eq!(out, vec!["1:sum", "4:=", "3:3", "4:*", "1:foo", "4:+", "2:312.50", "4:+", "1:bar"]);
    assert_eq!(pos, s.len());
}

#[test]
fn test_bug_448951() {
    let sm = |x: &str| Some(x.to_string());
    for op in ["", "?", "*"] {
        let p = format!(r"((.{op}):)?z");
        assert_eq!(re_match(&p, "z").unwrap().groups(), vec![None, None]);
        assert_eq!(re_match(&p, "a:z").unwrap().groups(), vec![sm("a:"), sm("a")]);
    }
}

#[test]
fn test_bug_725106() {
    let sm = |x: &str| Some(x.to_string());
    assert_eq!(re_match("^((a)|b)*", "abc").unwrap().groups(), vec![sm("b"), sm("a")]);
    assert_eq!(re_match("^(([ab])|c)*", "abc").unwrap().groups(), vec![sm("c"), sm("b")]);
    assert_eq!(re_match("^((d)|[ab])*", "abc").unwrap().groups(), vec![sm("b"), None]);
    assert_eq!(re_match("^((a)c|[ab])*", "abc").unwrap().groups(), vec![sm("b"), None]);
    assert_eq!(re_match("^((a)|b)*?c", "abc").unwrap().groups(), vec![sm("b"), sm("a")]);
    assert_eq!(re_match("^(([ab])|c)*?d", "abcd").unwrap().groups(), vec![sm("c"), sm("b")]);
    assert_eq!(re_match("^((d)|[ab])*?c", "abc").unwrap().groups(), vec![sm("b"), None]);
    assert_eq!(re_match("^((a)c|[ab])*?c", "abc").unwrap().groups(), vec![sm("b"), None]);
}

#[test]
fn test_bug_725149() {
    let sm = |x: &str| Some(x.to_string());
    assert_eq!(re_match("(a)(?:(?=(b)*)c)*", "abb").unwrap().groups(), vec![sm("a"), None]);
    assert_eq!(re_match("(a)((?!(b)*))*", "abb").unwrap().groups(), vec![sm("a"), None, None]);
}

#[test]
fn test_finditer() {
    let p = c(r":+");
    let s = cp("a:b::c:::d");
    let g = |pos, end| -> Vec<String> {
        p.scanner(&s, pos, end).map(|m| group_of(&s, &m, 0).unwrap()).collect()
    };
    assert_eq!(g(0, usize::MAX), strs(&[":", "::", ":::"]));
    assert_eq!(g(1, 10), strs(&[":", "::", ":::"]));
    assert_eq!(g(3, 8), strs(&["::", "::"]));
}

#[test]
fn test_bug_926075_931848() {
    assert_eq!(split("[\u{2e}\u{3002}\u{ff0e}\u{ff61}]", "a.b.c"), somes(&["a", "b", "c"]));
}

#[test]
fn test_bug_581080() {
    assert_eq!(finditer_spans(r"\s", "a b"), vec![(1, 2)]);
    let p = c(r"\s");
    let s = cp("a b");
    let mut sc = p.scanner(&s, 0, usize::MAX);
    assert_eq!(sc.search().unwrap().spans[0], (1, 2));
    assert!(sc.search().is_none());
}

#[test]
fn test_bug_817234() {
    assert_eq!(finditer_spans(r".*", "asdf"), vec![(0, 4), (4, 4)]);
}

#[test]
fn test_bug_6561() {
    for x in ["\u{37}", "\u{e58}", "\u{ff10}"] {
        assert_eq!(re_match(r"^\d$", x).unwrap().g0(), x);
    }
    for x in ["\u{2165}", "\u{3039}", "\u{2082}", "\u{32b4}"] {
        assert!(re_match(r"^\d$", x).is_none(), "{x}");
    }
}

#[test]
fn test_empty_array() {
    let e: [u8; 0] = [];
    assert!(cb(b"bla", 0).match_at(&e, 0, usize::MAX).is_none());
    assert_eq!(cb(b"", 0).match_at(&e, 0, usize::MAX).unwrap().spans, vec![(0, 0)]);
}

#[test]
fn test_inline_flags() {
    let upper = "\u{1ea0}";
    let lower = "\u{1ea1}";
    assert!(re_match_f(&format!(".{upper}"), &format!("\n{lower}"), IGNORECASE | DOTALL).is_some());
    assert!(re_match_f(&format!(".{lower}"), &format!("\n{upper}"), IGNORECASE | DOTALL).is_some());
    assert!(re_match_f(&format!("(?i).{upper}"), &format!("\n{lower}"), DOTALL).is_some());
    assert!(re_match_f(&format!("(?i).{lower}"), &format!("\n{upper}"), DOTALL).is_some());
    assert!(re_match(&format!("(?is).{upper}"), &format!("\n{lower}")).is_some());
    assert!(re_match(&format!("(?is).{lower}"), &format!("\n{upper}")).is_some());
    assert!(re_match(&format!("(?s)(?i).{upper}"), &format!("\n{lower}")).is_some());
    assert!(re_match(&format!("(?s)(?i).{lower}"), &format!("\n{upper}")).is_some());
    assert!(re_match(&format!("(?ix) {upper}"), lower).is_some());
    assert!(re_match(&format!("(?ix) {lower}"), upper).is_some());
    assert!(re_match_f(&format!(" (?i) {upper}"), lower, VERBOSE).is_some());
    assert!(re_match(&format!("(?x) (?i) {upper}"), lower).is_some());
    assert!(re_match_f(&format!(" (?x) (?i) {upper}"), lower, VERBOSE).is_some());

    let msg = "global flags not at the start of the expression";
    check_pattern_error(&format!("{upper}(?i)"), msg, Some(1));
    check_pattern_error_b(b"A(?i)", msg, Some(1));
    check_pattern_error(&format!("(?s).(?i){upper}"), msg, Some(5));
    check_pattern_error(&format!("(?i) {upper} (?x)"), msg, Some(7));
    check_pattern_error(&format!(" (?x) (?i) {upper}"), msg, Some(1));
    check_pattern_error(&format!("^(?i){upper}"), msg, Some(1));
    check_pattern_error(&format!("$|(?i){upper}"), msg, Some(2));
    check_pattern_error(&format!("(?:(?i){upper})"), msg, Some(3));
    check_pattern_error(&format!("(^)?(?(1)(?i){upper})"), msg, Some(9));
    check_pattern_error(&format!("($)?(?(1)|(?i){upper})"), msg, Some(10));
}

#[test]
fn test_dollar_matches_twice() {
    let p = c("$");
    assert_eq!(subn_pat(&p, "#", "a\nb\n", 0).unwrap().0, "a\nb#\n#");
    assert_eq!(subn_pat(&p, "#", "a\nb\nc", 0).unwrap().0, "a\nb\nc#");
    assert_eq!(subn_pat(&p, "#", "\n", 0).unwrap().0, "#\n#");
    let p = cf("$", MULTILINE);
    assert_eq!(subn_pat(&p, "#", "a\nb\n", 0).unwrap().0, "a#\nb#\n#");
    assert_eq!(subn_pat(&p, "#", "a\nb\nc", 0).unwrap().0, "a#\nb#\nc#");
    assert_eq!(subn_pat(&p, "#", "\n", 0).unwrap().0, "#\n#");
}

fn is_value_error(r: Result<Pattern, CompileError>) -> bool {
    matches!(r, Err(CompileError::Value(_)))
}
fn is_pattern_error(r: Result<Pattern, CompileError>) -> bool {
    matches!(r, Err(CompileError::Pattern { .. }))
}

#[test]
fn test_ascii_and_unicode_flag() {
    for flags in [0, UNICODE] {
        assert!(re_match_f("\u{c0}", "\u{e0}", flags | IGNORECASE).is_some());
        assert!(re_match_f(r"\w", "\u{e0}", flags).is_some());
    }
    assert!(re_match_f("\u{c0}", "\u{e0}", ASCII | IGNORECASE).is_none());
    assert!(re_match_f("(?a)\u{c0}", "\u{e0}", IGNORECASE).is_none());
    assert!(re_match_f(r"\w", "\u{e0}", ASCII).is_none());
    assert!(re_match(r"(?a)\w", "\u{e0}").is_none());
    for flags in [0, ASCII] {
        assert!(b_match(b"\xc0", b"\xe0", flags | IGNORECASE).is_none());
        assert!(b_match(br"\w", b"\xe0", flags).is_none());
    }
    assert!(is_value_error(compile_bytes(br"\w", UNICODE)));
    assert!(is_pattern_error(compile_bytes(br"(?u)\w", 0)));
    assert!(is_value_error(compile_str(r"\w", UNICODE | ASCII)));
    assert!(is_value_error(compile_str(r"(?u)\w", ASCII)));
    assert!(is_value_error(compile_str(r"(?a)\w", UNICODE)));
    assert!(is_pattern_error(compile_str(r"(?au)\w", 0)));
}

#[test]
fn test_locale_flag_errors() {
    assert!(is_value_error(compile_str("", LOCALE)));
    assert!(is_pattern_error(compile_str("(?L)", 0)));
    assert!(is_value_error(compile_bytes(b"", LOCALE | ASCII)));
    assert!(is_value_error(compile_bytes(b"(?L)", ASCII)));
    assert!(is_value_error(compile_bytes(b"(?a)", LOCALE)));
    assert!(is_pattern_error(compile_bytes(b"(?aL)", 0)));
}

#[test]
fn test_locale_ignorecase_negated_set() {
    let il = LOCALE | IGNORECASE;
    let fm = |p: &[u8], s: &[u8]| b_fullmatch(p, s, il).is_some();
    assert!(fm(br"[bc]", b"B"));
    assert!(!fm(br"[^bc]", b"B"));
    assert!(!fm(br"[^b-c]", b"C"));
    assert!(!fm(br"[^bc]", b"c"));
    assert!(fm(br"[^bc]", b"a"));
    assert!(!fm(br"[^b]", b"B"));
    assert!(fm(br"[^b]", b"a"));
    assert!(!fm(br"[^\wq]", b"Q"));
    assert!(fm(br"[ace]", b"C"));
    assert!(!fm(br"[^ace]", b"C"));
    assert!(fm(br"[^ace]", b"b"));
    assert!(!fm(br"(?:a|[^bc])", b"B"));
    assert!(fm(br"(?:a|[^bc])", b"A"));
    assert!(!fm(br"\w(?<!b)", b"B"));
    assert!(fm(br"\w(?<!b)", b"A"));
}

#[test]
fn test_scoped_flags() {
    assert!(re_match(r"(?i:a)b", "Ab").is_some());
    assert!(re_match(r"(?i:a)b", "aB").is_none());
    assert!(re_match_f(r"(?-i:a)b", "Ab", IGNORECASE).is_none());
    assert!(re_match_f(r"(?-i:a)b", "aB", IGNORECASE).is_some());
    assert!(re_match(r"(?i:(?-i:a)b)", "Ab").is_none());
    assert!(re_match(r"(?i:(?-i:a)b)", "aB").is_some());
    assert!(re_match(r"\w(?a:\W)\w", "\u{e0}\u{e0}\u{e0}").is_some());
    assert!(re_match(r"(?a:\W(?u:\w)\W)", "\u{e0}\u{e0}\u{e0}").is_some());
    assert!(re_match_f(r"\W(?u:\w)\W", "\u{e0}\u{e0}\u{e0}", ASCII).is_some());

    check_pattern_error(r"(?a)(?-a:\w)", "bad inline flags: cannot turn off flags 'a', 'u' and 'L'", Some(8));
    check_pattern_error(r"(?i-i:a)", "bad inline flags: flag turned on and off", Some(5));
    check_pattern_error(r"(?au:a)", "bad inline flags: flags 'a', 'u' and 'L' are incompatible", Some(4));
    check_pattern_error_b(br"(?aL:a)", "bad inline flags: flags 'a', 'u' and 'L' are incompatible", Some(4));
    check_pattern_error(r"(?-", "missing flag", Some(3));
    check_pattern_error(r"(?-+", "missing flag", Some(3));
    check_pattern_error(r"(?-z", "unknown flag", Some(3));
    check_pattern_error(r"(?-i", "missing :", Some(4));
    check_pattern_error(r"(?-i)", "missing :", Some(4));
    check_pattern_error(r"(?-i+", "missing :", Some(4));
    check_pattern_error(r"(?-iz", "unknown flag", Some(4));
    check_pattern_error(r"(?i:", "missing ), unterminated subpattern", Some(0));
    check_pattern_error(r"(?i", "missing -, : or )", Some(3));
    check_pattern_error(r"(?i+", "missing -, : or )", Some(3));
    check_pattern_error(r"(?iz", "unknown flag", Some(3));
}

#[test]
fn test_ignore_spaces() {
    for space in [" ", "\t", "\n", "\r", "\x0b", "\x0c"] {
        assert!(re_fullmatch_f(&format!("{space}a"), "a", VERBOSE).is_some());
        assert!(b_fullmatch(format!("{space}a").as_bytes(), b"a", VERBOSE).is_some());
    }
    assert!(re_fullmatch("(?x) a", "a").is_some());
    assert!(re_fullmatch_f(" (?x) a", "a", VERBOSE).is_some());
    assert!(re_fullmatch("(?x) (?x) a", "a").is_some());
    assert!(re_fullmatch(" a(?x: b) c", " ab c").is_some());
    assert!(re_fullmatch_f(" a(?-x: b) c", "a bc", VERBOSE).is_some());
    assert!(re_fullmatch("(?x) a(?-x: b) c", "a bc").is_some());
    assert!(re_fullmatch("(?x) a| b", "a").is_some());
    assert!(re_fullmatch("(?x) a| b", "b").is_some());
}

#[test]
fn test_comments() {
    assert!(re_fullmatch_f("#x\na", "a", VERBOSE).is_some());
    assert!(b_fullmatch(b"#x\na", b"a", VERBOSE).is_some());
    assert!(re_fullmatch("(?x)#x\na", "a").is_some());
    assert!(re_fullmatch_f("#x\n(?x)#y\na", "a", VERBOSE).is_some());
    assert!(re_fullmatch("(?x)#x\n(?x)#y\na", "a").is_some());
    assert!(re_fullmatch("#x\na(?x:#y\nb)#z\nc", "#x\nab#z\nc").is_some());
    assert!(re_fullmatch_f("#x\na(?-x:#y\nb)#z\nc", "a#y\nbc", VERBOSE).is_some());
    assert!(re_fullmatch("(?x)#x\na(?-x:#y\nb)#z\nc", "a#y\nbc").is_some());
    assert!(re_fullmatch("(?x)#x\na|#y\nb", "a").is_some());
    assert!(re_fullmatch("(?x)#x\na|#y\nb", "b").is_some());
}

#[test]
fn test_bug_6509() {
    assert_eq!(subn_pat(&c(r"a(\w)"), "b\\1", "ac", 0).unwrap().0, "bc");
    assert_eq!(subn_pat(&c("a(.)"), "b\\1", "a\u{1234}", 0).unwrap().0, "b\u{1234}");
}

#[test]
fn test_search_dot_unicode() {
    for s in ["123abc-", "123\u{e9}-", "123\u{20ac}-", "123\u{10ffff}-", "123\u{e9}\u{20ac}\u{10ffff}-"] {
        assert!(re_search("123.*-", s).is_some());
    }
}

#[test]
fn test_bug_16688() {
    assert_eq!(findall(r"(?i)(a)\1", "aa \u{100}"), strs(&["a"]));
    assert_eq!(re_match(r"(?s).{1,3}", "\u{100}\u{100}").unwrap().span(), (0, 2));
}

#[test]
fn test_repeat_minmax_overflow() {
    let string = "x".repeat(100000);
    assert_eq!(re_match(r".{65535}", &string).unwrap().span(), (0, 65535));
    assert_eq!(re_match(r".{,65535}", &string).unwrap().span(), (0, 65535));
    assert_eq!(re_match(r".{65535,}?", &string).unwrap().span(), (0, 65535));
    assert_eq!(re_match(r".{65536}", &string).unwrap().span(), (0, 65536));
    assert_eq!(re_match(r".{,65536}", &string).unwrap().span(), (0, 65536));
    assert_eq!(re_match(r".{65536,}?", &string).unwrap().span(), (0, 65536));
    let big = "340282366920938463463374607431768211456"; // 2**128
    let big2 = "680564733841876926926749214863536422912"; // 2**129
    for p in [format!(".{{{big}}}"), format!(".{{,{big}}}"), format!(".{{{big},}}?"), format!(".{{{big2},{big}}}")] {
        assert!(matches!(compile_str(&p, 0), Err(CompileError::Overflow(_))), "{p}");
    }
    // MAXREPEAT
    assert!(re_match(&format!(r".{{{}}}", MAXREPEAT - 1), &string).is_none());
    assert_eq!(re_match(&format!(r".{{,{}}}", MAXREPEAT - 1), &string).unwrap().span(), (0, 100000));
    assert!(re_match(&format!(r".{{{},}}?", MAXREPEAT - 1), &string).is_none());
    assert!(matches!(compile_str(&format!(r".{{{}}}", MAXREPEAT), 0), Err(CompileError::Overflow(_))));
    assert!(matches!(compile_str(&format!(r".{{,{}}}", MAXREPEAT), 0), Err(CompileError::Overflow(_))));
    assert!(matches!(compile_str(&format!(r".{{{},}}?", MAXREPEAT), 0), Err(CompileError::Overflow(_))));
}

#[test]
fn test_look_behind_overflow() {
    let string = "x".repeat(2_500_000);
    let p1 = |a: u64, b: u64, c: u64| format!(r"(?<=((.{{{a}}}){{{b}}}){{{c}}})");
    let p2 = |a: u64, b: u64, c: u64| format!(r"(?<!((.{{{a}}}){{{b}}}){{{c}}})");
    assert_eq!(re_search(&p1(128, 128, 128), &string).unwrap().span(), (1 << 21, 1 << 21));
    assert_eq!(re_search(&p2(128, 128, 128), &string).unwrap().span(), (0, 0));
    let big = 1u64 << 22;
    for p in [p1(big, 1, 1), p1(1, big, 1), p1(1, 1, big), p2(big, 1, 1), p2(1, big, 1), p2(1, 1, big)] {
        c(&p);
    }
    assert_eq!(perr(&p1(big, big, big)).0, "looks too much behind");
    assert_eq!(perr(&p2(big, big, big)).0, "looks too much behind");
}

#[test]
fn test_backref_group_name_in_exception() {
    check_pattern_error("(?P=<foo>)", "bad character in group name '<foo>'", Some(4));
    check_pattern_error("(?P<?foo>)", "bad character in group name '?foo'", Some(4));
}

#[test]
fn test_issue17998() {
    for reps in ["*", "+", "?", "{1}"] {
        for m in ["", "?"] {
            let pattern = format!(".{reps}{m}yz");
            assert_eq!(findall_f(&pattern, "xyz", DOTALL), strs(&["xyz"]), "{pattern}");
            let pat = cb(pattern.as_bytes(), DOTALL);
            let r: Vec<_> = pat.scanner(&b"xyz"[..], 0, usize::MAX).map(|m| m.spans[0]).collect();
            assert_eq!(r, vec![(0, 3)]);
        }
    }
}

#[test]
fn test_match_repr_like() {
    let m = re_search(r"(.+)(.*?)\1", "[abracadabra]").unwrap();
    assert_eq!(m.span(), (1, 12));
    let v: Vec<_> = finditer_spans("(aa)|(bb)", "aa bb");
    assert_eq!(v, vec![(0, 2), (3, 5)]);
}

#[test]
fn test_zerowidth() {
    let n = None::<String>;
    let _ = n;
    assert_eq!(split(r"\b", "a::bc"), somes(&["", "a", "::", "bc", ""]));
    assert_eq!(split(r"\b|:+", "a::bc"), somes(&["", "a", "", "", "bc", ""]));
    assert_eq!(split(r"(?<!\w)(?=\w)|:+", "a::bc"), somes(&["", "a", "", "bc"]));
    assert_eq!(split(r"(?<=\w)(?!\w)|:+", "a::bc"), somes(&["a", "", "bc", ""]));
    assert_eq!(sub(r"\b", "-", "a::bc"), "-a-::-bc-");
    assert_eq!(sub(r"\b|:+", "-", "a::bc"), "-a---bc-");
    assert_eq!(sub(r"(\b|:+)", r"[\1]", "a::bc"), "[]a[][::][]bc[]");
    assert_eq!(findall(r"\b|:+", "a::bc"), strs(&["", "", "::", "", ""]));
    assert_eq!(findall(r"\b|\w+", "a::bc"), strs(&["", "a", "", "", "bc", ""]));
    assert_eq!(finditer_spans(r"\b|:+", "a::bc"), vec![(0, 0), (1, 1), (1, 3), (3, 3), (5, 5)]);
    assert_eq!(finditer_spans(r"\b|\w+", "a::bc"), vec![(0, 0), (0, 1), (1, 1), (3, 3), (3, 5), (5, 5)]);
}

#[test]
fn test_bug_2537() {
    for outer_op in ["{0,}", "*", "+", "{1,187}"] {
        for inner_op in ["{0,}", "*", "?"] {
            let p = format!("^((x|y){inner_op}){outer_op}");
            let m = re_match(&p, "xyyzy").unwrap();
            assert_eq!(m.g0(), "xyy", "{p}");
            assert_eq!(m.g(1), Some("".into()), "{p}");
            assert_eq!(m.g(2), Some("y".into()), "{p}");
        }
    }
}

#[test]
fn test_keyword_parameters() {
    let pat = c(r"(ab)");
    let s = cp("abracadabra");
    assert_eq!(pat.match_at(&s, 7, 10).unwrap().spans[0], (7, 9));
    assert_eq!(pat.fullmatch(&s, 7, 9).unwrap().spans[0], (7, 9));
    assert_eq!(pat.search(&s, 3, 10).unwrap().spans[0], (7, 9));
    assert_eq!(findall_pos(&pat, "abracadabra", 3, 10), vec![strs(&["ab"])]);
    assert_eq!(split_pat(&pat, "abracadabra", 1), somes(&["", "ab", "racadabra"]));
    assert_eq!(pat.scanner(&s, 3, 10).search().unwrap().spans[0], (7, 9));
}

#[test]
fn test_bug_20998() {
    assert_eq!(re_fullmatch_f("[a-c]+", "ABC", IGNORECASE).unwrap().span(), (0, 3));
}

#[test]
fn test_error_positions() {
    let (_, pos) = perr("(\u{20ac}))");
    assert_eq!(pos, Some(3));
    let pat = cp("(\u{20ac}))");
    assert_eq!(line_col(&pat, 3), (1, 4));
    match compile_bytes(b"(\xa4))", 0) {
        Err(CompileError::Pattern { pos, .. }) => assert_eq!(pos, Some(3)),
        _ => panic!(),
    }
    let p = "\n                (\n                    abc\n                )\n                )\n                (\n                ";
    let (msg, pos) = perr_f(p, VERBOSE);
    assert_eq!(msg, "unbalanced parenthesis");
    assert_eq!(pos, Some(77));
    assert_eq!(line_col(&cp(p), 77), (5, 17));
    assert_eq!(
        format_error_message(&cp(p), &msg, pos),
        "unbalanced parenthesis at position 77 (line 5, column 17)"
    );
}

#[test]
fn test_misc_errors() {
    check_pattern_error(r"(", "missing ), unterminated subpattern", Some(0));
    check_pattern_error(r"((a|b)", "missing ), unterminated subpattern", Some(0));
    check_pattern_error(r"(a|b))", "unbalanced parenthesis", Some(5));
    check_pattern_error(r"(?P", "unexpected end of pattern", Some(3));
    check_pattern_error(r"(?z)", "unknown extension ?z", Some(1));
    check_pattern_error(r"(?iz)", "unknown flag", Some(3));
    check_pattern_error(r"(?i", "missing -, : or )", Some(3));
    check_pattern_error(r"(?#abc", "missing ), unterminated comment", Some(0));
    check_pattern_error(r"(?<", "unexpected end of pattern", Some(3));
    check_pattern_error(r"(?<>)", "unknown extension ?<>", Some(1));
    check_pattern_error(r"(?", "unexpected end of pattern", Some(2));
}

#[test]
fn test_bug_34294() {
    let sm = |x: &str| Some(x.to_string());
    assert_eq!(re_search(r"\b(?=(\t)|(x))x", "a\tx").unwrap().groups(), vec![None, sm("x")]);
    let p = c(r"(?=(.)(.)?)");
    assert_eq!(findall_pat(&p, "ab"), vec![strs(&["a", "b"]), strs(&["b", ""])]);
    let s = cp("ab");
    let g: Vec<_> = p.scanner(&s, 0, usize::MAX).map(|m| (group_of(&s, &m, 1), group_of(&s, &m, 2))).collect();
    assert_eq!(g, vec![(sm("a"), sm("b")), (sm("b"), None)]);

    let p = c(r"(?=<(?P<tag>\w+)/?>(?:(?P<text>.+?)</(?P=tag)>)?)");
    assert_eq!(findall_pat(&p, "<test><foo2/></test>"), vec![strs(&["test", "<foo2/>"]), strs(&["foo2", ""])]);
    let gd = |s: &str| -> Vec<(Option<String>, Option<String>)> {
        let v = cp(s);
        p.scanner(&v, 0, usize::MAX).map(|m| (group_of(&v, &m, 1), group_of(&v, &m, 2))).collect()
    };
    assert_eq!(gd("<test><foo2/></test>"), vec![(sm("test"), sm("<foo2/>")), (sm("foo2"), None)]);
    assert_eq!(gd("<test>Hello</test><foo/>"), vec![(sm("test"), sm("Hello")), (sm("foo"), None)]);
    assert_eq!(
        gd("<test>Hello</test><foo/><foo/>"),
        vec![(sm("test"), sm("Hello")), (sm("foo"), None), (sm("foo"), None)]
    );
}

#[test]
fn test_mark_bugs() {
    let sm = |x: &str| Some(x.to_string());
    assert_eq!(re_match(r"(ab|a)*?b", "ab").unwrap().groups(), vec![sm("a")]);
    assert_eq!(re_match(r"(ab|a)+?b", "ab").unwrap().groups(), vec![sm("a")]);
    assert_eq!(re_match(r"(ab|a){0,2}?b", "ab").unwrap().groups(), vec![sm("a")]);
    assert_eq!(re_match(r"(.b|a)*?b", "ab").unwrap().groups(), vec![sm("a")]);
    // MIN_UNTIL
    assert_eq!(re_match(r"(?:(?:a|bc)*?(xx)??z)*", "axxzbcz").unwrap().groups(), vec![sm("xx")]);
    let m = re_match(r"((x|yz)+?(t)??c)*", "xtcxyzxc").unwrap();
    assert_eq!(m.span(), (0, 8));
    assert_eq!(m.span_g(2), (6, 7));
    assert_eq!(m.groups(), vec![sm("xyzxc"), sm("x"), sm("t")]);
    // REPEAT_ONE
    let m = re_match(r"(?:[^b]*a(?=(b)|(a))ab)*", "aabaab").unwrap();
    assert_eq!(m.span(), (0, 6));
    assert_eq!(m.span_g(2), (4, 5));
    assert_eq!(m.groups(), vec![None, sm("a")]);
    let m = re_match(r"(?:[^b]*(?=(b)|(a))ab)*", "abab").unwrap();
    assert_eq!(m.span(), (0, 4));
    assert_eq!(m.span_g(2), (2, 3));
    assert_eq!(m.groups(), vec![None, sm("a")]);
    assert_eq!(re_match(r"(ab?)*?b", "ab").unwrap().groups(), vec![sm("a")]);
    // MIN_REPEAT_ONE
    let m = re_match(r"(?:.*?(?=(a)|(b))b)*", "abab").unwrap();
    assert_eq!(m.span(), (0, 4));
    assert_eq!(m.span_g(2), (3, 4));
    assert_eq!(m.groups(), vec![None, sm("b")]);
    assert_eq!(re_match(r"(?:a*?(xx)??z)*", "axxzaz").unwrap().groups(), vec![sm("xx")]);
    // ASSERT_NOT
    assert_eq!(re_match(r"(?!(..)c)", "ab").unwrap().groups(), vec![None]);
    let m = re_match(r"((?!(ab)c)(.))*", "abab").unwrap();
    assert_eq!(m.span(), (0, 4));
    assert_eq!(m.span_g(1), (3, 4));
    assert_eq!(m.span_g(3), (3, 4));
    assert_eq!(m.groups(), vec![sm("b"), None, sm("b")]);
}

#[test]
fn test_search_anchor_at_beginning() {
    let s = "x".repeat(10_000_000);
    let v = cp(&s);
    let t = std::time::Instant::now();
    for p in [r"\Ay", r"^y"] {
        let pat = c(p);
        assert!(pat.search(&v, 0, usize::MAX).is_none());
        assert_eq!(pat.scanner(&v, 0, usize::MAX).count(), 0);
    }
    assert!(t.elapsed().as_secs_f64() < 0.1, "{:?}", t.elapsed());
}

#[test]
fn test_possessive_quantifiers() {
    let sm = |x: &str| Some(x.to_string());
    assert!(re_match("e*+e", "eeee").is_none());
    assert_eq!(re_match("e++a", "eeea").unwrap().g0(), "eeea");
    assert_eq!(re_match("e?+a", "ea").unwrap().g0(), "ea");
    assert_eq!(re_match("e{2,4}+a", "eeea").unwrap().g0(), "eeea");
    assert!(re_match("(.)++.", "ee").is_none());
    assert_eq!(re_match("(ae)*+a", "aea").unwrap().groups(), vec![sm("ae")]);
    assert_eq!(re_match("([ae][ae])?+a", "aea").unwrap().groups(), vec![sm("ae")]);
    assert_eq!(re_match("(e?){2,4}+a", "eeea").unwrap().groups(), vec![sm("")]);
    assert_eq!(re_match("()*+a", "a").unwrap().groups(), vec![sm("")]);
    assert_eq!(re_search("x*+", "axx").unwrap().span(), (0, 0));
    assert_eq!(re_search("x++", "axx").unwrap().span(), (1, 3));
    assert_eq!(re_match("a*+", "xxx").unwrap().span(), (0, 0));
    assert_eq!(re_match("x*+", "xxxa").unwrap().span(), (0, 3));
    assert!(re_match("a++", "xxx").is_none());
    assert!(re_match(r"^(\w){1}+$", "abc").is_none());
    assert!(re_match(r"^(\w){1,2}+$", "abc").is_none());
    assert_eq!(re_match(r"^(\w){3}+$", "abc").unwrap().g(1), sm("c"));
    assert_eq!(re_match(r"^(\w){1,3}+$", "abc").unwrap().g(1), sm("c"));
    assert_eq!(re_match(r"^(\w){1,4}+$", "abc").unwrap().g(1), sm("c"));
    assert!(re_match("^x{1}+$", "xxx").is_none());
    assert!(re_match("^x{1,2}+$", "xxx").is_none());
    assert!(re_match("^x{3}+$", "xxx").is_some());
    assert!(re_match("^x{1,3}+$", "xxx").is_some());
    assert!(re_match("^x{1,4}+$", "xxx").is_some());
    assert!(re_match("^x{}+$", "xxx").is_none());
    assert!(re_match("^x{}+$", "x{}").is_some());
}

#[test]
fn test_fullmatch_possessive_and_atomic() {
    for p in [r"a++", r"a*+", r"a?+", r"a{1,3}+", r"(?>a+)", r"(?>a*)", r"(?>a?)", r"(?>a{1,3})"] {
        assert!(re_fullmatch(p, "a").is_some(), "{p}");
        assert!(re_fullmatch(p, "ab").is_none(), "{p}");
        assert!(re_fullmatch(&format!("{p}b"), "ab").is_some(), "{p}");
    }
    for p in [r"(?:ab)++", r"(?:ab)*+", r"(?:ab)?+", r"(?:ab){1,3}+", r"(?>(?:ab)+)", r"(?>(?:ab)*)", r"(?>(?:ab)?)", r"(?>(?:ab){1,3})"] {
        assert!(re_fullmatch(p, "ab").is_some(), "{p}");
        assert!(re_fullmatch(p, "abc").is_none(), "{p}");
        assert!(re_fullmatch(&format!("{p}c"), "abc").is_some(), "{p}");
    }
}

#[test]
fn test_findall_possessive_and_atomic() {
    for pre in ["", "(?>"] {
        let w = |p: &str| if pre.is_empty() { p.to_string() } else { format!("(?>{})", &p[..p.len() - 1]) };
        assert_eq!(findall(&w(r"a++"), "aab"), strs(&["aa"]));
        assert_eq!(findall(&w(r"a*+"), "aab"), strs(&["aa", "", ""]));
        assert_eq!(findall(&w(r"a?+"), "aab"), strs(&["a", "a", "", ""]));
        assert_eq!(findall(&w(r"a{1,3}+"), "aab"), strs(&["aa"]));
        assert_eq!(findall(&w(r"(?:ab)++"), "ababc"), strs(&["abab"]));
        assert_eq!(findall(&w(r"(?:ab)*+"), "ababc"), strs(&["abab", "", ""]));
        assert_eq!(findall(&w(r"(?:ab)?+"), "ababc"), strs(&["ab", "ab", "", ""]));
        assert_eq!(findall(&w(r"(?:ab){1,3}+"), "ababc"), strs(&["abab"]));
    }
}

#[test]
fn test_atomic_grouping() {
    let p = c(r"a(?>bc|b)c");
    assert!(p.match_at(&cp("abc"), 0, usize::MAX).is_none());
    assert!(p.match_at(&cp("abcc"), 0, usize::MAX).is_some());
    assert!(re_match(r"(?>.*).", "abc").is_none());
    assert!(re_match(r"(?>x)++", "xxx").is_some());
    assert!(re_match(r"(?>x++)", "xxx").is_some());
    assert!(re_match(r"(?>x)++x", "xxx").is_none());
    assert!(re_match(r"(?>x++)x", "xxx").is_none());
}

#[test]
fn test_bug_gh91616() {
    assert!(re_fullmatch(r"(?s:(?>.*?\.).*)\z", "a.txt").is_some());
    assert!(re_fullmatch(r"(?s:(?=(?P<g0>.*?\.))(?P=g0).*)\z", "a.txt").is_some());
}

#[test]
fn test_bug_gh100061() {
    assert_eq!(re_match("(?>(?:.(?!D))+)", "ABCDE").unwrap().span(), (0, 2));
    assert_eq!(re_match("(?:.(?!D))++", "ABCDE").unwrap().span(), (0, 2));
    assert_eq!(re_match("(?>(?:.(?!D))*)", "ABCDE").unwrap().span(), (0, 2));
    assert_eq!(re_match("(?:.(?!D))*+", "ABCDE").unwrap().span(), (0, 2));
    assert_eq!(re_match("(?>(?:.(?!D))?)", "CDE").unwrap().span(), (0, 0));
    assert_eq!(re_match("(?:.(?!D))?+", "CDE").unwrap().span(), (0, 0));
    assert_eq!(re_match("(?>(?:.(?!D)){1,3})", "ABCDE").unwrap().span(), (0, 2));
    assert_eq!(re_match("(?:.(?!D)){1,3}+", "ABCDE").unwrap().span(), (0, 2));
    assert_eq!(re_match("(?>(?:ab?c)+)", "aca").unwrap().span(), (0, 2));
    assert_eq!(re_match("(?:ab?c)++", "aca").unwrap().span(), (0, 2));
    assert_eq!(re_match("(?>(?:ab?c)*)", "aca").unwrap().span(), (0, 2));
    assert_eq!(re_match("(?:ab?c)*+", "aca").unwrap().span(), (0, 2));
    assert_eq!(re_match("(?>(?:ab?c)?)", "a").unwrap().span(), (0, 0));
    assert_eq!(re_match("(?:ab?c)?+", "a").unwrap().span(), (0, 0));
    assert_eq!(re_match("(?>(?:ab?c){1,3})", "aca").unwrap().span(), (0, 2));
    assert_eq!(re_match("(?:ab?c){1,3}+", "aca").unwrap().span(), (0, 2));
}

#[test]
fn test_bug_gh101955() {
    let sm = |x: &str| Some(x.to_string());
    assert_eq!(re_match("((x)|y|z)*+", "xyz").unwrap().groups(), vec![sm("z"), sm("x")]);
    assert_eq!(re_match("((x)|y|z){3}+", "xyz").unwrap().groups(), vec![sm("z"), sm("x")]);
    assert_eq!(re_match("((x)|y|z){3,}+", "xyz").unwrap().groups(), vec![sm("z"), sm("x")]);
}

#[test]
fn test_regression_gh94675() {
    let pattern = c(concat!(
        r"(?<=[({}])(((//[^\n]*)?[\n])([\000-\040])*)*",
        r"((/[^/\[\n]*(([^\n]|(\[\n]*(]*)*\]))",
        r"[^/\[]*)*/))((((//[^\n]*)?[\n])",
        r"([\000-\040]|(/\*[^*]*\*+",
        r"([^/*]\*+)*/))*)+(?=[^\000-\040);\]}]))"
    ));
    let input_js = "a(function() {\n            ///////////////////////////////////////////////////////////////////\n        });";
    let t = std::time::Instant::now();
    let _ = subn_pat(&pattern, "", input_js, 0).unwrap();
    assert!(t.elapsed().as_secs() < 10, "{:?}", t.elapsed());
}

#[test]
fn test_fail() {
    assert_eq!(re_search(r"12(?!)|3", "123").unwrap().g0(), "3");
}

#[test]
fn test_character_set_any_none() {
    let s = "1x\n";
    for p in [r"[\s\S]", r"[\d\D]", r"[\w\W]", r"[\S\s]", r"\s|\S"] {
        assert_eq!(findall(p, s), strs(&["1", "x", "\n"]), "{p}");
        assert_eq!(re_fullmatch(&format!("(?:{p})+"), s).unwrap().g0(), s);
    }
    for p in [r"[^\s\S]", r"[^\d\D]", r"[^\w\W]", r"[^\S\s]"] {
        assert!(re_search(p, s).is_none(), "{p}");
        assert!(re_search(&format!("(?s:.){p}"), s).is_none(), "{p}");
    }
}

#[test]
fn test_memory_leak_patterns() {
    // _fail_after is not available; just make sure these patterns work.
    for p in [r"(.)*:", r"([^:])*?:", r"([^:])*+:", r"(.){2,4}:", r"([^:]){2,4}?:", r"([^:]){2,4}+:"] {
        assert!(re_match(p, "abc:").is_some(), "{p}");
    }
}

#[test]
fn test_case_helpers() {
    for i in 0u32..128 {
        let ch = char::from_u32(i).unwrap();
        let lo = ch.to_ascii_lowercase() as u32;
        assert_eq!(ascii_tolower(i), lo);
        assert_eq!(unicode_tolower(&DEFAULT_PROVIDER, i), lo);
        assert_eq!(ascii_iscased(i), ch.is_ascii_alphabetic());
        assert_eq!(unicode_iscased(&DEFAULT_PROVIDER, i), ch.is_ascii_alphabetic());
    }
    assert_eq!(ascii_tolower(0x130), 0x130);
    assert_eq!(unicode_tolower(&DEFAULT_PROVIDER, 0x130), 'i' as u32);
    assert!(!ascii_iscased(0x130));
    assert!(unicode_iscased(&DEFAULT_PROVIDER, 0x130));
}

#[test]
fn test_fnmatch_style() {
    // patterns produced by fnmatch.translate
    let m = |p: &str, s: &str| re_match(p, s).is_some();
    assert!(m(r"(?s:.*)\z", "abc"));
    assert!(m(r"(?s:ab(?>.*?cd).*12)\z", "abxxcdyy12"));
    assert!(!m(r"(?s:ab(?>.*?cd).*12)\z", "abxxcdyy13"));
    assert!(m(r"(?s:(?>.*?ab)(?>.*?cd).*)\z", "xxabyycdzz"));
    assert!(m(r"(?s:[\[]a\])\z", "[a]"));
    let fat = format!(
        "(?:{})|(?:{})|(?:{})",
        r"(?s:(?>.*?a)(?>.*?a)(?>.*?a).*)\z", r"(?s:(?>.*?b)(?>.*?b)(?>.*?b).*)\z", r"(?s:(?>.*?c)(?>.*?c)(?>.*?c).*)\z"
    );
    assert!(m(&fat, "abaccad"));
    assert!(m(&fat, "abxbcab"));
    assert!(m(&fat, "cbabcaxc"));
    assert!(!m(&fat, "dabccbad"));
}
