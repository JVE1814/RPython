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
    pub fn p(self) -> PMode {
        match self {
            Mode::Exec => PMode::Exec,
            Mode::Eval => PMode::Eval,
            Mode::Single => PMode::Single,
            Mode::FuncType => PMode::FuncType,
        }
    }
    pub fn c(self) -> CMode {
        match self {
            Mode::Exec => CMode::Exec,
            Mode::Eval => CMode::Eval,
            Mode::Single => CMode::Single,
            Mode::FuncType => CMode::FuncType,
        }
    }
}

thread_local! {
    /// Set while the parser runs on this (Python) thread: identifiers are then
    /// normalized through `unicodedata.normalize`, as CPython does.
    static PY_NFKC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static NFKC_ERROR: std::cell::RefCell<Option<PyRef>> = const { std::cell::RefCell::new(None) };
}

fn py_nfkc(s: &str) -> Option<String> {
    let r = (|| -> PyResult<PyRef> {
        let ud = crate::import::import_absolute("unicodedata")?;
        let f = crate::abstract_::getattr_str(&ud, "normalize")?;
        let v = crate::abstract_::call(&f, &[&pystr::new_str("NFKC"), &pystr::new_str(s)])?;
        if !pystr::is_str(&v) {
            crate::raise!(TypeError, "unicodedata.normalize() must return a string, not {}", crate::types::typeobject::tname(&v));
        }
        Ok(v)
    })();
    match r {
        Ok(v) => Some(pystr::to_string_lossy(&v)),
        Err(_) => {
            let e = take_exc();
            NFKC_ERROR.with(|c| {
                if c.borrow().is_none() {
                    *c.borrow_mut() = e;
                }
            });
            None
        }
    }
}

