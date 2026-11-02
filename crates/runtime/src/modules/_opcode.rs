//! The `_opcode` module: describes OUR bytecode (rpy_compiler::bytecode) for
//! `opcode.py` / `dis.py`. Instructions are 32-bit little-endian words:
//! bits 0..8 opcode, bits 8..32 argument; jump arguments are absolute
//! instruction indices.
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::{bool_, dict, int, list, none, str as pystr, tuple};
use rpy_compiler::bytecode::{self as bc, Op};

fn camel_to_upper(s: &str) -> String {
    let mut out = String::new();
    let mut prev: Option<char> = None;
    for (i, c) in s.chars().enumerate() {
        if (c.is_ascii_uppercase() && i > 0) || (c.is_ascii_digit() && prev.is_some_and(|p| p.is_ascii_alphabetic())) {
            out.push('_');
        }
        prev = Some(c);
        out.push(c.to_ascii_uppercase());
    }
    out
}

pub fn op_name(op: Op) -> String {
    camel_to_upper(&format!("{:?}", op))
}

fn ops() -> impl Iterator<Item = Op> {
    (0..bc::NUM_OPS as u8).map(Op::from_u8)
}

pub fn has_arg(op: Op) -> bool {
    use Op::*;
    !matches!(
        op,
        Nop | PopTop
            | PushNull
            | LoadLocals
            | BinarySubscr
            | StoreSubscr
            | DeleteSubscr
            | BinarySlice
            | StoreSlice
            | UnaryNegative
            | UnaryPositive
            | UnaryInvert
            | UnaryNot
            | ToBool
            | GetIter
            | GetAiter
            | GetAnext
            | EndAsyncFor
            | GetYieldFromIter
            | CleanupThrow
            | ReturnValue
            | CallFunctionEx
            | MakeFunction
            | LoadBuildClass
            | PushExcInfo
            | PopExcept
            | CheckExcMatch
            | CheckEgMatch
            | WithExceptStart
            | BeforeWith
            | BeforeAsyncWith
            | SetupAnnotations
            | FormatSimple
            | FormatWithSpec
            | BuildTemplate
            | MatchMapping
            | MatchSequence
            | MatchKeys
            | GetLen
            | Unused1
            | Unused2
            | Unused3
            | LoadSpecial
            | ReturnGenerator
    )
}

pub fn has_const(op: Op) -> bool {
    matches!(op, Op::LoadConst)
}

pub fn has_name(op: Op) -> bool {
    use Op::*;
    matches!(
        op,
        LoadName
            | StoreName
            | DeleteName
            | LoadGlobal
            | StoreGlobal
            | DeleteGlobal
            | LoadFromDictOrGlobals
            | LoadAttr
            | LoadMethod
            | StoreAttr
            | DeleteAttr
            | LoadSuperAttr
            | ImportName
            | ImportFrom
    )
}

pub fn has_local(op: Op) -> bool {
    use Op::*;
    matches!(op, LoadFast | LoadFastCheck | LoadFastAndClear | StoreFast | DeleteFast)
}

pub fn has_free(op: Op) -> bool {
    use Op::*;
    matches!(op, MakeCell | LoadDeref | StoreDeref | DeleteDeref | LoadFromDictOrDeref)
}

