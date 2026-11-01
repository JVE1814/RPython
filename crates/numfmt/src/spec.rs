//! Parser for the format-specification mini-language:
//!
//! ```text
//! [[fill]align][sign]['z']['#']['0'][width][grouping]['.' [precision][grouping]][type]
//! ```

use crate::NumFmtError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    /// '<'
    Left,
    /// '>'
    Right,
    /// '^'
    Center,
    /// '=' (padding after the sign)
    AfterSign,
}

impl Align {
    fn from_char(c: char) -> Option<Align> {
        match c {
            '<' => Some(Align::Left),
            '>' => Some(Align::Right),
            '^' => Some(Align::Center),
            '=' => Some(Align::AfterSign),
            _ => None,
        }
    }
}

/// A parsed format specification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatSpec {
    pub fill: char,
    /// Whether a fill character was given explicitly.
    pub fill_specified: bool,
    pub align: Align,
    pub align_specified: bool,
    /// '+', '-', ' ' or None.
    pub sign: Option<char>,
    /// 'z' option.
    pub no_neg_0: bool,
    /// '#' option.
    pub alternate: bool,
    pub width: Option<usize>,
    /// ',' or '_' for the integer part.
    pub grouping: Option<char>,
    pub precision: Option<usize>,
    /// ',' or '_' for the fractional part (3.14+).
    pub frac_grouping: Option<char>,
    /// Presentation type; '\0' if none was given and the default is none.
    pub ty: char,
}

impl FormatSpec {
    /// Default spec (what an empty format string parses to).
    pub fn default_with(default_type: char, default_align: Align) -> FormatSpec {
        FormatSpec {
            fill: ' ',
            fill_specified: false,
            align: default_align,
            align_specified: false,
            sign: None,
            no_neg_0: false,
            alternate: false,
            width: None,
            grouping: None,
            precision: None,
            frac_grouping: None,
            ty: default_type,
        }
    }

    /// Parse a format spec.
    ///
    /// * `default_type`: 'd' for int, '\0' for float/complex, 's' for str.
    /// * `default_align`: Right for numbers, Left for strings.
    /// * `type_name`: Python type name of the object being formatted (used in
    ///   the "Invalid format specifier" message).
    pub fn parse(
        spec: &str,
        default_type: char,
        default_align: Align,
        type_name: &str,
    ) -> Result<FormatSpec, NumFmtError> {
        let mut f = FormatSpec::default_with(default_type, default_align);
        // Avoid a heap allocation for the (overwhelmingly common) short specs.
        let mut stack = ['\0'; 48];
        let heap: Vec<char>;
        let s: &[char] = if spec.len() <= stack.len() {
            let mut n = 0;
            for c in spec.chars() {
                stack[n] = c;
                n += 1;
            }
            &stack[..n]
        } else {
            heap = spec.chars().collect();
            &heap
        };
        let end = s.len();
        let mut pos = 0usize;

        // fill + align
        if end - pos >= 2 {
            if let Some(a) = Align::from_char(s[pos + 1]) {
                f.align = a;
                f.fill = s[pos];
                f.fill_specified = true;
                f.align_specified = true;
                pos += 2;
            }
        }
        if !f.align_specified && end - pos >= 1 {
            if let Some(a) = Align::from_char(s[pos]) {
                f.align = a;
                f.align_specified = true;
                pos += 1;
            }
        }
        // sign
        if end - pos >= 1 && matches!(s[pos], '+' | '-' | ' ') {
            f.sign = Some(s[pos]);
            pos += 1;
        }
        // 'z'
        if end - pos >= 1 && s[pos] == 'z' {
            f.no_neg_0 = true;
            pos += 1;
        }
        // '#'
        if end - pos >= 1 && s[pos] == '#' {
            f.alternate = true;
            pos += 1;
        }
        // '0'
        if !f.fill_specified && end - pos >= 1 && s[pos] == '0' {
            f.fill = '0';
            if !f.align_specified && default_align == Align::Right {
                f.align = Align::AfterSign;
            }
            pos += 1;
        }
        // width
        let (w, consumed) = get_integer(s, &mut pos)?;
        if consumed > 0 {
            f.width = Some(w);
        }
        // grouping
        if end - pos >= 1 && s[pos] == ',' {
            f.grouping = Some(',');
            pos += 1;
        }
        if end - pos >= 1 && s[pos] == '_' {
            if f.grouping.is_some() {
                return Err(both_comma_and_underscore());
            }
            f.grouping = Some('_');
            pos += 1;
        }
        if end - pos >= 1 && s[pos] == ',' && f.grouping == Some('_') {
            return Err(both_comma_and_underscore());
        }
        // precision
        if end - pos >= 1 && s[pos] == '.' {
            pos += 1;
            let (p, consumed) = get_integer(s, &mut pos)?;
            if consumed > 0 {
                f.precision = Some(p);
            }
            // fractional grouping
            if end - pos >= 1 && s[pos] == ',' {
                f.frac_grouping = Some(',');
                pos += 1;
            }
            if end - pos >= 1 && s[pos] == '_' {
                if f.frac_grouping.is_some() {
                    return Err(both_comma_and_underscore());
                }
                f.frac_grouping = Some('_');
                pos += 1;
            }
            if end - pos >= 1 && s[pos] == ',' && f.frac_grouping == Some('_') {
                return Err(both_comma_and_underscore());
            }
            if consumed == 0 && f.frac_grouping.is_none() {
                return Err(NumFmtError::value("Format specifier missing precision"));
            }
        }
        // type
        if end - pos > 1 {
            return Err(NumFmtError::value(format!(
                "Invalid format specifier '{}' for object of type '{}'",
                spec, type_name
            )));
        }
        if end - pos == 1 {
            f.ty = s[pos];
        }
        if let Some(sep) = f.grouping {
            match f.ty {
                'd' | 'e' | 'f' | 'g' | 'E' | 'G' | '%' | 'F' | '\0' => {}
                'b' | 'o' | 'x' | 'X' if sep == '_' => {}
                t => return Err(invalid_thousands_separator_type(sep, t)),
            }
        }
        if let Some(sep) = f.frac_grouping {
            match f.ty {
                'e' | 'f' | 'g' | 'E' | 'G' | '%' | 'F' | '\0' => {}
                t => return Err(invalid_thousands_separator_type(sep, t)),
            }
        }
        Ok(f)
    }

