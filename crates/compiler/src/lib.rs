//! `rpy-compiler`: symbol table analysis and code generation: AST -> CPython 3.15 wordcode
//! ([`wordcode::CodeObject`], the public code format, D24).
//!
//! Entry points: [`compile`], [`symtable`], [`optimize_ast`], [`wordcode::dis::disassemble`].
//! [`bytecode`] holds the runtime's internal instruction format (`Op`, `encode`/`decode`,
//! `ExceptionEntry`, `Location`), which the runtime translates wordcode into.

pub mod bytecode;
pub mod error;

// The AST is defined by the parser crate.

pub use rpy_parser::ast;

pub(crate) mod wc;
pub mod fold;
pub mod symtable;
pub mod unparse;
pub mod validate;
pub mod wordcode;

pub use error::{CompileError, ErrorKind, Warning, WarningCategory};

/// Compile mode (the `mode` argument of `compile()`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Exec,
    Eval,
    Single,
    FuncType,
}

/// `PyCF_*` and `CO_FUTURE_*` flag values (same numbers as CPython).
pub mod cf {
    pub const CO_FUTURE_DIVISION: u32 = 0x20000;
    pub const CO_FUTURE_ABSOLUTE_IMPORT: u32 = 0x40000;
    pub const CO_FUTURE_WITH_STATEMENT: u32 = 0x80000;
    pub const CO_FUTURE_PRINT_FUNCTION: u32 = 0x100000;
    pub const CO_FUTURE_UNICODE_LITERALS: u32 = 0x200000;
    pub const CO_FUTURE_BARRY_AS_BDFL: u32 = 0x400000;
    pub const CO_FUTURE_GENERATOR_STOP: u32 = 0x800000;
    pub const CO_FUTURE_ANNOTATIONS: u32 = 0x1000000;
    /// All future flags: these are inherited and copied into `co_flags`.
    pub const PYCF_MASK: u32 = CO_FUTURE_DIVISION
        | CO_FUTURE_ABSOLUTE_IMPORT
        | CO_FUTURE_WITH_STATEMENT
        | CO_FUTURE_PRINT_FUNCTION
        | CO_FUTURE_UNICODE_LITERALS
        | CO_FUTURE_BARRY_AS_BDFL
        | CO_FUTURE_GENERATOR_STOP
        | CO_FUTURE_ANNOTATIONS;
    pub const PYCF_SOURCE_IS_UTF8: u32 = 0x0100;
    pub const PYCF_DONT_IMPLY_DEDENT: u32 = 0x0200;
    pub const PYCF_ONLY_AST: u32 = 0x0400;
    pub const PYCF_IGNORE_COOKIE: u32 = 0x0800;
    pub const PYCF_TYPE_COMMENTS: u32 = 0x1000;
    pub const PYCF_ALLOW_TOP_LEVEL_AWAIT: u32 = 0x2000;
    pub const PYCF_ALLOW_INCOMPLETE_INPUT: u32 = 0x4000;
    pub const PYCF_OPTIMIZED_AST: u32 = 0x8000 | PYCF_ONLY_AST;
}

/// Options for [`compile`].
#[derive(Clone, Debug)]
pub struct CompileOptions {
    pub filename: String,
    pub mode: Mode,
    /// Optimization level: 0 (asserts and `__debug__` on), 1 (-O), 2 (-OO: also strip
    /// docstrings). The runtime resolves -1 to `sys.flags.optimize` before calling.
    pub optimize: i32,
    /// `PyCF_*` flags and inherited `CO_FUTURE_*` features (see [`cf`]).
    pub flags: u32,
    /// Interactive mode for [`Mode::Single`]: expression statements print their value.
    /// (`compile(..., 'single')` always sets this.)
    pub interactive: bool,
    /// Maximum AST nesting depth before `RecursionError` (bounds native stack use).
    pub max_depth: u32,
}

impl Default for CompileOptions {
    fn default() -> Self {
        CompileOptions {
            filename: "<string>".to_string(),
            mode: Mode::Exec,
            optimize: 0,
            flags: 0,
            interactive: false,
            max_depth: DEFAULT_MAX_DEPTH,
        }
    }
}

/// Default nesting limit. Each level costs a few KB of native stack in the recursive passes;
/// measured: < 1 MB of native stack at the default limit (debug and release).
pub const DEFAULT_MAX_DEPTH: u32 = 2000;

impl CompileOptions {
    pub fn new(filename: impl Into<String>, mode: Mode) -> CompileOptions {
        CompileOptions { filename: filename.into(), mode, interactive: mode == Mode::Single, ..Default::default() }
    }
}

/// The docstring cleaning the compiler applies (`_testinternalcapi.compiler_cleandoc`).
pub fn clean_docstring(code_points: &[u32]) -> Vec<u32> {
    wc::clean_doc(&ast::PyStr::CodePoints(code_points.to_vec()))
}

/// Compile a module/expression AST to a code object in the public CPython 3.15 format.
pub fn compile(module: &ast::Mod, opts: &CompileOptions) -> Result<WordcodeOutput, CompileError> {
    wc::compile_module(module, opts)
}

/// Build and analyse the symbol table (for the stdlib `symtable` module).
pub fn symtable(module: &ast::Mod, opts: &CompileOptions) -> Result<symtable::SymbolTable, CompileError> {
    let future = wc::check_future(module, opts.flags)?;
    let st = symtable::build(module, future | (opts.flags & cf::PYCF_ALLOW_TOP_LEVEL_AWAIT), opts.max_depth)?;
    Ok(st)
}

/// AST-level optimizations performed by `ast.parse(..., optimize=N)` / `PyCF_OPTIMIZED_AST`
/// for N > 0: `__debug__` folding, docstring removal at N >= 2, `'%s' % (a,)` to f-string,
/// and folding of constant expressions in match patterns.
pub fn optimize_ast(module: &mut ast::Mod, optimize: i32, flags: u32) -> Result<(), CompileError> {
    fold::optimize_ast(module, optimize, flags)
}

/// Position validation for hand-built ASTs (CPython's `_PyAST_Validate` position rules); call
/// before [`compile`] when compiling an `ast` object from Python. Errors are ValueErrors.
pub fn validate_ast(module: &ast::Mod) -> Result<(), CompileError> {
    validate::validate_positions(module)
}

/// Same as [`compile`] (name kept for existing callers).
pub fn compile_wordcode(module: &ast::Mod, opts: &CompileOptions) -> Result<wc::WordcodeOutput, CompileError> {
    wc::compile_module(module, opts)
}

pub use wc::WordcodeOutput;
/// the compiler stages behind `_testinternalcapi.compiler_codegen` / `optimize_cfg` /
/// `assemble_code_object`
pub use wc::hooks;

include!(concat!(env!("OUT_DIR"), "/bytecode_hash.rs"));
