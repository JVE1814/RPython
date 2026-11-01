//! Methods of `object` and `type`, and class creation (`type.__new__`).

use crate::abstract_ as ab;
use crate::args;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::typeobject::*;
use crate::types::{dict, funcobject, int, list, none, str as pystr, tuple};
use crate::{name, raise};
use std::ffi::{c_int, c_void};

// ================================================================ object

fn excess_args(args: *mut PyObject, kw: *mut PyObject) -> bool {
    unsafe { tuple::tuple_len(&*args) > 0 || (!kw.is_null() && dict::dict_len(&*kw) > 0) }
}

pub unsafe extern "C" fn object_init(op: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        if excess_args(args, kw) {
            let tp = (*op).type_ptr();
            if (*tp).tp_init != Some(object_init) {
                let _ = err!(TypeError, "object.__init__() takes exactly one argument (the instance to initialize)");
                return -1;
            }
            if (*tp).tp_new == Some(object_new) {
                let _ = err!(
                    TypeError,
                    "{}.__init__() takes exactly one argument (the instance to initialize)",
                    type_name(&*tp)
                );
                return -1;
            }
        }
        0
    }
}

pub unsafe extern "C" fn object_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        if excess_args(args, kw) {
            if (*tp).tp_new != Some(object_new) {
                return into_c(Err(err!(TypeError, "object.__new__() takes exactly one argument (the type to instantiate)")));
            }
            if (*tp).tp_init == Some(object_init) {
                return into_c(Err(err!(TypeError, "{}() takes no arguments", type_name(&*tp))));
            }
        }
        if (*tp).tp_flags & Py_TPFLAGS_IS_ABSTRACT != 0 {
            let r = (|| -> PyResult {
                let abstracts = ab::getattr(type_obj(tp), name!("__abstractmethods__"))?;
                let mut names: Vec<String> = Vec::new();
                for n in ab::collect_iterable(&abstracts)? {
                    names.push(pystr::to_string_lossy(&n));
                }
                names.sort();
                let joined = names.iter().map(|n| format!("'{}'", n)).collect::<Vec<_>>().join(", ");
                raise!(
                    TypeError,
                    "Can't instantiate abstract class {} without an implementation for abstract method{} {}",
                    type_name(&*tp),
                    if names.len() > 1 { "s" } else { "" },
                    joined
                );
            })();
            return into_c(r);
        }
        let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
        alloc(tp, 0)
    }
}

/// tp_new usable for simple types that just allocate.
pub unsafe extern "C" fn generic_new(tp: *mut PyTypeObject, _args: *mut PyObject, _kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
        alloc(tp, 0)
    }
}

pub unsafe extern "C" fn object_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*op;
        let tp = o.type_ptr();
        let module = match ab::getattr_opt(type_obj(tp), name!("__module__")) {
            Ok(Some(m)) if pystr::is_str(&m) => Some(pystr::to_string_lossy(&m)),
            Ok(_) => None,
            Err(_) => return std::ptr::null_mut(),
        };
        let q = type_qualname(&*tp);
        let s = match module {
            Some(m) if m != "builtins" => format!("<{}.{} object at {:p}>", m, q, op),
            _ => format!("<{} object at {:p}>", q, op),
        };
        pystr::new_str(&s).into_raw()
    }
}

unsafe extern "C" fn object_str(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let tp = (*op).ty();
        match tp.tp_repr {
            Some(f) => f(op),
            None => object_repr(op),
        }
    }
}

pub unsafe extern "C" fn object_hash(op: *mut PyObject) -> Py_hash_t {
    crate::hash::hash_pointer(op as usize)
}

pub unsafe extern "C" fn object_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        match op {
            Py_EQ => {
                if a == b {
                    crate::types::bool_::new_bool(true).into_raw()
                } else {
                    none::not_implemented().to_ref().into_raw()
                }
            }
            Py_NE => {
                // By default, __ne__ delegates to __eq__ and inverts the result.
                let tp = (*a).ty();
                match tp.tp_richcompare {
                    Some(f) => {
                        let r = f(a, b, Py_EQ);
                        if r.is_null() {
                            return r;
                        }
                        let r = PyRef::from_raw(r);
                        if r.is(none::not_implemented()) {
                            return r.into_raw();
                        }
                        match ab::is_true(&r) {
                            Ok(t) => crate::types::bool_::new_bool(!t).into_raw(),
                            Err(_) => std::ptr::null_mut(),
                        }
                    }
                    None => none::not_implemented().to_ref().into_raw(),
                }
            }
            _ => none::not_implemented().to_ref().into_raw(),
        }
    }
}

fn object_get_class(o: &PyObject) -> PyResult {
    Ok(type_ref(o.type_ptr()))
}

fn compatible_for_assignment(old: *mut PyTypeObject, new: *mut PyTypeObject) -> bool {
    unsafe {
        let mut o = old;
        while is_heap_type(o) && (*o).tp_dealloc == Some(subtype_dealloc) && (*o).tp_basicsize == (*(*o).tp_base).tp_basicsize {
            o = (*o).tp_base;
        }
        let mut n = new;
        while is_heap_type(n) && (*n).tp_dealloc == Some(subtype_dealloc) && (*n).tp_basicsize == (*(*n).tp_base).tp_basicsize {
            n = (*n).tp_base;
        }
        if (*old).tp_basicsize != (*new).tp_basicsize || (*old).tp_itemsize != (*new).tp_itemsize {
            return false;
        }
        if ((*old).tp_flags ^ (*new).tp_flags) & (Py_TPFLAGS_PREHEADER | Py_TPFLAGS_HAVE_GC) != 0 {
            return false;
        }
        if (*old).tp_dictoffset != (*new).tp_dictoffset {
            return false;
        }
        solid_base(o) == solid_base(n) || o == n || same_slots_added(o, n)
    }
}

/// Two heap types adding the same __slots__ to the same base have the same layout.
fn same_slots_added(a: *mut PyTypeObject, b: *mut PyTypeObject) -> bool {
    unsafe {
        if !is_heap_type(a) || !is_heap_type(b) || (*a).tp_base != (*b).tp_base || (*a).tp_basicsize != (*b).tp_basicsize {
            return false;
        }
        let (sa, sb) = ((*heap(a)).ht_slots, (*heap(b)).ht_slots);
        if sa.is_null() || sb.is_null() {
            return sa.is_null() && sb.is_null();
        }
        ab::rich_eq(&*sa, &*sb).unwrap_or_else(|_| {
            crate::errors::clear_exc();
            false
        })
    }
}

