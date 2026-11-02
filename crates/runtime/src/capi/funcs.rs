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
// ---------------------------------------------------------------- instancemethod
// classobject.h's PyInstanceMethodObject: a callable stored in a class that binds to the
// instance like a Python function does (used by C code to put builtins in classes).

#[repr(C)]
struct InstanceMethodObject {
    ob_base: PyObject,
    func: *mut PyObject,
}
#[unsafe(no_mangle)]
pub static mut PyInstanceMethod_Type: PyTypeObject = PyTypeObject::ZERO;

fn im_type() -> *mut PyTypeObject {
    &raw mut PyInstanceMethod_Type
}
unsafe fn im_func<'a>(o: *mut PyObject) -> &'a PyObject {
    unsafe { &*(*(o as *mut InstanceMethodObject)).func }
}
fn new_instancemethod(f: &PyObject) -> PyRef {
    unsafe {
        let op = crate::alloc::alloc_object(im_type(), std::mem::size_of::<InstanceMethodObject>());
        (*(op as *mut InstanceMethodObject)).func = f.to_ref().into_raw();
        crate::gc::track(op);
        PyRef::from_raw(op)
    }
}
unsafe extern "C" fn im_new(_t: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        if !kw.is_null() && dict::dict_len(&*kw) != 0 {
            let _ = type_error("instancemethod() takes no keyword arguments");
            return std::ptr::null_mut();
        }
        let a = tuple::tuple_to_vec(&*args);
        if a.len() != 1 {
            let _ = type_error(&format!("instancemethod expected 1 argument, got {}", a.len()));
            return std::ptr::null_mut();
        }
        if !ab::is_callable(&a[0]) {
            let _ = type_error("first argument must be callable");
            return std::ptr::null_mut();
        }
        new_instancemethod(&a[0]).into_raw()
    }
}
unsafe extern "C" fn im_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        xdecref(std::mem::replace(&mut (*(o as *mut InstanceMethodObject)).func, std::ptr::null_mut()));
        crate::alloc::free_object(o);
    }
}
unsafe extern "C" fn im_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let f = (*(o as *mut InstanceMethodObject)).func;
        if !f.is_null() {
            return visit.unwrap()(f, arg);
        }
        0
    }
}
unsafe extern "C" fn im_clear(o: *mut PyObject) -> c_int {
    unsafe {
        xdecref(std::mem::replace(&mut (*(o as *mut InstanceMethodObject)).func, std::ptr::null_mut()));
        0
    }
}
unsafe extern "C" fn im_call(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe { ret(ab::call_with_kwargs_dict(im_func(o), &tuple::tuple_to_vec(&*args).iter().map(|x| &**x).collect::<Vec<_>>(), opt(kw))) }
}
/// Through the class: the callable itself; through an instance: a method bound to it.
unsafe extern "C" fn im_descr_get(o: *mut PyObject, obj: *mut PyObject, _t: *mut PyObject) -> *mut PyObject {
    unsafe {
        let f = im_func(o);
        if obj.is_null() {
            return f.to_ref().into_raw();
        }
        fo::new_method(f, &*obj).into_raw()
    }
}
/// Attributes not defined by the type come from the wrapped callable.
unsafe extern "C" fn im_getattro(o: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    unsafe {
        match to::generic_getattr_opt(&*o, &*name, None) {
            Ok(Some(v)) => v.into_raw(),
            Ok(None) => ret(ab::getattr(im_func(o), &*name)),
            Err(_) => std::ptr::null_mut(),
        }
    }
}
unsafe extern "C" fn im_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let f = im_func(o);
        let name = match ab::getattr_opt(f, crate::name!("__name__")) {
            Ok(Some(n)) if pystr::is_str(&n) => pystr::to_string_lossy(&n),
            Ok(_) => "?".to_string(),
            Err(_) => return std::ptr::null_mut(),
        };
        ret(Ok(pystr::new_str(&format!("<instancemethod {} at {:#x}>", name, o as usize))))
    }
}
unsafe extern "C" fn im_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        if (*b).type_ptr() != im_type() || (op != Py_EQ && op != Py_NE) {
            return crate::types::none::not_implemented().to_ref().into_raw();
        }
        let same = (*(a as *mut InstanceMethodObject)).func == (*(b as *mut InstanceMethodObject)).func;
        crate::types::bool_::new_bool(same == (op == Py_EQ)).into_raw()
    }
}
fn im_get_doc(o: &PyObject) -> PyResult {
    ab::getattr(unsafe { im_func(o.as_ptr()) }, crate::name!("__doc__"))
}
fn im_get_func(o: &PyObject) -> PyResult {
    Ok(unsafe { im_func(o.as_ptr()) }.to_ref())
}

