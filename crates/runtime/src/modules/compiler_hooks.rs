//! The compiler-stage hooks of `_testinternalcapi` (D24): `new_instruction_sequence`,
//! `compiler_codegen`, `optimize_cfg` and `assemble_code_object`. The Rust side works on
//! plain lists of `(opcode, oparg, lineno, col, end_lineno, end_col)` tuples whose jump
//! arguments are instruction indices; a small Python class provides the label-based
//! instruction-sequence object the tests build and inspect.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::{bool_, bytes, dict, float, int, list, none, str as pystr, tuple};
use rpy_compiler::hooks::{self, AsmMeta, Seq, SeqInstr};
use rpy_compiler::wordcode::{CodeObject, Constant};

fn instr_to_py(i: &SeqInstr) -> PyRef {
    let mut v = vec![int::new_int(i.op as i64), int::new_int(i.arg as i64)];
    for x in i.loc {
        v.push(int::new_int(x as i64));
    }
    tuple::new_tuple(v)
}

fn instrs_to_py(v: &[SeqInstr]) -> PyRef {
    list::new_list(v.iter().map(instr_to_py).collect())
}

fn instrs_from_py(o: &PyObject) -> PyResult<Vec<SeqInstr>> {
    if !list::is_list(o) {
        raise!(TypeError, "expected a list of instructions");
    }
    let mut out = Vec::new();
    for item in list::list_to_vec(o) {
        if !tuple::is_tuple(&item) || tuple::tuple_len(&item) != 6 {
            raise!(TypeError, "instructions must be 6-tuples");
        }
        let t = tuple::tuple_items(&item);
        let mut f = [0i64; 6];
        for k in 0..6 {
            f[k] = if t[k].is(none::none()) { 0 } else { ab::as_i64(t[k])? };
        }
        if !(0..=u16::MAX as i64).contains(&f[0]) {
            raise!(ValueError, "invalid opcode {}", f[0]);
        }
        out.push(SeqInstr { op: f[0] as u16, arg: f[1] as i32, loc: [f[2] as i32, f[3] as i32, f[4] as i32, f[5] as i32] });
    }
    Ok(out)
}

/// A Python constant as a compiler constant, if it has an exact representation.
fn obj_to_const(o: &PyObject) -> Option<Constant> {
    if o.is(none::none()) {
        return Some(Constant::None);
    }
    if o.is(bool_::true_obj()) {
        return Some(Constant::Bool(true));
    }
    if o.is(bool_::false_obj()) {
        return Some(Constant::Bool(false));
    }
    if o.is(none::ellipsis()) {
        return Some(Constant::Ellipsis);
    }
    if int::is_int_exact(o) {
        if let Some(v) = int::as_i64(o) {
            return Some(Constant::Int(v));
        }
        let s = ab::repr_string(o).ok()?;
        return Some(match s.strip_prefix('-') {
            Some(d) => Constant::BigInt { neg: true, decimal: d.to_string() },
            None => Constant::BigInt { neg: false, decimal: s },
        });
    }
    if float::is_float_exact(o) {
        return Some(Constant::Float(float::float_value(o)));
    }
    if pystr::is_str_exact(o) {
        return Some(Constant::Str(pystr::str_data(o).to_vec()));
    }
    if bytes::is_bytes_exact(o) {
        return Some(Constant::Bytes(bytes::bytes_data(o).to_vec()));
    }
    if tuple::is_tuple_exact(o) {
        let items: Option<Vec<Constant>> = tuple::tuple_items(o).iter().map(|x| obj_to_const(x)).collect();
        return Some(Constant::Tuple(items?));
    }
    None
}

/// A compiler constant as an object; `Opaque(i)` is `originals[i]`.
fn const_to_obj(c: &Constant, originals: &[PyRef], filename: &PyRef) -> PyResult {
    Ok(match c {
        Constant::Opaque(i) => match originals.get(*i as usize) {
            Some(o) => o.clone(),
            None => none::none_ref(),
        },
        Constant::Tuple(items) => {
            let mut v = Vec::with_capacity(items.len());
            for it in items {
                v.push(const_to_obj(it, originals, filename)?);
            }
            tuple::new_tuple(v)
        }
        Constant::FrozenSet(items) => {
            let mut v = Vec::with_capacity(items.len());
            for it in items {
                v.push(const_to_obj(it, originals, filename)?);
            }
            crate::types::set::new_frozenset_from_vec(v)?
        }
        Constant::Slice(parts) => {
            let a = const_to_obj(&parts[0], originals, filename)?;
            let b = const_to_obj(&parts[1], originals, filename)?;
            let s = const_to_obj(&parts[2], originals, filename)?;
            crate::types::slice::new_slice(&a, &b, &s)
        }
        // nested units of codegen-only output are stand-ins, not runnable code
        Constant::Code(co) if co.code.is_empty() => pystr::new_str(&format!("<code {}>", co.qualname)),
        other => crate::types::code::wconst_to_obj(other, filename)?,
    })
}