fn object_set_class(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        Some(v) => v,
        None => raise!(TypeError, "can't delete __class__ attribute"),
    };
    if !is_type(v) {
        raise!(TypeError, "__class__ must be set to a class, not '{}' object", tname(v));
    }
    let new = as_type(v);
    let old = o.type_ptr();
    let mutable_ok = |t: *mut PyTypeObject| unsafe { is_heap_type(t) && (*t).tp_flags & Py_TPFLAGS_IMMUTABLETYPE == 0 };
    if !(mutable_ok(new) && mutable_ok(old)) {
        // module subclasses are allowed
        let modt = crate::types::module::module_type();
        if !(is_subtype(new, modt) && is_subtype(old, modt)) {
            raise!(TypeError, "__class__ assignment only supported for mutable types or ModuleType subclasses");
        }
    }
    if !compatible_for_assignment(old, new) {
        raise!(
            TypeError,
            "__class__ assignment: '{}' object layout differs from '{}'",
            type_name(unsafe { &*new }),
            type_name(unsafe { &*old })
        );
    }
    unsafe {
        incref(new as *mut PyObject);
        o.ob_type.store(new, std::sync::atomic::Ordering::Release);
        if is_heap_type(old) {
            decref(old as *mut PyObject);
        }
    }
    Ok(())
}

fn object_dir(o: &PyObject) -> PyResult {
    let d = dict::new_dict();
    if let Some(od) = ab::getattr_opt(o, name!("__dict__"))? {
        if dict::is_dict(&od) {
            dict::merge_dict(&d, &od, true)?;
        }
    }
    if let Some(cls) = ab::getattr_opt(o, name!("__class__"))? {
        merge_class_dict(&d, &cls)?;
    }
    Ok(list::new_list(dict::keys_vec(&d)))
}

fn merge_class_dict(d: &PyObject, cls: &PyObject) -> PyResult<()> {
    if let Some(cd) = ab::getattr_opt(cls, name!("__dict__"))? {
        let keys = ab::call_method_str(&cd, "keys", &[])?;
        for k in ab::collect_iterable(&keys)? {
            dict::set_item(d, &k, none::none())?;
        }
    }
    if let Some(bases) = ab::getattr_opt(cls, name!("__bases__"))? {
        if tuple::is_tuple(&bases) {
            for b in tuple::tuple_items(&bases) {
                merge_class_dict(d, b)?;
            }
        }
    }
    Ok(())
}

fn object_format(o: &PyObject, spec: &PyObject) -> PyResult {
    if !pystr::is_str(spec) {
        raise!(TypeError, "__format__() argument must be str, not {}", tname(spec));
    }
    if pystr::str_len(spec) > 0 {
        raise!(TypeError, "unsupported format string passed to {}.__format__", tname(o));
    }
    ab::str(o)
}

fn object_sizeof(o: &PyObject) -> PyResult {
    let tp = o.ty();
    let mut n = tp.tp_basicsize as i64;
    if tp.tp_itemsize != 0 {
        n += tp.tp_itemsize as i64 * var_size(o).abs() as i64;
    }
    Ok(int::new_int(n))
}

fn object_init_subclass(_cls: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let (pos, kws) = args::split(args, kw);
    if !pos.is_empty() {
        raise!(TypeError, "object.__init_subclass__() takes no arguments");
    }
    if !kws.is_empty() {
        raise!(
            TypeError,
            "{}.__init_subclass__() takes no keyword arguments",
            type_name(unsafe { &*as_type(_cls) })
        );
    }
    Ok(none::none_ref())
}

fn object_subclasshook(_cls: &PyObject, _args: &[&PyObject]) -> PyResult {
    Ok(none::not_implemented().to_ref())
}

fn object_getstate(o: &PyObject) -> PyResult {
    crate::modules::copyreg_support::object_getstate(o, false)
}

fn object_reduce(o: &PyObject) -> PyResult {
    crate::modules::copyreg_support::reduce_ex(o, 0)
}

fn object_reduce_ex(o: &PyObject, proto: &PyObject) -> PyResult {
    let p = ab::as_i64(proto)?;
    // If __reduce__ is overridden, call it.
    if let Some(r) = ab::getattr_opt(o, name!("__reduce__"))? {
        let obj_reduce = crate::types::typeobject::lookup(object_type(), name!("__reduce__"));
        let cls_reduce = crate::types::typeobject::lookup(o.type_ptr(), name!("__reduce__"));
        let overridden = match (&obj_reduce, &cls_reduce) {
            (Some(a), Some(b)) => !a.is(b),
            _ => false,
        };
        if overridden {
            return ab::call(&r, &[]);
        }
    }
    crate::modules::copyreg_support::reduce_ex(o, p)
}

// ================================================================ type

pub unsafe extern "C" fn type_repr(op: *mut PyObject) -> *mut PyObject {
    unsafe {
        let tp = op as *mut PyTypeObject;
        let q = type_qualname(&*tp);
        let module = match ab::getattr_opt(&*op, name!("__module__")) {
            Ok(Some(m)) if pystr::is_str(&m) => Some(pystr::to_string_lossy(&m)),
            Ok(_) => None,
            Err(_) => {
                clear_exc();
                None
            }
        };
        let s = match module {
            Some(m) if m != "builtins" => format!("<class '{}.{}'>", m, q),
            _ => format!("<class '{}'>", q),
        };
        pystr::new_str(&s).into_raw()
    }
}

unsafe extern "C" fn type_call(op: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let tp = op as *mut PyTypeObject;
        let nargs = tuple::tuple_len(&*args);
        if tp == type_type() && nargs == 1 && (kw.is_null() || dict::dict_len(&*kw) == 0) {
            return type_ref(tuple::tuple_get(&*args, 0).type_ptr()).into_raw();
        }
        let new = match (*tp).tp_new {
            Some(f) => f,
            None => return into_c(Err(err!(TypeError, "cannot create '{}' instances", type_fqname(tp)))),
        };
        let obj = new(tp, args, kw);
        if obj.is_null() {
            return obj;
        }
        if tp == type_type() && nargs == 1 {
            return obj;
        }
        let ot = (*obj).type_ptr();
        if !is_subtype(ot, tp) {
            return obj;
        }
        if let Some(init) = (*ot).tp_init {
            if init(obj, args, kw) < 0 {
                decref(obj);
                return std::ptr::null_mut();
            }
        }
        obj
    }
}

/// Vectorcall for type objects (avoids building a tuple for the common case).
pub unsafe extern "C" fn type_vectorcall(
    callable: *mut PyObject,
    args: *const *mut PyObject,
    nargsf: usize,
    kwnames: *mut PyObject,
) -> *mut PyObject {
    unsafe {
        let nargs = nargsf & !PY_VECTORCALL_ARGUMENTS_OFFSET;
        let all = if nargs == 0 { &[][..] } else { std::slice::from_raw_parts(args, nargs) };
        if kwnames.is_null() || tuple::tuple_len(&*kwnames) == 0 {
            let t = tuple::new_tuple_from_ptrs(all);
            return type_call(callable, t.as_ptr(), std::ptr::null_mut());
        }
        crate::abstract_::vectorcall_call(callable, std::ptr::null_mut(), std::ptr::null_mut());
        std::ptr::null_mut()
    }
}

