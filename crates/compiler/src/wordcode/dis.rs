//! A plain-text disassembler for [`CodeObject`]s (debugging aid; `Lib/dis.py` is the real one).

use super::*;

fn const_repr(c: &Constant) -> String {
    match c {
        Constant::None => "None".into(),
        Constant::Bool(b) => if *b { "True" } else { "False" }.into(),
        Constant::Ellipsis => "Ellipsis".into(),
        Constant::Int(i) => i.to_string(),
        Constant::BigInt { neg, decimal } => format!("{}{}", if *neg { "-" } else { "" }, decimal),
        Constant::Float(f) => format!("{:?}", f),
        Constant::Complex { real, imag } => format!("({}+{}j)", real, imag),
        Constant::Str(s) => format!("{:?}", s.iter().map(|&c| char::from_u32(c).unwrap_or('?')).collect::<String>()).replace('"', "'"),
        Constant::Bytes(b) => format!("b{:?}", String::from_utf8_lossy(b)),
        Constant::Tuple(v) => {
            let items: Vec<String> = v.iter().map(const_repr).collect();
            if items.len() == 1 {
                format!("({},)", items[0])
            } else {
                format!("({})", items.join(", "))
            }
        }
        Constant::FrozenSet(v) => format!("frozenset({{{}}})", v.iter().map(const_repr).collect::<Vec<_>>().join(", ")),
        Constant::Slice(s) => format!("slice({}, {}, {})", const_repr(&s[0]), const_repr(&s[1]), const_repr(&s[2])),
        Constant::Code(c) => format!("<code object {}, line {}>", c.name, c.firstlineno),
        Constant::Opaque(i) => format!("<const #{}>", i),
    }
}

/// Disassemble `code` (and nested code objects) in a `dis`-like layout.
pub fn disassemble(code: &CodeObject) -> String {
    let mut out = String::new();
    disassemble_into(code, &mut out);
    for c in &code.consts {
        if let Constant::Code(inner) = c {
            out.push_str(&format!("\nDisassembly of <code object {} line {}>:\n", inner.name, inner.firstlineno));
            out.push_str(&disassemble(inner));
        }
    }
    out
}

