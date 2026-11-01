//! Expected values ported from the CPython test-suite (spec/Lib/test):
//! test_float.py, test_format.py, test_types.py, test_long.py, test_str.py,
//! string_tests.py, test_complex.py, test_builtin.py.

use rpy_numfmt::format::{format_float_spec, int_presentation, str_layout};
use rpy_numfmt::percent::*;
use rpy_numfmt::*;

const INF: f64 = f64::INFINITY;
const NAN: f64 = f64::NAN;

fn ff(x: f64, spec: &str) -> String {
    format_float(x, spec, "float").unwrap_or_else(|e| panic!("format({:?}, {:?}): {:?}", x, spec, e))
}
fn ff_err(x: f64, spec: &str) -> NumFmtError {
    format_float(x, spec, "float").unwrap_err()
}

fn radix_digits(mut v: u128, radix: u32) -> String {
    if v == 0 {
        return "0".into();
    }
    let mut s = Vec::new();
    while v > 0 {
        s.push(std::char::from_digit((v % radix as u128) as u32, radix).unwrap());
        v /= radix as u128;
    }
    s.iter().rev().collect()
}

fn fi(v: i128, spec: &str) -> Result<String, NumFmtError> {
    if spec.is_empty() {
        return Ok(v.to_string());
    }
    let s = FormatSpec::parse_int(spec, "int")?;
    let mut out = String::new();
    match int_presentation(&s, "int")? {
        IntPresentation::Radix(r) => {
            format_int(&mut out, v < 0, &radix_digits(v.unsigned_abs(), r), &s, &Locale::c())?
        }
        IntPresentation::Float => format_float_spec(&mut out, v as f64, &s, &Locale::c(), "int")?,
        IntPresentation::Char => {
            format_int_char(&mut out, i64::try_from(v).ok(), &s)?;
        }
    }
    Ok(out)
}
fn fi_ok(v: i128, spec: &str) -> String {
    fi(v, spec).unwrap_or_else(|e| panic!("format({}, {:?}): {:?}", v, spec, e))
}

fn fs(s: &str, spec: &str) -> Result<String, NumFmtError> {
    format_str(s, spec, "str")
}

fn fc(re: f64, im: f64, spec: &str) -> String {
    format_complex(re, im, spec, "complex").unwrap_or_else(|e| panic!("{:?}", e))
}

fn ident(a: f64, b: f64) -> bool {
    if a.is_nan() || b.is_nan() {
        return a.is_nan() && b.is_nan();
    }
    a == b && a.is_sign_negative() == b.is_sign_negative()
}

// ---------------------------------------------------------------------------
// % formatting helpers

enum Arg<'a> {
    I(i128),
    F(f64),
    S(&'a str),
}

/// Minimal %-formatting driver (tuple arguments only).
fn pct(fmt: &str, args: &[Arg]) -> Result<String, NumFmtError> {
    let chars: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut ai = 0;
    while i < chars.len() {
        if chars[i] != '%' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let spec = parse_percent_spec(&chars, i, false, &default_is_printable)?;
        i = spec.end;
        if spec.conv == '%' {
            out.push('%');
            continue;
        }
        let mut opts = PercentOpts::from_spec(&spec);
        if spec.width == PercentNum::Star {
            let Arg::I(w) = args[ai] else { panic!() };
            ai += 1;
            if w < 0 {
                opts.flags.left = true;
            }
            opts.width = Some(w.unsigned_abs() as usize);
        }
        if spec.precision == PercentNum::Star {
            let Arg::I(p) = args[ai] else { panic!() };
            ai += 1;
            opts.precision = Some(p.max(0) as usize);
        }
        let arg = &args[ai];
        ai += 1;
        match spec.conv {
            'd' | 'i' | 'u' | 'o' | 'x' | 'X' => {
                let v = match arg {
                    Arg::I(v) => *v,
                    Arg::F(f) => *f as i128,
                    _ => panic!(),
                };
                let radix = match spec.conv {
                    'o' => 8,
                    'x' | 'X' => 16,
                    _ => 10,
                };
                percent_format_int(&mut out, v < 0, &radix_digits(v.unsigned_abs(), radix), spec.conv, &opts);
            }
            'e' | 'E' | 'f' | 'F' | 'g' | 'G' => {
                let v = match arg {
                    Arg::I(v) => *v as f64,
                    Arg::F(f) => *f,
                    _ => panic!(),
                };
                percent_format_float(&mut out, v, spec.conv, &opts)?;
            }
            's' | 'r' | 'a' | 'c' => {
                let s = match arg {
                    Arg::S(s) => s.to_string(),
                    Arg::I(v) => v.to_string(),
                    Arg::F(f) => float_repr(*f),
                };
                percent_format_str(&mut out, &s, spec.conv, &opts);
            }
            _ => unreachable!(),
        }
    }
    Ok(out)
}

fn pct_ok(fmt: &str, args: &[Arg]) -> String {
    pct(fmt, args).unwrap_or_else(|e| panic!("{:?}: {:?}", fmt, e))
}

// ---------------------------------------------------------------------------
// float repr

#[test]
fn float_repr_rules() {
    for (x, s) in [
        (1.0, "1.0"),
        (-0.0, "-0.0"),
        (0.0, "0.0"),
        (1e16, "1e+16"),
        (1e-05, "1e-05"),
        (1e-4, "0.0001"),
        (123.456, "123.456"),
        (1e22, "1e+22"),
        (1.5e300, "1.5e+300"),
        (5e-324, "5e-324"),
        (INF, "inf"),
        (-INF, "-inf"),
        (NAN, "nan"),
        (-NAN, "nan"),
        (100.0 / 7.0, "14.285714285714286"),
        (9999999999999998.0, "9999999999999998.0"),
        (0.1 + 0.2, "0.30000000000000004"),
    ] {
        assert_eq!(float_repr(x), s);
    }
    let short = [
        "0.0", "1.0", "0.01", "0.02", "0.03", "0.04", "0.05", "1.23456789", "10.0", "100.0",
        "1000000000000000.0", "9999999999999990.0", "1e+16", "1e+17", "0.001", "0.001001",
        "0.00010000000000001", "0.0001", "9.999999999999e-05", "1e-05", "8.72293771110361e+25",
        "7.47005307342313e+26", "2.86438000439698e+28", "8.89142905246179e+28",
        "3.08578087079232e+35",
    ];
    for s in short {
        assert_eq!(float_repr(parse_float(s).unwrap()), s);
        let neg = format!("-{}", s);
        assert_eq!(float_repr(parse_float(&neg).unwrap()), neg);
    }
}

// ---------------------------------------------------------------------------
// float.__format__

#[test]
fn float_format_types_py() {
    assert_eq!(ff(0.0, "f"), "0.000000");
    assert_eq!(ff(0.0, ""), "0.0");
    assert_eq!(ff(0.01, ""), "0.01");
    assert_eq!(ff(0.01, "g"), "0.01");
    assert_eq!(ff(1.23, "1"), "1.23");
    assert_eq!(ff(-1.23, "1"), "-1.23");
    assert_eq!(ff(1.23, "1g"), "1.23");
    assert_eq!(ff(-1.23, "1g"), "-1.23");
    assert_eq!(ff(1.0, " g"), " 1");
    assert_eq!(ff(-1.0, " g"), "-1");
    assert_eq!(ff(1.0, "+g"), "+1");
    assert_eq!(ff(-1.0, "+g"), "-1");
    assert_eq!(ff(1.1234e200, "g"), "1.1234e+200");
    assert_eq!(ff(1.1234e200, "G"), "1.1234E+200");
    assert_eq!(ff(1.0, "f"), "1.000000");
    assert_eq!(ff(-1.0, "f"), "-1.000000");
    assert_eq!(ff(1.0, " f"), " 1.000000");
    assert_eq!(ff(-1.0, " f"), "-1.000000");
    assert_eq!(ff(1.0, "+f"), "+1.000000");
    assert_eq!(ff(-1.0, "+f"), "-1.000000");
    for (f, len) in [(1.1234e90, 98), (1.1234e200, 208)] {
        for t in ["f", "F"] {
            let r = ff(f, t);
            assert_eq!(r.len(), len);
            assert_eq!(&r[r.len() - 7..r.len() - 6], ".");
            assert!(r.starts_with("112340000000") || r.starts_with("112339999999"));
        }
    }
    assert_eq!(ff(1.0, "e"), "1.000000e+00");
    assert_eq!(ff(-1.0, "e"), "-1.000000e+00");
    assert_eq!(ff(1.0, "E"), "1.000000E+00");
    assert_eq!(ff(-1.0, "E"), "-1.000000E+00");
    assert_eq!(ff(1.1234e20, "e"), "1.123400e+20");
    assert_eq!(ff(1.1234e20, "E"), "1.123400E+20");
    assert_eq!(ff(1.25e200, "+g"), "+1.25e+200");
    assert_eq!(ff(1.25e200, "+"), "+1.25e+200");
    assert_eq!(ff(1.1e200, "+g"), "+1.1e+200");
    assert_eq!(ff(1.1e200, "+"), "+1.1e+200");
    assert_eq!(ff(1234., "010f"), "1234.000000");
    assert_eq!(ff(1234., "011f"), "1234.000000");
    assert_eq!(ff(1234., "012f"), "01234.000000");
    assert_eq!(ff(-1234., "011f"), "-1234.000000");
    assert_eq!(ff(-1234., "012f"), "-1234.000000");
    assert_eq!(ff(-1234., "013f"), "-01234.000000");
    assert_eq!(ff(-1234.12341234, "013f"), "-01234.123412");
    assert_eq!(ff(-123456.12341234, "011.2f"), "-0123456.12");
    assert_eq!(ff(1.2, "010,.2"), "0,000,001.2");
    assert_eq!(ff(1234., "011,f"), "1,234.000000");
    assert_eq!(ff(1234., "012,f"), "1,234.000000");
    assert_eq!(ff(1234., "013,f"), "01,234.000000");
    assert_eq!(ff(-1234., "012,f"), "-1,234.000000");
    assert_eq!(ff(-1234., "013,f"), "-1,234.000000");
    assert_eq!(ff(-1234., "014,f"), "-01,234.000000");
    assert_eq!(ff(-12345., "015,f"), "-012,345.000000");
    assert_eq!(ff(-123456., "016,f"), "-0,123,456.000000");
    assert_eq!(ff(-123456., "017,f"), "-0,123,456.000000");
    assert_eq!(ff(-123456.12341234, "017,f"), "-0,123,456.123412");
    assert_eq!(ff(-123456.12341234, "013,.2f"), "-0,123,456.12");
    assert_eq!(ff(-1.0, "%"), "-100.000000%");
    for t in ["s", "b", "c", "d", "o", "x", "X"] {
        for v in [0.0, 1.0, -1.0, 1e100, -1e100, 1e-100, -1e-100] {
            assert_eq!(ff_err(v, t).kind, ErrorKind::ValueError);
        }
    }
    assert_eq!(ff_err(3.0, "s").msg, "Unknown format code 's' for object of type 'float'");
    assert_eq!(ff(1.0, ".0e"), "1e+00");
    assert_eq!(ff(1.0, "#.0e"), "1.e+00");
    assert_eq!(ff(1.0, ".0f"), "1");
    assert_eq!(ff(1.0, "#.0f"), "1.");
    assert_eq!(ff(1.1, "g"), "1.1");
    assert_eq!(ff(1.1, "#g"), "1.10000");
    assert_eq!(ff(1.0, ".0%"), "100%");
    assert_eq!(ff(1.0, "#.0%"), "100.%");
    assert_eq!(ff(1.0, "0e"), "1.000000e+00");
    assert_eq!(ff(1.0, "#0e"), "1.000000e+00");
    assert_eq!(ff(1.0, "0f"), "1.000000");
    assert_eq!(ff(1.0, "#0f"), "1.000000");
    assert_eq!(ff(1.0, ".1e"), "1.0e+00");
    assert_eq!(ff(1.0, "#.1e"), "1.0e+00");
    assert_eq!(ff(1.0, ".1f"), "1.0");
    assert_eq!(ff(1.0, "#.1f"), "1.0");
    assert_eq!(ff(1.0, ".1%"), "100.0%");
    assert_eq!(ff(1.0, "#.1%"), "100.0%");
    assert_eq!(ff(12345.6, "0<20"), "12345.60000000000000");
    assert_eq!(ff(12345.6, "1<20"), "12345.61111111111111");
    assert_eq!(ff(12345.6, "*<20"), "12345.6*************");
    assert_eq!(ff(12345.6, "0>20"), "000000000000012345.6");
    assert_eq!(ff(12345.6, "1>20"), "111111111111112345.6");
    assert_eq!(ff(12345.6, "*>20"), "*************12345.6");
    assert_eq!(ff(12345.6, "0=20"), "000000000000012345.6");
    assert_eq!(ff(12345.6, "1=20"), "111111111111112345.6");
    assert_eq!(ff(12345.6, "*=20"), "*************12345.6");
}

