#![allow(dead_code)]
use rpy_compiler::{bytecode::*, compile, CompileOptions, Mode};
use rpy_parser::{parse, Mode as PMode, ParseOptions};

pub fn parse_src(src: &str, mode: Mode) -> rpy_parser::ast::Mod {
    let pm = match mode {
        Mode::Exec => PMode::Exec,
        Mode::Eval => PMode::Eval,
        Mode::Single => PMode::Single,
        Mode::FuncType => PMode::FuncType,
    };
    parse(src, &ParseOptions::new(pm)).result.unwrap_or_else(|e| panic!("parse error: {e}\n{src}"))
}

pub fn try_compile(src: &str) -> Result<rpy_compiler::CompileOutput, rpy_compiler::CompileError> {
    let m = parse_src(src, Mode::Exec);
    compile(&m, &CompileOptions::new("<test>", Mode::Exec))
}

pub fn comp(src: &str) -> CodeUnit {
    match try_compile(src) {
        Ok(o) => o.code,
        Err(e) => panic!("compile error: {e}\n{src}"),
    }
}

pub fn comp_err(src: &str) -> rpy_compiler::CompileError {
    match try_compile(src) {
        Ok(_) => panic!("expected compile error for:\n{src}"),
        Err(e) => e,
    }
}

pub fn ops(c: &CodeUnit) -> Vec<Op> {
    c.code.iter().map(|&w| decode(w).0).collect()
}

pub fn find_code<'a>(c: &'a CodeUnit, name: &str) -> Option<&'a CodeUnit> {
    if c.name == name {
        return Some(c);
    }
    for k in &c.consts {
        if let Constant::Code(sub) = k {
            if let Some(f) = find_code(sub, name) {
                return Some(f);
            }
        }
    }
    None
}
