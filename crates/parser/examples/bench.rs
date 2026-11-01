//! Parse throughput benchmark: `cargo run --release --example bench [-- DIR [ITERS]]`.
//! Reads all .py files under DIR (default: spec/Lib/test), parses them ITERS times.
use rpy_parser::{parse, ParseOptions};

fn collect(dir: &std::path::Path, out: &mut Vec<String>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().map(|e| e == "py").unwrap_or(false) {
            if let Ok(s) = std::fs::read_to_string(&p) {
                out.push(s);
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = args.first().cloned().unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/Lib/test").to_string());
    let iters: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3);
    let mut files = Vec::new();
    collect(std::path::Path::new(&dir), &mut files);
    let bytes: usize = files.iter().map(|s| s.len()).sum();
    fn any_name(_: &str) -> Option<u32> {
        Some(0xFFFD)
    }
    let opts = ParseOptions { hooks: rpy_parser::UnicodeHooks { lookup_name: any_name, ..Default::default() }, ..Default::default() };
    // tokenizer only
    let t0 = std::time::Instant::now();
    for _ in 0..iters {
        for f in &files {
            let _ = rpy_parser::tokenizer::tokenize_all(f);
        }
    }
    let tt = t0.elapsed().as_secs_f64() / iters as f64;
    let t0 = std::time::Instant::now();
    let mut ok = 0;
    for _ in 0..iters {
        ok = 0;
        for f in &files {
            if parse(f, &opts).result.is_ok() {
                ok += 1;
            }
        }
    }
    let pt = t0.elapsed().as_secs_f64() / iters as f64;
    println!(
        "{} files ({} ok), {:.1} MB: tokenize {:.1} MB/s, parse {:.1} MB/s ({:.0} ms per pass)",
        files.len(),
        ok,
        bytes as f64 / 1e6,
        bytes as f64 / 1e6 / tt,
        bytes as f64 / 1e6 / pt,
        pt * 1000.0
    );
}