#[test]
fn float_to_string_exponents() {
    for i in -99..100 {
        let x = parse_float(&format!("1.5e{}", i)).unwrap();
        let exp = if i < 0 { format!("-{:02}", -i) } else { format!("+{:02}", i) };
        let want = format!("1.500000e{}", exp);
        assert_eq!(ff(x, "e"), want);
        assert_eq!(pct_ok("%e", &[Arg::F(x)]), want);
    }
    assert_eq!(ff(1.5e100, "e"), "1.500000e+100");
    assert_eq!(ff(1.5e101, "e"), "1.500000e+101");
    assert_eq!(ff(1.5e-100, "e"), "1.500000e-100");
    assert_eq!(ff(1.5e-101, "e"), "1.500000e-101");
    assert_eq!(pct_ok("%g", &[Arg::F(1.0)]), "1");
    assert_eq!(pct_ok("%#g", &[Arg::F(1.0)]), "1.00000");
}

#[test]
fn float_format_test_float_py() {
    let x = 100.0 / 7.0;
    for s in ["", "-", ">", "2"] {
        assert_eq!(ff(x, s), float_repr(x));
    }
    assert_eq!(ff(f64::NAN, "f"), "nan");
    assert_eq!(ff(f64::NAN, "F"), "NAN");
    assert_eq!(ff(INF, "f"), "inf");
    assert_eq!(ff(INF, "F"), "INF");
    let x = 123_456.123_456;
    assert_eq!(ff(x, "_f"), "123_456.123456");
    assert_eq!(ff(x, ",f"), "123,456.123456");
    assert_eq!(ff(x, "._f"), "123456.123_456");
    assert_eq!(ff(x, ".,f"), "123456.123,456");
    assert_eq!(ff(x, "_._f"), "123_456.123_456");
    assert_eq!(ff(x, ",.,f"), "123,456.123,456");
    assert_eq!(ff(x, ".10_f"), "123456.123_456_000_0");
    assert_eq!(ff(x, ".10,f"), "123456.123,456,000,0");
    assert_eq!(ff(x, ">21._f"), "       123456.123_456");
    assert_eq!(ff(x, "<21._f"), "123456.123_456       ");
    assert_eq!(ff(x, "+.11_e"), "+1.234_561_234_56e+05");
    assert_eq!(ff(x, "+.11,e"), "+1.234,561,234,56e+05");
    assert_eq!(ff(x, "021_._f"), "0_000_123_456.123_456");
    assert_eq!(ff(x, "020_._f"), "0_000_123_456.123_456");
    assert_eq!(ff(x, "+021_._f"), "+0_000_123_456.123_456");
    assert_eq!(ff(x, "21_._f"), "      123_456.123_456");
    assert_eq!(ff(x, ">021_._f"), "000000123_456.123_456");
    assert_eq!(ff(x, "<021_._f"), "123_456.123_456000000");
    assert_eq!(ff(x, "023_.10_f"), "0_123_456.123_456_000_0");
    assert_eq!(ff(x, "022_.10_f"), "0_123_456.123_456_000_0");
    assert_eq!(ff(x, "+023_.10_f"), "+0_123_456.123_456_000_0");
    assert_eq!(ff(x, "023_.9_f"), "000_123_456.123_456_000");
    assert_eq!(ff(x, "021_._e"), "0_000_001.234_561e+05");
    assert_eq!(ff(x, "020_._e"), "0_000_001.234_561e+05");
    assert_eq!(ff(x, "+021_._e"), "+0_000_001.234_561e+05");
    assert_eq!(ff(x, "023_.10_e"), "0_001.234_561_234_6e+05");
    assert_eq!(ff(x, "022_.10_e"), "0_001.234_561_234_6e+05");
    assert_eq!(ff(x, "023_.9_e"), "000_001.234_561_235e+05");
    for s in ["._6f", ".,_f", ".6,_f", ".6_,f", ".6_n", ".6,n"] {
        assert_eq!(ff_err(x, s).kind, ErrorKind::ValueError, "{}", s);
    }
    // issue5864
    assert_eq!(ff(123.456, ".4"), "123.5");
    assert_eq!(ff(1234.56, ".4"), "1.235e+03");
    assert_eq!(ff(12345.6, ".4"), "1.235e+04");
    // issue35560
    assert_eq!(ff(123.0, "00"), "123.0");
    assert_eq!(ff(123.34, "00f"), "123.340000");
    assert_eq!(ff(123.34, "00e"), "1.233400e+02");
    assert_eq!(ff(123.34, "00g"), "123.34");
    assert_eq!(ff(123.34, "00.10f"), "123.3400000000");
    assert_eq!(ff(123.34, "00.10e"), "1.2334000000e+02");
    assert_eq!(ff(123.34, "00.10g"), "123.34");
    assert_eq!(ff(123.34, "01f"), "123.340000");
    assert_eq!(ff(-123.0, "00"), "-123.0");
    assert_eq!(ff(-123.34, "00f"), "-123.340000");
    assert_eq!(ff(-123.34, "00e"), "-1.233400e+02");
    assert_eq!(ff(-123.34, "00g"), "-123.34");
    assert_eq!(ff(-123.34, "00.10f"), "-123.3400000000");
    assert_eq!(ff(-123.34, "00.10e"), "-1.2334000000e+02");
    assert_eq!(ff(-123.34, "00.10g"), "-123.34");
}

#[test]
fn format_specials() {
    let fmts = ["%e", "%f", "%g", "%.0e", "%.6f", "%.20g", "%#e", "%#f", "%#g", "%#.20e", "%#.15f", "%#.3g"];
    for fmt in fmts {
        let pfmt = format!("%+{}", &fmt[1..]);
        let sfmt = format!("% {}", &fmt[1..]);
        let t = |f: &str, v: f64, want: &str| {
            assert_eq!(pct_ok(f, &[Arg::F(v)]), want, "{}", f);
            assert_eq!(ff(v, &f[1..]), want, "{}", f);
        };
        t(fmt, INF, "inf");
        t(fmt, -INF, "-inf");
        t(fmt, NAN, "nan");
        t(fmt, -NAN, "nan");
        t(&pfmt, INF, "+inf");
        t(&pfmt, -INF, "-inf");
        t(&pfmt, NAN, "+nan");
        t(&pfmt, -NAN, "+nan");
        t(&sfmt, INF, " inf");
        t(&sfmt, -INF, "-inf");
        t(&sfmt, NAN, " nan");
        t(&sfmt, -NAN, " nan");
    }
}

#[test]
fn negative_zero_z() {
    assert_eq!(ff(-0., ".1f"), "-0.0");
    assert_eq!(ff(-0.01, ".1f"), "-0.0");
    assert_eq!(fi_ok(0, ".1f"), "0.0");
    assert_eq!(ff(0., "z.1f"), "0.0");
    assert_eq!(ff(0., "z6.1f"), "   0.0");
    assert_eq!(ff(-1., "z6.1f"), "  -1.0");
    assert_eq!(ff(-0., "z.1f"), "0.0");
    assert_eq!(ff(0.01, "z.1f"), "0.0");
    assert_eq!(fi_ok(0, "z.1f"), "0.0");
    assert_eq!(ff(-0.01, "z.1f"), "0.0");
    assert_eq!(ff(0., "z.2f"), "0.00");
    assert_eq!(ff(-0., "z.2f"), "0.00");
    assert_eq!(ff(0.001, "z.2f"), "0.00");
    assert_eq!(ff(-0.001, "z.2f"), "0.00");
    assert_eq!(ff(0., "z.1e"), "0.0e+00");
    assert_eq!(ff(-0., "z.1e"), "0.0e+00");
    assert_eq!(ff(0., "z.1E"), "0.0E+00");
    assert_eq!(ff(-0., "z.1E"), "0.0E+00");
    assert_eq!(ff(-0.001, "z.2e"), "-1.00e-03");
    assert_eq!(ff(-0.001, "z.2g"), "-0.001");
    assert_eq!(ff(-0.001, "z.2%"), "-0.10%");
    assert_eq!(ff(-0.000001, "z.1f"), "0.0");
    assert_eq!(ff(-0.000001, "z.2f"), "0.00");
    assert_eq!(ff(0.09, "z.1f"), "0.1");
    assert_eq!(ff(-0.09, "z.1f"), "-0.1");
    assert_eq!(ff(-0., " z.0f"), " 0");
    assert_eq!(ff(-0., "+z.0f"), "+0");
    assert_eq!(ff(-0., "-z.0f"), "0");
    assert_eq!(ff(-1., " z.0f"), "-1");
    assert_eq!(ff(-1., "+z.0f"), "-1");
    assert_eq!(ff(-1., "-z.0f"), "-1");
    assert_eq!(fc(0., 0., "z.1f"), "0.0+0.0j");
    assert_eq!(fc(0., -0., "z.1f"), "0.0+0.0j");
    assert_eq!(fc(0., 0.01, "z.1f"), "0.0+0.0j");
    assert_eq!(fc(0., -0.01, "z.1f"), "0.0+0.0j");
    assert_eq!(ff(-0., "z>6.1f"), "zz-0.0");
    assert_eq!(ff(-0., "z>z6.1f"), "zzz0.0");
    assert_eq!(ff(-0., "x>z6.1f"), "xxx0.0");
    assert_eq!(ff(-0., "🖤>z6.1f"), "🖤🖤🖤0.0");
    assert!(ff_err(0.0, "z+f").msg.starts_with("Invalid format specifier"));
    assert!(ff_err(0.0, "fz").msg.starts_with("Invalid format specifier"));
    assert!(fi(0, "z+f").unwrap_err().msg.starts_with("Invalid format specifier"));
    assert!(fi(0, "zd").unwrap_err().msg.starts_with("Negative zero coercion (z) not allowed"));
    assert!(fs("x", "zs").unwrap_err().msg.starts_with("Negative zero coercion (z) not allowed"));
    let e = pct("%z.1f", &[Arg::I(0)]).unwrap_err();
    assert_eq!(e.msg, "unsupported format %z at position 0");
}

#[test]
fn g_format_no_trailing_zeros_and_precision() {
    assert_eq!(pct_ok("%.3g", &[Arg::F(1505.0)]), "1.5e+03");
    assert_eq!(pct_ok("%#.3g", &[Arg::F(1505.0)]), "1.50e+03");
    assert_eq!(ff(1505.0, ".3g"), "1.5e+03");
    assert_eq!(ff(1505.0, "#.3g"), "1.50e+03");
    assert_eq!(ff(12300050.0, ".6g"), "1.23e+07");
    assert_eq!(ff(12300050.0, "#.6g"), "1.23000e+07");
    assert_eq!(ff(1.2, ".0f"), "1");
    assert_eq!(ff(1.2, ".3f"), "1.200");
    let huge = format!(".{}f", (i64::MAX as u64) + 1);
    assert_eq!(ff_err(1.2, &huge).kind, ErrorKind::ValueError);
    let huge = format!("{}f", (i64::MAX as u64) + 1);
    assert_eq!(ff_err(2.34, &huge).kind, ErrorKind::ValueError);
    let int_max = i32::MAX as i64;
    for p in [int_max + 1, int_max, int_max - 1023] {
        for code in ["f", "e", "g"] {
            let spec = format!(".{}{}", p, code);
            assert_eq!(format_float(1e300, &spec, "float").unwrap_err().kind, ErrorKind::ValueError);
            assert_eq!(format_complex(1e300, 0.0, &spec, "complex").unwrap_err().kind, ErrorKind::ValueError);
            let pfmt = format!("%{}", spec);
            assert_eq!(pct(&pfmt, &[Arg::F(1e300)]).unwrap_err().kind, ErrorKind::ValueError);
        }
    }
    assert_eq!(fc(1.2, 0.0, ".0f"), "1+0j");
    assert_eq!(fc(1.2, 0.0, ".3f"), "1.200+0.000j");
}

