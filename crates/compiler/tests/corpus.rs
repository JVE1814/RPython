//! Compile every .py file under spec/Lib/test that the parser accepts.
//! Files that are expected to fail compilation (badsyntax_* etc.) may error; any other
//! compile error or any panic fails the test. Run with --nocapture to see the counts.

use rpy_compiler::{compile, CompileOptions, Mode};
use rpy_parser::{parse_bytes, Mode as PMode, ParseOptions};
use std::path::{Path, PathBuf};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().collect();
    entries.sort_by_key(|e| e.path());
    for e in entries {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().map(|x| x == "py").unwrap_or(false) {
            out.push(p);
        }
    }
}

fn expected_failure(p: &Path) -> bool {
    let n = p.file_name().unwrap().to_string_lossy();
    let s = p.to_string_lossy();
    n.starts_with("badsyntax") || n.starts_with("bad_") || s.contains("/badsyntax/") || n.contains("badsyntax")
}

#[test]
fn compile_spec_corpus() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/Lib/test");
    let mut files = Vec::new();
    walk(&root, &mut files);
    let (mut parsed, mut compiled, mut expected_err, mut parse_fail) = (0, 0, 0, 0);
    let mut failures: Vec<String> = Vec::new();
    let t0 = std::time::Instant::now();
    for f in &files {
        let bytes = std::fs::read(f).unwrap();
        let mut po = ParseOptions::new(PMode::Exec);
        po.filename = f.to_string_lossy().into_owned();
        let m = match parse_bytes(&bytes, &po).result {
            Ok(m) => m,
            Err(_) => {
                parse_fail += 1;
                continue;
            }
        };
        parsed += 1;
        let opts = CompileOptions::new(f.to_string_lossy(), Mode::Exec);
        let r = std::panic::catch_unwind(|| compile(&m, &opts));
        match r {
            Ok(Ok(_)) => compiled += 1,
            Ok(Err(e)) => {
                if expected_failure(f) {
                    expected_err += 1;
                } else {
                    failures.push(format!("{}: {}", f.strip_prefix(&root).unwrap().display(), e));
                }
            }
            Err(_) => failures.push(format!("{}: PANIC", f.strip_prefix(&root).unwrap().display())),
        }
    }
    eprintln!(
        "files={} parse_fail={} parsed={} compiled={} expected_errors={} unexpected={} time={:?}",
        files.len(),
        parse_fail,
        parsed,
        compiled,
        expected_err,
        failures.len(),
        t0.elapsed()
    );
    for x in &failures {
        eprintln!("  {}", x);
    }
    assert!(failures.is_empty(), "{} unexpected failures", failures.len());
}
