//! Rough benchmarks: `cargo run --release -p rpy-bigint --example bench [filter]`
use rpy_bigint::{math, nat, radix, BigInt, IntRef};
use std::time::Instant;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn mag(&mut self, n: usize) -> Vec<u32> {
        let mut v: Vec<u32> = (0..n).map(|_| (self.next() as u32) & rpy_bigint::MASK).collect();
        v[n - 1] |= 1;
        v
    }
}

fn time<T>(name: &str, iters: u32, mut f: impl FnMut() -> T) -> f64 {
    let _ = f();
    let t = Instant::now();
    for _ in 0..iters {
        std::hint::black_box(f());
    }
    let per = t.elapsed().as_secs_f64() / iters as f64;
    if per >= 1e-3 {
        println!("{:<48} {:>10.3} ms", name, per * 1e3);
    } else {
        println!("{:<48} {:>10.3} us", name, per * 1e6);
    }
    per
}

/// Benchmarks-game pidigits (the classic Python program's arithmetic).
fn pidigits(n: usize) -> String {
    let mut out = String::new();
    let (mut k, mut n1, mut a, mut d) = (0u64, BigInt::from_i64(1), BigInt::zero(), BigInt::from_i64(1));
    let mut i = 0;
    let mut k1 = 1u64;
    while i < n {
        k += 1;
        let t = &n1 << 1;
        n1 = &n1 * &BigInt::from_u64(k);
        a = &a + &t;
        k1 += 2;
        let k1b = BigInt::from_u64(k1);
        a = &a * &k1b;
        d = &d * &k1b;
        if a >= n1 {
            let (t, u) = (&(&n1 * &BigInt::from_i64(3)) + &a).divmod(&d).unwrap();
            let u = &u + &n1;
            if d > u {
                out.push(char::from(b'0' + t.to_i64().unwrap() as u8));
                i += 1;
                a = &a - &(&d * &t);
                a = &a * &BigInt::from_i64(10);
                n1 = &n1 * &BigInt::from_i64(10);
            }
        }
    }
    out
}

fn main() {
    let filter = std::env::args().nth(1).unwrap_or_default();
    let want = |s: &str| filter.is_empty() || s.contains(&filter);
    let mut r = Rng(0x1234_5678_9abc_def1);

    if want("medium") {
        for &nd in &[10usize, 35, 100, 350, 1000, 3500] {
            // nd 30-bit digits ~ 9*nd decimal digits
            let a = r.mag(nd);
            let b = r.mag(nd);
            let iters = (2_000_000 / (nd * nd)).max(3) as u32;
            time(&format!("mul {}x{} digits (~{} dec)", nd, nd, nd * 9), iters, || nat::mul(&a, &b));
            time(&format!("sqr {} digits", nd), iters, || nat::sqr(&a));
            let p = nat::mul(&a, &b);
            time(&format!("divrem {}/{} digits", 2 * nd, nd), iters, || nat::divrem(&p, &b));
            time(&format!("divrem {}/{} digits (small quotient)", nd + 1, nd), iters * 10, || nat::divrem(&r.mag(nd + 1), &b));
            time(&format!("add {}+{} digits", nd, nd), 100_000, || nat::add(&a, &b));
            time(&format!("mul_u32 {} digits", nd), 100_000, || nat::mul_u32(&a, 12345));
            time(&format!("to_string {} digits", nd), (iters / 4).max(3), || radix::to_string(IntRef::new(false, &a), 10));
            let sa = radix::to_string(IntRef::new(false, &a), 10);
            time(&format!("from_str {} dec digits", sa.len()), (iters / 4).max(3), || radix::from_digits(sa.as_bytes(), 10));
            time(&format!("gcd {} digits", nd), (iters / 10).max(3), || math::gcd_mag(&a, &b));
        }
    }
    if want("pidigits") {
        time("pidigits(2000)", 3, || pidigits(2000));
        let t = Instant::now();
        let s = pidigits(10000);
        println!("{:<48} {:>10.3} ms", "pidigits(10000)", t.elapsed().as_secs_f64() * 1e3);
        assert!(s.starts_with("31415926535897932384"));
    }
    if want("huge") {
        // 10^6 decimal digit operands
        let ten = BigInt::from_i64(10);
        let t = Instant::now();
        let x = &ten.pow_u64(1_000_000).unwrap() - &BigInt::from_i64(12345);
        println!("{:<48} {:>10.3} ms", "10**1000000", t.elapsed().as_secs_f64() * 1e3);
        let y = BigInt::from_parts(false, r.mag(x.mag.len()));
        time("mul 1e6 x 1e6 decimal digits", 3, || &x * &y);
        time("sqr 1e6 decimal digits", 3, || nat::sqr(&x.mag));
        let p = &x * &y;
        time("divmod 2e6 / 1e6 decimal digits", 2, || p.divmod(&y).unwrap());
        let mut s = String::new();
        time("str(1e6-digit int)", 2, || {
            s = x.to_string();
        });
        assert_eq!(s.len(), 1_000_000);
        time("int(1e6-digit str)", 2, || radix::from_digits(s.as_bytes(), 10).unwrap());
        let ys = y.to_string();
        time("str(random 1e6-digit int)", 2, || y.to_string());
        time("int(random 1e6-digit str)", 2, || radix::from_digits(ys.as_bytes(), 10).unwrap());
        time("hex(1e6-digit int)", 3, || x.to_str_radix(16));
        time("factorial(100000)", 3, || math::factorial_mag(100_000));
        time("isqrt(1e6-digit int)", 2, || math::isqrt_mag(&y.mag));
        time("3**600000", 3, || BigInt::from_i64(3).pow_u64(600_000).unwrap());
    }
    if want("tenmillion") {
        let k = 1_000_000usize;
        let sn = "1234567890".repeat(k);
        let t = Instant::now();
        let n = radix::from_digits(sn.as_bytes(), 10).unwrap();
        println!("{:<48} {:>10.3} ms", "int(10M digit str)", t.elapsed().as_secs_f64() * 1e3);
        let t = Instant::now();
        let back = radix::to_string(IntRef::new(false, &n), 10);
        println!("{:<48} {:>10.3} ms", "str(10M digit int)", t.elapsed().as_secs_f64() * 1e3);
        assert_eq!(back, sn);
    }
}
