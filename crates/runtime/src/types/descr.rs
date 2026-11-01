//! Descriptor types created for builtin (and extension) types:
//! method_descriptor, classmethod_descriptor, member_descriptor,
//! getset_descriptor, wrapper_descriptor, method-wrapper, and `__new__`.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::*;
use crate::types::{cfunction, int, none, str as pystr, tuple};
use crate::{name, raise};
use std::ffi::{CStr, c_int, c_void};

#[unsafe(no_mangle)]
pub static mut PyMethodDescr_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyClassMethodDescr_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyMemberDescr_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyGetSetDescr_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyWrapperDescr_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut _PyMethodWrapper_Type: PyTypeObject = PyTypeObject::ZERO;

pub fn method_descr_type() -> *mut PyTypeObject {
    &raw mut PyMethodDescr_Type
}
pub fn classmethod_descr_type() -> *mut PyTypeObject {
    &raw mut PyClassMethodDescr_Type
}
pub fn member_descr_type() -> *mut PyTypeObject {
    &raw mut PyMemberDescr_Type
}
pub fn getset_descr_type() -> *mut PyTypeObject {
    &raw mut PyGetSetDescr_Type
}
pub fn wrapper_descr_type() -> *mut PyTypeObject {
    &raw mut PyWrapperDescr_Type
}
pub fn method_wrapper_type() -> *mut PyTypeObject {
    &raw mut _PyMethodWrapper_Type
}

/// Strip a "name(sig)\n--\n\n" signature prefix. Returns None if the doc is only a signature.
pub fn doc_without_signature(doc: &str) -> Option<String> {
    if let Some(i) = find_sig_end(doc) {
        let rest = &doc[i..];
        if rest.is_empty() {
            return None;
        }
        return Some(rest.to_string());
    }
    Some(doc.to_string())
}

/// Return the text signature ("(a, b)") if the doc starts with one.
pub fn text_signature(name: &str, doc: &str) -> Option<String> {
    if !doc.starts_with(name) {
        return None;
    }
    let after = &doc[name.len()..];
    if !after.starts_with('(') {
        return None;
    }
    let end = after.find(")\n--\n\n")?;
    let sig = &after[..end + 1];
    Some(sig.replace("($self, ", "($self, ").to_string())
}

fn find_sig_end(doc: &str) -> Option<usize> {
    let p = doc.find(")\n--\n\n")?;
    let head = &doc[..p];
    if head.contains('\n') || !head.contains('(') {
        return None;
    }
    Some(p + 6)
}

unsafe fn alloc_descr<T>(tp: *mut PyTypeObject, owner: *mut PyTypeObject, nm: PyRef) -> *mut T {
    unsafe {
        let op = crate::alloc::alloc_object(tp, std::mem::size_of::<T>());
        let d = op as *mut PyDescrObject;
        incref(owner as *mut PyObject);
        (*d).d_type = owner;
        (*d).d_qualname = std::ptr::null_mut();
        (*d).d_name = nm.into_raw();
        crate::gc::track(op);
        op as *mut T
    }
}

fn cname(p: *const std::ffi::c_char) -> PyRef {
    pystr::intern_str(&unsafe { CStr::from_ptr(p) }.to_string_lossy())
}

pub fn new_method_descr(owner: *mut PyTypeObject, ml: *mut PyMethodDef) -> PyRef {
    unsafe {
        let d = alloc_descr::<PyMethodDescrObject>(method_descr_type(), owner, cname((*ml).ml_name));
        (*d).d_method = ml;
        (*d).vectorcall = Some(method_descr_vectorcall);
        PyRef::from_raw(d as *mut PyObject)
    }
}

pub fn new_classmethod_descr(owner: *mut PyTypeObject, ml: *mut PyMethodDef) -> PyRef {
    unsafe {
        let d = alloc_descr::<PyMethodDescrObject>(classmethod_descr_type(), owner, cname((*ml).ml_name));
        (*d).d_method = ml;
        (*d).vectorcall = None;
        PyRef::from_raw(d as *mut PyObject)
    }
}

pub fn new_member_descr(owner: *mut PyTypeObject, m: *mut PyMemberDef) -> PyRef {
    unsafe {
        let d = alloc_descr::<PyMemberDescrObject>(member_descr_type(), owner, cname((*m).name));
        (*d).d_member = m;
        PyRef::from_raw(d as *mut PyObject)
    }
}

pub fn new_getset_descr(owner: *mut PyTypeObject, g: *mut PyGetSetDef) -> PyRef {
    unsafe {
        let d = alloc_descr::<PyGetSetDescrObject>(getset_descr_type(), owner, cname((*g).name));
        (*d).d_getset = g;
        PyRef::from_raw(d as *mut PyObject)
    }
}

