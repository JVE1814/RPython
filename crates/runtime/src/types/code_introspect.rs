//! Code-object introspection helpers exposed by `_testinternalcapi` (test_code):
//! local-variable kinds, variable counts, "stateless" checks, return analysis.
//! They read the public form (localsplus kinds and the wordcode).

use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::*;
use crate::raise;
use crate::types::{bool_, code, dict, funcobject, int, str as pystr};
use rpy_compiler::bytecode::fast;
use rpy_compiler::wordcode::{self as wc, opcodes as o};

fn code_of(x: &PyObject) -> PyResult<(PyRef, Option<PyRef>)> {
    if code::is_code(x) {
        return Ok((x.to_ref(), None));
    }
    if funcobject::is_function(x) {
        let f = funcobject::func(x);
        return Ok((unsafe { PyRef::from_borrowed(f.func_code) }, Some(x.to_ref())));
    }
    raise!(TypeError, "expected a code object or a function, got {}", crate::types::typeobject::tname(x));
}

/// `get_co_localskinds(code)`: {name: CO_FAST_* kind} for every localsplus entry.
pub fn get_co_localskinds(_m: &PyObject, x: &PyObject) -> PyResult {
    if !code::is_code(x) {
        raise!(TypeError, "expected a code object");
    }
    let m = code::code_meta(x);
    let d = dict::new_dict();
    for (n, k) in m.localsplusnames.iter().zip(m.localspluskinds.iter()) {
        dict::set_item(&d, n, &int::new_int(*k as i64))?;
    }
    Ok(d)
}

/// Names the bytecode uses, by role: (globals, attributes, other names).
fn name_uses(co: &PyObject) -> (Vec<PyRef>, Vec<PyRef>, Vec<PyRef>) {
    let m = code::code_meta(co);
    let (mut g, mut a, mut u): (Vec<PyRef>, Vec<PyRef>, Vec<PyRef>) = (Vec::new(), Vec::new(), Vec::new());
    let add = |v: &mut Vec<PyRef>, i: u32| {
        if let Some(n) = m.names.get(i as usize) {
            if !v.iter().any(|x| x.is(n)) {
                v.push(n.clone());
            }
        }
    };
    if !m.public {
        return (g, a, u);
    }
    for ins in wc::instructions(crate::types::bytes::bytes_data(&m.co_code)) {
        let op = wc::deopt(ins.op as u16);
        match op {
            o::LOAD_GLOBAL => add(&mut g, ins.arg >> 1),
            o::STORE_GLOBAL | o::DELETE_GLOBAL => add(&mut g, ins.arg),
            o::LOAD_ATTR => add(&mut a, ins.arg >> 1),
            o::STORE_ATTR | o::DELETE_ATTR | o::IMPORT_FROM => add(&mut a, ins.arg),
            o::LOAD_SUPER_ATTR => add(&mut a, ins.arg >> 2),
            o::LOAD_NAME | o::STORE_NAME | o::DELETE_NAME | o::LOAD_FROM_DICT_OR_GLOBALS => add(&mut u, ins.arg),
            o::IMPORT_NAME => add(&mut u, ins.arg >> 2),
            _ => {}
        }
    }
    (g, a, u)
}

fn kv(d: &PyRef, k: &str, v: i64) -> PyResult<()> {
    dict::set_item_str(d, k, &int::new_int(v))
}

