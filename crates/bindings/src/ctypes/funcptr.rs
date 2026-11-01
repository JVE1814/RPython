//! Foreign function calls: CArgObject (byref/from_param results), the CFuncPtr type
//! and its metaclass, argument conversion, the libffi call and callbacks (thunks).

use super::cdata::*;
use super::libffi::{FfiCif, FfiType};
use super::*;
use rpy_runtime::types::{bytes, dict, tuple};
use std::ffi::c_void;

// ------------------------------------------------------------------ CArgObject

#[repr(C)]
pub struct CArgObject {
    pub ob_base: PyObject,
    pub pffi_type: *mut FfiType,
    pub tag: u8,
    pub value: [u128; 2],
    pub obj: *mut PyObject,
    pub size: isize,
}

impl CArgObject {
    pub fn set_ptr(&mut self, p: *mut u8) {
        unsafe { *(self.value.as_mut_ptr() as *mut *mut u8) = p };
    }
    pub fn ptr(&self) -> *mut u8 {
        unsafe { *(self.value.as_ptr() as *const *mut u8) }
    }
}

pub static mut CARG_TYPE: PyTypeObject = PyTypeObject::ZERO;

pub fn carg_type() -> *mut PyTypeObject {
    &raw mut CARG_TYPE
}

pub fn is_carg(o: &PyObject) -> bool {
    o.type_ptr() == carg_type()
}

pub fn carg(o: &PyObject) -> &'static mut CArgObject {
    unsafe { &mut *(o.as_ptr() as *mut CArgObject) }
}

pub fn new_carg(tag: u8, ffi: *mut FfiType) -> PyResult {
    unsafe {
        let op = rpy_runtime::types::typeobject::generic_alloc(carg_type(), 0);
        if op.is_null() {
            return Err(PyErr);
        }
        let a = &mut *(op as *mut CArgObject);
        a.tag = tag;
        a.pffi_type = ffi;
        a.value = [0; 2];
        a.obj = std::ptr::null_mut();
        a.size = 0;
        Ok(PyRef::from_raw(op))
    }
}

pub fn carg_set_obj(a: &PyObject, o: PyRef) {
    let c = carg(a);
    let old = c.obj;
    c.obj = o.into_raw();
    unsafe { xdecref(old) };
}

unsafe extern "C" fn carg_dealloc(op: *mut PyObject) {
    unsafe {
        rpy_runtime::gc::untrack(op);
        let a = &mut *(op as *mut CArgObject);
        let o = a.obj;
        a.obj = std::ptr::null_mut();
        xdecref(o);
        rpy_runtime::types::typeobject::generic_free_and_type_decref(op, ob_type(op));
    }
}

unsafe extern "C" fn carg_traverse(op: *mut PyObject, v: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let a = &*(op as *mut CArgObject);
        if !a.obj.is_null() {
            return v.unwrap()(a.obj, arg);
        }
        0
    }
}

unsafe extern "C" fn carg_clear(op: *mut PyObject) -> c_int {
    unsafe {
        let a = &mut *(op as *mut CArgObject);
        let o = a.obj;
        a.obj = std::ptr::null_mut();
        xdecref(o);
    }
    0
}

fn fmt_double(v: f64) -> String {
    let f = rpy_runtime::types::float::new_float(v);
    repr(&f).unwrap_or_default()
}

unsafe extern "C" fn carg_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let a = &*(op as *mut CArgObject);
        let t = a.tag as char;
        let vp = a.value.as_ptr() as *const u8;
        let s = match a.tag {
            b'b' => format!("<cparam '{}' ({})>", t, *(vp as *const i8)),
            b'B' => format!("<cparam '{}' ({})>", t, *vp),
            b'h' => format!("<cparam '{}' ({})>", t, *(vp as *const i16)),
            b'H' => format!("<cparam '{}' ({})>", t, *(vp as *const u16)),
            b'i' => format!("<cparam '{}' ({})>", t, *(vp as *const i32)),
            b'I' => format!("<cparam '{}' ({})>", t, *(vp as *const u32)),
            b'l' | b'q' => format!("<cparam '{}' ({})>", t, *(vp as *const i64)),
            b'L' | b'Q' => format!("<cparam '{}' ({})>", t, *(vp as *const u64)),
            b'd' => format!("<cparam '{}' ({})>", t, fmt_double(*(vp as *const f64))),
            b'f' => format!("<cparam '{}' ({})>", t, fmt_double(*(vp as *const f32) as f64)),
            b'g' => format!("<cparam '{}' at {:#x}>", t, op as usize),
            b'F' | b'D' | b'G' => format!("<cparam 'Z{}' at {:#x}>", (a.tag as char).to_ascii_lowercase(), op as usize),
            b'c' => {
                let c = *vp;
                if (0x20..0x7f).contains(&c) {
                    format!("<cparam '{}' ('{}')>", t, c as char)
                } else {
                    format!("<cparam '{}' ('\\x{:02x}')>", t, c)
                }
            }
            b'z' | b'Z' | b'P' => format!("<cparam '{}' ({:#x})>", t, *(vp as *const usize)),
            _ => {
                if a.tag.is_ascii_graphic() {
                    format!("<cparam '{}' at {:#x}>", t, op as usize)
                } else {
                    format!("<cparam {:#04x} at {:#x}>", a.tag, op as usize)
                }
            }
        };
        new_str(&s).into_raw()
    }
}

fn carg_obj_get(o: &PyObject) -> PyResult {
    let a = carg(o);
    if a.obj.is_null() { Ok(none()) } else { Ok(unsafe { PyRef::from_borrowed(a.obj) }) }
}

/// `byref(obj, offset)` result.
pub fn byref_impl(obj: &PyObject, offset: isize) -> PyResult {
    if !is_cdata(obj) {
        raise!(TypeError, "byref() argument must be a ctypes instance, not '{}'", tname(obj));
    }
    let a = new_carg(b'P', libffi::types().pointer)?;
    let c = carg(&a);
    c.set_ptr(unsafe { cd(obj).b_ptr.offset(offset) });
    carg_set_obj(&a, obj.to_ref());
    Ok(a)
}

// ------------------------------------------------------------------ CFuncPtr object

#[repr(C)]
pub struct CFuncPtrObject {
    pub base: CDataObject,
    pub thunk: *mut PyObject,
    pub callable: *mut PyObject,
    pub converters: *mut PyObject,
    pub argtypes: *mut PyObject,
    pub restype: *mut PyObject,
    pub checker: *mut PyObject,
    pub errcheck: *mut PyObject,
    pub paramflags: *mut PyObject,
}

