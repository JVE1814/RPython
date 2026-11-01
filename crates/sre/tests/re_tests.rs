//! Runs the `tests` and `benchmarks` tables of spec/Lib/test/re_tests.py,
//! following test_re.ExternalTests.  The Python file is read at test time
//! with a tiny literal parser (strings, chr(N), names, `+`).

mod common;
use common::*;
use rpy_sre::*;

#[derive(Clone, Debug)]
enum V {
    S(Vec<u32>),
    #[allow(dead_code)]
    N(String),
    I(i64),
    Tuple(Vec<V>),
}

struct P<'a> {
    s: &'a [char],
    i: usize,
}

impl<'a> P<'a> {
    fn ws(&mut self) {
        while self.i < self.s.len() {
            let c = self.s[self.i];
            if c.is_whitespace() {
                self.i += 1;
            } else if c == '#' {
                while self.i < self.s.len() && self.s[self.i] != '\n' {
                    self.i += 1;
                }
            } else {
                break;
            }
        }
    }
    fn peek(&mut self) -> Option<char> {
        self.ws();
        self.s.get(self.i).copied()
    }
    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn starts(&self, t: &str) -> bool {
        let t: Vec<char> = t.chars().collect();
        self.s[self.i..].starts_with(&t)
    }
    fn string(&mut self) -> Vec<u32> {
        let mut raw = false;
        while matches!(self.s[self.i], 'r' | 'R' | 'b' | 'B' | 'u' | 'U') {
            if matches!(self.s[self.i], 'r' | 'R') {
                raw = true;
            }
            self.i += 1;
        }
        let q = self.s[self.i];
        let triple = self.s.get(self.i + 1) == Some(&q) && self.s.get(self.i + 2) == Some(&q);
        self.i += if triple { 3 } else { 1 };
        let mut out = Vec::new();
        loop {
            let c = self.s[self.i];
            if c == q {
                if !triple {
                    self.i += 1;
                    break;
                }
                if self.s.get(self.i + 1) == Some(&q) && self.s.get(self.i + 2) == Some(&q) {
                    self.i += 3;
                    break;
                }
            }
            if c == '\\' {
                let d = self.s[self.i + 1];
                if raw {
                    out.push('\\' as u32);
                    out.push(d as u32);
                    self.i += 2;
                    continue;
                }
                self.i += 2;
                match d {
                    '\\' => out.push(92),
                    '\'' => out.push(39),
                    '"' => out.push(34),
                    'n' => out.push(10),
                    't' => out.push(9),
                    'r' => out.push(13),
                    'a' => out.push(7),
                    'b' => out.push(8),
                    'f' => out.push(12),
                    'v' => out.push(11),
                    '\n' => {}
                    '0'..='7' => {
                        let mut v = d.to_digit(8).unwrap();
                        let mut k = 0;
                        while k < 2 && self.s[self.i].is_digit(8) {
                            v = v * 8 + self.s[self.i].to_digit(8).unwrap();
                            self.i += 1;
                            k += 1;
                        }
                        out.push(v);
                    }
                    'x' | 'u' | 'U' => {
                        let n = match d {
                            'x' => 2,
                            'u' => 4,
                            _ => 8,
                        };
                        let h: String = self.s[self.i..self.i + n].iter().collect();
                        out.push(u32::from_str_radix(&h, 16).unwrap());
                        self.i += n;
                    }
                    'N' => {
                        let start = self.i + 1;
                        let mut j = start;
                        while self.s[j] != '}' {
                            j += 1;
                        }
                        let name: String = self.s[start..j].iter().collect();
                        self.i = j + 1;
                        let v = match name.as_str() {
                            "LATIN CAPITAL LETTER A WITH DIAERESIS" => 0xC4,
                            _ => panic!("unknown name {name}"),
                        };
                        out.push(v);
                    }
                    _ => {
                        out.push('\\' as u32);
                        out.push(d as u32);
                    }
                }
                continue;
            }
            out.push(c as u32);
            self.i += 1;
        }
        out
    }
    fn atom(&mut self, env: &[(String, V)]) -> V {
        let c = self.peek().unwrap();
        if c == '(' {
            self.i += 1;
            let mut items = Vec::new();
            while !self.eat(')') {
                items.push(self.expr(env));
                self.eat(',');
            }
            return V::Tuple(items);
        }
        if c == '\'' || c == '"' || ((c == 'r' || c == 'b' || c == 'u') && {
            let n = self.s[self.i + 1];
            n == '\'' || n == '"'
        }) {
            return V::S(self.string());
        }
        if c.is_ascii_digit() {
            let mut n = 0i64;
            while self.s[self.i].is_ascii_digit() {
                n = n * 10 + self.s[self.i].to_digit(10).unwrap() as i64;
                self.i += 1;
            }
            return V::I(n);
        }
        let mut name = String::new();
        while self.s[self.i].is_alphanumeric() || self.s[self.i] == '_' {
            name.push(self.s[self.i]);
            self.i += 1;
        }
        if name == "chr" {
            assert!(self.eat('('));
            let v = match self.expr(env) {
                V::I(n) => n,
                _ => panic!(),
            };
            assert!(self.eat(')'));
            return V::S(vec![v as u32]);
        }
        for (n, v) in env {
            if *n == name {
                return v.clone();
            }
        }
        V::N(name)
    }
    fn expr(&mut self, env: &[(String, V)]) -> V {
        let mut v = self.atom(env);
        while self.eat('+') {
            let w = self.atom(env);
            v = match (v, w) {
                (V::S(mut a), V::S(b)) => {
                    a.extend(b);
                    V::S(a)
                }
                (a, b) => panic!("bad concat {a:?} {b:?}"),
            };
        }
        v
    }
    fn list(&mut self, env: &[(String, V)]) -> Vec<V> {
        assert!(self.eat('['));
        let mut out = Vec::new();
        while !self.eat(']') {
            out.push(self.expr(env));
            self.eat(',');
        }
        out
    }
}