#[test]
fn format_errors_messages() {
    for (spec, msg) in [
        ("%M", "Invalid format specifier '%M' for object of type 'float'"),
    ] {
        assert_eq!(ff_err(12.0, spec).msg, msg);
    }
    assert_eq!(fi(12, "%M").unwrap_err().msg, "Invalid format specifier '%M' for object of type 'int'");
    assert_eq!(fs("12", "%M").unwrap_err().msg, "Invalid format specifier '%M' for object of type 'str'");
    assert_eq!(
        format_complex(0.0, 12.0, "%M", "complex").unwrap_err().msg,
        "Invalid format specifier '%M' for object of type 'complex'"
    );
    assert_eq!(fi(1, ",,").unwrap_err().msg, "Cannot specify ',' with ','.");
    assert_eq!(fi(1, "__").unwrap_err().msg, "Cannot specify '_' with '_'.");
    assert_eq!(fi(1, ",_").unwrap_err().msg, "Cannot specify both ',' and '_'.");
    assert_eq!(ff_err(1.1, ".,_f").msg, "Cannot specify both ',' and '_'.");
    assert_eq!(fi(1, "_,").unwrap_err().msg, "Cannot specify both ',' and '_'.");
    assert_eq!(ff_err(1.1, "._,f").msg, "Cannot specify both ',' and '_'.");
    assert_eq!(fi(1000, "j").unwrap_err().msg, "Unknown format code 'j' for object of type 'int'");
    // test_format_spec_errors
    assert!(fi(0, &format!("{}d", "1".repeat(10000))).is_err());
    assert!(fi(0, &format!(".{}d", "1".repeat(10000))).is_err());
    assert!(fi(0, &format!("{}.{}d", "1".repeat(1000), "1".repeat(10000))).is_err());
    for code in ["x", "X", "o", "b", "n", "s"] {
        assert!(fi(0, &format!(",{}", code)).is_err());
    }
}

// ---------------------------------------------------------------------------
// int.__format__

#[test]
fn int_format() {
    assert_eq!(fi_ok(123456789, "d"), "123456789");
    assert_eq!(fi_ok(1, "c"), "\u{1}");
    assert_eq!(fi_ok(123456789, ","), "123,456,789");
    assert_eq!(fi_ok(123456789, "_"), "123_456_789");
    for (v, s, r) in [
        (1, "-", "1"), (-1, "-", "-1"), (1, "-3", "  1"), (-1, "-3", " -1"), (1, "+3", " +1"),
        (-1, "+3", " -1"), (1, " 3", "  1"), (-1, " 3", " -1"), (1, " ", " 1"), (-1, " ", "-1"),
        (3, "x", "3"), (3, "X", "3"), (1234, "x", "4d2"), (-1234, "x", "-4d2"),
        (1234, "8x", "     4d2"), (-1234, "8x", "    -4d2"), (-3, "x", "-3"), (-3, "X", "-3"),
        (0xbe, "x", "be"), (0xbe, "X", "BE"), (-0xbe, "x", "-be"), (-0xbe, "X", "-BE"),
        (1234567890, "_x", "4996_02d2"), (1234567890, "_X", "4996_02D2"),
        (3, "o", "3"), (-3, "o", "-3"), (65, "o", "101"), (-65, "o", "-101"), (1234, "o", "2322"),
        (-1234, "o", "-2322"), (1234, "-o", "2322"), (-1234, "-o", "-2322"), (1234, " o", " 2322"),
        (-1234, " o", "-2322"), (1234, "+o", "+2322"), (-1234, "+o", "-2322"),
        (1234567890, "_o", "111_4540_1322"),
        (3, "b", "11"), (-3, "b", "-11"), (1234, "b", "10011010010"), (-1234, "b", "-10011010010"),
        (1234, "-b", "10011010010"), (-1234, "-b", "-10011010010"), (1234, " b", " 10011010010"),
        (-1234, " b", "-10011010010"), (1234, "+b", "+10011010010"), (-1234, "+b", "-10011010010"),
        (12345, "_b", "11_0000_0011_1001"),
        (0, "#b", "0b0"), (0, "-#b", "0b0"), (1, "-#b", "0b1"), (-1, "-#b", "-0b1"),
        (-1, "-#5b", " -0b1"), (1, "+#5b", " +0b1"), (100, "+#b", "+0b1100100"),
        (100, "#012b", "0b0001100100"), (-100, "#012b", "-0b001100100"),
        (0, "#o", "0o0"), (0, "-#o", "0o0"), (1, "-#o", "0o1"), (-1, "-#o", "-0o1"),
        (-1, "-#5o", " -0o1"), (1, "+#5o", " +0o1"), (100, "+#o", "+0o144"),
        (100, "#012o", "0o0000000144"), (-100, "#012o", "-0o000000144"),
        (0, "#x", "0x0"), (0, "-#x", "0x0"), (1, "-#x", "0x1"), (-1, "-#x", "-0x1"),
        (-1, "-#5x", " -0x1"), (1, "+#5x", " +0x1"), (100, "+#x", "+0x64"),
        (100, "#012x", "0x0000000064"), (-100, "#012x", "-0x000000064"),
        (123456, "#012x", "0x000001e240"), (-123456, "#012x", "-0x00001e240"),
        (0, "#X", "0X0"), (0, "-#X", "0X0"), (1, "-#X", "0X1"), (-1, "-#X", "-0X1"),
        (-1, "-#5X", " -0X1"), (1, "+#5X", " +0X1"), (100, "+#X", "+0X64"),
        (100, "#012X", "0X0000000064"), (-100, "#012X", "-0X000000064"),
        (123456, "#012X", "0X000001E240"), (-123456, "#012X", "-0X00001E240"),
        (123, ",", "123"), (-123, ",", "-123"), (1234, ",", "1,234"), (-1234, ",", "-1,234"),
        (123456, ",", "123,456"), (-123456, ",", "-123,456"), (1234567, ",", "1,234,567"),
        (-1234567, ",", "-1,234,567"), (1234, "010,", "00,001,234"),
        (123456, "0<20", "12345600000000000000"), (123456, "1<20", "12345611111111111111"),
        (123456, "*<20", "123456**************"), (123456, "0>20", "00000000000000123456"),
        (123456, "1>20", "11111111111111123456"), (123456, "*>20", "**************123456"),
        (123456, "0=20", "00000000000000123456"), (123456, "1=20", "11111111111111123456"),
        (123456, "*=20", "**************123456"),
        (123, "\u{2007}<5", "123\u{2007}\u{2007}"), (123, "\u{2007}>5", "\u{2007}\u{2007}123"),
        (123, "\u{2007}^5", "\u{2007}123\u{2007}"),
        (3, "\0<6", "3\0\0\0\0\0"), (3, "\u{1}<6", "3\u{1}\u{1}\u{1}\u{1}\u{1}"),
        (3, "\0^6", "\0\03\0\0\0"), (3, "<6", "3     "),
    ] {
        assert_eq!(fi_ok(v, s), r, "format({}, {:?})", v, s);
    }
    let big = 10i128.pow(30);
    assert_eq!(fi_ok(big, "d"), format!("1{}", "0".repeat(30)));
    for s in ["1.3", "+c", ",n", ",c", "#c", "_c", ",x", ",o", ",b"] {
        assert_eq!(fi(3, s).unwrap_err().kind, ErrorKind::ValueError, "{}", s);
    }
    assert!(fi(3, "_,").unwrap_err().msg.starts_with("Cannot specify both"));
    assert!(fi(3, ",_").unwrap_err().msg.starts_with("Cannot specify both"));
    assert!(fi(3, "_,d").unwrap_err().msg.starts_with("Cannot specify both"));
    assert!(fi(3, ",_d").unwrap_err().msg.starts_with("Cannot specify both"));
    assert!(fi(3, ",s").unwrap_err().msg.starts_with("Cannot specify ',' with 's'"));
    assert!(fi(3, "_s").unwrap_err().msg.starts_with("Cannot specify '_' with 's'"));
    for c in ('a'..='z').chain('A'..='Z') {
        let s = c.to_string();
        if !"bcdoxXeEfFgGn%".contains(c) {
            for v in [0, 1, -1, 1i128 << 100, -(1i128 << 100)] {
                assert_eq!(fi(v, &s).unwrap_err().kind, ErrorKind::ValueError);
            }
        }
    }
    for t in ["e", "E", "f", "F", "g", "G", "%"] {
        for v in [0i128, 1, -1, 100, -100, 1234567890, -1234567890] {
            assert_eq!(fi_ok(v, t), ff(v as f64, t));
        }
    }
    assert_eq!(fi(-1, "c").unwrap_err().kind, ErrorKind::OverflowError);
    assert_eq!(fi(0x110000, "c").unwrap_err().msg, "%c arg not in range(0x110000)");
    assert_eq!(fi_ok(65, "05c"), "0000A");
    assert_eq!(fi_ok(0x1F5A4, "^3c"), " 🖤 ");
    // 'n' with the C locale: no grouping
    assert_eq!(fi_ok(123456789, "n"), "123456789");
    // locale-aware 'n'
    let loc = Locale { decimal_point: ",".into(), thousands_sep: ".".into(), grouping: vec![3] };
    let s = FormatSpec::parse_int("n", "int").unwrap();
    let mut out = String::new();
    format_int(&mut out, false, "1234567", &s, &loc).unwrap();
    assert_eq!(out, "1.234.567");
    let loc_in = Locale { decimal_point: ".".into(), thousands_sep: ",".into(), grouping: vec![3, 2, 0] };
    let mut out = String::new();
    format_int(&mut out, true, "123456789", &s, &loc_in).unwrap();
    assert_eq!(out, "-12,34,56,789");
    let loc_stop = Locale { decimal_point: ".".into(), thousands_sep: ",".into(), grouping: vec![3, 127] };
    let mut out = String::new();
    format_int(&mut out, false, "123456789", &s, &loc_stop).unwrap();
    assert_eq!(out, "123456,789");
    let sf = FormatSpec::parse_float("n", "float").unwrap();
    let mut out = String::new();
    format_float_spec(&mut out, 1234.5, &sf, &loc, "float").unwrap();
    assert_eq!(out, "1.234,5");
}

// ---------------------------------------------------------------------------
// str.__format__

#[test]
fn str_format() {
    let ok = |s: &str, spec: &str| fs(s, spec).unwrap();
    assert_eq!(ok("abc", ".3s"), "abc");
    assert_eq!(ok("ab", ".3s"), "ab");
    assert_eq!(ok("abcdef", ".3s"), "abc");
    assert_eq!(ok("abcdef", ".0s"), "");
    assert_eq!(ok("abc", "3.3s"), "abc");
    assert_eq!(ok("abc", "2.3s"), "abc");
    assert_eq!(ok("abc", "2.2s"), "ab");
    assert_eq!(ok("abc", "3.2s"), "ab ");
    assert_eq!(ok("result", "x<0s"), "result");
    assert_eq!(ok("result", "x<5s"), "result");
    assert_eq!(ok("result", "x<6s"), "result");
    assert_eq!(ok("result", "x<7s"), "resultx");
    assert_eq!(ok("result", "x<8s"), "resultxx");
    assert_eq!(ok("result", " <7s"), "result ");
    assert_eq!(ok("result", "<7s"), "result ");
    assert_eq!(ok("result", ">7s"), " result");
    assert_eq!(ok("result", ">8s"), "  result");
    assert_eq!(ok("result", "^8s"), " result ");
    assert_eq!(ok("result", "^9s"), " result  ");
    assert_eq!(ok("result", "^10s"), "  result  ");
    assert_eq!(ok("result", "8s"), "result  ");
    assert_eq!(ok("result", "0s"), "result");
    assert_eq!(ok("result", "08s"), "result00");
    assert_eq!(ok("result", "<08s"), "result00");
    assert_eq!(ok("result", ">08s"), "00result");
    assert_eq!(ok("result", "^08s"), "0result0");
    assert_eq!(ok("a", "10000"), format!("a{}", " ".repeat(9999)));
    assert_eq!(ok("", "10000"), " ".repeat(10000));
    assert_eq!(ok("foo", "\0<6s"), "foo\0\0\0");
    assert_eq!(ok("foo", "\u{1}<6s"), "foo\u{1}\u{1}\u{1}");
    assert_eq!(ok("foo", "\0^6s"), "\0foo\0\0");
    assert_eq!(ok("foo", "^6s"), " foo  ");
    assert_eq!(ok("Hello", "15"), "Hello          ");
    assert_eq!(ok("hello world", ".5"), "hello");
    assert_eq!(ok("hello world", "10.5s"), "hello     ");
    assert_eq!(ok("ABC\u{0410}\u{0411}\u{0412}", ".3s"), "ABC");
    assert_eq!(ok("ABC\u{0410}\u{0411}\u{0412}", ".0s"), "");
    assert_eq!(ok("abc", "\u{2007}<5"), "abc\u{2007}\u{2007}");
    assert_eq!(ok("abc", "\u{2007}>5"), "\u{2007}\u{2007}abc");
    assert_eq!(ok("abc", "\u{2007}^5"), "\u{2007}abc\u{2007}");
    let e = |spec: &str| fs("", spec).unwrap_err().msg;
    assert_eq!(e("-s"), "Sign not allowed in string format specifier");
    assert_eq!(e("-"), "Sign not allowed in string format specifier");
    assert_eq!(e(" "), "Space not allowed in string format specifier");
    assert_eq!(e("=s"), "'=' alignment not allowed in string format specifier");
    assert_eq!(e("#"), "Alternate form (#) not allowed in string format specifier");
    assert_eq!(e("#20"), "Alternate form (#) not allowed in string format specifier");
    assert_eq!(e("d"), "Unknown format code 'd' for object of type 'str'");
    assert_eq!(e("%ЫйЯЧ"), "Invalid format specifier '%ЫйЯЧ' for object of type 'str'");
    let sp = FormatSpec::parse_str("^7", "str").unwrap();
    let l = str_layout(3, &sp, "str").unwrap();
    assert_eq!((l.take, l.lpad, l.rpad), (3, 2, 2));
}