fn nfkc(s: &str) -> String {
    if PY_NFKC.with(|c| c.get()) {
        if let Some(r) = py_nfkc(s) {
            return r;
        }
    }
    let cps: Vec<u32> = s.chars().map(|c| c as u32).collect();
    let out = rpy_unicode::normalize(rpy_unicode::Form::NFKC, &cps);
    out.into_iter().filter_map(char::from_u32).collect()
}
thread_local! {
    /// Set when a `\N{...}` escape could not be resolved because the
    /// `unicodedata` module cannot be imported.
    static NO_UNICODEDATA: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn lookup_name(s: &str) -> Option<u32> {
    if !crate::modules::text_util::ensure_unicodedata() {
        NO_UNICODEDATA.with(|c| c.set(true));
        return None;
    }
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
    use rpy_parser::ErrorKind as K;
    let class = match e.kind {
        K::Indentation => crate::types::exceptions::IndentationError(),
        K::Tab => crate::types::exceptions::TabError(),
        K::IncompleteInput => crate::types::exceptions::IncompleteInputError(),
        K::MemoryError => return err!(MemoryError, "{}", e.msg),
        K::ValueError => return err!(ValueError, "{}", e.msg),
        K::RecursionError => return err!(RecursionError, "{}", e.msg),
        K::Syntax if e.msg == "incomplete input" => crate::types::exceptions::IncompleteInputError(),
        K::Syntax => crate::types::exceptions::SyntaxError(),
    };
    raise_syntax_error(class, &e.msg, filename, e.lineno, e.offset, e.text.as_deref(), e.end_lineno, e.end_offset)
}

pub(crate) fn compile_error(e: &rpy_compiler::CompileError, filename: &str, src: Option<&str>) -> PyErr {
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
    // errors found after parsing (symtable, code generation) take their text from
    // the file, as CPython does: none for compile()d strings
    let text = crate::modules::sys_full::source_line(filename, e.lineno).map(|t| {
        if e.lineno == 1 { t.trim_start_matches('\u{feff}').to_string() } else { t }
    });
    let have_text = text.is_some();
    let to_offset = |line: i32, col: i32| -> i32 {
        // byte column -> 1-based character offset; without the text to convert
        // with, the offset stays in bytes (as CPython reports it)
        if !have_text {
            return col + 1;
        }
        match src.and_then(|s| source_line(s, line)) {
            Some(l) => {
                let b = (col.max(0) as usize).min(l.len());
                match l.get(..b) {
                    Some(p) => p.chars().count() as i32 + 1,
                    None => col + 1,
                }
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
#[allow(clippy::too_many_arguments)]
fn emit_warning_m(
    category: &str,
    msg: &str,
    error_msg: &str,
    filename: &str,
    lineno: i32,
    offset: i32,
    text: Option<&str>,
    module: Option<&PyObject>,
    end: (Option<i32>, Option<i32>),
) -> PyResult<()> {
    let cat = match category {
        "DeprecationWarning" => crate::types::exceptions::DeprecationWarning(),
        _ => crate::types::exceptions::SyntaxWarning(),
    };
    let m = pystr::new_str(msg);
    let f = pystr::new_str(filename);
    let derived = if module.is_none() { module_name_for_file(filename) } else { None };
    let module = module.or(derived.as_deref());
    let r = crate::modules::_warnings::warn_explicit(cat, &m, &f, lineno as i64, module, None);
    if r.is_err() {
        if exc_matches(cat) {
            clear_exc();
            return Err(raise_syntax_error(crate::types::exceptions::SyntaxError(), error_msg, filename, lineno, offset, text, end.0, end.1));
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
    compile_source(Source::Str(src), &Opts::new(filename, mode, flags, optimize))
}

/// Compile source bytes (encoding detection, BOM).
pub fn compile_bytes(src: &[u8], filename: &str, mode: Mode, flags: u32, optimize: i32) -> PyResult<Compiled> {
    compile_source(Source::Bytes(src), &Opts::new(filename, mode, flags, optimize))
}

pub fn compile_ast(module: &rpy_parser::ast::Mod, filename: &str, mode: Mode, flags: u32, optimize: i32, src: Option<&str>) -> PyResult<Compiled> {
    compile_mod(module, &Opts::new(filename, mode, flags, optimize), src)
}

/// Source text of a compile() call.
#[derive(Clone, Copy)]
pub enum Source<'a> {
    /// Decoded text (encoding declarations are ignored, like PyCF_IGNORE_COOKIE).
    Str(&'a str),
    /// Raw bytes (BOM / PEP 263 cookie honoured).
    Bytes(&'a [u8]),
}

/// Options of a compile()/ast.parse() call.
#[derive(Clone, Copy)]
pub struct Opts<'a> {
    pub filename: &'a str,
    pub mode: Mode,
    pub flags: u32,
    pub optimize: i32,
    /// `_feature_version` minor version (only honoured with PyCF_ONLY_AST).
    pub feature_version: Option<u32>,
    /// `module=` argument of compile(): used to filter compile-time warnings.
    pub module: Option<&'a PyObject>,
}

impl<'a> Opts<'a> {
    pub fn new(filename: &'a str, mode: Mode, flags: u32, optimize: i32) -> Opts<'a> {
        Opts { filename, mode, flags, optimize, feature_version: None, module: None }
    }
}

/// Source decoding through the codec registry: `None` for an unknown
/// encoding, an error message for a non-text codec or a decoding error.
fn decode_hook(enc: &str, data: &[u8]) -> Option<Result<String, String>> {
    let msg = |e: Option<PyRef>| e.map(|e| crate::abstract_::str_string(&e).unwrap_or_default()).unwrap_or_default();
    if crate::modules::_codecs::lookup(enc).is_err() {
        if exc_matches(crate::types::exceptions::LookupError()) {
            clear_exc();
            return None;
        }
        return Some(Err(msg(take_exc())));
    }
    match crate::modules::_codecs::decode_text_for_source(data, enc) {
        Ok(s) => Some(Ok(pystr::to_string_lossy(&s))),
        Err(_) => Some(Err(msg(take_exc()))),
    }
}

pub fn full_parse_options(o: &Opts) -> ParseOptions {
    let mut p = parse_options(o.mode, o.flags, o.feature_version);
    p.dont_imply_dedent = o.flags & rpy_compiler::cf::PYCF_DONT_IMPLY_DEDENT != 0;
    p.filename = o.filename.to_string();
    p.decode = Some(decode_hook);
    let d = crate::modules::sys::int_max_str_digits();
    p.int_max_str_digits = if d <= 0 { 0 } else { d as usize };
    p
}

/// Decode `src` to text (BOM, PEP 263 cookie) or raise the decoding SyntaxError.
pub fn decode_source(src: &[u8], filename: &str) -> PyResult<String> {
    match rpy_parser::tokenizer::decode_source_with(src, Some(decode_hook), filename) {
        Ok(s) => Ok(s),
        Err(e) => Err(parse_error(&e, filename)),
    }
}

/// Parse source to a Rust AST, emitting parser warnings. Returns the AST and the decoded text.
pub fn parse_mod(src: Source, o: &Opts) -> PyResult<(rpy_parser::ast::Mod, String)> {
    let text: std::borrow::Cow<str> = match src {
        Source::Str(s) => std::borrow::Cow::Borrowed(s),
        Source::Bytes(b) => std::borrow::Cow::Owned(decode_source(b, o.filename)?),
    };
    let popts = full_parse_options(o);
    PY_NFKC.with(|c| c.set(true));
    NO_UNICODEDATA.with(|c| c.set(false));
    let mut out = rpy_parser::parse(&text, &popts);
    PY_NFKC.with(|c| c.set(false));
    if NO_UNICODEDATA.with(|c| c.replace(false)) {
        if let Err(e) = &mut out.result {
            if e.msg.contains("unknown Unicode character name") {
                e.msg = "(unicode error) \\N escapes not supported (can't load unicodedata module)".to_string();
            }
        }
    }
    if let Some(e) = NFKC_ERROR.with(|c| c.borrow_mut().take()) {
        set_exc(e);
        return Err(PyErr);
    }
    for w in &out.warnings {
        let cat = format!("{:?}", w.category);
        emit_warning_m(&cat, &w.msg, &w.error_msg, o.filename, w.lineno, w.offset, source_line(&text, w.lineno).as_deref(), o.module, (Some(w.end_lineno), Some(w.end_offset)))?;
    }
    match out.result {
        Ok(m) => Ok((m, text.into_owned())),
        Err(e) => {
            let err = parse_error(&e, o.filename);
            attach_source_metadata(&text);
            Err(err)
        }
    }
}

/// Give the pending SyntaxError the whole source as `_metadata` (start line,
/// offset, source), which the traceback module uses to look for misspelt
/// keywords even when the source is not in a file (e.g. `-c`).
fn attach_source_metadata(text: &str) {
    let Some(exc) = take_exc() else { return };
    if crate::types::typeobject::isinstance_exact_or_sub(&exc, crate::types::exceptions::SyntaxError()) {
        let meta = tuple::new_tuple(vec![int::new_int(0), int::new_int(0), pystr::new_str(text)]);
        if crate::abstract_::setattr(&exc, &pystr::new_str("_metadata"), &meta).is_err() {
            clear_exc();
        }
    }
    set_exc(exc);
}

/// Full pipeline: source -> code object.
pub fn compile_source(src: Source, o: &Opts) -> PyResult<Compiled> {
    let (module, text) = parse_mod(src, o)?;
    compile_mod(&module, o, Some(&text))
}

/// Rust AST -> code object (warnings filtered with `o.module`).
pub fn compile_mod(module: &rpy_parser::ast::Mod, o: &Opts, src: Option<&str>) -> PyResult<Compiled> {
    let filename = o.filename;
    let mut copts = CompileOptions::new(filename, o.mode.c());
    copts.flags = o.flags;
    copts.optimize = if o.optimize < 0 { crate::modules::sys::optimize_flag() } else { o.optimize };
    let out = match rpy_compiler::compile(module, &copts) {
        Ok(o) => o,
        Err(e) => return Err(compile_error(&e, filename, src)),
    };
    let (warnings, future_flags) = (&out.warnings, out.future_flags);
    for w in warnings {
        let cat = format!("{:?}", w.category);
        let text = src.and_then(|s| source_line(s, w.lineno));
        let off = match &text {
            Some(l) => {
                let b = (w.col_offset.max(0) as usize).min(l.len());
                l.get(..b).map_or(w.col_offset + 1, |p| p.chars().count() as i32 + 1)
            }
            None => w.col_offset + 1,
        };
        let end_text = src.and_then(|s| source_line(s, w.end_lineno));
        let end_off = match &end_text {
            Some(l) => {
                let b = (w.end_col_offset.max(0) as usize).min(l.len());
                l.get(..b).map_or(w.end_col_offset + 1, |p| p.chars().count() as i32 + 1)
            }
            None => w.end_col_offset + 1,
        };
        emit_warning_m(&cat, &w.msg, &w.msg, filename, w.lineno, off, text.as_deref(), o.module, (Some(w.end_lineno), Some(end_off)))?;
    }
    let fname = pystr::intern_str(filename);
    let code = crate::types::code::new_code_from_wordcode(&out.code, Some(&fname))?;
    Ok(Compiled { code, future_flags })
}

/// AST-level optimization of `ast.parse(..., optimize=N)` / PyCF_OPTIMIZED_AST.
pub fn optimize_mod(module: &mut rpy_parser::ast::Mod, o: &Opts) -> PyResult<()> {
    let opt = if o.optimize < 0 { crate::modules::sys::optimize_flag() } else { o.optimize };
    match rpy_compiler::optimize_ast(module, opt, o.flags) {
        Ok(()) => Ok(()),
        Err(e) => Err(compile_error(&e, o.filename, None)),
    }
}

/// Dotted module name of a source file found below one of the `sys.path`
/// directories (used as the warnings `module` of compile-time warnings).
fn module_name_for_file(filename: &str) -> Option<PyRef> {
    let stem = filename.strip_suffix(".py")?;
    let path = crate::modules::sys::get_sys_attr("path")?;
    if !crate::types::list::is_list(&path) {
        return None;
    }
    let mut best: Option<String> = None;
    for entry in crate::types::list::list_to_vec(&path) {
        if !pystr::is_str(&entry) {
            continue;
        }
        let mut dir = pystr::to_string_lossy(&entry);
        while dir.len() > 1 && dir.ends_with('/') {
            dir.pop();
        }
        if dir.is_empty() {
            continue;
        }
        let Some(rest) = stem.strip_prefix(dir.as_str()).and_then(|r| r.strip_prefix('/')) else { continue };
        if rest.is_empty() || rest.split('/').any(|p| p.is_empty() || p.contains('.')) {
            continue;
        }
        let mut dotted = rest.replace('/', ".");
        if let Some(pkg) = dotted.strip_suffix(".__init__") {
            dotted = pkg.to_string();
        }
        if best.as_ref().is_none_or(|b| dotted.len() < b.len()) {
            best = Some(dotted);
        }
    }
    best.map(|b| pystr::new_str(&b))
}

/// Map a compiler error (validation etc.) to a Python exception.
pub fn raise_compile_error(e: &rpy_compiler::CompileError, filename: &str, src: Option<&str>) -> PyErr {
    compile_error(e, filename, src)
}
