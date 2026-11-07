//! ceval.h / pythonrun.h / compile.h: compiling and running code, frame introspection,
//! recursion guards, C-level trace/profile functions.

use super::*;
use crate::abstract_ as ab;
use crate::compile::{self, Mode};
use crate::types::{dict, int, none, str as pystr, tuple};
use std::ffi::c_void;

#[repr(C)]
pub struct PyCompilerFlags {
    pub cf_flags: c_int,
    pub cf_feature_version: c_int,
}

fn mode_of(start: c_int) -> PyResult<Mode> {
    Ok(match start {
        256 => Mode::Single,
        257 => Mode::Exec,
        258 => Mode::Eval,
        345 => Mode::from_str("func_type").ok_or_else(|| value_error("func_type mode not supported"))?,
        _ => return Err(value_error("invalid start token")),
    })
}
unsafe fn flags_of(f: *mut PyCompilerFlags) -> u32 {
    unsafe { if f.is_null() { 0 } else { (*f).cf_flags as u32 } }
}

pub(crate) fn compile_source(src: &[u8], filename: &str, start: c_int, flags: u32, optimize: i32) -> PyResult {
    let mode = mode_of(start)?;
    if flags & 0x0400 != 0 {
        // PyCF_ONLY_AST: go through builtins.compile
        let b = crate::builtins::builtins_module();
        let c = ab::getattr_str(&b, "compile")?;
        let m = match start {
            256 => "single",
            258 => "eval",
            345 => "func_type",
            _ => "exec",
        };
        return ab::call(
            &c,
            &[&crate::types::bytes::new_bytes(src), &pystr::new_str(filename), &pystr::new_str(m), &int::new_int(flags as i64), &crate::types::bool_::new_bool(true), &int::new_int(optimize as i64)],
        );
    }
    Ok(compile::compile_bytes(src, filename, mode, flags, optimize)?.code)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_CompileStringObject(s: *const c_char, filename: *mut PyObject, start: c_int, flags: *mut PyCompilerFlags, optimize: c_int) -> *mut PyObject {
    unsafe {
        let f = pystr::to_string_lossy(&*filename);
        ret(compile_source(CStr::from_ptr(s).to_bytes(), &f, start, flags_of(flags), optimize))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_CompileStringExFlags(s: *const c_char, filename: *const c_char, start: c_int, flags: *mut PyCompilerFlags, optimize: c_int) -> *mut PyObject {
    unsafe {
        let f = match super::unicode::decode_fs(CStr::from_ptr(filename).to_bytes()) {
            Ok(f) => f,
            Err(_) => return std::ptr::null_mut(),
        };
        Py_CompileStringObject(s, f.as_ptr(), start, flags, optimize)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_CompileStringFlags(s: *const c_char, filename: *const c_char, start: c_int, flags: *mut PyCompilerFlags) -> *mut PyObject {
    unsafe { Py_CompileStringExFlags(s, filename, start, flags, -1) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_CompileString(s: *const c_char, filename: *const c_char, start: c_int) -> *mut PyObject {
    unsafe { Py_CompileStringExFlags(s, filename, start, std::ptr::null_mut(), -1) }
}

fn ensure_builtins(globals: &PyObject) -> PyResult<()> {
    // every C entry point that runs code needs a real dict for globals
    if !dict::is_dict(globals) {
        return Err(system_error("globals must be a dict"));
    }
    if dict::is_dict(globals) && dict::get_item_rs(globals, "__builtins__").is_none() {
        dict::set_item_str(globals, "__builtins__", &crate::builtins::builtins_module())?;
    }
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_EvalCode(co: *mut PyObject, globals: *mut PyObject, locals: *mut PyObject) -> *mut PyObject {
    unsafe {
        if globals.is_null() || !dict::is_dict(&*globals) {
            let _ = system_error("globals must be a dict");
            return std::ptr::null_mut();
        }
        if ensure_builtins(&*globals).is_err() {
            return std::ptr::null_mut();
        }
        let loc = opt(locals).unwrap_or(&*globals);
        ret(crate::interp::eval_code(&*co, &*globals, Some(loc)))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_EvalCodeEx(
    co: *mut PyObject,
    globals: *mut PyObject,
    locals: *mut PyObject,
    args: *const *mut PyObject,
    argc: c_int,
    kws: *const *mut PyObject,
    kwc: c_int,
    defs: *const *mut PyObject,
    defc: c_int,
    kwdefs: *mut PyObject,
    closure: *mut PyObject,
) -> *mut PyObject {
    unsafe {
        if argc == 0 && kwc == 0 && defc == 0 && kwdefs.is_null() {
            if globals.is_null() || ensure_builtins(&*globals).is_err() {
                return std::ptr::null_mut();
            }
            let loc = opt(locals).unwrap_or(&*globals);
            return ret(crate::interp::eval_code_with_closure(&*co, &*globals, Some(loc), opt(closure)));
        }
        // general case: build a function object and call it
        ret((|| -> PyResult {
            ensure_builtins(&*globals)?;
            let f = crate::types::funcobject::new_function(&*co, &*globals, None)?;
            let fo = crate::types::funcobject::func(&f);
            if defc > 0 {
                let d: Vec<PyRef> = (0..defc as usize).map(|i| (**defs.add(i)).to_ref()).collect();
                fo.func_defaults = tuple::new_tuple(d).into_raw();
            }
            if !kwdefs.is_null() {
                fo.func_kwdefaults = (*kwdefs).to_ref().into_raw();
            }
            if !closure.is_null() {
                fo.func_closure = (*closure).to_ref().into_raw();
            }
            let pos: Vec<&PyObject> = (0..argc as usize).map(|i| &**args.add(i)).collect();
            let kw = dict::new_dict();
            for i in 0..kwc as usize {
                dict::set_item(&kw, &**kws.add(2 * i), &**kws.add(2 * i + 1))?;
            }
            ab::call_with_kwargs_dict(&f, &pos, Some(&kw))
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_EvalFrame(f: *mut PyObject) -> *mut PyObject {
    unsafe { PyEval_EvalFrameEx(f, 0) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_EvalFrameEx(f: *mut PyObject, _throw: c_int) -> *mut PyObject {
    unsafe {
        let co = super::funcs::PyFrame_GetCode(f);
        if co.is_null() {
            return co;
        }
        let co = PyRef::from_raw(co);
        let g = match ab::getattr_str(&*f, "f_globals") {
            Ok(g) => g,
            Err(_) => return std::ptr::null_mut(),
        };
        let l = ab::getattr_str(&*f, "f_locals").ok();
        clear_exc();
        ret(crate::interp::eval_code(&co, &g, l.as_deref()))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyEval_EvalFrameDefault(_ts: *mut c_void, _frame: *mut c_void, _throwflag: c_int) -> *mut PyObject {
    let _ = system_error("_PyEval_EvalFrameDefault: interpreter frames are not CPython frames in this runtime");
    std::ptr::null_mut()
}

unsafe fn run_string(src: &[u8], start: c_int, globals: *mut PyObject, locals: *mut PyObject, flags: *mut PyCompilerFlags, filename: &str) -> PyResult {
    unsafe {
        let g = super::nn(globals)?;
        ensure_builtins(g)?;
        let co = compile_source(src, filename, start, flags_of(flags), -1)?;
        let l = opt(locals).unwrap_or(g);
        crate::interp::eval_code(&co, g, Some(l))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyRun_StringFlags(s: *const c_char, start: c_int, g: *mut PyObject, l: *mut PyObject, flags: *mut PyCompilerFlags) -> *mut PyObject {
    unsafe { ret(run_string(CStr::from_ptr(s).to_bytes(), start, g, l, flags, "<string>")) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyRun_String(s: *const c_char, start: c_int, g: *mut PyObject, l: *mut PyObject) -> *mut PyObject {
    unsafe { PyRun_StringFlags(s, start, g, l, std::ptr::null_mut()) }
}
unsafe fn main_dict() -> PyResult {
    unsafe {
        let m = super::import::PyImport_AddModuleRef(c"__main__".as_ptr());
        let m = from_c(m)?;
        Ok(crate::types::module::module_dict(&m))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyRun_SimpleStringFlags(s: *const c_char, flags: *mut PyCompilerFlags) -> c_int {
    unsafe {
        let r = main_dict().and_then(|d| run_string(CStr::from_ptr(s).to_bytes(), 257, d.as_ptr(), d.as_ptr(), flags, "<string>"));
        match r {
            Ok(_) => 0,
            Err(_) => {
                super::errors::PyErr_Print();
                -1
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyRun_SimpleString(s: *const c_char) -> c_int {
    unsafe { PyRun_SimpleStringFlags(s, std::ptr::null_mut()) }
}
unsafe fn read_file(fp: *mut libc::FILE) -> Vec<u8> {
    unsafe {
        let mut v = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = libc::fread(buf.as_mut_ptr() as *mut c_void, 1, buf.len(), fp);
            if n == 0 {
                break;
            }
            v.extend_from_slice(&buf[..n]);
        }
        v
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyRun_FileExFlags(
    fp: *mut libc::FILE,
    filename: *const c_char,
    start: c_int,
    g: *mut PyObject,
    l: *mut PyObject,
    closeit: c_int,
    flags: *mut PyCompilerFlags,
) -> *mut PyObject {
    unsafe {
        let src = read_file(fp);
        if closeit != 0 {
            libc::fclose(fp);
        }
        ret(run_string(&src, start, g, l, flags, &cs(filename)))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyRun_FileEx(fp: *mut libc::FILE, p: *const c_char, s: c_int, g: *mut PyObject, l: *mut PyObject, c: c_int) -> *mut PyObject {
    unsafe { PyRun_FileExFlags(fp, p, s, g, l, c, std::ptr::null_mut()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyRun_FileFlags(fp: *mut libc::FILE, p: *const c_char, s: c_int, g: *mut PyObject, l: *mut PyObject, f: *mut PyCompilerFlags) -> *mut PyObject {
    unsafe { PyRun_FileExFlags(fp, p, s, g, l, 0, f) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyRun_File(fp: *mut libc::FILE, p: *const c_char, s: c_int, g: *mut PyObject, l: *mut PyObject) -> *mut PyObject {
    unsafe { PyRun_FileExFlags(fp, p, s, g, l, 0, std::ptr::null_mut()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyRun_SimpleFileExFlags(fp: *mut libc::FILE, filename: *const c_char, closeit: c_int, flags: *mut PyCompilerFlags) -> c_int {
    unsafe {
        let src = read_file(fp);
        if closeit != 0 {
            libc::fclose(fp);
        }
        let r = main_dict().and_then(|d| {
            dict::set_item_str(&d, "__file__", &pystr::new_str(&cs(filename)))?;
            run_string(&src, 257, d.as_ptr(), d.as_ptr(), flags, &cs(filename))
        });
        match r {
            Ok(_) => 0,
            Err(_) => {
                super::errors::PyErr_Print();
                -1
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyRun_SimpleFileEx(fp: *mut libc::FILE, p: *const c_char, c: c_int) -> c_int {
    unsafe { PyRun_SimpleFileExFlags(fp, p, c, std::ptr::null_mut()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyRun_SimpleFile(fp: *mut libc::FILE, p: *const c_char) -> c_int {
    unsafe { PyRun_SimpleFileExFlags(fp, p, 0, std::ptr::null_mut()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyRun_AnyFileExFlags(fp: *mut libc::FILE, p: *const c_char, c: c_int, f: *mut PyCompilerFlags) -> c_int {
    unsafe { PyRun_SimpleFileExFlags(fp, p, c, f) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyRun_AnyFileEx(fp: *mut libc::FILE, p: *const c_char, c: c_int) -> c_int {
    unsafe { PyRun_SimpleFileExFlags(fp, p, c, std::ptr::null_mut()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyRun_AnyFileFlags(fp: *mut libc::FILE, p: *const c_char, f: *mut PyCompilerFlags) -> c_int {
    unsafe { PyRun_SimpleFileExFlags(fp, p, 0, f) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyRun_AnyFile(fp: *mut libc::FILE, p: *const c_char) -> c_int {
    unsafe { PyRun_SimpleFileExFlags(fp, p, 0, std::ptr::null_mut()) }
}
unsafe fn interactive_one(fp: *mut libc::FILE, filename: &str, flags: *mut PyCompilerFlags) -> c_int {
    unsafe {
        let mut line: *mut c_char = std::ptr::null_mut();
        let mut cap: usize = 0;
        if libc::getline(&mut line, &mut cap, fp) < 0 {
            libc::free(line as *mut c_void);
            return -1; // E_EOF
        }
        let src = CStr::from_ptr(line).to_bytes().to_vec();
        libc::free(line as *mut c_void);
        let r = main_dict().and_then(|d| run_string(&src, 256, d.as_ptr(), d.as_ptr(), flags, filename));
        match r {
            Ok(_) => 0,
            Err(_) => {
                super::errors::PyErr_Print();
                -1
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyRun_InteractiveOneObject(fp: *mut libc::FILE, filename: *mut PyObject, flags: *mut PyCompilerFlags) -> c_int {
    unsafe { interactive_one(fp, &pystr::to_string_lossy(&*filename), flags) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyRun_InteractiveOneFlags(fp: *mut libc::FILE, filename: *const c_char, flags: *mut PyCompilerFlags) -> c_int {
    unsafe { interactive_one(fp, &cs(filename), flags) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyRun_InteractiveOne(fp: *mut libc::FILE, filename: *const c_char) -> c_int {
    unsafe { interactive_one(fp, &cs(filename), std::ptr::null_mut()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyRun_InteractiveLoopFlags(fp: *mut libc::FILE, filename: *const c_char, flags: *mut PyCompilerFlags) -> c_int {
    unsafe {
        loop {
            if libc::feof(fp) != 0 {
                return 0;
            }
            let r = interactive_one(fp, &cs(filename), flags);
            if r < 0 && libc::feof(fp) != 0 {
                return 0;
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyRun_InteractiveLoop(fp: *mut libc::FILE, filename: *const c_char) -> c_int {
    unsafe { PyRun_InteractiveLoopFlags(fp, filename, std::ptr::null_mut()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_FdIsInteractive(fp: *mut libc::FILE, _filename: *const c_char) -> c_int {
    unsafe { (libc::isatty(libc::fileno(fp)) != 0) as c_int }
}

// ---------------------------------------------------------------- frame introspection

unsafe fn current_frame() -> Option<&'static mut crate::interp::frame::Frame> {
    unsafe {
        let f = crate::interp::current_frame_ptr();
        if f.is_null() { None } else { Some(&mut *f) }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_GetFrame() -> *mut PyObject {
    unsafe {
        match current_frame() {
            Some(f) => crate::interp::eval::frame_object(f),
            None => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_GetGlobals() -> *mut PyObject {
    unsafe {
        match current_frame() {
            Some(f) => f.globals,
            None => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_GetBuiltins() -> *mut PyObject {
    unsafe {
        match current_frame() {
            Some(f) => f.builtins,
            None => {
                let b = crate::builtins::builtins_module();
                let d = crate::types::module::module_dict(&b);
                let p = d.as_ptr();
                drop(d);
                p
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_GetLocals() -> *mut PyObject {
    unsafe {
        let Some(f) = current_frame() else {
            let _ = system_error("frame does not exist");
            return std::ptr::null_mut();
        };
        if !f.locals.is_null() {
            return f.locals;
        }
        // borrowed snapshot cached on the frame object
        let fo = crate::interp::eval::frame_object(f);
        let snap = ab::getattr_str(&*fo, "f_locals").and_then(|l| {
            if dict::is_dict(&l) { Ok(l) } else { ab::call(crate::types::typeobject::type_obj(dict::dict_type()), &[&l]) }
        });
        match snap {
            Ok(l) => {
                let p = l.as_ptr();
                let cache = &mut (*(fo as *mut crate::types::frameobject::PyFrameObject)).locals_snapshot;
                let old = std::mem::replace(cache, l.into_raw());
                xdecref(old);
                p
            }
            Err(_) => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_GetFrameBuiltins() -> *mut PyObject {
    unsafe {
        let p = PyEval_GetBuiltins();
        xincref(p);
        p
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_GetFrameGlobals() -> *mut PyObject {
    unsafe {
        let p = PyEval_GetGlobals();
        xincref(p);
        p
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_GetFrameLocals() -> *mut PyObject {
    unsafe {
        match current_frame() {
            Some(f) => {
                let fo = crate::interp::eval::frame_object(f);
                // PEP 667: a snapshot dict for optimized (function) frames, the real
                // namespace for module and class frames
                ret(ab::getattr_str(&*fo, "f_locals").and_then(|l| {
                    if crate::types::dict::is_dict(&l) { Ok(l) } else { ab::call(crate::types::typeobject::type_obj(crate::types::dict::dict_type()), &[&l]) }
                }))
            }
            None => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_GetFuncName(f: *mut PyObject) -> *const c_char {
    unsafe {
        let o = &*f;
        if crate::types::funcobject::is_method(o) {
            return PyEval_GetFuncName(crate::types::funcobject::method_parts(o).0.as_ptr());
        }
        if crate::types::cfunction::is_cfunction(o) {
            return (*(*(f as *mut PyCFunctionObject)).m_ml).ml_name;
        }
        if crate::types::funcobject::is_function(o) {
            // the name string is owned by the function object
            let n = crate::types::funcobject::func(o).func_name;
            if !n.is_null() {
                let p = super::unicode::PyUnicode_AsUTF8(n);
                if !p.is_null() {
                    return p;
                }
                clear_exc();
            }
        }
        (*o.type_ptr()).tp_name
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_GetFuncDesc(f: *mut PyObject) -> *const c_char {
    unsafe {
        let o = &*f;
        if crate::types::funcobject::is_function(o) || crate::types::cfunction::is_cfunction(o) || crate::types::funcobject::is_method(o) {
            c"()".as_ptr()
        } else {
            c" object".as_ptr()
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_MergeCompilerFlags(cf: *mut PyCompilerFlags) -> c_int {
    unsafe {
        if cf.is_null() {
            return 0;
        }
        if let Some(f) = current_frame() {
            let flags = crate::types::code::code(f.code()).flags() as c_int;
            (*cf).cf_flags |= flags & 0x3fe0000; // CO_FUTURE_* bits
        }
        ((*cf).cf_flags != 0) as c_int
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyEval_SliceIndex(v: *mut PyObject, pi: *mut Py_ssize_t) -> c_int {
    unsafe {
        if v.is_null() || none::is_none(&*v) {
            return 1;
        }
        _PyEval_SliceIndexNotNone(v, pi)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyEval_SliceIndexNotNone(v: *mut PyObject, pi: *mut Py_ssize_t) -> c_int {
    unsafe {
        if !ab::has_index(&*v) {
            let _ = type_error("slice indices must be integers or have an __index__ method");
            return 0;
        }
        let i = match ab::index(&*v) {
            Ok(i) => i,
            Err(_) => return 0,
        };
        *pi = match int::as_i64(&i) {
            Some(x) => x as isize,
            None => {
                if int::sign(&i) < 0 { isize::MIN } else { isize::MAX }
            }
        };
        1
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyEval_UnpackIndices(start: *mut PyObject, stop: *mut PyObject, len: Py_ssize_t, istart: *mut Py_ssize_t, istop: *mut Py_ssize_t) -> c_int {
    unsafe {
        if len < 0 {
            return 0;
        }
        *istart = 0;
        *istop = len;
        if _PyEval_SliceIndex(start, istart) == 0 || _PyEval_SliceIndex(stop, istop) == 0 {
            return 0;
        }
        super::containers::PySlice_AdjustIndices(len, istart, istop, 1);
        1
    }
}

// ---------------------------------------------------------------- recursion

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_EnterRecursiveCall(where_: *const c_char) -> c_int {
    unsafe { ret_i(crate::interp::enter_recursive_call(crate::threads::current(), &cs(where_))) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_LeaveRecursiveCall() {
    crate::interp::leave_recursive_call(crate::threads::current())
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_GetRecursionLimit() -> c_int {
    crate::threads::current().c.py_recursion_limit
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_SetRecursionLimit(n: c_int) {
    crate::threads::set_recursion_limit_all(n)
}

// ---------------------------------------------------------------- C trace/profile functions

type Tracefunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject, c_int, *mut PyObject) -> c_int;

#[repr(C)]
struct CTracer {
    ob_base: PyObject,
    func: Tracefunc,
    obj: *mut PyObject,
}
static mut CTRACER_TYPE: PyTypeObject = PyTypeObject::ZERO;
unsafe extern "C" fn ctracer_call(s: *mut PyObject, args: *mut PyObject, _kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let t = &*(s as *mut CTracer);
        let a = tuple::tuple_items(&*args);
        if a.len() < 3 {
            return none::none_ref().into_raw();
        }
        let ev = pystr::to_string_lossy(a[1]);
        let what = match ev.as_str() {
            "call" => 0,
            "exception" => 1,
            "line" => 2,
            "return" => 3,
            "c_call" => 4,
            "c_exception" => 5,
            "c_return" => 6,
            "opcode" => 7,
            _ => return s_ret(s),
        };
        let arg = if what == 1 { a[2].as_ptr() } else { a[2].as_ptr() };
        if (t.func)(t.obj, a[0].as_ptr(), what, arg) != 0 {
            return std::ptr::null_mut();
        }
        s_ret(s)
    }
}
unsafe fn s_ret(s: *mut PyObject) -> *mut PyObject {
    unsafe {
        incref(s);
        s
    }
}
unsafe extern "C" fn ctracer_dealloc(o: *mut PyObject) {
    unsafe {
        xdecref((*(o as *mut CTracer)).obj);
        crate::alloc::free_object(o);
    }
}
fn ctracer_type() -> *mut PyTypeObject {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        let t = &mut *(&raw mut CTRACER_TYPE);
        t.tp_name = c"_ctracefunc".as_ptr();
        t.tp_basicsize = std::mem::size_of::<CTracer>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT;
        t.tp_call = Some(ctracer_call);
        t.tp_dealloc = Some(ctracer_dealloc);
        let _ = crate::types::typeobject::type_ready(t);
    });
    &raw mut CTRACER_TYPE
}
unsafe fn make_tracer(func: Option<Tracefunc>, obj: *mut PyObject) -> PyRef {
    unsafe {
        match func {
            None => none::none_ref(),
            Some(f) => {
                let op = crate::alloc::alloc_object(ctracer_type(), std::mem::size_of::<CTracer>());
                let t = &mut *(op as *mut CTracer);
                t.func = f;
                t.obj = obj;
                xincref(obj);
                PyRef::from_raw(op)
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_SetTrace(func: Option<Tracefunc>, obj: *mut PyObject) {
    unsafe {
        let t = make_tracer(func, obj);
        if crate::modules::sys_trace_api::settrace(&t).is_err() {
            clear_exc();
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_SetProfile(func: Option<Tracefunc>, obj: *mut PyObject) {
    unsafe {
        let t = make_tracer(func, obj);
        if crate::modules::sys_trace_api::setprofile(&t).is_err() {
            clear_exc();
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_SetTraceAllThreads(func: Option<Tracefunc>, obj: *mut PyObject) {
    unsafe {
        let t = make_tracer(func, obj);
        if crate::modules::sys_trace_api::settrace_all(&t).is_err() {
            clear_exc();
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_SetProfileAllThreads(func: Option<Tracefunc>, obj: *mut PyObject) {
    unsafe {
        let t = make_tracer(func, obj);
        if crate::modules::sys_trace_api::setprofile_all(&t).is_err() {
            clear_exc();
        }
    }
}

// ---------------------------------------------------------------- compile.h

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCompile_OpcodeStackEffect(_opcode: c_int, _oparg: c_int) -> c_int {
    let _ = value_error("invalid opcode or oparg");
    -1
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCompile_OpcodeStackEffectWithJump(opcode: c_int, oparg: c_int, _jump: c_int) -> c_int {
    unsafe { PyCompile_OpcodeStackEffect(opcode, oparg) }
}

/// Is `o` a trace function installed from C (PyEval_SetTrace)? Such functions receive the
/// events of every frame, including frames that were already running when it was set.
pub fn is_ctracer(o: *mut PyObject) -> bool {
    !o.is_null() && unsafe { (*o).type_ptr() } == ctracer_type()
}