/// Stack effect (same contract as the compiler's assembler).
pub fn stack_effect(op: Op, arg: u32, jump: bool) -> i32 {
    use Op::*;
    let n = arg as i32;
    match op {
        Nop | Resume | ExtendedArg => 0,
        PopTop => -1,
        PushNull | Copy => 1,
        Swap => 0,
        LoadConst | LoadFast | LoadFastCheck | LoadFastAndClear => 1,
        StoreFast => -1,
        DeleteFast | MakeCell | CopyFreeVars => 0,
        LoadDeref => 1,
        StoreDeref => -1,
        DeleteDeref => 0,
        LoadFromDictOrDeref | LoadFromDictOrGlobals => 0,
        LoadLocals | LoadName | LoadGlobal => 1,
        StoreName | StoreGlobal => -1,
        DeleteName | DeleteGlobal => 0,
        LoadAttr => 0,
        LoadMethod => 1,
        StoreAttr => -2,
        DeleteAttr => -1,
        LoadSuperAttr => {
            if arg & 1 != 0 {
                -1
            } else {
                -2
            }
        }
        BinaryOp => -1,
        UnaryNegative | UnaryPositive | UnaryInvert | UnaryNot | ToBool => 0,
        CompareOp | IsOp | ContainsOp | BinarySubscr => -1,
        StoreSubscr => -3,
        DeleteSubscr => -2,
        BinarySlice => -2,
        StoreSlice => -4,
        BuildSlice => 1 - n,
        BuildTuple | BuildList | BuildSet | BuildString => 1 - n,
        BuildMap => 1 - 2 * n,
        ListAppend | SetAdd => -1,
        MapAdd => -2,
        ListExtend | SetUpdate | DictUpdate | DictMerge => -1,
        UnpackSequence => n - 1,
        UnpackEx => ((arg & 0xff) + (arg >> 8)) as i32,
        Jump => 0,
        PopJumpIfFalse | PopJumpIfTrue | PopJumpIfNone | PopJumpIfNotNone => -1,
        JumpIfFalseOrPop | JumpIfTrueOrPop => {
            if jump {
                0
            } else {
                -1
            }
        }
        GetIter | GetAiter => 0,
        ForIter => {
            if jump {
                -1
            } else {
                1
            }
        }
        GetAnext => 1,
        EndAsyncFor => -2,
        GetAwaitable | GetYieldFromIter => 0,
        Send => {
            if jump {
                -1
            } else {
                0
            }
        }
        YieldValue => 0,
        CleanupThrow => -1,
        ReturnValue => -1,
        Call => -n - 1,
        CallKw => -n - 2,
        CallFunctionEx => -3,
        CallIntrinsic1 => 0,
        CallIntrinsic2 => -1,
        MakeFunction => 0,
        SetFunctionAttribute => -1,
        LoadBuildClass => 1,
        RaiseVarargs => -n,
        Reraise => {
            if arg != 0 {
                -2
            } else {
                -1
            }
        }
        PushExcInfo => 1,
        PopExcept => -1,
        CheckExcMatch | CheckEgMatch => 0,
        WithExceptStart => 1,
        BeforeWith | BeforeAsyncWith => 2,
        SetupAnnotations => 0,
        ImportName => -1,
        ImportFrom => 1,
        FormatSimple | ConvertValue => 0,
        FormatWithSpec => -1,
        BuildTemplate => -1,
        BuildInterpolation => -1 - (arg & 1) as i32,
        MatchClass => -2,
        MatchMapping | MatchSequence | MatchKeys | GetLen => 1,
        LoadCommonConstant => 1,
        Unused1 => -1,
        Unused2 => 0,
        Unused3 => -1,
        LoadSpecial => 1,
        ReturnGenerator => 0,
    }
}

fn op_arg(o: &PyObject) -> PyResult<Option<Op>> {
    let v = ab::as_i64(o)?;
    if v < 0 || v >= bc::NUM_OPS as i64 {
        return Ok(None);
    }
    Ok(Some(Op::from_u8(v as u8)))
}

fn pred(o: &PyObject, f: fn(Op) -> bool) -> PyResult {
    if !int::is_int(o) {
        raise!(TypeError, "'{}' object cannot be interpreted as an integer", crate::types::typeobject::tname(o));
    }
    Ok(bool_::new_bool(op_arg(o)?.map_or(false, f)))
}

fn m_is_valid(_m: &PyObject, o: &PyObject) -> PyResult {
    pred(o, |_| true)
}
fn m_has_arg(_m: &PyObject, o: &PyObject) -> PyResult {
    pred(o, has_arg)
}
fn m_has_const(_m: &PyObject, o: &PyObject) -> PyResult {
    pred(o, has_const)
}
fn m_has_name(_m: &PyObject, o: &PyObject) -> PyResult {
    pred(o, has_name)
}
fn m_has_jump(_m: &PyObject, o: &PyObject) -> PyResult {
    pred(o, |op| op.is_jump())
}
fn m_has_free(_m: &PyObject, o: &PyObject) -> PyResult {
    pred(o, has_free)
}
fn m_has_local(_m: &PyObject, o: &PyObject) -> PyResult {
    pred(o, has_local)
}
fn m_has_exc(_m: &PyObject, o: &PyObject) -> PyResult {
    pred(o, |_| false)
}

