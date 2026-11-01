//! Check that the raw tokenizer stream rejects the CTokenizeTest.test_invalid_syntax inputs.
fn main() {
    let deep = format!("{}a{}", "(".repeat(1000), ")".repeat(1000));
    let cases: Vec<String> = vec![
        "(1+2]", "(1+2}", "{1+2]", "1_", "1.2_", "1e2_", "1e+", "\u{a0}", "€", "0b12", "0b1_2", "0b2", "0b1_", "0b", "0o18",
        "0o1_8", "0o8", "0o1_", "0o", "0x1_", "0x", "1_", "012", "1.2_", "1e2_", "1e+", "'sdfsdf", "'''sdfsdf''", &deep, "]",
        "f'__{\n    x:d\n}__'", " a\n\x00",
    ]
    .into_iter()
    .map(|s| s.to_string())
    .collect();
    for c in &cases {
        let mut err = None;
        for t in rpy_parser::tokenizer::TokenIter::new(c, false, Default::default()) {
            if let Err(e) = t {
                err = Some(e.msg);
                break;
            }
        }
        println!("{:?} -> {:?}", if c.len() > 30 { &c[..30] } else { c }, err);
    }
}
