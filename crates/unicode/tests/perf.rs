//! Rough timing checks (run with `cargo test --release --test perf -- --ignored --nocapture`).
use rpy_unicode as u;
use std::time::Instant;

#[test]
#[ignore]
fn perf() {
    let t = Instant::now();
    let mut n = 0u32;
    for _ in 0..10 {
        for c in 0..0x110000u32 {
            n += u::is_alpha(c) as u32 + u::is_lowercase(c) as u32;
        }
    }
    println!("predicates: {:.2} ns/call ({n})", t.elapsed().as_nanos() as f64 / (2.0 * 11.0 * 1114112.0));
    let t = Instant::now();
    let mut b = [0; 3];
    for _ in 0..10 {
        for c in 0..0x110000u32 {
            n = n.wrapping_add(u::to_upper_full(c, &mut b) as u32 + b[0]);
        }
    }
    println!("to_upper_full: {:.2} ns/call ({n})", t.elapsed().as_nanos() as f64 / (10.0 * 1114112.0));
    let text: Vec<u32> = "Ça été très difficile à résoudre, n'est-ce pas? 한국어 텍스트".chars().map(|c| c as u32).collect();
    let d = u::normalize(u::Form::NFD, &text);
    let t = Instant::now();
    for _ in 0..100000 {
        n = n.wrapping_add(u::normalize(u::Form::NFC, &d).len() as u32);
    }
    println!("NFC of {} chars: {:.0} ns ({n})", d.len(), t.elapsed().as_nanos() as f64 / 100000.0);
    let t = Instant::now();
    for _ in 0..100000 {
        n = n.wrapping_add(u::lookup_name("LATIN SMALL LETTER A WITH GRAVE").unwrap());
    }
    println!("lookup_name: {:.0} ns ({n})", t.elapsed().as_nanos() as f64 / 100000.0);
    let t = Instant::now();
    for _ in 0..100000 {
        n = n.wrapping_add(u::name(0xE0).unwrap().len() as u32);
    }
    println!("name: {:.0} ns ({n})", t.elapsed().as_nanos() as f64 / 100000.0);
}