// ---------------------------------------------------------------------------
// complex

#[test]
fn complex_repr_and_format() {
    assert_eq!(complex_repr(1.0, 6.0), "(1+6j)");
    assert_eq!(complex_repr(1.0, -6.0), "(1-6j)");
    assert_ne!(complex_repr(-1.0, -0.0), "(-1+-0j)");
    assert_eq!(complex_repr(1.0, INF), "(1+infj)");
    assert_eq!(complex_repr(1.0, -INF), "(1-infj)");
    assert_eq!(complex_repr(INF, 1.0), "(inf+1j)");
    assert_eq!(complex_repr(-INF, INF), "(-inf+infj)");
    assert_eq!(complex_repr(NAN, 1.0), "(nan+1j)");
    assert_eq!(complex_repr(1.0, NAN), "(1+nanj)");
    assert_eq!(complex_repr(NAN, NAN), "(nan+nanj)");
    assert_eq!(complex_repr(-NAN, -NAN), "(nan+nanj)");
    assert_eq!(complex_repr(0.0, INF), "infj");
    assert_eq!(complex_repr(0.0, -INF), "-infj");
    assert_eq!(complex_repr(0.0, NAN), "nanj");
    assert_eq!(complex_repr(0.0, 1.0), "1j");
    assert_eq!(complex_repr(-0.0, 1.0), "(-0+1j)");
    assert_eq!(complex_repr(0.0, -1.0), "-1j");
    assert_eq!(complex_repr(-0.0, -1.0), "(-0-1j)");
    assert_eq!(complex_repr(0.0, 0.0), "0j");
    assert_eq!(complex_repr(0.0, -0.0), "-0j");
    assert_eq!(complex_repr(-0.0, 0.0), "(-0+0j)");
    assert_eq!(complex_repr(-0.0, -0.0), "(-0-0j)");
    assert_eq!(complex_repr(1e16, 1.5), "(1e+16+1.5j)");

    for (re, im) in [(1.0, 3.0), (1.5, 3.5), (0.0, 3.0), (0.0, 3.2), (3.0, 0.0), (3.2, 0.0)] {
        assert_eq!(fc(re, im, ""), complex_repr(re, im));
    }
    assert_eq!(fc(3.2, 0.0, "-"), complex_repr(3.2, 0.0));
    assert_eq!(fc(3.2, 0.0, "<"), complex_repr(3.2, 0.0));
    let (re, im) = (4.0 / 7.0, -100.0 / 7.0);
    for s in ["", "-", "<", "10"] {
        assert_eq!(fc(re, im, s), complex_repr(re, im));
    }
    for s in ["", "-", "<", "2"] {
        assert_eq!(fc(0.0, 3.0, s), complex_repr(0.0, 3.0));
    }
    for s in ["", "-", "<", "3"] {
        assert_eq!(fc(-0.0, 2.0, s), complex_repr(-0.0, 2.0));
    }
    assert_eq!(fc(1.0, 3.0, "g"), "1+3j");
    assert_eq!(fc(0.0, 3.0, "g"), "0+3j");
    assert_eq!(fc(1.5, 3.5, "g"), "1.5+3.5j");
    assert_eq!(fc(1.5, 3.5, "+g"), "+1.5+3.5j");
    assert_eq!(fc(1.5, -3.5, "+g"), "+1.5-3.5j");
    assert_eq!(fc(1.5, -3.5, "-g"), "1.5-3.5j");
    assert_eq!(fc(1.5, 3.5, " g"), " 1.5+3.5j");
    assert_eq!(fc(1.5, -3.5, " g"), " 1.5-3.5j");
    assert_eq!(fc(-1.5, 3.5, " g"), "-1.5+3.5j");
    assert_eq!(fc(-1.5, -3.5, " g"), "-1.5-3.5j");
    assert_eq!(fc(-1.5, -3.5e-20, "g"), "-1.5-3.5e-20j");
    assert_eq!(fc(-1.5, -3.5, "f"), "-1.500000-3.500000j");
    assert_eq!(fc(-1.5, -3.5, "F"), "-1.500000-3.500000j");
    assert_eq!(fc(-1.5, -3.5, "e"), "-1.500000e+00-3.500000e+00j");
    assert_eq!(fc(-1.5, -3.5, ".2e"), "-1.50e+00-3.50e+00j");
    assert_eq!(fc(-1.5, -3.5, ".2E"), "-1.50E+00-3.50E+00j");
    assert_eq!(fc(-1.5e10, -3.5e5, ".2G"), "-1.5E+10-3.5E+05j");
    assert_eq!(fc(1.5, 3.0, "<20g"), "1.5+3j              ");
    assert_eq!(fc(1.5, 3.0, "*<20g"), "1.5+3j**************");
    assert_eq!(fc(1.5, 3.0, ">20g"), "              1.5+3j");
    assert_eq!(fc(1.5, 3.0, "^20g"), "       1.5+3j       ");
    assert_eq!(fc(1.5, 3.0, "<20"), "(1.5+3j)            ");
    assert_eq!(fc(1.5, 3.0, ">20"), "            (1.5+3j)");
    assert_eq!(fc(1.5, 3.0, "^20"), "      (1.5+3j)      ");
    assert_eq!(fc(1.123, -3.123, "^20.2"), "     (1.1-3.1j)     ");
    assert_eq!(fc(1.5, 3.0, "20.2f"), "          1.50+3.00j");
    assert_eq!(fc(1.5, 3.0, ">20.2f"), "          1.50+3.00j");
    assert_eq!(fc(1.5, 3.0, "<20.2f"), "1.50+3.00j          ");
    assert_eq!(fc(1.5e20, 3.0, "<20.2f"), "150000000000000000000.00+3.00j");
    assert_eq!(fc(1.5e20, 3.0, ">40.2f"), "          150000000000000000000.00+3.00j");
    assert_eq!(fc(1.5e20, 3.0, "^40,.2f"), "  150,000,000,000,000,000,000.00+3.00j  ");
    assert_eq!(fc(1.5e21, 3.0, "^40,.2f"), " 1,500,000,000,000,000,000,000.00+3.00j ");
    assert_eq!(fc(1.5e21, 3000.0, ",.2f"), "1,500,000,000,000,000,000,000.00+3,000.00j");
    assert_eq!(fc(1.0, 1.0, ".0e"), "1e+00+1e+00j");
    assert_eq!(fc(1.0, 1.0, "#.0e"), "1.e+00+1.e+00j");
    assert_eq!(fc(1.0, 1.0, ".0f"), "1+1j");
    assert_eq!(fc(1.0, 1.0, "#.0f"), "1.+1.j");
    assert_eq!(fc(1.1, 1.1, "g"), "1.1+1.1j");
    assert_eq!(fc(1.1, 1.1, "#g"), "1.10000+1.10000j");
    assert_eq!(fc(1.0, 1.0, ".1e"), "1.0e+00+1.0e+00j");
    assert_eq!(fc(1.0, 1.0, "#.1e"), "1.0e+00+1.0e+00j");
    assert_eq!(fc(1.0, 1.0, ".1f"), "1.0+1.0j");
    assert_eq!(fc(1.0, 1.0, "#.1f"), "1.0+1.0j");
    assert_eq!(fc(-1.5, 0.5, "#f"), "-1.500000+0.500000j");
    assert_eq!(fc(-1.5, 0.5, "#.0f"), "-2.+0.j");
    assert_eq!(fc(-1.5, 0.5, "#e"), "-1.500000e+00+5.000000e-01j");
    assert_eq!(fc(-1.5, 0.5, "#.0e"), "-2.e+00+5.e-01j");
    assert_eq!(fc(-1.5, 0.5, "#g"), "-1.50000+0.500000j");
    assert_eq!(fc(-1.5, 0.5, ".0g"), "-2+0.5j");
    assert_eq!(fc(-1.5, 0.5, "#.0g"), "-2.+0.5j");
    assert!(format_complex(1.5, 0.5, "010f", "complex").is_err());
    assert!(format_complex(1.5, 3.0, "=20", "complex").is_err());
    for t in ["b", "c", "d", "o", "x", "X", "%"] {
        assert!(format_complex(1.5, 0.5, t, "complex").is_err());
    }
    assert_eq!(fc(3.14159, 2.71828, ".3f"), "3.142+2.718j");
    assert_eq!(fc(NAN, NAN, "f"), "nan+nanj");
    assert_eq!(fc(1.0, NAN, "f"), "1.000000+nanj");
    assert_eq!(fc(NAN, 1.0, "f"), "nan+1.000000j");
    assert_eq!(fc(NAN, -1.0, "f"), "nan-1.000000j");
    assert_eq!(fc(NAN, NAN, "F"), "NAN+NANj");
    assert_eq!(fc(1.0, NAN, "F"), "1.000000+NANj");
    assert_eq!(fc(NAN, 1.0, "F"), "NAN+1.000000j");
    assert_eq!(fc(NAN, -1.0, "F"), "NAN-1.000000j");
    assert_eq!(fc(INF, INF, "f"), "inf+infj");
    assert_eq!(fc(1.0, INF, "f"), "1.000000+infj");
    assert_eq!(fc(INF, 1.0, "f"), "inf+1.000000j");
    assert_eq!(fc(INF, -1.0, "f"), "inf-1.000000j");
    assert_eq!(fc(INF, INF, "F"), "INF+INFj");
    assert_eq!(fc(0.0, 0.0, "\u{2007}<4"), "0j\u{2007}\u{2007}");
    assert_eq!(fc(1.0, 2.0, "\u{2007}<8"), "(1+2j)\u{2007}\u{2007}");
    assert_eq!(fc(1.0, 2.0, "\u{2007}>8"), "\u{2007}\u{2007}(1+2j)");
    assert_eq!(fc(0.0, 0.0, "\u{2007}>4"), "\u{2007}\u{2007}0j");
    assert_eq!(fc(1.0, 2.0, "\u{2007}^8"), "\u{2007}(1+2j)\u{2007}");
    assert_eq!(fc(0.0, 0.0, "\u{2007}^4"), "\u{2007}0j\u{2007}");
    assert_eq!(fc(3.0, 2.0, "\0<12"), "(3+2j)\0\0\0\0\0\0");
    assert_eq!(fc(3.0, 2.0, "\0^12"), "\0\0\0(3+2j)\0\0\0");
    assert_eq!(fc(3.0, 2.0, "^12"), "   (3+2j)   ");
}

