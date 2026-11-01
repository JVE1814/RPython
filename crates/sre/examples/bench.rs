//! Micro benchmarks modelled on pyperformance's regex_dna / regex_effbot /
//! regex_v8 workloads.  Run with:
//!   cargo run --release --example bench -p rpy-sre

use rpy_sre::*;
use std::time::Instant;

fn cp(s: &str) -> Vec<u32> {
    s.chars().map(|c| c as u32).collect()
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn count<T: CodeUnit>(p: &Pattern, s: &[T]) -> usize {
    p.scanner(s, 0, usize::MAX).count()
}

fn sub<T: CodeUnit>(p: &Pattern, repl: &[T], s: &[T]) -> Vec<T> {
    let mut out = Vec::with_capacity(s.len());
    let mut last = 0;
    for m in p.scanner(s, 0, usize::MAX) {
        out.extend_from_slice(&s[last..m.start()]);
        out.extend_from_slice(repl);
        last = m.end();
    }
    out.extend_from_slice(&s[last..]);
    out
}

fn time<F: FnMut() -> usize>(name: &str, iters: usize, mut f: F) {
    let t = Instant::now();
    let mut acc = 0;
    for _ in 0..iters {
        acc += f();
    }
    let dt = t.elapsed().as_secs_f64() / iters as f64;
    println!("{name:<44} {:>10.3} ms/iter  (checksum {acc})", dt * 1e3);
}

fn main() {
    // ---------------- regex_dna-like
    let mut rng = Rng(42);
    let n = 500_000;
    let mut seq = String::with_capacity(n + n / 60);
    for i in 0..n {
        let r = rng.next() % 100;
        let c = match r {
            0..=29 => 'a',
            30..=49 => 'c',
            50..=69 => 'g',
            70..=98 => 't',
            _ => ['B', 'D', 'H', 'K', 'M', 'N', 'R', 'S', 'V', 'W', 'Y'][(rng.next() % 11) as usize],
        };
        seq.push(c);
        if i % 60 == 59 {
            seq.push('\n');
        }
    }
    let seq8: Vec<u8> = seq.bytes().collect();
    let variants = [
        "agggtaaa|tttaccct",
        "[cgt]gggtaaa|tttaccc[acg]",
        "a[act]ggtaaa|tttacc[agt]t",
        "ag[act]gtaaa|tttac[agt]ct",
        "agg[act]taaa|ttta[agt]cct",
        "aggg[acg]aaa|ttt[cgt]ccct",
        "agggt[cgt]aa|tt[acg]accct",
        "agggta[cgt]a|t[acg]taccct",
        "agggtaa[cgt]|[acg]ttaccct",
    ];
    let pats: Vec<Pattern> = variants.iter().map(|v| compile_str(v, 0).unwrap()).collect();
    time("dna: 9 variant findall (500KB, latin1)", 5, || pats.iter().map(|p| count(p, &seq8)).sum());
    let substs = [
        ("tHa[Nt]", "<4>"),
        ("aND|caN|Ha[DS]|WaS", "<3>"),
        ("a[NSt]|BY", "<2>"),
        ("<[^>]*>", "|"),
        ("\\|[^|][^|]*\\|", "-"),
    ];
    let sp: Vec<(Pattern, Vec<u8>)> =
        substs.iter().map(|(p, r)| (compile_str(p, 0).unwrap(), r.bytes().collect())).collect();
    time("dna: substitutions", 5, || {
        let mut s = seq8.clone();
        for (p, r) in &sp {
            s = sub(p, r, &s);
        }
        s.len()
    });
    let clean = compile_str(">.*\n|\n", 0).unwrap();
    time("dna: strip newlines sub('>.*\\n|\\n')", 5, || sub(&clean, &[], &seq8).len());
    let pi = compile_str("agggtaaa|tttaccct", flags::IGNORECASE).unwrap();
    time("dna: ignorecase variant", 5, || count(&pi, &seq8));

    // ---------------- regex_effbot-like (many small searches)
    let effbot: Vec<(&str, String)> = vec![
        ("Python|Perl", "Perl".into()),
        ("(Python|Perl)", "Perl".into()),
        ("(?:Python|Perl)", "Perl".into()),
        ("Python", "Python".into()),
        ("Python$", "Python".into()),
        ("Python\\Z", "Python".into()),
        ("(Python)", "Python".into()),
        ("(?:Python)", "Python".into()),
        ("(Python)\\1", "PythonPython".into()),
        ("([0a-z][a-z0-9]*,)+", "a5,b7,c9,".into()),
        ("(?:[0a-z][a-z0-9]*,)+", "a5,b7,c9,".into()),
        ("([a-z][a-z0-9]*,)+", "a5,b7,c9,".into()),
        ("(?:[a-z][a-z0-9]*,)+", "a5,b7,c9,".into()),
        (".*Python", "Python".into()),
        (".*Python.*", "Python".into()),
        (".*(Python)", "Python".into()),
    ];
    let mut cases = Vec::new();
    for (p, s) in &effbot {
        let pat = compile_str(p, 0).unwrap();
        for pad in [0usize, 10, 100, 1000] {
            let mut x = "-".repeat(pad);
            x.push_str(s);
            x.push_str(&"-".repeat(pad));
            cases.push((pat.clone(), cp(&x)));
        }
    }
    time("effbot: 64 searches", 2000, || {
        cases.iter().filter(|(p, s)| p.search(s, 0, usize::MAX).is_some()).count()
    });
    time("effbot: compile 16 patterns", 2000, || {
        effbot.iter().map(|(p, _)| compile_str(p, 0).unwrap().groups()).sum()
    });

    // ---------------- regex_v8-like text processing
    let mut text = String::new();
    let words = ["the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "foo@example.com",
        "<b>bold</b>", "12345", "  ", "x=1;y=2", "http://www.example.com/path?q=1&r=2"];
    for i in 0..20000 {
        text.push_str(words[(rng.next() % words.len() as u64) as usize]);
        text.push(if i % 13 == 0 { '\n' } else { ' ' });
    }
    let t8: Vec<u8> = text.bytes().collect();
    let v8pats = [
        r"\s+", r"^\s*|\s*$", r"[^\d\s]+", r"(\w+)@(\w+)\.com", r"<[^>]*>", r"\b\w+\b",
        r"(?i)QUICK|LAZY", r"https?://([^/]+)(/[^\s?]*)?(\?\S*)?", r"\d+", r"(\w+)=(\w+);?",
        r"[a-z]+ing\b", r"(?m)^\w+", r"o(?=v)|(?<=d)o",
    ];
    let vps: Vec<Pattern> = v8pats.iter().map(|p| compile_str(p, 0).unwrap()).collect();
    for (src, p) in v8pats.iter().zip(&vps) {
        time(&format!("v8: findall {src}"), 20, || count(p, &t8));
    }
    // backtracking-heavy
    let bt = compile_str(r"(a|b)*?c", 0).unwrap();
    let s = cp(&("ab".repeat(100000) + "c"));
    time("backtrack: (a|b)*?c on 200K", 5, || bt.search(&s, 0, usize::MAX).map_or(0, |m| m.end()));
    let bt = compile_str(r"(?:ab|cd)*x", 0).unwrap();
    let s = cp(&"ab".repeat(100000));
    time("backtrack: (?:ab|cd)*x fail on 200K", 5, || {
        bt.match_at(&s, 0, usize::MAX).map_or(1, |_| 0)
    });
}