pub fn fp(o: &PyObject) -> &'static mut CFuncPtrObject {
    unsafe { &mut *(o.as_ptr() as *mut CFuncPtrObject) }
}

pub static mut FUNCPTR_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut FUNCPTR_META: PyTypeObject = PyTypeObject::ZERO;
pub static mut THUNK_TYPE: PyTypeObject = PyTypeObject::ZERO;

pub fn funcptr_type() -> *mut PyTypeObject {
    &raw mut FUNCPTR_TYPE
}
pub fn funcptr_meta() -> *mut PyTypeObject {
    &raw mut FUNCPTR_META
}

fn set_slot(slot: &mut *mut PyObject, v: Option<PyRef>) {
    let old = *slot;
    *slot = v.map_or(std::ptr::null_mut(), |x| x.into_raw());
    unsafe { xdecref(old) };
}

fn opt_ref(p: *mut PyObject) -> Option<PyRef> {
    if p.is_null() { None } else { Some(unsafe { PyRef::from_borrowed(p) }) }
}

unsafe extern "C" fn funcptr_dealloc(op: *mut PyObject) {
    unsafe {
        let f = &mut *(op as *mut CFuncPtrObject);
        for s in [&mut f.thunk, &mut f.callable, &mut f.converters, &mut f.argtypes, &mut f.restype, &mut f.checker, &mut f.errcheck, &mut f.paramflags] {
            set_slot(s, None);
        }
        let d = (*cdata_type()).tp_dealloc.unwrap();
        d(op);
    }
}

unsafe extern "C" fn funcptr_traverse(op: *mut PyObject, v: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let f = &*(op as *mut CFuncPtrObject);
        let g = v.unwrap();
        for p in [f.thunk, f.callable, f.converters, f.argtypes, f.restype, f.checker, f.errcheck, f.paramflags, f.base.b_objects, f.base.b_base] {
            if !p.is_null() {
                let r = g(p, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        0
    }
}

unsafe extern "C" fn funcptr_clear(op: *mut PyObject) -> c_int {
    unsafe {
        let f = &mut *(op as *mut CFuncPtrObject);
        for s in [&mut f.thunk, &mut f.callable, &mut f.converters, &mut f.argtypes, &mut f.restype, &mut f.checker, &mut f.errcheck, &mut f.paramflags, &mut f.base.b_objects] {
            set_slot(s, None);
        }
    }
    0
}

fn func_addr(o: &PyObject) -> *mut c_void {
    unsafe { *(cd(o).b_ptr as *const *mut c_void) }
}

/// Validate `_argtypes_` and return the tuple of from_param converters.
pub fn converters_from_argtypes(argtypes: &PyObject) -> PyResult<(PyRef, PyRef)> {
    let t = match from_c(unsafe { capi::PySequence_Tuple(argtypes.as_ptr()) }) {
        Ok(t) => t,
        Err(_) => {
            rpy_runtime::errors::clear_exc();
            raise!(TypeError, "_argtypes_ must be a sequence of types");
        }
    };
    let n = tuple::tuple_len(&t);
    if n > 1024 {
        return Err(set_err(argument_error(), &format!("_argtypes_ has too many arguments ({}), maximum is 1024", n)));
    }
    let mut convs = Vec::with_capacity(n);
    for (i, tp) in tuple::tuple_items(&t).iter().enumerate() {
        if let Some(info) = stginfo_of_type(tp) {
            if info.kind == Kind::Struct || info.kind == Kind::Union {
                if info.dict_flags & TYPEFLAG_HASUNION != 0 {
                    raise!(TypeError, "item {} in _argtypes_ passes a union by value, which is unsupported.", i + 1);
                }
                if info.dict_flags & TYPEFLAG_HASBITFIELD != 0 {
                    raise!(TypeError, "item {} in _argtypes_ passes a struct/union with a bitfield by value, which is unsupported.", i + 1);
                }
            }
        }
        match getattr_opt(tp, "from_param")? {
            Some(c) => convs.push(c),
            None => raise!(TypeError, "item {} in _argtypes_ has no from_param method", i + 1),
        }
    }
    Ok((t, tuple::new_tuple(convs)))
}

// ---- getsets

fn fp_errcheck_get(o: &PyObject) -> PyResult {
    Ok(opt_ref(fp(o).errcheck).unwrap_or_else(none))
}
fn fp_errcheck_set(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    if let Some(v) = v {
        if !is_none(v) && !rpy_runtime::abstract_::is_callable(v) {
            raise!(TypeError, "the errcheck attribute must be callable");
        }
    }
    set_slot(&mut fp(o).errcheck, v.filter(|x| !is_none(x)).map(|x| x.to_ref()));
    Ok(())
}

fn fp_restype_get(o: &PyObject) -> PyResult {
    let f = fp(o);
    if !f.restype.is_null() {
        return Ok(opt_ref(f.restype).unwrap());
    }
    if let Some(i) = stginfo_of_obj(o) {
        if let Some(r) = &i.restype {
            return Ok(r.clone());
        }
    }
    Ok(none())
}
fn fp_restype_set(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let f = fp(o);
    match v {
        None => {
            set_slot(&mut f.restype, None);
            set_slot(&mut f.checker, None);
        }
        Some(v) => {
            if !is_none(v) && stginfo_of_type(v).is_none() && !rpy_runtime::abstract_::is_callable(v) {
                raise!(TypeError, "restype must be a type, a callable, or None");
            }
            let checker = getattr_opt(v, "_check_retval_")?;
            set_slot(&mut f.restype, Some(v.to_ref()));
            set_slot(&mut f.checker, checker);
        }
    }
    Ok(())
}

fn fp_argtypes_get(o: &PyObject) -> PyResult {
    let f = fp(o);
    if !f.argtypes.is_null() {
        return Ok(opt_ref(f.argtypes).unwrap());
    }
    if let Some(i) = stginfo_of_obj(o) {
        if let Some(r) = &i.argtypes {
            return Ok(r.clone());
        }
    }
    Ok(none())
}
fn fp_argtypes_set(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let f = fp(o);
    match v {
        Some(v) if !is_none(v) => {
            let (t, c) = converters_from_argtypes(v)?;
            if !f.paramflags.is_null() {
                validate_paramflags_with(Some(&t), Some(r(f.paramflags)))?;
            }
            set_slot(&mut f.argtypes, Some(t));
            set_slot(&mut f.converters, Some(c));
        }
        _ => {
            set_slot(&mut f.argtypes, None);
            set_slot(&mut f.converters, None);
        }
    }
    Ok(())
}

unsafe extern "C" fn funcptr_repr(op: *mut PyObject) -> *mut PyObject {
    let o = r(op);
    new_str(&format!("<{} object at {:#x}>", type_name(o.type_ptr()), op as usize)).into_raw()
}

unsafe extern "C" fn funcptr_bool(op: *mut PyObject) -> c_int {
    if func_addr(r(op)).is_null() { 0 } else { 1 }
}

static mut FUNCPTR_AS_NUMBER: PyNumberMethods = unsafe { std::mem::zeroed() };

// ---- construction

const PARAMFLAG_FIN: i64 = 1;
const PARAMFLAG_FOUT: i64 = 2;
const PARAMFLAG_FLCID: i64 = 4;

fn validate_paramflags(tp: *mut PyTypeObject, paramflags: Option<&PyObject>) -> PyResult<()> {
    let argtypes = stginfo(tp).and_then(|i| i.argtypes.clone());
    validate_paramflags_with(argtypes.as_deref(), paramflags)
}

fn validate_paramflags_with(argtypes: Option<&PyObject>, paramflags: Option<&PyObject>) -> PyResult<()> {
    let pf = match paramflags {
        None => return Ok(()),
        Some(p) if is_none(p) => return Ok(()),
        Some(p) => p,
    };
    if !is_tuple(pf) {
        raise!(TypeError, "paramflags must be a tuple or None");
    }
    let argtypes = match argtypes {
        Some(a) => a.to_ref(),
        None => raise!(TypeError, "paramflags require argtypes"),
    };
    if tuple::tuple_len(pf) != tuple::tuple_len(&argtypes) {
        raise!(ValueError, "paramflags must have the same length as argtypes");
    }
    for (i, item) in tuple::tuple_items(pf).iter().enumerate() {
        let ok = is_tuple(item) && (1..=3).contains(&tuple::tuple_len(item)) && is_int(tuple::tuple_get(item, 0));
        if !ok {
            raise!(TypeError, "paramflags must be a sequence of (int [,string [,value]]) tuples");
        }
        if tuple::tuple_len(item) >= 2 {
            let nm = tuple::tuple_get(item, 1);
            if !is_none(nm) && !is_str(nm) {
                raise!(TypeError, "paramflags must be a sequence of (int [,string [,value]]) tuples");
            }
        }
        let flag = rpy_runtime::types::int::as_i64(tuple::tuple_get(item, 0)).unwrap_or(-1);
        let typ = tuple::tuple_get(&argtypes, i);
        match flag & (PARAMFLAG_FIN | PARAMFLAG_FOUT | PARAMFLAG_FLCID) {
            0 | 1 | 4 | 5 => {}
            2 | 3 => {
                let ok = stginfo_of_type(typ).map_or(false, |ti| {
                    ti.kind == Kind::Pointer || ti.kind == Kind::Array || (ti.kind == Kind::Simple && matches!(ti.code, b'P' | b'z' | b'Z'))
                });
                if !ok {
                    raise!(TypeError, "'out' parameter {} must be a pointer type, not {}", i + 1, type_name(typ.as_ptr() as *mut PyTypeObject));
                }
            }
            _ => raise!(TypeError, "paramflag value {} not supported", flag),
        }
    }
    Ok(())
}

unsafe extern "C" fn funcptr_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let a = rpy_runtime::modules::fncommon::targs(args);
        into_c(funcptr_new_impl(tp, a, rpy_runtime::modules::fncommon::opt(kw)))
    }
}