/// wrapperbase entries, one per slotdef (offset field = slotdef index).
fn wrapperbases() -> &'static [wrapperbase] {
    static TABLE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let p = *TABLE.get_or_init(|| {
        let n = crate::types::slots::len_of_slotdefs();
        let mut v: Vec<wrapperbase> = Vec::with_capacity(n);
        for i in 0..n {
            let sd = crate::types::slots::slotdef(i);
            v.push(wrapperbase {
                name: crate::methods::cstr_static(sd.name),
                offset: i as c_int,
                function: std::ptr::null_mut(),
                wrapper: std::ptr::null_mut(),
                doc: crate::methods::cstr_static(sd.doc),
                flags: if sd.kw { 1 } else { 0 },
                name_strobj: pystr::intern_str(sd.name).into_raw(),
            });
        }
        Box::leak(v.into_boxed_slice()).as_ptr() as usize
    });
    unsafe { std::slice::from_raw_parts(p as *const wrapperbase, crate::types::slots::len_of_slotdefs()) }
}

pub fn new_wrapper_descr(owner: *mut PyTypeObject, slotdef_index: usize, wrapped: *mut c_void) -> PyRef {
    unsafe {
        let base = &wrapperbases()[slotdef_index] as *const wrapperbase as *mut wrapperbase;
        let d = alloc_descr::<PyWrapperDescrObject>(wrapper_descr_type(), owner, PyRef::from_borrowed((*base).name_strobj));
        (*d).d_base = base;
        (*d).d_wrapped = wrapped;
        PyRef::from_raw(d as *mut PyObject)
    }
}

/// (slot, wrapped fn, owner type) for a wrapper_descriptor.
pub fn wrapper_descr_info(o: &PyObject) -> Option<(crate::types::slots::Slot, *mut c_void, *mut PyTypeObject)> {
    if !o.is_type(wrapper_descr_type()) {
        return None;
    }
    unsafe {
        let d = o.as_ptr() as *mut PyWrapperDescrObject;
        let idx = (*(*d).d_base).offset as usize;
        Some((crate::types::slots::slotdef(idx).slot, (*d).d_wrapped, (*d).d_common.d_type))
    }
}

pub fn is_wrapper_for(o: &PyObject, f: *mut c_void) -> bool {
    match wrapper_descr_info(o) {
        Some((_, w, _)) => w == f,
        None => false,
    }
}

fn descr_owner(o: &PyObject) -> *mut PyTypeObject {
    unsafe { (*(o.as_ptr() as *mut PyDescrObject)).d_type }
}
fn descr_name(o: &PyObject) -> String {
    unsafe {
        let n = (*(o.as_ptr() as *mut PyDescrObject)).d_name;
        if n.is_null() { "?".into() } else { pystr::to_string_lossy(&*n) }
    }
}

fn descr_check(d: &PyObject, obj: &PyObject) -> PyResult<()> {
    let owner = descr_owner(d);
    if !isinstance_exact_or_sub(obj, owner) {
        raise!(
            TypeError,
            "descriptor '{}' for '{}' objects doesn't apply to a '{}' object",
            descr_name(d),
            type_name(unsafe { &*owner }),
            type_name(obj.ty())
        );
    }
    Ok(())
}

// ---- method_descriptor

unsafe extern "C" fn method_descr_get(d: *mut PyObject, obj: *mut PyObject, _tp: *mut PyObject) -> *mut PyObject {
    unsafe {
        let dd = &*d;
        if obj.is_null() {
            return dd.to_ref().into_raw();
        }
        if let Err(_) = descr_check(dd, &*obj) {
            return std::ptr::null_mut();
        }
        let md = d as *mut PyMethodDescrObject;
        let owner = (*md).d_common.d_type;
        let cls = if (*(*md).d_method).ml_flags & METH_METHOD != 0 { Some(owner) } else { None };
        cfunction::new_cfunction(
            (*md).d_method,
            Some(PyRef::from_borrowed(obj)),
            None,
        )
        .pipe_set_class(cls)
        .into_raw()
    }
}

trait PipeClass {
    fn pipe_set_class(self, cls: Option<*mut PyTypeObject>) -> PyRef;
}
impl PipeClass for PyRef {
    fn pipe_set_class(self, cls: Option<*mut PyTypeObject>) -> PyRef {
        if let Some(c) = cls {
            cfunction::set_defining_class(&self, c);
        }
        self
    }
}

fn method_qualname(d: &PyObject) -> String {
    format!("{}.{}", type_name(unsafe { &*descr_owner(d) }), descr_name(d))
}

