//! Syntax errors and warnings produced by the tokenizer and parser.

use std::fmt;

/// Which subclass of `SyntaxError` to raise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Syntax,
    Indentation,
    Tab,
    /// "incomplete input" with PyCF_ALLOW_INCOMPLETE_INPUT (CPython raises
    /// `_IncompleteInputError`, a SyntaxError subclass).
    IncompleteInput,
}

impl ErrorKind {
    pub fn class_name(self) -> &'static str {
        match self {
            ErrorKind::Syntax => "SyntaxError",
            ErrorKind::Indentation => "IndentationError",
            ErrorKind::Tab => "TabError",
            ErrorKind::IncompleteInput => "_IncompleteInputError",
        }
    }
}

/// A syntax error with CPython-compatible message and location.
///
/// `lineno`/`end_lineno` are 1-based. `col_offset`/`end_col_offset` are 0-based UTF-8 byte
/// offsets into the respective lines (-1 when unknown). `offset`/`end_offset` are what Python's
/// `SyntaxError.offset`/`end_offset` attributes contain: 1-based *character* offsets (0 when
/// unknown). `end_lineno`/`end_offset` are `None` when CPython leaves them unset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxError {
    pub kind: ErrorKind,
    pub msg: String,
    pub lineno: i32,
    pub col_offset: i32,
    pub end_lineno: Option<i32>,
    pub end_col_offset: Option<i32>,
    pub offset: i32,
    pub end_offset: Option<i32>,
    /// Source line(s) the error refers to (CPython's `SyntaxError.text`), if available.
    pub text: Option<String>,
}

impl SyntaxError {
    pub fn simple(msg: impl Into<String>) -> SyntaxError {
        SyntaxError {
            kind: ErrorKind::Syntax,
            msg: msg.into(),
            lineno: 0,
            col_offset: -1,
            end_lineno: None,
            end_col_offset: None,
            offset: 0,
            end_offset: None,
            text: None,
        }
    }
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} (line {}, offset {})", self.kind.class_name(), self.msg, self.lineno, self.offset)
    }
}

impl std::error::Error for SyntaxError {}

/// Category of a compile-time warning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WarningCategory {
    SyntaxWarning,
    DeprecationWarning,
}

/// A warning produced while parsing; the runtime emits it through the `warnings` module.
/// If the warning is turned into an error, the runtime should raise a `SyntaxError` with the
/// warning message at the warning's location (this is what CPython does).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    pub category: WarningCategory,
    pub msg: String,
    /// Message to use when the warning is turned into a SyntaxError (may differ from `msg`).
    pub error_msg: String,
    pub lineno: i32,
    /// 0-based byte column.
    pub col_offset: i32,
    pub end_lineno: i32,
    pub end_col_offset: i32,
    /// 1-based character offsets (as for `SyntaxError.offset`).
    pub offset: i32,
    pub end_offset: i32,
}

/// Compute the 1-based character offset for a 0-based byte column within `line`.
pub fn byte_col_to_char_offset(line: &str, col: i32) -> i32 {
    if col < 0 {
        return 0;
    }
    let col = col as usize;
    let b = line.as_bytes();
    if col >= b.len() {
        // beyond the end: count chars and add the excess bytes
        let n = line.chars().count();
        return (n + (col - b.len())) as i32 + 1;
    }
    let mut n = 0;
    for &c in &b[..col] {
        if c & 0xC0 != 0x80 {
            n += 1;
        }
    }
    n as i32 + 1
}
