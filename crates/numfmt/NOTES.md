# rpy-numfmt — number formatting / parsing (CPython 3.15 semantics)

No dependencies. `build.rs` generates the 617-entry 128-bit power-of-ten table
for Schubfach (it reuses `src/bignum.rs`).

All errors are `NumFmtError { kind: ErrorKind::{ValueError, OverflowError,
TypeError, MemoryError}, msg }`. The messages match CPython exactly unless noted
below. `MemoryError` (with an empty message) is returned when padding or
precision would need more memory than can be reserved, for example
`format(x, "9223372036854775807f")`, where CPython raises MemoryError.
No function panics on user input.

## Float → string

| function | meaning |
|---|---|
| `float_repr(x) -> String`, `write_float_repr(&mut String, x)` | `repr(x)` / `str(x)`. Uses Schubfach for the shortest digits. Exact ties between two shortest candidates go to the even last digit, as in Gay's dtoa mode 0. About 55 ns per value. |
| `double_to_string(&mut String, x, code, precision, DtoaFlags) -> Result<FloatKind>` | `PyOS_double_to_string`. `code` ∈ `e E f F g G r`. Flags: `sign`, `add_dot_0`, `alt`, `no_neg_0`. Correctly rounded (half-even on the exact binary value) for any precision. A u128 fast path handles small cases; a fixed-capacity bignum handles the rest. Precision above `PRECISION_MAX` (INT_MAX − 2^20) gives `ValueError("precision too big")`. |
| `float_fmt::write_float_repr_no_dot0(out, x, always_sign)` | repr of one complex component |

## Format-spec mini language (`__format__`)

* `FormatSpec::parse(spec, default_type, default_align, type_name)` is the
  general parser. Shortcuts: `parse_float` (type none, `>`), `parse_int` (`'d'`,
  `>`) and `parse_str` (`'s'`, `<`). It covers 3.14's fractional grouping
  (`.6_f`) and all parse errors: "Invalid format specifier '…' for object of type '…'",
  "Cannot specify ',' with ','.", "Cannot specify both ',' and '_'.",
  "Format specifier missing precision", "Too many decimal digits in format string".
* float: `format_float(x, spec, type_name) -> Result<String>`. An empty spec
  gives repr. `format::format_float_spec(out, x, &FormatSpec, &Locale, type_name)`
  takes an already parsed spec.
* int, three steps (the caller owns the bigint):
  1. `spec = FormatSpec::parse_int(s, "int")?`
  2. `match format::int_presentation(&spec, "int")?`:
     * `IntPresentation::Radix(r)`: call
       `format_int(out, negative, magnitude_digits_in_radix_r_lowercase, &spec, &Locale)`.
     * `Char`: call `format_int_char(out, value_as_Option<i64>, &spec)`. Use
       `format::int_char_layout` instead for lone surrogates.
     * `Float`: convert to float (the caller raises OverflowError) and call
       `format_float_spec`.
  * `format_i128(v, spec, type_name)` does all of this for machine-size ints.
* str: `format_str(s, spec, type_name)`. `format::str_layout(len_in_code_points, &spec, type_name)`
  returns `StrLayout { take, lpad, rpad, fill }` for runtimes whose strings are not
  Rust `&str` (for example PEP 393 strings or strings with surrogates).
* complex: `format_complex(re, im, spec, type_name)` and `format::format_complex_spec`.
  Also `complex_repr(re, im)` and `write_complex_repr`.
* Locale: `Locale { decimal_point, thousands_sep, grouping: Vec<u8> }` with C
  `localeconv` grouping semantics (0 repeats the last size, 127 stops grouping).
  `Locale::c()` is the default. The `&str` convenience wrappers always use the C
  locale. Pass the real locale to the `*_spec` functions for type `'n'`.

## printf-style `%`

* `percent::parse_percent_spec(fmt: &[T], start, is_bytes, is_printable) -> Result<PercentSpec>`
  takes `T` = `u8`/`u16`/`u32`/`char`. It parses one conversion starting at the
  `%`: `%%`, `(key)` with nesting, the flags `-+ #0`, width/precision as
  `PercentNum::{None, Star, Value}`, and the `h l L` modifiers, which are
  skipped. Errors are the 3.15 messages: "stray % at position N", "unsupported
  format %z at position N", "stray % at position N or unexpected format character
  'c' (U+XXXX) at position M" (bytes: "with code 0x..") ,
  "stray % or incomplete format key at position N", "width too big …",
  "precision too big …", and "* cannot be used with a parenthesised mapping key …".
  `percent::default_is_printable` is an approximation; pass the Unicode DB's
  `isprintable` instead.