unsafe extern "C" fn method_descr_vectorcall(
    d: *mut PyObject,
    args: *const *mut PyObject,
    nargsf: usize,
    kwnames: *mut PyObject,
) -> *mut PyObject {
    unsafe {
        let dd = &*d;
        let nargs = nargsf & !PY_VECTORCALL_ARGUMENTS_OFFSET;
        if nargs < 1 {
            return into_c(Err(err!(
                TypeError,
                "unbound method {}() needs an argument",
                method_qualname(dd)
            )));
        }
        let slf = *args;
        if descr_check(dd, &*slf).is_err() {
            return std::ptr::null_mut();
        }
        let md = d as *mut PyMethodDescrObject;
        let nkw = if kwnames.is_null() { 0 } else { tuple::tuple_len(&*kwnames) };
        let all = std::slice::from_raw_parts(args.add(1), nargs - 1 + nkw);
        into_c(crate::methods::call_methoddef(
            (*md).d_method,
            slf,
            (*md).d_common.d_type,
            all,
            kwnames,
            &|| method_qualname(dd),
        ))
    }
}

// ---- classmethod_descriptor

unsafe extern "C" fn classmethod_descr_get(d: *mut PyObject, obj: *mut PyObject, tp: *mut PyObject) -> *mut PyObject {
    unsafe {
        let dd = &*d;
        let owner = descr_owner(dd);
        let t = if !tp.is_null() {
            tp
        } else if !obj.is_null() {
            (*obj).type_ptr() as *mut PyObject
        } else {
            return into_c(Err(err!(
                TypeError,
                "descriptor '{}' for type '{}' needs either an object or a type",
                descr_name(dd),
                type_name(&*owner)
            )));
        };
        if !is_type(&*t) {
            return into_c(Err(err!(
                TypeError,
                "descriptor '{}' for type '{}' needs a type, not a '{}' as arg 2",
                descr_name(dd),
                type_name(&*owner),
                type_name((*t).ty())
            )));
        }
        if !is_subtype(as_type(&*t), owner) {
            return into_c(Err(err!(
                TypeError,
                "descriptor '{}' requires a subtype of '{}' but received '{}'",
                descr_name(dd),
                type_name(&*owner),
                type_name(&*as_type(&*t))
            )));
        }
        let md = d as *mut PyMethodDescrObject;
        let f = cfunction::new_cfunction((*md).d_method, Some(PyRef::from_borrowed(t)), None);
        if (*(*md).d_method).ml_flags & METH_METHOD != 0 {
            cfunction::set_defining_class(&f, owner);
        }
        f.into_raw()
    }
}

unsafe extern "C" fn classmethod_descr_call(d: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let dd = &*d;
        let items = tuple::tuple_items(&*args);
        if items.is_empty() {
            return into_c(Err(err!(
                TypeError,
                "descriptor '{}' of '{}' object needs an argument",
                descr_name(dd),
                type_name(&*descr_owner(dd))
            )));
        }
        let bound = classmethod_descr_get(d, std::ptr::null_mut(), items[0].as_ptr());
        if bound.is_null() {
            return bound;
        }
        let bound = PyRef::from_raw(bound);
        let rest: Vec<&PyObject> = items[1..].to_vec();
        into_c(ab::call_with_kwargs_dict(&bound, &rest, if kw.is_null() { None } else { Some(&*kw) }))
    }
}

// ---- member_descriptor

pub unsafe fn member_get(obj: *mut PyObject, m: *const PyMemberDef) -> PyResult {
    unsafe {
        let mut off = (*m).offset;
        if (*m).flags & Py_RELATIVE_OFFSET != 0 {
            let tp = (*obj).type_ptr();
            off += (*(*tp).tp_base).tp_basicsize;
        }
        let p = (obj as *mut u8).offset(off);
        Ok(match (*m).type_code {
            Py_T_BOOL => crate::types::bool_::new_bool(*(p as *const i8) != 0),
            Py_T_BYTE => int::new_int(*(p as *const i8) as i64),
            Py_T_UBYTE => int::new_int(*(p as *const u8) as i64),
            Py_T_SHORT => int::new_int(*(p as *const i16) as i64),
            Py_T_USHORT => int::new_int(*(p as *const u16) as i64),
            Py_T_INT => int::new_int(*(p as *const i32) as i64),
            Py_T_UINT => int::new_int(*(p as *const u32) as i64),
            Py_T_LONG | Py_T_LONGLONG | Py_T_PYSSIZET => int::new_int(*(p as *const i64)),
            Py_T_ULONG | Py_T_ULONGLONG => int::new_int_u128(*(p as *const u64) as u128),
            Py_T_FLOAT => crate::types::float::new_float(*(p as *const f32) as f64),
            Py_T_DOUBLE => crate::types::float::new_float(*(p as *const f64)),
            Py_T_STRING => {
                let s = *(p as *const *const std::ffi::c_char);
                if s.is_null() { none::none_ref() } else { pystr::new_str(&CStr::from_ptr(s).to_string_lossy()) }
            }
            Py_T_STRING_INPLACE => pystr::new_str(&CStr::from_ptr(p as *const std::ffi::c_char).to_string_lossy()),
            Py_T_CHAR => pystr::from_latin1(&[*p]),
            Py_T_OBJECT => {
                let o = *(p as *const *mut PyObject);
                if o.is_null() { none::none_ref() } else { PyRef::from_borrowed(o) }
            }
            Py_T_OBJECT_EX => {
                let o = *(p as *const *mut PyObject);
                if o.is_null() {
                    let tp = (*obj).type_ptr();
                    raise!(
                        AttributeError,
                        "'{}' object has no attribute '{}'",
                        crate::types::typeobject::type_fqname(tp),
                        CStr::from_ptr((*m).name).to_string_lossy()
                    );
                }
                PyRef::from_borrowed(o)
            }
            Py_T_NONE => none::none_ref(),
            _ => raise!(SystemError, "bad memberdescr type"),
        })
    }
}