fn load() -> (Vec<V>, Vec<V>) {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/Lib/test/re_tests.py");
    let text = std::fs::read_to_string(path).expect("re_tests.py");
    let chars: Vec<char> = text.chars().collect();
    let mut p = P { s: &chars, i: 0 };
    let mut env: Vec<(String, V)> = vec![
        ("SUCCEED".into(), V::I(0)),
        ("FAIL".into(), V::I(1)),
        ("SYNTAX_ERROR".into(), V::I(2)),
    ];
    let mut benchmarks = Vec::new();
    let mut tests = Vec::new();
    while p.i < chars.len() {
        p.ws();
        if p.starts("benchmarks = ") {
            p.i += "benchmarks = ".len();
            benchmarks = p.list(&env);
        } else if p.starts("tests = ") {
            p.i += "tests = ".len();
            tests = p.list(&env);
        } else if p.starts("u = ") {
            p.i += 4;
            let v = p.expr(&env);
            env.push(("u".into(), v));
        } else if p.starts("tests.extend(") {
            p.i += "tests.extend(".len();
            tests.extend(p.list(&env));
            assert!(p.eat(')'));
        } else {
            // skip line
            while p.i < chars.len() && chars[p.i] != '\n' {
                p.i += 1;
            }
            p.i += 1;
        }
    }
    (benchmarks, tests)
}

fn s_of(v: &V) -> Vec<u32> {
    match v {
        V::S(s) => s.clone(),
        _ => panic!("expected string, got {v:?}"),
    }
}

fn eval(expr: &[u32], vars: &dyn Fn(&str) -> Option<String>) -> String {
    let text: Vec<char> = expr.iter().map(|&c| char::from_u32(c).unwrap()).collect();
    let mut p = P { s: &text, i: 0 };
    let mut out = String::new();
    loop {
        p.ws();
        let c = p.peek().unwrap();
        if c == '"' || c == '\'' {
            out.push_str(&st(&p.string()));
        } else {
            let mut name = String::new();
            while p.i < text.len() && (text[p.i].is_alphanumeric() || text[p.i] == '_') {
                name.push(text[p.i]);
                p.i += 1;
            }
            out.push_str(&vars(&name).unwrap_or_else(|| panic!("unknown var {name}")));
        }
        if !p.eat('+') {
            break;
        }
    }
    out
}

fn ascii(v: &[u32]) -> Option<Vec<u8>> {
    v.iter().map(|&c| if c < 128 { Some(c as u8) } else { None }).collect()
}