unsafe extern "C" fn type_getattro(op: *mut PyObject, nm: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(type_getattr_impl(&*op, &*nm, false).and_then(|r| r.ok_or_else(|| attribute_error_for(&*op, &*nm)))) }
}

/// type.__getattribute__ with optional suppression of AttributeError.
pub fn type_getattr_impl(o: &PyObject, nm: &PyObject, _suppress: bool) -> PyResult<Option<PyRef>> {
    if !pystr::is_str(nm) {
        raise!(TypeError, "attribute name must be string, not '{}'", tname(nm));
    }
    let tp = as_type(o);
    let meta = o.type_ptr();
    let meta_attr = lookup(meta, nm);
    let mut meta_get: descrgetfunc = None;
    if let Some(ma) = &meta_attr {
        meta_get = ma.ty().tp_descr_get;
        if meta_get.is_some() && ma.ty().tp_descr_set.is_some() {
            return unsafe { from_c(meta_get.unwrap()(ma.as_ptr(), o.as_ptr(), meta as *mut PyObject)).map(Some) };
        }
    }
    if let Some(attr) = lookup(tp, nm) {
        if let Some(g) = attr.ty().tp_descr_get {
            return unsafe { from_c(g(attr.as_ptr(), std::ptr::null_mut(), o.as_ptr())).map(Some) };
        }
        return Ok(Some(attr));
    }
    if let Some(ma) = meta_attr {
        if let Some(g) = meta_get {
            return unsafe { from_c(g(ma.as_ptr(), o.as_ptr(), meta as *mut PyObject)).map(Some) };
        }
        return Ok(Some(ma));
    }
    Ok(None)
}

unsafe extern "C" fn type_setattro(op: *mut PyObject, nm: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        let tp = op as *mut PyTypeObject;
        if (*tp).tp_flags & Py_TPFLAGS_IMMUTABLETYPE != 0 {
            let _ = err!(
                TypeError,
                "cannot set '{}' attribute of immutable type '{}'",
                pystr::to_string_lossy(&*nm),
                std::ffi::CStr::from_ptr((*tp).tp_name).to_string_lossy()
            );
            return -1;
        }
        if !pystr::is_str(&*nm) {
            let _ = err!(TypeError, "attribute name must be string, not '{}'", tname(&*nm));
            return -1;
        }
        let n = pystr::intern(PyRef::from_borrowed(nm));
        let r = generic_setattr_with_dict(&*op, &n, if v.is_null() { None } else { Some(&*v) }, Some(&*(*tp).tp_dict));
        if r.is_err() {
            return -1;
        }
        crate::types::slots::type_modified(tp);
        crate::types::slots::update_slot(tp, &n);
        0
    }
}

unsafe extern "C" fn type_dealloc(op: *mut PyObject) {
    unsafe {
        let tp = op as *mut PyTypeObject;
        crate::gc::untrack(op);
        crate::interp::cache::unwatch_type_dict(tp); // jit
        crate::types::weakref::clear_weakrefs(op);
        if !(*tp).tp_bases.is_null() {
            for b in tuple::tuple_items(&*(*tp).tp_bases) {
                remove_subclass(as_type(b), tp);
            }
        }
        let h = heap(tp);
        for p in [(*tp).tp_dict, (*tp).tp_bases, (*tp).tp_mro, (*h).ht_name, (*h).ht_qualname, (*h).ht_slots, (*h).ht_module] {
            xdecref(p);
        }
        xdecref((*tp).tp_base as *mut PyObject);
        if !(*tp).tp_subclasses.is_null() {
            drop(Box::from_raw((*tp).tp_subclasses as *mut Vec<usize>));
        }
        if !(*h)._ht_tpname.is_null() {
            crate::alloc::raw_free((*h)._ht_tpname as *mut u8);
        }
        if !(*tp).tp_members.is_null() && (*tp).tp_members != std::ptr::null_mut() {
            // members array allocated by type_new (leaked strings are fine)
        }
        let meta = (*op).type_ptr();
        crate::alloc::free_object(op);
        if is_heap_type(meta) {
            decref(meta as *mut PyObject);
        }
    }
}