pub unsafe fn member_set(obj: *mut PyObject, m: *const PyMemberDef, v: Option<&PyObject>) -> PyResult<()> {
    unsafe {
        let nm = CStr::from_ptr((*m).name).to_string_lossy();
        if (*m).flags & Py_READONLY != 0 {
            raise!(AttributeError, "readonly attribute");
        }
        let mut off = (*m).offset;
        if (*m).flags & Py_RELATIVE_OFFSET != 0 {
            let tp = (*obj).type_ptr();
            off += (*(*tp).tp_base).tp_basicsize;
        }
        let p = (obj as *mut u8).offset(off);
        let v = match v {
            Some(v) => v,
            None => {
                if (*m).type_code == Py_T_OBJECT_EX {
                    let old = *(p as *mut *mut PyObject);
                    if old.is_null() {
                        raise!(AttributeError, "'{}' object has no attribute '{}'", type_name((*obj).ty()), nm);
                    }
                    *(p as *mut *mut PyObject) = std::ptr::null_mut();
                    decref(old);
                    return Ok(());
                }
                if (*m).type_code == Py_T_OBJECT {
                    let old = *(p as *mut *mut PyObject);
                    *(p as *mut *mut PyObject) = std::ptr::null_mut();
                    xdecref(old);
                    return Ok(());
                }
                raise!(TypeError, "can't delete numeric/char attribute");
            }
        };
        // C-API member semantics: values must fit the "hard" range (C long / unsigned
        // long for small types) else OverflowError; values outside the field's own range
        // are truncated with a RuntimeWarning.
        macro_rules! int_member {
            ($t:ty) => {{
                int_member!($t, i64::MIN as i128, i64::MAX as i128, true)
            }};
            ($t:ty, $hmin:expr, $hmax:expr, $index:expr) => {{
                if !$index && !crate::types::int::is_int(v) {
                    raise!(TypeError, "attribute value type must be int");
                }
                let x = match ab::as_i128_index(v) {
                    Ok(x) => x,
                    Err(e) => {
                        if exc_matches(crate::types::exceptions::OverflowError()) {
                            clear_exc();
                            raise!(OverflowError, "Python int too large to convert to C {}", stringify!($t));
                        }
                        return Err(e);
                    }
                };
                if x < $hmin || x > $hmax {
                    raise!(OverflowError, "Python int too large to convert to C {}", stringify!($t));
                }
                if x < <$t>::MIN as i128 || x > <$t>::MAX as i128 {
                    crate::modules::warnings::warn(
                        crate::types::exceptions::RuntimeWarning(),
                        &format!("Truncation of value to {}", stringify!($t)),
                        1,
                    )?;
                }
                *(p as *mut $t) = x as $t;
            }};
        }
        match (*m).type_code {
            Py_T_BOOL => {
                if !crate::types::bool_::is_bool(v) {
                    raise!(TypeError, "attribute value type must be bool");
                }
                *(p as *mut i8) = v.is(crate::types::bool_::true_obj()) as i8;
            }
            Py_T_BYTE => int_member!(i8),
            Py_T_UBYTE => int_member!(u8),
            Py_T_SHORT => int_member!(i16),
            Py_T_USHORT => int_member!(u16),
            Py_T_INT => int_member!(i32),
            Py_T_UINT => int_member!(u32, i64::MIN as i128, u64::MAX as i128, true),
            Py_T_LONG | Py_T_LONGLONG => int_member!(i64, i64::MIN as i128, i64::MAX as i128, true),
            Py_T_PYSSIZET => int_member!(i64, i64::MIN as i128, i64::MAX as i128, false),
            Py_T_ULONG | Py_T_ULONGLONG => int_member!(u64, i64::MIN as i128, u64::MAX as i128, true),
            Py_T_FLOAT => *(p as *mut f32) = ab::float_value(v)? as f32,
            Py_T_DOUBLE => *(p as *mut f64) = ab::float_value(v)?,
            Py_T_OBJECT | Py_T_OBJECT_EX => {
                let old = *(p as *mut *mut PyObject);
                *(p as *mut *mut PyObject) = v.to_ref().into_raw();
                xdecref(old);
            }
            Py_T_CHAR => {
                if !pystr::is_str(v) || pystr::str_len(v) != 1 {
                    raise!(TypeError, "expected a character");
                }
                *p = pystr::str_data(v).get(0) as u8;
            }
            _ => raise!(TypeError, "readonly attribute"),
        }
        Ok(())
    }
}