fn new_funcptr_obj(tp: *mut PyTypeObject) -> PyResult {
    let o = generic_new(tp)?;
    Ok(o)
}

fn funcptr_new_impl(tp: *mut PyTypeObject, a: &[&PyObject], _kw: Option<&PyObject>) -> PyResult {
    if a.is_empty() {
        return new_funcptr_obj(tp);
    }
    if is_tuple(a[0]) {
        return funcptr_from_dll(tp, a);
    }
    if a.len() == 1 && is_int(a[0]) {
        let addr = unsafe { capi::PyLong_AsVoidPtr(a[0].as_ptr()) };
        if addr.is_null() && err_occurred() {
            return Err(PyErr);
        }
        let o = new_funcptr_obj(tp)?;
        unsafe { *(cd(&o).b_ptr as *mut *mut c_void) = addr };
        return Ok(o);
    }
    let callable = a[0];
    if !rpy_runtime::abstract_::is_callable(callable) {
        raise!(TypeError, "argument must be callable or integer function address");
    }
    let info = match stginfo(tp) {
        Some(i) if i.argtypes.is_some() => i,
        _ => raise!(TypeError, "cannot construct instance of this class: no argtypes"),
    };
    let thunk = alloc_callback(callable, info.argtypes.as_ref().unwrap(), info.restype.as_deref(), info.flags)?;
    let o = new_funcptr_obj(tp)?;
    let code = thunk_ref(&thunk).code;
    unsafe { *(cd(&o).b_ptr as *mut *mut c_void) = code };
    let f = fp(&o);
    set_slot(&mut f.callable, Some(callable.to_ref()));
    set_slot(&mut f.thunk, Some(thunk.clone()));
    keep_ref(&o, 0, Some(thunk))?;
    Ok(o)
}

fn funcptr_from_dll(tp: *mut PyTypeObject, a: &[&PyObject]) -> PyResult {
    if a.len() > 2 {
        raise!(TypeError, "function takes at most 2 arguments ({} given)", a.len());
    }
    let ftuple = a[0];
    let paramflags = a.get(1).copied();
    if tuple::tuple_len(ftuple) != 2 {
        raise!(TypeError, "illegal func_spec argument");
    }
    let name = tuple::tuple_get(ftuple, 0);
    let dll = tuple::tuple_get(ftuple, 1);
    let handle_obj = getattr(dll, "_handle")?;
    if !is_int(&handle_obj) {
        raise!(TypeError, "the _handle attribute of the second argument must be an integer");
    }
    let handle = unsafe { capi::PyLong_AsVoidPtr(handle_obj.as_ptr()) };
    if handle.is_null() && err_occurred() {
        raise!(ValueError, "could not convert the _handle attribute to a pointer");
    }
    let nm = if is_str(name) {
        str_of(name)
    } else if is_bytes(name) {
        String::from_utf8_lossy(bytes::bytes_data(name)).into_owned()
    } else if is_int(name) {
        raise!(TypeError, "function ordinal {} not found", repr(name)?);
    } else {
        raise!(TypeError, "function name must be string, bytes object or integer");
    };
    validate_paramflags(tp, paramflags)?;
    unsafe {
        if capi::PySys_Audit(cs!("ctypes.dlsym"), cs!("OO"), dll.as_ptr(), name.as_ptr()) < 0 {
            return Err(PyErr);
        }
    }
    let cn = cstring(&nm);
    let addr = unsafe {
        libc::dlerror();
        libc::dlsym(handle, cn.as_ptr())
    };
    if addr.is_null() {
        let e = crate::dl::dlerror_string();
        if e.is_empty() {
            raise!(AttributeError, "function '{}' not found", nm);
        }
        raise!(AttributeError, "{}", e);
    }
    let o = new_funcptr_obj(tp)?;
    unsafe { *(cd(&o).b_ptr as *mut *mut c_void) = addr };
    if let Some(p) = paramflags {
        if !is_none(p) {
            set_slot(&mut fp(&o).paramflags, Some(p.to_ref()));
        }
    }
    keep_ref(&o, -1, Some(dll.to_ref()))?;
    Ok(o)
}

