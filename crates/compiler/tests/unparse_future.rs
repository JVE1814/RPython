//! Unparse of annotations under `from __future__ import annotations`, checked against the
//! eq(...) cases of spec/Lib/test/test_future_stmt/test_future.py.
mod common;
use rpy_compiler::bytecode::Constant;
use rpy_compiler::Mode;

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

fn actual(ann: &str) -> Option<String> {
    let src = format!("from __future__ import annotations\ndef f() -> {}: pass\n", ann);
    let m = rpy_parser::parse(&src, &rpy_parser::ParseOptions::new(rpy_parser::Mode::Exec)).result.ok()?;
    let out = rpy_compiler::compile(&m, &rpy_compiler::CompileOptions::new("<f>", Mode::Exec)).ok()?;
    let ann_fn = common::find_code(&out.code, "__annotate__")?;
    for c in &ann_fn.consts {
        if let Constant::Str(s) = c {
            let t: String = s.iter().map(|&c| char::from_u32(c).unwrap()).collect();
            if t != "return" {
                return Some(t);
            }
        }
    }
    None
}

#[test]
fn future_annotation_unparse() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/Lib/test/test_future_stmt/test_future.py");
    let text = std::fs::read_to_string(path).unwrap();
    let b = text.as_bytes();
    let (mut checked, mut skipped, mut bad) = (0, 0, Vec::new());
    for pat in ["eq(", "self.assertAnnotationEqual("] {
        let mut pos = 0;
        while let Some(k) = text[pos..].find(pat) {
            let at = pos + k;
            pos = at + pat.len();
            if pat == "eq(" && at > 0 && (b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_' || b[at - 1] == b'.') {
                continue;
            }
            let Some((ann, j)) = parse_str_lits(b, pos) else { skipped += 1; continue };
            let rest = &text[j..];
            let rest_t = rest.trim_start();
            let mut expected = ann.clone();
            if rest_t.starts_with(',') {
                let r2 = rest_t[1..].trim_start();
                if let Some(e) = r2.strip_prefix("expected=") {
                    let off = text.len() - e.len();
                    match parse_str_lits(b, off) {
                        Some((e, _)) => expected = e,
                        None => { skipped += 1; continue; }
                    }
                } else if r2.starts_with("is_tuple=True") {
                    expected = ann[1..ann.len() - 1].to_string();
                } else {
                    skipped += 1;
                    continue;
                }
            } else if !rest_t.starts_with(')') {
                skipped += 1;
                continue;
            }
            checked += 1;
            match actual(&ann) {
                None => bad.push(format!("{:?}: failed to compile", ann)),
                Some(a) if a != expected => bad.push(format!("{:?}: got {:?} expected {:?}", ann, a, expected)),
                _ => {}
            }
        }
    }
    eprintln!("checked {} skipped {} bad {}", checked, skipped, bad.len());
    for x in &bad {
        eprintln!("  {}", x);
    }
    assert!(bad.is_empty());
}
