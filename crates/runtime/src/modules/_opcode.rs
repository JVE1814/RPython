//! The `_opcode` module: metadata of the public CPython 3.15 instruction set (D24) for
//! `opcode.py` / `dis.py`. Numbering comes from `rpy_compiler::wordcode` (generated from the
//! public header opcode_ids.h).
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::{bool_, dict, int, list, none, str as pystr, tuple};
use rpy_compiler::wordcode::{self as wc, op as O};

/// Opcodes known to `opcode.opmap` (generic, instrumented and pseudo; not specialised).
fn in_opmap(code: i64) -> bool {
    if !wc::is_valid(code) {
        return false;
    }
    let c = code as u16;
    wc::specialization_base(c).is_none()
}

fn opcode_arg(o: &PyObject) -> PyResult<Option<u16>> {
    if !int::is_int(o) {
        raise!(TypeError, "'{}' object cannot be interpreted as an integer", crate::types::typeobject::tname(o));
    }
    let v = match ab::as_i64(o) {
        Ok(v) => v,
        Err(_) => {
            clear_exc();
            return Ok(None);
        }
    };
    Ok(if wc::is_valid(v) { Some(v as u16) } else { None })
}

fn pred(o: &PyObject, f: fn(u16) -> bool) -> PyResult {
    Ok(bool_::new_bool(opcode_arg(o)?.is_some_and(f)))
}

fn m_is_valid(_m: &PyObject, o: &PyObject) -> PyResult {
    pred(o, |_| true)
}
fn m_has_arg(_m: &PyObject, o: &PyObject) -> PyResult {
    pred(o, wc::has_arg)
}
fn m_has_const(_m: &PyObject, o: &PyObject) -> PyResult {
    pred(o, |c| c == O::LOAD_CONST)
}
fn m_has_name(_m: &PyObject, o: &PyObject) -> PyResult {
    pred(o, |c| wc::deopt(c) == c && wc::has_name(c))
}
fn m_has_jump(_m: &PyObject, o: &PyObject) -> PyResult {
    pred(o, |c| wc::deopt(c) == c && wc::has_jump(c))
}
fn m_has_free(_m: &PyObject, o: &PyObject) -> PyResult {
    pred(o, |c| wc::deopt(c) == c && wc::has_free(c))
}
fn m_has_local(_m: &PyObject, o: &PyObject) -> PyResult {
    pred(o, |c| wc::deopt(c) == c && wc::has_local(c))
}
fn m_has_exc(_m: &PyObject, o: &PyObject) -> PyResult {
    pred(o, wc::has_exc)
}

fn m_stack_effect(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("stack_effect", &["opcode", "oparg", "jump"], 1, 1).kwonly(1);
    let [opcode, oparg, jump] = crate::args::parse::<3>(&P, args, kw)?;
    let code = ab::as_i64(opcode.unwrap())?;
    if !in_opmap(code) || code as u16 >= O::MIN_INSTRUMENTED_OPCODE && (code as u16) < 256 {
        raise!(ValueError, "invalid opcode or oparg");
    }
    // the argument is optional (missing means 0) and ignored by opcodes without one
    let arg = match oparg {
        Some(a) if !a.is(none::none()) => ab::as_i64(a)? as i32,
        _ => 0,
    };
    let jump = match jump {
        Some(j) if !j.is(none::none()) => Some(ab::is_true(j)?),
        _ => None,
    };
    match wc::stack_effect(code as u16, arg, jump) {
        Some(e) => Ok(int::new_int(e as i64)),
        None => raise!(ValueError, "invalid opcode or oparg"),
    }
}

fn str_list(v: &[&str]) -> PyRef {
    list::new_list(v.iter().map(|s| pystr::new_str(s)).collect())
}

fn m_intrinsic1(_m: &PyObject) -> PyResult {
    Ok(str_list(&wc::INTRINSIC1))
}
fn m_intrinsic2(_m: &PyObject) -> PyResult {
    Ok(str_list(&wc::INTRINSIC2))
}
fn m_nb_ops(_m: &PyObject) -> PyResult {
    Ok(list::new_list(
        wc::NB_OPS.iter().map(|(n, s)| tuple::new_tuple(vec![pystr::new_str(n), pystr::new_str(s)])).collect(),
    ))
}
fn m_special_methods(_m: &PyObject) -> PyResult {
    Ok(str_list(&wc::SPECIAL_METHODS))
}

