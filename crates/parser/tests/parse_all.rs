//! Parse every `.py` file under spec/Lib/test. All must parse except the files listed in
//! `EXPECTED_INVALID` (see NOTES.md for the justification of each entry).

mod common;
use rpy_parser::{parse_bytes, ParseOptions};
use std::path::{Path, PathBuf};

/// Files that are intentionally not valid Python 3.15 source (paths relative to Lib/test).
const EXPECTED_INVALID: &[&str] = &[
    "tokenizedata/bad_coding.py",
    "tokenizedata/bad_coding2.py",
    "tokenizedata/badsyntax_3131.py",
    "tokenizedata/badsyntax_pep3120.py",
];

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().map(|e| e == "py").unwrap_or(false) {
            out.push(p);
        }
    }
}

#[test]
fn parse_all_spec_files() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/Lib/test");
    let mut files = Vec::new();
    collect(&root, &mut files);
    let mut ok = 0;
    let mut unexpected_fail = Vec::new();
    let mut unexpected_ok = Vec::new();
    let mut expected_fail = 0;
    let mut bytes = 0usize;
    let start = std::time::Instant::now();
    for f in &files {
        let rel = f.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
        let src = std::fs::read(f).unwrap();
        bytes += src.len();
        let out = parse_bytes(&src, &ParseOptions { hooks: common::hooks(), ..Default::default() });
        let invalid = EXPECTED_INVALID.contains(&rel.as_str());
        match (out.result, invalid) {
            (Ok(_), false) => ok += 1,
            (Ok(_), true) => unexpected_ok.push(rel),
            (Err(_), true) => expected_fail += 1,
            (Err(e), false) => unexpected_fail.push(format!("{}: {} (line {}, offset {})", rel, e.msg, e.lineno, e.offset)),
        }
    }
    let el = start.elapsed();
    eprintln!(
        "parse_all: {} files, {} ok, {} expected failures, {} unexpected failures, {} unexpectedly ok; {:.1} MB in {:.2?}",
        files.len(),
        ok,
        expected_fail,
        unexpected_fail.len(),
        unexpected_ok.len(),
        bytes as f64 / 1e6,
        el
    );
    for f in &unexpected_fail {
        eprintln!("FAIL {}", f);
    }
    for f in &unexpected_ok {
        eprintln!("UNEXPECTED OK {}", f);
    }
    assert!(unexpected_fail.is_empty() && unexpected_ok.is_empty());
}
