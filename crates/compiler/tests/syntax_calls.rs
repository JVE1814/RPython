//! Extracts `check_syntax_error(self, "src", "msg")`, `self._check_error("src", "msg")`,
//! `self.assert_syntax_error("""src""")` calls with literal arguments from the test suite and
//! checks that, when the parser accepts the source, the compiler raises a SyntaxError whose
//! message matches.

use rpy_compiler::{compile, CompileOptions, Mode};
use rpy_parser::{parse, Mode as PMode, ParseOptions};

/// Parse a Python string literal (sequence of adjacent literals) at `s[i..]`.
fn parse_str_lits(s: &[u8], mut i: usize) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut any = false;
    loop {
        while i < s.len() && (s[i] == b' ' || s[i] == b'\n' || s[i] == b'\t') {
            i += 1;
        }
        let mut raw = false;
        let start = i;
        while i < s.len() && matches!(s[i], b'r' | b'R' | b'u' | b'U') {
            if s[i] == b'r' || s[i] == b'R' {
                raw = true;
            }
            i += 1;
        }
        if i >= s.len() || (s[i] != b'\'' && s[i] != b'"') {
            if any {
                return Some((out, start));
            }
            return None;
        }
        let q = s[i];
        let triple = i + 2 < s.len() && s[i + 1] == q && s[i + 2] == q;
        i += if triple { 3 } else { 1 };
        loop {
            if i >= s.len() {
                return None;
            }
            if triple {
                if s[i] == q && i + 2 < s.len() && s[i + 1] == q && s[i + 2] == q {
                    i += 3;
                    break;
                }
            } else if s[i] == q {
                i += 1;
                break;
            } else if s[i] == b'\n' {
                return None;
            }
            if s[i] == b'\\' && i + 1 < s.len() {
                let c = s[i + 1];
                if raw {
                    out.push('\\');
                    out.push(c as char);
                } else {
                    match c {
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'0' => out.push('\0'),
                        b'\\' => out.push('\\'),
                        b'\'' => out.push('\''),
                        b'"' => out.push('"'),
                        b'\n' => {}
                        _ => return None,
                    }
                }
                i += 2;
                continue;
            }
            // copy one UTF-8 char
            let len = match s[i] {
                0..=0x7f => 1,
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                _ => 4,
            };
            out.push_str(std::str::from_utf8(&s[i..i + len]).ok()?);
            i += len;
        }
        any = true;
    }
}

fn dedent(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let margin = lines.iter().filter(|l| !l.trim().is_empty()).map(|l| l.len() - l.trim_start().len()).min().unwrap_or(0);
    let mut out = String::new();
    for l in lines {
        if l.len() >= margin {
            out.push_str(&l[margin..]);
        } else {
            out.push_str(l.trim_start());
        }
        out.push('\n');
    }
    out
}

fn regex_plain(msg: &str) -> Option<String> {
    // unescape simple regex escapes; give up on real regex syntax
    let mut out = String::new();
    let mut it = msg.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\\' => out.push(it.next()?),
            '.' | '*' | '+' | '?' | '[' | ']' | '^' | '$' | '|' | '{' | '}' | '(' | ')' => return None,
            _ => out.push(c),
        }
    }
    Some(out)
}