/// The objects LOAD_COMMON_CONSTANT pushes, in oparg order.
fn m_common_constants(_m: &PyObject) -> PyResult {
    let ex = crate::types::typeobject::type_ref(crate::types::exceptions::AssertionError());
    let nie = crate::types::typeobject::type_ref(crate::types::exceptions::NotImplementedError());
    let b = |name: &str| -> PyResult { crate::builtins::builtin(name) };
    Ok(list::new_list(vec![
        ex,
        nie,
        b("tuple")?,
        b("all")?,
        b("any")?,
        b("list")?,
        b("set")?,
        none::none_ref(),
        pystr::new_str(""),
        bool_::new_bool(true),
        bool_::new_bool(false),
        int::new_int(-1),
    ]))
}

/// (name, number) of every opcode, including specialised and pseudo ones.
fn m_opcodes(_m: &PyObject) -> PyResult {
    Ok(list::new_list(
        wc::op::ALL.iter().map(|(n, c)| tuple::new_tuple(vec![pystr::new_str(n), int::new_int(*c as i64)])).collect(),
    ))
}

/// {generic name: [specialised names]}.
fn m_specializations(_m: &PyObject) -> PyResult {
    let d = dict::new_dict();
    for (base, specs) in wc::specializations() {
        let names: Vec<&str> = specs.iter().filter_map(|s| wc::opname(*s)).collect();
        dict::set_item_str(&d, wc::opname(base).unwrap_or("?"), &str_list(&names))?;
    }
    Ok(d)
}

/// {name: [(cache field, size in code units)]} for opcodes with inline caches.
fn m_cache_format(_m: &PyObject) -> PyResult {
    let d = dict::new_dict();
    for &(name, code) in wc::op::ALL.iter() {
        if code >= 256 || wc::deopt(code) != code {
            continue;
        }
        let f = wc::cache_format(code);
        if f.is_empty() {
            continue;
        }
        let items: Vec<PyRef> =
            f.iter().map(|(n, s)| tuple::new_tuple(vec![pystr::new_str(n), int::new_int(*s as i64)])).collect();
        dict::set_item_str(&d, name, &list::new_list(items))?;
    }
    Ok(d)
}

fn m_get_executor(_m: &PyObject, _args: &[&PyObject]) -> PyResult {
    raise!(RuntimeError, "no executor at this offset");
}

fn m_specialization_stats(_m: &PyObject) -> PyResult {
    Ok(none::none_ref())
}

pub fn init_module() -> PyResult {
    let m = crate::types::module::new_module("_opcode")?;
    let d = crate::types::module::module_dict(&m);
    let methods = crate::mdefs![
        ("is_valid", O, m_is_valid, "is_valid($module, /, opcode)\n--\n\nReturn True if opcode is valid, False otherwise."),
        ("has_arg", O, m_has_arg, "has_arg($module, /, opcode)\n--\n\nReturn True if the opcode uses its oparg, False otherwise."),
        ("has_const", O, m_has_const, "has_const($module, /, opcode)\n--\n\nReturn True if the opcode accesses a constant, False otherwise."),
        ("has_name", O, m_has_name, "has_name($module, /, opcode)\n--\n\nReturn True if the opcode accesses an attribute by name, False otherwise."),
        ("has_jump", O, m_has_jump, "has_jump($module, /, opcode)\n--\n\nReturn True if the opcode has a jump target, False otherwise."),
        ("has_free", O, m_has_free, "has_free($module, /, opcode)\n--\n\nReturn True if the opcode accesses a free variable, False otherwise."),
        ("has_local", O, m_has_local, "has_local($module, /, opcode)\n--\n\nReturn True if the opcode accesses a local variable, False otherwise."),
        ("has_exc", O, m_has_exc, "has_exc($module, /, opcode)\n--\n\nReturn True if the opcode sets an exception handler, False otherwise."),
        ("stack_effect", FASTKW, m_stack_effect, "stack_effect($module, opcode, oparg=None, /, *, jump=None)\n--\n\nCompute the stack effect of the opcode."),
        ("get_intrinsic1_descs", NOARGS, m_intrinsic1),
        ("get_intrinsic2_descs", NOARGS, m_intrinsic2),
        ("get_nb_ops", NOARGS, m_nb_ops),
        ("get_special_method_names", NOARGS, m_special_methods),
        ("get_common_constants", NOARGS, m_common_constants),
        ("get_specialization_stats", NOARGS, m_specialization_stats),
        ("get_executor", FAST, m_get_executor, "get_executor($module, code, offset, /)\n--\n\nReturn the executor object at offset in code if exists."),
        ("_opcodes", NOARGS, m_opcodes),
        ("_specializations", NOARGS, m_specializations),
        ("_cache_format", NOARGS, m_cache_format),
    ];
    crate::builtins::add_methods_to_dict(&d, methods, Some(&m))?;
    dict::set_item_str(&d, "ENABLE_SPECIALIZATION", &bool_::new_bool(true))?;
    dict::set_item_str(&d, "ENABLE_SPECIALIZATION_FT", &bool_::new_bool(true))?;
    Ok(m)
}
