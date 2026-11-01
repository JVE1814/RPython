//! The `builtins` module (core functions; type objects are added by their modules).

use crate::abstract_ as ab;
use crate::args::{self, Params};
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::*;
use crate::types::{bool_, dict, int, list, none, str as pystr, tuple};
use crate::{name, raise};
use std::sync::atomic::{AtomicPtr, Ordering::*};

static BUILTINS: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
static IMPORT_FN: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());

pub fn builtins_module() -> PyRef {
    unsafe { PyRef::from_borrowed(BUILTINS.load(Acquire)) }
}
pub fn builtins_dict() -> PyRef {
    crate::types::module::module_dict(&builtins_module())
}
pub fn builtin(nm: &str) -> PyResult {
    match dict::get_item_rs(&builtins_dict(), nm) {
        Some(v) => Ok(v),
        None => raise!(NameError, "name '{}' is not defined", nm),
    }
}
pub fn is_our_import(f: &PyObject) -> bool {
    f.as_ptr() == IMPORT_FN.load(Relaxed)
}

/// Add a method table to a module dict as builtin functions (module = `m` if given).
pub fn add_methods_to_dict(d: &PyObject, methods: *mut PyMethodDef, module: Option<&PyObject>) -> PyResult<()> {
    unsafe {
        let mut m = methods;
        while !(*m).ml_name.is_null() {
            let nm = std::ffi::CStr::from_ptr((*m).ml_name).to_string_lossy().into_owned();
            let modname = match module {
                Some(md) => dict::get_item_str(&crate::types::module::module_dict(md), name!("__name__")),
                None => dict::get_item_str(d, name!("__name__")),
            };
            let f = crate::types::cfunction::new_cfunction(m, module.map(|x| x.to_ref()), modname);
            dict::set_item_str(d, &nm, &f)?;
            m = m.add(1);
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ functions

fn b_print(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let (pos, kws) = args::split(args, kw);
    let mut sep: Option<PyRef> = None;
    let mut end: Option<PyRef> = None;
    let mut file: Option<PyRef> = None;
    let mut flush = false;
    for (k, v) in kws {
        match pystr::to_string_lossy(k).as_str() {
            "sep" => sep = Some(v.to_ref()),
            "end" => end = Some(v.to_ref()),
            "file" => file = Some(v.to_ref()),
            "flush" => flush = ab::is_true(v)?,
            other => raise!(TypeError, "print() got an unexpected keyword argument '{}'", other),
        }
    }
    let check = |o: &Option<PyRef>, what: &str| -> PyResult<()> {
        if let Some(v) = o {
            if !v.is(none::none()) && !pystr::is_str(v) {
                raise!(TypeError, "{} must be None or a string, not {}", what, tname(v));
            }
        }
        Ok(())
    };
    check(&sep, "sep")?;
    check(&end, "end")?;
    let file = match file.filter(|f| !f.is(none::none())) {
        Some(f) => f,
        None => match crate::modules::sys::get_sys_attr("stdout") {
            Some(f) if !f.is(none::none()) => f,
            _ => return Ok(none::none_ref()),
        },
    };
    let write = ab::getattr(&file, name!("write"))?;
    let sep = sep.filter(|s| !s.is(none::none())).unwrap_or_else(|| pystr::new_str(" "));
    let end = end.filter(|s| !s.is(none::none())).unwrap_or_else(|| pystr::new_str("\n"));
    for (i, a) in pos.iter().enumerate() {
        if i > 0 {
            ab::call(&write, &[&sep])?;
        }
        let s = ab::str(a)?;
        ab::call(&write, &[&s])?;
    }
    ab::call(&write, &[&end])?;
    if flush {
        ab::call_method_str(&file, "flush", &[])?;
    }
    Ok(none::none_ref())
}

fn update_bases(bases: &[&PyObject]) -> PyResult<Option<Vec<PyRef>>> {
    let mut changed = false;
    let mut out: Vec<PyRef> = Vec::with_capacity(bases.len());
    for b in bases {
        if is_type(b) {
            out.push(b.to_ref());
            continue;
        }
        match ab::getattr_opt(b, name!("__mro_entries__"))? {
            None => out.push(b.to_ref()),
            Some(meth) => {
                let orig = tuple::new_tuple(bases.iter().map(|x| x.to_ref()).collect());
                let r = ab::call(&meth, &[&orig])?;
                if !tuple::is_tuple(&r) {
                    raise!(TypeError, "__mro_entries__ must return a tuple");
                }
                out.extend(tuple::tuple_to_vec(&r));
                changed = true;
            }
        }
    }
    Ok(if changed { Some(out) } else { None })
}

fn b_build_class(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let (pos, kws) = args::split(args, kw);
    if pos.len() < 2 {
        raise!(TypeError, "__build_class__: not enough arguments");
    }
    let func = pos[0];
    if !crate::types::funcobject::is_function(func) {
        raise!(TypeError, "__build_class__: func must be a function");
    }
    let nm = pos[1];
    if !pystr::is_str(nm) {
        raise!(TypeError, "__build_class__: name is not a string");
    }
    let orig_bases: Vec<&PyObject> = pos[2..].to_vec();
    let new_bases = update_bases(&orig_bases)?;
    let bases = tuple::new_tuple(match &new_bases {
        Some(b) => b.clone(),
        None => orig_bases.iter().map(|b| b.to_ref()).collect(),
    });
    let mut meta: Option<PyRef> = None;
    let mkw = dict::new_dict();
    for (k, v) in kws {
        if pystr::str_eq_ascii(k, "metaclass") {
            meta = Some(v.to_ref());
        } else {
            dict::set_item(&mkw, k, v)?;
        }
    }
    let is_class = meta.as_ref().map_or(true, |m| is_type(m));
    let mut meta = match meta {
        Some(m) => m,
        None => {
            if tuple::tuple_len(&bases) == 0 {
                type_ref(type_type())
            } else {
                type_ref(tuple::tuple_get(&bases, 0).type_ptr())
            }
        }
    };
    if is_class {
        let winner = crate::types::object_type::calculate_metaclass(as_type(&meta), &bases)?;
        meta = type_ref(winner);
    }
    let kwopt = if dict::dict_len(&mkw) > 0 { Some(&*mkw) } else { None };
    let ns = match ab::getattr_opt(&meta, name!("__prepare__"))? {
        Some(prep) => ab::call_with_kwargs_dict(&prep, &[nm, &bases], kwopt)?,
        None => dict::new_dict(),
    };
    if !(ns.ty().tp_flags & Py_TPFLAGS_MAPPING != 0 || dict::is_dict(&ns) || ab::hasattr(&ns, name!("__getitem__"))?) {
        raise!(
            TypeError,
            "{}.__prepare__() must return a mapping, not {}",
            if is_class { type_name(unsafe { &*as_type(&meta) }) } else { "<metaclass>".into() },
            tname(&ns)
        );
    }
    // run the class body
    let fo = crate::types::funcobject::func(func);
    let code = unsafe { &*fo.func_code };
    let frame = crate::interp::frame::Frame::new(code, Some(func), unsafe { &*fo.func_globals }, unsafe { &*fo.func_builtins }, Some(&ns));
    let cell = crate::interp::run_frame(frame)?;
    if let Some(nb) = &new_bases {
        let _ = nb;
        let ob = tuple::new_tuple(orig_bases.iter().map(|b| b.to_ref()).collect());
        ab::setitem(&ns, name!("__orig_bases__"), Some(&ob))?;
    }
    let cls = ab::call_with_kwargs_dict(&meta, &[nm, &bases, &ns], kwopt)?;
    if is_type(&cls) && cell.is_type(crate::types::funcobject::cell_type()) {
        let v = crate::types::funcobject::cell_get(&cell);
        if !v.as_ref().map_or(false, |v| v.is(&cls)) {
            match v {
                None => raise!(
                    RuntimeError,
                    "__class__ not set defining {} as {}. Was __classcell__ propagated to type.__new__?",
                    ab::repr_string(nm)?,
                    ab::repr_string(&cls)?
                ),
                Some(_) => raise!(
                    TypeError,
                    "__class__ set to {} defining {} as {}",
                    ab::repr_string(&crate::types::funcobject::cell_get(&cell).unwrap())?,
                    ab::repr_string(nm)?,
                    ab::repr_string(&cls)?
                ),
            }
        }
    }
    Ok(cls)
}

fn b_import(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("__import__", &["name", "globals", "locals", "fromlist", "level"], 0, 1);
    let [nm, globals, _locals, fromlist, level] = args::parse::<5>(&P, args, kw)?;
    let level = match level {
        Some(l) => ab::as_i64(l)?,
        None => 0,
    };
    let empty = tuple::empty_tuple();
    let fl = fromlist.unwrap_or(&empty);
    let g = globals.filter(|g| !g.is(none::none()));
    crate::import::import_module_level(nm.unwrap(), g, fl, level)
}

fn b_len(_m: &PyObject, o: &PyObject) -> PyResult {
    Ok(int::new_int(ab::length(o)? as i64))
}
fn b_repr(_m: &PyObject, o: &PyObject) -> PyResult {
    ab::repr(o)
}
fn b_ascii(_m: &PyObject, o: &PyObject) -> PyResult {
    ab::ascii(o)
}
fn b_id(_m: &PyObject, o: &PyObject) -> PyResult {
    Ok(int::new_int_usize(o.as_ptr() as usize))
}
fn b_hash(_m: &PyObject, o: &PyObject) -> PyResult {
    Ok(int::new_int(ab::hash(o)? as i64))
}
fn b_callable(_m: &PyObject, o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(ab::is_callable(o)))
}
fn b_isinstance(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    args::check_pos("isinstance", args, 2, 2)?;
    Ok(bool_::new_bool(ab::isinstance(args[0], args[1])?))
}
fn b_issubclass(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    args::check_pos("issubclass", args, 2, 2)?;
    Ok(bool_::new_bool(ab::issubclass(args[0], args[1])?))
}
fn b_getattr(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    args::check_pos("getattr", args, 2, 3)?;
    if !pystr::is_str(args[1]) {
        raise!(TypeError, "attribute name must be string, not '{}'", tname(args[1]));
    }
    if args.len() == 3 {
        return Ok(ab::getattr_opt(args[0], args[1])?.unwrap_or_else(|| args[2].to_ref()));
    }
    ab::getattr(args[0], args[1])
}
fn b_setattr(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    args::check_pos("setattr", args, 3, 3)?;
    ab::setattr(args[0], args[1], args[2])?;
    Ok(none::none_ref())
}
fn b_delattr(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    args::check_pos("delattr", args, 2, 2)?;
    ab::delattr(args[0], args[1])?;
    Ok(none::none_ref())
}
fn b_hasattr(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    args::check_pos("hasattr", args, 2, 2)?;
    if !pystr::is_str(args[1]) {
        raise!(TypeError, "attribute name must be string, not '{}'", tname(args[1]));
    }
    Ok(bool_::new_bool(ab::hasattr(args[0], args[1])?))
}
fn b_iter(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    args::check_pos("iter", args, 1, 2)?;
    if args.len() == 2 {
        return crate::types::iterobject::new_callable_iter(args[0], args[1]);
    }
    ab::get_iter(args[0])
}
fn b_next(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    args::check_pos("next", args, 1, 2)?;
    let it = args[0];
    if it.ty().tp_iternext.is_none() {
        raise!(TypeError, "'{}' object is not an iterator", tname(it));
    }
    let f = it.ty().tp_iternext.unwrap();
    let r = unsafe { f(it.as_ptr()) };
    if !r.is_null() {
        return Ok(unsafe { PyRef::from_raw(r) });
    }
    if err_occurred() {
        if args.len() == 2 && exc_matches(crate::types::exceptions::StopIteration()) {
            clear_exc();
            return Ok(args[1].to_ref());
        }
        return Err(PyErr);
    }
    if args.len() == 2 {
        return Ok(args[1].to_ref());
    }
    Err(set_err_noargs(crate::types::exceptions::StopIteration()))
}
fn b_globals(_m: &PyObject) -> PyResult {
    match crate::interp::current_globals() {
        Some(g) => Ok(g),
        None => Ok(dict::new_dict()),
    }
}
fn b_locals(_m: &PyObject) -> PyResult {
    let f = crate::interp::current_frame_ptr();
    if f.is_null() {
        return Ok(dict::new_dict());
    }
    crate::types::frameobject::locals_dict(unsafe { &*f })
}
fn b_vars(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    args::check_pos("vars", args, 0, 1)?;
    if args.is_empty() {
        return b_locals(_m);
    }
    match ab::getattr_opt(args[0], name!("__dict__"))? {
        Some(d) => Ok(d),
        None => raise!(TypeError, "vars() argument must have __dict__ attribute"),
    }
}
fn b_dir(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    args::check_pos("dir", args, 0, 1)?;
    if args.is_empty() {
        let l = list::new_list(dict::keys_vec(&*b_locals(_m)?));
        crate::types::list_methods::sort_list(&l, None, false)?;
        return Ok(l);
    }
    ab::dir(args[0])
}
fn b_abs(_m: &PyObject, o: &PyObject) -> PyResult {
    ab::absolute(o)
}
fn b_divmod(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    args::check_pos("divmod", args, 2, 2)?;
    ab::binary_op(args[0], args[1], ab::NbSlot::Divmod)
}
fn b_pow(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("pow", &["base", "exp", "mod"], 0, 2);
    let [b, e, m] = args::parse::<3>(&P, args, kw)?;
    ab::power(b.unwrap(), e.unwrap(), m.unwrap_or(none::none()))
}
fn b_format(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    args::check_pos("format", args, 1, 2)?;
    let empty = pystr::new_str("");
    let spec = args.get(1).copied().unwrap_or(&empty);
    if !pystr::is_str(spec) {
        raise!(TypeError, "format() argument 2 must be str, not {}", tname(spec));
    }
    ab::format(args[0], spec)
}
fn b_chr(_m: &PyObject, o: &PyObject) -> PyResult {
    let v = match ab::index(o) {
        Ok(i) => i,
        Err(e) => return Err(e),
    };
    let c = match int::as_i64(&v) {
        Some(c) => c,
        None => raise!(OverflowError, "Python int too large to convert to C int"),
    };
    if !(0..=0x10ffff).contains(&c) {
        raise!(ValueError, "chr() arg not in range(0x110000)");
    }
    Ok(pystr::from_codepoints(&[c as u32]))
}
fn b_ord(_m: &PyObject, o: &PyObject) -> PyResult {
    if pystr::is_str(o) {
        let n = pystr::str_len(o);
        if n != 1 {
            raise!(TypeError, "ord() expected a character, but string of length {} found", n);
        }
        return Ok(int::new_int(pystr::str_data(o).get(0) as i64));
    }
    if let Some(b) = crate::types::bytes::bytes_like_data(o) {
        if crate::types::bytes::is_bytes(o) || crate::types::bytearray::is_bytearray(o) {
            if b.len() != 1 {
                raise!(TypeError, "ord() expected a character, but string of length {} found", b.len());
            }
            return Ok(int::new_int(b[0] as i64));
        }
    }
    raise!(TypeError, "ord() expected string of length 1, but {} found", tname(o));
}
fn b_all(_m: &PyObject, o: &PyObject) -> PyResult {
    let it = ab::get_iter(o)?;
    while let Some(x) = ab::iter_next(&it)? {
        if !ab::is_true(&x)? {
            return Ok(bool_::new_bool(false));
        }
    }
    Ok(bool_::new_bool(true))
}
fn b_any(_m: &PyObject, o: &PyObject) -> PyResult {
    let it = ab::get_iter(o)?;
    while let Some(x) = ab::iter_next(&it)? {
        if ab::is_true(&x)? {
            return Ok(bool_::new_bool(true));
        }
    }
    Ok(bool_::new_bool(false))
}
fn b_sum(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("sum", &["iterable", "start"], 1, 1);
    let [it, start] = args::parse::<2>(&P, args, kw)?;
    let mut acc = match start {
        Some(s) => {
            if pystr::is_str(s) {
                raise!(TypeError, "sum() can't sum strings [use ''.join(seq) instead]");
            }
            if crate::types::bytes::is_bytes(s) {
                raise!(TypeError, "sum() can't sum bytes [use b''.join(seq) instead]");
            }
            if crate::types::bytearray::is_bytearray(s) {
                raise!(TypeError, "sum() can't sum bytearray [use b''.join(seq) instead]");
            }
            s.to_ref()
        }
        None => int::new_int(0),
    };
    let it = ab::get_iter(it.unwrap())?;
    while let Some(x) = ab::iter_next(&it)? {
        acc = crate::interp::eval::binary_op(rpy_compiler::bytecode::BinOp::Add, &acc, &x)?;
    }
    Ok(acc)
}
fn minmax(args: &[&PyObject], kw: Option<&PyObject>, op: std::ffi::c_int, fname: &str) -> PyResult {
    let (pos, kws) = args::split(args, kw);
    let mut key: Option<PyRef> = None;
    let mut default: Option<PyRef> = None;
    for (k, v) in kws {
        match pystr::to_string_lossy(k).as_str() {
            "key" => {
                if !v.is(none::none()) {
                    key = Some(v.to_ref())
                }
            }
            "default" => default = Some(v.to_ref()),
            other => raise!(TypeError, "{}() got an unexpected keyword argument '{}'", fname, other),
        }
    }
    if pos.is_empty() {
        raise!(TypeError, "{} expected at least 1 argument, got 0", fname);
    }
    let items: Vec<PyRef> = if pos.len() == 1 {
        ab::collect_iterable(pos[0])?
    } else {
        if default.is_some() {
            raise!(TypeError, "Cannot specify a default for {}() with multiple positional arguments", fname);
        }
        pos.iter().map(|x| x.to_ref()).collect()
    };
    let mut best: Option<(PyRef, PyRef)> = None;
    for it in items {
        let k = match &key {
            Some(f) => ab::call(f, &[&it])?,
            None => it.clone(),
        };
        match &best {
            None => best = Some((k, it)),
            Some((bk, _)) => {
                if ab::rich_compare_bool(&k, bk, op)? {
                    best = Some((k, it));
                }
            }
        }
    }
    match best {
        Some((_, v)) => Ok(v),
        None => match default {
            Some(d) => Ok(d),
            None => raise!(ValueError, "{}() iterable argument is empty", fname),
        },
    }
}
fn b_min(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    minmax(args, kw, Py_LT, "min")
}
fn b_max(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    minmax(args, kw, Py_GT, "max")
}
fn b_sorted(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let (pos, kws) = args::split(args, kw);
    if pos.len() != 1 {
        raise!(TypeError, "sorted expected 1 argument, got {}", pos.len());
    }
    let l = ab::sequence_list(pos[0])?;
    let mut key = None;
    let mut reverse = false;
    for (k, v) in kws {
        match pystr::to_string_lossy(k).as_str() {
            "key" => {
                if !v.is(none::none()) {
                    key = Some(v.to_ref())
                }
            }
            "reverse" => reverse = ab::is_true(v)?,
            other => raise!(TypeError, "sort() got an unexpected keyword argument '{}'", other),
        }
    }
    crate::types::list_methods::sort_list(&l, key.as_deref(), reverse)?;
    Ok(l)
}

fn source_arg(o: &PyObject, fname: &str) -> PyResult<Vec<u8>> {
    if pystr::is_str(o) {
        let s = pystr::as_utf8(o)?;
        if s.contains('\0') {
            raise!(SyntaxError, "source code string cannot contain null bytes");
        }
        return Ok(s.as_bytes().to_vec());
    }
    match crate::types::bytes::bytes_like_data(o) {
        Some(b) => {
            if b.contains(&0) {
                raise!(SyntaxError, "source code string cannot contain null bytes");
            }
            Ok(b)
        }
        None => raise!(TypeError, "{}() arg 1 must be a string, bytes or code object", fname),
    }
}

fn b_compile(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    // compile() is implemented in modules/ast_bridge.rs (ast workstream).
    crate::modules::ast_bridge::builtin_compile(args, kw)
}

fn exec_or_eval(args: &[&PyObject], kw: Option<&PyObject>, is_eval: bool) -> PyResult {
    let fname = if is_eval { "eval" } else { "exec" };
    static PE: Params = Params::new("eval", &["source", "globals", "locals"], 0, 1);
    static PX: Params = Params::new("exec", &["source", "globals", "locals", "closure"], 0, 1);
    let (src, globals, locals, closure) = if is_eval {
        let [s, g, l] = args::parse::<3>(&PE, args, kw)?;
        (s.unwrap(), g, l, None)
    } else {
        let [s, g, l, c] = args::parse::<4>(&PX, args, kw)?;
        (s.unwrap(), g, l, c)
    };
    let globals = globals.filter(|g| !g.is(none::none()));
    let locals = locals.filter(|l| !l.is(none::none()));
    if let Some(g) = globals {
        if !dict::is_dict(g) {
            if is_eval {
                raise!(TypeError, "globals must be a real dict; try eval(expr, {{}}, mapping)");
            }
            raise!(TypeError, "exec() globals must be a dict, not {}", tname(g));
        }
    }
    if let Some(l) = locals {
        if l.ty().tp_as_mapping.is_null() {
            raise!(TypeError, "locals must be a mapping");
        }
    }
    let (g, l): (PyRef, Option<PyRef>) = match (globals, locals) {
        (Some(g), Some(l)) => (g.to_ref(), Some(l.to_ref())),
        (Some(g), None) => (g.to_ref(), Some(g.to_ref())),
        (None, l) => {
            let g = crate::interp::current_globals().ok_or_else(|| err!(SystemError, "globals and locals cannot be NULL"))?;
            let l = match l {
                Some(l) => l.to_ref(),
                None => {
                    let f = crate::interp::current_frame_ptr();
                    if f.is_null() { g.clone() } else { crate::types::frameobject::locals_dict(unsafe { &*f })? }
                }
            };
            (g, Some(l))
        }
    };
    if dict::get_item_str(&g, name!("__builtins__")).is_none() {
        dict::set_item(&g, name!("__builtins__"), &builtins_module())?;
    }
    let code = if crate::types::code::is_code(src) {
        src.to_ref()
    } else {
        let b = source_arg(src, fname)?;
        let mut b = b;
        if is_eval {
            // eval strips leading spaces and tabs
            let i = b.iter().position(|c| *c != b' ' && *c != b'\t').unwrap_or(b.len());
            b.drain(..i);
        }
        let mode = if is_eval { crate::compile::Mode::Eval } else { crate::compile::Mode::Exec };
        crate::compile::compile_bytes(&b, "<string>", mode, 0, -1)?.code
    };
    let nfree = crate::types::code::code(&code).nfreevars();
    if !is_eval {
        match closure.filter(|c| !c.is(none::none())) {
            Some(c) => {
                if !tuple::is_tuple(c) || tuple::tuple_len(c) != nfree {
                    raise!(TypeError, "code object requires a closure of exactly length {}", nfree);
                }
                crate::interp::eval_code_with_closure(&code, &g, l.as_deref(), Some(c))?;
                return Ok(none::none_ref());
            }
            None => {
                if nfree > 0 {
                    raise!(TypeError, "code object requires a closure of exactly length {}", nfree);
                }
            }
        }
    } else if nfree > 0 {
        raise!(TypeError, "code object passed to eval() may not contain free variables");
    }
    let r = crate::interp::eval_code(&code, &g, l.as_deref())?;
    Ok(if is_eval { r } else { none::none_ref() })
}
fn b_exec(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    exec_or_eval(args, kw, false)
}
fn b_eval(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    exec_or_eval(args, kw, true)
}
fn b_input(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    args::check_pos("input", args, 0, 1)?;
    if let Some(r) = crate::modules::tty::input::tty_input(args.first().copied())? {
        return Ok(r);
    }
    let out = crate::modules::sys::get_sys_attr("stdout");
    if let (Some(p), Some(o)) = (args.first(), &out) {
        let s = ab::str(p)?;
        ab::call_method_str(o, "write", &[&s])?;
        let _ = ab::call_method_str(o, "flush", &[]);
    }
    let inp = match crate::modules::sys::get_sys_attr("stdin") {
        Some(i) if !i.is(none::none()) => i,
        _ => raise!(RuntimeError, "input(): lost sys.stdin"),
    };
    let line = ab::call_method_str(&inp, "readline", &[])?;
    let s = pystr::to_string_lossy(&line);
    if s.is_empty() {
        return Err(set_err_noargs(crate::types::exceptions::EOFError()));
    }
    Ok(pystr::new_str(s.strip_suffix('\n').unwrap_or(&s)))
}

pub fn init() -> PyResult<PyRef> {
    let m = crate::types::module::new_module("builtins")?;
    BUILTINS.store(m.clone().into_raw(), Release);
    let d = crate::types::module::module_dict(&m);
    dict::set_item_str(&d, "__doc__", &pystr::new_str("Built-in functions, types, exceptions, and other objects.\n\nThis module provides direct access to all 'built-in'\nidentifiers of Python; for example, builtins.len is\nthe full name for the built-in function len().\n\nThis module is not normally accessed explicitly by most\napplications, but can be useful in modules that provide\nobjects with the same name as a built-in value, but in\nwhich the built-in of that name is also needed."))?;
    let methods = crate::mdefs![
        ("__build_class__", FASTKW, b_build_class, "__build_class__(func, name, /, *bases, [metaclass], **kwds) -> class\n\nInternal helper function used by the class statement."),
        ("__import__", FASTKW, b_import, "__import__(name, globals=None, locals=None, fromlist=(), level=0)\n--\n\nImport a module."),
        ("abs", O, b_abs, "abs($module, x, /)\n--\n\nReturn the absolute value of the argument."),
        ("all", O, b_all, "all($module, iterable, /)\n--\n\nReturn True if bool(x) is True for all values x in the iterable.\n\nIf the iterable is empty, return True."),
        ("any", O, b_any, "any($module, iterable, /)\n--\n\nReturn True if bool(x) is True for any x in the iterable.\n\nIf the iterable is empty, return False."),
        ("ascii", O, b_ascii, "ascii($module, obj, /)\n--\n\nReturn an ASCII-only representation of an object."),
        ("callable", O, b_callable, "callable($module, obj, /)\n--\n\nReturn whether the object is callable (i.e., some kind of function)."),
        ("chr", O, b_chr, "chr($module, i, /)\n--\n\nReturn a Unicode string of one character with ordinal i; 0 <= i <= 0x10ffff."),
        ("compile", FASTKW, b_compile, "compile($module, /, source, filename, mode, flags=0,\n        dont_inherit=False, optimize=-1, *, module=None,\n        _feature_version=-1)\n--\n\nCompile source into a code object that can be executed by exec() or eval()."),
        ("delattr", FAST, b_delattr, "delattr($module, obj, name, /)\n--\n\nDeletes the named attribute from the given object."),
        ("dir", FAST, b_dir, "Show attributes of an object."),
        ("divmod", FAST, b_divmod, "divmod($module, x, y, /)\n--\n\nReturn the tuple (x//y, x%y).  Invariant: div*y + mod == x."),
        ("eval", FASTKW, b_eval, "eval($module, source, /, globals=None, locals=None)\n--\n\nEvaluate the given source in the context of globals and locals."),
        ("exec", FASTKW, b_exec, "exec($module, source, /, globals=None, locals=None, *, closure=None)\n--\n\nExecute the given source in the context of globals and locals."),
        ("format", FAST, b_format, "format($module, value, format_spec='', /)\n--\n\nReturn type(value).__format__(value, format_spec)"),
        ("getattr", FAST, b_getattr, "getattr(object, name[, default]) -> value\n\nGet a named attribute from an object."),
        ("globals", NOARGS, b_globals, "globals($module, /)\n--\n\nReturn the dictionary containing the current scope's global variables."),
        ("hasattr", FAST, b_hasattr, "hasattr($module, obj, name, /)\n--\n\nReturn whether the object has an attribute with the given name."),
        ("hash", O, b_hash, "hash($module, obj, /)\n--\n\nReturn the hash value for the given object."),
        ("id", O, b_id, "id($module, obj, /)\n--\n\nReturn the identity of an object."),
        ("input", FAST, b_input, "input($module, prompt='', /)\n--\n\nRead a string from standard input.  The trailing newline is stripped."),
        ("isinstance", FAST, b_isinstance, "isinstance($module, obj, class_or_tuple, /)\n--\n\nReturn whether an object is an instance of a class or of a subclass thereof."),
        ("issubclass", FAST, b_issubclass, "issubclass($module, cls, class_or_tuple, /)\n--\n\nReturn whether 'cls' is derived from another class or is the same class."),
        ("iter", FAST, b_iter, "iter(iterable) -> iterator\niter(callable, sentinel) -> iterator\n\nGet an iterator from an object."),
        ("len", O, b_len, "len($module, obj, /)\n--\n\nReturn the number of items in a container."),
        ("locals", NOARGS, b_locals, "locals($module, /)\n--\n\nReturn a dictionary containing the current scope's local variables."),
        ("max", FASTKW, b_max, "max(iterable, *[, default=obj, key=func]) -> value\nmax(arg1, arg2, *args, *[, key=func]) -> value\n\nWith a single iterable argument, return its biggest item."),
        ("min", FASTKW, b_min, "min(iterable, *[, default=obj, key=func]) -> value\nmin(arg1, arg2, *args, *[, key=func]) -> value\n\nWith a single iterable argument, return its smallest item."),
        ("next", FAST, b_next, "next(iterator[, default])\n\nReturn the next item from the iterator."),
        ("ord", O, b_ord, "ord($module, c, /)\n--\n\nReturn the Unicode code point for a one-character string."),
        ("pow", FASTKW, b_pow, "pow($module, /, base, exp, mod=None)\n--\n\nEquivalent to base**exp with 2 arguments or base**exp % mod with 3 arguments"),
        ("print", FASTKW, b_print, "print($module, /, *args, sep=' ', end='\\n', file=None, flush=False)\n--\n\nPrints the values to a stream, or to sys.stdout by default."),
        ("repr", O, b_repr, "repr($module, obj, /)\n--\n\nReturn the canonical string representation of the object."),
        ("setattr", FAST, b_setattr, "setattr($module, obj, name, value, /)\n--\n\nSets the named attribute on the given object to the specified value."),
        ("sorted", FASTKW, b_sorted, "sorted($module, iterable, /, *, key=None, reverse=False)\n--\n\nReturn a new list containing all items from the iterable in ascending order."),
        ("sum", FASTKW, b_sum, "sum($module, iterable, /, start=0)\n--\n\nReturn the sum of a 'start' value (default: 0) plus an iterable of numbers"),
        ("vars", FAST, b_vars, "vars([object]) -> dictionary\n\nWithout arguments, equivalent to locals().\nWith an argument, equivalent to object.__dict__."),
    ];
    add_methods_to_dict(&d, methods, Some(&m))?;
    add_methods_to_dict(&d, more_builtins(), Some(&m))?;
    crate::modules::builtins_numbers::install(&d, &m)?;
    if let Some(f) = dict::get_item_rs(&d, "__import__") {
        IMPORT_FN.store(f.as_ptr(), Release);
    }
    let set = |k: &str, v: &PyObject| dict::set_item_str(&d, k, v);
    set("None", none::none())?;
    set("Ellipsis", none::ellipsis())?;
    set("NotImplemented", none::not_implemented())?;
    set("True", bool_::true_obj())?;
    set("False", bool_::false_obj())?;
    set("__debug__", &bool_::new_bool(crate::modules::sys::optimize_flag() == 0))?;
    for (n, t) in crate::types::exceptions::all_exception_types() {
        if n.starts_with('_') && n != "_IncompleteInputError" {
            continue;
        }
        set(n, type_obj(t))?;
    }
    set("EnvironmentError", type_obj(crate::types::exceptions::OSError()))?;
    set("IOError", type_obj(crate::types::exceptions::OSError()))?;
    for (n, t) in crate::types::builtin_types() {
        set(n, type_obj(t))?;
    }
    crate::types::genericalias::register_late_class_getitems(&d);
    Ok(m)
}

fn b_round(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("round", &["number", "ndigits"], 0, 1);
    let [n, nd] = args::parse::<2>(&P, args, kw)?;
    let n = n.unwrap();
    let f = match ab::lookup_special(n, name!("__round__"))? {
        Some(f) => f,
        None => raise!(TypeError, "type {} doesn't define __round__ method", tname(n)),
    };
    match nd.filter(|x| !x.is(none::none())) {
        Some(d) => ab::call(&f, &[d]),
        None => ab::call(&f, &[]),
    }
}
fn radix_str(o: &PyObject, radix: u32, prefix: &str) -> PyResult {
    let i = ab::index(o)?;
    let neg = int::sign(&i) < 0;
    let mag = int::int_methods_mag_string(&i, radix);
    Ok(pystr::new_str(&format!("{}{}{}", if neg { "-" } else { "" }, prefix, mag)))
}
fn b_bin(_m: &PyObject, o: &PyObject) -> PyResult {
    radix_str(o, 2, "0b")
}
fn b_oct(_m: &PyObject, o: &PyObject) -> PyResult {
    radix_str(o, 8, "0o")
}
fn b_hex(_m: &PyObject, o: &PyObject) -> PyResult {
    radix_str(o, 16, "0x")
}
fn b_aiter(_m: &PyObject, o: &PyObject) -> PyResult {
    let am = o.ty().tp_as_async;
    unsafe {
        if !am.is_null() {
            if let Some(f) = (*am).am_aiter {
                let r = from_c(f(o.as_ptr()))?;
                let ram = r.ty().tp_as_async;
                if ram.is_null() || (*ram).am_anext.is_none() {
                    raise!(TypeError, "aiter() returned not an async iterator of type '{}'", tname(&r));
                }
                return Ok(r);
            }
        }
    }
    raise!(TypeError, "'{}' object is not an async iterable", tname(o));
}
fn b_anext(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    args::check_pos("anext", args, 1, 2)?;
    let o = args[0];
    let am = o.ty().tp_as_async;
    unsafe {
        if am.is_null() || (*am).am_anext.is_none() {
            raise!(TypeError, "'{}' object is not an async iterator", tname(o));
        }
        let aw = from_c((*am).am_anext.unwrap()(o.as_ptr()))?;
        if args.len() == 1 {
            return Ok(aw);
        }
        crate::types::asyncgen::anext_with_default(aw, args[1].to_ref())
    }
}
fn b_breakpoint(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    match crate::modules::sys::get_sys_attr("breakpointhook") {
        Some(h) => ab::call_kw(&h, args, kw),
        None => raise!(RuntimeError, "lost sys.breakpointhook"),
    }
}

pub fn more_builtins() -> *mut PyMethodDef {
    crate::mdefs![
        ("round", FASTKW, b_round, "round($module, /, number, ndigits=None)\n--\n\nRound a number to a given precision in decimal digits."),
        ("bin", O, b_bin, "bin($module, number, /)\n--\n\nReturn the binary representation of an integer."),
        ("oct", O, b_oct, "oct($module, number, /)\n--\n\nReturn the octal representation of an integer."),
        ("hex", O, b_hex, "hex($module, number, /)\n--\n\nReturn the hexadecimal representation of an integer."),
        ("aiter", O, b_aiter, "aiter($module, async_iterable, /)\n--\n\nReturn an AsyncIterator for an AsyncIterable object."),
        ("anext", FAST, b_anext, "anext(aiterator[, default])\n\nReturn the next item from the async iterator."),
        ("breakpoint", FASTKW, b_breakpoint, "breakpoint($module, /, *args, **kws)\n--\n\nCall sys.breakpointhook(*args, **kws)."),
    ]
}
