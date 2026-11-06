//! Usage: cargo run --example wcdis -- <file.py | -c source> [-O]
//! Disassembles the public (CPython 3.15 wordcode) output of the compiler.
use rpy_compiler::{compile_wordcode, wordcode::dis::disassemble, CompileOptions, Mode};
use rpy_parser::{parse, Mode as PMode, ParseOptions};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let src = if args.get(1).map(|s| s == "-c").unwrap_or(false) {
        args[2].clone()
    } else {
        std::fs::read_to_string(&args[1]).expect("read")
    };
    let m = match parse(&src, &ParseOptions::new(PMode::Exec)).result {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1)
        }
    };
    let mut o = CompileOptions::new("<dis>", Mode::Exec);
    if args.iter().any(|a| a == "-O") {
        o.optimize = 1;
    }
    match compile_wordcode(&m, &o) {
        Ok(out) => print!("{}", disassemble(&out.code)),
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1)
        }
    }
}
