//! Low-level float -> string conversion with Python semantics
//! (the equivalent of CPython's `PyOS_double_to_string`) and float repr.

use crate::dtoa::{self, Digits};
use crate::{NumFmtError, PRECISION_MAX};

/// Flags for [`double_to_string`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DtoaFlags {
    /// Always emit a sign ('+' for non-negative values and NaNs).
    pub sign: bool,
    /// Append ".0" to integral results in fixed notation (repr style).
    pub add_dot_0: bool,
    /// Alternate form: always include a decimal point; for 'g' keep
    /// trailing zeros.
    pub alt: bool,
    /// Coerce negative values that round to zero to positive ('z' option).
    pub no_neg_0: bool,
}

/// What kind of value was formatted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatKind {
    Finite,
    Infinite,
    Nan,
}

#[inline]
fn push_zeros(out: &mut String, n: i64) {
    if n > 0 {
        out.extend(core::iter::repeat_n('0', n as usize));
    }
}

/// Lay out `digits`/`decpt` (dtoa convention) according to the Python
/// float formatting rules.  `mode` is one of b'e', b'f', b'g', b'r'.
#[allow(clippy::too_many_arguments)]
pub(crate) fn format_digits(
    out: &mut String,
    digits: &[u8],
    mut decpt: i64,
    mut neg: bool,
    mode: u8,
    precision: i64,
    flags: DtoaFlags,
    upper: bool,
) {
    let digits_len = digits.len() as i64;
    if neg && flags.no_neg_0 && digits.iter().all(|&d| d == b'0') {
        neg = false;
    }
    if neg {
        out.push('-');
    } else if flags.sign {
        out.push('+');
    }
    let mut vdigits_end = digits_len;
    let mut use_exp = false;
    match mode {
        b'e' => {
            use_exp = true;
            vdigits_end = precision;
        }
        b'f' => vdigits_end = decpt + precision,
        b'g' => {
            let thresh = if flags.add_dot_0 { precision - 1 } else { precision };
            if decpt <= -4 || decpt > thresh {
                use_exp = true;
            }
            if flags.alt {
                vdigits_end = precision;
            }
        }
        _ => {
            // 'r'
            if decpt <= -4 || decpt > 16 {
                use_exp = true;
            }
        }
    }
    let mut exp = 0i64;
    if use_exp {
        exp = decpt - 1;
        decpt = 1;
    }
    let vdigits_start = if decpt <= 0 { decpt - 1 } else { 0 };
    if !use_exp && flags.add_dot_0 {
        vdigits_end = vdigits_end.max(decpt + 1);
    } else {
        vdigits_end = vdigits_end.max(decpt);
    }
    // Zero padding on the left.
    if decpt <= 0 {
        push_zeros(out, decpt - vdigits_start);
        out.push('.');
        push_zeros(out, -decpt);
    } else {
        push_zeros(out, -vdigits_start);
    }
    // Digits with the decimal point.
    // SAFETY of from_utf8: digits are ASCII.
    let ds = unsafe { core::str::from_utf8_unchecked(digits) };
    if 0 < decpt && decpt <= digits_len {
        out.push_str(&ds[..decpt as usize]);
        out.push('.');
        out.push_str(&ds[decpt as usize..]);
    } else {
        out.push_str(ds);
    }
    // Zeros on the right.
    if digits_len < decpt {
        push_zeros(out, decpt - digits_len);
        out.push('.');
        push_zeros(out, vdigits_end - decpt);
    } else {
        push_zeros(out, vdigits_end - digits_len);
    }
    if out.ends_with('.') && !flags.alt {
        out.pop();
    }
    if use_exp {
        out.push(if upper { 'E' } else { 'e' });
        if exp < 0 {
            out.push('-');
        } else {
            out.push('+');
        }
        let a = exp.unsigned_abs();
        if a < 10 {
            out.push('0');
        }
        let mut buf = [0u8; 20];
        let n = crate::bignum::write_u64(a, &mut buf);
        if n == 0 {
            out.push('0');
        } else {
            out.push_str(unsafe { core::str::from_utf8_unchecked(&buf[..n]) });
        }
    }
}