fn m_stack_effect(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("stack_effect", &["opcode", "oparg", "jump"], 1, 1).kwonly(1);
    let [opcode, oparg, jump] = crate::args::parse::<3>(&P, args, kw)?;
    let op = match op_arg(opcode.unwrap())? {
        Some(op) => op,
        None => raise!(ValueError, "invalid opcode or oparg"),
    };
    // the argument is optional for every opcode (missing means 0) and
    // ignored by those that take none
    let arg = match oparg {
        Some(a) if !a.is(none::none()) => ab::as_i64(a)? as u32,
        _ => 0,
    };
    let jump = match jump {
        Some(j) if !j.is(none::none()) => Some(ab::is_true(j)?),
        _ => None,
    };
    let e = match jump {
        Some(j) => stack_effect(op, arg, j),
        None => stack_effect(op, arg, true).max(stack_effect(op, arg, false)),
    };
    Ok(int::new_int(e as i64))
}

fn str_list(v: Vec<String>) -> PyRef {
    list::new_list(v.into_iter().map(|s| pystr::new_str(&s)).collect())
}

fn m_intrinsic1(_m: &PyObject) -> PyResult {
    let mut names = vec!["INTRINSIC_1_INVALID".to_string()];
    for n in [
        "PRINT",
        "IMPORT_STAR",
        "STOPITERATION_ERROR",
        "UNARY_POSITIVE",
        "LIST_TO_TUPLE",
        "TYPEVAR",
        "PARAMSPEC",
        "TYPEVARTUPLE",
        "SUBSCRIPT_GENERIC",
        "TYPEALIAS",
        "ASYNC_GEN_WRAP",
    ] {
        names.push(format!("INTRINSIC_{}", n));
    }
    Ok(str_list(names))
}

fn m_intrinsic2(_m: &PyObject) -> PyResult {
    let mut names = vec!["INTRINSIC_2_INVALID".to_string()];
    for n in ["PREP_RERAISE_STAR", "TYPEVAR_WITH_BOUND", "TYPEVAR_WITH_CONSTRAINTS", "SET_FUNCTION_TYPE_PARAMS", "SET_TYPEPARAM_DEFAULT"] {
        names.push(format!("INTRINSIC_{}", n));
    }
    Ok(str_list(names))
}

fn m_nb_ops(_m: &PyObject) -> PyResult {
    let ops = [
        ("NB_ADD", "+"),
        ("NB_AND", "&"),
        ("NB_FLOOR_DIVIDE", "//"),
        ("NB_LSHIFT", "<<"),
        ("NB_MATRIX_MULTIPLY", "@"),
        ("NB_MULTIPLY", "*"),
        ("NB_REMAINDER", "%"),
        ("NB_OR", "|"),
        ("NB_POWER", "**"),
        ("NB_RSHIFT", ">>"),
        ("NB_SUBTRACT", "-"),
        ("NB_TRUE_DIVIDE", "/"),
        ("NB_XOR", "^"),
        ("NB_INPLACE_ADD", "+="),
        ("NB_INPLACE_AND", "&="),
        ("NB_INPLACE_FLOOR_DIVIDE", "//="),
        ("NB_INPLACE_LSHIFT", "<<="),
        ("NB_INPLACE_MATRIX_MULTIPLY", "@="),
        ("NB_INPLACE_MULTIPLY", "*="),
        ("NB_INPLACE_REMAINDER", "%="),
        ("NB_INPLACE_OR", "|="),
        ("NB_INPLACE_POWER", "**="),
        ("NB_INPLACE_RSHIFT", ">>="),
        ("NB_INPLACE_SUBTRACT", "-="),
        ("NB_INPLACE_TRUE_DIVIDE", "/="),
        ("NB_INPLACE_XOR", "^="),
    ];
    Ok(list::new_list(ops.iter().map(|(a, b)| tuple::new_tuple(vec![pystr::new_str(a), pystr::new_str(b)])).collect()))
}