unsafe extern "C" fn type_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let tp = op as *mut PyTypeObject;
        if !is_heap_type(tp) {
            return 0;
        }
        let v = visit.unwrap();
        let h = heap(tp);
        for p in [(*tp).tp_dict, (*tp).tp_mro, (*tp).tp_bases, (*tp).tp_base as *mut PyObject, (*h).ht_module] {
            if !p.is_null() {
                let r = v(p, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        let meta = (*op).type_ptr();
        if is_heap_type(meta) {
            return v(meta as *mut PyObject, arg);
        }
        0
    }
}

unsafe extern "C" fn type_clear(op: *mut PyObject) -> c_int {
    unsafe {
        let tp = op as *mut PyTypeObject;
        if !(*tp).tp_dict.is_null() {
            dict::clear(&*(*tp).tp_dict);
        }
        let m = (*tp).tp_mro;
        (*tp).tp_mro = std::ptr::null_mut();
        xdecref(m);
        0
    }
}

unsafe extern "C" fn type_is_gc(op: *mut PyObject) -> c_int {
    unsafe { is_heap_type(op as *mut PyTypeObject) as c_int }
}

fn t(o: &PyObject) -> *mut PyTypeObject {
    as_type(o)
}

fn type_get_name(o: &PyObject) -> PyResult {
    Ok(pystr::new_str(&type_name(unsafe { &*t(o) })))
}
fn check_set_special(o: &PyObject, v: Option<&PyObject>, attr: &str) -> PyResult<()> {
    let tp = t(o);
    if unsafe { (*tp).tp_flags & Py_TPFLAGS_IMMUTABLETYPE != 0 } {
        raise!(TypeError, "cannot set '{}' attribute of immutable type '{}'", attr, type_name(unsafe { &*tp }));
    }
    if v.is_none() {
        raise!(TypeError, "cannot delete '{}' attribute of immutable type '{}'", attr, type_name(unsafe { &*tp }));
    }
    Ok(())
}
fn type_set_name(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    check_set_special(o, v, "__name__")?;
    let v = v.unwrap();
    if !pystr::is_str(v) {
        raise!(TypeError, "can only assign string to {}.__name__, not '{}'", type_name(unsafe { &*t(o) }), tname(v));
    }
    let s = pystr::as_utf8(v)?;
    if s.contains('\0') {
        raise!(ValueError, "type name must not contain null characters");
    }
    unsafe {
        let h = heap(t(o));
        let old = (*h).ht_name;
        (*h).ht_name = v.to_ref().into_raw();
        xdecref(old);
        let mut b = s.as_bytes().to_vec();
        b.push(0);
        let p = crate::alloc::raw_malloc(b.len());
        std::ptr::copy_nonoverlapping(b.as_ptr(), p, b.len());
        let oldp = (*h)._ht_tpname;
        (*h)._ht_tpname = p as *mut _;
        (*t(o)).tp_name = p as *const _;
        if !oldp.is_null() {
            crate::alloc::raw_free(oldp as *mut u8);
        }
    }
    Ok(())
}
fn type_get_qualname(o: &PyObject) -> PyResult {
    Ok(pystr::new_str(&type_qualname(unsafe { &*t(o) })))
}
fn type_set_qualname(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    check_set_special(o, v, "__qualname__")?;
    let v = v.unwrap();
    if !pystr::is_str(v) {
        raise!(TypeError, "can only assign string to {}.__qualname__, not '{}'", type_name(unsafe { &*t(o) }), tname(v));
    }
    unsafe {
        let h = heap(t(o));
        let old = (*h).ht_qualname;
        (*h).ht_qualname = v.to_ref().into_raw();
        xdecref(old);
    }
    Ok(())
}
fn type_get_bases(o: &PyObject) -> PyResult {
    unsafe { Ok(PyRef::from_borrowed((*t(o)).tp_bases)) }
}
fn type_set_bases(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    check_set_special(o, v, "__bases__")?;
    let v = v.unwrap();
    let tp = t(o);
    if !tuple::is_tuple(v) {
        raise!(TypeError, "can only assign tuple to {}.__bases__, not {}", type_name(unsafe { &*tp }), tname(v));
    }
    if tuple::tuple_len(v) == 0 {
        raise!(TypeError, "can only assign non-empty tuple to {}.__bases__, not ()", type_name(unsafe { &*tp }));
    }
    for b in tuple::tuple_items(v) {
        if !is_type(b) {
            raise!(TypeError, "{}.__bases__ must be tuple of classes, not '{}'", type_name(unsafe { &*tp }), tname(b));
        }
        if is_subtype(as_type(b), tp) {
            raise!(TypeError, "a __bases__ item causes an inheritance cycle");
        }
    }
    let new_base = best_base(v)?;
    if !compatible_for_assignment(unsafe { (*tp).tp_base }, new_base) {
        raise!(
            TypeError,
            "__bases__ assignment: '{}' deallocator differs from '{}'",
            type_name(unsafe { &*new_base }),
            type_name(unsafe { &*(*tp).tp_base })
        );
    }
    unsafe {
        let old_bases = (*tp).tp_bases;
        let old_base = (*tp).tp_base;
        for b in tuple::tuple_items(&*old_bases) {
            remove_subclass(as_type(b), tp);
        }
        (*tp).tp_bases = v.to_ref().into_raw();
        incref(new_base as *mut PyObject);
        (*tp).tp_base = new_base;
        let r = mro_internal(tp);
        if r.is_err() {
            let nb = (*tp).tp_bases;
            (*tp).tp_bases = old_bases;
            decref(nb);
            decref(new_base as *mut PyObject);
            (*tp).tp_base = old_base;
            for b in tuple::tuple_items(&*old_bases) {
                add_subclass(as_type(b), tp);
            }
            return r;
        }
        for b in tuple::tuple_items(&*(*tp).tp_bases) {
            add_subclass(as_type(b), tp);
        }
        // recompute subclass MROs
        for sub in subclasses(tp) {
            let _ = mro_internal(sub);
        }
        decref(old_bases);
        decref(old_base as *mut PyObject);
        crate::types::slots::fixup_slots(tp);
    }
    Ok(())
}
fn type_get_base(o: &PyObject) -> PyResult {
    unsafe {
        let b = (*t(o)).tp_base;
        Ok(if b.is_null() { none::none_ref() } else { type_ref(b) })
    }
}
fn type_get_mro(o: &PyObject) -> PyResult {
    unsafe {
        let m = (*t(o)).tp_mro;
        Ok(if m.is_null() { none::none_ref() } else { PyRef::from_borrowed(m) })
    }
}
fn type_get_module(o: &PyObject) -> PyResult {
    let tp = t(o);
    if is_heap_type(tp) {
        match dict::get_item_str(type_dict(tp), name!("__module__")) {
            Some(m) => Ok(m),
            None => raise!(AttributeError, "__module__"),
        }
    } else {
        Ok(pystr::new_str(&type_module(tp).unwrap_or_else(|| "builtins".into())))
    }
}
fn type_set_module(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    check_set_special(o, v, "__module__")?;
    crate::types::slots::type_modified(t(o));
    dict::set_item(type_dict(t(o)), name!("__module__"), v.unwrap())
}
fn type_get_dict(o: &PyObject) -> PyResult {
    crate::types::mappingproxy::new_mappingproxy(type_dict(t(o)))
}
fn type_get_doc(o: &PyObject) -> PyResult {
    let tp = t(o);
    if !is_heap_type(tp) && unsafe { !(*tp).tp_doc.is_null() } {
        let raw = unsafe { std::ffi::CStr::from_ptr((*tp).tp_doc).to_string_lossy().into_owned() };
        return Ok(match crate::types::descr::doc_without_signature(&raw) {
            Some(s) => pystr::new_str(&s),
            None => none::none_ref(),
        });
    }
    match dict::get_item_str(type_dict(tp), name!("__doc__")) {
        Some(d) => match d.ty().tp_descr_get {
            Some(g) => unsafe { from_c(g(d.as_ptr(), std::ptr::null_mut(), o.as_ptr())) },
            None => Ok(d),
        },
        None => Ok(none::none_ref()),
    }
}
fn type_set_doc(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    check_set_special(o, v, "__doc__")?;
    crate::types::slots::type_modified(t(o));
    dict::set_item(type_dict(t(o)), name!("__doc__"), v.unwrap())
}
fn type_get_text_signature(o: &PyObject) -> PyResult {
    let tp = t(o);
    unsafe {
        if (*tp).tp_doc.is_null() {
            return Ok(none::none_ref());
        }
        let raw = std::ffi::CStr::from_ptr((*tp).tp_doc).to_string_lossy().into_owned();
        Ok(match crate::types::descr::text_signature(&type_name(&*tp), &raw) {
            Some(s) => pystr::new_str(&s),
            None => none::none_ref(),
        })
    }
}
fn type_get_abstractmethods(o: &PyObject) -> PyResult {
    let tp = t(o);
    if tp != type_type() {
        if let Some(v) = dict::get_item_str(type_dict(tp), name!("__abstractmethods__")) {
            return Ok(v);
        }
    }
    raise!(AttributeError, "__abstractmethods__");
}
fn type_set_abstractmethods(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let tp = t(o);
    let abstract_ = match v {
        Some(v) => {
            dict::set_item(type_dict(tp), name!("__abstractmethods__"), v)?;
            ab::is_true(v)?
        }
        None => {
            if !dict::del_item(type_dict(tp), name!("__abstractmethods__"))? {
                raise!(AttributeError, "__abstractmethods__");
            }
            false
        }
    };
    crate::types::slots::type_modified(tp);
    unsafe {
        if abstract_ {
            (*tp).tp_flags |= Py_TPFLAGS_IS_ABSTRACT;
        } else {
            (*tp).tp_flags &= !Py_TPFLAGS_IS_ABSTRACT;
        }
    }
    Ok(())
}
fn type_get_annotations(o: &PyObject) -> PyResult {
    crate::modules::annotations::type_get_annotations(o)
}
fn type_set_annotations(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    crate::modules::annotations::type_set_annotations(o, v)
}
fn type_get_annotate(o: &PyObject) -> PyResult {
    crate::modules::annotations::type_get_annotate(o)
}
fn type_set_annotate(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    crate::modules::annotations::type_set_annotate(o, v)
}
fn type_get_type_params(o: &PyObject) -> PyResult {
    let tp = t(o);
    if tp == type_type() {
        return Ok(tuple::empty_tuple());
    }
    match dict::get_item_str(type_dict(tp), name!("__type_params__")) {
        Some(v) => Ok(v),
        None => Ok(tuple::empty_tuple()),
    }
}
fn type_set_type_params(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    check_set_special(o, v, "__type_params__")?;
    let v = v.unwrap();
    if !tuple::is_tuple(v) {
        raise!(TypeError, "__type_params__ must be set to a tuple, not {}", tname(v));
    }
    crate::types::slots::type_modified(t(o));
    dict::set_item(type_dict(t(o)), name!("__type_params__"), v)
}
fn type_basicsize(o: &PyObject) -> PyResult {
    Ok(int::new_int(unsafe { (*t(o)).tp_basicsize } as i64))
}
fn type_itemsize(o: &PyObject) -> PyResult {
    Ok(int::new_int(unsafe { (*t(o)).tp_itemsize } as i64))
}
fn type_flags(o: &PyObject) -> PyResult {
    Ok(int::new_int(unsafe { (*t(o)).tp_flags } as i64))
}
fn type_weakrefoffset(o: &PyObject) -> PyResult {
    Ok(int::new_int(unsafe { (*t(o)).tp_weaklistoffset } as i64))
}
fn type_dictoffset(o: &PyObject) -> PyResult {
    Ok(int::new_int(unsafe { (*t(o)).tp_dictoffset } as i64))
}

fn type_mro_method(o: &PyObject) -> PyResult {
    let r = compute_c3_mro(t(o))?;
    Ok(list::new_list(tuple::tuple_to_vec(&r)))
}
fn type_subclasses_method(o: &PyObject) -> PyResult {
    Ok(list::new_list(subclasses(t(o)).into_iter().map(type_ref).collect()))
}
fn type_instancecheck(o: &PyObject, inst: &PyObject) -> PyResult {
    let tp = t(o);
    if isinstance_exact_or_sub(inst, tp) {
        return Ok(crate::types::bool_::new_bool(true));
    }
    if let Some(c) = ab::getattr_opt(inst, name!("__class__"))? {
        if is_type(&c) && !c.is(type_obj(inst.type_ptr())) {
            return Ok(crate::types::bool_::new_bool(is_subtype(as_type(&c), tp)));
        }
    }
    Ok(crate::types::bool_::new_bool(false))
}
fn type_subclasscheck(o: &PyObject, sub: &PyObject) -> PyResult {
    if !is_type(sub) {
        if ab::getattr_opt(sub, name!("__bases__"))?.map_or(false, |b| tuple::is_tuple(&b)) {
            return Ok(crate::types::bool_::new_bool(ab::issubclass(sub, o)?));
        }
        raise!(TypeError, "issubclass() arg 1 must be a class");
    }
    Ok(crate::types::bool_::new_bool(is_subtype(as_type(sub), t(o))))
}
fn type_prepare(_cls: &PyObject, _args: &[&PyObject], _kw: Option<&PyObject>) -> PyResult {
    Ok(dict::new_dict())
}
fn type_dir(o: &PyObject) -> PyResult {
    let d = dict::new_dict();
    merge_class_dict(&d, o)?;
    Ok(list::new_list(dict::keys_vec(&d)))
}
fn type_sizeof(o: &PyObject) -> PyResult {
    let tp = t(o);
    let n = if is_heap_type(tp) { std::mem::size_of::<PyHeapTypeObject>() } else { std::mem::size_of::<PyTypeObject>() };
    Ok(int::new_int(n as i64))
}
fn type_or(a: &PyObject, b: &PyObject) -> PyResult {
    crate::types::union::union_or(a, b)
}
fn type_ror(a: &PyObject, b: &PyObject) -> PyResult {
    crate::types::union::union_or(b, a)
}

unsafe extern "C" fn type_init(_op: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let n = tuple::tuple_len(&*args);
        if n == 1 && !kw.is_null() && dict::dict_len(&*kw) > 0 {
            let _ = err!(TypeError, "type.__init__() takes no keyword arguments");
            return -1;
        }
        if n != 1 && n != 3 {
            let _ = err!(TypeError, "type.__init__() takes 1 or 3 arguments");
            return -1;
        }
        0
    }
}

