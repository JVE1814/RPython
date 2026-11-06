//! methodobject.h / funcobject.h / classobject.h / cellobject.h / code.h /
//! frameobject.h / genobject.h / descrobject.h / weakrefobject.h / context.h:
//! function-like objects, code & frame accessors, descriptors, weak references.

use super::*;
use crate::abstract_ as ab;
use crate::types::funcobject::{self as fo, PyFunctionObject};
use crate::types::typeobject::{self as to, type_obj};
use crate::types::{cfunction, code, descr, dict, frameobject, int, none, str as pystr, tuple};
use std::ffi::c_void;

// ---------------------------------------------------------------- builtin functions

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCMethod_New(ml: *mut PyMethodDef, slf: *mut PyObject, module: *mut PyObject, cls: *mut PyTypeObject) -> *mut PyObject {
    unsafe {
        let flags = (*ml).ml_flags;
        if flags & METH_METHOD != 0 && cls.is_null() {
            let _ = system_error("attempting to create PyCMethod with a METH_METHOD flag but no class");
            return std::ptr::null_mut();
        }
        if flags & METH_METHOD == 0 && !cls.is_null() {
            let _ = system_error("attempting to create PyCFunction with class but no METH_METHOD flag");
            return std::ptr::null_mut();
        }
        let f = cfunction::new_cfunction(ml, opt(slf).map(|s| s.to_ref()), opt(module).map(|m| m.to_ref()));
        if !cls.is_null() {
            cfunction::set_defining_class(&f, cls);
        }
        f.into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCFunction_NewEx(ml: *mut PyMethodDef, slf: *mut PyObject, module: *mut PyObject) -> *mut PyObject {
    unsafe { PyCMethod_New(ml, slf, module, std::ptr::null_mut()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCFunction_New(ml: *mut PyMethodDef, slf: *mut PyObject) -> *mut PyObject {
    unsafe { PyCMethod_New(ml, slf, std::ptr::null_mut(), std::ptr::null_mut()) }
}
unsafe fn chk_cfunc(o: *mut PyObject) -> bool {
    unsafe {
        if o.is_null() || !cfunction::is_cfunction(&*o) {
            let _ = bad_internal();
            return false;
        }
        true
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCFunction_GetFunction(o: *mut PyObject) -> *mut c_void {
    unsafe {
        if !chk_cfunc(o) {
            return std::ptr::null_mut();
        }
        (*(*(o as *mut PyCFunctionObject)).m_ml).ml_meth as *mut c_void
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCFunction_GetSelf(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        if !chk_cfunc(o) {
            return std::ptr::null_mut();
        }
        let f = &*(o as *mut PyCFunctionObject);
        if (*f.m_ml).ml_flags & METH_STATIC != 0 { std::ptr::null_mut() } else { f.m_self }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCFunction_GetFlags(o: *mut PyObject) -> c_int {
    unsafe {
        if !chk_cfunc(o) {
            return -1;
        }
        (*(*(o as *mut PyCFunctionObject)).m_ml).ml_flags
    }
}

// ---------------------------------------------------------------- cells, methods

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCell_New(v: *mut PyObject) -> *mut PyObject {
    unsafe { fo::new_cell(opt(v).map(|x| x.to_ref())).into_raw() }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCell_Get(c: *mut PyObject) -> *mut PyObject {
    unsafe {
        if c.is_null() || !(*c).is_type(fo::cell_type()) {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        ret_opt(fo::cell_get(&*c))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCell_Set(c: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        if c.is_null() || !(*c).is_type(fo::cell_type()) {
            let _ = bad_internal();
            return -1;
        }
        fo::cell_set(&*c, opt(v).map(|x| x.to_ref()));
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMethod_New(f: *mut PyObject, slf: *mut PyObject) -> *mut PyObject {
    unsafe {
        if slf.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        fo::new_method(&*f, &*slf).into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMethod_Function(m: *mut PyObject) -> *mut PyObject {
    unsafe {
        if m.is_null() || !fo::is_method(&*m) {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        fo::method_parts(&*m).0.as_ptr()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMethod_Self(m: *mut PyObject) -> *mut PyObject {
    unsafe {
        if m.is_null() || !fo::is_method(&*m) {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        fo::method_parts(&*m).1.as_ptr()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInstanceMethod_New(f: *mut PyObject) -> *mut PyObject {
    // instancemethod behaves like the function itself when bound through a class
    unsafe { (*f).to_ref().into_raw() }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInstanceMethod_Function(m: *mut PyObject) -> *mut PyObject {
    m
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyStaticMethod_New(f: *mut PyObject) -> *mut PyObject {
    unsafe { fo::new_staticmethod((*f).to_ref()).into_raw() }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyClassMethod_New(f: *mut PyObject) -> *mut PyObject {
    unsafe { fo::new_classmethod((*f).to_ref()).into_raw() }
}

// ---------------------------------------------------------------- functions

unsafe fn pyfunc<'a>(o: *mut PyObject) -> Option<&'a mut PyFunctionObject> {
    unsafe {
        if o.is_null() || !fo::is_function(&*o) {
            let _ = bad_internal();
            return None;
        }
        Some(fo::func(&*o))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFunction_NewWithQualName(code: *mut PyObject, globals: *mut PyObject, qualname: *mut PyObject) -> *mut PyObject {
    unsafe { ret(fo::new_function(&*code, &*globals, opt(qualname))) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFunction_New(code: *mut PyObject, globals: *mut PyObject) -> *mut PyObject {
    unsafe { PyFunction_NewWithQualName(code, globals, std::ptr::null_mut()) }
}
macro_rules! func_get {
    ($name:ident, $field:ident) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(o: *mut PyObject) -> *mut PyObject {
            unsafe {
                match pyfunc(o) {
                    Some(f) => f.$field,
                    None => std::ptr::null_mut(),
                }
            }
        }
    };
}
func_get!(PyFunction_GetCode, func_code);
func_get!(PyFunction_GetGlobals, func_globals);
func_get!(PyFunction_GetModule, func_module);
func_get!(PyFunction_GetDefaults, func_defaults);
func_get!(PyFunction_GetKwDefaults, func_kwdefaults);
func_get!(PyFunction_GetClosure, func_closure);
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFunction_GetAnnotations(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        if pyfunc(o).is_none() {
            return std::ptr::null_mut();
        }
        let fo = crate::types::funcobject::func(&*o);
        if !fo.func_annotations.is_null() {
            return fo.func_annotations;
        }
        if fo.func_annotate.is_null() || none::is_none(&*fo.func_annotate) {
            return none::none().as_ptr();
        }
        match ab::getattr_str(&*o, "__annotations__") {
            Ok(a) => {
                let p = a.as_ptr();
                // the function caches the dict: return it borrowed
                drop(a);
                p
            }
            Err(_) => std::ptr::null_mut(),
        }
    }
}
unsafe fn func_set(o: *mut PyObject, attr: &str, v: *mut PyObject) -> c_int {
    unsafe {
        if pyfunc(o).is_none() {
            return -1;
        }
        let v = opt(v).unwrap_or(none::none());
        ret_i(ab::setattr(&*o, &pystr::new_str(attr), v))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFunction_SetDefaults(o: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        if !v.is_null() && !none::is_none(&*v) && !tuple::is_tuple(&*v) {
            let _ = system_error("non-tuple default args");
            return -1;
        }
        func_set(o, "__defaults__", v)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFunction_SetKwDefaults(o: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        if !v.is_null() && !none::is_none(&*v) && !dict::is_dict(&*v) {
            let _ = system_error("non-dict keyword only default args");
            return -1;
        }
        func_set(o, "__kwdefaults__", v)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFunction_SetClosure(o: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let Some(f) = pyfunc(o) else { return -1 };
        if !v.is_null() && !none::is_none(&*v) && !tuple::is_tuple(&*v) {
            let _ = system_error(&format!("expected tuple for closure, got '{}'", tname(&*v)));
            return -1;
        }
        let old = f.func_closure;
        f.func_closure = if v.is_null() || none::is_none(&*v) { std::ptr::null_mut() } else { (*v).to_ref().into_raw() };
        xdecref(old);
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFunction_SetAnnotations(o: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        if !v.is_null() && !none::is_none(&*v) && !dict::is_dict(&*v) {
            let _ = system_error("non-dict annotations");
            return -1;
        }
        func_set(o, "__annotations__", v)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFunction_SetVectorcall(o: *mut PyObject, vc: vectorcallfunc) {
    unsafe {
        if let Some(f) = pyfunc(o) {
            f.vectorcall = vc;
        }
    }
}
static FUNC_WATCHERS: std::sync::Mutex<[usize; 8]> = std::sync::Mutex::new([0; 8]);
static CODE_WATCHERS: std::sync::Mutex<[usize; 8]> = std::sync::Mutex::new([0; 8]);
static CTX_WATCHERS: std::sync::Mutex<[usize; 8]> = std::sync::Mutex::new([0; 8]);
fn add_watcher(w: &std::sync::Mutex<[usize; 8]>, cb: *mut c_void, what: &str) -> c_int {
    let mut g = w.lock().unwrap();
    for (i, s) in g.iter_mut().enumerate() {
        if *s == 0 {
            *s = cb as usize;
            return i as c_int;
        }
    }
    let _ = crate::err!(RuntimeError, "no more {} watcher IDs available", what);
    -1
}
fn clear_watcher(w: &std::sync::Mutex<[usize; 8]>, id: c_int, what: &str) -> c_int {
    let mut g = w.lock().unwrap();
    if !(0..8).contains(&id) {
        let _ = value_error(&format!("Invalid {} watcher ID {}", what, id));
        return -1;
    }
    if g[id as usize] == 0 {
        let _ = value_error(&format!("No {} watcher set for ID {}", what, id));
        return -1;
    }
    g[id as usize] = 0;
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFunction_AddWatcher(cb: *mut c_void) -> c_int {
    add_watcher(&FUNC_WATCHERS, cb, "func")
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFunction_ClearWatcher(id: c_int) -> c_int {
    clear_watcher(&FUNC_WATCHERS, id, "func")
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCode_AddWatcher(cb: *mut c_void) -> c_int {
    add_watcher(&CODE_WATCHERS, cb, "code")
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCode_ClearWatcher(id: c_int) -> c_int {
    clear_watcher(&CODE_WATCHERS, id, "code")
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyContext_AddWatcher(cb: *mut c_void) -> c_int {
    add_watcher(&CTX_WATCHERS, cb, "context")
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyContext_ClearWatcher(id: c_int) -> c_int {
    clear_watcher(&CTX_WATCHERS, id, "context")
}

// ---------------------------------------------------------------- code objects

unsafe fn code_attr(co: *mut PyObject, a: &str) -> *mut PyObject {
    unsafe { ret(ab::getattr_str(&*co, a)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCode_GetCode(co: *mut PyObject) -> *mut PyObject {
    unsafe { code_attr(co, "co_code") }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCode_GetVarnames(co: *mut PyObject) -> *mut PyObject {
    unsafe { code_attr(co, "co_varnames") }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCode_GetCellvars(co: *mut PyObject) -> *mut PyObject {
    unsafe { code_attr(co, "co_cellvars") }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCode_GetFreevars(co: *mut PyObject) -> *mut PyObject {
    unsafe { code_attr(co, "co_freevars") }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCode_Addr2Line(co: *mut PyObject, addr: c_int) -> c_int {
    unsafe {
        if co.is_null() || !code::is_code(&*co) {
            return -1;
        }
        code::addr2line(&*co, addr as i64) as c_int
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCode_Addr2Location(co: *mut PyObject, addr: c_int, sl: *mut c_int, sc: *mut c_int, el: *mut c_int, ec: *mut c_int) -> c_int {
    unsafe {
        if co.is_null() || !code::is_code(&*co) {
            return 0;
        }
        let (l, c, e, x) = code::addr2location(&*co, addr as i64);
        *sl = l;
        *sc = c;
        *el = e;
        *ec = x;
        1
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyCode_CheckLineNumber(addr: c_int, _bounds: *mut c_void) -> c_int {
    addr
}
fn str_tuple(o: Option<&PyObject>, what: &str) -> PyResult<Vec<PyRef>> {
    let Some(o) = o else { return Ok(Vec::new()) };
    if !tuple::is_tuple(o) {
        return Err(system_error(&format!("code: {} must be a tuple", what)));
    }
    let v = tuple::tuple_to_vec(o);
    for n in &v {
        if !pystr::is_str(n) {
            return Err(system_error(&format!("code: {} must be a tuple of str", what)));
        }
    }
    Ok(v.into_iter().map(pystr::intern).collect())
}

/// What a C caller (PyUnstable_Code_New*, PyCode_NewEmpty) supplies. The bytecode,
/// location table and exception table are CPython 3.15 public formats; the code object
/// is executable (translated on first use like any other).
pub(crate) struct CCodeArgs<'a> {
    pub argcount: i32,
    pub posonlyargcount: i32,
    pub kwonlyargcount: i32,
    pub stacksize: i32,
    pub flags: i32,
    pub code: Option<&'a PyObject>,
    pub consts: Option<&'a PyObject>,
    pub names: Option<&'a PyObject>,
    pub varnames: Option<&'a PyObject>,
    pub freevars: Option<&'a PyObject>,
    pub cellvars: Option<&'a PyObject>,
    pub filename: &'a PyObject,
    pub name: &'a PyObject,
    pub qualname: Option<&'a PyObject>,
    pub firstlineno: i32,
    pub linetable: Option<&'a PyObject>,
    pub exceptiontable: Option<&'a PyObject>,
}

/// Body of PyCode_NewEmpty code objects: `RESUME 0; LOAD_COMMON_CONSTANT AssertionError;
/// RAISE_VARARGS 1` (running one raises instead of crashing), all on the first line.
fn empty_body(firstlineno: i32) -> (Vec<u8>, Vec<u8>) {
    use crate::interp::wc::{self, op};
    let code = vec![op::RESUME, 0, 0, 0, op::LOAD_COMMON_CONSTANT, 0, op::RAISE_VARARGS, 1];
    let l = firstlineno.max(0) as u32;
    let pos = wc::Position { lineno: Some(l), end_lineno: Some(l), col: None, end_col: None };
    (code, wc::encode_linetable(&[(4, pos)], l))
}

pub(crate) fn new_code_from_c(a: CCodeArgs) -> PyResult {
    use rpy_compiler::bytecode::fast;
    if a.argcount < 0 || a.posonlyargcount < 0 || a.kwonlyargcount < 0 {
        return Err(system_error("code: argument counts must not be negative"));
    }
    for (o, what) in [(a.filename, "filename"), (a.name, "name")] {
        if !pystr::is_str(o) {
            return Err(system_error(&format!("code: {} must be a str", what)));
        }
    }
    let qualname = match a.qualname {
        Some(q) if pystr::is_str(q) => q,
        Some(_) => return Err(system_error("code: qualname must be a str")),
        None => a.name,
    };
    let consts = match a.consts {
        Some(c) if tuple::is_tuple(c) => tuple::tuple_to_vec(c),
        Some(_) => return Err(system_error("code: consts must be a tuple")),
        None => Vec::new(),
    };
    let names = str_tuple(a.names, "names")?;
    let varnames = str_tuple(a.varnames, "varnames")?;
    let freevars = str_tuple(a.freevars, "freevars")?;
    let cellvars = str_tuple(a.cellvars, "cellvars")?;
    // localsplus: the variable names (arguments first), then cells that are not also
    // variables, then free variables
    let flags = a.flags as u32;
    let npos = a.argcount as usize;
    let nkw = a.kwonlyargcount as usize;
    let nstar = (flags & 0x4 != 0) as usize + (flags & 0x8 != 0) as usize;
    let mut lpn: Vec<PyRef> = Vec::new();
    let mut kinds: Vec<u8> = Vec::new();
    for (i, n) in varnames.iter().enumerate() {
        let k = if i < (a.posonlyargcount as usize).min(npos) {
            fast::LOCAL | fast::ARG_POS
        } else if i < npos {
            fast::LOCAL | fast::ARG_POS | fast::ARG_KW
        } else if i < npos + nkw {
            fast::LOCAL | fast::ARG_KW
        } else if i < npos + nkw + nstar {
            let first_star = i == npos + nkw;
            if first_star && flags & 0x4 != 0 {
                fast::LOCAL | fast::ARG_VAR | fast::ARG_POS
            } else {
                fast::LOCAL | fast::ARG_VAR | fast::ARG_KW
            }
        } else {
            fast::LOCAL
        };
        lpn.push(n.clone());
        kinds.push(k);
    }
    for n in &cellvars {
        match lpn.iter().position(|x| pystr::str_eq(x, n)) {
            Some(i) => kinds[i] |= fast::CELL,
            None => {
                lpn.push(n.clone());
                kinds.push(fast::CELL);
            }
        }
    }
    for n in &freevars {
        lpn.push(n.clone());
        kinds.push(fast::FREE);
    }
    let bytes_or = |o: Option<&PyObject>, what: &str| -> PyResult<Option<PyRef>> {
        match o {
            None => Ok(None),
            Some(b) if crate::types::bytes::is_bytes(b) => Ok(Some(b.to_ref())),
            Some(_) => Err(system_error(&format!("code: {} must be bytes", what))),
        }
    };
    let (code, linetable, stacksize) = match bytes_or(a.code, "code")? {
        Some(c) => (c, bytes_or(a.linetable, "linetable")?.unwrap_or_else(|| crate::types::bytes::new_bytes(b"")), a.stacksize),
        None => {
            let (c, l) = empty_body(a.firstlineno);
            (crate::types::bytes::new_bytes(&c), crate::types::bytes::new_bytes(&l), a.stacksize.max(1))
        }
    };
    let exceptiontable = bytes_or(a.exceptiontable, "exceptiontable")?.unwrap_or_else(|| crate::types::bytes::new_bytes(b""));
    code::new_code_from_public(code::PublicParts {
        argcount: a.argcount,
        posonlyargcount: a.posonlyargcount,
        kwonlyargcount: a.kwonlyargcount,
        stacksize,
        flags: a.flags,
        firstlineno: a.firstlineno,
        code,
        consts,
        names,
        localsplusnames: lpn,
        kinds,
        filename: a.filename.to_ref(),
        name: pystr::intern(a.name.to_ref()),
        qualname: qualname.to_ref(),
        linetable,
        exceptiontable,
    })
}

#[allow(clippy::too_many_arguments)]
unsafe fn code_new_c(
    a: c_int, p: c_int, k: c_int, s: c_int, f: c_int, code: *mut PyObject, consts: *mut PyObject, names: *mut PyObject,
    varnames: *mut PyObject, freevars: *mut PyObject, cellvars: *mut PyObject, filename: *mut PyObject, name: *mut PyObject,
    qualname: *mut PyObject, fl: c_int, lt: *mut PyObject, et: *mut PyObject,
) -> *mut PyObject {
    unsafe {
        if filename.is_null() || name.is_null() || code.is_null() {
            return null_arg();
        }
        ret(new_code_from_c(CCodeArgs {
            argcount: a,
            posonlyargcount: p,
            kwonlyargcount: k,
            stacksize: s,
            flags: f,
            code: opt(code),
            consts: opt(consts),
            names: opt(names),
            varnames: opt(varnames),
            freevars: opt(freevars),
            cellvars: opt(cellvars),
            filename: &*filename,
            name: &*name,
            qualname: opt(qualname),
            firstlineno: fl,
            linetable: opt(lt),
            exceptiontable: opt(et),
        }))
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCode_NewEmpty(filename: *const c_char, funcname: *const c_char, firstlineno: c_int) -> *mut PyObject {
    unsafe {
        let Ok(fname) = cstr_obj(filename) else { return std::ptr::null_mut() };
        let Ok(name) = cstr_obj(funcname) else { return std::ptr::null_mut() };
        ret(new_code_from_c(CCodeArgs {
            argcount: 0,
            posonlyargcount: 0,
            kwonlyargcount: 0,
            stacksize: 0,
            flags: 0,
            code: None,
            consts: None,
            names: None,
            varnames: None,
            freevars: None,
            cellvars: None,
            filename: &fname,
            name: &name,
            qualname: None,
            firstlineno,
            linetable: None,
            exceptiontable: None,
        }))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCode_Optimize(code: *mut PyObject, _c: *mut PyObject, _n: *mut PyObject, _l: *mut PyObject) -> *mut PyObject {
    unsafe { (*code).to_ref().into_raw() }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_Code_New(
    a: c_int, k: c_int, _n: c_int, s: c_int, f: c_int, code: *mut PyObject, consts: *mut PyObject, names: *mut PyObject,
    varnames: *mut PyObject, freevars: *mut PyObject, cellvars: *mut PyObject, filename: *mut PyObject, name: *mut PyObject,
    qualname: *mut PyObject, fl: c_int, lt: *mut PyObject, et: *mut PyObject,
) -> *mut PyObject {
    unsafe { code_new_c(a, 0, k, s, f, code, consts, names, varnames, freevars, cellvars, filename, name, qualname, fl, lt, et) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_Code_NewWithPosOnlyArgs(
    a: c_int, p: c_int, k: c_int, _n: c_int, s: c_int, f: c_int, code: *mut PyObject, consts: *mut PyObject,
    names: *mut PyObject, varnames: *mut PyObject, freevars: *mut PyObject, cellvars: *mut PyObject, filename: *mut PyObject,
    name: *mut PyObject, qualname: *mut PyObject, fl: c_int, lt: *mut PyObject, et: *mut PyObject,
) -> *mut PyObject {
    unsafe { code_new_c(a, p, k, s, f, code, consts, names, varnames, freevars, cellvars, filename, name, qualname, fl, lt, et) }
}
static CODE_EXTRA: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());
static CODE_EXTRA_DATA: std::sync::Mutex<Vec<(usize, isize, usize)>> = std::sync::Mutex::new(Vec::new());
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_Eval_RequestCodeExtraIndex(free: *mut c_void) -> Py_ssize_t {
    let mut g = CODE_EXTRA.lock().unwrap();
    g.push(free as usize);
    (g.len() - 1) as Py_ssize_t
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_Code_GetExtra(co: *mut PyObject, index: Py_ssize_t, extra: *mut *mut c_void) -> c_int {
    unsafe {
        let g = CODE_EXTRA_DATA.lock().unwrap();
        *extra = g.iter().find(|(c, i, _)| *c == co as usize && *i == index).map_or(std::ptr::null_mut(), |x| x.2 as *mut c_void);
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_Code_SetExtra(co: *mut PyObject, index: Py_ssize_t, extra: *mut c_void) -> c_int {
    if index < 0 || index as usize >= CODE_EXTRA.lock().unwrap().len() {
        let _ = bad_internal();
        return -1;
    }
    let mut g = CODE_EXTRA_DATA.lock().unwrap();
    g.retain(|(c, i, _)| !(*c == co as usize && *i == index));
    g.push((co as usize, index, extra as usize));
    0
}

// ---------------------------------------------------------------- frames

unsafe fn frame<'a>(f: *mut PyObject) -> Option<&'a mut crate::interp::frame::Frame> {
    unsafe { if f.is_null() { None } else { frameobject::frame_of(&*f) } }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFrame_GetLineNumber(f: *mut PyObject) -> c_int {
    unsafe {
        match ab::getattr_str(&*f, "f_lineno") {
            Ok(v) => int::as_i64(&v).unwrap_or(-1) as c_int,
            Err(_) => {
                clear_exc();
                frame(f).map_or(-1, |fr| fr.lineno())
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFrame_GetCode(f: *mut PyObject) -> *mut PyObject {
    unsafe {
        match frame(f) {
            Some(fr) => fr.code().to_ref().into_raw(),
            None => ret(ab::getattr_str(&*f, "f_code")),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFrame_GetBack(f: *mut PyObject) -> *mut PyObject {
    unsafe {
        match ab::getattr_str(&*f, "f_back") {
            Ok(b) if !none::is_none(&b) => b.into_raw(),
            Ok(_) => std::ptr::null_mut(),
            Err(_) => {
                clear_exc();
                std::ptr::null_mut()
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFrame_GetLocals(f: *mut PyObject) -> *mut PyObject {
    unsafe { ret(ab::getattr_str(&*f, "f_locals")) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFrame_GetGlobals(f: *mut PyObject) -> *mut PyObject {
    unsafe { ret(ab::getattr_str(&*f, "f_globals")) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFrame_GetBuiltins(f: *mut PyObject) -> *mut PyObject {
    unsafe { ret(ab::getattr_str(&*f, "f_builtins")) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFrame_GetGenerator(f: *mut PyObject) -> *mut PyObject {
    unsafe {
        match frame(f) {
            Some(fr) if !fr.generator.is_null() => (*fr.generator).to_ref().into_raw(),
            _ => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFrame_GetLasti(f: *mut PyObject) -> c_int {
    unsafe {
        match ab::getattr_str(&*f, "f_lasti") {
            Ok(v) => int::as_i64(&v).unwrap_or(-1) as c_int,
            Err(_) => {
                clear_exc();
                -1
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFrame_GetVar(f: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    unsafe {
        if !pystr::is_str(&*name) {
            let _ = type_error(&format!("name must be str, not {}", tname(&*name)));
            return std::ptr::null_mut();
        }
        let l = PyFrame_GetLocals(f);
        if l.is_null() {
            return l;
        }
        let l = PyRef::from_raw(l);
        match ab::getitem(&l, &*name) {
            Ok(v) => v.into_raw(),
            Err(_) => {
                if exc_matches(crate::types::exceptions::KeyError()) {
                    clear_exc();
                    let _ = crate::types::exceptions::name_error_with(
                        crate::types::exceptions::NameError(),
                        &format!("variable {} does not exist", ab::repr_string(&*name).unwrap_or_default()),
                        &*name,
                    );
                }
                std::ptr::null_mut()
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFrame_GetVarString(f: *mut PyObject, name: *const c_char) -> *mut PyObject {
    unsafe {
        match cstr_obj(name) {
            Ok(n) => PyFrame_GetVar(f, n.as_ptr()),
            Err(_) => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFrame_FastToLocalsWithError(_f: *mut PyObject) -> c_int {
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFrame_FastToLocals(_f: *mut PyObject) {}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFrame_LocalsToFast(_f: *mut PyObject, _clear: c_int) {}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFrame_New(_ts: *mut c_void, code: *mut PyObject, globals: *mut PyObject, locals: *mut PyObject) -> *mut PyObject {
    unsafe {
        let b = crate::builtins::builtins_module();
        let bd = crate::types::module::module_dict(&b);
        if code.is_null() || !code::is_code(&*code) || globals.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        // a frame for non-optimized code without a locals mapping uses its globals
        let locals = if locals.is_null() && code::code(&*code).flags() & 1 == 0 { globals } else { locals };
        let fr = crate::interp::frame::Frame::new(&*code, None, &*globals, &bd, opt(locals));
        let p = Box::into_raw(fr);
        let fobj = frameobject::new_frame_object(&mut *p);
        (*p).frame_obj = fobj;
        incref(fobj); // the caller's reference (take_ownership drops the frame's self-reference)
        frameobject::take_ownership(fobj, Box::from_raw(p));
        fobj
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyFrame_IsEntryFrame(_f: *mut PyObject) -> c_int {
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_InterpreterFrame_GetCode(_f: *mut c_void) -> *mut PyObject {
    std::ptr::null_mut()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_InterpreterFrame_GetLasti(_f: *mut c_void) -> c_int {
    -1
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_InterpreterFrame_GetLine(_f: *mut c_void) -> c_int {
    -1
}

// ---------------------------------------------------------------- generators

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyGen_GetCode(g: *mut PyObject) -> *mut PyObject {
    unsafe { ret(ab::getattr_str(&*g, "gi_code")) }
}
unsafe fn no_gen_from_frame(what: &str) -> *mut PyObject {
    let _ = system_error(&format!("{}: creating generators from frame objects is not supported", what));
    std::ptr::null_mut()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyGen_New(_f: *mut PyObject) -> *mut PyObject {
    unsafe { no_gen_from_frame("PyGen_New") }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyGen_NewWithQualName(_f: *mut PyObject, _n: *mut PyObject, _q: *mut PyObject) -> *mut PyObject {
    unsafe { no_gen_from_frame("PyGen_NewWithQualName") }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCoro_New(_f: *mut PyObject, _n: *mut PyObject, _q: *mut PyObject) -> *mut PyObject {
    unsafe { no_gen_from_frame("PyCoro_New") }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyAsyncGen_New(_f: *mut PyObject, _n: *mut PyObject, _q: *mut PyObject) -> *mut PyObject {
    unsafe { no_gen_from_frame("PyAsyncGen_New") }
}

// ---------------------------------------------------------------- descriptors

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDescr_NewMethod(t: *mut PyTypeObject, ml: *mut PyMethodDef) -> *mut PyObject {
    descr::new_method_descr(t, ml).into_raw()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDescr_NewClassMethod(t: *mut PyTypeObject, ml: *mut PyMethodDef) -> *mut PyObject {
    descr::new_classmethod_descr(t, ml).into_raw()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDescr_NewMember(t: *mut PyTypeObject, m: *mut PyMemberDef) -> *mut PyObject {
    descr::new_member_descr(t, m).into_raw()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDescr_NewGetSet(t: *mut PyTypeObject, g: *mut PyGetSetDef) -> *mut PyObject {
    descr::new_getset_descr(t, g).into_raw()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDescr_NewWrapper(t: *mut PyTypeObject, base: *mut wrapperbase, wrapped: *mut c_void) -> *mut PyObject {
    unsafe {
        // find the matching slot definition by name
        let name = CStr::from_ptr((*base).name).to_string_lossy();
        for i in 0..crate::types::slots::len_of_slotdefs() {
            if crate::types::slots::slotdef(i).name == name {
                return descr::new_wrapper_descr(t, i, wrapped).into_raw();
            }
        }
        let _ = system_error("PyDescr_NewWrapper: unknown wrapper");
        std::ptr::null_mut()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyDescr_IsData(d: *mut PyObject) -> c_int {
    unsafe { (*d).ty().tp_descr_set.is_some() as c_int }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyWrapper_New(d: *mut PyObject, slf: *mut PyObject) -> *mut PyObject {
    unsafe {
        match (*d).ty().tp_descr_get {
            Some(g) => g(d, slf, (*slf).type_ptr() as *mut PyObject),
            None => {
                let _ = bad_internal();
                std::ptr::null_mut()
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMember_GetOne(obj: *const c_char, m: *mut PyMemberDef) -> *mut PyObject {
    unsafe {
        // PyMember_GetOne takes the object address (char*) and the def with an absolute offset
        ret(descr::member_get(obj as *mut PyObject, m))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMember_SetOne(obj: *mut c_char, m: *mut PyMemberDef, v: *mut PyObject) -> c_int {
    unsafe { ret_i(descr::member_set(obj as *mut PyObject, m, opt(v))) }
}

// ---------------------------------------------------------------- weak references
// Backed by types::weakref. A NULL argument is a SystemError; any other
// non-weakref argument is a TypeError (PyWeakref_GetObject: SystemError).

use crate::types::weakref as wref;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyWeakref_NewRef(o: *mut PyObject, cb: *mut PyObject) -> *mut PyObject {
    unsafe {
        match nn(o) {
            Ok(o) => ret(wref::new_ref(wref::ref_type(), o, opt(cb))),
            Err(_) => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyWeakref_NewProxy(o: *mut PyObject, cb: *mut PyObject) -> *mut PyObject {
    unsafe {
        match nn(o) {
            Ok(o) => ret(wref::new_proxy(o, opt(cb))),
            Err(_) => std::ptr::null_mut(),
        }
    }
}
/// The weakref behind `r`, or an error (SystemError for NULL, TypeError otherwise).
unsafe fn weakref_arg<'a>(r: *mut PyObject) -> PyResult<&'a PyObject> {
    unsafe {
        let r = nn(r)?;
        if !wref::is_weakref(r) {
            return Err(type_error(&format!("expected a weakref, got {}", tname(r))));
        }
        Ok(r)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyWeakref_GetRef(r: *mut PyObject, out: *mut *mut PyObject) -> c_int {
    unsafe {
        *out = std::ptr::null_mut();
        let Ok(r) = weakref_arg(r) else { return -1 };
        match wref::get_object(r) {
            Some(v) => {
                *out = v.into_raw();
                1
            }
            None => 0,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyWeakref_IsDead(r: *mut PyObject) -> c_int {
    unsafe {
        match weakref_arg(r) {
            Ok(r) => wref::is_dead(r) as c_int,
            Err(_) => -1,
        }
    }
}
/// For the deprecated borrowed-reference getter (compat.rs).
pub(crate) unsafe fn weakref_get_borrowed(r: *mut PyObject) -> *mut PyObject {
    unsafe {
        if r.is_null() || !wref::is_weakref(&*r) {
            return null_arg_or_bad_internal(r);
        }
        match wref::get_object(&*r) {
            // the referent is kept alive by its other references (legacy API contract)
            Some(v) => {
                let p = v.as_ptr();
                drop(v);
                p
            }
            None => none::none().as_ptr(),
        }
    }
}
unsafe fn null_arg_or_bad_internal(r: *mut PyObject) -> *mut PyObject {
    if r.is_null() {
        return null_arg();
    }
    let _ = bad_internal();
    std::ptr::null_mut()
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyWeakref_ClearRef(_r: *mut PyObject) {}

// ---------------------------------------------------------------- contextvars

unsafe fn ctxmod() -> PyResult {
    crate::import::import_absolute("_contextvars")
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyContext_New() -> *mut PyObject {
    unsafe { ret(ctxmod().and_then(|m| ab::getattr_str(&m, "Context")).and_then(|c| ab::call(&c, &[]))) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyContext_CopyCurrent() -> *mut PyObject {
    unsafe { ret(ctxmod().and_then(|m| ab::getattr_str(&m, "copy_context")).and_then(|c| ab::call(&c, &[]))) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyContext_Copy(c: *mut PyObject) -> *mut PyObject {
    unsafe { ret(ab::call_method_str(&*c, "copy", &[])) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyContext_Enter(c: *mut PyObject) -> c_int {
    unsafe { ret_i(ab::call_method_str(&*c, "_enter", &[]).map(|_| ())) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyContext_Exit(c: *mut PyObject) -> c_int {
    unsafe { ret_i(ab::call_method_str(&*c, "_exit", &[]).map(|_| ())) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyContextVar_New(name: *const c_char, def: *mut PyObject) -> *mut PyObject {
    unsafe {
        ret((|| -> PyResult {
            let cv = ab::getattr_str(&*(ctxmod()?), "ContextVar")?;
            let n = cstr_obj(name)?;
            match opt(def) {
                Some(d) => {
                    let kwn = tuple::new_tuple(vec![pystr::new_str("default")]);
                    ab::call_kw(&cv, &[&n, d], Some(&kwn))
                }
                None => ab::call(&cv, &[&n]),
            }
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyContextVar_Get(var: *mut PyObject, def: *mut PyObject, value: *mut *mut PyObject) -> c_int {
    unsafe {
        *value = std::ptr::null_mut();
        let r = match opt(def) {
            Some(d) => ab::call_method_str(&*var, "get", &[d]),
            None => ab::call_method_str(&*var, "get", &[]),
        };
        match r {
            Ok(v) => {
                *value = v.into_raw();
                0
            }
            Err(_) => {
                if exc_matches(crate::types::exceptions::LookupError()) {
                    clear_exc();
                    return 0;
                }
                -1
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyContextVar_Set(var: *mut PyObject, v: *mut PyObject) -> *mut PyObject {
    unsafe { ret(ab::call_method_str(&*var, "set", &[&*v])) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyContextVar_Reset(var: *mut PyObject, tok: *mut PyObject) -> c_int {
    unsafe { ret_i(ab::call_method_str(&*var, "reset", &[&*tok]).map(|_| ())) }
}

// ---------------------------------------------------------------- misc objects

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_GenericAlias(origin: *mut PyObject, args: *mut PyObject) -> *mut PyObject {
    unsafe {
        ret((|| -> PyResult {
            let types = crate::import::import_absolute("types")?;
            let ga = ab::getattr_str(&types, "GenericAlias")?;
            ab::call(&ga, &[&*origin, &*args])
        })())
    }
}

#[allow(dead_code)]
fn _unused() -> &'static PyObject {
    type_obj(fo::function_type())
}
