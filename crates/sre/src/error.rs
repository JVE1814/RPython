//! Error and warning types.

use std::fmt;

/// Error raised while compiling a pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    /// `re.PatternError` (alias `re.error`): `msg` plus optional position
    /// (index into the pattern, in code points / bytes).
    Pattern { msg: String, pos: Option<usize> },
    /// `OverflowError` (e.g. "the repetition number is too large").
    Overflow(String),
    /// `ValueError` (incompatible flags, e.g. "cannot use LOCALE flag with a str pattern").
    Value(String),
}

impl CompileError {
    pub(crate) fn pat(msg: impl Into<String>, pos: usize) -> Self {
        CompileError::Pattern { msg: msg.into(), pos: Some(pos) }
    }
    pub(crate) fn pat_nopos(msg: impl Into<String>) -> Self {
        CompileError::Pattern { msg: msg.into(), pos: None }
    }
    /// The bare message (for PatternError: without position info).
    pub fn msg(&self) -> &str {
        match self {
            CompileError::Pattern { msg, .. } => msg,
            CompileError::Overflow(m) | CompileError::Value(m) => m,
        }
    }
    pub fn pos(&self) -> Option<usize> {
        match self {
            CompileError::Pattern { pos, .. } => *pos,
            _ => None,
        }
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompileError::Pattern { msg, pos: Some(p) } => write!(f, "{msg} at position {p}"),
            CompileError::Pattern { msg, pos: None } => f.write_str(msg),
            CompileError::Overflow(m) => write!(f, "OverflowError: {m}"),
            CompileError::Value(m) => write!(f, "ValueError: {m}"),
        }
    }
}

impl std::error::Error for CompileError {}

/// Error raised while parsing a replacement template (`re.sub`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateError {
    /// `re.PatternError` with message and position inside the template.
    Pattern { msg: String, pos: usize },
    /// `IndexError` ("unknown group name 'x'").
    Index(String),
}

impl TemplateError {
    pub fn msg(&self) -> &str {
        match self {
            TemplateError::Pattern { msg, .. } => msg,
            TemplateError::Index(m) => m,
        }
    }
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TemplateError::Pattern { msg, pos } => write!(f, "{msg} at position {pos}"),
            TemplateError::Index(m) => write!(f, "IndexError: {m}"),
        }
    }
}

impl std::error::Error for TemplateError {}

/// Category of a warning emitted at compile time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarningKind {
    FutureWarning,
    DeprecationWarning,
}

/// A warning the runtime should emit (`warnings.warn(message, kind)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub kind: WarningKind,
    pub message: String,
}

/// Compute CPython's `(lineno, colno)` for an error position.
pub fn line_col(pattern: &[u32], pos: usize) -> (usize, usize) {
    let pos = pos.min(pattern.len());
    let before = &pattern[..pos];
    let lineno = before.iter().filter(|&&c| c == 10).count() + 1;
    let colno = match before.iter().rposition(|&c| c == 10) {
        Some(i) => pos - i,
        None => pos + 1,
    };
    (lineno, colno)
}

/// Full `str(err)` text as CPython formats it.
pub fn format_error_message(pattern: &[u32], msg: &str, pos: Option<usize>) -> String {
    match pos {
        None => msg.to_string(),
        Some(p) => {
            if pattern.contains(&10) {
                let (l, c) = line_col(pattern, p);
                format!("{msg} at position {p} (line {l}, column {c})")
            } else {
                format!("{msg} at position {p}")
            }
        }
    }
}