#[test]
fn complex_parse() {
    let c = |s: &str| parse_complex(s).unwrap_or_else(|e| panic!("{:?}: {:?}", s, e));
    let chk = |s: &str, x: f64, y: f64| {
        let (a, b) = c(s);
        assert!(ident(a, x) && ident(b, y), "{:?} -> ({}, {})", s, a, b);
    };
    chk("1", 1.0, 0.0);
    chk("1j", 0.0, 1.0);
    chk("-1", -1.0, 0.0);
    chk("+1", 1.0, 0.0);
    chk("1+2j", 1.0, 2.0);
    chk("(1+2j)", 1.0, 2.0);
    chk("(1.5+4.25j)", 1.5, 4.25);
    chk("4.25+1J", 4.25, 1.0);
    chk(" ( +4.25-6J )", 4.25, -6.0);
    chk(" ( +4.25-J )", 4.25, -1.0);
    chk(" ( +4.25+j )", 4.25, 1.0);
    chk("J", 0.0, 1.0);
    chk("( j )", 0.0, 1.0);
    chk("+J", 0.0, 1.0);
    chk("( -j)", 0.0, -1.0);
    chk("1-1j", 1.0, -1.0);
    chk("1J", 0.0, 1.0);
    chk("1e-500", 0.0, 0.0);
    chk("-1e-500j", 0.0, -0.0);
    chk("1e-500+1e-500j", 0.0, 0.0);
    chk("-1e-500+1e-500j", -0.0, 0.0);
    chk("1e-500-1e-500j", 0.0, -0.0);
    chk("-1e-500-1e-500j", -0.0, -0.0);
    chk("1e500", INF, 0.0);
    chk("-1e500j", 0.0, -INF);
    chk("-1e500+1.8e308j", -INF, INF);
    chk("\u{2003}(\u{2002}1+1j ) ", 1.0, 1.0);
    for bad in [
        "1+1j\0j", "", "\0", "3\09", "1+", "1+1j+1j", "--", "(1+2j", "1+2j)", "1+(2j)",
        "(1+2j)123", "x", "1j+2", "1e1ej", "1e++1ej", ")1+2j(", "1..1j", "1.11.1j", "1e1.1j",
        "\u{3053}\u{3093}\u{306b}\u{3061}\u{306f}",
    ] {
        let e = parse_complex(bad).unwrap_err();
        assert_eq!(e.msg, "complex() arg is a malformed string", "{:?}", bad);
    }
    assert_eq!(c(&"1".repeat(500)).0, INF);
    assert!(c("-nan").0.is_sign_negative() && c("-nan").0.is_nan());
    assert!(c("-nanj").1.is_sign_negative());
    let (a, b) = c("-nan-nanj");
    assert!(a.is_sign_negative() && b.is_sign_negative());
    // repr round trip
    let vals = [0.0, 1e-500f64.max(0.0), 1e-315, 1e-200, 0.0123, 3.1415, 1e50, INF, NAN];
    let mut all = vals.to_vec();
    all.extend(vals.iter().map(|v| -v));
    for &x in &all {
        for &y in &all {
            let (a, b) = c(&complex_repr(x, y));
            assert!(ident(a, x) && ident(b, y), "{}", complex_repr(x, y));
        }
    }
    // underscores
    for lit in ["1_00_00j", "1_00_00.5j", "1_00_00e5_1j", ".1_4j", "(1_2.5+3_3j)", "(.5_6j)", "1_00_00.5e5", "1e1_0"] {
        assert!(parse_complex(lit).is_ok(), "{}", lit);
    }
    for lit in ["1.4j_", "1._4j", "._5j", "1.0e+_1j", "1.4_j", "1.4e5_j", "1.4_e1j", "1.4e_1j", "(1+1.5_j_)", "(1+1.5_j)", "0.1__4j", "1e1__0j", "1_.4j"] {
        assert!(parse_complex(lit).is_err(), "{}", lit);
    }
}

// ---------------------------------------------------------------------------
// float()

#[test]
fn float_parse() {
    let p = |s: &str| parse_float(s).unwrap_or_else(|e| panic!("{:?}: {:?}", s, e));
    assert_eq!(p("  3.14  "), 3.14);
    for bad in [
        "  0x3.1  ", "  -0x3.p-1  ", "  +0x3.p-1  ", "++3.14", "+-3.14", "-+3.14", "--3.14",
        ".nan", "+.inf", ".", "-.", "-1.7d29", "3D-14", "\u{3053}\u{3093}\u{306b}\u{3061}\u{306f}",
        "\u{bd}", "123\u{bd}", "  123 456  ", "", " ", "\t \n", "\u{663}\u{661}\u{664}!", "123\0",
        "123\0 245", "123\0245", "_NaN", "Na_N", "IN_F", "-_INF", "-INF_", "info", "+info",
        "-info", "in", "+in", "-in", "infinit", "+Infin", "-INFI", "infinitys", "++Inf", "-+inf",
        "+-infinity", "--Infinity", "nana", "+nana", "-nana", "na", "+na", "-na", "++nan",
        "-+NAN", "+-NaN", "--nAn", "  3,14  ",
    ] {
        let e = parse_float(bad).unwrap_err();
        assert_eq!(e.kind, ErrorKind::ValueError);
        assert_eq!(e.msg, "could not convert string to float", "{:?}", bad);
    }
    assert!(parse_float_bytes(b"0_.\xff9").is_err());
    assert!(parse_float_bytes(b"123\xa0").is_err());
    assert_eq!(p("  \u{663}.\u{661}\u{664}  "), 3.14);
    assert_eq!(p("\u{2003}3.14\u{2002}"), 3.14);
    p(&format!(".{}", "1".repeat(1000)));
    assert_eq!(p("+3.14  "), 3.14);
    assert_eq!(p("-3.14  "), -3.14);
    assert_eq!(p(".14  "), 0.14);
    assert_eq!(p("3.  "), 3.0);
    assert_eq!(p("3.e3  "), 3000.0);
    assert_eq!(p("3.2e3  "), 3200.0);
    assert_eq!(p("2.5e-1  "), 0.25);
    assert_eq!(p("5e-1"), 0.5);
    assert_eq!(p("  25.e-1  "), 2.5);
    for s in ["inf", "+inf", "infinity", "+infinity", "INF", "+Inf", "Infinity", "+iNfInItY"] {
        assert_eq!(p(s), INF);
    }
    for s in ["-inf", "-infinity", "-iNF", "-INFINITY"] {
        assert_eq!(p(s), -INF);
    }
    for s in ["nan", "+nan", "NAN", "+NAn"] {
        assert!(p(s).is_nan() && p(s).is_sign_positive());
    }
    for s in ["-nan", "-NaN"] {
        assert!(p(s).is_nan() && p(s).is_sign_negative());
    }
    // underscores (test.support.numbers)
    let valid = ["0_0_0", "4_2", "1_0000_0000", "1_00_00.5", "1_00_00.5e5", "1_00_00e5_1", "1e1_0", ".1_4", ".1_4e1"];
    for lit in valid {
        assert_eq!(p(lit), p(&lit.replace('_', "")));
        assert_eq!(parse_float_literal(lit).unwrap(), p(&lit.replace('_', "")), "{}", lit);
    }
    let invalid = [
        "0_", "42_", "1.4j_", "0x_", "0b1_", "0xf_", "0o5_", "0 if 1_Else 1", "0_b0", "0_xf",
        "0_o5", "4_______2", "0.1__4", "0.1__4j", "0b1001__0100", "0xffff__ffff", "0x___",
        "0o5__77", "1e1__0", "1e1__0j", "1_.4", "1_.4j", "1._4", "1._4j", "._5", "._5j",
        "1.0e+_1", "1.0e+_1j", "1.4_j", "1.4e5_j", "1_e1", "1.4_e1", "1.4_e1j", "1e_1",
        "1.4e_1", "1.4e_1j", "(1+1.5_j_)", "(1+1.5_j)",
    ];
    for lit in invalid {
        assert!(parse_float(lit).is_err(), "{}", lit);
        assert!(parse_float_literal(lit).is_err(), "{}", lit);
    }
    assert_eq!(parse_float_literal("0777").unwrap(), 777.0);
    assert_eq!(parse_float_literal("1.").unwrap(), 1.0);
    assert!(parse_float_literal("inf").is_err());
    assert!(parse_float_literal("-1.0").is_err());
    assert!(parse_float_literal(" 1.0").is_err());
    assert!(parse_float_literal("1e").is_err());
    // test_strtod: large exponents and particular cases
    for n in [10000, 20000, 30000] {
        assert_eq!(p(&format!("0.{}1e+{}", "0".repeat(n - 1), n)), 1.0);
        assert_eq!(p(&format!("1{}e-{}", "0".repeat(n), n)), 1.0);
    }
    assert_eq!(float_hex(p("1.00000000000000001e44")), float_hex(1e44));
    assert_eq!(p("1e500"), INF);
    assert_eq!(p("-1e500"), -INF);
    assert!(ident(p("-1e-500"), -0.0));
    let max_boundary = "179769313486231580793728971405303415079934132710037826936173778980444968292764750946649017977587207096330286416692887910946555547851940402630657488671505820681908902000708383676273854845817711531764475730270069855571366959622842914819860834936475292719074168444365510704342711559699508093042880177904174497792";
    assert_eq!(p(max_boundary), INF);
    assert_eq!(p(&format!("{}.001", &max_boundary)), INF);
    assert_eq!(p("179769313486231580793728971405303415079934132710037826936173778980444968292764750946649017977587207096330286416692887910946555547851940402630657488671505820681908902000708383676273854845817711531764475730270069855571366959622842914819860834936475292719074168444365510704342711559699508093042880177904174497791.999"), f64::MAX);
}

// ---------------------------------------------------------------------------
// float.hex / fromhex

