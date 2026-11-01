//! Error type for integer operations. Each variant maps to a Python exception type and message.

use core::fmt;

/// The Python exception class an [`IntError`] corresponds to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExcKind {
    ZeroDivisionError,
    OverflowError,
    ValueError,
    MemoryError,
}

/// Errors from integer operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntError {
    /// `a // 0`, `divmod(a, 0)`: ZeroDivisionError "integer division or modulo by zero".
    DivisionByZero,
    /// `a % 0`: ZeroDivisionError "integer modulo by zero".
    ModuloByZero,
    /// `a / 0`: ZeroDivisionError "division by zero".
    TrueDivisionByZero,
    /// OverflowError "integer division result too large for a float".
    TrueDivOverflow,
    /// OverflowError "int too large to convert to float".
    FloatOverflow,
    /// ValueError "negative shift count".
    NegativeShiftCount,
    /// OverflowError "too many digits in integer".
    TooManyDigits,
    /// MemoryError (result would not fit in memory).
    MemoryError,
    /// ValueError "pow() 3rd argument cannot be 0".
    PowModulusZero,
    /// ValueError "base is not invertible for the given modulus".
    NotInvertible,
    /// `pow(a, negative)` without modulus: the caller must compute a float power instead.
    NegativeExponent,
    /// OverflowError "cannot convert float infinity to integer".
    FloatInfinity,
    /// ValueError "cannot convert float NaN to integer".
    FloatNaN,
    /// OverflowError "int too big to convert" (int.to_bytes).
    IntTooBig,
    /// OverflowError "can't convert negative int to unsigned".
    NegativeToUnsigned,
    /// Value does not fit the requested fixed-size integer type (OverflowError; the caller picks
    /// the message, e.g. "Python int too large to convert to C long").
    Overflow,
    /// ValueError "invalid literal for int() with base N: '...'" — the caller formats the
    /// message with the original object repr; `base` is the base as given by the user.
    InvalidLiteral { base: u32 },
    /// ValueError "int() base must be >= 2 and <= 36, or 0".
    InvalidBase,
    /// ValueError for str -> int beyond the max-str-digits limit.
    StrToIntDigitLimit { digits: usize, limit: usize },
    /// ValueError for int -> str beyond the max-str-digits limit.
    IntToStrDigitLimit { limit: usize },
    /// ValueError "isqrt() argument must be nonnegative".
    IsqrtNegative,
    /// ValueError "factorial() not defined for negative values".
    FactorialNegative,
    /// ValueError "n must be a non-negative integer".
    NNegative,
    /// ValueError "k must be a non-negative integer".
    KNegative,
    /// ValueError "math domain error" (e.g. log of non-positive int).
    MathDomain,
}

impl IntError {
    /// Python exception class.
    pub fn kind(&self) -> ExcKind {
        use IntError::*;
        match self {
            DivisionByZero | ModuloByZero | TrueDivisionByZero => ExcKind::ZeroDivisionError,
            TrueDivOverflow | FloatOverflow | TooManyDigits | FloatInfinity | IntTooBig
            | NegativeToUnsigned | Overflow => ExcKind::OverflowError,
            MemoryError => ExcKind::MemoryError,
            NegativeShiftCount | PowModulusZero | NotInvertible | FloatNaN | InvalidLiteral { .. }
            | InvalidBase | StrToIntDigitLimit { .. } | IntToStrDigitLimit { .. }
            | IsqrtNegative | FactorialNegative | NNegative | KNegative | MathDomain
            | NegativeExponent => ExcKind::ValueError,
        }
    }

    /// CPython's message for this error. For `InvalidLiteral` the caller should build the
    /// message itself (it needs the repr of the original argument); this returns a prefix.
    pub fn message(&self) -> String {
        use IntError::*;
        match self {
            DivisionByZero => "integer division or modulo by zero".into(),
            ModuloByZero => "integer modulo by zero".into(),
            TrueDivisionByZero => "division by zero".into(),
            TrueDivOverflow => "integer division result too large for a float".into(),
            FloatOverflow => "int too large to convert to float".into(),
            NegativeShiftCount => "negative shift count".into(),
            TooManyDigits => "too many digits in integer".into(),
            MemoryError => String::new(),
            PowModulusZero => "pow() 3rd argument cannot be 0".into(),
            NotInvertible => "base is not invertible for the given modulus".into(),
            NegativeExponent => "negative exponent".into(),
            FloatInfinity => "cannot convert float infinity to integer".into(),
            FloatNaN => "cannot convert float NaN to integer".into(),
            IntTooBig => "int too big to convert".into(),
            NegativeToUnsigned => "can't convert negative int to unsigned".into(),
            Overflow => "Python int too large to convert".into(),
            InvalidLiteral { base } => format!("invalid literal for int() with base {}", base),
            InvalidBase => "int() base must be >= 2 and <= 36, or 0".into(),
            StrToIntDigitLimit { digits, limit } => format!(
                "Exceeds the limit ({} digits) for integer string conversion: value has {} digits; use sys.set_int_max_str_digits() to increase the limit",
                limit, digits
            ),
            IntToStrDigitLimit { limit } => format!(
                "Exceeds the limit ({} digits) for integer string conversion; use sys.set_int_max_str_digits() to increase the limit",
                limit
            ),
            IsqrtNegative => "isqrt() argument must be nonnegative".into(),
            FactorialNegative => "factorial() not defined for negative values".into(),
            NNegative => "n must be a non-negative integer".into(),
            KNegative => "k must be a non-negative integer".into(),
            MathDomain => "math domain error".into(),
        }
    }
}

impl fmt::Display for IntError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for IntError {}
