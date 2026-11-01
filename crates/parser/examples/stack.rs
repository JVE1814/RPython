//! Parse one deeply nested construct on a thread with a given stack size (for measuring).
//! usage: stack CASE N STACK_BYTES   (exit 0 = ok; abort = overflow)
use rpy_parser::{parse, ParseOptions};
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let n: usize = a[2].parse().unwrap();
    let stack: usize = a[3].parse().unwrap();
    let src = match a[1].as_str() {
        "lambda" => format!("{}0", "lambda:".repeat(n)),
        "not" => format!("{}0", "not ".repeat(n)),
        "unary" => format!("{}0", "-".repeat(n)),
        "await" => format!("{}0", "await ".repeat(n)),
        "ifexp" => format!("{}0", "0 if 0 else ".repeat(n)),
        "power" => format!("{}0", "2**".repeat(n)),
        "calls" => format!("{}x{}", "f(".repeat(n), ")".repeat(n)),
        "trailers" => format!("a{}", "()".repeat(n)),
        "binop" => format!("{}1", "1+".repeat(n)),
        _ => panic!(),
    };
    let h = std::thread::Builder::new().stack_size(stack).spawn(move || {
        let r = parse(&src, &ParseOptions::default());
        let ok = r.result.is_ok();
        let msg = r.result.err().map(|e| e.msg);
        (ok, msg)
    }).unwrap();
    let (ok, msg) = h.join().unwrap();
    println!("ok={} {:?}", ok, msg);
}
