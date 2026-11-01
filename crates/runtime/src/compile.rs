//! Source -> code object pipeline (parser + compiler), SyntaxError conversion,
//! and compile-time warnings.

use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::*;
use crate::types::{int, none, str as pystr, tuple};
use rpy_compiler::{CompileOptions, Mode as CMode};
use rpy_parser::{Mode as PMode, ParseOptions, UnicodeHooks};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Exec,
    Eval,
    Single,
    FuncType,
}

impl Mode {
    pub fn from_str(s: &str) -> Option<Mode> {
        Some(match s {
            "exec" => Mode::Exec,
            "eval" => Mode::Eval,
            "single" => Mode::Single,
            "func_type" => Mode::FuncType,
            _ => return None,
        })
    }
    fn p(self) -> PMode {
        match self {
            Mode::Exec => PMode::Exec,
            Mode::Eval => PMode::Eval,
            Mode::Single => PMode::Single,
            Mode::FuncType => PMode::FuncType,
        }
    }
    fn c(self) -> CMode {
        match self {
            Mode::Exec => CMode::Exec,
            Mode::Eval => CMode::Eval,
            Mode::Single => CMode::Single,
            Mode::FuncType => CMode::FuncType,
        }
    }
}

fn nfkc(s: &str) -> String {
    let cps: Vec<u32> = s.chars().map(|c| c as u32).collect();
    let out = rpy_unicode::normalize(rpy_unicode::Form::NFKC, &cps);
    out.into_iter().filter_map(char::from_u32).collect()
}
fn lookup_name(s: &str) -> Option<u32> {
    rpy_unicode::lookup_name(s)
}
fn xid_start(c: char) -> bool {
    rpy_unicode::is_xid_start(c as u32)
}
fn xid_continue(c: char) -> bool {
    rpy_unicode::is_xid_continue(c as u32)
}
fn printable(c: char) -> bool {
    rpy_unicode::is_printable(c as u32)
}

pub fn unicode_hooks() -> UnicodeHooks {
    UnicodeHooks { lookup_name, is_xid_start: xid_start, is_xid_continue: xid_continue, is_printable: printable, nfkc }
}

pub fn parse_options(mode: Mode, flags: u32, feature_version: Option<u32>) -> ParseOptions {
    let mut o = ParseOptions::new(mode.p());
    o.type_comments = flags & rpy_compiler::cf::PYCF_TYPE_COMMENTS != 0;
    o.feature_version = feature_version;
    o.barry_as_flufl = flags & rpy_compiler::cf::CO_FUTURE_BARRY_AS_BDFL != 0;
    o.allow_incomplete_input = flags & rpy_compiler::cf::PYCF_ALLOW_INCOMPLETE_INPUT != 0;
    o.hooks = unicode_hooks();
    o
}

/// Build and set a SyntaxError (or subclass) exception.
pub fn raise_syntax_error(
    class: *mut crate::ffi::PyTypeObject,
    msg: &str,
    filename: &str,
    lineno: i32,
    offset: i32,
    text: Option<&str>,
    end_lineno: Option<i32>,
    end_offset: Option<i32>,
) -> PyErr {
    let opt_int = |v: Option<i32>| v.map(|x| int::new_int(x as i64)).unwrap_or_else(none::none_ref);
    let info = tuple::new_tuple(vec![
        pystr::new_str(filename),
        int::new_int(lineno as i64),
        if offset < 0 { none::none_ref() } else { int::new_int(offset as i64) },
        text.map(pystr::new_str).unwrap_or_else(none::none_ref),
        opt_int(end_lineno),
        opt_int(end_offset),
    ]);
    match crate::types::exceptions::make_exception(class, &[pystr::new_str(msg), info]) {
        Ok(e) => set_exc(e),
        Err(_) => {}
    }
    PyErr
}

fn source_line(src: &str, lineno: i32) -> Option<String> {
    if lineno < 1 {
        return None;
    }
    src.split_inclusive('\n').nth((lineno - 1) as usize).map(|s| s.to_string())
}

fn parse_error(e: &rpy_parser::SyntaxError, filename: &str) -> PyErr {
    let class = match e.kind.class_name() {
        "IndentationError" => crate::types::exceptions::IndentationError(),
        "TabError" => crate::types::exceptions::TabError(),
        "_IncompleteInputError" | "IncompleteInputError" => crate::types::exceptions::IncompleteInputError(),
        _ if e.msg == "incomplete input" => crate::types::exceptions::IncompleteInputError(),
        _ => crate::types::exceptions::SyntaxError(),
    };
    raise_syntax_error(class, &e.msg, filename, e.lineno, e.offset, e.text.as_deref(), e.end_lineno, e.end_offset)
}