unsafe extern "C" fn member_descr_get(d: *mut PyObject, obj: *mut PyObject, _tp: *mut PyObject) -> *mut PyObject {
    unsafe {
        if obj.is_null() {
            return (*d).to_ref().into_raw();
        }
        if descr_check(&*d, &*obj).is_err() {
            return std::ptr::null_mut();
        }
        let m = (*(d as *mut PyMemberDescrObject)).d_member;
        into_c(member_get(obj, m))
    }
}

unsafe extern "C" fn member_descr_set(d: *mut PyObject, obj: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        if descr_check(&*d, &*obj).is_err() {
            return -1;
        }
        let m = (*(d as *mut PyMemberDescrObject)).d_member;
        int_into_c(member_set(obj, m, if v.is_null() { None } else { Some(&*v) }))
    }
}

// ---- getset_descriptor

unsafe extern "C" fn getset_descr_get(d: *mut PyObject, obj: *mut PyObject, _tp: *mut PyObject) -> *mut PyObject {
    unsafe {
        if obj.is_null() {
            return (*d).to_ref().into_raw();
        }
        if descr_check(&*d, &*obj).is_err() {
            return std::ptr::null_mut();
        }
        let g = (*(d as *mut PyGetSetDescrObject)).d_getset;
        match (*g).get {
            Some(f) => f(obj, (*g).closure),
            None => into_c(Err(err!(
                AttributeError,
                "attribute '{}' of '{}' objects is not readable",
                descr_name(&*d),
                type_name(&*descr_owner(&*d))
            ))),
        }
    }
}

unsafe extern "C" fn getset_descr_set(d: *mut PyObject, obj: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        if descr_check(&*d, &*obj).is_err() {
            return -1;
        }
        let g = (*(d as *mut PyGetSetDescrObject)).d_getset;
        match (*g).set {
            Some(f) => f(obj, v, (*g).closure),
            None => {
                let _ = err!(
                    AttributeError,
                    "attribute '{}' of '{}' objects is not writable",
                    descr_name(&*d),
                    type_name(&*descr_owner(&*d))
                );
                -1
            }
        }
    }
}

// ---- wrapper_descriptor / method-wrapper

#[repr(C)]
pub struct MethodWrapper {
    pub ob_base: PyObject,
    pub descr: *mut PyObject,
    pub slf: *mut PyObject,
}

unsafe extern "C" fn wrapper_descr_get(d: *mut PyObject, obj: *mut PyObject, _tp: *mut PyObject) -> *mut PyObject {
    unsafe {
        if obj.is_null() {
            return (*d).to_ref().into_raw();
        }
        if descr_check(&*d, &*obj).is_err() {
            return std::ptr::null_mut();
        }
        let op = crate::alloc::alloc_object(method_wrapper_type(), std::mem::size_of::<MethodWrapper>());
        let mw = op as *mut MethodWrapper;
        incref(d);
        incref(obj);
        (*mw).descr = d;
        (*mw).slf = obj;
        crate::gc::track(op);
        op
    }
}

unsafe fn wrapper_call_impl(d: *mut PyObject, slf: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    unsafe {
        let wd = d as *mut PyWrapperDescrObject;
        let idx = (*(*wd).d_base).offset as usize;
        crate::types::slots::call_wrapper(idx, slf, args, kw, (*wd).d_wrapped)
    }
}

unsafe extern "C" fn wrapper_descr_call(d: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let dd = &*d;
        let items = tuple::tuple_items(&*args);
        if items.is_empty() {
            return into_c(Err(err!(
                TypeError,
                "descriptor '{}' of '{}' object needs an argument",
                descr_name(dd),
                type_name(&*descr_owner(dd))
            )));
        }
        if descr_check(dd, items[0]).is_err() {
            return std::ptr::null_mut();
        }
        into_c(wrapper_call_impl(d, items[0], &items[1..], if kw.is_null() { None } else { Some(&*kw) }))
    }
}

unsafe extern "C" fn method_wrapper_call(w: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let mw = w as *mut MethodWrapper;
        let items = tuple::tuple_items(&*args);
        into_c(wrapper_call_impl((*mw).descr, &*(*mw).slf, items, if kw.is_null() { None } else { Some(&*kw) }))
    }
}

