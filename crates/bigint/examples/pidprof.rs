use rpy_bigint::BigInt;
use std::time::Instant;
fn main() {
    let n = 10000;
    let mut tm = [0f64; 8];
    let names = ["shl", "mul_small", "add", "cmp", "n1*3+a", "divmod", "sub(a-d*t)", "mul10"];
    let mut out = String::new();
    let (mut k, mut n1, mut a, mut d) = (0u64, BigInt::from_i64(1), BigInt::zero(), BigInt::from_i64(1));
    let mut i = 0; let mut k1 = 1u64;
    let total = Instant::now();
    macro_rules! t { ($i:expr, $e:expr) => {{ let s = Instant::now(); let r = $e; tm[$i] += s.elapsed().as_secs_f64(); r }} }
    while i < n {
        k += 1;
        let t = t!(0, &n1 << 1);
        n1 = t!(1, &n1 * &BigInt::from_u64(k));
        a = t!(2, &a + &t);
        k1 += 2;
        let k1b = BigInt::from_u64(k1);
        a = t!(1, &a * &k1b);
        d = t!(1, &d * &k1b);
        if t!(3, a >= n1) {
            let x = t!(4, &(&n1 * &BigInt::from_i64(3)) + &a);
            let (t, u) = t!(5, x.divmod(&d).unwrap());
            let u = t!(2, &u + &n1);
            if t!(3, d > u) {
                out.push(char::from(b'0' + t.to_i64().unwrap() as u8));
                i += 1;
                a = t!(6, &a - &(&d * &t));
                a = t!(7, &a * &BigInt::from_i64(10));
                n1 = t!(7, &n1 * &BigInt::from_i64(10));
            }
        }
    }
    println!("total {:.3}s, k={}, sizes n1={} a={} d={}", total.elapsed().as_secs_f64(), k, n1.mag.len(), a.mag.len(), d.mag.len());
    for (nm, v) in names.iter().zip(tm.iter()) { println!("{:<12} {:.3}s", nm, v); }
}
