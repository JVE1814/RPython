//! (perf7) crates/launcher/startup.ld lists the functions that run at start-up so the linker
//! places them together (fewer code pages touched by `python3 -c pass`). The list names
//! functions, and names drift as code is merged: when too many entries no longer match a
//! function of the executable, the ordering has decayed and start-up RSS grows again
//! (perf6 measured +0.6 MB file-backed RSS after one merge). Regenerate the list with
//! `tools/bench/perf/gen_order.sh target/release/python3` when this test fails.

use std::collections::HashSet;
use std::process::Command;

/// `Cs<base62 hash>_` (a v0-mangled crate root with its disambiguator) -> `Cs*_`, as
/// tools/bench/perf/gen_order.sh writes the names.
fn without_crate_hashes(name: &str) -> String {
    let b = name.as_bytes();
    let mut out = String::with_capacity(name.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'C' && i + 1 < b.len() && b[i + 1] == b's' {
            let mut j = i + 2;
            while j < b.len() && b[j].is_ascii_alphanumeric() {
                j += 1;
            }
            if j > i + 2 && j < b.len() && b[j] == b'_' {
                out.push_str("Cs*_");
                i = j + 1;
                continue;
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

#[test]
fn startup_order_matches_the_executable() {
    let exe = env!("CARGO_BIN_EXE_python3");
    let script = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/startup.ld")).unwrap();
    let out = Command::new("nm").args(["--defined-only", exe]).output().expect("nm must be available");
    assert!(out.status.success(), "nm failed");
    let syms: HashSet<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let (_, kind, name) = (it.next()?, it.next()?, it.next()?);
            if !matches!(kind, "t" | "T" | "w" | "W") {
                return None;
            }
            // ThinLTO's ".llvm.N" suffixes are matched by the script's patterns, and so
            // are the crate hashes (`Cs<hash>_` -> `Cs*_`)
            Some(without_crate_hashes(name.split(".llvm.").next().unwrap_or(name)))
        })
        .collect();
    if syms.is_empty() {
        return; // stripped executable: nothing to compare
    }
    let mut total = 0usize;
    let mut stale = Vec::new();
    for line in script.lines() {
        // `    *(.text.NAME .text.NAME.llvm.* ...)`
        let Some(rest) = line.trim_start().strip_prefix("*(.text.") else { continue };
        let Some(name) = rest.split_whitespace().next() else { continue };
        total += 1;
        if !syms.contains(name) {
            stale.push(name.to_string());
        }
    }
    assert!(total > 0, "startup.ld lists no functions");
    let ratio = stale.len() as f64 / total as f64;
    assert!(
        ratio < 0.10,
        "{} of {} functions in startup.ld are not in the executable ({:.0}%), e.g. {:?}: \
         run tools/bench/perf/gen_order.sh target/release/python3",
        stale.len(),
        total,
        ratio * 100.0,
        &stale[..stale.len().min(5)]
    );
}