// ================================================================ class creation

/// Determine the most derived metaclass.
pub fn calculate_metaclass(meta: *mut PyTypeObject, bases: &PyObject) -> PyResult<*mut PyTypeObject> {
    let mut winner = meta;
    for b in tuple::tuple_items(bases) {
        let bt = b.type_ptr();
        if is_subtype(winner, bt) {
            continue;
        }
        if is_subtype(bt, winner) {
            winner = bt;
            continue;
        }
        raise!(
            TypeError,
            "metaclass conflict: the metaclass of a derived class must be a (non-strict) subclass of the metaclasses of all its bases"
        );
    }
    Ok(winner)
}

fn best_base(bases: &PyObject) -> PyResult<*mut PyTypeObject> {
    let mut base: *mut PyTypeObject = std::ptr::null_mut();
    let mut winner: *mut PyTypeObject = std::ptr::null_mut();
    for b in tuple::tuple_items(bases) {
        if !is_type(b) {
            raise!(TypeError, "bases must be types");
        }
        let bt = as_type(b);
        unsafe {
            if (*bt).tp_flags & Py_TPFLAGS_READY == 0 {
                type_ready(bt)?;
            }
            if (*bt).tp_flags & Py_TPFLAGS_BASETYPE == 0 {
                raise!(TypeError, "type '{}' is not an acceptable base type", type_fqname(bt));
            }
        }
        let candidate = solid_base(bt);
        if winner.is_null() {
            winner = candidate;
            base = bt;
        } else if is_subtype(winner, candidate) {
        } else if is_subtype(candidate, winner) {
            winner = candidate;
            base = bt;
        } else {
            raise!(TypeError, "multiple bases have instance lay-out conflict");
        }
    }
    Ok(base)
}

fn mangle(class_name: &str, nm: &str) -> String {
    if !nm.starts_with("__") || nm.ends_with("__") || nm.contains('.') {
        return nm.to_string();
    }
    let stripped = class_name.trim_start_matches('_');
    if stripped.is_empty() {
        return nm.to_string();
    }
    format!("_{}{}", stripped, nm)
}