#[test]
fn hex_float() {
    let fh = |s: &str| float_fromhex(s).unwrap_or_else(|e| panic!("{:?}: {:?}", s, e));
    let max = fh("0x.fffffffffffff8p+1024");
    let min = fh("0x1p-1022");
    let tiny = fh("0x0.0000000000001p-1022");
    let eps = fh("0x0.0000000000001p0");
    assert_eq!(max, f64::MAX);
    assert_eq!(min, f64::MIN_POSITIVE);
    assert_eq!(tiny, 5e-324);
    assert_eq!(eps, f64::EPSILON);
    for bad in [
        "infi", "-Infinit", "++inf", "-+Inf", "--nan", "+-NaN", "snan", "NaNs", "nna", "an", "nf",
        "nfinity", "inity", "iinity", "0xnan", "", " ", "x1.0p0", "0xX1.0p0", "+ 0x1.0p0",
        "- 0x1.0p0", "0 x1.0p0", "0x 1.0p0", "0x1 2.0p0", "+0x1 .0p0", "0x1. 0p0", "-0x1.0 1p0",
        "-0x1.0 p0", "+0x1.0p +0", "0x1.0p -0", "0x1.0p 0", "+0x1.0p+ 0", "-0x1.0p- 0",
        "++0x1.0p-0", "--0x1.0p0", "+-0x1.0p+0", "-+0x1.0p0", "0x1.0p++0", "+0x1.0p+-0",
        "-0x1.0p-+0", "0x1.0p--0", "0x1.0.p0", "0x.p0", "0x1,p0", "0x1pa", "0x1p\u{ff10}",
        "\u{ff10}x1p0", "0x\u{ff11}p0", "0x1.\u{ff10}p0", "0x1p0 \n 0x2p0", "0x1p0\0 0x1p0",
    ] {
        let e = float_fromhex(bad).unwrap_err();
        assert_eq!(e.kind, ErrorKind::ValueError, "{:?}", bad);
        assert_eq!(e.msg, "invalid hexadecimal floating-point string");
    }
    for (inp, exp) in [("inf", INF), ("-Infinity", -INF), ("nan", NAN), ("1.0", 1.0), ("-0x.2", -0.125), ("-0.0", -0.0)] {
        for lead in ["", " ", "\t", "\n", "\n \t", "\x0c", "\x0b", "\r"] {
            for trail in ["", " ", "\t", "\n", "\n \t", "\x0c", "\x0b", "\r"] {
                assert!(ident(fh(&format!("{}{}{}", lead, inp, trail)), exp));
            }
        }
    }
    let cases: &[(&str, f64)] = &[
        ("inf", INF), ("+Inf", INF), ("-INF", -INF), ("iNf", INF), ("Infinity", INF),
        ("+INFINITY", INF), ("-infinity", -INF), ("-iNFiNitY", -INF),
        ("1", 1.0), ("+1", 1.0), ("1.", 1.0), ("1.0", 1.0), ("1.0p0", 1.0), ("01", 1.0),
        ("01.", 1.0), ("0x1", 1.0), ("0x1.", 1.0), ("0x1.0", 1.0), ("+0x1.0", 1.0),
        ("0x1p0", 1.0), ("0X1p0", 1.0), ("0X1P0", 1.0), ("0x1P0", 1.0), ("0x1.p0", 1.0),
        ("0x1.0p0", 1.0), ("0x.1p4", 1.0), ("0x.1p04", 1.0), ("0x.1p004", 1.0), ("0x1p+0", 1.0),
        ("0x1P-0", 1.0), ("+0x1p0", 1.0), ("0x01p0", 1.0), ("0x1p00", 1.0), (" 0x1p0 ", 1.0),
        ("\n 0x1p0", 1.0), ("0x1p0 \t", 1.0), ("0xap0", 10.0), ("0xAp0", 10.0), ("0xaP0", 10.0),
        ("0xAP0", 10.0), ("0xbep0", 190.0), ("0xBep0", 190.0), ("0xbEp0", 190.0),
        ("0XBE0P-4", 190.0), ("0xBEp0", 190.0), ("0xB.Ep4", 190.0), ("0x.BEp8", 190.0),
        ("0x.0BEp12", 190.0),
        ("0x.8p-1074", 0.0), ("0x.80p-1074", 0.0), ("0x.81p-1074", 5e-324), ("0x8p-1078", 0.0),
        ("0x8.0p-1078", 0.0), ("0x8.1p-1078", 5e-324), ("0x80p-1082", 0.0), ("0x81p-1082", 5e-324),
        (".8p-1074", 0.0), ("8p-1078", 0.0), ("-.8p-1074", -0.0), ("+8p-1078", 0.0),
        ("+0x1.fffffffffffffp+1023", f64::MAX), ("-0X1.fffffffffffff7p1023", -f64::MAX),
        ("0X1.fffffffffffff7fffffffffffffp1023", f64::MAX),
        ("0x0p0", 0.0), ("0x0p1000", 0.0), ("-0x0p1023", -0.0), ("0X0p1024", 0.0),
        ("-0x0p1025", -0.0), ("0X0p2000", 0.0), ("0x0p123456789123456789", 0.0), ("-0X0p-0", -0.0),
        ("-0X0p-1000", -0.0), ("0x0p-1023", 0.0), ("-0X0p-1024", -0.0), ("-0x0p-1025", -0.0),
        ("-0x0p-1072", -0.0), ("0X0p-1073", 0.0), ("-0x0p-1074", -0.0), ("0x0p-1075", 0.0),
        ("0X0p-1076", 0.0), ("-0X0p-2000", -0.0), ("-0x0p-123456789123456789", -0.0),
        ("0X1p-1075", 0.0), ("-0X1p-1075", -0.0), ("-0x1p-123456789123456789", -0.0),
        ("0x1.00000000000000001p-1075", 5e-324), ("-0x1.1p-1075", -5e-324),
        ("0x1.fffffffffffffffffp-1075", 5e-324),
    ];
    for &(s, v) in cases {
        assert!(ident(fh(s), v), "{} -> {:e}", s, fh(s));
    }
    assert!(fh("nan").is_nan() && fh("-NaN").is_nan() && fh("+NaN").is_nan());
    // round-half-even near 0, MIN and 1.0
    for (k, m) in [(1, 0), (2, 0), (3, 1), (4, 1), (5, 1), (6, 2), (7, 2), (8, 2), (9, 2), (10, 2), (11, 3), (12, 3), (13, 3), (14, 4), (15, 4), (16, 4)] {
        assert!(ident(fh(&format!("0x{:x}p-1076", k)), m as f64 * tiny));
        assert!(ident(fh(&format!("-0x{:X}p-1076", k)), -(m as f64) * tiny));
    }
    let near_min: &[(&str, f64)] = &[
        ("0x0.ffffffffffffd6p-1022", min - 3.0 * tiny), ("0x0.ffffffffffffd8p-1022", min - 2.0 * tiny),
        ("0x0.ffffffffffffdap-1022", min - 2.0 * tiny), ("0x0.ffffffffffffe8p-1022", min - 2.0 * tiny),
        ("0x0.ffffffffffffeap-1022", min - tiny), ("0x0.fffffffffffff6p-1022", min - tiny),
        ("0x0.fffffffffffff8p-1022", min), ("0x0.fffffffffffffep-1022", min),
        ("0x1.00000000000008p-1022", min), ("0x1.0000000000000ap-1022", min + tiny),
        ("0x1.00000000000016p-1022", min + tiny), ("0x1.00000000000018p-1022", min + 2.0 * tiny),
    ];
    for &(s, v) in near_min {
        assert!(ident(fh(s), v), "{}", s);
    }
    let near_one: &[(&str, f64)] = &[
        ("0x0.fffffffffffff0p0", 1.0 - eps), ("0x0.fffffffffffff4p0", 1.0 - eps),
        ("0X0.fffffffffffff5p0", 1.0 - eps / 2.0), ("0x0.fffffffffffff8p0", 1.0 - eps / 2.0),
        ("0x0.fffffffffffffbp0", 1.0 - eps / 2.0), ("0X0.fffffffffffffcp0", 1.0),
        ("0x0.ffffffffffffffp0", 1.0), ("0X1.00000000000007p0", 1.0),
        ("0x1.00000000000007ffffffffffffffffffffp0", 1.0), ("0x1.00000000000008p0", 1.0),
        ("0x1.00000000000008000000000000000001p0", 1.0 + eps), ("0X1.00000000000009p0", 1.0 + eps),
        ("0x1.00000000000017ffffffffffffffffffffp0", 1.0 + eps), ("0x1.00000000000018p0", 1.0 + 2.0 * eps),
        ("0X1.00000000000018000000000000000001p0", 1.0 + 2.0 * eps), ("0X1.0000000000001bp0", 1.0 + 2.0 * eps),
        ("0x1.00000000000020p0", 1.0 + 2.0 * eps),
    ];
    for &(s, v) in near_one {
        assert!(ident(fh(s), v), "{}", s);
    }
    let pi = fh("0x1.921fb54442d18p1");
    for s in [
        "0x.006487ed5110b46p11", "0x.00c90fdaa22168cp10", "0x.01921fb54442d18p9", "0x.03243f6a8885a3p8",
        "0x3.243f6a8885a3p0", "0x6.487ed5110b46p-1", "0x1921.fb54442d18p-11", "0x1921fb54442d1.8p-47",
        "0x3243f6a8885a3p-48", "0x1921fb54442d180p-55",
    ] {
        assert_eq!(fh(s), pi, "{}", s);
    }
    for s in [
        "-0x1p1024", "0x1p+1025", "+0X1p1030", "-0x1p+1100", "0X1p123456789123456789", "+0X.8p+1025",
        "+0x0.8p1025", "-0x0.4p1026", "0X2p+1023", "0x2.p1023", "-0x2.0p+1023", "+0X4p+1022",
        "0x1.ffffffffffffffp+1023", "-0X1.fffffffffffff9p1023", "0X1.fffffffffffff8p1023",
        "+0x3.fffffffffffffp1022", "0x3fffffffffffffp+970", "0x10000000000000000p960",
        "-0Xffffffffffffffffp960",
    ] {
        let e = float_fromhex(s).unwrap_err();
        assert_eq!(e.kind, ErrorKind::OverflowError, "{}", s);
        assert_eq!(e.msg, "hexadecimal value too large to represent as a float");
    }
    // hex output and round trip
    assert_eq!(float_hex(1.0), "0x1.0000000000000p+0");
    assert_eq!(float_hex(-0.0), "-0x0.0p+0");
    assert_eq!(float_hex(0.0), "0x0.0p+0");
    assert_eq!(float_hex(5e-324), "0x0.0000000000001p-1022");
    assert_eq!(float_hex(3.14159), "0x1.921f9f01b866ep+1");
    assert_eq!(float_hex(INF), "inf");
    assert_eq!(float_hex(-INF), "-inf");
    assert_eq!(float_hex(NAN), "nan");
    for x in [NAN, INF, max, min, min - tiny, tiny, 0.0] {
        assert!(ident(fh(&float_hex(x)), x));
        assert!(ident(fh(&float_hex(-x)), -x));
    }
    let mut state = 0x9E3779B97F4A7C15u64;
    for _ in 0..10000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let x = f64::from_bits(state);
        if x.is_nan() {
            continue;
        }
        assert!(ident(fh(&float_hex(x)), x));
    }
}

// ---------------------------------------------------------------------------
// round / as_integer_ratio

#[test]
fn rounding() {
    assert_eq!(float_round_to_int(INF).unwrap_err().kind, ErrorKind::OverflowError);
    assert_eq!(float_round_to_int(-INF).unwrap_err().kind, ErrorKind::OverflowError);
    assert_eq!(float_round_to_int(NAN).unwrap_err().kind, ErrorKind::ValueError);
    assert_eq!(float_round(INF, 0).unwrap(), INF);
    assert_eq!(float_round(-INF, 0).unwrap(), -INF);
    assert!(float_round(NAN, 0).unwrap().is_nan());
    for n in [324i64, 325, 400, (1 << 31) - 1, 1 << 31, 1 << 32, i64::MAX] {
        assert_eq!(float_round(123.456, n).unwrap(), 123.456);
        assert_eq!(float_round(-123.456, n).unwrap(), -123.456);
        assert_eq!(float_round(1e300, n).unwrap(), 1e300);
        assert_eq!(float_round(1e-320, n).unwrap(), 1e-320);
    }
    assert_eq!(float_round(1e150, 300).unwrap(), 1e150);
    assert_eq!(float_round(1e300, 307).unwrap(), 1e300);
    assert_eq!(float_round(-3.1415, 308).unwrap(), -3.1415);
    assert_eq!(float_round(1e150, 309).unwrap(), 1e150);
    assert_eq!(float_round(1.4e-315, 315).unwrap(), 1e-315);
    for n in [-308i64, -309, -400, 1 - (1 << 31), -(1 << 31), -(1 << 31) - 1, i64::MIN] {
        assert!(ident(float_round(123.456, n).unwrap(), 0.0));
        assert!(ident(float_round(-123.456, n).unwrap(), -0.0));
        assert!(ident(float_round(1e300, n).unwrap(), 0.0));
        assert!(ident(float_round(1e-320, n).unwrap(), 0.0));
    }
    assert_eq!(float_round(1.6e308, -308).unwrap_err().kind, ErrorKind::OverflowError);
    assert_eq!(float_round(-1.7e308, -308).unwrap_err().msg, "rounded value too large to represent");
    assert_eq!(float_round(562949953421312.5, 1).unwrap(), 562949953421312.5);
    assert_eq!(float_round(56294995342131.5, 3).unwrap(), 56294995342131.5);
    for (x, r) in [(25.0, 20.0), (35.0, 40.0), (45.0, 40.0), (55.0, 60.0), (65.0, 60.0), (75.0, 80.0), (85.0, 80.0), (95.0, 100.0)] {
        assert_eq!(float_round(x, -1).unwrap(), r);
    }
    assert_eq!(float_round(-8.0, -1).unwrap(), -10.0);
    // round matches float formatting
    for i in 0..500 {
        let x = i as f64 / 1000.0;
        for n in 0..4 {
            let f = parse_float(&ff(x, &format!(".{}f", n))).unwrap();
            assert_eq!(f, float_round(x, n as i64).unwrap(), "{} {}", x, n);
        }
    }
    for i in (5..5000).step_by(10) {
        let x = i as f64 / 1000.0;
        for n in 0..4 {
            let f = parse_float(&ff(x, &format!(".{}f", n))).unwrap();
            assert_eq!(f, float_round(x, n as i64).unwrap(), "{} {}", x, n);
        }
    }
    // builtin round()
    for (x, r) in [
        (0.0, 0.0), (1.0, 1.0), (10.0, 10.0), (1e9, 1e9), (1e20, 1e20), (-1.0, -1.0), (0.1, 0.0),
        (1.1, 1.0), (10.1, 10.0), (1000000000.1, 1e9), (-1.1, -1.0), (0.9, 1.0), (9.9, 10.0),
        (999999999.9, 1e9), (-0.9, -1.0), (-9.9, -10.0), (5.5, 6.0), (6.5, 6.0), (-5.5, -6.0),
        (-6.5, -6.0), (5e15 - 1.0, 5e15 - 1.0), (5e15, 5e15), (5e15 + 1.0, 5e15 + 1.0),
    ] {
        assert_eq!(float_round_to_int(x).unwrap(), r);
    }
}