pub fn compile_error(e: &rpy_compiler::CompileError, filename: &str, src: Option<&str>) -> PyErr {
    use rpy_compiler::ErrorKind as K;
    let class = match e.kind {
        K::SyntaxError => crate::types::exceptions::SyntaxError(),
        K::IndentationError => crate::types::exceptions::IndentationError(),
        K::RecursionError => return err!(RecursionError, "{}", e.msg),
        K::ValueError => return err!(ValueError, "{}", e.msg),
        K::TypeError => return err!(TypeError, "{}", e.msg),
        K::SystemError => return err!(SystemError, "{}", e.msg),
        K::MemoryError => return memory_error(),
    };
    let text = src.and_then(|s| source_line(s, e.lineno));
    let to_offset = |line: i32, col: i32| -> i32 {
        // convert byte column to 1-based char offset
        match src.and_then(|s| source_line(s, line)) {
            Some(l) => {
                let b = (col.max(0) as usize).min(l.len());
                l[..b].chars().count() as i32 + 1
            }
            None => col + 1,
        }
    };
    raise_syntax_error(
        class,
        &e.msg,
        filename,
        e.lineno,
        to_offset(e.lineno, e.col_offset),
        text.as_deref(),
        Some(e.end_lineno),
        Some(to_offset(e.end_lineno, e.end_col_offset)),
    )
}

/// Emit a compile-time warning; if the filter turns it into an error, raise SyntaxError.
fn emit_warning(category: &str, msg: &str, error_msg: &str, filename: &str, lineno: i32, offset: i32, text: Option<&str>) -> PyResult<()> {
    let cat = match category {
        "DeprecationWarning" => crate::types::exceptions::DeprecationWarning(),
        _ => crate::types::exceptions::SyntaxWarning(),
    };
    let r = crate::modules::warnings::warn_explicit(cat, msg, filename, lineno);
    if r.is_err() {
        if exc_matches(cat) {
            clear_exc();
            return Err(raise_syntax_error(
                crate::types::exceptions::SyntaxError(),
                error_msg,
                filename,
                lineno,
                offset,
                text,
                None,
                None,
            ));
        }
        return r;
    }
    Ok(())
}

pub struct Compiled {
    pub code: PyRef,
    pub future_flags: u32,
}

/// Compile source text to a code object.
pub fn compile_str(src: &str, filename: &str, mode: Mode, flags: u32, optimize: i32) -> PyResult<Compiled> {
    let popts = parse_options(mode, flags, None);
    let out = rpy_parser::parse(src, &popts);
    for w in &out.warnings {
        let cat = format!("{:?}", w.category);
        emit_warning(&cat, &w.msg, &w.error_msg, filename, w.lineno, w.col_offset + 1, source_line(src, w.lineno).as_deref())?;
    }
    let module = match out.result {
        Ok(m) => m,
        Err(e) => return Err(parse_error(&e, filename)),
    };
    compile_ast(&module, filename, mode, flags, optimize, Some(src))
}

/// Compile source bytes (encoding detection, BOM).
pub fn compile_bytes(src: &[u8], filename: &str, mode: Mode, flags: u32, optimize: i32) -> PyResult<Compiled> {
    let popts = parse_options(mode, flags, None);
    let out = rpy_parser::parse_bytes(src, &popts);
    let text = String::from_utf8_lossy(src).into_owned();
    for w in &out.warnings {
        let cat = format!("{:?}", w.category);
        emit_warning(&cat, &w.msg, &w.error_msg, filename, w.lineno, w.col_offset + 1, source_line(&text, w.lineno).as_deref())?;
    }
    let module = match out.result {
        Ok(m) => m,
        Err(e) => return Err(parse_error(&e, filename)),
    };
    compile_ast(&module, filename, mode, flags, optimize, Some(&text))
}

pub fn compile_ast(module: &rpy_parser::ast::Mod, filename: &str, mode: Mode, flags: u32, optimize: i32, src: Option<&str>) -> PyResult<Compiled> {
    let mut copts = CompileOptions::new(filename, mode.c());
    copts.flags = flags;
    copts.optimize = if optimize < 0 { crate::modules::sys::optimize_flag() } else { optimize };
    let out = match rpy_compiler::compile(module, &copts) {
        Ok(o) => o,
        Err(e) => return Err(compile_error(&e, filename, src)),
    };
    for w in &out.warnings {
        let cat = format!("{:?}", w.category);
        let text = src.and_then(|s| source_line(s, w.lineno));
        emit_warning(&cat, &w.msg, &w.msg, filename, w.lineno, w.col_offset + 1, text.as_deref())?;
    }
    let fname = pystr::intern_str(filename);
    let code = crate::types::code::new_code_from_unit(&out.code, Some(&fname))?;
    Ok(Compiled { code, future_flags: out.future_flags })
}

#[allow(dead_code)]
fn _unused(_: &PyObject) {}

/// Parse source bytes to a Rust AST (for `compile(..., PyCF_ONLY_AST)`), emitting
/// parser warnings and raising SyntaxError like `compile_bytes`.
pub fn parse_to_ast(src: &[u8], filename: &str, mode: Mode, flags: u32, feature_version: Option<u32>) -> PyResult<rpy_parser::ast::Mod> {
    let popts = parse_options(mode, flags, feature_version);
    let out = rpy_parser::parse_bytes(src, &popts);
    let text = String::from_utf8_lossy(src).into_owned();
    for w in &out.warnings {
        let cat = format!("{:?}", w.category);
        emit_warning(&cat, &w.msg, &w.error_msg, filename, w.lineno, w.col_offset + 1, source_line(&text, w.lineno).as_deref())?;
    }
    match out.result {
        Ok(m) => Ok(m),
        Err(e) => Err(parse_error(&e, filename)),
    }
}