    /// Parse a spec for a `float` (`default_type` none, right aligned).
    pub fn parse_float(spec: &str, type_name: &str) -> Result<FormatSpec, NumFmtError> {
        FormatSpec::parse(spec, '\0', Align::Right, type_name)
    }

    /// Parse a spec for an `int` (`default_type` 'd', right aligned).
    pub fn parse_int(spec: &str, type_name: &str) -> Result<FormatSpec, NumFmtError> {
        FormatSpec::parse(spec, 'd', Align::Right, type_name)
    }

    /// Parse a spec for a `str` (`default_type` 's', left aligned).
    pub fn parse_str(spec: &str, type_name: &str) -> Result<FormatSpec, NumFmtError> {
        FormatSpec::parse(spec, 's', Align::Left, type_name)
    }
}

fn both_comma_and_underscore() -> NumFmtError {
    NumFmtError::value("Cannot specify both ',' and '_'.")
}

pub(crate) fn invalid_thousands_separator_type(sep: char, ty: char) -> NumFmtError {
    let t = ty as u32;
    if t > 32 && t < 128 {
        NumFmtError::value(format!("Cannot specify '{}' with '{}'.", sep, ty))
    } else {
        NumFmtError::value(format!("Cannot specify '{}' with '\\x{:x}'.", sep, t))
    }
}

/// "Unknown format code" error.
pub fn unknown_presentation_type(ty: char, type_name: &str) -> NumFmtError {
    let t = ty as u32;
    if t > 32 && t < 128 {
        NumFmtError::value(format!(
            "Unknown format code '{}' for object of type '{}'",
            ty, type_name
        ))
    } else {
        NumFmtError::value(format!(
            "Unknown format code '\\x{:x}' for object of type '{}'",
            t, type_name
        ))
    }
}

