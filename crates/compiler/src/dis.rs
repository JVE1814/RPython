//! Human-readable disassembly of code units (debugging aid).

use crate::bytecode::*;
use std::fmt::Write;

fn const_str(c: &Constant) -> String {
    match c {
        Constant::None => "None".into(),
        Constant::Bool(b) => if *b { "True" } else { "False" }.into(),
        Constant::Ellipsis => "Ellipsis".into(),
        Constant::Int(i) => i.to_string(),
        Constant::BigInt { neg, decimal } => format!("{}{}", if *neg { "-" } else { "" }, decimal),
        Constant::Float(f) => crate::unparse::float_repr(*f),
        Constant::Complex { real, imag } => format!("({}+{}j)", real, imag),
        Constant::Str(s) => crate::unparse::str_repr(s),
        Constant::Bytes(b) => crate::unparse::bytes_repr(b),
        Constant::Tuple(v) => {
            let inner: Vec<String> = v.iter().map(const_str).collect();
            if v.len() == 1 {
                format!("({},)", inner[0])
            } else {
                format!("({})", inner.join(", "))
            }
        }
        Constant::FrozenSet(v) => format!("frozenset({{{}}})", v.iter().map(const_str).collect::<Vec<_>>().join(", ")),
        Constant::Code(c) => format!("<code object {}, line {}>", c.name, c.firstlineno),
    }
}

/// Disassemble a code unit and (recursively) the code units in its constants.
pub fn disassemble(code: &CodeUnit) -> String {
    let mut out = String::new();
    dis_one(code, &mut out);
    for c in &code.consts {
        if let Constant::Code(sub) = c {
            out.push('\n');
            out.push_str(&disassemble(sub));
        }
    }
    out
}

fn dis_one(code: &CodeUnit, out: &mut String) {
    let _ = writeln!(
        out,
        "Disassembly of {} ({}) argc={} posonly={} kwonly={} flags={:#x} stack={}",
        code.qualname, code.name, code.argcount, code.posonlyargcount, code.kwonlyargcount, code.flags, code.stacksize
    );
    let _ = writeln!(out, "  localsplus: {:?}", code.localsplusnames.iter().zip(code.localspluskinds.iter()).map(|(n, k)| format!("{}:{:#x}", n, k)).collect::<Vec<_>>());
    let mut targets = std::collections::HashSet::new();
    for &w in &code.code {
        let (op, arg) = decode(w);
        if op.is_jump() {
            targets.insert(arg);
        }
    }
    for e in &code.exception_table {
        targets.insert(e.target);
    }
    let mut ext: u32 = 0;
    let mut last_line = 0;
    for (i, &w) in code.code.iter().enumerate() {
        let (op, a) = decode(w);
        let arg = (ext << 24) | a;
        if op == Op::ExtendedArg {
            ext = a;
        } else {
            ext = 0;
        }
        let loc = code.locations.get(i).copied().unwrap_or_default();
        let line = if loc.lineno != last_line && loc.lineno != 0 {
            last_line = loc.lineno;
            format!("{:>4}", loc.lineno)
        } else {
            "    ".to_string()
        };
        let mark = if targets.contains(&(i as u32)) { ">>" } else { "  " };
        let detail = match op {
            Op::LoadConst => code.consts.get(arg as usize).map(const_str).unwrap_or_default(),
            Op::LoadName | Op::StoreName | Op::DeleteName | Op::LoadGlobal | Op::StoreGlobal | Op::DeleteGlobal | Op::LoadAttr
            | Op::LoadMethod | Op::StoreAttr | Op::DeleteAttr | Op::ImportFrom | Op::LoadFromDictOrGlobals => {
                code.names.get(arg as usize).cloned().unwrap_or_default()
            }
            Op::ImportName => code.names.get((arg & 0x7f_ffff) as usize).cloned().unwrap_or_default() + if arg >> 23 != 0 { " (lazy)" } else { "" },
            Op::LoadSuperAttr => code.names.get((arg >> 2) as usize).cloned().unwrap_or_default(),
            Op::LoadFast | Op::LoadFastCheck | Op::LoadFastAndClear | Op::StoreFast | Op::DeleteFast | Op::LoadDeref
            | Op::StoreDeref | Op::DeleteDeref | Op::MakeCell | Op::LoadFromDictOrDeref => {
                code.localsplusnames.get(arg as usize).cloned().unwrap_or_default()
            }
            _ if op.is_jump() => format!("to {}", arg),
            _ => String::new(),
        };
        let _ = writeln!(out, "{} {} {:>5} {:<24} {:>4} {}", line, mark, i, format!("{:?}", op), arg, detail);
    }
    if !code.exception_table.is_empty() {
        let _ = writeln!(out, "ExceptionTable:");
        for e in &code.exception_table {
            let _ = writeln!(out, "  {} to {} -> {} [{}]{}", e.start, e.end, e.target, e.depth, if e.lasti { " lasti" } else { "" });
        }
    }
}