#[test]
fn re_tests_table() {
    let (_, tests) = load();
    assert!(tests.len() > 300, "parsed only {} tests", tests.len());
    let mut failures = Vec::new();
    for t in &tests {
        let V::Tuple(items) = t else { panic!() };
        let pattern = s_of(&items[0]);
        let s = s_of(&items[1]);
        let outcome = match items[2] {
            V::I(n) => n,
            _ => panic!(),
        };
        let pstr = st(&pattern);
        let r = compile(PatternSource::Ucs4(&pattern), 0);
        if outcome == 2 {
            match r {
                Err(CompileError::Pattern { .. }) => {}
                other => failures.push(format!("{pstr:?}: expected syntax error, got {other:?}")),
            }
            continue;
        }
        let pat = match r {
            Ok(p) => p,
            Err(e) => {
                failures.push(format!("{pstr:?}: compile error {e}"));
                continue;
            }
        };
        let result = pat.search(&s, 0, usize::MAX);
        if outcome == 1 {
            if result.is_some() {
                failures.push(format!("{pstr:?} on {:?}: succeeded incorrectly", st(&s)));
            }
            continue;
        }
        let Some(m) = result else {
            failures.push(format!("{pstr:?} on {:?}: failed incorrectly", st(&s)));
            continue;
        };
        if items.len() == 5 {
            let repl = s_of(&items[3]);
            let expected = st(&s_of(&items[4]));
            let vars = |name: &str| -> Option<String> {
                let grp = |g: usize| -> String {
                    if g > pat.groups() {
                        "Error".into()
                    } else {
                        group_of(&s, &m, g).unwrap_or_else(|| "None".into())
                    }
                };
                if name == "found" {
                    return Some(grp(0));
                }
                if let Some(n) = name.strip_prefix('g') {
                    if let Ok(g) = n.parse::<usize>() {
                        return Some(grp(g));
                    }
                }
                pat.group_by_name(name).map(grp)
            };
            let got = eval(&repl, &vars);
            if got != expected {
                failures.push(format!(
                    "{pstr:?} on {:?}: grouping error: {got:?} != {expected:?}",
                    st(&s)
                ));
            }
        }
        // bytes version
        if let (Some(bp), Some(bs)) = (ascii(&pattern), ascii(&s)) {
            match compile_bytes(&bp, 0) {
                Ok(bpat) => {
                    if bpat.search(&bs[..], 0, usize::MAX).is_none() {
                        failures.push(format!("{pstr:?}: bytes pattern match failed"));
                    }
                }
                Err(e) => failures.push(format!("{pstr:?}: bytes compile failed {e}")),
            }
            match compile_bytes(&bp, flags::LOCALE) {
                Ok(_) => {}
                Err(e) => failures.push(format!("{pstr:?}: locale compile failed {e}")),
            }
        }
        // range-limited
        let n = pattern.len();
        let starts_b = n >= 2 && pattern[0] == '\\' as u32 && pattern[1] == 'B' as u32;
        let ends_b = n >= 2 && pattern[n - 2] == '\\' as u32 && pattern[n - 1] == 'B' as u32;
        if !starts_b && !ends_b {
            if pat.search(&s, m.start(), m.end() + 1).is_none() {
                failures.push(format!("{pstr:?} on {:?}: range-limited match failed", st(&s)));
            }
        }
        let ipat = compile(PatternSource::Ucs4(&pattern), flags::IGNORECASE).unwrap();
        if ipat.search(&s, 0, usize::MAX).is_none() {
            failures.push(format!("{pstr:?} on {:?}: case-insensitive match failed", st(&s)));
        }
        let upat = compile(PatternSource::Ucs4(&pattern), flags::UNICODE).unwrap();
        if upat.search(&s, 0, usize::MAX).is_none() {
            failures.push(format!("{pstr:?} on {:?}: unicode match failed", st(&s)));
        }
    }
    if !failures.is_empty() {
        panic!("{} failures:\n{}", failures.len(), failures.join("\n"));
    }
}

#[test]
fn re_tests_benchmarks() {
    let (benchmarks, _) = load();
    assert!(benchmarks.len() >= 10);
    for b in &benchmarks {
        let V::Tuple(items) = b else { panic!() };
        let pattern = s_of(&items[0]);
        let s = s_of(&items[1]);
        let p = compile(PatternSource::Ucs4(&pattern), 0).unwrap();
        let ps = st(&pattern);
        assert!(p.search(&s, 0, usize::MAX).is_some(), "{ps}");
        assert!(p.match_at(&s, 0, usize::MAX).is_some(), "{ps}");
        assert!(p.fullmatch(&s, 0, usize::MAX).is_some(), "{ps}");
        let mut s2: Vec<u32> = vec![' ' as u32; 10000];
        s2.extend_from_slice(&s);
        s2.extend(std::iter::repeat(' ' as u32).take(10000));
        assert!(p.search(&s2, 0, usize::MAX).is_some(), "{ps}");
        assert!(p.match_at(&s2, 10000, usize::MAX).is_some(), "{ps}");
        assert!(p.match_at(&s2, 10000, 10000 + s.len()).is_some(), "{ps}");
        assert!(p.fullmatch(&s2, 10000, 10000 + s.len()).is_some(), "{ps}");
    }
}