// ------------------------------------------------------------------ call machinery

pub fn argument_error() -> *mut PyObject {
    module::argument_error_type()
}

/// One converted argument.
pub struct Arg {
    pub ffi: *mut FfiType,
    pub value: [u128; 2],
    pub by_ptr: *mut u8,
    pub keep: Option<PyRef>,
}

impl Arg {
    fn new() -> Arg {
        Arg { ffi: libffi::types().pointer, value: [0; 2], by_ptr: std::ptr::null_mut(), keep: None }
    }
    fn set_ptr(&mut self, p: *mut u8) {
        unsafe { *(self.value.as_mut_ptr() as *mut *mut u8) = p };
    }
    fn valptr(&mut self) -> *mut c_void {
        if !self.by_ptr.is_null() { self.by_ptr as *mut c_void } else { self.value.as_mut_ptr() as *mut c_void }
    }
}

/// Convert a Python object (already passed through from_param) into a C argument.
fn conv_param(obj: &PyObject, index: usize, depth: usize) -> PyResult<Arg> {
    let mut a = Arg::new();
    if is_carg(obj) {
        let c = carg(obj);
        a.ffi = c.pffi_type;
        a.value = c.value;
        if c.tag == b'V' {
            a.by_ptr = c.ptr();
        }
        a.keep = Some(obj.to_ref());
        return Ok(a);
    }
    if is_cdata(obj) {
        let info = match stginfo_of_obj(obj) {
            Some(i) => i,
            None => raise!(TypeError, "Don't know how to convert parameter {}", index + 1),
        };
        let c = cd(obj);
        a.keep = Some(obj.to_ref());
        match info.kind {
            Kind::Simple => {
                a.ffi = simple::code_ffi(info.code);
                let n = (info.size.max(0) as usize).min(32);
                unsafe { std::ptr::copy_nonoverlapping(c.b_ptr, a.value.as_mut_ptr() as *mut u8, n) };
            }
            Kind::Pointer | Kind::FuncPtr => {
                a.set_ptr(unsafe { *(c.b_ptr as *const *mut u8) });
            }
            Kind::Array => {
                a.set_ptr(c.b_ptr);
            }
            Kind::Struct | Kind::Union => {
                a.ffi = info.ffi_ptr();
                a.by_ptr = c.b_ptr;
            }
        }
        return Ok(a);
    }
    if is_none(obj) {
        a.set_ptr(std::ptr::null_mut());
        return Ok(a);
    }
    if is_int(obj) {
        a.ffi = libffi::types().sint32;
        let v = match int_mask(obj) {
            Ok(v) => v,
            Err(_) => {
                rpy_runtime::errors::clear_exc();
                raise!(OverflowError, "int too long to convert");
            }
        };
        unsafe { *(a.value.as_mut_ptr() as *mut i32) = v as i32 };
        return Ok(a);
    }
    if is_bytes(obj) {
        a.set_ptr(bytes::bytes_ptr(obj));
        a.keep = Some(obj.to_ref());
        return Ok(a);
    }
    if is_str(obj) {
        let w = simple::str_to_wide_bytes(obj)?;
        a.set_ptr(bytes::bytes_ptr(&w));
        a.keep = Some(w);
        return Ok(a);
    }
    if let Some(asp) = getattr_opt(obj, "_as_parameter_")? {
        if depth > 100 {
            raise!(RecursionError, "maximum recursion depth exceeded while processing _as_parameter_");
        }
        return conv_param(&asp, index, depth + 1);
    }
    raise!(TypeError, "Don't know how to convert parameter {}", index + 1)
}

/// Wrap the current exception as `ArgumentError("argument N: Type: msg")`.
fn wrap_argument_error(index: usize) -> PyErr {
    let e = match rpy_runtime::errors::take_exc() {
        Some(e) => e,
        None => return PyErr,
    };
    let tn = type_name(e.type_ptr());
    let msg = rpy_runtime::abstract_::str_string(&e).unwrap_or_default();
    let full = if msg.is_empty() { format!("argument {}: {}", index + 1, tn) } else { format!("argument {}: {}: {}", index + 1, tn, msg) };
    let err = set_err(argument_error(), &full);
    if let Some(ne) = rpy_runtime::errors::take_exc() {
        rpy_runtime::types::exceptions::set_exc_cause(&ne, Some(&e));
        rpy_runtime::types::exceptions::set_context_and_cause(&ne, Some(&e));
        rpy_runtime::errors::set_exc(ne);
    }
    err
}

thread_local! {
    pub static CTYPES_ERRNO: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
}

/// Return-value storage large enough for any type (incl. long double complex).
#[repr(C, align(16))]
struct RetBuf([u8; 64]);