unsafe extern "C" fn method_wrapper_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let mw = o as *mut MethodWrapper;
        xdecref((*mw).descr);
        xdecref((*mw).slf);
        crate::alloc::free_object(o);
    }
}

unsafe extern "C" fn method_wrapper_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let mw = o as *mut MethodWrapper;
        let f = visit.unwrap();
        if !(*mw).descr.is_null() {
            let r = f((*mw).descr, arg);
            if r != 0 {
                return r;
            }
        }
        if !(*mw).slf.is_null() {
            return f((*mw).slf, arg);
        }
        0
    }
}

unsafe extern "C" fn method_wrapper_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let mw = o as *mut MethodWrapper;
        let s = &*(*mw).slf;
        into_c(Ok(pystr::new_str(&format!(
            "<method-wrapper '{}' of {} object at {:p}>",
            descr_name(&*(*mw).descr),
            type_name(s.ty()),
            s.as_ptr()
        ))))
    }
}

unsafe extern "C" fn method_wrapper_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        if !(*b).is_type(method_wrapper_type()) || (op != Py_EQ && op != Py_NE) {
            return none::not_implemented().to_ref().into_raw();
        }
        let x = a as *mut MethodWrapper;
        let y = b as *mut MethodWrapper;
        let eq = (*x).descr == (*y).descr && (*x).slf == (*y).slf;
        crate::types::bool_::new_bool(if op == Py_EQ { eq } else { !eq }).into_raw()
    }
}

unsafe extern "C" fn method_wrapper_hash(o: *mut PyObject) -> Py_hash_t {
    unsafe {
        let mw = o as *mut MethodWrapper;
        let h = crate::hash::hash_pointer((*mw).slf as usize) ^ crate::hash::hash_pointer((*mw).descr as usize);
        if h == -1 { -2 } else { h }
    }
}

// ---- common descriptor attributes

unsafe extern "C" fn descr_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let d = o as *mut PyDescrObject;
        xdecref((*d).d_type as *mut PyObject);
        xdecref((*d).d_name);
        xdecref((*d).d_qualname);
        crate::alloc::free_object(o);
    }
}

unsafe extern "C" fn descr_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let d = o as *mut PyDescrObject;
        if !(*d).d_type.is_null() {
            return visit.unwrap()((*d).d_type as *mut PyObject, arg);
        }
        0
    }
}

unsafe extern "C" fn descr_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let oo = &*o;
        let kind = if oo.is_type(method_descr_type()) {
            "method"
        } else if oo.is_type(classmethod_descr_type()) {
            "method"
        } else if oo.is_type(member_descr_type()) {
            "member"
        } else if oo.is_type(getset_descr_type()) {
            "attribute"
        } else {
            "slot wrapper"
        };
        into_c(Ok(pystr::new_str(&format!(
            "<{} '{}' of '{}' objects>",
            kind,
            descr_name(oo),
            type_name(&*descr_owner(oo))
        ))))
    }
}

fn descr_doc_raw(o: &PyObject) -> Option<String> {
    unsafe {
        let p = if o.is_type(method_descr_type()) || o.is_type(classmethod_descr_type()) {
            (*(*(o.as_ptr() as *mut PyMethodDescrObject)).d_method).ml_doc
        } else if o.is_type(member_descr_type()) {
            (*(*(o.as_ptr() as *mut PyMemberDescrObject)).d_member).doc
        } else if o.is_type(getset_descr_type()) {
            (*(*(o.as_ptr() as *mut PyGetSetDescrObject)).d_getset).doc
        } else if o.is_type(wrapper_descr_type()) {
            (*(*(o.as_ptr() as *mut PyWrapperDescrObject)).d_base).doc
        } else {
            std::ptr::null()
        };
        if p.is_null() { None } else { Some(CStr::from_ptr(p).to_string_lossy().into_owned()) }
    }
}

fn descr_get_doc(o: &PyObject) -> PyResult {
    match descr_doc_raw(o) {
        None => Ok(none::none_ref()),
        Some(d) => match doc_without_signature(&d) {
            Some(s) => Ok(pystr::new_str(&s)),
            None => Ok(none::none_ref()),
        },
    }
}

fn descr_get_text_signature(o: &PyObject) -> PyResult {
    match descr_doc_raw(o) {
        Some(d) => match text_signature(&descr_name(o), &d) {
            Some(s) => Ok(pystr::new_str(&s)),
            None => Ok(none::none_ref()),
        },
        None => Ok(none::none_ref()),
    }
}

fn descr_get_qualname(o: &PyObject) -> PyResult {
    Ok(pystr::new_str(&format!("{}.{}", type_qualname(unsafe { &*descr_owner(o) }), descr_name(o))))
}

fn descr_get_objclass(o: &PyObject) -> PyResult {
    Ok(type_ref(descr_owner(o)))
}

