#![allow(dead_code)]
//! Test helpers emulating the parts of the `re` module the tests need.

use rpy_sre::*;

pub fn cp(s: &str) -> Vec<u32> {
    s.chars().map(|c| c as u32).collect()
}

pub fn st(v: &[u32]) -> String {
    v.iter().map(|&c| char::from_u32(c).unwrap_or('\u{FFFD}')).collect()
}

pub fn c(p: &str) -> Pattern {
    cf(p, 0)
}

pub fn cf(p: &str, flags: u32) -> Pattern {
    match compile_str(p, flags) {
        Ok(x) => x,
        Err(e) => panic!("compile {p:?} failed: {e}"),
    }
}

pub fn cb(p: &[u8], flags: u32) -> Pattern {
    match compile_bytes(p, flags) {
        Ok(x) => x,
        Err(e) => panic!("compile {p:?} failed: {e}"),
    }
}

/// Match result with the subject, for convenient group access.
pub struct M {
    pub caps: Captures,
    pub s: Vec<u32>,
    pub pat: Pattern,
}

impl M {
    pub fn span(&self) -> (isize, isize) {
        self.caps.spans[0]
    }
    pub fn span_g(&self, g: usize) -> (isize, isize) {
        self.caps.spans[g]
    }
    pub fn group(&self, g: usize) -> Option<String> {
        let (a, b) = self.caps.spans[g];
        if a < 0 { None } else { Some(st(&self.s[a as usize..b as usize])) }
    }
    pub fn g0(&self) -> String {
        self.group(0).unwrap()
    }
    pub fn g(&self, g: usize) -> Option<String> {
        self.group(g)
    }
    pub fn named(&self, name: &str) -> Option<String> {
        self.group(self.pat.group_by_name(name).expect("no such group"))
    }
    pub fn groups(&self) -> Vec<Option<String>> {
        (1..self.caps.spans.len()).map(|g| self.group(g)).collect()
    }
    pub fn lastindex(&self) -> Option<usize> {
        self.caps.lastindex
    }
    pub fn lastgroup(&self) -> Option<String> {
        self.caps.lastindex.and_then(|i| self.pat.group_name(i).map(|s| s.to_string()))
    }
}

fn mk(pat: &Pattern, s: &str, caps: Option<Captures>) -> Option<M> {
    caps.map(|caps| M { caps, s: cp(s), pat: pat.clone() })
}

pub fn re_match(p: &str, s: &str) -> Option<M> {
    re_match_f(p, s, 0)
}
pub fn re_match_f(p: &str, s: &str, flags: u32) -> Option<M> {
    let pat = cf(p, flags);
    let v = cp(s);
    let r = pat.match_at(&v, 0, usize::MAX);
    mk(&pat, s, r)
}
pub fn re_search(p: &str, s: &str) -> Option<M> {
    re_search_f(p, s, 0)
}
pub fn re_search_f(p: &str, s: &str, flags: u32) -> Option<M> {
    let pat = cf(p, flags);
    let v = cp(s);
    let r = pat.search(&v, 0, usize::MAX);
    mk(&pat, s, r)
}
pub fn re_fullmatch(p: &str, s: &str) -> Option<M> {
    re_fullmatch_f(p, s, 0)
}
pub fn re_fullmatch_f(p: &str, s: &str, flags: u32) -> Option<M> {
    let pat = cf(p, flags);
    let v = cp(s);
    let r = pat.fullmatch(&v, 0, usize::MAX);
    mk(&pat, s, r)
}

/// Bytes match helpers (subject as bytes; groups returned as latin-1 strings).
pub fn b_match(p: &[u8], s: &[u8], flags: u32) -> Option<Captures> {
    cb(p, flags).match_at(s, 0, usize::MAX)
}
pub fn b_search(p: &[u8], s: &[u8], flags: u32) -> Option<Captures> {
    cb(p, flags).search(s, 0, usize::MAX)
}
pub fn b_fullmatch(p: &[u8], s: &[u8], flags: u32) -> Option<Captures> {
    cb(p, flags).fullmatch(s, 0, usize::MAX)
}

pub fn group_of(s: &[u32], caps: &Captures, g: usize) -> Option<String> {
    let (a, b) = caps.spans[g];
    if a < 0 { None } else { Some(st(&s[a as usize..b as usize])) }
}

/// `re.findall`: groups as tuples rendered into a Vec; unmatched groups -> "".
pub fn findall_pat(pat: &Pattern, s: &str) -> Vec<Vec<String>> {
    findall_pos(pat, s, 0, usize::MAX)
}

pub fn findall_pos(pat: &Pattern, s: &str, pos: usize, endpos: usize) -> Vec<Vec<String>> {
    let v = cp(s);
    let mut out = Vec::new();
    for caps in pat.scanner(&v, pos, endpos) {
        let mut item = Vec::new();
        if pat.groups() == 0 {
            item.push(group_of(&v, &caps, 0).unwrap());
        } else {
            for g in 1..=pat.groups() {
                item.push(group_of(&v, &caps, g).unwrap_or_default());
            }
        }
        out.push(item);
    }
    out
}

/// findall for patterns with at most one group -> flat list.
pub fn findall(p: &str, s: &str) -> Vec<String> {
    findall_f(p, s, 0)
}
pub fn findall_f(p: &str, s: &str, flags: u32) -> Vec<String> {
    let pat = cf(p, flags);
    findall_pat(&pat, s).into_iter().map(|mut v| v.remove(0)).collect()
}

