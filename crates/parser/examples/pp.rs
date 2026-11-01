//! Debug helper: `cargo run --example pp -- [-m exec|eval|single|func_type] [-t] (-c SRC | FILE)`
//! prints the to_tuple dump or the SyntaxError. `-k` dumps tokenize tokens instead.
use rpy_parser::{dump, parse, parse_bytes, Mode, ParseOptions};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut mode = Mode::Exec;
    let mut src: Option<Vec<u8>> = None;
    let mut tokens = false;
    let mut raw_tokens = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-m" => {
                i += 1;
                mode = match args[i].as_str() {
                    "eval" => Mode::Eval,
                    "single" => Mode::Single,
                    "func_type" => Mode::FuncType,
                    _ => Mode::Exec,
                };
            }
            "-k" => tokens = true,
            "-K" => raw_tokens = true,
            "-c" => {
                i += 1;
                src = Some(args[i].replace("\\n", "\n").replace("\\t", "\t").into_bytes());
            }
            f => src = Some(std::fs::read(f).unwrap()),
        }
        i += 1;
    }
    let src = src.expect("no source");
    if tokens || raw_tokens {
        let s = String::from_utf8(src).unwrap();
        for t in rpy_parser::tokenizer::TokenIter::new(&s, tokens, Default::default()) {
            match t {
                Ok(t) => println!("{:14} {:?} {:?} {:?}", t.exact_type.name(), t.string, t.start, t.end),
                Err(e) => {
                    println!("ERROR {:?}", e);
                    break;
                }
            }
        }
        return;
    }
    let opts = ParseOptions { mode, ..Default::default() };
    let out = parse_bytes(&src, &opts);
    for w in &out.warnings {
        println!("WARNING {:?}", w);
    }
    match out.result {
        Ok(m) => println!("{}", dump::to_tuple(&m)),
        Err(e) => println!("ERROR {:?}", e),
    }
    let _ = parse;
}