fn seq_to_py(seq: &Seq, filename: &PyRef) -> PyResult {
    let nested: PyResult<Vec<PyRef>> = seq.nested.iter().map(|s| seq_to_py(s, filename)).collect();
    let mut consts = Vec::with_capacity(seq.consts.len());
    for c in &seq.consts {
        consts.push(const_to_obj(c, &[], filename)?);
    }
    Ok(tuple::new_tuple(vec![instrs_to_py(&seq.instrs), list::new_list(nested?), list::new_list(consts)]))
}

/// `_codegen(ast, filename, optimize, compile_mode)` -> `(instrs, nested, consts)`
fn m_codegen(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.len() != 4 {
        raise!(TypeError, "_codegen expects 4 arguments");
    }
    let fname = pystr::to_string_lossy(args[1]);
    let optimize = ab::as_i64(args[2])? as i32;
    let mode = match ab::as_i64(args[3])? {
        0 => crate::compile::Mode::Exec,
        1 => crate::compile::Mode::Single,
        2 => crate::compile::Mode::Eval,
        _ => raise!(ValueError, "invalid compile mode"),
    };
    let m = crate::modules::_ast::from_py::py_to_mod(args[0], mode)?;
    crate::modules::ast_bridge::validate(&m, &fname)?;
    let mut copts = rpy_compiler::CompileOptions::new(&fname, mode.c());
    copts.optimize = if optimize < 0 { crate::modules::sys::optimize_flag() } else { optimize };
    let seq = match hooks::codegen(&m, &copts) {
        Ok(s) => s,
        Err(e) => return Err(crate::compile::compile_error(&e, &fname, None)),
    };
    seq_to_py(&seq, &pystr::new_str(&fname))
}

/// `_optimize(instrs, consts, nlocals)` -> instrs; `consts` (a list) is rewritten in place.
fn m_optimize(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.len() != 3 {
        raise!(TypeError, "_optimize expects 3 arguments");
    }
    let instrs = instrs_from_py(args[0])?;
    let consts_o = args[1];
    if !list::is_list(consts_o) {
        raise!(TypeError, "consts must be a list");
    }
    let nlocals = ab::as_i64(args[2])?.max(0) as usize;
    let originals = list::list_to_vec(consts_o);
    let mut consts: Vec<Constant> =
        originals.iter().enumerate().map(|(i, o)| obj_to_const(o).unwrap_or(Constant::Opaque(i as u32))).collect();
    let out = match hooks::optimize(&instrs, &mut consts, nlocals) {
        Ok(v) => v,
        Err(msg) => raise!(ValueError, "{}", msg),
    };
    let filename = pystr::new_str("<optimize_cfg>");
    let mut objs = Vec::with_capacity(consts.len());
    for c in &consts {
        objs.push(const_to_obj(c, &originals, &filename)?);
    }
    list::set_contents(consts_o, objs);
    Ok(instrs_to_py(&out))
}

fn str_list(o: &PyObject) -> PyResult<Vec<String>> {
    let mut v = Vec::new();
    for x in list::list_to_vec(o) {
        if !pystr::is_str(&x) {
            raise!(TypeError, "names must be strings");
        }
        v.push(pystr::to_string_lossy(&x));
    }
    Ok(v)
}

/// `_assemble(filename, instrs, name, qualname, firstlineno, argcount, posonlyargcount,
/// kwonlyargcount, consts, names, varnames, cellvars, freevars)` -> code object
fn m_assemble(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.len() != 13 {
        raise!(TypeError, "_assemble expects 13 arguments");
    }
    let filename = args[0].to_ref();
    let instrs = instrs_from_py(args[1])?;
    let consts = list::list_to_vec(args[8]);
    let meta = AsmMeta {
        name: pystr::to_string_lossy(args[2]),
        qualname: pystr::to_string_lossy(args[3]),
        filename: pystr::to_string_lossy(args[0]),
        firstlineno: ab::as_i64(args[4])?.max(0) as u32,
        argcount: ab::as_i64(args[5])?.max(0) as u32,
        posonlyargcount: ab::as_i64(args[6])?.max(0) as u32,
        kwonlyargcount: ab::as_i64(args[7])?.max(0) as u32,
        nconsts: consts.len(),
        names: str_list(args[9])?,
        varnames: str_list(args[10])?,
        cellvars: str_list(args[11])?,
        freevars: str_list(args[12])?,
        flags: 0x3, // optimized | newlocals
    };
    let co: CodeObject = match hooks::assemble(&instrs, meta) {
        Ok(c) => c,
        Err(msg) => raise!(SystemError, "{}", msg),
    };
    let mut objs = Vec::with_capacity(co.consts.len());
    for c in &co.consts {
        objs.push(const_to_obj(c, &consts, &filename)?);
    }
    crate::types::code::new_code_from_public(crate::types::code::PublicParts {
        argcount: co.argcount as i32,
        posonlyargcount: co.posonlyargcount as i32,
        kwonlyargcount: co.kwonlyargcount as i32,
        stacksize: co.stacksize as i32,
        flags: co.flags as i32,
        firstlineno: co.firstlineno as i32,
        code: bytes::new_bytes(&co.code),
        consts: objs,
        names: co.names.iter().map(|n| pystr::intern_str(n)).collect(),
        localsplusnames: co.localsplusnames.iter().map(|n| pystr::intern_str(n)).collect(),
        kinds: co.localspluskinds.clone(),
        filename,
        name: pystr::intern_str(&co.name),
        qualname: pystr::intern_str(&co.qualname),
        linetable: bytes::new_bytes(&co.linetable),
        exceptiontable: bytes::new_bytes(&co.exceptiontable),
    })
}