#[test]
fn as_integer_ratio() {
    let ratio = |x: f64| -> (i128, u128) {
        let (neg, m, e) = float_as_integer_ratio(x).unwrap();
        let (n, d) = if e >= 0 { ((m as i128) << e, 1u128) } else { (m as i128, 1u128 << -e) };
        (if neg { -n } else { n }, d)
    };
    assert_eq!(ratio(0.875), (7, 8));
    assert_eq!(ratio(-0.875), (-7, 8));
    assert_eq!(ratio(0.0), (0, 1));
    assert_eq!(ratio(11.5), (23, 2));
    assert_eq!(ratio(2.5), (5, 2));
    assert_eq!(ratio(0.5), (1, 2));
    assert_eq!(ratio(2.1), (4728779608739021, 2251799813685248));
    assert_eq!(ratio(-2.1), (-4728779608739021, 2251799813685248));
    assert_eq!(ratio(-2100.0), (-2100, 1));
    assert_eq!(float_as_integer_ratio(INF).unwrap_err().kind, ErrorKind::OverflowError);
    assert_eq!(float_as_integer_ratio(-INF).unwrap_err().kind, ErrorKind::OverflowError);
    assert_eq!(float_as_integer_ratio(NAN).unwrap_err().kind, ErrorKind::ValueError);
}

// ---------------------------------------------------------------------------
// % formatting

#[test]
fn percent_common() {
    let big: i128 = 123456789012345678901234567890;
    for (f, v, r) in [
        ("%.1d", 1, "1"),
        ("%x", 10, "a"),
        ("%x", 100000000000, "174876e800"),
        ("%o", 10, "12"),
        ("%o", 100000000000, "1351035564000"),
        ("%d", 10, "10"),
        ("%d", 100000000000, "100000000000"),
        ("%d", big, "123456789012345678901234567890"),
        ("%d", -big, "-123456789012345678901234567890"),
        ("%5d", -big, "-123456789012345678901234567890"),
        ("%31d", -big, "-123456789012345678901234567890"),
        ("%32d", -big, " -123456789012345678901234567890"),
        ("%-32d", -big, "-123456789012345678901234567890 "),
        ("%032d", -big, "-0123456789012345678901234567890"),
        ("%-032d", -big, "-123456789012345678901234567890 "),
        ("%034d", -big, "-000123456789012345678901234567890"),
        ("%034d", big, "0000123456789012345678901234567890"),
        ("%0+34d", big, "+000123456789012345678901234567890"),
        ("%+34d", big, "   +123456789012345678901234567890"),
        ("%34d", big, "    123456789012345678901234567890"),
        ("%.2d", big, "123456789012345678901234567890"),
        ("%.30d", big, "123456789012345678901234567890"),
        ("%.31d", big, "0123456789012345678901234567890"),
        ("%32.31d", big, " 0123456789012345678901234567890"),
        ("%d", 42, "42"),
        ("%d", -42, "-42"),
        ("%#x", 1, "0x1"),
        ("%#X", 1, "0X1"),
        ("%#o", 1, "0o1"),
        ("%#o", 0, "0o0"),
        ("%o", 0, "0"),
        ("%d", 0, "0"),
        ("%#x", 0, "0x0"),
        ("%#X", 0, "0X0"),
        ("%x", 0x42, "42"),
        ("%x", -0x42, "-42"),
        ("%o", 0o42, "42"),
        ("%o", -0o42, "-42"),
        ("%3ld", 42, " 42"),
    ] {
        assert_eq!(pct_ok(f, &[Arg::I(v)]), r, "{}", f);
    }
    assert_eq!(pct_ok("%.100d", &[Arg::I(1)]), format!("{}1", "0".repeat(99)));
    assert_eq!(pct_ok("%#.117x", &[Arg::I(1)]), format!("0x{}1", "0".repeat(116)));
    assert_eq!(pct_ok("%#.118x", &[Arg::I(1)]), format!("0x{}1", "0".repeat(117)));
    let big: i128 = 0x1234567890abcdef12345;
    for (f, v, r) in [
        ("%x", big, "1234567890abcdef12345"),
        ("%x", -big, "-1234567890abcdef12345"),
        ("%5x", -big, "-1234567890abcdef12345"),
        ("%22x", -big, "-1234567890abcdef12345"),
        ("%23x", -big, " -1234567890abcdef12345"),
        ("%-23x", -big, "-1234567890abcdef12345 "),
        ("%023x", -big, "-01234567890abcdef12345"),
        ("%-023x", -big, "-1234567890abcdef12345 "),
        ("%025x", -big, "-0001234567890abcdef12345"),
        ("%025x", big, "00001234567890abcdef12345"),
        ("%0+25x", big, "+0001234567890abcdef12345"),
        ("%+25x", big, "   +1234567890abcdef12345"),
        ("%25x", big, "    1234567890abcdef12345"),
        ("%.2x", big, "1234567890abcdef12345"),
        ("%.21x", big, "1234567890abcdef12345"),
        ("%.22x", big, "01234567890abcdef12345"),
        ("%23.22x", big, " 01234567890abcdef12345"),
        ("%-23.22x", big, "01234567890abcdef12345 "),
        ("%X", big, "1234567890ABCDEF12345"),
        ("%#X", big, "0X1234567890ABCDEF12345"),
        ("%#x", big, "0x1234567890abcdef12345"),
        ("%#x", -big, "-0x1234567890abcdef12345"),
        ("%#27x", big, "    0x1234567890abcdef12345"),
        ("%#-27x", big, "0x1234567890abcdef12345    "),
        ("%#027x", big, "0x00001234567890abcdef12345"),
        ("%#.23x", big, "0x001234567890abcdef12345"),
        ("%#.23x", -big, "-0x001234567890abcdef12345"),
        ("%#27.23x", big, "  0x001234567890abcdef12345"),
        ("%#-27.23x", big, "0x001234567890abcdef12345  "),
        ("%#027.23x", big, "0x00001234567890abcdef12345"),
        ("%#+.23x", big, "+0x001234567890abcdef12345"),
        ("%# .23x", big, " 0x001234567890abcdef12345"),
        ("%#+.23X", big, "+0X001234567890ABCDEF12345"),
        ("%#+027.23X", big, "+0X0001234567890ABCDEF12345"),
        ("%# 027.23X", big, " 0X0001234567890ABCDEF12345"),
        ("%#+27.23X", big, " +0X001234567890ABCDEF12345"),
        ("%#-+27.23x", big, "+0x001234567890abcdef12345 "),
        ("%#- 27.23x", big, " 0x001234567890abcdef12345 "),
    ] {
        assert_eq!(pct_ok(f, &[Arg::I(v)]), r, "{}", f);
    }
    let big: i128 = 0o12345670123456701234567012345670;
    for (f, v, r) in [
        ("%o", big, "12345670123456701234567012345670"),
        ("%o", -big, "-12345670123456701234567012345670"),
        ("%5o", -big, "-12345670123456701234567012345670"),
        ("%33o", -big, "-12345670123456701234567012345670"),
        ("%34o", -big, " -12345670123456701234567012345670"),
        ("%-34o", -big, "-12345670123456701234567012345670 "),
        ("%034o", -big, "-012345670123456701234567012345670"),
        ("%-034o", -big, "-12345670123456701234567012345670 "),
        ("%036o", -big, "-00012345670123456701234567012345670"),
        ("%036o", big, "000012345670123456701234567012345670"),
        ("%0+36o", big, "+00012345670123456701234567012345670"),
        ("%+36o", big, "   +12345670123456701234567012345670"),
        ("%36o", big, "    12345670123456701234567012345670"),
        ("%.2o", big, "12345670123456701234567012345670"),
        ("%.32o", big, "12345670123456701234567012345670"),
        ("%.33o", big, "012345670123456701234567012345670"),
        ("%34.33o", big, " 012345670123456701234567012345670"),
        ("%-34.33o", big, "012345670123456701234567012345670 "),
        ("%#o", big, "0o12345670123456701234567012345670"),
        ("%#o", -big, "-0o12345670123456701234567012345670"),
        ("%#38o", big, "    0o12345670123456701234567012345670"),
        ("%#-38o", big, "0o12345670123456701234567012345670    "),
        ("%#038o", big, "0o000012345670123456701234567012345670"),
        ("%#.34o", big, "0o0012345670123456701234567012345670"),
        ("%#.34o", -big, "-0o0012345670123456701234567012345670"),
        ("%#38.34o", big, "  0o0012345670123456701234567012345670"),
        ("%#-38.34o", big, "0o0012345670123456701234567012345670  "),
        ("%#038.34o", big, "0o000012345670123456701234567012345670"),
        ("%#+.34o", big, "+0o0012345670123456701234567012345670"),
        ("%# .34o", big, " 0o0012345670123456701234567012345670"),
        ("%#+38.34o", big, " +0o0012345670123456701234567012345670"),
        ("%#-+38.34o", big, "+0o0012345670123456701234567012345670 "),
        ("%#- 38.34o", big, " 0o0012345670123456701234567012345670 "),
        ("%#+038.34o", big, "+0o00012345670123456701234567012345670"),
        ("%# 038.34o", big, " 0o00012345670123456701234567012345670"),
        ("%#.33o", big, "0o012345670123456701234567012345670"),
        ("%#.32o", big, "0o12345670123456701234567012345670"),
        ("%035.33o", big, "00012345670123456701234567012345670"),
        ("%0#35.33o", big, "0o012345670123456701234567012345670"),
    ] {
        assert_eq!(pct_ok(f, &[Arg::I(v)]), r, "{}", f);
    }
    assert_eq!(pct_ok("%d", &[Arg::F(42.0)]), "42");
    assert_eq!(pct_ok("%g", &[Arg::F(1.1)]), "1.1");
    assert_eq!(pct_ok("%#g", &[Arg::F(1.1)]), "1.10000");
    assert_eq!(pct_ok("%f", &[Arg::F(1.0)]), "1.000000");
    assert_eq!(pct_ok("%%", &[]), "%");
    assert_eq!(pct_ok("\u{20ac}=%f", &[Arg::F(1.0)]), "\u{20ac}=1.000000");
    assert_eq!(pct_ok("%07.2f", &[Arg::I(42)]), "0042.00");
    assert_eq!(pct_ok("%07.2F", &[Arg::I(42)]), "0042.00");
    assert_eq!(pct_ok("%.1f", &[Arg::F(1.0)]), "1.0");
    assert_eq!(
        pct_ok("%s, %s, %i, %f, %5.2f", &[Arg::S("abc"), Arg::S("abc"), Arg::I(-1), Arg::I(-2), Arg::F(3.57)]),
        "abc, abc, -1, -2.000000,  3.57"
    );
    assert_eq!(pct_ok("%5.2f", &[Arg::F(1003.57)]), "1003.57");
    assert_eq!(pct_ok("%*s", &[Arg::I(5), Arg::S("abc")]), "  abc");
    assert_eq!(pct_ok("%*s", &[Arg::I(-5), Arg::S("abc")]), "abc  ");
    assert_eq!(pct_ok("%*.*s", &[Arg::I(5), Arg::I(2), Arg::S("abc")]), "   ab");
    assert_eq!(pct_ok("%*.*s", &[Arg::I(5), Arg::I(3), Arg::S("abc")]), "  abc");
    assert_eq!(pct_ok("%i %*.*s", &[Arg::I(10), Arg::I(5), Arg::I(3), Arg::S("abc")]), "10   abc");
    assert_eq!(pct_ok("%.1s", &[Arg::S("a\u{e9}\u{20ac}")]), "a");
    assert_eq!(pct_ok("%.2s", &[Arg::S("a\u{e9}\u{20ac}")]), "a\u{e9}");
    assert_eq!(pct_ok("%f", &[Arg::F(NAN)]), "nan");
    assert_eq!(pct_ok("%F", &[Arg::F(NAN)]), "NAN");
    assert_eq!(pct_ok("%f", &[Arg::F(INF)]), "inf");
    assert_eq!(pct_ok("%F", &[Arg::F(INF)]), "INF");
    assert_eq!(pct_ok("%x", &[Arg::I(42)]), "2a");
    assert_eq!(pct_ok("%X", &[Arg::I(15)]), "F");
    assert_eq!(pct_ok("%o", &[Arg::I(9)]), "11");
    // huge precisions / buffers
    for p in [109i128, 110] {
        assert!(pct("%#.*g", &[Arg::I(p), Arg::F(-1.0e49 / 3.0)]).is_ok());
    }
    for c in ["g", "G", "f", "F"] {
        let r = pct_ok(&format!("%#.*{}", c), &[Arg::I(110), Arg::F(-1.0e100 / 3.0)]);
        assert!(r.starts_with("-3.3333333333") || r.starts_with("-33333333333"), "{}", r);
    }
    assert_eq!(pct_ok("%12.*f", &[Arg::I(123456), Arg::F(1.0)]).len(), 123458);
    assert_eq!(pct_ok("%#.*g", &[Arg::I(110), Arg::F(-1.0e100 / 3.0)]).len(), 112);
    // test_floatformatting from string_tests: all precisions work
    for prec in 0..100 {
        let f = format!("%.{}f", prec);
        let mut value = 0.01;
        for _ in 0..60 {
            value = value * 3.14159265359 / 3.0 * 10.0;
            let r = pct_ok(&f, &[Arg::F(value)]);
            assert_eq!(r, format!("{:.*}", prec, value));
        }
    }
}

