//! The `_symtable` module: exposes the compiler's symbol table analysis
//! (rpy_compiler::symtable) to `Lib/symtable.py`.
//!
//! `symtable(source, filename, compile_type)` returns the top-level entry as a
//! dict tree: {'type', 'id', 'name', 'lineno', 'col_offset', 'end_lineno',
//! 'end_col_offset', 'nested', 'children', 'symbols', 'varnames',
//! 'is_generator', 'is_coroutine', 'comp_inlined', 'is_comprehension'}.
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::{bool_, dict, int, list, str as pystr};
use rpy_compiler::symtable as st;

fn entry(table: &st::SymbolTable, id: usize) -> PyResult {
    let sc = &table.scopes[id];
    let d = dict::new_dict();
    let kind = match sc.kind {
        st::ScopeKind::Function => 0,
        st::ScopeKind::Class => 1,
        st::ScopeKind::Module => 2,
        st::ScopeKind::Annotation => 3,
        st::ScopeKind::TypeAlias => 4,
        st::ScopeKind::TypeParams => 5,
        st::ScopeKind::TypeVariable => 6,
    };
    dict::set_item_str(&d, "type", &int::new_int(kind))?;
    dict::set_item_str(&d, "id", &int::new_int(sc.id as i64))?;
    dict::set_item_str(&d, "name", &pystr::new_str(&sc.name))?;
    dict::set_item_str(&d, "lineno", &int::new_int(sc.lineno as i64))?;
    dict::set_item_str(&d, "col_offset", &int::new_int(sc.col_offset as i64))?;
    dict::set_item_str(&d, "end_lineno", &int::new_int(sc.end_lineno as i64))?;
    dict::set_item_str(&d, "end_col_offset", &int::new_int(sc.end_col_offset as i64))?;
    dict::set_item_str(&d, "nested", &bool_::new_bool(sc.nested))?;
    dict::set_item_str(&d, "is_generator", &bool_::new_bool(sc.generator))?;
    dict::set_item_str(&d, "is_coroutine", &bool_::new_bool(sc.coroutine))?;
    dict::set_item_str(&d, "comp_inlined", &bool_::new_bool(sc.comp_inlined))?;
    dict::set_item_str(&d, "is_comprehension", &bool_::new_bool(sc.comprehension != st::CompKind::None))?;
    let syms = dict::new_dict();
    for s in &sc.symbols {
        dict::set_item_str(&syms, &s.name, &int::new_int(s.raw_flags() as i64))?;
    }
    dict::set_item_str(&d, "symbols", &syms)?;
    dict::set_item_str(&d, "varnames", &list::new_list(sc.varnames.iter().map(|v| pystr::new_str(v)).collect()))?;
    let mut children = Vec::new();
    for &c in &sc.children {
        children.push(entry(table, c)?);
    }
    dict::set_item_str(&d, "children", &list::new_list(children))?;
    Ok(d)
}

fn m_symtable(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("symtable", args, 3, 3)?;
    let fname = crate::modules::os_fspath::fsdecode(args[1])?;
    let mode_s = ab::str_string(args[2])?;
    let mode = match crate::compile::Mode::from_str(&mode_s) {
        Some(m) if mode_s != "func_type" => m,
        _ => raise!(ValueError, "symtable() arg 3 must be 'exec' or 'eval' or 'single'"),
    };
    let src = if pystr::is_str(args[0]) {
        pystr::as_utf8(args[0])?.as_bytes().to_vec()
    } else {
        match crate::types::bytes::bytes_like_data(args[0]) {
            Some(b) => b,
            None => raise!(TypeError, "symtable() arg 1 must be a string, bytes or AST object"),
        }
    };
    let module = crate::compile::parse_to_ast(&src, &fname, mode, 0, None)?;
    let opts = rpy_compiler::CompileOptions::new(fname.clone(), match mode {
        crate::compile::Mode::Eval => rpy_compiler::Mode::Eval,
        crate::compile::Mode::Single => rpy_compiler::Mode::Single,
        _ => rpy_compiler::Mode::Exec,
    });
    let table = match rpy_compiler::symtable(&module, &opts) {
        Ok(t) => t,
        Err(e) => {
            let text = String::from_utf8_lossy(&src).into_owned();
            return Err(crate::compile::compile_error(&e, &fname, Some(&text)));
        }
    };
    entry(&table, 0)
}

pub fn init_module() -> PyResult {
    let m = crate::types::module::new_module("_symtable")?;
    let d = crate::types::module::module_dict(&m);
    let methods = crate::mdefs![("symtable", FAST, m_symtable, "Return symbol and scope dictionaries used internally by compiler.")];
    crate::builtins::add_methods_to_dict(&d, methods, Some(&m))?;
    let consts: &[(&str, i64)] = &[
        ("USE", st::USE as i64),
        ("DEF_GLOBAL", st::DEF_GLOBAL as i64),
        ("DEF_NONLOCAL", st::DEF_NONLOCAL as i64),
        ("DEF_LOCAL", st::DEF_LOCAL as i64),
        ("DEF_PARAM", st::DEF_PARAM as i64),
        ("DEF_TYPE_PARAM", st::DEF_TYPE_PARAM as i64),
        ("DEF_FREE", st::DEF_FREE as i64),
        ("DEF_FREE_CLASS", st::DEF_FREE_CLASS as i64),
        ("DEF_IMPORT", st::DEF_IMPORT as i64),
        ("DEF_BOUND", st::DEF_BOUND as i64),
        ("DEF_ANNOT", st::DEF_ANNOT as i64),
        ("DEF_COMP_ITER", st::DEF_COMP_ITER as i64),
        ("DEF_COMP_CELL", st::DEF_COMP_CELL as i64),
        ("TYPE_FUNCTION", 0),
        ("TYPE_CLASS", 1),
        ("TYPE_MODULE", 2),
        ("TYPE_ANNOTATION", 3),
        ("TYPE_TYPE_ALIAS", 4),
        ("TYPE_TYPE_PARAMETERS", 5),
        ("TYPE_TYPE_VARIABLE", 6),
        ("LOCAL", st::SymScope::Local as i64),
        ("GLOBAL_EXPLICIT", st::SymScope::GlobalExplicit as i64),
        ("GLOBAL_IMPLICIT", st::SymScope::GlobalImplicit as i64),
        ("FREE", st::SymScope::Free as i64),
        ("CELL", st::SymScope::Cell as i64),
        ("SCOPE_OFF", st::SCOPE_OFFSET as i64),
        ("SCOPE_MASK", 0xf),
    ];
    for (k, v) in consts {
        dict::set_item_str(&d, k, &int::new_int(*v))?;
    }
    Ok(m)
}