/// Make PyInstanceMethod_Type ready (before the first extension module loads).
pub(crate) fn init_instancemethod_type() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        let t = &mut *im_type();
        t.tp_name = c"instancemethod".as_ptr();
        t.tp_basicsize = std::mem::size_of::<InstanceMethodObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
        t.tp_new = Some(im_new);
        t.tp_dealloc = Some(im_dealloc);
        t.tp_traverse = Some(im_traverse);
        t.tp_clear = Some(im_clear);
        t.tp_call = Some(im_call);
        t.tp_descr_get = Some(im_descr_get);
        t.tp_getattro = Some(im_getattro);
        t.tp_repr = Some(im_repr);
        t.tp_richcompare = Some(im_richcompare);
        t.tp_getset = crate::getset_table![("__doc__", im_get_doc), ("__func__", im_get_func)];
        if super::typeobj::PyType_Ready(t) < 0 {
            clear_exc();
        }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInstanceMethod_New(f: *mut PyObject) -> *mut PyObject {
    unsafe {
        if f.is_null() {
            return null_arg();
        }
        init_instancemethod_type();
        new_instancemethod(&*f).into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyInstanceMethod_Function(m: *mut PyObject) -> *mut PyObject {
    unsafe {
        if m.is_null() || (*m).type_ptr() != im_type() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        (*(m as *mut InstanceMethodObject)).func
    }
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
        if addr < 0 {
            return ab::getattr_str(&*co, "co_firstlineno").ok().and_then(|v| int::as_i64(&v)).unwrap_or(-1) as c_int;
        }
        code::line_for_instr(&*co, (addr / 2) as usize).map_or(-1, |l| l as c_int)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCode_Addr2Location(co: *mut PyObject, addr: c_int, sl: *mut c_int, sc: *mut c_int, el: *mut c_int, ec: *mut c_int) -> c_int {
    unsafe {
        let l = PyCode_Addr2Line(co, addr);
        *sl = l;
        *el = l;
        *sc = -1;
        *ec = -1;
        1
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyCode_CheckLineNumber(addr: c_int, _bounds: *mut c_void) -> c_int {
    addr
}
/// Body shared by every code object created from C: it raises AssertionError when run
/// (an "empty" code object must not crash the VM when executed). Built once by compiling
/// `assert 0` and kept alive for the life of the process.
fn c_code_stub() -> &'static PyObject {
    static STUB: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let p = *STUB.get_or_init(|| match crate::compile::compile_str("assert 0", "<C>", crate::compile::Mode::Exec, 0, 0) {
        Ok(c) => c.code.into_raw() as usize,
        Err(_) => {
            clear_exc();
            0
        }
    });
    assert!(p != 0, "cannot build the stub body for C-created code objects");
    unsafe { &*(p as *const PyObject) }
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

/// What a C caller (PyUnstable_Code_New*, PyCode_NewEmpty) supplies. CPython-format
/// bytecode, line tables and exception tables are accepted but not interpreted: these code
/// objects carry metadata (names, flags, argument counts, file and first line) for
/// tracebacks, frames and introspection; executing one raises AssertionError.
pub(crate) struct CCodeArgs<'a> {
    pub argcount: i32,
    pub posonlyargcount: i32,
    pub kwonlyargcount: i32,
    pub flags: i32,
    pub consts: Option<&'a PyObject>,
    pub names: Option<&'a PyObject>,
    pub varnames: Option<&'a PyObject>,
    pub freevars: Option<&'a PyObject>,
    pub cellvars: Option<&'a PyObject>,
    pub filename: &'a PyObject,
    pub name: &'a PyObject,
    pub qualname: Option<&'a PyObject>,
    pub firstlineno: i32,
    /// CPython-format location table (bytes), if the caller supplied one.
    pub linetable: Option<&'a PyObject>,
}

/// Decode a location table in CPython's published code-object format into one location per
/// 2-byte code unit. Each entry starts with a byte that has bit 7 set; bits 3-6 hold the
/// entry kind and bits 0-2 the number of code units minus one. Kinds 0-9 keep the line and
/// carry the start column in the kind (high bits) and the next byte (low 3 bits, then the
/// width); 10-12 move the line by 0-2 and carry both columns in two bytes; 13 moves the line
/// (signed varint) without columns; 14 moves the line and gives the end-line delta and both
/// columns plus one as varints; 15 has no location. Varints are 6-bit groups, low group
/// first, bit 6 meaning "more follows". Returns None for a malformed table.
fn decode_location_table(t: &[u8], firstlineno: i32) -> Option<Vec<rpy_compiler::bytecode::Location>> {
    use rpy_compiler::bytecode::Location;
    let mut pos = 0usize;
    let next = |pos: &mut usize| -> Option<u8> {
        let b = *t.get(*pos)?;
        *pos += 1;
        Some(b)
    };
    let varint = |pos: &mut usize| -> Option<u64> {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let b = next(pos)?;
            v |= ((b & 63) as u64) << shift;
            if b & 64 == 0 {
                return Some(v);
            }
            shift += 6;
            if shift > 60 {
                return None;
            }
        }
    };
    let svarint = |pos: &mut usize| -> Option<i64> {
        let u = varint(pos)?;
        Some(if u & 1 != 0 { -((u >> 1) as i64) } else { (u >> 1) as i64 })
    };
    let mut line = firstlineno as i64;
    let mut out = Vec::new();
    while pos < t.len() {
        let head = next(&mut pos)?;
        if head & 0x80 == 0 {
            return None;
        }
        let kind = (head >> 3) & 15;
        let len = (head & 7) as usize + 1;
        let as_u32 = |x: i64| if x <= 0 { 0 } else { x.min(u32::MAX as i64 - 1) as u32 };
        let loc = match kind {
            0..=9 => {
                let b = next(&mut pos)?;
                let col = (kind as u32) * 8 + ((b >> 4) & 7) as u32;
                let l = as_u32(line);
                Location { lineno: l, end_lineno: l, col, end_col: col + (b & 15) as u32 }
            }
            10..=12 => {
                line += (kind - 10) as i64;
                let c0 = next(&mut pos)? as u32;
                let c1 = next(&mut pos)? as u32;
                let l = as_u32(line);
                Location { lineno: l, end_lineno: l, col: c0, end_col: c1 }
            }
            13 => {
                line += svarint(&mut pos)?;
                let l = as_u32(line);
                Location { lineno: l, end_lineno: l, col: u32::MAX, end_col: u32::MAX }
            }
            14 => {
                line += svarint(&mut pos)?;
                let end = line + varint(&mut pos)? as i64;
                let c0 = varint(&mut pos)?;
                let c1 = varint(&mut pos)?;
                let col = |c: u64| if c == 0 { u32::MAX } else { (c - 1).min(u32::MAX as u64 - 1) as u32 };
                Location { lineno: as_u32(line), end_lineno: as_u32(end), col: col(c0), end_col: col(c1) }
            }
            _ => Location { lineno: 0, end_lineno: 0, col: u32::MAX, end_col: u32::MAX },
        };
        if out.len() + len > 1 << 20 {
            return None;
        }
        out.extend(std::iter::repeat_n(loc, len));
    }
    Some(out)
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
        Some(c) if tuple::is_tuple(c) => c.to_ref(),
        Some(_) => return Err(system_error("code: consts must be a tuple")),
        None => tuple::new_tuple(Vec::new()),
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
        let k = if i < npos {
            fast::LOCAL | fast::ARG_POS
        } else if i < npos + nkw {
            fast::LOCAL | fast::ARG_KW
        } else if i < npos + nkw + nstar {
            fast::LOCAL | fast::ARG_VAR
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
    let stub = c_code_stub();
    let sd = code::code_data(stub);
    let line = a.firstlineno.max(0) as u32;
    let first = rpy_compiler::bytecode::Location { lineno: line, end_lineno: line, col: u32::MAX, end_col: u32::MAX };
    let mut code_words = sd.code.clone();
    let mut locations = vec![first; sd.locations.len()];
    // With a location table (Cython emits one per function), instruction i reports the
    // table's entry for code unit i, so co_lines()/co_positions() give the source lines.
    // The padding instructions after the stub's own are never reached.
    if let Some(lt) = a.linetable.filter(|t| crate::types::bytes::is_bytes(t)) {
        if let Some(units) = decode_location_table(crate::types::bytes::bytes_data(lt), a.firstlineno) {
            while code_words.len() < units.len() {
                code_words.push(rpy_compiler::bytecode::encode(rpy_compiler::bytecode::Op::Nop, 0));
                locations.push(first);
            }
            for (slot, u) in locations.iter_mut().zip(units) {
                *slot = u;
            }
        }
    }
    let parts = code::CodeParts {
        argcount: a.argcount as u32,
        posonlyargcount: a.posonlyargcount as u32,
        kwonlyargcount: a.kwonlyargcount as u32,
        flags,
        stacksize: code::code(stub).obj.co_stacksize as u32,
        firstlineno: line,
        code: code_words,
        locations,
        exception_table: sd.exception_table.clone(),
        kinds,
        filename: a.filename.to_ref(),
        consts: sd.consts.clone(),
        names: sd.names.clone(),
        localsplusnames: lpn,
        name: pystr::intern(a.name.to_ref()),
        qualname: qualname.to_ref(),
    };
    let r = code::new_code_from_parts(parts)?;
    // co_consts / co_names show the caller's tuples; the stub body keeps its own
    // constants and names in the code data.
    unsafe {
        let c = &mut *(r.as_ptr() as *mut code::PyCodeObject);
        let old = std::mem::replace(&mut c.co_consts, consts.into_raw());
        xdecref(old);
        let old = std::mem::replace(&mut c.co_names, tuple::new_tuple(names).into_raw());
        xdecref(old);
    }
    Ok(r)
}

#[allow(clippy::too_many_arguments)]
unsafe fn code_new_c(
    a: c_int, p: c_int, k: c_int, f: c_int, consts: *mut PyObject, names: *mut PyObject, varnames: *mut PyObject,
    freevars: *mut PyObject, cellvars: *mut PyObject, filename: *mut PyObject, name: *mut PyObject, qualname: *mut PyObject,
    fl: c_int, lt: *mut PyObject,
) -> *mut PyObject {
    unsafe {
        if filename.is_null() || name.is_null() {
            return null_arg();
        }
        ret(new_code_from_c(CCodeArgs {
            argcount: a,
            posonlyargcount: p,
            kwonlyargcount: k,
            flags: f,
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
        }))
    }
}

/// `types.CodeType(argcount, posonlyargcount, kwonlyargcount, nlocals, stacksize, flags,
/// codestring, constants, names, varnames, filename, name, qualname, firstlineno, linetable,
/// exceptiontable, freevars=(), cellvars=(), /)`. Like the C constructors, the result carries
/// the given metadata over the shared stub body (the bytecode argument is checked for type
/// only); used e.g. by limited-API Cython modules for their functions' `__code__`.
pub(crate) unsafe extern "C" fn code_tp_new(_t: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe { ret(code_new_impl(&*args, opt(kw))) }
}

fn code_new_impl(args: &PyObject, kw: Option<&PyObject>) -> PyResult {
    if let Some(k) = kw {
        if dict::dict_len(k) != 0 {
            raise!(TypeError, "code() takes no keyword arguments");
        }
    }
    let a = tuple::tuple_to_vec(args);
    if a.len() < 16 || a.len() > 18 {
        let (what, n) = if a.len() < 16 { ("at least", 16) } else { ("at most", 18) };
        raise!(TypeError, "code expected {} {} arguments, got {}", what, n, a.len());
    }
    let int_arg = |i: usize, what: &str| -> PyResult<i32> {
        if !int::is_int(&a[i]) {
            raise!(TypeError, "code() argument {} must be int, not {}", i + 1, tname(&a[i]));
        }
        let v = ab::as_i64(&a[i])?;
        if v < i32::MIN as i64 || v > i32::MAX as i64 {
            raise!(OverflowError, "code: {} is out of range", what);
        }
        Ok(v as i32)
    };
    let typed = |i: usize, ok: bool, tn: &str| -> PyResult<()> {
        if !ok {
            raise!(TypeError, "code() argument {} must be {}, not {}", i + 1, tn, tname(&a[i]));
        }
        Ok(())
    };
    let argcount = int_arg(0, "argcount")?;
    let posonly = int_arg(1, "posonlyargcount")?;
    let kwonly = int_arg(2, "kwonlyargcount")?;
    let nlocals = int_arg(3, "nlocals")?;
    let _stacksize = int_arg(4, "stacksize")?;
    let flags = int_arg(5, "flags")?;
    typed(6, crate::types::bytes::is_bytes(&a[6]), "bytes")?;
    for i in [7, 8, 9] {
        typed(i, tuple::is_tuple(&a[i]), "tuple")?;
    }
    for i in [10, 11, 12] {
        typed(i, pystr::is_str(&a[i]), "str")?;
    }
    let firstlineno = int_arg(13, "firstlineno")?;
    typed(14, crate::types::bytes::is_bytes(&a[14]), "bytes")?;
    typed(15, crate::types::bytes::is_bytes(&a[15]), "bytes")?;
    for i in 16..a.len() {
        typed(i, tuple::is_tuple(&a[i]), "tuple")?;
    }
    if argcount < posonly || posonly < 0 || kwonly < 0 || nlocals < 0 {
        raise!(ValueError, "code: argcount must not be negative");
    }
    if nlocals as usize != tuple::tuple_len(&a[9]) {
        raise!(ValueError, "code: co_nlocals != len(co_varnames)");
    }
    new_code_from_c(CCodeArgs {
        argcount,
        posonlyargcount: posonly,
        kwonlyargcount: kwonly,
        flags,
        consts: Some(&a[7]),
        names: Some(&a[8]),
        varnames: Some(&a[9]),
        freevars: a.get(16).map(|x| &**x),
        cellvars: a.get(17).map(|x| &**x),
        filename: &a[10],
        name: &a[11],
        qualname: Some(&a[12]),
        firstlineno,
        linetable: Some(&a[14]),
    })
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
            flags: 0,
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
        }))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCode_Optimize(code: *mut PyObject, _c: *mut PyObject, _n: *mut PyObject, _l: *mut PyObject) -> *mut PyObject {
    unsafe { (*code).to_ref().into_raw() }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_Code_New(
    a: c_int, k: c_int, _n: c_int, _s: c_int, f: c_int, _code: *mut PyObject, consts: *mut PyObject, names: *mut PyObject,
    varnames: *mut PyObject, freevars: *mut PyObject, cellvars: *mut PyObject, filename: *mut PyObject, name: *mut PyObject,
    qualname: *mut PyObject, fl: c_int, _lt: *mut PyObject, _et: *mut PyObject,
) -> *mut PyObject {
    unsafe { code_new_c(a, 0, k, f, consts, names, varnames, freevars, cellvars, filename, name, qualname, fl, _lt) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_Code_NewWithPosOnlyArgs(
    a: c_int, p: c_int, k: c_int, _n: c_int, _s: c_int, f: c_int, _code: *mut PyObject, consts: *mut PyObject,
    names: *mut PyObject, varnames: *mut PyObject, freevars: *mut PyObject, cellvars: *mut PyObject, filename: *mut PyObject,
    name: *mut PyObject, qualname: *mut PyObject, fl: c_int, _lt: *mut PyObject, _et: *mut PyObject,
) -> *mut PyObject {
    unsafe { code_new_c(a, p, k, f, consts, names, varnames, freevars, cellvars, filename, name, qualname, fl, _lt) }
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