/// Parse a run of ASCII decimal digits.  Returns (value, digits consumed).
/// Overflow of Py_ssize_t raises "Too many decimal digits in format string".
fn get_integer(s: &[char], pos: &mut usize) -> Result<(usize, usize), NumFmtError> {
    let mut acc: u64 = 0;
    let mut n = 0;
    while *pos < s.len() {
        let c = s[*pos];
        let d = match c {
            '0'..='9' => c as u64 - '0' as u64,
            _ => break,
        };
        // Py_ssize_t max on 64-bit platforms.
        if acc > (i64::MAX as u64 - d) / 10 {
            return Err(NumFmtError::value("Too many decimal digits in format string"));
        }
        acc = acc * 10 + d;
        *pos += 1;
        n += 1;
    }
    Ok((acc as usize, n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<FormatSpec, NumFmtError> {
        FormatSpec::parse_float(s, "float")
    }

    #[test]
    fn parse_basic() {
        let f = p("*^+z#012,.3_f").unwrap();
        assert_eq!(f.fill, '*');
        assert_eq!(f.align, Align::Center);
        assert_eq!(f.sign, Some('+'));
        assert!(f.no_neg_0 && f.alternate);
        // fill was specified, so the '0' belongs to the width
        assert_eq!(f.width, Some(12));
        assert_eq!(f.grouping, Some(','));
        assert_eq!(f.precision, Some(3));
        assert_eq!(f.frac_grouping, Some('_'));
        assert_eq!(f.ty, 'f');

        let f = p("010").unwrap();
        assert_eq!((f.fill, f.align, f.width), ('0', Align::AfterSign, Some(10)));
        let f = FormatSpec::parse_str("08", "str").unwrap();
        assert_eq!((f.fill, f.align, f.width), ('0', Align::Left, Some(8)));
        let f = p("00").unwrap();
        assert_eq!(f.width, Some(0));
    }

    #[test]
    fn parse_errors() {
        let e = |s: &str| p(s).unwrap_err().msg;
        assert_eq!(e("%M"), "Invalid format specifier '%M' for object of type 'float'");
        assert_eq!(e("fz"), "Invalid format specifier 'fz' for object of type 'float'");
        assert_eq!(e("z+f"), "Invalid format specifier 'z+f' for object of type 'float'");
        assert_eq!(e(",,"), "Cannot specify ',' with ','.");
        assert_eq!(e("__"), "Cannot specify '_' with '_'.");
        assert_eq!(e(",_"), "Cannot specify both ',' and '_'.");
        assert_eq!(e("_,"), "Cannot specify both ',' and '_'.");
        assert_eq!(e(".,_f"), "Cannot specify both ',' and '_'.");
        assert_eq!(e("._,f"), "Cannot specify both ',' and '_'.");
        assert_eq!(e(".6,_f"), "Cannot specify both ',' and '_'.");
        assert_eq!(e(".6_,f"), "Cannot specify both ',' and '_'.");
        assert!(p("._6f").is_err());
        assert_eq!(e(".6_n"), "Cannot specify '_' with 'n'.");
        assert_eq!(e(".6,n"), "Cannot specify ',' with 'n'.");
        assert_eq!(e(".f"), "Format specifier missing precision");
        let big = format!(".{}f", "9".repeat(30));
        assert_eq!(e(&big), "Too many decimal digits in format string");
        let big = format!("{}f", "1".repeat(10000));
        assert_eq!(e(&big), "Too many decimal digits in format string");
        let es = |s: &str| FormatSpec::parse_str(s, "str").unwrap_err().msg;
        assert_eq!(es(",s"), "Cannot specify ',' with 's'.");
        assert_eq!(es(","), "Cannot specify ',' with 's'.");
        assert_eq!(
            es("%ЫйЯЧ"),
            "Invalid format specifier '%ЫйЯЧ' for object of type 'str'"
        );
        let ei = |s: &str| FormatSpec::parse_int(s, "int").unwrap_err().msg;
        assert_eq!(ei(",x"), "Cannot specify ',' with 'x'.");
        assert_eq!(ei(",c"), "Cannot specify ',' with 'c'.");
        assert_eq!(ei(",n"), "Cannot specify ',' with 'n'.");
        assert_eq!(ei("_c"), "Cannot specify '_' with 'c'.");
        assert_eq!(ei(",\u{1}"), "Cannot specify ',' with '\\x1'.");
        assert!(FormatSpec::parse_int("_x", "int").is_ok());
    }
}