fn descr_get_name(o: &PyObject) -> PyResult {
    unsafe { Ok(PyRef::from_borrowed((*(o.as_ptr() as *mut PyDescrObject)).d_name)) }
}

fn method_descr_reduce(o: &PyObject) -> PyResult {
    let getattr = crate::builtins::builtin("getattr")?;
    Ok(tuple::new_tuple(vec![getattr, tuple::new_tuple(vec![type_ref(descr_owner(o)), descr_get_name(o)?])]))
}

fn mw_get_self(o: &PyObject) -> PyResult {
    unsafe { Ok(PyRef::from_borrowed((*(o.as_ptr() as *mut MethodWrapper)).slf)) }
}
fn mw_get_name(o: &PyObject) -> PyResult {
    unsafe { descr_get_name(&*(*(o.as_ptr() as *mut MethodWrapper)).descr) }
}
fn mw_get_qualname(o: &PyObject) -> PyResult {
    unsafe { descr_get_qualname(&*(*(o.as_ptr() as *mut MethodWrapper)).descr) }
}
fn mw_get_doc(o: &PyObject) -> PyResult {
    unsafe { descr_get_doc(&*(*(o.as_ptr() as *mut MethodWrapper)).descr) }
}
fn mw_get_textsig(o: &PyObject) -> PyResult {
    unsafe { descr_get_text_signature(&*(*(o.as_ptr() as *mut MethodWrapper)).descr) }
}
fn mw_get_objclass(o: &PyObject) -> PyResult {
    unsafe { descr_get_objclass(&*(*(o.as_ptr() as *mut MethodWrapper)).descr) }
}
fn mw_reduce(o: &PyObject) -> PyResult {
    let getattr = crate::builtins::builtin("getattr")?;
    Ok(tuple::new_tuple(vec![getattr, tuple::new_tuple(vec![mw_get_self(o)?, mw_get_name(o)?])]))
}

// ---- __new__ of builtin types

static mut TP_NEW_DEF: PyMethodDef = PyMethodDef {
    ml_name: c"__new__".as_ptr(),
    ml_meth: std::ptr::null(),
    ml_flags: METH_VARARGS | METH_KEYWORDS,
    ml_doc: c"Create and return a new object.  See help(type) for accurate signature.".as_ptr(),
};

unsafe extern "C" fn tp_new_wrapper(slf: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let tp = slf as *mut PyTypeObject;
        let items = tuple::tuple_items(&*args);
        if items.is_empty() {
            return into_c(Err(err!(TypeError, "{}.__new__(): not enough arguments", type_name(&*tp))));
        }
        let arg0 = items[0];
        if !is_type(arg0) {
            return into_c(Err(err!(
                TypeError,
                "{}.__new__(X): X is not a type object ({})",
                type_name(&*tp),
                type_name(arg0.ty())
            )));
        }
        let sub = as_type(arg0);
        if !is_subtype(sub, tp) {
            return into_c(Err(err!(
                TypeError,
                "{}.__new__({}): {} is not a subtype of {}",
                type_name(&*tp),
                type_name(&*sub),
                type_name(&*sub),
                type_name(&*tp)
            )));
        }
        if (*sub).tp_flags & Py_TPFLAGS_DISALLOW_INSTANTIATION != 0 || (*sub).tp_new.is_none() {
            return into_c(Err(err!(TypeError, "cannot create '{}' instances", crate::types::typeobject::type_fqname(sub))));
        }
        // Check that the most derived non-heap base of `sub` uses this tp_new.
        let mut staticbase = sub;
        while !staticbase.is_null() && (*staticbase).tp_new == Some(std::mem::transmute(crate::types::slots::Slot::TpNew.generic())) {
            staticbase = (*staticbase).tp_base;
        }
        if !staticbase.is_null() && (*staticbase).tp_new != (*tp).tp_new {
            return into_c(Err(err!(
                TypeError,
                "{}.__new__({}) is not safe, use {}.__new__()",
                type_name(&*tp),
                type_name(&*sub),
                type_name(&*staticbase)
            )));
        }
        let rest = tuple::new_tuple(items[1..].iter().map(|x| x.to_ref()).collect());
        match (*tp).tp_new {
            Some(f) => f(sub, rest.as_ptr(), kw),
            None => into_c(Err(err!(TypeError, "cannot create '{}' instances", type_name(&*sub)))),
        }
    }
}

pub fn new_tp_new_wrapper(tp: *mut PyTypeObject) -> PyRef {
    unsafe {
        let def = &raw mut TP_NEW_DEF;
        if (*def).ml_meth.is_null() {
            (*def).ml_meth = tp_new_wrapper as *const c_void;
        }
        cfunction::new_cfunction(def, Some(type_ref(tp)), None)
    }
}