/// Call a C function. `argtypes` only decides variadic handling.
pub fn callproc(
    addr: *mut c_void,
    args: &[PyRef],
    flags: i32,
    nfixed: Option<usize>,
    restype: Option<&PyObject>,
    checker: Option<&PyObject>,
) -> PyResult {
    let lib = match libffi::lib() {
        Ok(l) => l,
        Err(e) => raise!(OSError, "libffi is not available: {}", e),
    };
    if addr.is_null() {
        raise!(ValueError, "NULL pointer access");
    }
    unsafe {
        if capi::PySys_Audit(cs!("ctypes.call_function"), cs!("nO"), addr as isize, tuple::new_tuple(args.to_vec()).as_ptr()) < 0 {
            return Err(PyErr);
        }
    }
    let mut cargs: Vec<Arg> = Vec::with_capacity(args.len());
    for (i, a) in args.iter().enumerate() {
        match conv_param(a, i, 0) {
            Ok(x) => cargs.push(x),
            Err(_) => return Err(wrap_argument_error(i)),
        }
    }
    // result type
    let t = libffi::types();
    let (rtype, rinfo): (*mut FfiType, Option<&StgInfo>) = match restype {
        None => (t.sint32, None),
        Some(r) if is_none(r) => (t.void, None),
        Some(r) => match stginfo_of_type(r) {
            Some(i) => (if i.kind == Kind::Simple { simple::code_ffi(i.code) } else if i.kind == Kind::Struct || i.kind == Kind::Union { i.ffi_ptr() } else { t.pointer }, Some(&*i)),
            None => (t.sint32, None),
        },
    };
    let mut atypes: Vec<*mut FfiType> = cargs.iter().map(|a| a.ffi).collect();
    let mut avalues: Vec<*mut c_void> = cargs.iter_mut().map(|a| a.valptr()).collect();
    let mut cif = FfiCif::new();
    let st = unsafe {
        match nfixed {
            Some(nf) if nf < atypes.len() => {
                (lib.ffi_prep_cif_var.unwrap())(&mut cif, libffi::FFI_DEFAULT_ABI, nf as u32, atypes.len() as u32, rtype, atypes.as_mut_ptr())
            }
            _ => (lib.ffi_prep_cif.unwrap())(&mut cif, libffi::FFI_DEFAULT_ABI, atypes.len() as u32, rtype, atypes.as_mut_ptr()),
        }
    };
    if st != 0 {
        raise!(RuntimeError, "ffi_prep_cif failed");
    }
    let mut ret = RetBuf([0; 64]);
    let ffi_call = lib.ffi_call.unwrap();
    let use_errno = flags & FUNCFLAG_USE_ERRNO != 0;
    let pyapi = flags & FUNCFLAG_PYTHONAPI != 0;
    let retp = ret.0.as_mut_ptr() as *mut c_void;
    let mut do_call = || unsafe {
        if use_errno {
            let e = CTYPES_ERRNO.with(|c| c.get());
            *libc::__errno_location() = e;
        }
        ffi_call(&mut cif, addr, retp, avalues.as_mut_ptr());
        if use_errno {
            let e = *libc::__errno_location();
            CTYPES_ERRNO.with(|c| c.set(e));
        }
    };
    if pyapi {
        do_call();
        if err_occurred() {
            return Err(PyErr);
        }
    } else {
        unsafe {
            let ts = capi::PyEval_SaveThread();
            do_call();
            capi::PyEval_RestoreThread(ts);
        }
        if err_occurred() {
            // an exception from a callback is reported as unraisable; nothing pending here
        }
    }
    drop(cargs);
    get_result(restype, rinfo, ret.0.as_mut_ptr(), checker)
}

fn get_result(restype: Option<&PyObject>, rinfo: Option<&StgInfo>, buf: *mut u8, checker: Option<&PyObject>) -> PyResult {
    let restype = match restype {
        None => return Ok(new_int(unsafe { *(buf as *const i32) } as i64)),
        Some(r) => r,
    };
    if is_none(restype) {
        return Ok(none());
    }
    let retval = match rinfo {
        None => {
            let v = new_int(unsafe { *(buf as *const i32) } as i64);
            return call(restype, &[&v]);
        }
        Some(info) => {
            let tpp = restype.as_ptr() as *mut PyTypeObject;
            if info.kind == Kind::Simple && !simple_instance(tpp) {
                if info.code == b'O' {
                    // the function returned a new reference
                    let p = unsafe { *(buf as *const *mut PyObject) };
                    if p.is_null() {
                        if err_occurred() {
                            return Err(PyErr);
                        }
                        raise!(ValueError, "PyObject is NULL");
                    }
                    unsafe { PyRef::from_raw(p) }
                } else {
                    simple::get(info.code, info.swapped, buf, info.size, None)?
                }
            } else {
                from_base_obj(tpp, None, 0, buf)?
            }
        }
    };
    match checker {
        Some(c) if !is_none(c) => call(c, &[&retval]),
        _ => Ok(retval),
    }
}

// ---- tp_call

unsafe extern "C" fn funcptr_call(op: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let a = rpy_runtime::modules::fncommon::targs(args);
        into_c(funcptr_call_impl(&*op, args, a, rpy_runtime::modules::fncommon::opt(kw)))
    }
}

fn funcptr_call_impl(o: &PyObject, args_tuple: *mut PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let f = fp(o);
    let info = match stginfo_of_obj(o) {
        Some(i) => i,
        None => raise!(TypeError, "abstract class"),
    };
    let restype = opt_ref(f.restype).or_else(|| info.restype.clone());
    let converters = opt_ref(f.converters).or_else(|| info.converters.clone());
    let checker = opt_ref(f.checker).or_else(|| info.checker.clone());
    let argtypes = opt_ref(f.argtypes).or_else(|| info.argtypes.clone());
    let errcheck = opt_ref(f.errcheck).or_else(|| info.errcheck.clone());
    let paramflags = opt_ref(f.paramflags);
    let addr = func_addr(o);
    if addr.is_null() {
        raise!(ValueError, "NULL pointer access");
    }
    if args.len() > 1024 {
        return Err(set_err(argument_error(), &format!("too many arguments ({}), maximum is 1024", args.len())));
    }
    // callargs + output bookkeeping
    let (callargs, outmask, inoutmask, numretvals) = build_callargs(o, argtypes.as_deref(), paramflags.as_deref(), args_tuple, args, kw)?;
    let n = tuple::tuple_len(&callargs);
    if let Some(c) = &converters {
        let required = tuple::tuple_len(c);
        if info.flags & FUNCFLAG_CDECL != 0 {
            if required > n {
                raise!(TypeError, "this function takes at least {} argument{} ({} given)", required, if required == 1 { "" } else { "s" }, n);
            }
        } else if required != n {
            raise!(TypeError, "this function takes {} argument{} ({} given)", required, if required == 1 { "" } else { "s" }, n);
        }
    }
    // convert through from_param
    let mut conv: Vec<PyRef> = Vec::with_capacity(n);
    for (i, a) in tuple::tuple_items(&callargs).iter().enumerate() {
        match &converters {
            Some(c) if i < tuple::tuple_len(c) => {
                let cv = tuple::tuple_get(c, i);
                match call(cv, &[a]) {
                    Ok(v) => conv.push(v),
                    Err(_) => return Err(wrap_argument_error(i)),
                }
            }
            _ => conv.push(a.to_ref()),
        }
    }
    let nfixed = converters.as_ref().map(|c| tuple::tuple_len(c));
    let result = callproc(addr, &conv, info.flags, nfixed, restype.as_deref(), checker.as_deref())?;
    if let Some(ec) = errcheck {
        let v = call(&ec, &[&result, o, &callargs])?;
        if !v.is(&callargs) {
            return Ok(v);
        }
    }
    build_result(result, &callargs, outmask, inoutmask, numretvals)
}

