//! The `_symtable` module: the compiler's symbol table analysis
//! (`rpy_compiler::symtable`) as a tree of `symtable entry` objects for `Lib/symtable.py`.
//!
//! Each entry has the attributes `id`, `name`, `type` (TYPE_* constant), `lineno`,
//! `nested`, `symbols` (name -> raw flags, insertion ordered), `varnames` (parameter
//! names) and `children` (list of entries).

use crate::abstract_ as ab;
use crate::args::Params;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::fncommon as fc;
use crate::object::*;
use crate::types::{bool_, dict, int, list, none, str as pystr};
use crate::{name, raise};
use rpy_compiler::symtable as st;
use std::ffi::{c_int, c_void};

#[repr(C)]
struct Entry {
    ob_base: PyObject,
    dict: *mut PyObject,
}

static mut ENTRY_TYPE: PyTypeObject = PyTypeObject::ZERO;

unsafe extern "C" fn entry_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        fc::clear_field(&raw mut (*(o as *mut Entry)).dict);
        fc::free(o);
    }
}
unsafe extern "C" fn entry_traverse(o: *mut PyObject, v: visitproc, arg: *mut c_void) -> c_int {
    unsafe { fc::visit((*(o as *mut Entry)).dict, v, arg) }
}
unsafe extern "C" fn entry_clear(o: *mut PyObject) -> c_int {
    unsafe { fc::clear_field(&raw mut (*(o as *mut Entry)).dict) };
    0
}
unsafe extern "C" fn entry_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let o = &*o;
            let name = ab::str_string(&*ab::getattr(o, name!("name"))?)?;
            let id = ab::repr_string(&*ab::getattr(o, name!("id"))?)?;
            let line = ab::repr_string(&*ab::getattr(o, name!("lineno"))?)?;
            Ok(pystr::new_str(&format!("<symtable entry {}({}), line {}>", name, id, line)))
        })())
    }
}

fn entry_type() -> PyResult<*mut PyTypeObject> {
    unsafe {
        let t = &raw mut ENTRY_TYPE;
        if (*t).tp_flags & Py_TPFLAGS_READY == 0 {
            fc::init_type(t, c"symtable entry", std::mem::size_of::<Entry>(), Py_TPFLAGS_HAVE_GC);
            (*t).tp_dealloc = Some(entry_dealloc);
            (*t).tp_traverse = Some(entry_traverse);
            (*t).tp_clear = Some(entry_clear);
            (*t).tp_repr = Some(entry_repr);
            (*t).tp_dictoffset = std::mem::offset_of!(Entry, dict) as isize;
            (*t).tp_setattro = Some(crate::types::typeobject::object_generic_setattro);
            crate::types::typeobject::type_ready(t)?;
        }
        Ok(t)
    }
}

fn block_type(k: st::ScopeKind) -> i64 {
    match k {
        st::ScopeKind::Function => 0,
        st::ScopeKind::Class => 1,
        st::ScopeKind::Module => 2,
        st::ScopeKind::Annotation => 3,
        st::ScopeKind::TypeAlias => 4,
        st::ScopeKind::TypeParams => 5,
        st::ScopeKind::TypeVariable => 6,
    }
}

fn make_entry(table: &st::SymbolTable, id: usize) -> PyResult {
    let sc = &table.scopes[id];
    let tp = entry_type()?;
    let o = unsafe { PyRef::from_raw(fc::alloc(tp)?) };
    let d = crate::types::typeobject::instance_dict(&o).ok_or_else(|| err!(SystemError, "symtable entry without dict"))?;
    dict::set_item_str(&d, "id", &int::new_int(sc.id as i64))?;
    dict::set_item_str(&d, "name", &pystr::new_str(&sc.name))?;
    dict::set_item_str(&d, "type", &int::new_int(block_type(sc.kind)))?;
    dict::set_item_str(&d, "lineno", &int::new_int(sc.lineno as i64))?;
    dict::set_item_str(&d, "nested", &bool_::new_bool(sc.nested))?;
    let syms = dict::new_dict();
    for s in &sc.symbols {
        dict::set_item_str(&syms, &s.name, &int::new_int(s.raw_flags() as i64))?;
    }
    dict::set_item_str(&d, "symbols", &syms)?;
    dict::set_item_str(&d, "varnames", &list::new_list(sc.varnames.iter().map(|v| pystr::new_str(v)).collect()))?;
    let mut kids = Vec::with_capacity(sc.children.len());
    for &c in &sc.children {
        kids.push(make_entry(table, c)?);
    }
    dict::set_item_str(&d, "children", &list::new_list(kids))?;
    Ok(o)
}