/// If `o` is the `__new__` of a builtin type, return that type.
pub fn tp_new_wrapper_type(o: &PyObject) -> Option<*mut PyTypeObject> {
    if !o.is_type(cfunction::cfunction_type()) {
        return None;
    }
    unsafe {
        let f = o.as_ptr() as *mut PyCFunctionObject;
        if (*f).m_ml == &raw mut TP_NEW_DEF {
            return Some((*f).m_self as *mut PyTypeObject);
        }
    }
    None
}

// ---- type initialisation

pub fn init_types() {
    unsafe {
        let getsets_common = crate::getset_table![
            ("__doc__", descr_get_doc),
            ("__qualname__", descr_get_qualname),
            ("__text_signature__", descr_get_text_signature),
        ];
        let members_common = crate::member_table![
            ("__objclass__", Py_T_OBJECT, std::mem::offset_of!(PyDescrObject, d_type), Py_READONLY),
            ("__name__", Py_T_OBJECT, std::mem::offset_of!(PyDescrObject, d_name), Py_READONLY),
        ];
        let reduce_methods = crate::mdefs![("__reduce__", NOARGS, method_descr_reduce)];
        let base_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;

        let setup = |tp: *mut PyTypeObject, name: &'static CStr, size: usize| {
            let t = &mut *tp;
            t.tp_name = name.as_ptr();
            t.tp_basicsize = size as isize;
            t.tp_flags = base_flags;
            t.tp_dealloc = Some(descr_dealloc);
            t.tp_traverse = Some(descr_traverse);
            t.tp_repr = Some(descr_repr);
            t.tp_getattro = Some(object_generic_getattro);
            t.tp_members = members_common;
            t.tp_getset = getsets_common;
            t.tp_methods = reduce_methods;
        };
        setup(method_descr_type(), c"method_descriptor", std::mem::size_of::<PyMethodDescrObject>());
        let t = &mut *method_descr_type();
        t.tp_descr_get = Some(method_descr_get);
        t.tp_call = Some(crate::abstract_::vectorcall_call);
        t.tp_vectorcall_offset = std::mem::offset_of!(PyMethodDescrObject, vectorcall) as isize;
        t.tp_flags |= Py_TPFLAGS_HAVE_VECTORCALL | Py_TPFLAGS_METHOD_DESCRIPTOR;

        setup(classmethod_descr_type(), c"classmethod_descriptor", std::mem::size_of::<PyMethodDescrObject>());
        // Unlike the other descriptors, classmethod descriptors cannot be pickled.
        (*classmethod_descr_type()).tp_methods = std::ptr::null_mut();
        let t = &mut *classmethod_descr_type();
        t.tp_descr_get = Some(classmethod_descr_get);
        t.tp_call = Some(classmethod_descr_call);

        setup(member_descr_type(), c"member_descriptor", std::mem::size_of::<PyMemberDescrObject>());
        let t = &mut *member_descr_type();
        t.tp_descr_get = Some(member_descr_get);
        t.tp_descr_set = Some(member_descr_set);
        t.tp_getset = crate::getset_table![("__doc__", descr_get_doc), ("__qualname__", descr_get_qualname)];

        setup(getset_descr_type(), c"getset_descriptor", std::mem::size_of::<PyGetSetDescrObject>());
        let t = &mut *getset_descr_type();
        t.tp_descr_get = Some(getset_descr_get);
        t.tp_descr_set = Some(getset_descr_set);
        t.tp_getset = crate::getset_table![("__doc__", descr_get_doc), ("__qualname__", descr_get_qualname)];

        setup(wrapper_descr_type(), c"wrapper_descriptor", std::mem::size_of::<PyWrapperDescrObject>());
        let t = &mut *wrapper_descr_type();
        t.tp_descr_get = Some(wrapper_descr_get);
        t.tp_call = Some(wrapper_descr_call);
        t.tp_flags |= Py_TPFLAGS_METHOD_DESCRIPTOR;

        let t = &mut *method_wrapper_type();
        t.tp_name = c"method-wrapper".as_ptr();
        t.tp_basicsize = std::mem::size_of::<MethodWrapper>() as isize;
        t.tp_flags = base_flags;
        t.tp_dealloc = Some(method_wrapper_dealloc);
        t.tp_traverse = Some(method_wrapper_traverse);
        t.tp_repr = Some(method_wrapper_repr);
        t.tp_call = Some(method_wrapper_call);
        t.tp_richcompare = Some(method_wrapper_richcompare);
        t.tp_hash = Some(method_wrapper_hash);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_getset = crate::getset_table![
            ("__self__", mw_get_self),
            ("__name__", mw_get_name),
            ("__qualname__", mw_get_qualname),
            ("__doc__", mw_get_doc),
            ("__text_signature__", mw_get_textsig),
            ("__objclass__", mw_get_objclass),
        ];
        t.tp_methods = crate::mdefs![("__reduce__", NOARGS, mw_reduce)];
    }
    let _ = name!("__doc__");
}