fn build_callargs(
    _o: &PyObject,
    argtypes: Option<&PyObject>,
    paramflags: Option<&PyObject>,
    args_tuple: *mut PyObject,
    args: &[&PyObject],
    kw: Option<&PyObject>,
) -> PyResult<(PyRef, u64, u64, usize)> {
    let (pf, at) = match (paramflags, argtypes) {
        (Some(p), Some(a)) => (p, a),
        _ => {
            if let Some(k) = kw {
                if dict::dict_len(k) > 0 {
                    raise!(TypeError, "call takes no keyword arguments");
                }
            }
            let t = if args_tuple.is_null() { tuple::new_tuple_from_refs(args) } else { unsafe { PyRef::from_borrowed(args_tuple) } };
            return Ok((t, 0, 0, 0));
        }
    };
    let len = tuple::tuple_len(pf);
    let mut out: Vec<PyRef> = Vec::with_capacity(len);
    let mut outmask = 0u64;
    let mut inoutmask = 0u64;
    let mut numretvals = 0usize;
    let mut inargs_index = 0usize;
    let mut kwused = 0usize;
    for i in 0..len {
        let item = tuple::tuple_get(pf, i);
        let flag = rpy_runtime::types::int::as_i64(tuple::tuple_get(item, 0)).unwrap_or(0);
        let name = if tuple::tuple_len(item) > 1 { Some(tuple::tuple_get(item, 1)) } else { None };
        let defval = if tuple::tuple_len(item) > 2 { Some(tuple::tuple_get(item, 2)) } else { None };
        let get_input = |inargs_index: &mut usize, kwused: &mut usize| -> PyResult<PyRef> {
            if *inargs_index < args.len() {
                let v = args[*inargs_index].to_ref();
                *inargs_index += 1;
                return Ok(v);
            }
            if let (Some(k), Some(n)) = (kw, name) {
                if !is_none(n) {
                    if let Some(v) = dict::get_item_str(k, n) {
                        *kwused += 1;
                        return Ok(v);
                    }
                }
            }
            if let Some(d) = defval {
                return Ok(d.to_ref());
            }
            match name {
                Some(n) if !is_none(n) => raise!(TypeError, "required argument '{}' missing", str_of(n)),
                _ => raise!(TypeError, "not enough arguments"),
            }
        };
        match flag & (PARAMFLAG_FIN | PARAMFLAG_FOUT | PARAMFLAG_FLCID) {
            v if v == PARAMFLAG_FIN | PARAMFLAG_FLCID => {
                let x = match defval {
                    Some(d) => d.to_ref(),
                    None => new_int(0),
                };
                out.push(x);
            }
            v if v == PARAMFLAG_FIN | PARAMFLAG_FOUT => {
                inoutmask |= 1 << i;
                numretvals += 1;
                out.push(get_input(&mut inargs_index, &mut kwused)?);
            }
            0 | PARAMFLAG_FIN => {
                out.push(get_input(&mut inargs_index, &mut kwused)?);
            }
            PARAMFLAG_FOUT => {
                if let Some(d) = defval {
                    // an explicit default for an out parameter is passed as-is
                    outmask |= 1 << i;
                    numretvals += 1;
                    out.push(d.to_ref());
                    continue;
                }
                let typ = tuple::tuple_get(at, i);
                let tinfo = match stginfo_of_type(typ) {
                    Some(ti) => ti,
                    None => raise!(TypeError, "NULL stgdict unexpected"),
                };
                if tinfo.kind == Kind::Simple {
                    raise!(TypeError, "{} 'out' parameter must be passed as default value", type_name(typ.as_ptr() as *mut PyTypeObject));
                }
                // an instance of the target type; the converter passes it by reference
                let ob = if tinfo.kind == Kind::Array {
                    call(typ, &[])?
                } else if let Some(p) = &tinfo.proto {
                    call(p, &[])?
                } else {
                    raise!(TypeError, "'out' parameter {} must be a pointer type", i + 1);
                };
                outmask |= 1 << i;
                numretvals += 1;
                out.push(ob);
            }
            _ => raise!(ValueError, "paramflag {} not yet implemented", flag),
        }
    }
    let nkw = kw.map_or(0, |k| dict::dict_len(k));
    if inargs_index < args.len() {
        raise!(TypeError, "call takes exactly {} arguments ({} given)", inargs_index, args.len());
    }
    if kwused < nkw {
        raise!(TypeError, "call takes exactly {} arguments ({} given)", inargs_index, args.len() + nkw);
    }
    Ok((tuple::new_tuple(out), outmask, inoutmask, numretvals))
}

fn build_result(result: PyRef, callargs: &PyObject, outmask: u64, inoutmask: u64, numretvals: usize) -> PyResult {
    if outmask == 0 && inoutmask == 0 {
        return Ok(result);
    }
    let mut vals: Vec<PyRef> = Vec::with_capacity(numretvals);
    for (i, v) in tuple::tuple_items(callargs).iter().enumerate() {
        if i >= 64 {
            break;
        }
        let bit = 1u64 << i;
        if inoutmask & bit != 0 {
            vals.push(v.to_ref());
        } else if outmask & bit != 0 {
            let base = if is_carg(v) && !carg(v).obj.is_null() { unsafe { PyRef::from_borrowed(carg(v).obj) } } else { v.to_ref() };
            let x = rpy_runtime::abstract_::call_method_str(&base, "__ctypes_from_outparam__", &[])?;
            vals.push(x);
        }
    }
    if vals.len() == 1 {
        return Ok(vals.pop().unwrap());
    }
    Ok(tuple::new_tuple(vals))
}

// ------------------------------------------------------------------ callbacks (thunks)

pub struct Thunk {
    pub closure: *mut c_void,
    pub code: *mut c_void,
    pub cif: Box<FfiCif>,
    pub atypes: Vec<*mut FfiType>,
    pub callable: PyRef,
    pub converters: PyRef,
    pub restype: Option<PyRef>,
    pub rcode: u8,
    pub rsize: usize,
    pub flags: i32,
}
unsafe impl Send for Thunk {}