fn m_target_ops(_m: &PyObject) -> PyResult {
    let v: Vec<PyRef> = (0u16..=u16::MAX >> 6).filter(|&op| hooks::is_target_op(op)).map(|op| int::new_int(op as i64)).collect();
    Ok(tuple::new_tuple(v))
}

fn m_arg_ops(_m: &PyObject) -> PyResult {
    let v: Vec<PyRef> = (0u16..=u16::MAX >> 6).filter(|&op| rpy_compiler::wordcode::has_arg(op)).map(|op| int::new_int(op as i64)).collect();
    Ok(tuple::new_tuple(v))
}

const SRC: &str = r#"
_TARGET_OPS = frozenset(_TARGET_OPS)
_ARG_OPS = frozenset(_ARG_OPS)

class InstructionSequence:
    """A list of instructions with symbolic labels (compiler test hooks)."""

    def __init__(self):
        self._instrs = []
        self._labels = {}
        self._nested = []
        self._next_label = 0

    def new_label(self):
        lbl = self._next_label
        self._next_label += 1
        return lbl

    def use_label(self, label):
        self._labels[label] = len(self._instrs)
        if label >= self._next_label:
            self._next_label = label + 1

    def addop(self, opcode, oparg, lineno=-1, col_offset=-1,
              end_lineno=-1, end_col_offset=-1):
        self._instrs.append((opcode, oparg, lineno, col_offset,
                             end_lineno, end_col_offset))

    def add_nested(self, seq):
        self._nested.append(seq)

    def get_nested(self):
        return list(self._nested)

    def get_instructions(self):
        labels = self._labels
        result = []
        for op, arg, *loc in self._instrs:
            if op in _TARGET_OPS:
                arg = labels.get(arg, arg)
            elif op not in _ARG_OPS:
                arg = None
            result.append((op, arg, *loc))
        return result


def _seq_from_raw(instrs, nested=()):
    seq = InstructionSequence()
    seq._instrs = list(instrs)
    for sub in nested:
        seq.add_nested(_seq_from_raw(sub[0], sub[1]))
    return seq


def new_instruction_sequence():
    return InstructionSequence()


def compiler_codegen(ast, filename, optimize, compile_mode=0):
    instrs, nested, consts = _hook_codegen(ast, filename, optimize, compile_mode)
    return _seq_from_raw(instrs, nested), {'consts': consts}


def optimize_cfg(seq, consts, nlocals):
    if not isinstance(consts, list):
        raise TypeError("consts must be a list")
    if not isinstance(seq, InstructionSequence):
        raise TypeError("expected an instruction sequence")
    return _seq_from_raw(_hook_optimize(seq.get_instructions(), consts, nlocals))


def _ordered(mapping):
    if mapping is None:
        return []
    if isinstance(mapping, dict):
        return [k for k, v in sorted(mapping.items(), key=lambda kv: kv[1])]
    return list(mapping)


def assemble_code_object(filename, seq, metadata):
    if not isinstance(seq, InstructionSequence):
        raise TypeError("expected an instruction sequence")
    md = metadata
    return _hook_assemble(
        filename, seq.get_instructions(),
        md.get('name', ''), md.get('qualname', md.get('name', '')),
        md.get('firstlineno', 1), md.get('argcount', 0),
        md.get('posonlyargcount', 0), md.get('kwonlyargcount', 0),
        _ordered(md.get('consts')), _ordered(md.get('names')),
        _ordered(md.get('varnames')), _ordered(md.get('cellvars')),
        _ordered(md.get('freevars')))
"#;

/// Add the hooks to `_testinternalcapi`'s namespace `d`.
pub fn install(d: &PyObject, m: &PyObject) -> PyResult<()> {
    crate::builtins::add_methods_to_dict(
        d,
        crate::mdefs![
            ("_hook_codegen", FAST, m_codegen),
            ("_hook_optimize", FAST, m_optimize),
            ("_hook_assemble", FAST, m_assemble),
        ],
        Some(m),
    )?;
    let ops = m_target_ops(m)?;
    dict::set_item_str(d, "_TARGET_OPS", &ops)?;
    let ops = m_arg_ops(m)?;
    dict::set_item_str(d, "_ARG_OPS", &ops)?;
    let code = crate::compile::compile_str(SRC, "<_testinternalcapi>", crate::compile::Mode::Exec, 0, -1)?;
    crate::interp::eval_code(&code.code, d, Some(d))?;
    Ok(())
}