fn is_identifier(s: &PyObject) -> bool {
    let d = pystr::str_data(s);
    if d.len() == 0 {
        return false;
    }
    let cps = d.to_vec();
    rpy_unicode::is_identifier(&cps)
}

unsafe extern "C" fn type_new_slot(meta: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(type_new(meta, &*args, if kw.is_null() { None } else { Some(&*kw) })) }
}

pub fn type_new(meta: *mut PyTypeObject, args: &PyObject, kw: Option<&PyObject>) -> PyResult {
    let a = tuple::tuple_items(args);
    let nkw = kw.map_or(0, |k| dict::dict_len(k));
    if meta == type_type() && a.len() == 1 && nkw == 0 {
        return Ok(type_ref(a[0].type_ptr()));
    }
    if a.len() != 3 {
        if a.len() == 1 {
            raise!(TypeError, "type() takes 1 or 3 arguments");
        }
        raise!(TypeError, "type() takes 1 or 3 arguments");
    }
    let (nm, bases, ns) = (a[0], a[1], a[2]);
    if !pystr::is_str(nm) {
        raise!(TypeError, "type.__new__() argument 1 must be str, not {}", tname(nm));
    }
    if !tuple::is_tuple(bases) {
        raise!(TypeError, "type.__new__() argument 2 must be tuple, not {}", tname(bases));
    }
    if !dict::is_dict(ns) {
        raise!(TypeError, "type.__new__() argument 3 must be dict, not {}", tname(ns));
    }
    // metaclass
    let winner = calculate_metaclass(meta, bases)?;
    if winner != meta {
        unsafe {
            if (*winner).tp_new != Some(type_new_slot) {
                let f = (*winner).tp_new.unwrap();
                let kwp = kw.map_or(std::ptr::null_mut(), |k| k.as_ptr());
                return from_c(f(winner, args.as_ptr(), kwp));
            }
        }
    }
    let meta = winner;
    let bases_owned: PyRef = if tuple::tuple_len(bases) == 0 {
        tuple::new_tuple(vec![type_ref(object_type())])
    } else {
        for b in tuple::tuple_items(bases) {
            if !is_type(b) {
                if ab::getattr_opt(b, name!("__mro_entries__"))?.is_some() {
                    raise!(TypeError, "type() doesn't support MRO entry resolution; use types.new_class()");
                }
            }
        }
        bases.to_ref()
    };
    let base = best_base(&bases_owned)?;
    let dict = dict::copy(ns)?;
    let name_s = pystr::to_string_lossy(nm);
    if pystr::as_utf8(nm)?.contains('\0') {
        raise!(ValueError, "type name must not contain null characters");
    }

    // __slots__
    let slots = dict::get_item_str(&dict, name!("__slots__"));
    let mut add_dict = false;
    let mut add_weak = false;
    let mut slot_names: Vec<PyRef> = Vec::new();
    let base_has_dict = unsafe { (*base).tp_dictoffset != 0 || (*base).tp_flags & Py_TPFLAGS_MANAGED_DICT != 0 };
    let base_has_weak = unsafe { (*base).tp_weaklistoffset != 0 || (*base).tp_flags & Py_TPFLAGS_MANAGED_WEAKREF != 0 };
    match &slots {
        None => {
            add_dict = !base_has_dict;
            add_weak = !base_has_weak;
        }
        Some(s) => {
            let seq: Vec<PyRef> = if pystr::is_str(s) { vec![s.clone()] } else { ab::collect_iterable(s)? };
            unsafe {
                if !seq.is_empty() && (*base).tp_itemsize != 0 {
                    raise!(
                        TypeError,
                        "nonempty __slots__ not supported for subtype of '{}'",
                        type_name(&*base)
                    );
                }
            }
            for item in seq.iter() {
                if !pystr::is_str(item) {
                    raise!(TypeError, "__slots__ items must be strings, not '{}'", tname(item));
                }
                if !is_identifier(item) {
                    raise!(TypeError, "__slots__ must be identifiers");
                }
                let st = pystr::to_string_lossy(item);
                if st == "__dict__" {
                    if base_has_dict || add_dict {
                        raise!(TypeError, "__dict__ slot disallowed: we already got one");
                    }
                    add_dict = true;
                    continue;
                }
                if st == "__weakref__" {
                    if base_has_weak || add_weak {
                        raise!(TypeError, "__weakref__ slot disallowed: we already got one");
                    }
                    add_weak = true;
                    continue;
                }
                let mangled = pystr::intern_str(&mangle(&name_s, &st));
                if slot_names.iter().any(|x| pystr::str_eq(x, &mangled)) {
                    raise!(TypeError, "duplicate slot name");
                }
                if dict::get_item_str(&dict, &mangled).is_some() {
                    let is_qual = st == "__qualname__" || st == "__classcell__";
                    if !is_qual {
                        raise!(ValueError, "'{}' in __slots__ conflicts with class variable", st);
                    }
                }
                slot_names.push(mangled);
            }
            slot_names.sort_by(|x, y| pystr::str_cmp(x, y));
            // if slots is a dict, keep it as slot docs; store the tuple of names
        }
    }

    unsafe {
        // allocate the heap type with room for the slot member table
        let nslots = slot_names.len();
        let tp_obj = generic_alloc(meta, nslots as isize);
        if tp_obj.is_null() {
            return Err(PyErr);
        }
        let tp = tp_obj as *mut PyTypeObject;
        let h = heap(tp);
        let typ = PyRef::from_raw(tp_obj);
        (*tp).tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HEAPTYPE | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_GC;
        if (*base).tp_flags & Py_TPFLAGS_HAVE_GC == 0 && nslots == 0 && !add_dict && !add_weak {
            // could stay non-GC but heap type instances may still form cycles via __class__
        }
        (*tp).tp_as_async = &raw mut (*h).as_async;
        (*tp).tp_as_number = &raw mut (*h).as_number;
        (*tp).tp_as_sequence = &raw mut (*h).as_sequence;
        (*tp).tp_as_mapping = &raw mut (*h).as_mapping;
        (*tp).tp_as_buffer = &raw mut (*h).as_buffer;
        (*h).ht_name = nm.to_ref().into_raw();
        let mut b = pystr::as_utf8(nm)?.as_bytes().to_vec();
        b.push(0);
        let p = crate::alloc::raw_malloc(b.len());
        std::ptr::copy_nonoverlapping(b.as_ptr(), p, b.len());
        (*h)._ht_tpname = p as *mut _;
        (*tp).tp_name = p as *const _;
        (*tp).tp_bases = bases_owned.clone().into_raw();
        incref(base as *mut PyObject);
        (*tp).tp_base = base;
        (*tp).tp_dict = dict.clone().into_raw();

        // __module__
        if dict::get_item_str(&dict, name!("__module__")).is_none() {
            if let Some(g) = crate::interp::current_globals() {
                if let Some(m) = dict::get_item_str(&g, name!("__name__")) {
                    dict::set_item(&dict, name!("__module__"), &m)?;
                }
            }
        }
        // __qualname__
        match dict::get_item_str(&dict, name!("__qualname__")) {
            Some(q) => {
                if !pystr::is_str(&q) {
                    raise!(TypeError, "type __qualname__ must be a str, not {}", tname(&q));
                }
                (*h).ht_qualname = q.into_raw();
                dict::del_item(&dict, name!("__qualname__"))?;
            }
            None => (*h).ht_qualname = nm.to_ref().into_raw(),
        }
        // __doc__
        match dict::get_item_str(&dict, name!("__doc__")) {
            Some(d) => {
                if pystr::is_str(&d) {
                    let s = pystr::to_string_lossy(&d);
                    let mut v = s.into_bytes();
                    v.push(0);
                    (*tp).tp_doc = Box::leak(v.into_boxed_slice()).as_ptr() as *const _;
                }
            }
            // a `__doc__` slot provides the member descriptor instead
            None if slot_names.iter().any(|s| pystr::str_eq_ascii(s, "__doc__")) => {}
            None => dict::set_item(&dict, name!("__doc__"), none::none())?,
        }
        // special methods that are implicitly static/class methods
        if let Some(f) = dict::get_item_str(&dict, name!("__new__")) {
            if funcobject::is_function(&f) {
                dict::set_item(&dict, name!("__new__"), &funcobject::new_staticmethod(f))?;
            }
        }
        for n in [name!("__init_subclass__"), name!("__class_getitem__")] {
            if let Some(f) = dict::get_item_str(&dict, n) {
                if funcobject::is_function(&f) {
                    dict::set_item(&dict, n, &funcobject::new_classmethod(f))?;
                }
            }
        }
        // layout
        let mut slotoffset = (*base).tp_basicsize as usize;
        let members_ptr = (tp_obj as *mut u8).add(std::mem::size_of::<PyHeapTypeObject>()) as *mut PyMemberDef;
        let mut mvec: Vec<PyMemberDef> = Vec::with_capacity(nslots + 1);
        for s in slot_names.iter() {
            mvec.push(PyMemberDef {
                name: crate::methods::cstr_static(&pystr::to_string_lossy(s)),
                type_code: Py_T_OBJECT_EX,
                offset: slotoffset as isize,
                flags: 0,
                doc: std::ptr::null(),
            });
            slotoffset += 8;
        }
        let _ = members_ptr;
        if nslots > 0 {
            (*tp).tp_members = crate::methods::mbtable(mvec);
        }
        (*h).ht_slots = if slots.is_some() { tuple::new_tuple(slot_names.clone()).into_raw() } else { std::ptr::null_mut() };
        if add_dict {
            (*tp).tp_flags |= Py_TPFLAGS_MANAGED_DICT;
        }
        if add_weak {
            (*tp).tp_flags |= Py_TPFLAGS_MANAGED_WEAKREF;
        }
        // inherit preheader flags from base
        (*tp).tp_flags |= (*base).tp_flags & Py_TPFLAGS_PREHEADER;
        (*tp).tp_basicsize = slotoffset as isize;
        (*tp).tp_itemsize = (*base).tp_itemsize;
        (*tp).tp_dealloc = Some(subtype_dealloc);
        (*tp).tp_traverse = Some(subtype_traverse);
        (*tp).tp_clear = Some(subtype_clear);
        (*tp).tp_alloc = Some(generic_alloc);
        (*tp).tp_free = Some(generic_free);
        if (*tp).tp_flags & Py_TPFLAGS_MANAGED_DICT != 0 && dict::get_item_str(&dict, name!("__dict__")).is_none() {
            let gs = subtype_getset_dict();
            let descr = crate::types::descr::new_getset_descr(tp, gs);
            dict::set_item(&dict, name!("__dict__"), &descr)?;
        }
        if (*tp).tp_flags & Py_TPFLAGS_MANAGED_WEAKREF != 0 && add_weak && dict::get_item_str(&dict, name!("__weakref__")).is_none() {
            let gs = subtype_getset_weakref();
            let descr = crate::types::descr::new_getset_descr(tp, gs);
            dict::set_item(&dict, name!("__weakref__"), &descr)?;
        }
        // __firstlineno__ / __static_attributes__ are provided by the compiler in the namespace
        type_ready(tp)?;
        crate::types::slots::fixup_slots(tp);
        if (*tp).tp_new.is_none() || (*base).tp_new.is_none() {
            (*tp).tp_new = (*base).tp_new;
        }
        // abstract?
        if let Some(am) = dict::get_item_str(&dict, name!("__abstractmethods__")) {
            if ab::is_true(&am)? {
                (*tp).tp_flags |= Py_TPFLAGS_IS_ABSTRACT;
            }
        }
        // __classcell__
        if let Some(cell) = dict::get_item_str(&dict, name!("__classcell__")) {
            if !cell.is_type(funcobject::cell_type()) {
                raise!(TypeError, "__classcell__ must be a nonlocal cell, not {}", tname(&cell));
            }
            funcobject::cell_set(&cell, Some(typ.clone()));
            dict::del_item(&dict, name!("__classcell__"))?;
        }
        if let Some(cd) = dict::get_item_str(&dict, name!("__classdictcell__")) {
            if cd.is_type(funcobject::cell_type()) {
                funcobject::cell_set(&cd, Some(dict.clone()));
            }
            dict::del_item(&dict, name!("__classdictcell__"))?;
        }
        // __set_name__
        for (k, v) in dict::items_vec(&dict) {
            if let Some(sn) = lookup(v.type_ptr(), name!("__set_name__")) {
                if let Err(e) = ab::call_unbound(&sn, &v, &[&typ, &k]) {
                    crate::modules::traceback_util::add_note_to_current(&format!(
                        "Error calling __set_name__ on '{}' instance '{}' in '{}'",
                        tname(&v),
                        pystr::to_string_lossy(&k),
                        name_s
                    ));
                    return Err(e);
                }
            }
        }
        // __init_subclass__ on the parent
        let sup = crate::types::super_::new_super(type_obj(tp), &typ)?;
        let isc = ab::getattr(&sup, name!("__init_subclass__"))?;
        ab::call_with_kwargs_dict(&isc, &[], kw)?;
        Ok(typ)
    }
}