fn disassemble_into(code: &CodeObject, out: &mut String) {
    let instrs = instructions(&code.code);
    let positions = decode_linetable(&code.linetable, code.firstlineno, code.code.len() / 2);
    let excs = decode_exception_table(&code.exceptiontable);
    // labels: jump targets and exception-table boundaries/targets
    let mut label_offsets: Vec<u32> = Vec::new();
    for i in &instrs {
        if let Some(t) = i.jump_target() {
            label_offsets.push(t);
        }
    }
    for e in &excs {
        label_offsets.push(e.start);
        label_offsets.push(e.end);
        label_offsets.push(e.target);
    }
    label_offsets.sort();
    label_offsets.dedup();
    let label_of = |off: u32| label_offsets.iter().position(|&o| o == off).map(|p| p + 1);
    let mut last_line: Option<Option<u32>> = None;
    for i in &instrs {
        let pos = positions.get(i.start as usize).copied().unwrap_or(Position::NONE);
        let line = pos.lineno;
        let line_field = if last_line != Some(line) {
            if last_line.is_some() {
                out.push('\n');
            }
            last_line = Some(line);
            match line {
                Some(l) => format!("{:>4}", l),
                None => "  --".to_string(),
            }
        } else {
            "    ".to_string()
        };
        let label = match label_of(i.start) {
            Some(l) => format!("L{}:", l),
            None => String::new(),
        };
        let op = i.op as u16;
        let name = opname(op).unwrap_or("<?>");
        let a = i.arg;
        let argrepr = if has_const(op) {
            code.consts.get(a as usize).map(const_repr).unwrap_or_default()
        } else if op == LOAD_GLOBAL {
            format!("{}{}", code.names.get((a >> 1) as usize).cloned().unwrap_or_default(), if a & 1 != 0 { " + NULL" } else { "" })
        } else if op == LOAD_ATTR {
            format!("{}{}", code.names.get((a >> 1) as usize).cloned().unwrap_or_default(), if a & 1 != 0 { " + NULL|self" } else { "" })
        } else if op == LOAD_SUPER_ATTR {
            code.names.get((a >> 2) as usize).cloned().unwrap_or_default()
        } else if op == IMPORT_NAME {
            let m = ["", " + lazy", " + eager", ""][(a & 3) as usize];
            format!("{}{}", code.names.get((a >> 2) as usize).cloned().unwrap_or_default(), m)
        } else if has_name(op) {
            code.names.get(a as usize).cloned().unwrap_or_default()
        } else if matches!(op, LOAD_FAST_LOAD_FAST | LOAD_FAST_BORROW_LOAD_FAST_BORROW | STORE_FAST_LOAD_FAST | STORE_FAST_STORE_FAST) {
            let n1 = code.localsplusnames.get((a >> 4) as usize).cloned().unwrap_or_default();
            let n2 = code.localsplusnames.get((a & 15) as usize).cloned().unwrap_or_default();
            format!("{}, {}", n1, n2)
        } else if has_local(op) || has_free(op) {
            code.localsplusnames.get(a as usize).cloned().unwrap_or_default()
        } else if let Some(t) = i.jump_target() {
            let l = label_of(t).map(|l| format!("L{}", l)).unwrap_or_else(|| t.to_string());
            if op == END_ASYNC_FOR {
                format!("from {}", l)
            } else {
                format!("to {}", l)
            }
        } else if op == COMPARE_OP {
            let s = CMP_OP.get((a >> 5) as usize).copied().unwrap_or("?");
            if a & 16 != 0 {
                format!("bool({})", s)
            } else {
                s.to_string()
            }
        } else if op == BINARY_OP {
            NB_OPS.get(a as usize).map(|x| x.1.to_string()).unwrap_or_default()
        } else if op == CALL_INTRINSIC_1 {
            INTRINSIC1.get(a as usize).copied().unwrap_or("").to_string()
        } else if op == CALL_INTRINSIC_2 {
            INTRINSIC2.get(a as usize).copied().unwrap_or("").to_string()
        } else if op == LOAD_SPECIAL {
            SPECIAL_METHODS.get(a as usize).copied().unwrap_or("").to_string()
        } else if op == LOAD_COMMON_CONSTANT {
            ["AssertionError", "NotImplementedError", "tuple", "all", "any", "list", "set", "None", "''", "True", "False", "-1"]
                .get(a as usize)
                .copied()
                .unwrap_or("")
                .to_string()
        } else if op == IS_OP {
            if a != 0 { "is not" } else { "is" }.into()
        } else if op == CONTAINS_OP {
            if a != 0 { "not in" } else { "in" }.into()
        } else if op == SET_FUNCTION_ATTRIBUTE {
            FUNCTION_ATTR_NAMES.iter().find(|x| x.0 == a).map(|x| x.1.to_string()).unwrap_or_default()
        } else {
            String::new()
        };
        let argfield = if has_arg(op) { format!("{:>5}", a as i32) } else { String::new() };
        let rest = if argrepr.is_empty() { String::new() } else { format!(" ({})", argrepr) };
        let line = format!("{} {:>6}     {:<20} {}{}", line_field, label, name, argfield, rest);
        out.push_str(line.trim_end());
        out.push('\n');
    }
    if !excs.is_empty() {
        out.push_str("ExceptionTable:\n");
        for e in &excs {
            let l = |o: u32| label_of(o).map(|l| format!("L{}", l)).unwrap_or_else(|| o.to_string());
            out.push_str(&format!("  {} to {} -> {} [{}]{}\n", l(e.start), l(e.end), l(e.target), e.depth, if e.lasti { " lasti" } else { "" }));
        }
    }
}
