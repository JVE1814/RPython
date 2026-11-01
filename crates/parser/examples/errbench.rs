//! Timing of error reporting (second pass) on large / deeply nested inputs.
use rpy_parser::{parse, ParseOptions};
fn main() {
    let big = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/Lib/test/test_typing.py")).unwrap();
    let cases = vec![
        ("big file + error at end", format!("{}\nx = = 1\n", big)),
        ("big file + unclosed paren at end", format!("{}\nx = (1,\n", big)),
        ("nested parens 190 + error", format!("{}1 1{}", "(".repeat(190), ")".repeat(190))),
        ("nested calls 190 + error", format!("{}x y{}", "f(".repeat(190), ")".repeat(190))),
        ("nested lists 190 + error", format!("{}x y{}", "[".repeat(190), "]".repeat(190))),
        ("nested unary 2000", format!("x = {}1", "-".repeat(2000))),
        ("long binop chain", format!("x = {}1", "1 + ".repeat(20000))),
        ("long binop chain + error", format!("x = {}1 1", "1 + ".repeat(20000))),
        ("many nested ifs", (0..99).map(|i| format!("{}if x:\n", " ".repeat(i))).collect::<String>() + &" ".repeat(99) + "pass pass\n"),
    ];
    for (name, src) in &cases {
        let t = std::time::Instant::now();
        let r = parse(src, &ParseOptions::default());
        let el = t.elapsed();
        let msg = match &r.result {
            Ok(_) => "ok".to_string(),
            Err(e) => format!("{} (line {})", e.msg, e.lineno),
        };
        println!("{:40} {:>10.2?}  {}", name, el, msg);
    }
}