impl rpy_runtime::modules::fncommon::Trav for Thunk {
    fn trav(&self, v: &mut dyn FnMut(*mut PyObject) -> c_int) -> c_int {
        for o in [Some(&self.callable), Some(&self.converters), self.restype.as_ref()] {
            if let Some(r) = o {
                let x = v(r.as_ptr());
                if x != 0 {
                    return x;
                }
            }
        }
        0
    }
}

impl Drop for Thunk {
    fn drop(&mut self) {
        if !self.closure.is_null() {
            if let Ok(l) = libffi::lib() {
                unsafe { (l.ffi_closure_free.unwrap())(self.closure) };
            }
            self.closure = std::ptr::null_mut();
        }
    }
}

pub fn thunk_ref(o: &PyObject) -> &'static mut Thunk {
    unsafe { &mut *(rpy_runtime::modules::fncommon::rs::<Thunk>(o) as *mut Thunk) }
}

fn alloc_callback(callable: &PyObject, argtypes: &PyObject, restype: Option<&PyObject>, flags: i32) -> PyResult {
    let lib = match libffi::lib() {
        Ok(l) => l,
        Err(e) => raise!(OSError, "libffi is not available: {}", e),
    };
    let t = libffi::types();
    let mut atypes: Vec<*mut FfiType> = Vec::new();
    for tp in tuple::tuple_items(argtypes) {
        let ft = match stginfo_of_type(tp) {
            Some(i) => match i.kind {
                Kind::Simple => simple::code_ffi(i.code),
                Kind::Struct | Kind::Union => i.ffi_ptr(),
                _ => t.pointer,
            },
            None => t.pointer,
        };
        atypes.push(ft);
    }
    let (rtype, rcode, rsize) = match restype {
        None => (t.void, 0u8, 0usize),
        Some(r) if is_none(r) => (t.void, 0, 0),
        Some(r) => match stginfo_of_type(r) {
            Some(i) if i.kind == Kind::Simple => (simple::code_ffi(i.code), i.code, i.size as usize),
            _ => raise!(TypeError, "invalid result type for callback function"),
        },
    };
    let mut cif = Box::new(FfiCif::new());
    let st = unsafe { (lib.ffi_prep_cif.unwrap())(&mut *cif, libffi::FFI_DEFAULT_ABI, atypes.len() as u32, rtype, atypes.as_mut_ptr()) };
    if st != 0 {
        raise!(RuntimeError, "ffi_prep_cif failed with {}", st);
    }
    let mut code: *mut c_void = std::ptr::null_mut();
    let closure = unsafe { (lib.ffi_closure_alloc.unwrap())(64, &mut code) };
    if closure.is_null() {
        raise!(MemoryError, "unable to allocate closure");
    }
    let thunk = Thunk {
        closure,
        code,
        cif,
        atypes,
        callable: callable.to_ref(),
        converters: argtypes.to_ref(),
        restype: restype.map(|r| r.to_ref()),
        rcode,
        rsize,
        flags,
    };
    let obj = rpy_runtime::modules::fncommon::robj_new(&raw mut THUNK_TYPE, thunk)?;
    let th = thunk_ref(&obj);
    // the cif's arg_types must point into the (now moved) vector
    th.cif.arg_types = th.atypes.as_mut_ptr();
    let st = unsafe { (lib.ffi_prep_closure_loc.unwrap())(th.closure, &mut *th.cif, closure_fn, obj.as_ptr() as *mut c_void, th.code) };
    if st != 0 {
        raise!(RuntimeError, "ffi_prep_closure failed with {}", st);
    }
    Ok(obj)
}

unsafe extern "C" fn closure_fn(_cif: *mut FfiCif, resp: *mut c_void, args: *mut *mut c_void, user: *mut c_void) {
    unsafe {
        let g = capi::PyGILState_Ensure();
        let obj = &*(user as *mut PyObject);
        let th = thunk_ref(obj);
        let use_errno = th.flags & FUNCFLAG_USE_ERRNO != 0;
        if use_errno {
            // swap the C errno into the ctypes-private copy
            let e = *libc::__errno_location();
            let old = CTYPES_ERRNO.with(|c| c.replace(e));
            *libc::__errno_location() = old;
        }
        run_callback(th, resp as *mut u8, args);
        if use_errno {
            let e = *libc::__errno_location();
            let old = CTYPES_ERRNO.with(|c| c.replace(e));
            *libc::__errno_location() = old;
        }
        capi::PyGILState_Release(g);
    }
}

unsafe fn run_callback(th: &Thunk, resp: *mut u8, args: *mut *mut c_void) {
    unsafe {
        // zero the result first (also covers error paths)
        if th.rsize > 0 {
            std::ptr::write_bytes(resp, 0, th.rsize.max(8));
        }
        let mut pyargs: Vec<PyRef> = Vec::new();
        for (i, tp) in tuple::tuple_items(&th.converters).iter().enumerate() {
            let p = *args.add(i) as *mut u8;
            let v = match stginfo_of_type(tp) {
                Some(info) if info.kind == Kind::Simple && !simple_instance(tp.as_ptr() as *mut PyTypeObject) => {
                    simple::get(info.code, info.swapped, p, info.size, None)
                }
                Some(_) => from_base_obj(tp.as_ptr() as *mut PyTypeObject, None, 0, p),
                None => Err(err!(TypeError, "cannot build parameter")),
            };
            match v {
                Ok(v) => pyargs.push(v),
                Err(_) => {
                    report_unraisable("Exception ignored while creating argument {} for ctypes callback function", Some(i), &th.callable);
                    return;
                }
            }
        }
        let refs: Vec<&PyObject> = pyargs.iter().map(|x| &**x).collect();
        let result = match call(&th.callable, &refs) {
            Ok(r) => r,
            Err(_) => {
                report_unraisable("Exception ignored while calling ctypes callback function", None, &th.callable);
                return;
            }
        };
        if th.rcode == 0 {
            return;
        }
        // store the result (widened to ffi_arg for small integers)
        let mut tmp = [0u8; 32];
        match simple::set(th.rcode, false, tmp.as_mut_ptr(), &result, th.rsize as isize, None) {
            Ok(keep) => {
                if th.rcode == b'O' {
                    // the C caller receives a new reference
                    std::mem::forget(result.clone());
                } else if keep.is_some() {
                    let _ = capi::PyErr_WarnEx(capi::exc::RuntimeWarning(), cs!("memory leak in callback function."), 1);
                    if err_occurred() {
                        report_unraisable("Exception ignored while converting result of ctypes callback function", None, &th.callable);
                    }
                    std::mem::forget(keep);
                }
                widen_result(th.rcode, &tmp, resp);
            }
            Err(_) => {
                report_unraisable("Exception ignored while converting result of ctypes callback function", None, &th.callable);
            }
        }
    }
}