/// `get_code_var_counts(code_or_func, globalsns=None, builtinsns=None)`.
pub fn get_code_var_counts(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("get_code_var_counts", &["code", "globalsns", "builtinsns"], 0, 1);
    let [x, gns, bns] = crate::args::parse::<3>(&P, args, kw)?;
    let (co, func) = code_of(x.unwrap())?;
    let none = |o: Option<&PyObject>| o.filter(|v| !crate::types::none::is_none(v)).map(|v| v.to_ref());
    let mut globals = none(gns);
    let mut builtins = none(bns);
    if let Some(f) = &func {
        let fo = funcobject::func(f);
        if globals.is_none() {
            globals = Some(unsafe { PyRef::from_borrowed(fo.func_globals) });
        }
        if builtins.is_none() {
            builtins = Some(unsafe { PyRef::from_borrowed(fo.func_builtins) });
        }
    }
    let m = code::code_meta(&co);
    let (mut posonly, mut posorkw, mut kwonly, mut varargs, mut varkwargs) = (0, 0, 0, 0, 0);
    let (mut pure, mut argcells, mut othercells, mut free) = (0, 0, 0, 0);
    let (mut hidden, mut hpure, mut hcells) = (0, 0, 0);
    let mut nlocals = 0;
    for &k in &m.localspluskinds {
        if k & fast::FREE != 0 && k & (fast::LOCAL | fast::CELL) == 0 {
            free += 1;
            continue;
        }
        nlocals += 1;
        let is_arg = k & (fast::ARG_POS | fast::ARG_KW | fast::ARG_VAR) != 0;
        if k & fast::HIDDEN != 0 {
            hidden += 1;
            if k & fast::CELL != 0 { hcells += 1 } else { hpure += 1 }
        }
        if is_arg {
            if k & fast::ARG_VAR != 0 {
                if k & fast::ARG_POS != 0 { varargs += 1 } else { varkwargs += 1 }
            } else if k & fast::ARG_POS != 0 && k & fast::ARG_KW != 0 {
                posorkw += 1;
            } else if k & fast::ARG_POS != 0 {
                posonly += 1;
            } else {
                kwonly += 1;
            }
            if k & fast::CELL != 0 {
                argcells += 1;
            }
        } else if k & fast::CELL != 0 {
            othercells += 1;
        } else if k & fast::HIDDEN == 0 {
            pure += 1;
        }
    }
    let (gnames, attrs, unknown) = name_uses(&co);
    let (mut ng, mut nb, mut nu) = (0i64, 0i64, 0i64);
    for n in &gnames {
        let in_dict = |d: &Option<PyRef>| d.as_ref().is_some_and(|d| dict::is_dict(d) && dict::get_item_str(d, n).is_some());
        if in_dict(&globals) {
            ng += 1;
        } else if in_dict(&builtins) {
            nb += 1;
        } else {
            nu += 1;
        }
    }
    let nargs = posonly + posorkw + kwonly + varargs + varkwargs;
    let args_d = dict::new_dict();
    kv(&args_d, "total", nargs)?;
    kv(&args_d, "numposonly", posonly)?;
    kv(&args_d, "numposorkw", posorkw)?;
    kv(&args_d, "numkwonly", kwonly)?;
    kv(&args_d, "varargs", varargs)?;
    kv(&args_d, "varkwargs", varkwargs)?;
    let cells_d = dict::new_dict();
    kv(&cells_d, "total", argcells + othercells)?;
    kv(&cells_d, "numargs", argcells)?;
    kv(&cells_d, "numothers", othercells)?;
    let hidden_d = dict::new_dict();
    kv(&hidden_d, "total", hidden)?;
    kv(&hidden_d, "numpure", hpure)?;
    kv(&hidden_d, "numcells", hcells)?;
    let locals_d = dict::new_dict();
    kv(&locals_d, "total", nlocals)?;
    dict::set_item_str(&locals_d, "args", &args_d)?;
    kv(&locals_d, "numpure", pure)?;
    dict::set_item_str(&locals_d, "cells", &cells_d)?;
    dict::set_item_str(&locals_d, "hidden", &hidden_d)?;
    let glob_d = dict::new_dict();
    kv(&glob_d, "total", gnames.len() as i64)?;
    kv(&glob_d, "numglobal", ng)?;
    kv(&glob_d, "numbuiltin", nb)?;
    kv(&glob_d, "numunknown", nu)?;
    let unbound_total = gnames.len() + attrs.len() + unknown.len();
    let unbound_d = dict::new_dict();
    kv(&unbound_d, "total", unbound_total as i64)?;
    dict::set_item_str(&unbound_d, "globals", &glob_d)?;
    kv(&unbound_d, "numattrs", attrs.len() as i64)?;
    kv(&unbound_d, "numunknown", unknown.len() as i64)?;
    let out = dict::new_dict();
    kv(&out, "total", nlocals + free + unbound_total as i64)?;
    dict::set_item_str(&out, "locals", &locals_d)?;
    kv(&out, "numfree", free)?;
    dict::set_item_str(&out, "unbound", &unbound_d)?;
    Ok(out)
}

/// `verify_stateless_code(code_or_func, globalsns=None)`: raises ValueError unless the code
/// keeps no state of its own: no free variables; for a function, no closure and every
/// global it reads resolves to a builtin.
pub fn verify_stateless_code(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("verify_stateless_code", &["code", "globalsns"], 0, 1);
    let [x, gns] = crate::args::parse::<2>(&P, args, kw)?;
    let (co, func) = code_of(x.unwrap())?;
    let m = code::code_meta(&co);
    if m.localspluskinds.iter().any(|k| k & fast::FREE != 0) {
        raise!(ValueError, "code has free variables");
    }
    if let Some(f) = &func {
        let fo = funcobject::func(f);
        if !fo.func_closure.is_null() {
            raise!(ValueError, "function has a closure");
        }
        let globals = match gns {
            Some(g) if !crate::types::none::is_none(g) => g.to_ref(),
            _ => unsafe { PyRef::from_borrowed(fo.func_globals) },
        };
        let builtins = unsafe { PyRef::from_borrowed(fo.func_builtins) };
        let (gnames, _, _) = name_uses(&co);
        for n in &gnames {
            let in_globals = dict::is_dict(&globals) && dict::get_item_str(&globals, n).is_some();
            let in_builtins = dict::is_dict(&builtins) && dict::get_item_str(&builtins, n).is_some();
            if in_globals || !in_builtins {
                raise!(ValueError, "function uses global '{}'", pystr::to_string_lossy(n));
            }
        }
    }
    Ok(crate::types::none::none_ref())
}

/// `code_returns_only_none(code)`: every RETURN_VALUE returns a constant None.
pub fn code_returns_only_none(_m: &PyObject, x: &PyObject) -> PyResult {
    if !code::is_code(x) {
        raise!(TypeError, "expected a code object");
    }
    let m = code::code_meta(x);
    if !m.public {
        return Ok(bool_::new_bool(false));
    }
    let instrs = wc::instructions(crate::types::bytes::bytes_data(&m.co_code));
    let mut prev: Option<&wc::Instr> = None;
    for ins in &instrs {
        if wc::deopt(ins.op as u16) == o::RETURN_VALUE {
            let none_load = prev.is_some_and(|p| {
                let op = wc::deopt(p.op as u16);
                (op == o::LOAD_COMMON_CONSTANT && p.arg == 7)
                    || (op == o::LOAD_CONST && m.consts.get(p.arg as usize).is_some_and(|c| crate::types::none::is_none(c)))
            });
            if !none_load {
                return Ok(bool_::new_bool(false));
            }
        }
        prev = Some(ins);
    }
    Ok(bool_::new_bool(true))
}
