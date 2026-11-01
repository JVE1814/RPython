use rpy_bigint::{div, nat};
use std::time::Instant;
fn mag(seed: &mut u64, n: usize) -> Vec<u32> { (0..n).map(|_| { *seed ^= *seed << 13; *seed ^= *seed >> 7; *seed ^= *seed << 17; (*seed as u32) & rpy_bigint::MASK | if false {0} else {0} }).chain(std::iter::once(1)).collect() }
fn t<F: FnMut()>(mut f: F) -> f64 { f(); let it = 3; let s = Instant::now(); for _ in 0..it { f(); } s.elapsed().as_secs_f64() / it as f64 * 1e3 }
fn main() {
    let mut seed = 88172645463325252u64;
    for &n in &[100usize, 200, 400, 1000, 2000, 4000, 10000, 30000, 100000] {
        let b = mag(&mut seed, n);
        let a = mag(&mut seed, 2 * n);
        let tm = t(|| { std::hint::black_box(nat::mul(&a[..n], &b)); });
        let tk = if n <= 10000 { t(|| { std::hint::black_box(div::divrem_knuth(&a, &b)); }) } else { f64::NAN };
        let tb = t(|| { std::hint::black_box(div::divrem_bz(&a, &b)); });
        let tn = t(|| { std::hint::black_box(div::divrem_large(&a, &b)); });
        let dv = div::Divisor::new(&b);
        let tc = t(|| { std::hint::black_box(dv.divrem(&a)); });
        println!("n={:>6} mul {:>9.3}ms knuth {:>9.3} bz {:>9.3} newton {:>9.3} cached-barrett {:>9.3}", n, tm, tk, tb, tn, tc);
    }
}