#[test]
fn percent_parse_errors() {
    let e = |f: &str| pct(f, &[Arg::I(1), Arg::I(1)]).unwrap_err().msg;
    assert_eq!(e("abc %"), "stray % at position 4");
    assert_eq!(e("abc % %s"), "stray % at position 4 or unexpected format character '%' at position 6");
    assert_eq!(e("abc %z"), "unsupported format %z at position 4");
    assert_eq!(e("abc %Id"), "unsupported format %I at position 4");
    assert_eq!(e("abc %'d"), "stray % at position 4 or unexpected format character \"'\" at position 5");
    assert_eq!(e("abc %1 d"), "stray % at position 4 or unexpected format character ' ' at position 6");
    assert_eq!(e("abc % (x)r"), "stray % at position 4 or unexpected format character '(' at position 6");
    assert_eq!(e("abc %((x)r"), "stray % or incomplete format key at position 4");
    assert_eq!(e(&format!("abc %{}r", "9".repeat(50))), "width too big at position 4");
    assert_eq!(e(&format!("abc %.{}r", "9".repeat(50))), "precision too big at position 4");
    assert_eq!(e("abc %b"), "unsupported format %b at position 4");
    assert_eq!(e("abc %\nd"), "stray % at position 4 or unexpected format character U+000A at position 5");
    assert_eq!(e("abc %\x1fd"), "stray % at position 4 or unexpected format character U+001F at position 5");
    assert_eq!(e("abc %\x7fd"), "stray % at position 4 or unexpected format character U+007F at position 5");
    assert_eq!(e("abc %\u{80}d"), "stray % at position 4 or unexpected format character U+0080 at position 5");
    assert_eq!(e("abc %äd"), "stray % at position 4 or unexpected format character 'ä' (U+00E4) at position 5");
    assert_eq!(e("abc %€d"), "stray % at position 4 or unexpected format character '€' (U+20AC) at position 5");
    assert!(pct("%.1\u{1032}f", &[Arg::F(1.0)]).is_err());
    assert!(pct("%10", &[Arg::I(42)]).is_err());
    assert!(pct(&format!("%{}f", 1u128 << 64), &[Arg::F(3.2)]).is_err());
    assert!(pct(&format!("%.{}f", 1u128 << 64), &[Arg::F(3.2)]).is_err());
    assert!(pct(&format!("%.{}f", i32::MAX as i64 + 1), &[Arg::F(2.34)]).is_err());
    assert!(pct(&format!("%{}f", i64::MAX as u64 + 1), &[Arg::F(2.34)]).is_err());
    // mapping keys
    let chars: Vec<char> = "%(x)*r".chars().collect();
    assert_eq!(
        parse_percent_spec(&chars, 0, false, &default_is_printable).unwrap_err().msg,
        "* cannot be used with a parenthesised mapping key at position 0"
    );
    let chars: Vec<char> = "%(x).*r".chars().collect();
    assert_eq!(
        parse_percent_spec(&chars, 0, false, &default_is_printable).unwrap_err().msg,
        "* cannot be used with a parenthesised mapping key at position 0"
    );
    let chars: Vec<char> = "%((foo))s".chars().collect();
    let s = parse_percent_spec(&chars, 0, false, &default_is_printable).unwrap();
    assert_eq!(s.key, Some((2, 7)));
    assert_eq!(s.conv, 's');
    assert!(parse_percent_spec(&"%(foo".chars().collect::<Vec<_>>(), 0, false, &default_is_printable).is_err());
    // bytes
    let be = |f: &[u8]| parse_percent_spec(f, 4, true, &default_is_printable).unwrap_err().msg;
    assert_eq!(be(b"abc %\nd"), "stray % at position 4 or unexpected format character with code 0x0a at position 5");
    assert_eq!(be(b"abc %'d"), "stray % at position 4 or unexpected format character \"'\" at position 5");
    assert_eq!(be(b"abc %\x1fd"), "stray % at position 4 or unexpected format character with code 0x1f at position 5");
    assert_eq!(be(b"abc %\x7fd"), "stray % at position 4 or unexpected format character with code 0x7f at position 5");
    assert_eq!(be(b"abc %\x80d"), "stray % at position 4 or unexpected format character with code 0x80 at position 5");
    assert_eq!(parse_percent_spec(b"%z.1f", 0, true, &default_is_printable).unwrap_err().msg, "unsupported format %z at position 0");
    assert_eq!(parse_percent_spec(b"%b", 0, true, &default_is_printable).unwrap().conv, 'b');
}

#[test]
fn misc_edge_cases() {
    assert_eq!(ff(1e16, ","), "1e+16");
    assert_eq!(ff(1234.5, ","), "1,234.5");
    assert_eq!(ff(1234567.0, "_"), "1_234_567.0");
    assert_eq!(ff(INF, "010"), "0000000inf");
    assert_eq!(ff(-INF, "010"), "-000000inf");
    assert_eq!(ff(NAN, "+010"), "+000000nan");
    assert_eq!(ff(-0.0, ""), "-0.0");
    assert_eq!(ff(-0.0, "z"), "0.0");
    assert_eq!(ff(1.0, ".1"), "1e+00");
    assert_eq!(ff(1.0, ".3"), "1.0");
    assert_eq!(ff(INF, "%"), "inf%");
    assert_eq!(ff(0.5, ".0%"), "50%");
    assert_eq!(ff(1234.5, "n"), "1234.5");
    assert_eq!(ff(1e-7, ""), "1e-07");
    assert_eq!(ff(123.0, "e"), "1.230000e+02");
    assert_eq!(ff(f64::MAX, ".0f").len(), 309);
    assert_eq!(ff(5e-324, ".1074f").len(), 1076);
    let s = ff(5e-324, ".1074f");
    assert!(s.starts_with("0.000") && s.ends_with("625") && s[2..].trim_start_matches('0').len() == 751);
    assert_eq!(&ff(5e-324, ".1100e")[..20], "4.940656458412465441");
    assert_eq!(format_i128(255, "#_b", "int").unwrap(), "0b1111_1111");
    assert_eq!(format_i128(-255, "08X", "int").unwrap(), "-00000FF");
    assert_eq!(format_i128(i128::MIN, "x", "int").unwrap(), format!("-{:x}", i128::MIN.unsigned_abs()));
    assert_eq!(format_i128(10, "%", "int").unwrap(), "1000.000000%");
    assert_eq!(format_i128(97, "c", "int").unwrap(), "a");
    assert_eq!(format_i128(-5, "", "int").unwrap(), "-5");
    // %-format zero padding of non-finite values follows the generic rule
    assert_eq!(pct_ok("%05f", &[Arg::F(INF)]), "00inf");
    assert_eq!(pct_ok("%-6f|", &[Arg::F(-INF)]), "-inf  |");
    assert_eq!(pct_ok("% d", &[Arg::I(5)]), " 5");
    assert_eq!(pct_ok("%+d", &[Arg::I(0)]), "+0");
    assert_eq!(pct_ok("%.0d", &[Arg::I(0)]), "0");
    assert_eq!(pct_ok("%5s|%-5s|", &[Arg::S("ab"), Arg::S("cd")]), "   ab|cd   |");
    assert_eq!(pct_ok("%05s", &[Arg::S("ab")]), "   ab");
    assert_eq!(pct_ok("% .3e", &[Arg::F(12345.678)]), " 1.235e+04");
    assert_eq!(pct_ok("%+.3e", &[Arg::F(12345.678)]), "+1.235e+04");
    assert_eq!(pct_ok("% 10.3e", &[Arg::F(-12345.678)]), "-1.235e+04");
    assert_eq!(pct_ok("% 011.3e", &[Arg::F(12345.678)]), " 01.235e+04");
}

/// test_strtod-style checks: exact halfway decimal strings must round half
/// to even; boundaries near 2^-1075 and DBL_MAX.
#[test]
fn strtod_halfway_cases() {
    let mut state = 0x0123_4567_89ab_cdefu64;
    let mut rnd = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..20000 {
        let m = (rnd() & ((1u64 << 52) - 1)) | (1u64 << 52);
        let e = (rnd() % 60) as i32 - 29; // value = m * 2^e
        let half = 2 * m as u128 + 1; // (2m+1) * 2^(e-1)
        let s = if e - 1 >= 0 {
            format!("{}", half << (e - 1))
        } else {
            let k = (1 - e) as u32;
            format!("{}e-{}", half * 5u128.pow(k), k)
        };
        let got = parse_float(&s).unwrap();
        let want_m = if m % 2 == 0 { m } else { m + 1 };
        let want = want_m as f64 * 2f64.powi(e);
        assert_eq!(got, want, "{}", s);
        // just below / above
        let below = parse_float(&format!("{}", if e - 1 >= 0 { format!("{}", (half << (e - 1)) - 1) } else { format!("{}e-{}", half * 5u128.pow((1 - e) as u32) - 1, 1 - e) })).unwrap();
        assert_eq!(below, m as f64 * 2f64.powi(e));
    }
    // underflow boundary: 2^-1075 rounds to 0 (tie, even), anything above to 5e-324
    let tiny_half = "2.4703282292062327208828439643411068618252990130716238221279284125033775363510437593264991818081799618989828234772285886546332835517796989819938739800539093906315035659515570226392290858392449105184435931802849936536152500319370457678249219365623669863658480757001585769269903706311928279558551332927834338409351978015531246597263579574622766465272827220056374006485499977096599470454020828166226237857393450736339007967761930577506740176324673600968951340535537458516661134223766678604162159680461914467291840300530057530849048765391711386591646239524912623653881879636239373280423891018672348497668235089863388587925628302755995657524455507255189313690836254779186948667994968324049705821028513185451396213837722826145437693412532098591327667236328125e-324";
    assert!(ident(parse_float(tiny_half).unwrap(), 0.0));
    let just_above = tiny_half.replace("328125e-324", "328126e-324");
    assert_eq!(parse_float(&just_above).unwrap(), 5e-324);
    for s in [
        "2183167012312112312312.23538020374420446192e-370",
        "12579816049008305546974391768996369464963024663104e-357",
        "1.00000000000000001e44",
        "1.0000000000000000100000000000000000000001e44",
        "99999999999999994487665465554760717039532578546e-47",
        "247032822920623295e-341",
        "1000000000000000000000000000000000000000e-16",
        "10000000000000000000000000000000000000000e-17",
        "991633793189150720000000000000000000000000000000000000000e-33",
        "4106250198039490000000000000000000000000000000000000000e-38",
        "10.900000000000000012345678912345678912345",
        "9007199254740992.00",
        "999999999999999944488848768742172978818416595458984375e-54",
        "9999999999999999444888487687421729788184165954589843749999999e-54",
        "9999999999999999444888487687421729788184165954589843750000001e-54",
    ] {
        // consistency: value round-trips through repr and hex
        let v = parse_float(s).unwrap();
        assert_eq!(parse_float(&float_repr(v)).unwrap(), v, "{}", s);
    }
    assert_eq!(parse_float("10.900000000000000012345678912345678912345").unwrap(), 10.9);
    assert_eq!(parse_float("999999999999999944488848768742172978818416595458984375e-54").unwrap(), 1.0);
}