/// Python's `PyOS_double_to_string`.
///
/// `code` is one of `e E f F g G r` (`r` = repr, precision must be 0).
/// For 'g'/'G' a precision of 0 is treated as 1.
pub fn double_to_string(
    out: &mut String,
    x: f64,
    code: char,
    precision: usize,
    flags: DtoaFlags,
) -> Result<FloatKind, NumFmtError> {
    let upper = matches!(code, 'E' | 'F' | 'G');
    let mode = match code {
        'e' | 'E' => b'e',
        'f' | 'F' => b'f',
        'g' | 'G' => b'g',
        'r' => b'r',
        _ => {
            return Err(NumFmtError::value(format!(
                "invalid format code '{}' for double_to_string",
                code
            )))
        }
    };
    if precision > PRECISION_MAX {
        return Err(NumFmtError::value("precision too big"));
    }
    if x.is_nan() {
        if flags.sign {
            out.push('+');
        }
        out.push_str(if upper { "NAN" } else { "nan" });
        return Ok(FloatKind::Nan);
    }
    let neg = x.is_sign_negative();
    if x.is_infinite() {
        if neg {
            out.push('-');
        } else if flags.sign {
            out.push('+');
        }
        out.push_str(if upper { "INF" } else { "inf" });
        return Ok(FloatKind::Infinite);
    }
    let ax = x.abs();
    let mut d = Digits::new();
    let mut precision = precision as i64;
    match mode {
        b'e' => {
            dtoa::significant(ax, precision as usize + 1, &mut d);
            precision += 1;
        }
        b'f' => dtoa::fixed(ax, precision, &mut d),
        b'g' => {
            if precision == 0 {
                precision = 1;
            }
            dtoa::significant(ax, precision as usize, &mut d);
        }
        _ => {
            dtoa::shortest(ax, &mut d);
            precision = 0;
        }
    }
    format_digits(out, d.digits(), d.decpt as i64, neg, mode, precision, flags, upper);
    Ok(FloatKind::Finite)
}

/// Append `repr(x)` (== `str(x)`) for a Python float.
pub fn write_float_repr(out: &mut String, x: f64) {
    if x.is_nan() {
        out.push_str("nan");
        return;
    }
    if x.is_infinite() {
        out.push_str(if x < 0.0 { "-inf" } else { "inf" });
        return;
    }
    let mut d = Digits::new();
    dtoa::shortest(x.abs(), &mut d);
    format_digits(
        out,
        d.digits(),
        d.decpt as i64,
        x.is_sign_negative(),
        b'r',
        0,
        DtoaFlags { add_dot_0: true, ..Default::default() },
        false,
    );
}

/// `repr(x)` for a Python float.
pub fn float_repr(x: f64) -> String {
    let mut s = String::with_capacity(24);
    write_float_repr(&mut s, x);
    s
}

/// The repr used for the parts of a complex number: like float repr but
/// without the trailing ".0" for integral values.
pub fn write_float_repr_no_dot0(out: &mut String, x: f64, always_sign: bool) {
    let _ = double_to_string(
        out,
        x,
        'r',
        0,
        DtoaFlags { sign: always_sign, ..Default::default() },
    );
}

#[cfg(test)]
pub(crate) mod tests_support {
    /// Render digits/decpt as fixed notation with `p` fraction digits.
    pub(crate) fn fixed_string(d: &str, decpt: i32, p: usize) -> String {
        let mut s = String::new();
        super::format_digits(
            &mut s,
            d.as_bytes(),
            decpt as i64,
            false,
            b'f',
            p as i64,
            Default::default(),
            false,
        );
        s
    }
}
