//! Compile errors and warnings.

use crate::ast::Loc;

/// Which exception the runtime should raise for a [`CompileError`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    SyntaxError,
    IndentationError,
    RecursionError,
    ValueError,
    TypeError,
    SystemError,
    MemoryError,
}

impl ErrorKind {
    pub fn class_name(self) -> &'static str {
        match self {
            ErrorKind::SyntaxError => "SyntaxError",
            ErrorKind::IndentationError => "IndentationError",
            ErrorKind::RecursionError => "RecursionError",
            ErrorKind::ValueError => "ValueError",
            ErrorKind::TypeError => "TypeError",
            ErrorKind::SystemError => "SystemError",
            ErrorKind::MemoryError => "MemoryError",
        }
    }
}

/// An error detected by the symbol table builder or the code generator.
///
/// Positions are AST positions: 1-based lines, 0-based UTF-8 *byte* columns (-1 = unknown).
/// Use [`CompileError::offsets`] with the source line to get CPython's 1-based character
/// `offset`/`end_offset` for `SyntaxError` objects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileError {
    pub kind: ErrorKind,
    pub msg: String,
    pub lineno: i32,
    pub col_offset: i32,
    pub end_lineno: i32,
    pub end_col_offset: i32,
}

impl CompileError {
    pub fn syntax(msg: impl Into<String>, loc: Loc) -> CompileError {
        CompileError {
            kind: ErrorKind::SyntaxError,
            msg: msg.into(),
            lineno: loc.lineno,
            col_offset: loc.col_offset,
            end_lineno: loc.end_lineno,
            end_col_offset: loc.end_col_offset,
        }
    }
    pub fn other(kind: ErrorKind, msg: impl Into<String>) -> CompileError {
        CompileError { kind, msg: msg.into(), lineno: 0, col_offset: -1, end_lineno: 0, end_col_offset: -1 }
    }
    pub fn recursion() -> CompileError {
        CompileError::other(ErrorKind::RecursionError, "maximum recursion depth exceeded during compilation")
    }
    /// 1-based character offsets (`SyntaxError.offset`, `SyntaxError.end_offset`) computed from
    /// the text of the start line and the end line (pass the same line twice if equal).
    pub fn offsets(&self, start_line: Option<&str>, end_line: Option<&str>) -> (i32, i32) {
        (byte_col_to_char_offset(start_line, self.col_offset), byte_col_to_char_offset(end_line, self.end_col_offset))
    }
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {} (line {}, col {})", self.kind.class_name(), self.msg, self.lineno, self.col_offset)
    }
}

impl std::error::Error for CompileError {}

/// 1-based character offset for a 0-based byte column (0 if unknown).
pub fn byte_col_to_char_offset(line: Option<&str>, col: i32) -> i32 {
    if col < 0 {
        return 0;
    }
    match line {
        None => col + 1,
        Some(line) => {
            let col = col as usize;
            let b = line.as_bytes();
            if col >= b.len() {
                return (line.chars().count() + (col - b.len())) as i32 + 1;
            }
            b[..col].iter().filter(|&&c| c & 0xC0 != 0x80).count() as i32 + 1
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarningCategory {
    SyntaxWarning,
    DeprecationWarning,
}

/// A compile-time warning for the runtime to emit through `warnings.warn_explicit`
/// (category, message, filename, lineno). If the warning filter turns it into an error, CPython
/// raises `SyntaxError(msg)` at the same location instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    pub category: WarningCategory,
    pub msg: String,
    pub lineno: i32,
    pub col_offset: i32,
    pub end_lineno: i32,
    pub end_col_offset: i32,
}

impl Warning {
    pub fn syntax(msg: impl Into<String>, loc: Loc) -> Warning {
        Warning {
            category: WarningCategory::SyntaxWarning,
            msg: msg.into(),
            lineno: loc.lineno,
            col_offset: loc.col_offset,
            end_lineno: loc.end_lineno,
            end_col_offset: loc.end_col_offset,
        }
    }
}
