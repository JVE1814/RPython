//! Runner for the IBM .decTest files (spec/Lib/test/decimaltestdata), mirroring the
//! semantics of test_decimal.IBMTestCases against the engine.

use super::dec::*;
use super::ops;
use super::transc;

const SKIPPED: &[&str] = &[
    "scbx164", "scbx165", "expx901", "expx902", "expx903", "expx905", "lnx901", "lnx902", "lnx903", "lnx905", "logx901",
    "logx902", "logx903", "logx905", "powx1183", "powx1184", "powx4001", "powx4002", "powx4003", "powx4005", "powx4008",
    "powx4010", "powx4012", "powx4014",
];

fn signal_names(st: u32) -> Vec<&'static str> {
    let mut v = vec![];
    if st & CLAMPED != 0 {
        v.push("clamped");
    }
    if st & DIVISION_BY_ZERO != 0 {
        v.push("division_by_zero");
    }
    if st & INEXACT != 0 {
        v.push("inexact");
    }
    if st & IEEE_INVALID != 0 {
        v.push("invalid_operation");
    }
    if st & OVERFLOW != 0 {
        v.push("overflow");
    }
    if st & ROUNDED != 0 {
        v.push("rounded");
    }
    if st & SUBNORMAL != 0 {
        v.push("subnormal");
    }
    if st & UNDERFLOW != 0 {
        v.push("underflow");
    }
    v.sort();
    v
}

