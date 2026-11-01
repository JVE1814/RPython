mod common;
use rpy_compiler::{compile, CompileOptions, ErrorKind, Mode};

fn run_in_stack(stack: usize, src: String) -> Result<(), ErrorKind> {
    std::thread::Builder::new()
        .stack_size(stack)
        .spawn(move || {
            let m = common::parse_src(&src, Mode::Exec);
            compile(&m, &CompileOptions::new("<d>", Mode::Exec)).map(|_| ()).map_err(|e| e.kind)
        })
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn deep_binop_chain_ok() {
    // 1900-deep left-nested chain compiles within 16 MB of stack
    let src = format!("x = {}\n", vec!["a"; 1900].join(" + "));
    assert_eq!(run_in_stack(64 << 20, src), Ok(()));
}

#[test]
fn too_deep_is_recursion_error() {
    let src = format!("x = {}\n", vec!["a"; 3000].join(" + "));
    assert_eq!(run_in_stack(64 << 20, src), Err(ErrorKind::RecursionError));
}

#[test]
fn nested_calls_ok() {
    // a()()()... 100 deep (test_compile.test_compiler_recursion_limit success depth)
    for rep in ["()", ".b", "[0]"] {
        let src = format!("a{}\n", rep.repeat(100));
        assert_eq!(run_in_stack(8 << 20, src), Ok(()));
    }
}

#[test]
#[ignore]
fn measure_stack() {
    let mb: usize = std::env::var("STACK_MB").unwrap().parse().unwrap();
    let src = format!("x = {}\n", vec!["a"; 1990].join(" + "));
    assert_eq!(run_in_stack(mb << 20, src), Ok(()));
}