fn widen_result(code: u8, tmp: &[u8; 32], resp: *mut u8) {
    unsafe {
        let v: Option<u64> = match code {
            b'b' => Some(*(tmp.as_ptr() as *const i8) as i64 as u64),
            b'h' | b'v' => Some(*(tmp.as_ptr() as *const i16) as i64 as u64),
            b'i' => Some(*(tmp.as_ptr() as *const i32) as i64 as u64),
            b'B' | b'c' | b'?' => Some(*tmp.as_ptr() as u64),
            b'H' => Some(*(tmp.as_ptr() as *const u16) as u64),
            b'I' | b'u' => Some(*(tmp.as_ptr() as *const u32) as u64),
            _ => None,
        };
        match v {
            Some(v) => *(resp as *mut u64) = v,
            None => {
                let n = simple::code_size(code).map_or(8, |x| x.0);
                std::ptr::copy_nonoverlapping(tmp.as_ptr(), resp, n);
            }
        }
    }
}

fn report_unraisable(msg: &str, index: Option<usize>, callable: &PyObject) {
    let m = match index {
        Some(i) => msg.replace("{}", &i.to_string()),
        None => msg.to_string(),
    };
    let full = format!("{} %R", m.replace('%', "%%"));
    let c = cstring(&full);
    unsafe { capi::PyErr_FormatUnraisable(c.as_ptr(), callable.as_ptr()) };
}

// ------------------------------------------------------------------ metaclass

unsafe extern "C" fn funcptr_meta_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let tp_init = (*rpy_runtime::types::typeobject::type_type()).tp_init.unwrap();
        if tp_init(o, args, kw) < 0 {
            return -1;
        }
        int_into_c(funcptr_meta_init_impl(&*o))
    }
}

fn funcptr_meta_init_impl(cls: &PyObject) -> PyResult<()> {
    let tp = cls.as_ptr() as *mut PyTypeObject;
    if has_stginfo_slot_set(tp) {
        raise!(SystemError, "StgInfo of '{}' is already initialized.", type_name(tp));
    }
    let mut info = StgInfo::new(Kind::FuncPtr);
    info.size = 8;
    info.align = 8;
    info.length = 1;
    *info.ffi = unsafe { *libffi::types().pointer };
    info.dict_flags |= TYPEFLAG_ISPOINTER;
    info.format = Some("X{}".to_string());
    // class attributes (looked up through the MRO like CPython's stgdict copy)
    if let Some(flags) = getattr_opt(cls, "_flags_")? {
        if !is_int(&flags) {
            raise!(TypeError, "class must define _flags_ which must be an integer");
        }
        info.flags = rpy_runtime::types::int::as_i64(&flags).unwrap_or(0) as i32;
    } else {
        raise!(TypeError, "class must define _flags_ which must be an integer");
    }
    if let Some(at) = getattr_opt(cls, "_argtypes_")? {
        if !is_none(&at) {
            let (t, c) = converters_from_argtypes(&at)?;
            info.argtypes = Some(t);
            info.converters = Some(c);
        }
    }
    if let Some(rt) = getattr_opt(cls, "_restype_")? {
        if !is_none(&rt) && stginfo_of_type(&rt).is_none() && !rpy_runtime::abstract_::is_callable(&rt) {
            raise!(TypeError, "_restype_ must be a type, a callable, or None");
        }
        info.checker = getattr_opt(&rt, "_check_retval_")?;
        info.restype = Some(rt);
    }
    init_stginfo(tp, info)?;
    Ok(())
}

pub fn init_types() -> PyResult<()> {
    unsafe {
        let m = funcptr_meta();
        init_meta(m, c"_ctypes.PyCFuncPtrType", ctype_type());
        (*m).tp_init = Some(funcptr_meta_init);
        (*m).tp_methods = meta_methods(rpy_runtime::mdefs![("from_param", O, ct_from_param, "Convert a Python object into a function call parameter.")], vec![]);
        (*m).tp_doc = c"metatype for C function pointers".as_ptr();
        set_meta_number(m);
        rpy_runtime::types::typeobject::type_ready(m)?;

        let t = funcptr_type();
        init_base_type(t, c"_ctypes.CFuncPtr", m, std::mem::size_of::<CFuncPtrObject>());
        (*t).tp_new = Some(funcptr_new);
        (*t).tp_dealloc = Some(funcptr_dealloc);
        (*t).tp_traverse = Some(funcptr_traverse);
        (*t).tp_clear = Some(funcptr_clear);
        (*t).tp_call = Some(funcptr_call);
        (*t).tp_repr = Some(funcptr_repr);
        FUNCPTR_AS_NUMBER.nb_bool = Some(funcptr_bool);
        (*t).tp_as_number = &raw mut FUNCPTR_AS_NUMBER;
        (*t).tp_doc = c"Function Pointer".as_ptr();
        (*t).tp_getset = rpy_runtime::getset_table![
            ("errcheck", fp_errcheck_get, fp_errcheck_set; "a function to check for errors"),
            ("restype", fp_restype_get, fp_restype_set; "specify the result type"),
            ("argtypes", fp_argtypes_get, fp_argtypes_set; "specify the argument types"),
        ];
        rpy_runtime::types::typeobject::type_ready(t)?;

        let c = carg_type();
        rpy_runtime::modules::fncommon::init_type(c, c"CArgObject", std::mem::size_of::<CArgObject>(), Py_TPFLAGS_HAVE_GC);
        (*c).tp_dealloc = Some(carg_dealloc);
        (*c).tp_traverse = Some(carg_traverse);
        (*c).tp_clear = Some(carg_clear);
        (*c).tp_repr = Some(carg_repr);
        (*c).tp_getset = rpy_runtime::getset_table![("_obj", carg_obj_get)];
        rpy_runtime::types::typeobject::type_ready(c)?;

        rpy_runtime::modules::fncommon::init_robj_type::<Thunk>(&raw mut THUNK_TYPE, c"_ctypes.CThunkObject", 0);
        rpy_runtime::types::typeobject::type_ready(&raw mut THUNK_TYPE)?;
    }
    Ok(())
}
