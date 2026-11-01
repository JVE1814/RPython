//! rpy-numfmt: number formatting and parsing with exact CPython 3.15
//! semantics.
//!
//! * float repr / str ([`float_repr`], [`write_float_repr`])
//! * correctly rounded fixed/exponent/general formatting
//!   ([`double_to_string`])
//! * the format-spec mini language ([`FormatSpec`], [`format_float`],
//!   [`format_int`], [`format_str`], [`format_complex`], ...)
//! * printf-style `%` conversions ([`percent`])
//! * float / complex parsing ([`parse_float`], [`parse_complex`], ...)
//! * float.hex / float.fromhex / round / as_integer_ratio ([`float_ops`])
//!
//! See NOTES.md for the API overview.

mod bignum;
mod dtoa;
pub mod float_fmt;
pub mod float_ops;
pub mod format;
pub mod parse;
pub mod percent;
pub mod spec;

pub use float_fmt::{double_to_string, float_repr, write_float_repr, DtoaFlags, FloatKind};
pub use float_ops::{
    float_as_integer_ratio, float_fromhex, float_hex, float_round, float_round_to_int,
    write_float_hex,
};
pub use format::{
    complex_repr, format_complex, format_float, format_i128, format_int, format_int_char,
    format_str,
    write_complex_repr, IntPresentation, Locale,
};
pub use parse::{parse_complex, parse_float, parse_float_bytes, parse_float_literal};
pub use spec::{Align, FormatSpec};

/// Largest precision accepted for float formatting.  Requests above this
/// raise `ValueError("precision too big")` (mirrors CPython rejecting
/// precisions close to INT_MAX before computing buffer sizes).
pub const PRECISION_MAX: usize = (i32::MAX as usize) - 1024 * 1024;

/// Python exception class an error maps to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    ValueError,
    OverflowError,
    TypeError,
}

/// Error with the exact CPython exception type and message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NumFmtError {
    pub kind: ErrorKind,
    pub msg: String,
}

impl NumFmtError {
    pub fn value(msg: impl Into<String>) -> Self {
        NumFmtError { kind: ErrorKind::ValueError, msg: msg.into() }
    }
    pub fn overflow(msg: impl Into<String>) -> Self {
        NumFmtError { kind: ErrorKind::OverflowError, msg: msg.into() }
    }
    pub fn type_error(msg: impl Into<String>) -> Self {
        NumFmtError { kind: ErrorKind::TypeError, msg: msg.into() }
    }
}

impl core::fmt::Display for NumFmtError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.msg)
    }
}

impl std::error::Error for NumFmtError {}

