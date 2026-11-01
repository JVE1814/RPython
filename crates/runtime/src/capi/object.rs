//! object.h / abstract.h (generic object protocol): attributes, repr/str, hashing,
//! comparisons, truth, managed dicts, type data.

use super::*;
use crate::abstract_ as ab;
use crate::types::typeobject::{self as to, is_subtype, type_ref};
use crate::types::{bool_, bytes, dict, none, str as pystr, tuple};
use std::ffi::c_void;

unsafe fn name_obj(s: *const c_char) -> PyResult {
    unsafe {
        let b = CStr::from_ptr(s).to_bytes();
        let o = super::unicode::decode_utf8(b, None)?;
        Ok(pystr::intern(o))
    }
}

// ---------------------------------------------------------------- attributes

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GetAttr(o: *mut PyObject, n: *mut PyObject) -> *mut PyObject {
    unsafe {
        if o.is_null() || n.is_null() {
            return null_arg();
        }
        if !pystr::is_str(&*n) {
            let _ = type_error(&format!("attribute name must be string, not '{}'", tname(&*n)));
            return std::ptr::null_mut();
        }
        ret(ab::getattr(&*o, &*n))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GetAttrString(o: *mut PyObject, n: *const c_char) -> *mut PyObject {
    unsafe {
        if o.is_null() || n.is_null() {
            return null_arg();
        }
        match name_obj(n) {
            Ok(nm) => ret(ab::getattr(&*o, &nm)),
            Err(_) => std::ptr::null_mut(),
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GetOptionalAttr(o: *mut PyObject, n: *mut PyObject, result: *mut *mut PyObject) -> c_int {
    unsafe {
        *result = std::ptr::null_mut();
        if !pystr::is_str(&*n) {
            let _ = type_error(&format!("attribute name must be string, not '{}'", tname(&*n)));
            return -1;
        }
        match ab::getattr_opt(&*o, &*n) {
            Ok(Some(v)) => {
                *result = v.into_raw();
                1
            }
            Ok(None) => 0,
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GetOptionalAttrString(o: *mut PyObject, n: *const c_char, result: *mut *mut PyObject) -> c_int {
    unsafe {
        *result = std::ptr::null_mut();
        match name_obj(n) {
            Ok(nm) => PyObject_GetOptionalAttr(o, nm.as_ptr(), result),
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_HasAttrWithError(o: *mut PyObject, n: *mut PyObject) -> c_int {
    unsafe {
        let mut r = std::ptr::null_mut();
        let x = PyObject_GetOptionalAttr(o, n, &mut r);
        xdecref(r);
        x
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_HasAttrStringWithError(o: *mut PyObject, n: *const c_char) -> c_int {
    unsafe {
        let mut r = std::ptr::null_mut();
        let x = PyObject_GetOptionalAttrString(o, n, &mut r);
        xdecref(r);
        x
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_HasAttr(o: *mut PyObject, n: *mut PyObject) -> c_int {
    unsafe {
        let r = PyObject_HasAttrWithError(o, n);
        if r < 0 {
            write_unraisable_attr("PyObject_HasAttr");
            return 0;
        }
        r
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_HasAttrString(o: *mut PyObject, n: *const c_char) -> c_int {
    unsafe {
        let r = PyObject_HasAttrStringWithError(o, n);
        if r < 0 {
            write_unraisable_attr("PyObject_HasAttrString");
            return 0;
        }
        r
    }
}
fn write_unraisable_attr(what: &str) {
    let msg = pystr::new_str(&format!("Exception ignored in {}(); consider using {}WithError()", what, what));
    super::errors::write_unraisable_msg(Some(msg), std::ptr::null_mut());
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_SetAttr(o: *mut PyObject, n: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe { if o.is_null() || n.is_null() { null_arg(); return -1; } ret_i(ab::setattr_opt(&*o, &*n, opt(v))) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_SetAttrString(o: *mut PyObject, n: *const c_char, v: *mut PyObject) -> c_int {
    unsafe {
        if o.is_null() || n.is_null() {
            null_arg();
            return -1;
        }
        match name_obj(n) {
            Ok(nm) => ret_i(ab::setattr_opt(&*o, &nm, opt(v))),
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_DelAttr(o: *mut PyObject, n: *mut PyObject) -> c_int {
    unsafe { ret_i(ab::setattr_opt(&*o, &*n, None)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_DelAttrString(o: *mut PyObject, n: *const c_char) -> c_int {
    unsafe { PyObject_SetAttrString(o, n, std::ptr::null_mut()) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GenericGetAttr(o: *mut PyObject, n: *mut PyObject) -> *mut PyObject {
    unsafe {
        if !pystr::is_str(&*n) {
            let _ = type_error(&format!("attribute name must be string, not '{}'", tname(&*n)));
            return std::ptr::null_mut();
        }
        ret(to::generic_getattr(&*o, &*n))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GenericSetAttr(o: *mut PyObject, n: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        if !pystr::is_str(&*n) {
            let _ = type_error(&format!("attribute name must be string, not '{}'", tname(&*n)));
            return -1;
        }
        ret_i(to::generic_setattr(&*o, &*n, opt(v)))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_GenericGetAttrWithDict(o: *mut PyObject, n: *mut PyObject, d: *mut PyObject, suppress: c_int) -> *mut PyObject {
    unsafe {
        match to::generic_getattr_opt(&*o, &*n, opt(d)) {
            Ok(Some(v)) => v.into_raw(),
            Ok(None) => {
                if suppress == 0 {
                    let _ = to::attribute_error_for(&*o, &*n);
                }
                std::ptr::null_mut()
            }
            Err(_) => {
                if suppress != 0 && exc_matches(crate::types::exceptions::AttributeError()) {
                    clear_exc();
                }
                std::ptr::null_mut()
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_GenericSetAttrWithDict(o: *mut PyObject, n: *mut PyObject, v: *mut PyObject, d: *mut PyObject) -> c_int {
    unsafe { ret_i(to::generic_setattr_with_dict(&*o, &*n, opt(v), opt(d))) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GenericGetDict(o: *mut PyObject, _ctx: *mut c_void) -> *mut PyObject {
    unsafe {
        match to::instance_dict(&*o) {
            Some(d) => d.into_raw(),
            None => {
                let _ = crate::err!(AttributeError, "This object has no __dict__");
                std::ptr::null_mut()
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GenericSetDict(o: *mut PyObject, v: *mut PyObject, _ctx: *mut c_void) -> c_int {
    unsafe {
        let slot = to::dict_slot(o);
        if slot.is_null() {
            let _ = crate::err!(AttributeError, "This object has no __dict__");
            return -1;
        }
        if v.is_null() {
            let _ = type_error("cannot delete __dict__");
            return -1;
        }
        if !dict::is_dict(&*v) {
            let _ = type_error(&format!("__dict__ must be set to a dictionary, not a '{}'", tname(&*v)));
            return -1;
        }
        let old = *slot;
        *slot = (*v).to_ref().into_raw();
        xdecref(old);
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_GetDictPtr(o: *mut PyObject) -> *mut *mut PyObject {
    unsafe { to::dict_slot(o) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GetItemData(o: *mut PyObject) -> *mut c_void {
    unsafe {
        let tp = (*o).type_ptr();
        if (*tp).tp_flags & Py_TPFLAGS_ITEMS_AT_END == 0 {
            let _ = type_error(&format!("type '{}' does not have Py_TPFLAGS_ITEMS_AT_END", to::type_name(&*tp)));
            return std::ptr::null_mut();
        }
        (o as *mut u8).offset((*tp).tp_basicsize) as *mut c_void
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GetItemData_DuringGC(o: *mut PyObject) -> *mut c_void {
    unsafe {
        let tp = (*o).type_ptr();
        (o as *mut u8).offset((*tp).tp_basicsize) as *mut c_void
    }
}

// ---------------------------------------------------------------- managed dict / weakrefs

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_VisitManagedDict(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        if (*(*o).type_ptr()).tp_flags & Py_TPFLAGS_MANAGED_DICT == 0 || !crate::alloc::has_managed(o) {
            return 0;
        }
        let r = crate::shape::traverse(o, visit, arg);
        if r != 0 {
            return r;
        }
        let d = *crate::alloc::managed_dict_slot(o);
        if !d.is_null() && d as usize & 1 == 0 {
            if let Some(v) = visit {
                return v(d, arg);
            }
        }
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_ClearManagedDict(o: *mut PyObject) {
    unsafe {
        if (*(*o).type_ptr()).tp_flags & Py_TPFLAGS_MANAGED_DICT == 0 || !crate::alloc::has_managed(o) {
            return;
        }
        crate::shape::clear_values(o, true);
        let slot = crate::alloc::managed_dict_slot(o);
        let d = *slot;
        if d as usize & 1 != 0 {
            return; // inline values (now empty) stay
        }
        *slot = std::ptr::null_mut();
        xdecref(d);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GET_WEAKREFS_LISTPTR(o: *mut PyObject) -> *mut *mut PyObject {
    unsafe {
        let tp = (*o).type_ptr();
        if (*tp).tp_flags & Py_TPFLAGS_MANAGED_WEAKREF != 0 && crate::alloc::has_managed(o) {
            return crate::alloc::managed_weakref_slot(o);
        }
        if (*tp).tp_weaklistoffset > 0 {
            return (o as *mut u8).offset((*tp).tp_weaklistoffset) as *mut *mut PyObject;
        }
        std::ptr::null_mut()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_SUPPORTS_WEAKREFS(t: *mut PyTypeObject) -> c_int {
    unsafe { ((*t).tp_weaklistoffset != 0 || (*t).tp_flags & Py_TPFLAGS_MANAGED_WEAKREF != 0) as c_int }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_ClearWeakRefs(o: *mut PyObject) {
    unsafe { crate::types::weakref::clear_weakrefs(o) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_Object_ClearWeakRefsNoCallbacks(o: *mut PyObject) {
    unsafe { crate::types::weakref::clear_weakrefs(o) }
}

// ---------------------------------------------------------------- repr / str / hash / compare

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        if o.is_null() {
            return pystr::new_str("<NULL>").into_raw();
        }
        ret(ab::repr(&*o))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Str(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        if o.is_null() {
            return pystr::new_str("<NULL>").into_raw();
        }
        ret(ab::str(&*o))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_ASCII(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        if o.is_null() {
            return pystr::new_str("<NULL>").into_raw();
        }
        ret(ab::ascii(&*o))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Bytes(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        if o.is_null() {
            return bytes::new_bytes(b"<NULL>").into_raw();
        }
        if bytes::is_bytes(&*o) && (*o).is_type(bytes::bytes_type()) {
            return (*o).to_ref().into_raw();
        }
        ret((|| -> PyResult {
            if let Some(r) = ab::call_special_opt(&*o, crate::name!("__bytes__"), &[])? {
                if !bytes::is_bytes(&r) {
                    return Err(type_error(&format!("__bytes__ returned non-bytes (type {})", tname(&r))));
                }
                return Ok(r);
            }
            ab::call(to::type_obj(bytes::bytes_type()), &[&*o])
        })())
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Hash(o: *mut PyObject) -> Py_hash_t {
    unsafe { ab::hash(&*o).unwrap_or(-1) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_HashNotImplemented(o: *mut PyObject) -> Py_hash_t {
    unsafe {
        let _ = type_error(&format!("unhashable type: '{}'", tname(&*o)));
        -1
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GenericHash(o: *mut PyObject) -> Py_hash_t {
    unsafe { super::long::Py_HashPointer(o as *const c_void) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_RichCompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        if a.is_null() || b.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        ret(ab::rich_compare(&*a, &*b, op))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_RichCompareBool(a: *mut PyObject, b: *mut PyObject, op: c_int) -> c_int {
    unsafe {
        if a == b {
            if op == Py_EQ {
                return 1;
            }
            if op == Py_NE {
                return 0;
            }
        }
        ret_b(ab::rich_compare_bool(&*a, &*b, op))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_IsTrue(o: *mut PyObject) -> c_int {
    unsafe { ret_b(ab::is_true(&*o)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Not(o: *mut PyObject) -> c_int {
    unsafe {
        match ab::is_true(&*o) {
            Ok(b) => (!b) as c_int,
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Type(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        if o.is_null() {
            let _ = bad_internal();
            return std::ptr::null_mut();
        }
        type_ref((*o).type_ptr()).into_raw()
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Format(o: *mut PyObject, spec: *mut PyObject) -> *mut PyObject {
    unsafe {
        if o.is_null() {
            return null_arg();
        }
        let s = if spec.is_null() { pystr::new_str("") } else { (*spec).to_ref() };
        ret(ab::format(&*o, &s))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Dir(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        if o.is_null() {
            return ret((|| -> PyResult {
                let b = crate::builtins::builtins_module();
                let d = ab::getattr_str(&b, "dir")?;
                ab::call(&d, &[])
            })());
        }
        ret(ab::dir(&*o))
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_IsInstance(inst: *mut PyObject, cls: *mut PyObject) -> c_int {
    unsafe { if inst.is_null() || cls.is_null() { null_arg(); return -1; } ret_b(ab::isinstance(&*inst, &*cls)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_IsSubclass(d: *mut PyObject, cls: *mut PyObject) -> c_int {
    unsafe { if d.is_null() || cls.is_null() { null_arg(); return -1; } ret_b(ab::issubclass(&*d, &*cls)) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCallable_Check(o: *mut PyObject) -> c_int {
    unsafe {
        if o.is_null() {
            return 0;
        }
        ab::is_callable(&*o) as c_int
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_SelfIter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Print(o: *mut PyObject, fp: *mut libc::FILE, flags: c_int) -> c_int {
    unsafe {
        let s = if o.is_null() {
            Ok(pystr::new_str("<nil>"))
        } else if flags & 1 != 0 {
            ab::str(&*o)
        } else {
            ab::repr(&*o)
        };
        match s {
            Ok(s) => {
                let u = pystr::to_string_lossy(&s);
                libc::fwrite(u.as_ptr() as *const c_void, 1, u.len(), fp);
                0
            }
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Dump(o: *mut PyObject) {
    unsafe {
        if o.is_null() {
            eprintln!("<object at NULL>");
            return;
        }
        let saved = take_exc();
        let r = ab::repr_string(&*o).unwrap_or_else(|_| "<repr failed>".into());
        clear_exc();
        if let Some(s) = saved {
            set_exc(s);
        }
        eprintln!("object address  : {:p}\nobject refcount : {}\nobject type     : {:p}\nobject type name: {}\nobject repr     : {}",
            o, (*o).refcnt(), (*o).type_ptr(), tname(&*o), r);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_FunctionStr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        ret((|| -> PyResult {
            let q = ab::getattr_opt(&*o, crate::name!("__qualname__"))?;
            let Some(q) = q else { return ab::str(&*o) };
            let m = ab::getattr_opt(&*o, crate::name!("__module__"))?;
            match m {
                Some(m) if pystr::is_str(&m) && pystr::to_string_lossy(&m) != "builtins" => {
                    Ok(pystr::new_str(&format!("{}.{}()", pystr::to_string_lossy(&m), pystr::to_string_lossy(&*(ab::str(&q)?)))))
                }
                _ => Ok(pystr::new_str(&format!("{}()", pystr::to_string_lossy(&*(ab::str(&q)?))))),
            }
        })())
    }
}

// ---------------------------------------------------------------- finalizers / misc

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_CallFinalizer(o: *mut PyObject) {
    unsafe {
        let tp = (*o).type_ptr();
        let Some(f) = (*tp).tp_finalize else { return };
        if crate::alloc::type_is_gc(tp) && crate::gc::is_finalized(o) {
            return;
        }
        f(o);
        if crate::alloc::type_is_gc(tp) {
            crate::gc::set_finalized(o);
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_CallFinalizerFromDealloc(o: *mut PyObject) -> c_int {
    unsafe {
        if crate::gc::call_finalizer_from_dealloc(o) {
            return -1;
        }
        0
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_AsFileDescriptor(o: *mut PyObject) -> c_int {
    unsafe {
        let r = (|| -> PyResult<i64> {
            let v = if crate::types::int::is_int(&*o) {
                (*o).to_ref()
            } else {
                match ab::getattr_opt(&*o, crate::name!("fileno"))? {
                    Some(f) => {
                        let r = ab::call(&f, &[])?;
                        if !crate::types::int::is_int(&r) {
                            return Err(type_error("fileno() returned a non-integer"));
                        }
                        r
                    }
                    None => return Err(type_error("argument must be an int, or have a fileno() method.")),
                }
            };
            let fd = crate::types::int::as_i64(&v).filter(|x| *x >= i32::MIN as i64 && *x <= i32::MAX as i64);
            let Some(fd) = fd else { return Err(overflow_error("Python int too large to convert to C int")) };
            if fd < 0 {
                return Err(value_error(&format!("file descriptor cannot be a negative integer ({})", fd)));
            }
            Ok(fd)
        })();
        match r {
            Ok(fd) => fd as c_int,
            Err(_) => -1,
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_ReprEnter(o: *mut PyObject) -> c_int {
    unsafe {
        let ts = crate::threads::current_mut();
        let r = (|| -> PyResult<c_int> {
            let d = if (*ts).c.dict.is_null() {
                let d = dict::new_dict();
                (*ts).c.dict = d.clone().into_raw();
                d
            } else {
                PyRef::from_borrowed((*ts).c.dict)
            };
            let key = crate::name!("Py_Repr");
            let l = match dict::get_item_str(&d, key) {
                Some(l) => l,
                None => {
                    let l = crate::types::list::new_list_empty();
                    dict::set_item(&d, key, &l)?;
                    l
                }
            };
            for x in crate::types::list::list_items(&l) {
                if x.as_ptr() == o {
                    return Ok(1);
                }
            }
            crate::types::list::append(&l, (*o).to_ref());
            Ok(0)
        })();
        r.unwrap_or(-1)
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_ReprLeave(o: *mut PyObject) {
    unsafe {
        let saved = take_exc();
        let ts = crate::threads::current_mut();
        if !(*ts).c.dict.is_null() {
            if let Some(l) = dict::get_item_str(&*(*ts).c.dict, crate::name!("Py_Repr")) {
                let items = crate::types::list::list_items(&l);
                if let Some(i) = items.iter().rposition(|x| x.as_ptr() == o) {
                    let _ = crate::types::list::pop(&l, i);
                }
            }
        }
        if let Some(s) = saved {
            set_exc(s);
        }
    }
}

// ---------------------------------------------------------------- type data (PEP 697)

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GetTypeData(o: *mut PyObject, cls: *mut PyTypeObject) -> *mut c_void {
    unsafe {
        let base = (*cls).tp_base;
        let off = if base.is_null() { 0 } else { (*base).tp_basicsize };
        (o as *mut u8).offset(super::typeobj::align_up(off)) as *mut c_void
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GetTypeData_DuringGC(o: *mut PyObject, cls: *mut PyTypeObject) -> *mut c_void {
    unsafe { PyObject_GetTypeData(o, cls) }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GetTypeDataSize(cls: *mut PyTypeObject) -> Py_ssize_t {
    unsafe {
        let base = (*cls).tp_base;
        let off = if base.is_null() { 0 } else { super::typeobj::align_up((*base).tp_basicsize) };
        ((*cls).tp_basicsize - off).max(0)
    }
}

// ---------------------------------------------------------------- unstable object API

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_IsImmortal(o: *mut PyObject) -> c_int {
    unsafe { (*o).is_immortal() as c_int }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_SetImmortal(o: *mut PyObject) -> c_int {
    unsafe {
        if (*o).is_immortal() || pystr::is_str(&*o) {
            return 0;
        }
        make_immortal(o);
        1
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_TryIncRef(o: *mut PyObject) -> c_int {
    unsafe {
        let ob = &*o;
        if ob.is_immortal() {
            return 1;
        }
        if ob.ob_tid.load(std::sync::atomic::Ordering::Relaxed) == thread_id() {
            if ob.ob_ref_local.load(std::sync::atomic::Ordering::Relaxed) == 0 {
                return 0;
            }
            incref(o);
            return 1;
        }
        let mut cur = ob.ob_ref_shared.load(std::sync::atomic::Ordering::Relaxed);
        loop {
            if cur == 0 || cur == REF_MERGED {
                return 0;
            }
            match ob.ob_ref_shared.compare_exchange_weak(cur, cur + (1 << REF_SHARED_SHIFT), std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Relaxed) {
                Ok(_) => return 1,
                Err(x) => cur = x,
            }
        }
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_EnableTryIncRef(o: *mut PyObject) {
    unsafe {
        let _ = (*o).ob_ref_shared.compare_exchange(0, REF_MAYBE_WEAKREF, std::sync::atomic::Ordering::Relaxed, std::sync::atomic::Ordering::Relaxed);
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_Object_EnableDeferredRefcount(_o: *mut PyObject) -> c_int {
    0
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_Object_IsUniquelyReferenced(o: *mut PyObject) -> c_int {
    unsafe {
        let ob = &*o;
        (ob.ob_tid.load(std::sync::atomic::Ordering::Relaxed) == thread_id()
            && ob.ob_ref_local.load(std::sync::atomic::Ordering::Relaxed) == 1
            && ob.ob_ref_shared.load(std::sync::atomic::Ordering::Relaxed) == 0) as c_int
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyUnstable_Object_IsUniqueReferencedTemporary(o: *mut PyObject) -> c_int {
    unsafe { PyUnstable_Object_IsUniquelyReferenced(o) }
}

#[allow(dead_code)]
fn _unused() -> (PyRef, PyRef, PyRef) {
    (none::none_ref(), bool_::new_bool(true), tuple::empty_tuple())
}
#[allow(dead_code)]
fn _unused2(a: *mut PyTypeObject) -> bool {
    is_subtype(a, a)
}
