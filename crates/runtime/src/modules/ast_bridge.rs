//! builtin compile(): source/AST -> code object or AST, with the CPython 3.15 flag handling
//! (PyCF_ONLY_AST, PyCF_OPTIMIZED_AST, PyCF_TYPE_COMMENTS, `_feature_version`, `module=`),
//! AST validation and future-flag inheritance.

use crate::abstract_ as ab;
use crate::args::Params;
use crate::compile::{Mode, Opts, Source};
use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::*;
use crate::types::{bytes, none, str as pystr};
use crate::raise;
use rpy_compiler::cf;

pub fn is_ast_node(o: &PyObject) -> bool {
    crate::modules::_ast::is_ast_node(o)
}

const PYCF_MASK_OBSOLETE: u32 = 0x0010; // CO_NESTED
const PYCF_COMPILE_MASK: u32 = cf::PYCF_ONLY_AST
    | cf::PYCF_SOURCE_IS_UTF8
    | cf::PYCF_DONT_IMPLY_DEDENT
    | cf::PYCF_IGNORE_COOKIE
    | cf::PYCF_TYPE_COMMENTS
    | cf::PYCF_ALLOW_TOP_LEVEL_AWAIT
    | cf::PYCF_ALLOW_INCOMPLETE_INPUT
    | cf::PYCF_OPTIMIZED_AST;

/// Future flags of the currently executing code object (PyEval_MergeCompilerFlags).
pub fn inherited_future_flags() -> u32 {
    let f = crate::interp::current_frame_ptr();
    if f.is_null() {
        return 0;
    }
    unsafe {
        let code = (*f).code;
        if code.is_null() || !crate::types::code::is_code(&*code) {
            return 0;
        }
        crate::types::code::code(&*code).flags() & cf::PYCF_MASK
    }
}

/// Source text/bytes of compile()'s first argument.
enum Src {
    Text(String),
    Bytes(Vec<u8>),
}

fn source_of(o: &PyObject, fname: &str) -> PyResult<Src> {
    if pystr::is_str(o) {
        // (capi3) through the str's cached UTF-8 form, which compiling creates as in
        // CPython (sys.getsizeof counts it); lone surrogates raise here
        let s = pystr::as_utf8(o)?.to_string();
        if s.contains('\0') {
            raise!(SyntaxError, "source code string cannot contain null bytes");
        }
        return Ok(Src::Text(s));
    }
    match bytes::bytes_like_data(o) {
        Some(b) => {
            if b.contains(&0) {
                raise!(SyntaxError, "source code string cannot contain null bytes");
            }
            Ok(Src::Bytes(b))
        }
        None => raise!(TypeError, "{}() arg 1 must be a string, bytes or AST object", fname),
    }
}

