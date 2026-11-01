//! Tests driven by the data files in spec/Lib/test/mathdata.

use rpy_numfmt::percent::{parse_percent_spec, percent_format_float, PercentOpts};
use rpy_numfmt::*;
use std::path::PathBuf;

fn spec_path(rel: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../spec/Lib/test");
    p.push(rel);
    p
}

fn percent_float(fmt: &str, x: f64) -> String {
    let chars: Vec<char> = fmt.chars().collect();
    let spec = parse_percent_spec(&chars, 0, false, &percent::default_is_printable).unwrap();
    assert_eq!(spec.end, chars.len(), "{}", fmt);
    let mut out = String::new();
    if spec.conv == 'r' {
        write_float_repr(&mut out, x);
    } else {
        percent_format_float(&mut out, x, spec.conv, &PercentOpts::from_spec(&spec)).unwrap();
    }
    out
}

#[test]
fn formatfloat_testcases() {
    let text = std::fs::read_to_string(spec_path("mathdata/formatfloat_testcases.txt")).unwrap();
    let mut n = 0;
    for line in text.lines() {
        if line.starts_with("--") {
            continue;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (lhs, rhs) = line.split_once("->").unwrap();
        let (lhs, rhs) = (lhs.trim(), rhs.trim());
        let mut it = lhs.split_whitespace();
        let fmt = it.next().unwrap();
        let arg = it.next().unwrap();
        let f = parse_float(arg).unwrap();
        assert_eq!(percent_float(fmt, f), rhs, "{} % {}", fmt, arg);
        assert_eq!(percent_float(fmt, -f), format!("-{}", rhs), "{} % -{}", fmt, arg);
        if fmt != "%r" {
            let fmt2 = &fmt[1..];
            assert_eq!(format_float(f, fmt2, "float").unwrap(), rhs, "format({}, {})", arg, fmt2);
            assert_eq!(
                format_float(-f, fmt2, "float").unwrap(),
                format!("-{}", rhs),
                "format(-{}, {})",
                arg,
                fmt2
            );
        }
        n += 1;
    }
    assert!(n > 250);
}

#[test]
fn floating_points_roundtrip() {
    let text = std::fs::read_to_string(spec_path("mathdata/floating_points.txt")).unwrap();
    let mut n = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let v = parse_float(line).unwrap();
        let r = float_repr(v);
        let back = parse_float(&r).unwrap();
        assert_eq!(back.to_bits(), v.to_bits(), "{} -> {}", line, r);
        // hex round trip too
        assert_eq!(float_fromhex(&float_hex(v)).unwrap().to_bits(), v.to_bits());
        n += 1;
    }
    assert!(n > 900);
}

/// math_testcases.txt / cmath_testcases.txt values must all parse.
#[test]
fn mathdata_values_parse() {
    for (file, nvals) in [("mathdata/math_testcases.txt", 1), ("mathdata/cmath_testcases.txt", 2)] {
        let text = std::fs::read_to_string(spec_path(file)).unwrap();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("--") {
                continue;
            }
            let (lhs, rhs) = match line.split_once("->") {
                Some(x) => x,
                None => continue,
            };
            let lhs_vals = lhs.split_whitespace().skip(2);
            let rhs_vals = rhs.split_whitespace().take(nvals);
            for tok in lhs_vals.chain(rhs_vals) {
                assert!(parse_float(tok).is_ok(), "{}: {:?} in {}", file, tok, line);
            }
        }
    }
}