#[test]
fn syntax_error_calls() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/Lib/test");
    let files = [
        "test_syntax.py",
        "test_type_params.py",
        "test_scope.py",
        "test_grammar.py",
        "test_positional_only_arg.py",
        "test_patma.py",
        "test_type_annotations.py",
        "test_named_expressions.py",
        "test_coroutines.py",
        "test_compile.py",
        "test_genexps.py",
        "test_keywordonlyarg.py",
        "test_exceptions.py",
        "test_unpack_ex.py",
        "test_listcomps.py",
        "test_global.py",
        "test_except_star.py",
        "test_fstring.py",
        "test_tstring.py",
        "test_super.py",
    ];
    let (mut checked, mut bad) = (0, Vec::new());
    for file in files {
        let path = root.join(file);
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let b = text.as_bytes();
        for pat in ["check_syntax_error(self,", "self._check_error(", "self.assert_syntax_error("] {
            let mut pos = 0;
            while let Some(k) = text[pos..].find(pat) {
                let at = pos + k;
                pos = at + pat.len();
                let line = text[..at].matches('\n').count() + 1;
                let mut i = pos;
                let mut dedent_it = pat.contains("assert_syntax_error");
                while i < b.len() && (b[i] == b' ' || b[i] == b'\n') {
                    i += 1;
                }
                for w in ["textwrap.dedent(", "dedent("] {
                    if text[i..].starts_with(w) {
                        i += w.len();
                        dedent_it = true;
                    }
                }
                let Some((mut src, mut j)) = parse_str_lits(b, i) else { continue };
                if dedent_it {
                    while j < b.len() && (b[j] == b')' || b[j] == b' ' || b[j] == b'\n') && b[j] != b',' {
                        if b[j] == b')' {
                            j += 1;
                            break;
                        }
                        j += 1;
                    }
                    src = dedent(&src);
                }
                while j < b.len() && (b[j] == b' ' || b[j] == b'\n') {
                    j += 1;
                }
                let msg = if j < b.len() && b[j] == b',' {
                    match parse_str_lits(b, j + 1) {
                        Some((m, _)) => Some(m),
                        None => {
                            continue;
                        }
                    }
                } else {
                    None
                };
                let mode = if pat.contains("Single") { PMode::Single } else { PMode::Exec };
                let Ok(m) = parse(&src, &ParseOptions::new(mode)).result else { continue };
                checked += 1;
                let cm = if mode == PMode::Single { Mode::Single } else { Mode::Exec };
                match compile(&m, &CompileOptions::new("<t>", cm)) {
                    Ok(_) => bad.push(format!("{}:{}: no error (expected {:?})\n{}", file, line, msg, src)),
                    Err(e) => {
                        // positional checks (lineno=, offset=, end_lineno=, end_offset=)
                        let call_end = { let mut d = 0i32; let mut q = at; let mut k = at; while k < b.len() { match b[k] { b'(' => d += 1, b')' => { d -= 1; if d == 0 { q = k; break; } } _ => {} } k += 1; } q };
                        let tail = &text[j.min(call_end)..call_end];
                        let kw = |name: &str| -> Option<i32> {
                            let p = tail.find(&format!("{}=", name))?;
                            if p > 0 && (tail.as_bytes()[p - 1].is_ascii_alphanumeric() || tail.as_bytes()[p - 1] == b'_') { return None; }
                            let digits: String = tail[p + name.len() + 1..].chars().take_while(|c| c.is_ascii_digit()).collect();
                            digits.parse().ok()
                        };
                        let line_of = |n: i32| src.lines().nth((n - 1).max(0) as usize).map(|s| s.to_string());
                        let (o, eo) = e.offsets(line_of(e.lineno).as_deref(), line_of(e.end_lineno).as_deref());
                        for (name, got) in [("lineno", e.lineno), ("offset", o), ("end_lineno", e.end_lineno), ("end_offset", eo)] {
                            if let Some(want) = kw(name) {
                                if want != got { bad.push(format!("{}:{}: {} {} != expected {} ({:?})\n{}", file, line, name, got, want, e.msg, src)); }
                            }
                        }
                        if let Some(msg) = &msg {
                            if let Some(p) = regex_plain(msg) {
                                if !e.msg.contains(&p) {
                                    bad.push(format!("{}:{}: got {:?}, expected {:?}\n{}", file, line, e.msg, p, src));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    eprintln!("checked {} calls, {} mismatches", checked, bad.len());
    for x in &bad {
        eprintln!("{}\n", x);
    }
    assert!(bad.is_empty());
}