/// compile(source, filename, mode, flags=0, dont_inherit=False, optimize=-1, *,
///         module=None, _feature_version=-1)
pub fn builtin_compile(args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new(
        "compile",
        &["source", "filename", "mode", "flags", "dont_inherit", "optimize", "module", "_feature_version"],
        0,
        3,
    )
    .kwonly(2);
    let [src, fname, mode, flags, dont_inherit, optimize, module, fv] = crate::args::parse::<8>(&P, args, kw)?;
    let src = src.unwrap();
    let fname = crate::modules::os_fspath::fsdecode(fname.unwrap())?;
    let mut flags = match flags {
        Some(f) => {
            let v = ab::as_i64(f)?;
            if v < 0 || v > u32::MAX as i64 {
                raise!(ValueError, "compile(): unrecognised flags");
            }
            v as u32
        }
        None => 0,
    };
    let dont_inherit = match dont_inherit {
        Some(d) => ab::is_true(d)?,
        None => false,
    };
    let optimize = match optimize {
        Some(o) => ab::as_i64(o)?,
        None => -1,
    };
    let feature_version = match fv {
        Some(v) => {
            let v = ab::as_i64(v)?;
            if v >= 0 { Some(v as u32) } else { None }
        }
        None => None,
    };
    let module = module.filter(|m| !m.is(none::none()));
    if flags & !(cf::PYCF_MASK | PYCF_MASK_OBSOLETE | PYCF_COMPILE_MASK) != 0 {
        raise!(ValueError, "compile(): unrecognised flags");
    }
    if !(-1..=2).contains(&optimize) {
        raise!(ValueError, "compile(): invalid optimize value");
    }
    if !dont_inherit {
        flags |= inherited_future_flags();
    }
    let mode = mode.unwrap();
    if !pystr::is_str(mode) {
        raise!(TypeError, "compile() argument 'mode' must be str, not {}", crate::types::typeobject::tname(mode));
    }
    let mode_s = pystr::to_string_lossy(mode);
    let mode = match Mode::from_str(&mode_s) {
        Some(Mode::FuncType) if flags & cf::PYCF_ONLY_AST == 0 => {
            raise!(ValueError, "compile() mode 'func_type' requires flag PyCF_ONLY_AST")
        }
        Some(m) => m,
        None => {
            if flags & cf::PYCF_ONLY_AST != 0 {
                raise!(ValueError, "compile() mode must be 'exec', 'eval', 'single' or 'func_type'");
            }
            raise!(ValueError, "compile() mode must be 'exec', 'eval' or 'single'");
        }
    };
    crate::modules::sys_full::audit("compile", &[src, &pystr::new_str(&fname)])?;
    let mut opts = Opts::new(&fname, mode, flags, optimize as i32);
    opts.module = module;
    if flags & cf::PYCF_ONLY_AST != 0 {
        opts.feature_version = feature_version;
    }

    if is_ast_node(src) {
        if flags & cf::PYCF_OPTIMIZED_AST == cf::PYCF_ONLY_AST {
            crate::modules::_ast::from_py::check_mod_type(src, mode)?;
            return Ok(src.to_ref());
        }
        let mut m = crate::modules::_ast::from_py::py_to_mod(src, mode)?;
        validate(&m, &fname)?;
        if flags & cf::PYCF_ONLY_AST != 0 {
            crate::compile::optimize_mod(&mut m, &opts)?;
            return crate::modules::_ast::to_py::mod_to_py(&m);
        }
        return Ok(crate::compile::compile_mod(&m, &opts, None)?.code);
    }

    let s = source_of(src, "compile")?;
    let source = match &s {
        Src::Text(t) => Source::Str(t),
        Src::Bytes(b) => Source::Bytes(b),
    };
    if flags & cf::PYCF_ONLY_AST != 0 {
        let (mut m, _text) = crate::compile::parse_mod(source, &opts)?;
        if flags & cf::PYCF_OPTIMIZED_AST == cf::PYCF_OPTIMIZED_AST {
            crate::compile::optimize_mod(&mut m, &opts)?;
        }
        return crate::modules::_ast::to_py::mod_to_py(&m);
    }
    Ok(crate::compile::compile_source(source, &opts)?.code)
}

/// Full `_PyAST_Validate` of a hand-built AST.
pub fn validate(m: &rpy_parser::ast::Mod, filename: &str) -> PyResult<()> {
    match rpy_compiler::validate_ast(m) {
        Ok(()) => Ok(()),
        Err(e) => Err(crate::compile::raise_compile_error(&e, filename, None)),
    }
}

/// Old entry point kept for callers: compile an AST object or with PyCF_ONLY_AST.
pub fn compile_with_ast(src: &PyObject, fname: &str, mode: Mode, flags: u32, optimize: i32) -> PyResult {
    let fname_o = pystr::new_str(fname);
    let mode_o = pystr::new_str(match mode {
        Mode::Exec => "exec",
        Mode::Eval => "eval",
        Mode::Single => "single",
        Mode::FuncType => "func_type",
    });
    let fl = crate::types::int::new_int(flags as i64);
    let opt = crate::types::int::new_int(optimize as i64);
    let t = crate::types::bool_::true_obj();
    builtin_compile(&[src, &fname_o, &mode_o, &fl, t, &opt], None)
}