fn subtype_get_dict(o: &PyObject) -> PyResult {
    match instance_dict(o) {
        Some(d) => Ok(d),
        None => raise!(AttributeError, "This object has no __dict__"),
    }
}
fn subtype_set_dict(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    unsafe {
        let slot = dict_slot(o.as_ptr());
        if slot.is_null() {
            raise!(AttributeError, "This object has no __dict__");
        }
        match v {
            Some(v) if !dict::is_dict(v) => raise!(TypeError, "__dict__ must be set to a dictionary, not a '{}'", tname(v)),
            _ => {}
        }
        let old = *slot;
        *slot = v.map_or(std::ptr::null_mut(), |v| v.to_ref().into_raw());
        xdecref(old);
    }
    Ok(())
}
fn subtype_get_weakref(o: &PyObject) -> PyResult {
    match crate::types::weakref::first_weakref(o) {
        Some(w) => Ok(w),
        None => Ok(none::none_ref()),
    }
}
fn subtype_getset_dict() -> *mut PyGetSetDef {
    static P: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *P.get_or_init(|| crate::getset_table![("__dict__", subtype_get_dict, subtype_set_dict; "dictionary for instance variables")] as usize)
        as *mut PyGetSetDef
}
fn subtype_getset_weakref() -> *mut PyGetSetDef {
    static P: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *P.get_or_init(|| crate::getset_table![("__weakref__", subtype_get_weakref; "list of weak references to the object")] as usize)
        as *mut PyGetSetDef
}