/// symtable(source, filename, compile_type, *, module=None)
fn m_symtable(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("symtable", &["source", "filename", "compile_type", "module"], 3, 3).kwonly(1);
    let [src, fname, ctype, module] = crate::args::parse::<4>(&P, args, kw)?;
    let (src, fname, ctype) = (src.unwrap(), fname.unwrap(), ctype.unwrap());
    if !pystr::is_str(fname) && !crate::types::bytes::is_bytes(fname) && ab::getattr_opt(fname, name!("__fspath__"))?.is_none() {
        raise!(TypeError, "symtable() argument 'filename' must be str, bytes or os.PathLike, not {}", crate::types::typeobject::tname(fname));
    }
    let filename = crate::modules::os_fspath::fsdecode(fname)?;
    if !pystr::is_str(ctype) {
        raise!(TypeError, "symtable() argument 'compile_type' must be str, not {}", crate::types::typeobject::tname(ctype));
    }
    let mode = match pystr::to_string_lossy(ctype).as_str() {
        "exec" => crate::compile::Mode::Exec,
        "eval" => crate::compile::Mode::Eval,
        "single" => crate::compile::Mode::Single,
        _ => raise!(ValueError, "symtable() arg 3 must be 'exec' or 'eval' or 'single'"),
    };
    let module = module.filter(|m| !m.is(none::none()));
    let mut opts = crate::compile::Opts::new(&filename, mode, 0, 0);
    opts.module = module;
    let parsed = if pystr::is_str(src) {
        let s = pystr::as_utf8(src)?;
        if s.contains('\0') {
            raise!(SyntaxError, "source code string cannot contain null bytes");
        }
        crate::compile::parse_mod(crate::compile::Source::Str(s), &opts)?
    } else {
        match crate::types::bytes::bytes_like_data(src) {
            Some(b) => {
                if b.contains(&0) {
                    raise!(SyntaxError, "source code string cannot contain null bytes");
                }
                crate::compile::parse_mod(crate::compile::Source::Bytes(&b), &opts)?
            }
            None => raise!(TypeError, "symtable() arg 1 must be a string, bytes or AST object"),
        }
    };
    finish(parsed, &filename, mode)
}

fn finish(parsed: (rpy_parser::ast::Mod, String), filename: &str, mode: crate::compile::Mode) -> PyResult {
    let (module, text) = parsed;
    let copts = rpy_compiler::CompileOptions::new(filename.to_string(), mode.c());
    match rpy_compiler::symtable(&module, &copts) {
        Ok(t) => make_entry(&t, 0),
        Err(e) => Err(crate::compile::raise_compile_error(&e, filename, Some(&text))),
    }
}

pub fn init_module() -> PyResult {
    let methods = crate::mdefs![(
        "symtable",
        FASTKW,
        m_symtable,
        "symtable($module, source, filename, compile_type, /, *, module=None)\n--\n\nReturn symbol and scope dictionaries used internally by compiler."
    )];
    let (m, d) = fc::new_module("_symtable", methods, None)?;
    let consts: &[(&str, i64)] = &[
        ("USE", st::USE as i64),
        ("DEF_GLOBAL", st::DEF_GLOBAL as i64),
        ("DEF_NONLOCAL", st::DEF_NONLOCAL as i64),
        ("DEF_LOCAL", st::DEF_LOCAL as i64),
        ("DEF_PARAM", st::DEF_PARAM as i64),
        ("DEF_TYPE_PARAM", st::DEF_TYPE_PARAM as i64),
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