pub fn finditer_spans(p: &str, s: &str) -> Vec<(isize, isize)> {
    let pat = c(p);
    let v = cp(s);
    pat.scanner(&v, 0, usize::MAX).map(|c| c.spans[0]).collect()
}

pub fn subn_pat(pat: &Pattern, repl: &str, s: &str, count: usize) -> Result<(String, usize), TemplateError> {
    let v = cp(s);
    let tpl = pat.parse_template(&cp(repl))?;
    let mut out: Vec<u32> = Vec::new();
    let mut last = 0;
    let mut n = 0;
    let mut sc = pat.scanner(&v, 0, usize::MAX);
    while count == 0 || n < count {
        let Some(caps) = sc.search() else { break };
        let (a, b) = (caps.start(), caps.end());
        out.extend_from_slice(&v[last..a]);
        expand_template(
            &tpl,
            |g| {
                let (x, y) = caps.spans[g];
                if x < 0 { None } else { Some(&v[x as usize..y as usize]) }
            },
            &mut out,
        );
        last = b;
        n += 1;
    }
    out.extend_from_slice(&v[last..]);
    Ok((st(&out), n))
}

pub fn sub(p: &str, repl: &str, s: &str) -> String {
    subn_pat(&c(p), repl, s, 0).unwrap().0
}
pub fn subn(p: &str, repl: &str, s: &str, count: usize) -> (String, usize) {
    subn_pat(&c(p), repl, s, count).unwrap()
}
pub fn sub_err(p: &str, repl: &str, s: &str) -> TemplateError {
    subn_pat(&c(p), repl, s, 0).unwrap_err()
}

/// Substitution with a callback.
pub fn sub_fn(p: &str, f: impl Fn(&M) -> String, s: &str, count: usize) -> String {
    let pat = c(p);
    let v = cp(s);
    let mut out = String::new();
    let mut last = 0;
    let mut n = 0;
    let mut sc = pat.scanner(&v, 0, usize::MAX);
    while count == 0 || n < count {
        let Some(caps) = sc.search() else { break };
        out.push_str(&st(&v[last..caps.start()]));
        let end = caps.end();
        let m = M { caps, s: v.clone(), pat: pat.clone() };
        out.push_str(&f(&m));
        last = end;
        n += 1;
    }
    out.push_str(&st(&v[last..]));
    out
}

/// `re.split` -> items (None for unmatched groups).
pub fn split_pat(pat: &Pattern, s: &str, maxsplit: usize) -> Vec<Option<String>> {
    let v = cp(s);
    let mut out = Vec::new();
    let mut last = 0;
    let mut n = 0;
    let mut sc = pat.scanner(&v, 0, usize::MAX);
    while maxsplit == 0 || n < maxsplit {
        let Some(caps) = sc.search() else { break };
        out.push(Some(st(&v[last..caps.start()])));
        for g in 1..=pat.groups() {
            out.push(group_of(&v, &caps, g));
        }
        last = caps.end();
        n += 1;
    }
    out.push(Some(st(&v[last..])));
    out
}

pub fn split(p: &str, s: &str) -> Vec<Option<String>> {
    split_pat(&c(p), s, 0)
}
pub fn split_max(p: &str, s: &str, maxsplit: usize) -> Vec<Option<String>> {
    split_pat(&c(p), s, maxsplit)
}

pub fn somes(v: &[&str]) -> Vec<Option<String>> {
    v.iter().map(|s| Some(s.to_string())).collect()
}

pub fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// Compile error (msg, pos) for a str pattern.
pub fn perr(p: &str) -> (String, Option<usize>) {
    perr_f(p, 0)
}
pub fn perr_f(p: &str, flags: u32) -> (String, Option<usize>) {
    match compile_str(p, flags) {
        Ok(_) => panic!("pattern {p:?} compiled but an error was expected"),
        Err(CompileError::Pattern { msg, pos }) => (msg, pos),
        Err(e) => panic!("pattern {p:?}: unexpected error kind {e:?}"),
    }
}

pub fn check_pattern_error(p: &str, msg: &str, pos: Option<usize>) {
    let (m, ps) = perr(p);
    assert_eq!(m, msg, "pattern {p:?}");
    if let Some(pos) = pos {
        assert_eq!(ps, Some(pos), "pattern {p:?} msg {msg:?}");
    }
}

pub fn check_pattern_error_b(p: &[u8], msg: &str, pos: Option<usize>) {
    match compile_bytes(p, 0) {
        Ok(_) => panic!("pattern {p:?} compiled but an error was expected"),
        Err(CompileError::Pattern { msg: m, pos: ps }) => {
            assert_eq!(m, msg, "pattern {p:?}");
            if let Some(pos) = pos {
                assert_eq!(ps, Some(pos), "pattern {p:?}");
            }
        }
        Err(e) => panic!("pattern {p:?}: unexpected error kind {e:?}"),
    }
}

pub fn check_template_error(p: &str, repl: &str, s: &str, msg: &str, pos: Option<usize>) {
    match sub_err(p, repl, s) {
        TemplateError::Pattern { msg: m, pos: ps } => {
            assert_eq!(m, msg, "pattern {p:?} repl {repl:?}");
            if let Some(pos) = pos {
                assert_eq!(ps, pos, "pattern {p:?} repl {repl:?}");
            }
        }
        e => panic!("pattern {p:?} repl {repl:?}: unexpected {e:?}"),
    }
}
