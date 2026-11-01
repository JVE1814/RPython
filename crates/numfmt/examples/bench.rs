//! Micro-benchmark: `cargo run --release --example bench`
use std::time::Instant;

fn main() {
    let mut state: u64 = 0x1234_5678_9abc_def1;
    let mut xs = Vec::new();
    for i in 0..1_000_000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let x = if i % 2 == 0 { f64::from_bits(state >> 2) } else { (state % 100000) as f64 / 100.0 };
        xs.push(x);
    }
    let mut s = String::with_capacity(64);
    let t = Instant::now();
    let mut total = 0;
    for &x in &xs {
        s.clear();
        rpy_numfmt::write_float_repr(&mut s, x);
        total += s.len();
    }
    let d = t.elapsed();
    println!("repr:        {:.1} ns/op ({})", d.as_nanos() as f64 / xs.len() as f64, total);
    let t = Instant::now();
    let mut total = 0;
    for &x in &xs {
        s.clear();
        use std::fmt::Write;
        write!(s, "{:?}", x).unwrap();
        total += s.len();
    }
    let d = t.elapsed();
    println!("rust {{:?}}:  {:.1} ns/op ({})", d.as_nanos() as f64 / xs.len() as f64, total);
    let t = Instant::now();
    let mut total = 0;
    let spec = rpy_numfmt::FormatSpec::parse_float(".2f", "float").unwrap();
    let loc = rpy_numfmt::Locale::c();
    for &x in &xs {
        s.clear();
        rpy_numfmt::format::format_float_spec(&mut s, x % 1e6, &spec, &loc, "float").unwrap();
        total += s.len();
    }
    let d = t.elapsed();
    println!("format .2f:  {:.1} ns/op ({})", d.as_nanos() as f64 / xs.len() as f64, total);
    let t = Instant::now();
    let mut total = 0;
    for &x in &xs {
        s.clear();
        let mut o = rpy_numfmt::percent::PercentOpts::default();
        o.precision = Some(17);
        rpy_numfmt::percent::percent_format_float(&mut s, x, 'g', &o).unwrap();
        total += s.len();
    }
    let d = t.elapsed();
    println!("%.17g:       {:.1} ns/op ({})", d.as_nanos() as f64 / xs.len() as f64, total);
    let strs: Vec<String> = xs.iter().map(|x| rpy_numfmt::float_repr(*x)).collect();
    let t = Instant::now();
    let mut acc = 0.0;
    for s in &strs {
        acc += rpy_numfmt::parse_float(s).unwrap();
    }
    let d = t.elapsed();
    println!("parse:       {:.1} ns/op ({})", d.as_nanos() as f64 / xs.len() as f64, acc);
}
