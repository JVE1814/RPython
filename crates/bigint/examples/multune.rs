use rpy_bigint::{mul, nat};
use std::time::Instant;
fn mag(seed: &mut u64, n: usize) -> Vec<u32> { (0..n).map(|_| { *seed ^= *seed << 13; *seed ^= *seed >> 7; *seed ^= *seed << 17; (*seed as u32) & rpy_bigint::MASK }).chain(std::iter::once(1)).collect() }
fn t<F: FnMut()>(mut f: F) -> f64 { f(); let mut it = 1; loop { let s = Instant::now(); for _ in 0..it { f(); } let e = s.elapsed().as_secs_f64(); if e > 0.01 { break; } it *= 2; } let mut best = f64::MAX; for _ in 0..5 { let s = Instant::now(); for _ in 0..it { f(); } best = best.min(s.elapsed().as_secs_f64() / it as f64 * 1e6); } best }
fn main() {
    if let Ok(v) = std::env::var("MT") { let t: Vec<usize> = v.split(",").map(|x| x.parse().unwrap()).collect(); mul::set_thresholds(t[0], t[1], t[2], t[3], t[4]); }
    eprintln!("thresholds {:?}", mul::thresholds());
    let mut seed = 88172645463325252u64;
    let sizes: Vec<usize> = std::env::args().skip(1).map(|s| s.parse().unwrap()).collect();
    let sizes = if sizes.is_empty() { vec![20, 32, 48, 64, 96, 128, 160, 200, 300, 500, 1000, 1500, 2000, 3000, 5000, 8000, 12000, 20000] } else { sizes };
    for &n in &sizes {
        let a = mag(&mut seed, n - 1);
        let b = mag(&mut seed, n - 1);
        let ts = if n <= 3000 { t(|| { std::hint::black_box(mul::mul_schoolbook(&a, &b)); }) } else { f64::NAN };
        let tr = t(|| { std::hint::black_box(mul::mul_recursive(&a, &b)); });
        let td = t(|| { std::hint::black_box(nat::mul(&a, &b)); });
        let tq = t(|| { std::hint::black_box(mul::sqr_recursive(&a)); });
        let tsq = t(|| { std::hint::black_box(nat::sqr(&a)); });
        println!("n={:>6} school {:>10.2}us rec {:>10.2} dispatch {:>10.2} | sqr rec {:>10.2} sqr {:>10.2}", n, ts, tr, td, tq, tsq);
    }
}