fn expected_names(ex: &[String]) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = ex
        .iter()
        .map(|e| match e.to_lowercase().as_str() {
            "clamped" => "clamped",
            "conversion_syntax" | "division_impossible" | "division_undefined" | "invalid_context" | "invalid_operation" => {
                "invalid_operation"
            }
            "division_by_zero" => "division_by_zero",
            "inexact" => "inexact",
            "overflow" => "overflow",
            "rounded" => "rounded",
            "subnormal" => "subnormal",
            "underflow" => "underflow",
            other => panic!("unknown condition {other}"),
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

fn fix_quotes(s: &str) -> String {
    s.replace("''", "\u{1}").replace("\"\"", "\u{2}").replace(['\'', '"'], "").replace('\u{1}', "'").replace('\u{2}', "\"")
}

/// Exact conversion (Decimal(v, context)).
pub fn convert_exact(s: &str, ctx: &mut Ctx) -> Dec {
    match parse(s.trim().as_bytes()) {
        None => {
            ctx.status |= CONVERSION_SYNTAX;
            Dec::nan()
        }
        Some(d) => {
            let mut mc = Ctx::max_ctx();
            let r = if d.is_finite() { finalize(d, &mut mc, nat_rest_zero()) } else { d };
            if mc.status & (INEXACT | ROUNDED | CLAMPED) != 0 {
                ctx.status |= INVALID_OPERATION;
                return Dec::nan();
            }
            r
        }
    }
}

fn nat_rest_zero() -> super::nat::Rest {
    super::nat::Rest::Zero
}

/// Context.create_decimal(v)
pub fn convert_ctx(s: &str, ctx: &mut Ctx) -> Dec {
    match parse(s.as_bytes()) {
        None => {
            ctx.status |= CONVERSION_SYNTAX;
            Dec::nan()
        }
        Some(d) => {
            if d.is_nan() && super::nat::digits(&d.coef) as i64 > ctx.prec - ctx.clamp as i64 {
                ctx.status |= CONVERSION_SYNTAX;
                return Dec::nan();
            }
            ops::apply(&d, ctx)
        }
    }
}

pub struct Outcome {
    pub pass: usize,
    pub fail: Vec<String>,
}

fn b(v: bool) -> String {
    if v { "1".into() } else { "0".into() }
}

pub fn run_file(path: &std::path::Path, out: &mut Outcome) {
    let text = std::fs::read_to_string(path).unwrap();
    let mut ctx = Ctx::default_ctx();
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        let s: String = if line.contains(" -> ") && !line.starts_with("--") && !line.starts_with("  --") {
            let parts: Vec<&str> = line.split("->").collect();
            let rhs = parts[1].split("--").next().unwrap();
            format!("{}->{}", parts[0], rhs).trim().to_string()
        } else {
            line.split("--").next().unwrap().trim().to_string()
        };
        if s.contains('#') || s.is_empty() {
            continue;
        }
        if s.contains(':') && !s.contains("->") {
            let mut it = s.splitn(2, ':');
            let f = it.next().unwrap().trim().to_lowercase();
            let v = it.next().unwrap().trim().to_lowercase();
            match f.as_str() {
                "precision" => ctx.prec = v.parse().unwrap(),
                "rounding" => {
                    ctx.round = match v.as_str() {
                        "ceiling" => Round::Ceiling,
                        "down" => Round::Down,
                        "floor" => Round::Floor,
                        "half_down" => Round::HalfDown,
                        "half_even" => Round::HalfEven,
                        "half_up" => Round::HalfUp,
                        "up" => Round::Up,
                        "05up" => Round::Up05,
                        _ => panic!("rounding {v}"),
                    }
                }
                "maxexponent" => ctx.emax = v.parse().unwrap(),
                "minexponent" => ctx.emin = v.parse().unwrap(),
                "clamp" => ctx.clamp = v.parse::<i64>().unwrap() != 0,
                _ => {}
            }
            continue;
        }
        if s.contains(':') {
            // a directive-like line with "->"? treat as equation anyway
        }
        let sides: Vec<&str> = s.split("->").collect();
        if sides.len() < 2 {
            continue;
        }
        let l: Vec<&str> = sides[0].trim().split_whitespace().collect();
        let id = l[0];
        let funct = l[1].to_lowercase();
        let r: Vec<&str> = sides[1].trim().split_whitespace().collect();
        let ans = fix_quotes(r[0]);
        let exceptions: Vec<String> = r[1..].iter().map(|x| x.to_string()).collect();
        if SKIPPED.contains(&id) || funct == "rescale" {
            continue;
        }
        // operands with quote handling
        let mut vals_s: Vec<String> = vec![];
        let mut conglomerate = String::new();
        let mut quote = false;
        for val in &l[2..] {
            if val.matches('\'').count() % 2 == 1 {
                quote = !quote;
            }
            if quote {
                conglomerate = format!("{} {}", conglomerate, val);
                continue;
            }
            let v = format!("{}{}", conglomerate, val);
            conglomerate.clear();
            vals_s.push(fix_quotes(&v));
        }
        ctx.status = 0;
        let to_str = matches!(funct.as_str(), "tosci" | "toeng" | "apply");
        let vals: Vec<Dec> =
            vals_s.iter().map(|v| if to_str { convert_ctx(v, &mut ctx) } else { convert_exact(v, &mut ctx) }).collect();
        let result = eval(&funct, &vals, &mut ctx);
        let result = match result {
            Some(r) => r,
            None => {
                out.fail.push(format!("{id}: unknown op {funct}"));
                continue;
            }
        };
        let mine = signal_names(ctx.status);
        let theirs = expected_names(&exceptions);
        if result == ans && mine == theirs {
            out.pass += 1;
        } else {
            out.fail.push(format!(
                "{} [{}] prec={} round={:?} emin={} emax={} clamp={}: got {} {:?}",
                s, path.file_name().unwrap().to_string_lossy(), ctx.prec, ctx.round, ctx.emin, ctx.emax, ctx.clamp as u8, result, mine
            ));
        }
    }
}

fn eval(f: &str, v: &[Dec], c: &mut Ctx) -> Option<String> {
    let s = |d: Dec| to_string(&d, false, true);
    Some(match f {
        "abs" => s(ops::abs(&v[0], c)),
        "add" => s(ops::add(&v[0], &v[1], c)),
        "subtract" => s(ops::sub(&v[0], &v[1], c)),
        "multiply" => s(ops::mul(&v[0], &v[1], c)),
        "divide" => s(ops::div(&v[0], &v[1], c)),
        "divideint" => s(ops::divint(&v[0], &v[1], c)),
        "remainder" => s(ops::rem(&v[0], &v[1], c)),
        "remaindernear" => s(ops::rem_near(&v[0], &v[1], c)),
        "fma" => s(ops::fma(&v[0], &v[1], &v[2], c)),
        "compare" => s(ops::compare(&v[0], &v[1], c)),
        "comparesig" => s(ops::compare_signal(&v[0], &v[1], c)),
        "comparetotal" => s(ops::compare_total(&v[0], &v[1])),
        "comparetotmag" => s(ops::compare_total_mag(&v[0], &v[1])),
        "max" => s(ops::max(&v[0], &v[1], c)),
        "min" => s(ops::min(&v[0], &v[1], c)),
        "maxmag" | "max_mag" => s(ops::max_mag(&v[0], &v[1], c)),
        "minmag" | "min_mag" => s(ops::min_mag(&v[0], &v[1], c)),
        "minus" => s(ops::minus(&v[0], c)),
        "plus" => s(ops::plus(&v[0], c)),
        "quantize" => {
            let r = c.round;
            s(ops::quantize(&v[0], &v[1], r, c))
        }
        "samequantum" => b(ops::same_quantum(&v[0], &v[1])),
        "reduce" => s(ops::reduce(&v[0], c)),
        "tointegral" => {
            let r = c.round;
            s(ops::to_integral(&v[0], r, false, c))
        }
        "tointegralx" => {
            let r = c.round;
            s(ops::to_integral(&v[0], r, true, c))
        }
        "logb" => s(ops::logb(&v[0], c)),
        "scaleb" => s(ops::scaleb(&v[0], &v[1], c)),
        "shift" => s(ops::shift(&v[0], &v[1], c)),
        "rotate" => s(ops::rotate(&v[0], &v[1], c)),
        "and" => s(ops::logical_op(&v[0], &v[1], b'&', c)),
        "or" => s(ops::logical_op(&v[0], &v[1], b'|', c)),
        "xor" => s(ops::logical_op(&v[0], &v[1], b'^', c)),
        "invert" => s(ops::logical_invert(&v[0], c)),
        "nextplus" => s(ops::next_plus(&v[0], c)),
        "nextminus" => s(ops::next_minus(&v[0], c)),
        "nexttoward" => s(ops::next_toward(&v[0], &v[1], c)),
        "class" => ops::number_class(&v[0], c).to_string(),
        "copy" => s(v[0].clone()),
        "copyabs" => s(v[0].abs()),
        "copynegate" => s(v[0].negated()),
        "copysign" => s(ops::copy_sign(&v[0], &v[1])),
        "iscanonical" => b(true),
        "isfinite" => b(v[0].is_finite()),
        "isinfinite" => b(v[0].is_inf()),
        "isnan" => b(v[0].is_nan()),
        "isnormal" => b(ops::is_normal(&v[0], c)),
        "isqnan" => b(v[0].is_qnan()),
        "issigned" => b(v[0].neg),
        "issnan" => b(v[0].is_snan()),
        "issubnormal" => b(ops::is_subnormal(&v[0], c)),
        "iszero" => b(v[0].is_zero()),
        "squareroot" => s(transc::sqrt(&v[0], c)),
        "exp" => s(transc::exp(&v[0], c)),
        "ln" => s(transc::ln(&v[0], c)),
        "log10" => s(transc::log10(&v[0], c)),
        "power" => {
            if v.len() == 3 {
                s(transc::power_mod(&v[0], &v[1], &v[2], c))
            } else {
                s(transc::power(&v[0], &v[1], c))
            }
        }
        "tosci" | "apply" => s(v[0].clone()),
        "toeng" => to_string(&v[0], true, true),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        std::path::PathBuf::from(
            std::env::var("DECTEST_DIR")
                .unwrap_or_else(|_| format!("{}/../../spec/Lib/test/decimaltestdata", option_env!("CARGO_MANIFEST_DIR").unwrap_or("."))),
        )
    }

    #[test]
    fn dectest_all() {
        let filter = std::env::var("DECTEST_FILTER").ok();
        let mut files: Vec<_> = std::fs::read_dir(dir()).unwrap().map(|e| e.unwrap().path()).collect();
        files.sort();
        let mut out = Outcome { pass: 0, fail: vec![] };
        for f in files {
            let name = f.file_name().unwrap().to_string_lossy().to_string();
            if !name.ends_with(".decTest") {
                continue;
            }
            if let Some(flt) = &filter {
                if !name.starts_with(flt.as_str()) {
                    continue;
                }
            }
            let t = std::time::Instant::now();
            run_file(&f, &mut out);
            let el = t.elapsed().as_secs_f64();
            if el > 1.0 {
                eprintln!("slow file {name}: {el:.1}s");
            }
        }
        let max = std::env::var("DECTEST_SHOW").ok().and_then(|v| v.parse().ok()).unwrap_or(60);
        for f in out.fail.iter().take(max) {
            eprintln!("FAIL {f}");
        }
        eprintln!("decTest: {} passed, {} failed", out.pass, out.fail.len());
        assert!(out.fail.is_empty());
    }
}