* `PercentOpts { flags, width, precision }` holds the resolved values (the
  runtime resolves `*`; a negative `*` width sets `flags.left`).
  `PercentOpts::from_spec` covers the case with no `*`.
* `percent_format_float(out, x, conv, &opts)` handles `e E f F g G`.
* `percent_format_int(out, negative, digits, conv, &opts) -> Result<()>` handles
  `d i u o x X`. It applies precision zero-padding, the `0o`/`0x`/`0X` prefixes
  for `#`, and the `0` flag.
* `percent_format_str(out, s, conv, &opts) -> Result<()>` and `percent_str_layout(len, conv, &opts)`
  handle `s r a c b`.
* `percent_char_range_error(is_bytes)` returns the `%c` OverflowError text.
  The runtime builds the TypeError messages ("format argument 1: %x requires
  an integer, not float", …) because they depend on argument plumbing.

## Parsing

* `parse_float(&str)` implements `float(str)` using built-in Unicode
  approximations: `default_is_space` (White_Space) and `default_decimal`
  (a table of Nd blocks). `parse::parse_float_with(s, is_space, decimal)` takes
  the Unicode DB predicates instead. `parse_float_bytes(&[u8])` covers bytes
  and ASCII-transformed input.
  * It accepts surrounding whitespace, PEP 515 underscores between digits,
    and `inf`/`infinity`/`nan` in any case with a sign. `-nan` gives a NaN with
    the sign bit set.
  * The digits are correctly rounded: the grammar is validated here, then the
    work goes to Rust's `str::parse::<f64>`. Overflow gives ±inf.
  * On failure the message is `parse::FLOAT_PARSE_ERROR` ("could not convert
    string to float"). **The runtime must append `": " + repr(arg)`.**
* `parse::parse_float_prefix(&[u8]) -> Option<(f64, consumed)>` is
  `PyOS_string_to_double` with an end pointer.
* `parse_float_literal(&str)` uses the tokenizer grammar (digitpart /
  pointfloat / exponentfloat with single `_`), with no sign, whitespace, inf or nan.
* `parse_complex(&str) -> Result<(re, im)>` implements `complex(str)`,
  including parentheses, the `j`/`+j`/`1+j` forms, inf/nan parts and
  underscores. It also has `_with` and `_bytes` variants. The error message
  is "complex() arg is a malformed string".
* `parse::transform_decimal_and_space` mirrors `_PyUnicode_TransformDecimalAndSpaceToASCII`.

## float methods

* `float_hex(x)` / `write_float_hex`: `float.hex()`.
* `float_fromhex(&str)`: an exact `float.fromhex` with round-half-even and
  subnormal handling. Errors: ValueError "invalid hexadecimal floating-point
  string" and OverflowError "hexadecimal value too large to represent as a
  float". The caller handles subclass construction.
* `float_as_integer_ratio(x) -> (negative, odd_mantissa, exp2)` returns the
  ratio as `(m << exp, 1)` or `(m, 1 << -exp)`. Inf gives OverflowError, NaN
  gives ValueError.
* `float_round(x, ndigits: i64)` is `round(x, n)`: correctly rounded and
  half-even on the exact value, returning x for n > 323 and ±0.0 for n < −308.
  Overflow gives OverflowError("rounded value too large to represent"). Clamp
  huge Python ints to i64.
* `float_round_to_int(x)` is `round(x)` as an integral f64. Inf gives
  OverflowError and NaN gives ValueError, both with CPython's messages.

## Decisions / known gaps

* Rust `&str`/`char` cannot hold lone surrogates. Use the layout APIs
  (`str_layout`, `int_char_layout`, `percent_str_layout`) for such strings.
  Format specs themselves must be valid `&str`; a surrogate fill character is
  not supported.
* For `%`-formatting, the `0` flag also zero-pads `inf`/`nan` (`'%05f' % inf
  == '00inf'`), following the generic CPython output routine as I recall it.
  `format()` does the same (`format(inf, '010') == '0000000inf'`).
* `PRECISION_MAX` (INT_MAX − 2^20) is a guess at CPython's gh-158446 limit.
  The tests need INT_MAX − 1023 to be rejected.
* "Negative zero coercion (z) not allowed in string format specifier" and the
  `int` variant are written from memory. The tests check only the prefix.
* `%`-format end-of-string after the flags or width (`'%10'`) reports "stray % at
  position N". The 3.15 text for that case is untested.
* The default Unicode tables (`default_decimal`, `default_is_printable`,
  `default_is_space`) are approximations. The runtime should pass functions
  from the unicode crate.