// ================================================================ setup

static mut TYPE_AS_NUMBER: PyNumberMethods = PyNumberMethods::ZERO;

pub fn init_types() {
    unsafe {
        let o = &mut *object_type();
        o.tp_name = c"object".as_ptr();
        o.tp_basicsize = std::mem::size_of::<PyObject>() as isize;
        o.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE;
        o.tp_doc = c"object()\n--\n\nThe base class of the class hierarchy.\n\nWhen called, it accepts no arguments and returns a new featureless\ninstance that has no instance attributes and cannot be given any.\n".as_ptr();
        o.tp_dealloc = Some(crate::alloc::free_object_c);
        o.tp_repr = Some(object_repr);
        o.tp_str = Some(object_str);
        o.tp_hash = Some(object_hash);
        o.tp_richcompare = Some(object_richcompare);
        o.tp_getattro = Some(object_generic_getattro);
        o.tp_setattro = Some(object_generic_setattro);
        o.tp_init = Some(object_init);
        o.tp_new = Some(object_new);
        o.tp_alloc = Some(generic_alloc);
        o.tp_free = Some(generic_free);
        o.tp_getset = crate::getset_table![("__class__", object_get_class, object_set_class; "the object's class")];
        o.tp_methods = crate::mdefs![
            ("__reduce_ex__", O, object_reduce_ex, "Helper for pickle."),
            ("__reduce__", NOARGS, object_reduce, "Helper for pickle."),
            ("__getstate__", NOARGS, object_getstate, "Helper for pickle."),
            ("__subclasshook__", FAST, object_subclasshook, "Abstract classes can override this to customize issubclass().\n\nThis is invoked early on by abc.ABCMeta.__subclasscheck__().\nIt should return True, False or NotImplemented.  If it returns\nNotImplemented, the normal algorithm is used.  Otherwise, it\noverrides the normal algorithm (and the outcome is cached).\n"; METH_CLASS),
            ("__init_subclass__", FASTKW, object_init_subclass, "This method is called when a class is subclassed.\n\nThe default implementation does nothing. It may be\noverridden to extend subclasses.\n"; METH_CLASS),
            ("__format__", O, object_format, "Default object formatter.\n\nReturn str(self) if format_spec is empty. Raise TypeError otherwise."),
            ("__sizeof__", NOARGS, object_sizeof, "Size of object in memory, in bytes."),
            ("__dir__", NOARGS, object_dir, "Default dir() implementation."),
        ];

        let t = &mut *type_type();
        t.tp_name = c"type".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyHeapTypeObject>() as isize;
        t.tp_itemsize = std::mem::size_of::<PyMemberDef>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_TYPE_SUBCLASS | Py_TPFLAGS_HAVE_VECTORCALL;
        t.tp_doc = c"type(object) -> the object's type\ntype(name, bases, dict, **kwds) -> a new type".as_ptr();
        t.tp_dealloc = Some(type_dealloc);
        t.tp_traverse = Some(type_traverse);
        t.tp_clear = Some(type_clear);
        t.tp_is_gc = Some(type_is_gc);
        t.tp_repr = Some(type_repr);
        t.tp_call = Some(type_call);
        t.tp_getattro = Some(type_getattro);
        t.tp_setattro = Some(type_setattro);
        t.tp_init = Some(type_init);
        t.tp_new = Some(type_new_slot);
        t.tp_alloc = Some(generic_alloc);
        t.tp_free = Some(generic_free);
        t.tp_vectorcall = Some(type_vectorcall);
        t.tp_weaklistoffset = std::mem::offset_of!(PyTypeObject, tp_weaklist) as isize;
        t.tp_dictoffset = std::mem::offset_of!(PyTypeObject, tp_dict) as isize;
        let n = &mut *(&raw mut TYPE_AS_NUMBER);
        n.nb_or = Some(crate::methods::binary_shim(type_or));
        t.tp_as_number = n;
        let _ = type_ror;
        t.tp_getset = crate::getset_table![
            ("__name__", type_get_name, type_set_name),
            ("__qualname__", type_get_qualname, type_set_qualname),
            ("__bases__", type_get_bases, type_set_bases),
            ("__mro__", type_get_mro),
            ("__module__", type_get_module, type_set_module),
            ("__abstractmethods__", type_get_abstractmethods, type_set_abstractmethods),
            ("__dict__", type_get_dict),
            ("__doc__", type_get_doc, type_set_doc),
            ("__text_signature__", type_get_text_signature),
            ("__annotations__", type_get_annotations, type_set_annotations),
            ("__annotate__", type_get_annotate, type_set_annotate),
            ("__type_params__", type_get_type_params, type_set_type_params),
            ("__base__", type_get_base),
            ("__basicsize__", type_basicsize),
            ("__itemsize__", type_itemsize),
            ("__flags__", type_flags),
            ("__weakrefoffset__", type_weakrefoffset),
            ("__dictoffset__", type_dictoffset),
        ];
        t.tp_methods = crate::mdefs![
            ("mro", NOARGS, type_mro_method, "Return a type's method resolution order."),
            ("__subclasses__", NOARGS, type_subclasses_method, "Return a list of immediate subclasses."),
            ("__prepare__", FASTKW, type_prepare, "__prepare__($cls, name, bases, /, **kwds)\n--\n\nCreate the namespace for the class statement"; METH_CLASS),
            ("__instancecheck__", O, type_instancecheck, "Check if an object is an instance."),
            ("__subclasscheck__", O, type_subclasscheck, "Check if a class is a subclass."),
            ("__dir__", NOARGS, type_dir, "Specialized __dir__ implementation for types."),
            ("__sizeof__", NOARGS, type_sizeof, "Return memory consumption of the type object."),
        ];
    }
}

/// A private unique object (used as a "missing" sentinel).
pub fn sentinel() -> PyRef {
    static S: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let p = *S.get_or_init(|| unsafe {
        let op = generic_alloc(object_type(), 0);
        make_immortal(op);
        op as usize
    });
    unsafe { PyRef::from_raw(p as *mut PyObject) }
}