fn m_special_methods(_m: &PyObject) -> PyResult {
    Ok(str_list(["__enter__", "__exit__", "__aenter__", "__aexit__"].iter().map(|s| s.to_string()).collect()))
}

fn m_common_constants(_m: &PyObject) -> PyResult {
    // The values LOAD_COMMON_CONSTANT can push, in argument order.
    Ok(str_list(["AssertionError", "NotImplementedError", "tuple", "all", "any"].iter().map(|s| s.to_string()).collect()))
}

/// Exception table of a code object: list of (start, end, target, depth, lasti)
/// with instruction indices.
fn m_exception_table(_m: &PyObject, code: &PyObject) -> PyResult {
    if !crate::types::code::is_code(code) {
        raise!(TypeError, "expected a code object");
    }
    let d = crate::types::code::code_data(code);
    let items = d
        .exception_table
        .iter()
        .map(|e| {
            tuple::new_tuple(vec![
                int::new_int(e.start as i64),
                int::new_int(e.end as i64),
                int::new_int(e.target as i64),
                int::new_int(e.depth as i64),
                bool_::new_bool(e.lasti),
            ])
        })
        .collect();
    Ok(list::new_list(items))
}

/// Names of the localsplus slots of a code object (locals, cells, free vars).
fn m_localsplusnames(_m: &PyObject, code: &PyObject) -> PyResult {
    if !crate::types::code::is_code(code) {
        raise!(TypeError, "expected a code object");
    }
    let d = crate::types::code::code_data(code);
    Ok(tuple::new_tuple(d.localsplusnames.clone()))
}

fn m_get_executor(_m: &PyObject, _args: &[&PyObject]) -> PyResult {
    raise!(RuntimeError, "no executor at this offset");
}

pub fn init_module() -> PyResult {
    let m = crate::types::module::new_module("_opcode")?;
    let d = crate::types::module::module_dict(&m);
    let methods = crate::mdefs![
        ("is_valid", O, m_is_valid),
        ("has_arg", O, m_has_arg),
        ("has_const", O, m_has_const),
        ("has_name", O, m_has_name),
        ("has_jump", O, m_has_jump),
        ("has_free", O, m_has_free),
        ("has_local", O, m_has_local),
        ("has_exc", O, m_has_exc),
        ("stack_effect", FASTKW, m_stack_effect, "stack_effect($module, opcode, oparg=None, /, *, jump=None)\n--\n\nReturn the stack effect of an opcode with the given argument."),
        ("get_intrinsic1_descs", NOARGS, m_intrinsic1),
        ("get_intrinsic2_descs", NOARGS, m_intrinsic2),
        ("get_nb_ops", NOARGS, m_nb_ops),
        ("get_special_method_names", NOARGS, m_special_methods),
        ("get_common_constants", NOARGS, m_common_constants),
        ("get_executor", FAST, m_get_executor, "get_executor($module, code, offset, /)\n--\n\nReturn the JIT executor attached at an instruction offset."),
        ("_exception_table", O, m_exception_table),
        ("_localsplusnames", O, m_localsplusnames),
    ];
    crate::builtins::add_methods_to_dict(&d, methods, Some(&m))?;
    let names: Vec<String> = ops().map(op_name).collect();
    dict::set_item_str(&d, "_opnames", &str_list(names))?;
    dict::set_item_str(&d, "ENABLE_SPECIALIZATION", &bool_::new_bool(false))?;
    dict::set_item_str(&d, "ENABLE_SPECIALIZATION_FT", &bool_::new_bool(false))?;
    dict::set_item_str(&d, "_INSTRUCTION_SIZE", &int::new_int(4))?;
    Ok(m)
}
