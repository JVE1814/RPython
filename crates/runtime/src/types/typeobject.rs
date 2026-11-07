//! `type` and `object`: readiness, MRO, attribute lookup, slot inheritance,
//! generic attribute access, instance allocation.

use crate::alloc;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::{dict, str as pystr, tuple};
use crate::{name, raise};
use std::ffi::{CStr, c_int, c_void};

#[unsafe(no_mangle)]
pub static mut PyType_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyBaseObject_Type: PyTypeObject = PyTypeObject::ZERO;

#[inline(always)]
pub fn type_type() -> *mut PyTypeObject {
    &raw mut PyType_Type
}
#[inline(always)]
pub fn object_type() -> *mut PyTypeObject {
    &raw mut PyBaseObject_Type
}
#[inline(always)]
pub fn is_type(o: &PyObject) -> bool {
    o.ty().tp_flags & Py_TPFLAGS_TYPE_SUBCLASS != 0
}
#[inline(always)]
pub fn as_type(o: &PyObject) -> *mut PyTypeObject {
    o.as_ptr() as *mut PyTypeObject
}
#[inline(always)]
pub fn type_obj(tp: *mut PyTypeObject) -> &'static PyObject {
    unsafe { &*(tp as *mut PyObject) }
}
#[inline(always)]
pub fn type_ref(tp: *mut PyTypeObject) -> PyRef {
    type_obj(tp).to_ref()
}
#[inline(always)]
pub fn is_heap_type(tp: *const PyTypeObject) -> bool {
    unsafe { (*tp).tp_flags & Py_TPFLAGS_HEAPTYPE != 0 }
}
#[inline(always)]
pub fn heap(tp: *mut PyTypeObject) -> *mut PyHeapTypeObject {
    tp as *mut PyHeapTypeObject
}

/// The short name (`__name__`).
pub fn type_name(tp: &PyTypeObject) -> String {
    unsafe {
        let tpp = tp as *const PyTypeObject as *mut PyTypeObject;
        if is_heap_type(tpp) && !(*heap(tpp)).ht_name.is_null() {
            return pystr::to_string_lossy(&*(*heap(tpp)).ht_name);
        }
        if tp.tp_name.is_null() {
            return "?".into();
        }
        let full = CStr::from_ptr(tp.tp_name).to_string_lossy();
        match full.rfind('.') {
            Some(i) => full[i + 1..].to_string(),
            None => full.into_owned(),
        }
    }
}

/// `__qualname__`.
pub fn type_qualname(tp: &PyTypeObject) -> String {
    unsafe {
        let tpp = tp as *const PyTypeObject as *mut PyTypeObject;
        if is_heap_type(tpp) && !(*heap(tpp)).ht_qualname.is_null() {
            return pystr::to_string_lossy(&*(*heap(tpp)).ht_qualname);
        }
    }
    type_name(tp)
}

/// The type's tp_name as error messages show it: the full dotted name of a
/// static type ('types.SimpleNamespace'), the name of a class statement's type,
/// the full spec name of a PyType_FromSpec type ('pandas._libs.interval.Interval').
pub fn tp_name_of(o: &PyObject) -> String {
    unsafe {
        let tp = o.type_ptr();
        if (*tp).tp_name.is_null() {
            return type_name(&*tp);
        }
        CStr::from_ptr((*tp).tp_name).to_string_lossy().into_owned()
    }
}

pub fn type_name_of(o: &PyObject) -> String {
    type_name(o.ty())
}

/// `__module__` of a type as Rust string (best effort).
/// True if `__module__` in a heap type's dict is a member/getset descriptor for its
/// instances (C types whose instances carry their own module, e.g. Cython's function
/// type made with PyType_FromSpec); the type's own module then comes from its dotted name.
pub fn module_is_instance_descr(m: &PyObject) -> bool {
    let t = m.type_ptr();
    t == crate::types::descr::member_descr_type() || t == crate::types::descr::getset_descr_type()
}

pub fn type_module(tp: *mut PyTypeObject) -> Option<String> {
    unsafe {
        if is_heap_type(tp) {
            let d = (*tp).tp_dict;
            let mut from_name = false;
            if !d.is_null() {
                if let Some(m) = dict::get_item_str(&*d, name!("__module__")) {
                    if pystr::is_str(&m) {
                        return Some(pystr::to_string_lossy(&m));
                    }
                    from_name = module_is_instance_descr(&m);
                }
            }
            if !from_name {
                return None;
            }
        }
        let full = CStr::from_ptr((*tp).tp_name).to_string_lossy();
        match full.rfind('.') {
            Some(i) => Some(full[..i].to_string()),
            None => Some("builtins".to_string()),
        }
    }
}

/// Fully qualified name for error messages: "module.qualname" unless builtins.
pub fn type_fqname(tp: *mut PyTypeObject) -> String {
    let q = type_qualname(unsafe { &*tp });
    match type_module(tp) {
        Some(m) if m != "builtins" && m != "__main__" => format!("{}.{}", m, q),
        _ => q,
    }
}

pub fn is_subtype(a: *mut PyTypeObject, b: *mut PyTypeObject) -> bool {
    if a == b {
        return true;
    }
    unsafe {
        let mro = (*a).tp_mro;
        if !mro.is_null() {
            for t in tuple::tuple_items(&*mro) {
                if t.as_ptr() as *mut PyTypeObject == b {
                    return true;
                }
            }
            return false;
        }
        let mut t = (*a).tp_base;
        while !t.is_null() {
            if t == b {
                return true;
            }
            t = (*t).tp_base;
        }
        b == object_type()
    }
}

#[inline]
pub fn isinstance_exact_or_sub(o: &PyObject, tp: *mut PyTypeObject) -> bool {
    o.is_type(tp) || is_subtype(o.type_ptr(), tp)
}

/// Look up `name` along the MRO (no descriptors invoked). `name` must be a str.
/// Results are memoised in the global method cache keyed on the type version tag
/// (jit workstream, interp/cache.rs).
pub fn lookup(tp: *mut PyTypeObject, name: &PyObject) -> Option<PyRef> {
    if let Some(r) = crate::interp::cache::mc_lookup(tp, name) {
        return r;
    }
    let tag = crate::interp::cache::assign_type_tag(tp);
    let r = lookup_uncached(tp, name);
    crate::interp::cache::mc_store(tag, name, r.as_deref());
    r
}

fn lookup_uncached(tp: *mut PyTypeObject, name: &PyObject) -> Option<PyRef> {
    unsafe {
        let mro = (*tp).tp_mro;
        if mro.is_null() {
            // not ready yet: walk base chain
            let mut t = tp;
            while !t.is_null() {
                let d = (*t).tp_dict;
                if !d.is_null() {
                    if let Some(v) = dict::get_item_str(&*d, name) {
                        return Some(v);
                    }
                }
                t = (*t).tp_base;
            }
            return None;
        }
        let h = pystr::str_hash(name);
        for base in tuple::tuple_items(&*mro) {
            let d = (*(base.as_ptr() as *mut PyTypeObject)).tp_dict;
            if d.is_null() {
                continue;
            }
            match dict::get_item_hash(&*d, name, h) {
                Ok(Some(v)) => return Some(v),
                Ok(None) => {}
                Err(_) => clear_exc(),
            }
        }
        None
    }
}

pub fn lookup_str(tp: *mut PyTypeObject, name: &str) -> Option<PyRef> {
    let n = pystr::intern_str(name);
    lookup(tp, &n)
}

/// Like lookup, but also returns the class in whose dict it was found.
pub fn lookup_with_owner(tp: *mut PyTypeObject, name: &PyObject) -> Option<(PyRef, *mut PyTypeObject)> {
    unsafe {
        let mro = (*tp).tp_mro;
        if mro.is_null() {
            return lookup(tp, name).map(|v| (v, tp));
        }
        for base in tuple::tuple_items(&*mro) {
            let bt = base.as_ptr() as *mut PyTypeObject;
            let d = (*bt).tp_dict;
            if d.is_null() {
                continue;
            }
            if let Some(v) = dict::get_item_str(&*d, name) {
                return Some((v, bt));
            }
        }
        None
    }
}

// ---------------------------------------------------------------- instances

/// Address of the instance `__dict__` pointer slot, if the object has one.
/// Objects keeping their attributes inline (`shape.rs`) get a real dict first, so
/// `*slot` is always NULL or a dict.
pub unsafe fn dict_slot(o: *mut PyObject) -> *mut *mut PyObject {
    unsafe {
        let tp = (*o).type_ptr();
        if (*tp).tp_flags & Py_TPFLAGS_MANAGED_DICT != 0 && alloc::has_managed(o) {
            crate::shape::materialize(&*o);
            return alloc::managed_dict_slot(o);
        }
        dict_slot_raw(o)
    }
}

/// The dict slot without materialising inline values: for managed dicts `*slot` may
/// hold a tagged values pointer (bit 0 set). For dealloc/traverse/clear only.
pub unsafe fn dict_slot_raw(o: *mut PyObject) -> *mut *mut PyObject {
    unsafe {
        let tp = (*o).type_ptr();
        if (*tp).tp_flags & Py_TPFLAGS_MANAGED_DICT != 0 {
            // (tp_dictoffset is then the sentinel -1, not an offset)
            return if alloc::has_managed(o) { alloc::managed_dict_slot(o) } else { std::ptr::null_mut() };
        }
        let off = (*tp).tp_dictoffset;
        if off == 0 {
            return std::ptr::null_mut();
        }
        if off > 0 {
            return (o as *mut u8).offset(off) as *mut *mut PyObject;
        }
        // negative: relative to end of variable-size object
        let n = var_size(&*o).unsigned_abs();
        let size = (*tp).tp_basicsize as usize + n * (*tp).tp_itemsize as usize;
        let size = (size + 7) & !7;
        (o as *mut u8).offset(size as isize + off) as *mut *mut PyObject
    }
}

/// Get (creating if needed) the instance dict.
pub fn instance_dict(o: &PyObject) -> Option<PyRef> {
    unsafe {
        let slot = dict_slot(o.as_ptr());
        if slot.is_null() {
            return None;
        }
        // create it with a CAS: threads setting the first attribute concurrently must
        // all end up in the same dict (a plain store lost the other threads' items)
        let a = std::sync::atomic::AtomicPtr::from_ptr(slot);
        if a.load(std::sync::atomic::Ordering::Acquire).is_null() {
            let d = dict::new_dict().into_raw();
            if a.compare_exchange(std::ptr::null_mut(), d, std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire).is_err() {
                decref(d);
            }
        }
        Some(PyRef::from_borrowed(a.load(std::sync::atomic::Ordering::Acquire)))
    }
}

/// Existing instance dict without creating one. Inline values count as a dict when
/// they hold attributes (they are materialised); an empty values area does not.
pub fn instance_dict_existing(o: &PyObject) -> Option<PyRef> {
    unsafe {
        if !crate::shape::values_of(o.as_ptr()).is_null() && crate::shape::inline_len(o) == 0 && crate::shape::has_inline_values(o) {
            return None;
        }
        let slot = dict_slot(o.as_ptr());
        if slot.is_null() || (*slot).is_null() {
            return None;
        }
        Some(PyRef::from_borrowed(*slot))
    }
}

pub fn attribute_error_for(o: &PyObject, name: &PyObject) -> PyErr {
    let msg = if is_type(o) {
        format!(
            "type object '{}' has no attribute '{}'",
            unsafe { CStr::from_ptr((*as_type(o)).tp_name).to_string_lossy() },
            pystr::to_string_lossy(name)
        )
    } else {
        format!("'{}' object has no attribute '{}'", type_name(o.ty()), pystr::to_string_lossy(name))
    };
    crate::types::exceptions::attribute_error_with(&msg, o, name)
}

/// PyObject_GenericGetAttr with optional suppression of AttributeError.
pub fn generic_getattr_opt(o: &PyObject, name: &PyObject, dict_override: Option<&PyObject>) -> PyResult<Option<PyRef>> {
    let tp = o.type_ptr();
    if !pystr::is_str(name) {
        raise!(TypeError, "attribute name must be string, not '{}'", type_name_of(name));
    }
    let descr = lookup(tp, name);
    let mut getf: descrgetfunc = None;
    if let Some(d) = &descr {
        getf = d.ty().tp_descr_get;
        if getf.is_some() && d.ty().tp_descr_set.is_some() {
            let r = unsafe { getf.unwrap()(d.as_ptr(), o.as_ptr(), tp as *mut PyObject) };
            return match unsafe { from_c(r) } {
                Ok(v) => Ok(Some(v)),
                Err(e) => Err(e),
            };
        }
    }
    let idict = match dict_override {
        Some(d) => Some(d.to_ref()),
        None => match crate::shape::get(o, name) {
            crate::shape::Get::Found(v) => return Ok(Some(v)),
            crate::shape::Get::Missing => None,
            crate::shape::Get::NotValues => instance_dict_existing(o),
        },
    };
    if let Some(d) = idict {
        if let Some(v) = dict::get_item(&d, name)? {
            return Ok(Some(v));
        }
    }
    if let Some(d) = descr {
        if let Some(f) = getf {
            let r = unsafe { f(d.as_ptr(), o.as_ptr(), tp as *mut PyObject) };
            return unsafe { from_c(r) }.map(Some);
        }
        return Ok(Some(d));
    }
    Ok(None)
}

pub fn generic_getattr(o: &PyObject, name: &PyObject) -> PyResult {
    match generic_getattr_opt(o, name, None)? {
        Some(v) => Ok(v),
        None => Err(attribute_error_for(o, name)),
    }
}

pub fn generic_setattr(o: &PyObject, name: &PyObject, value: Option<&PyObject>) -> PyResult<()> {
    generic_setattr_with_dict(o, name, value, None)
}

pub fn generic_setattr_with_dict(
    o: &PyObject,
    name: &PyObject,
    value: Option<&PyObject>,
    dict_override: Option<&PyObject>,
) -> PyResult<()> {
    let tp = o.type_ptr();
    if !pystr::is_str(name) {
        raise!(TypeError, "attribute name must be string, not '{}'", type_name_of(name));
    }
    if let Some(d) = lookup(tp, name) {
        if let Some(f) = d.ty().tp_descr_set {
            let r = unsafe { f(d.as_ptr(), o.as_ptr(), value.map_or(std::ptr::null_mut(), |v| v.as_ptr())) };
            return if r < 0 { Err(PyErr) } else { Ok(()) };
        }
    }
    let idict = match dict_override {
        Some(d) => Some(d.to_ref()),
        None => match crate::shape::set(o, name, value) {
            crate::shape::Set::Done => return Ok(()),
            crate::shape::Set::Missing => return Err(attribute_error_for(o, name)),
            crate::shape::Set::NotValues => instance_dict(o),
        },
    };
    match idict {
        Some(d) => match value {
            Some(v) => dict::set_item(&d, name, v),
            None => {
                if !dict::del_item(&d, name)? {
                    return Err(attribute_error_for(o, name));
                }
                Ok(())
            }
        },
        None => {
            let has_descr = lookup(tp, name).is_some();
            if has_descr {
                raise!(
                    AttributeError,
                    "'{}' object attribute '{}' is read-only",
                    type_name(o.ty()),
                    pystr::to_string_lossy(name)
                );
            }
            if value.is_none() {
                return Err(attribute_error_for(o, name));
            }
            Err(crate::types::exceptions::attribute_error_with(
                &format!(
                    "'{}' object has no attribute '{}' and no __dict__ for setting new attributes",
                    type_name(o.ty()),
                    pystr::to_string_lossy(name)
                ),
                o,
                name,
            ))
        }
    }
}

/// object's tp_getattro, exported as PyObject_GenericGetAttr: extensions compare a type's
/// tp_getattro with that symbol to detect standard attribute lookup (msgspec, Cython).
#[unsafe(export_name = "PyObject_GenericGetAttr")]
pub unsafe extern "C" fn object_generic_getattro(o: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    unsafe {
        if !pystr::is_str(&*name) {
            return into_c(Err(crate::err!(TypeError, "attribute name must be string, not '{}'", tname(&*name))));
        }
        into_c(generic_getattr(&*o, &*name))
    }
}

/// object's tp_setattro, exported as PyObject_GenericSetAttr (see above).
#[unsafe(export_name = "PyObject_GenericSetAttr")]
pub unsafe extern "C" fn object_generic_setattro(o: *mut PyObject, name: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        if !pystr::is_str(&*name) {
            return int_into_c(Err(crate::err!(TypeError, "attribute name must be string, not '{}'", tname(&*name))));
        }
        int_into_c(generic_setattr(&*o, &*name, if v.is_null() { None } else { Some(&*v) }))
    }
}

// ---------------------------------------------------------------- allocation

/// PyType_GenericAlloc: zeroed instance with `nitems` variable items, GC-tracked if GC.
pub unsafe extern "C" fn generic_alloc(tp: *mut PyTypeObject, nitems: Py_ssize_t) -> *mut PyObject {
    unsafe {
        let size = (*tp).tp_basicsize as usize + ((nitems + 1) as usize) * (*tp).tp_itemsize as usize;
        let size = (size + 7) & !7;
        // (core6) instances of Python classes honour a failure-injecting allocator hook
        // (`_testcapi.set_nomemory`); exceptions are exempt so MemoryError stays raisable
        if (*tp).tp_flags & Py_TPFLAGS_HEAPTYPE != 0
            && (*tp).tp_flags & Py_TPFLAGS_BASE_EXC_SUBCLASS == 0
            && crate::capi::mem::hooked_alloc_refuses(size)
        {
            let _ = memory_error();
            return std::ptr::null_mut();
        }
        let (extra, cap) = crate::shape::alloc_extra(tp);
        let op = alloc::alloc_object(tp, size + extra);
        if op.is_null() {
            let _ = memory_error();
            return op;
        }
        if extra != 0 {
            crate::shape::init_values(op, tp, size, cap);
        }
        if (*tp).tp_itemsize != 0 {
            set_var_size(op, nitems);
        }
        if (*tp).tp_flags & Py_TPFLAGS_TUPLE_SUBCLASS != 0 {
            // (jit6) tuples of every type cache their hash (tuple.rs `tuple_hash`)
            (*(op as *mut PyTupleObject)).ob_hash = -1;
        }
        if (*tp).tp_flags & Py_TPFLAGS_HAVE_GC != 0 {
            crate::gc::track(op);
        }
        op
    }
}

/// Free memory of an object whose contents are already released, and drop the
/// reference a heap type instance holds on its type.
pub unsafe fn generic_free_and_type_decref(op: *mut PyObject, tp: *mut PyTypeObject) {
    unsafe {
        match (*tp).tp_free {
            Some(f) => f(op as *mut c_void),
            None => alloc::free_object(op),
        }
        if is_heap_type(tp) {
            crate::object::decref_type(tp);
        }
    }
}

pub unsafe extern "C" fn generic_free(p: *mut c_void) {
    unsafe { alloc::free_object(p as *mut PyObject) }
}

/// Default dealloc for instances of heap types (subtype_dealloc).
pub unsafe extern "C" fn subtype_dealloc(op: *mut PyObject) {
    unsafe {
        let tp = (*op).type_ptr();
        // finalizer (__del__)
        if (*tp).tp_finalize.is_some() && !crate::gc::is_finalized(op) {
            let resurrected = crate::gc::call_finalizer_from_dealloc(op);
            if (*tp).tp_flags & Py_TPFLAGS_HAVE_GC == 0 {
                // only GC objects remember being finalized: a resurrected non-GC
                // object runs __del__ again when it dies again
                (*op).ob_gc_bits.fetch_and(!crate::alloc::GC_BIT_FINALIZED, std::sync::atomic::Ordering::Relaxed);
            }
            if resurrected {
                return;
            }
        }
        // __del__ may have assigned __class__: use the current type from here on
        let tp = (*op).type_ptr();
        crate::gc::untrack(op);
        // clear weakrefs
        crate::types::weakref::clear_weakrefs(op);
        // legacy finalizer (tp_del), after the weakrefs are gone; it may resurrect
        if let Some(del) = (*tp).tp_del {
            // called with a zero count (the slot resurrects temporarily itself)
            // and with the object tracked again, as a live object would be
            let o = &*op;
            o.ob_tid.store(crate::object::thread_id(), std::sync::atomic::Ordering::Relaxed);
            let gc_type = (*tp).tp_flags & Py_TPFLAGS_HAVE_GC != 0;
            if gc_type {
                crate::gc::track(op);
            }
            let saved = crate::errors::take_exc();
            del(op);
            if let Some(e) = saved {
                crate::errors::set_exc(e);
            }
            let local = o.ob_ref_local.load(std::sync::atomic::Ordering::Relaxed);
            if local != 0 || (o.ob_ref_shared.load(std::sync::atomic::Ordering::Relaxed) >> 2) != 0 {
                return; // resurrected
            }
            if gc_type {
                crate::gc::untrack(op);
            }
        }
        // clear __slots__ members
        let mut base = tp;
        while !base.is_null() && is_heap_type(base) {
            clear_slots(base, op);
            base = (*base).tp_base;
        }
        // clear inline values, then the managed / offset dict
        crate::shape::clear_values(op, false);
        let slot = dict_slot_raw(op);
        if !slot.is_null() && !(*slot).is_null() {
            let d = *slot;
            *slot = std::ptr::null_mut();
            decref(d);
        }
        // find the nearest static (non-heap) base to dealloc the remaining layout
        let mut b = tp;
        while is_heap_type(b) && (*b).tp_dealloc == Some(subtype_dealloc) {
            b = (*b).tp_base;
        }
        if b.is_null() || b == object_type() || (*b).tp_dealloc.is_none() {
            generic_free_and_type_decref(op, tp);
        } else {
            // The base dealloc frees the memory. Who drops the instance's reference to
            // its heap type: our own deallocs do it themselves
            // (generic_free_and_type_decref with the object's type); an extension's
            // heap-type dealloc does it too (Py_DECREF(Py_TYPE(self)), the C-API
            // contract for heap types); an extension's *static* type knows nothing of
            // heap types, so the reference is dropped here.
            let f = (*b).tp_dealloc.unwrap();
            f(op);
            if !is_heap_type(b) && !is_own_code(f as usize) {
                crate::object::decref_type(tp);
            }
        }
    }
}

/// Is `addr` inside this executable's code (as opposed to an extension module's)?
pub fn is_own_code(addr: usize) -> bool {
    static RANGE: std::sync::OnceLock<(usize, usize)> = std::sync::OnceLock::new();
    let &(lo, hi) = RANGE.get_or_init(|| {
        // the first object dl_iterate_phdr reports is the main program; take the
        // executable PT_LOAD segment that contains this very function
        unsafe extern "C" fn cb(info: *mut libc::dl_phdr_info, _size: usize, data: *mut c_void) -> c_int {
            unsafe {
                let out = &mut *(data as *mut (usize, usize));
                let me = is_own_code as usize;
                let info = &*info;
                let (mut lo, mut hi, mut mine) = (usize::MAX, 0usize, false);
                for i in 0..info.dlpi_phnum as usize {
                    let ph = &*info.dlpi_phdr.add(i);
                    if ph.p_type == libc::PT_LOAD && ph.p_flags & libc::PF_X != 0 {
                        let s = info.dlpi_addr as usize + ph.p_vaddr as usize;
                        let e = s + ph.p_memsz as usize;
                        mine |= (s..e).contains(&me);
                        lo = lo.min(s);
                        hi = hi.max(e);
                    }
                }
                if mine {
                    *out = (lo, hi);
                    return 1;
                }
                0
            }
        }
        let mut r = (0usize, 0usize);
        unsafe { libc::dl_iterate_phdr(Some(cb), &mut r as *mut _ as *mut c_void) };
        r
    });
    (lo..hi).contains(&addr)
}

unsafe fn clear_slots(tp: *mut PyTypeObject, op: *mut PyObject) {
    unsafe {
        let mut m = (*tp).tp_members;
        if m.is_null() {
            return;
        }
        while !(*m).name.is_null() {
            if (*m).type_code == Py_T_OBJECT_EX && (*m).flags & Py_READONLY == 0 {
                let p = crate::types::descr::member_addr(op, m) as *mut *mut PyObject;
                let old = *p;
                if !old.is_null() {
                    *p = std::ptr::null_mut();
                    decref(old);
                }
            }
            m = m.add(1);
        }
    }
}

/// tp_traverse for heap types: visit __dict__, __slots__ and the type.
pub unsafe extern "C" fn subtype_traverse(op: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let tp = (*op).type_ptr();
        let f = visit.unwrap();
        let mut base = tp;
        while !base.is_null() && is_heap_type(base) {
            let mut m = (*base).tp_members;
            if !m.is_null() {
                while !(*m).name.is_null() {
                    if (*m).type_code == Py_T_OBJECT_EX {
                        let p = *(crate::types::descr::member_addr(op, m) as *mut *mut PyObject);
                        if !p.is_null() {
                            let r = f(p, arg);
                            if r != 0 {
                                return r;
                            }
                        }
                    }
                    m = m.add(1);
                }
            }
            base = (*base).tp_base;
        }
        let r = crate::shape::traverse(op, visit, arg);
        if r != 0 {
            return r;
        }
        let slot = dict_slot_raw(op);
        if !slot.is_null() && !(*slot).is_null() && (*slot) as usize & 1 == 0 {
            let r = f(*slot, arg);
            if r != 0 {
                return r;
            }
        }
        if is_heap_type(tp) {
            let r = f(tp as *mut PyObject, arg);
            if r != 0 {
                return r;
            }
        }
        // the static base's traverse
        let mut b = tp;
        while is_heap_type(b) && (*b).tp_traverse == Some(subtype_traverse) {
            b = (*b).tp_base;
        }
        if !b.is_null() {
            if let Some(t) = (*b).tp_traverse {
                return t(op, visit, arg);
            }
        }
        0
    }
}

pub unsafe extern "C" fn subtype_clear(op: *mut PyObject) -> c_int {
    unsafe {
        let tp = (*op).type_ptr();
        let mut base = tp;
        while !base.is_null() && is_heap_type(base) {
            clear_slots(base, op);
            base = (*base).tp_base;
        }
        crate::shape::clear_values(op, true);
        let mut released: *mut PyObject = std::ptr::null_mut();
        let slot = dict_slot_raw(op);
        if !slot.is_null() && !(*slot).is_null() && (*slot) as usize & 1 == 0 {
            dict::clear(&**slot);
            if (*op).ob_gc_bits.load(std::sync::atomic::Ordering::Relaxed) & crate::alloc::GC_BIT_DICT_DETACHED != 0 {
                // a dict assigned from Python can be part of the cycle itself
                // (`self.__dict__ = self`): drop the reference, not just the contents
                // (released last: it may be `op` itself)
                released = *slot;
                *slot = std::ptr::null_mut();
            }
        }
        let mut b = tp;
        while is_heap_type(b) && (*b).tp_clear == Some(subtype_clear) {
            b = (*b).tp_base;
        }
        let mut r = 0;
        if !b.is_null() {
            if let Some(c) = (*b).tp_clear {
                r = c(op);
            }
        }
        xdecref(released);
        r
    }
}

// ---------------------------------------------------------------- readiness

/// Mark a static type as immortal and prepare it (PyType_Ready).
thread_local! {
    /// Set while PyType_Ready runs for an extension: static types readied then share their
    /// base's number/async/sequence/mapping/buffer tables when they have none of their own.
    pub static SHARE_BASE_SUBSTRUCTS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn type_ready(tp: *mut PyTypeObject) -> PyResult<()> {
    unsafe {
        if (*tp).tp_flags & Py_TPFLAGS_READY != 0 {
            return Ok(());
        }
        (*tp).tp_flags |= Py_TPFLAGS_READYING;
        let o = tp as *mut PyObject;
        if (*o).ob_type.load(std::sync::atomic::Ordering::Relaxed).is_null() {
            // a static type without an explicit metatype gets its base's metatype (numpy's
            // DTypes set only tp_base = &PyArrayDescr_Type and expect _DTypeMeta)
            let b = (*tp).tp_base;
            let meta = if b.is_null() { std::ptr::null_mut() } else { (*(b as *mut PyObject)).ob_type.load(std::sync::atomic::Ordering::Relaxed) };
            (*o).ob_type.store(if meta.is_null() { type_type() } else { meta }, std::sync::atomic::Ordering::Relaxed);
        }
        if !is_heap_type(tp) {
            make_immortal(o);
        }
        let base = if (*tp).tp_base.is_null() && tp != object_type() {
            (*tp).tp_base = object_type();
            object_type()
        } else {
            (*tp).tp_base
        };
        if !base.is_null() && (*base).tp_flags & Py_TPFLAGS_READY == 0 {
            type_ready(base)?;
        }
        if (*tp).tp_bases.is_null() {
            let b = if base.is_null() { tuple::empty_tuple() } else { tuple::new_tuple(vec![type_ref(base)]) };
            (*tp).tp_bases = b.into_raw();
        }
        if (*tp).tp_dict.is_null() {
            (*tp).tp_dict = dict::new_dict().into_raw();
        }
        // descriptors for methods/members/getsets
        add_methods(tp)?;
        add_members(tp)?;
        add_getsets(tp)?;
        // operator wrappers for the slots this type defines itself (before inheritance,
        // so inherited slots never shadow a base's dunder)
        crate::types::slots::add_operators(tp)?;
        // MRO
        mro_internal(tp)?;
        // static types are immutable (no attribute assignment from Python)
        if (*tp).tp_flags & Py_TPFLAGS_HEAPTYPE == 0 {
            (*tp).tp_flags |= Py_TPFLAGS_IMMUTABLETYPE;
        }
        // a static type deriving directly from object without its own tp_new cannot be
        // instantiated from Python (it does not inherit object.__new__)
        if (*tp).tp_flags & Py_TPFLAGS_HEAPTYPE == 0 && base == object_type() && (*tp).tp_new.is_none() && tp != object_type() {
            (*tp).tp_flags |= Py_TPFLAGS_DISALLOW_INSTANTIATION;
        }
        // inheritance
        if !base.is_null() {
            inherit_special(tp, base);
            if (*tp).tp_flags & Py_TPFLAGS_HEAPTYPE == 0 && SHARE_BASE_SUBSTRUCTS.with(|c| c.get()) {
                // an extension's static type without its own method tables uses its base's
                // tables (the same structs), so slots of other MRO entries are not added
                // (np.str_ keeps str's sq_repeat for `*` rather than numpy's generic multiply)
                let t = &mut *tp;
                let b = &*base;
                if t.tp_as_number.is_null() {
                    t.tp_as_number = b.tp_as_number;
                }
                if t.tp_as_async.is_null() {
                    t.tp_as_async = b.tp_as_async;
                }
                if t.tp_as_sequence.is_null() {
                    t.tp_as_sequence = b.tp_as_sequence;
                }
                if t.tp_as_mapping.is_null() {
                    t.tp_as_mapping = b.tp_as_mapping;
                }
                if t.tp_as_buffer.is_null() {
                    t.tp_as_buffer = b.tp_as_buffer;
                }
            }
        }
        let mro = (*tp).tp_mro;
        for b in tuple::tuple_items(&*mro).iter().skip(1) {
            inherit_slots(tp, as_type(b));
        }
        // `match` collection kind: a class without its own SEQUENCE/MAPPING flag takes it from the
        // nearest class in its MRO that has one (so a mixin listed first does not hide it)
        if (*tp).tp_flags & (Py_TPFLAGS_SEQUENCE | Py_TPFLAGS_MAPPING) == 0 {
            for b in tuple::tuple_items(&*mro).iter().skip(1) {
                let f = (*as_type(b)).tp_flags & (Py_TPFLAGS_SEQUENCE | Py_TPFLAGS_MAPPING);
                if f != 0 {
                    (*tp).tp_flags |= f;
                    break;
                }
            }
        }
        if (*tp).tp_flags & Py_TPFLAGS_DISALLOW_INSTANTIATION != 0 {
            (*tp).tp_new = None;
        }
        // __hash__ = None if __eq__ overridden without __hash__
        let d = &*(*tp).tp_dict;
        if dict::get_item_str(d, name!("__hash__")).is_none() {
            if (*tp).tp_hash.is_none() || (*tp).tp_hash == Some(crate::types::slots::hash_not_implemented) {
                dict::set_item(d, name!("__hash__"), crate::types::none::none())?;
                (*tp).tp_hash = Some(crate::types::slots::hash_not_implemented);
            }
        }
        if dict::get_item_str(d, name!("__doc__")).is_none() {
            if !(*tp).tp_doc.is_null() {
                let raw = CStr::from_ptr((*tp).tp_doc).to_string_lossy().into_owned();
                let doc = crate::types::descr::doc_without_signature(&raw);
                match doc {
                    Some(s) => dict::set_item(d, name!("__doc__"), &pystr::new_str(&s))?,
                    None => dict::set_item(d, name!("__doc__"), crate::types::none::none())?,
                }
            } else {
                dict::set_item(d, name!("__doc__"), crate::types::none::none())?;
            }
        }
        // register as subclass of bases
        for b in tuple::tuple_items(&*(*tp).tp_bases) {
            add_subclass(as_type(b), tp);
        }
        (*tp).tp_flags = ((*tp).tp_flags & !Py_TPFLAGS_READYING) | Py_TPFLAGS_READY;
        crate::interp::cache::watch_type_dict(tp); // jit: dict writes invalidate caches
        Ok(())
    }
}

/// Guards every type's `tp_subclasses` vector (no GIL: classes are created and
/// destroyed concurrently; bc-vm fix found by test_opcache's race tests).
static SUBCLASSES_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn add_subclass(base: *mut PyTypeObject, sub: *mut PyTypeObject) {
    let _g = SUBCLASSES_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        if (*base).tp_subclasses.is_null() {
            (*base).tp_subclasses = Box::into_raw(Box::new(Vec::<usize>::new())) as *mut c_void;
        }
        let v = &mut *((*base).tp_subclasses as *mut Vec<usize>);
        crate::object::enable_try_incref(sub as *mut PyObject); // see subclasses()
        if !v.contains(&(sub as usize)) {
            v.push(sub as usize);
        }
    }
}

pub fn remove_subclass(base: *mut PyTypeObject, sub: *mut PyTypeObject) {
    let _g = SUBCLASSES_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        if (*base).tp_subclasses.is_null() {
            return;
        }
        let v = &mut *((*base).tp_subclasses as *mut Vec<usize>);
        v.retain(|&x| x != sub as usize);
    }
}

/// A subclass reached through `tp_subclasses` (which holds no references), kept
/// alive while the caller works on it: another thread may drop the last reference
/// to it at any time (classes are created and freed concurrently).
pub struct SubType(PyRef);
impl SubType {
    #[inline]
    pub fn p(&self) -> *mut PyTypeObject {
        self.0.as_ptr() as *mut PyTypeObject
    }
    pub fn into_ref(self) -> PyRef {
        self.0
    }
}

/// The live direct subclasses of `tp` (dying ones are skipped).
pub fn subclasses(tp: *mut PyTypeObject) -> Vec<SubType> {
    let _g = SUBCLASSES_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        if (*tp).tp_subclasses.is_null() {
            return Vec::new();
        }
        let v = &*((*tp).tp_subclasses as *mut Vec<usize>);
        // a subclass is removed from the list in its dealloc, which needs this lock:
        // its memory is valid here, and try_incref refuses one that is dying
        v.iter()
            .filter(|&&x| crate::object::try_incref(x as *mut PyObject))
            .map(|&x| SubType(PyRef::from_raw(x as *mut PyObject)))
            .collect()
    }
}

unsafe fn add_methods(tp: *mut PyTypeObject) -> PyResult<()> {
    unsafe {
        let mut m = (*tp).tp_methods;
        if m.is_null() {
            return Ok(());
        }
        let d = &*(*tp).tp_dict;
        while !(*m).ml_name.is_null() {
            let nm = pystr::intern_str(CStr::from_ptr((*m).ml_name).to_str().unwrap());
            if (*m).ml_flags & METH_COEXIST == 0 && dict::get_item_str(d, &nm).is_some() {
                m = m.add(1);
                continue;
            }
            let descr = if (*m).ml_flags & METH_CLASS != 0 {
                crate::types::descr::new_classmethod_descr(tp, m)
            } else if (*m).ml_flags & METH_STATIC != 0 {
                let owner = if is_heap_type(tp) { None } else { Some(type_ref(tp)) };
                let f = crate::types::cfunction::new_cfunction(m, owner, None);
                crate::types::funcobject::new_staticmethod(f)
            } else {
                crate::types::descr::new_method_descr(tp, m)
            };
            dict::set_item(d, &nm, &descr)?;
            m = m.add(1);
        }
        Ok(())
    }
}

unsafe fn add_members(tp: *mut PyTypeObject) -> PyResult<()> {
    unsafe {
        let mut m = (*tp).tp_members;
        if m.is_null() {
            return Ok(());
        }
        let d = &*(*tp).tp_dict;
        while !(*m).name.is_null() {
            let nm = pystr::intern_str(CStr::from_ptr((*m).name).to_str().unwrap());
            if dict::get_item_str(d, &nm).is_none() {
                let descr = crate::types::descr::new_member_descr(tp, m);
                dict::set_item(d, &nm, &descr)?;
            }
            m = m.add(1);
        }
        Ok(())
    }
}

unsafe fn add_getsets(tp: *mut PyTypeObject) -> PyResult<()> {
    unsafe {
        let mut g = (*tp).tp_getset;
        if g.is_null() {
            return Ok(());
        }
        let d = &*(*tp).tp_dict;
        while !(*g).name.is_null() {
            let nm = pystr::intern_str(CStr::from_ptr((*g).name).to_str().unwrap());
            if dict::get_item_str(d, &nm).is_none() {
                let descr = crate::types::descr::new_getset_descr(tp, g);
                dict::set_item(d, &nm, &descr)?;
            }
            g = g.add(1);
        }
        Ok(())
    }
}

/// Copy layout-related fields from the base.
unsafe fn inherit_special(tp: *mut PyTypeObject, base: *mut PyTypeObject) {
    unsafe {
        let t = &mut *tp;
        let b = &*base;
        if t.tp_flags & Py_TPFLAGS_HAVE_GC == 0 && b.tp_flags & Py_TPFLAGS_HAVE_GC != 0 && t.tp_traverse.is_none() && t.tp_clear.is_none() {
            t.tp_flags |= Py_TPFLAGS_HAVE_GC;
            if t.tp_traverse.is_none() {
                t.tp_traverse = b.tp_traverse;
            }
            if t.tp_clear.is_none() {
                t.tp_clear = b.tp_clear;
            }
        }
        if t.tp_basicsize == 0 {
            t.tp_basicsize = b.tp_basicsize;
        }
        if t.tp_itemsize == 0 {
            t.tp_itemsize = b.tp_itemsize;
        }
        if t.tp_weaklistoffset == 0 {
            t.tp_weaklistoffset = b.tp_weaklistoffset;
        }
        if t.tp_dictoffset == 0 {
            t.tp_dictoffset = b.tp_dictoffset;
        }
        t.tp_flags |= b.tp_flags & Py_TPFLAGS_PREHEADER & if t.tp_flags & Py_TPFLAGS_HAVE_GC != 0 { !0 } else { 0 };
        // subclass bits
        let subs = Py_TPFLAGS_LONG_SUBCLASS
            | Py_TPFLAGS_LIST_SUBCLASS
            | Py_TPFLAGS_TUPLE_SUBCLASS
            | Py_TPFLAGS_BYTES_SUBCLASS
            | Py_TPFLAGS_UNICODE_SUBCLASS
            | Py_TPFLAGS_DICT_SUBCLASS
            | Py_TPFLAGS_BASE_EXC_SUBCLASS
            | Py_TPFLAGS_TYPE_SUBCLASS;
        t.tp_flags |= b.tp_flags & subs;
    }
}

macro_rules! copyslot {
    ($t:expr, $b:expr, $f:ident) => {
        if $t.$f.is_none() {
            $t.$f = $b.$f;
        }
    };
}

/// Inherit slot functions from a base in the MRO (inherit_slots).
unsafe fn inherit_slots(tp: *mut PyTypeObject, base: *mut PyTypeObject) {
    unsafe {
        let t = &mut *tp;
        let b = &*base;
        // a table shared with tp_base (see type_ready) belongs to the base: never fill it
        let tb = t.tp_base;
        let shared = |own: *mut c_void, base_own: Option<*mut c_void>| -> bool {
            !is_heap_type(tp) && !own.is_null() && base_own == Some(own)
        };
        let tbr = if tb.is_null() { None } else { Some(&*tb) };
        let skip_num = shared(t.tp_as_number as *mut c_void, tbr.map(|x| x.tp_as_number as *mut c_void));
        let skip_async = shared(t.tp_as_async as *mut c_void, tbr.map(|x| x.tp_as_async as *mut c_void));
        let skip_seq = shared(t.tp_as_sequence as *mut c_void, tbr.map(|x| x.tp_as_sequence as *mut c_void));
        let skip_map = shared(t.tp_as_mapping as *mut c_void, tbr.map(|x| x.tp_as_mapping as *mut c_void));
        let skip_buf = shared(t.tp_as_buffer as *mut c_void, tbr.map(|x| x.tp_as_buffer as *mut c_void));
        if !b.tp_as_number.is_null() && !skip_num {
            if t.tp_as_number.is_null() {
                t.tp_as_number = Box::into_raw(Box::new(PyNumberMethods::ZERO));
            }
            let tn = &mut *t.tp_as_number;
            let bn = &*b.tp_as_number;
            copyslot!(tn, bn, nb_add);
            copyslot!(tn, bn, nb_subtract);
            copyslot!(tn, bn, nb_multiply);
            copyslot!(tn, bn, nb_remainder);
            copyslot!(tn, bn, nb_divmod);
            copyslot!(tn, bn, nb_power);
            copyslot!(tn, bn, nb_negative);
            copyslot!(tn, bn, nb_positive);
            copyslot!(tn, bn, nb_absolute);
            copyslot!(tn, bn, nb_bool);
            copyslot!(tn, bn, nb_invert);
            copyslot!(tn, bn, nb_lshift);
            copyslot!(tn, bn, nb_rshift);
            copyslot!(tn, bn, nb_and);
            copyslot!(tn, bn, nb_xor);
            copyslot!(tn, bn, nb_or);
            copyslot!(tn, bn, nb_int);
            copyslot!(tn, bn, nb_float);
            copyslot!(tn, bn, nb_inplace_add);
            copyslot!(tn, bn, nb_inplace_subtract);
            copyslot!(tn, bn, nb_inplace_multiply);
            copyslot!(tn, bn, nb_inplace_remainder);
            copyslot!(tn, bn, nb_inplace_power);
            copyslot!(tn, bn, nb_inplace_lshift);
            copyslot!(tn, bn, nb_inplace_rshift);
            copyslot!(tn, bn, nb_inplace_and);
            copyslot!(tn, bn, nb_inplace_xor);
            copyslot!(tn, bn, nb_inplace_or);
            copyslot!(tn, bn, nb_floor_divide);
            copyslot!(tn, bn, nb_true_divide);
            copyslot!(tn, bn, nb_inplace_floor_divide);
            copyslot!(tn, bn, nb_inplace_true_divide);
            copyslot!(tn, bn, nb_index);
            copyslot!(tn, bn, nb_matrix_multiply);
            copyslot!(tn, bn, nb_inplace_matrix_multiply);
        }
        if !b.tp_as_async.is_null() && !skip_async {
            if t.tp_as_async.is_null() {
                t.tp_as_async = Box::into_raw(Box::new(PyAsyncMethods::ZERO));
            }
            let ta = &mut *t.tp_as_async;
            let ba = &*b.tp_as_async;
            copyslot!(ta, ba, am_await);
            copyslot!(ta, ba, am_aiter);
            copyslot!(ta, ba, am_anext);
            copyslot!(ta, ba, am_send);
        }
        if !b.tp_as_sequence.is_null() && !skip_seq {
            if t.tp_as_sequence.is_null() {
                t.tp_as_sequence = Box::into_raw(Box::new(PySequenceMethods::ZERO));
            }
            let ts = &mut *t.tp_as_sequence;
            let bs = &*b.tp_as_sequence;
            copyslot!(ts, bs, sq_length);
            copyslot!(ts, bs, sq_concat);
            copyslot!(ts, bs, sq_repeat);
            copyslot!(ts, bs, sq_item);
            copyslot!(ts, bs, sq_ass_item);
            copyslot!(ts, bs, sq_contains);
            copyslot!(ts, bs, sq_inplace_concat);
            copyslot!(ts, bs, sq_inplace_repeat);
        }
        if !b.tp_as_mapping.is_null() && !skip_map {
            if t.tp_as_mapping.is_null() {
                t.tp_as_mapping = Box::into_raw(Box::new(PyMappingMethods::ZERO));
            }
            let tm = &mut *t.tp_as_mapping;
            let bm = &*b.tp_as_mapping;
            copyslot!(tm, bm, mp_length);
            copyslot!(tm, bm, mp_subscript);
            copyslot!(tm, bm, mp_ass_subscript);
        }
        if !b.tp_as_buffer.is_null() && !skip_buf {
            if t.tp_as_buffer.is_null() {
                t.tp_as_buffer = Box::into_raw(Box::new(PyBufferProcs::ZERO));
            }
            let tb = &mut *t.tp_as_buffer;
            let bb = &*b.tp_as_buffer;
            copyslot!(tb, bb, bf_getbuffer);
            copyslot!(tb, bb, bf_releasebuffer);
        }
        copyslot!(t, b, tp_dealloc);
        if t.tp_getattr.is_none() && t.tp_getattro.is_none() {
            t.tp_getattr = b.tp_getattr;
            t.tp_getattro = b.tp_getattro;
        }
        if t.tp_setattr.is_none() && t.tp_setattro.is_none() {
            t.tp_setattr = b.tp_setattr;
            t.tp_setattro = b.tp_setattro;
        }
        copyslot!(t, b, tp_repr);
        copyslot!(t, b, tp_call);
        copyslot!(t, b, tp_str);
        // the offset is always inherited (PyVectorcall_Call reads it even without the flag);
        // the flag only goes with an inherited tp_call. A heap class defining `__call__` in its
        // body does not get it (its tp_call is recomputed from the dict after type_ready), and
        // assigning `__call__` later clears it (slots::update_slot).
        if t.tp_vectorcall_offset == 0 {
            t.tp_vectorcall_offset = b.tp_vectorcall_offset;
        }
        if t.tp_call == b.tp_call
            && b.tp_flags & Py_TPFLAGS_HAVE_VECTORCALL != 0
            && t.tp_vectorcall_offset == b.tp_vectorcall_offset
            && !(is_heap_type(tp) && !t.tp_dict.is_null() && dict::get_item_str(&*t.tp_dict, name!("__call__")).is_some())
        {
            t.tp_flags |= Py_TPFLAGS_HAVE_VECTORCALL;
        }
        if t.tp_richcompare.is_none() && t.tp_hash.is_none() {
            let d = &*t.tp_dict;
            let own_eq = dict::get_item_str(d, name!("__eq__")).is_some();
            let own_hash = dict::get_item_str(d, name!("__hash__")).is_some();
            if !own_eq && !own_hash {
                t.tp_richcompare = b.tp_richcompare;
                t.tp_hash = b.tp_hash;
            }
        }
        copyslot!(t, b, tp_iter);
        copyslot!(t, b, tp_iternext);
        if t.tp_descr_get.is_none()
            && b.tp_descr_get.is_some()
            && b.tp_flags & Py_TPFLAGS_METHOD_DESCRIPTOR != 0
            && t.tp_flags & Py_TPFLAGS_IMMUTABLETYPE != 0
        {
            // only immutable types: a mutable class could later get its own __get__
            t.tp_flags |= Py_TPFLAGS_METHOD_DESCRIPTOR;
        }
        copyslot!(t, b, tp_descr_get);
        if t.tp_descr_set.is_none() && t.tp_descr_get == b.tp_descr_get {
            t.tp_descr_set = b.tp_descr_set;
        }
        copyslot!(t, b, tp_init);
        copyslot!(t, b, tp_alloc);
        copyslot!(t, b, tp_is_gc);
        copyslot!(t, b, tp_finalize);
        if t.tp_flags & Py_TPFLAGS_HAVE_GC == b.tp_flags & Py_TPFLAGS_HAVE_GC {
            copyslot!(t, b, tp_free);
        } else if t.tp_flags & Py_TPFLAGS_HAVE_GC != 0 && t.tp_free.is_none() {
            t.tp_free = Some(generic_free);
        }
        if base == (*tp).tp_base {
            copyslot!(t, b, tp_new);
        }
        if t.tp_flags & Py_TPFLAGS_HAVE_GC != 0 {
            copyslot!(t, b, tp_traverse);
            copyslot!(t, b, tp_clear);
        }
    }
}

// ---------------------------------------------------------------- MRO

pub fn mro_internal(tp: *mut PyTypeObject) -> PyResult<()> {
    unsafe {
        let meta = (*(tp as *mut PyObject)).type_ptr();
        let mro = if meta != type_type() && is_heap_type(meta) {
            // custom mro() on metaclass?
            let custom = lookup(meta, name!("mro"));
            let default = lookup(type_type(), name!("mro"));
            let is_default = match (&custom, &default) {
                (Some(a), Some(b)) => a.is(b),
                _ => true,
            };
            if !is_default {
                let r = crate::abstract_::call_method(type_obj(tp), name!("mro"), &[])?;
                let t = crate::abstract_::sequence_tuple(&r)?;
                for it in tuple::tuple_items(&t) {
                    if !is_type(it) {
                        raise!(TypeError, "mro() returned a non-class ('{}')", type_name_of(it));
                    }
                    if !is_subtype(tp, as_type(it)) && as_type(it) != tp {
                        // CPython checks solid base compatibility
                        if !is_subtype(solid_base(tp), solid_base(as_type(it))) {
                            raise!(
                                TypeError,
                                "mro() returned base with unsuitable layout ('{}')",
                                type_name(&*as_type(it))
                            );
                        }
                    }
                }
                t
            } else {
                compute_c3_mro(tp)?
            }
        } else {
            compute_c3_mro(tp)?
        };
        let old = (*tp).tp_mro;
        (*tp).tp_mro = mro.into_raw();
        xdecref(old);
        crate::types::slots::type_modified(tp);
        Ok(())
    }
}

/// C3 linearisation of tp's bases.
pub fn compute_c3_mro(tp: *mut PyTypeObject) -> PyResult {
    unsafe {
        let bases = tuple::tuple_items(&*(*tp).tp_bases);
        if bases.len() == 1 {
            let b = as_type(bases[0]);
            let bm = (*b).tp_mro;
            let mut v = vec![type_ref(tp)];
            if !bm.is_null() {
                v.extend(tuple::tuple_to_vec(&*bm));
            } else {
                v.push(type_ref(b));
            }
            return Ok(tuple::new_tuple(v));
        }
        if bases.is_empty() {
            return Ok(tuple::new_tuple(vec![type_ref(tp)]));
        }
        // duplicate base check
        for (i, b) in bases.iter().enumerate() {
            for b2 in &bases[..i] {
                if b.is(b2) {
                    raise!(TypeError, "duplicate base class {}", type_name(&*as_type(b)));
                }
            }
        }
        let mut seqs: Vec<Vec<*mut PyTypeObject>> = Vec::new();
        for b in bases {
            let bm = (*as_type(b)).tp_mro;
            let s: Vec<*mut PyTypeObject> = if bm.is_null() {
                vec![as_type(b)]
            } else {
                tuple::tuple_items(&*bm).iter().map(|x| as_type(x)).collect()
            };
            seqs.push(s);
        }
        seqs.push(bases.iter().map(|x| as_type(x)).collect());
        let mut result: Vec<*mut PyTypeObject> = vec![tp];
        loop {
            seqs.retain(|s| !s.is_empty());
            if seqs.is_empty() {
                break;
            }
            let mut cand = None;
            for s in &seqs {
                let c = s[0];
                if !seqs.iter().any(|s2| s2[1..].contains(&c)) {
                    cand = Some(c);
                    break;
                }
            }
            match cand {
                Some(c) => {
                    result.push(c);
                    for s in seqs.iter_mut() {
                        if s[0] == c {
                            s.remove(0);
                        }
                    }
                }
                None => {
                    let mut names: Vec<String> = Vec::new();
                    for s in &seqs {
                        let n = type_name(&*s[0]);
                        if !names.contains(&n) {
                            names.push(n);
                        }
                    }
                    raise!(
                        TypeError,
                        "Cannot create a consistent method resolution order (MRO) for bases {}",
                        names.join(", ")
                    );
                }
            }
        }
        Ok(tuple::new_tuple(result.into_iter().map(type_ref).collect()))
    }
}

/// The most derived base that adds to the instance layout.
pub fn solid_base(tp: *mut PyTypeObject) -> *mut PyTypeObject {
    unsafe {
        let base = (*tp).tp_base;
        let sb = if base.is_null() { object_type() } else { solid_base(base) };
        if shape_differs(tp, sb) { tp } else { sb }
    }
}

unsafe fn shape_differs(t1: *mut PyTypeObject, t2: *mut PyTypeObject) -> bool {
    unsafe {
        if t1 == t2 {
            return false;
        }
        let extra = extra_ivars(t1, t2);
        extra
    }
}

unsafe fn extra_ivars(tp: *mut PyTypeObject, base: *mut PyTypeObject) -> bool {
    unsafe {
        let t_size = (*tp).tp_basicsize;
        let b_size = (*base).tp_basicsize;
        if (*tp).tp_itemsize != 0 || (*base).tp_itemsize != 0 {
            return t_size != b_size || (*tp).tp_itemsize != (*base).tp_itemsize;
        }
        // __dict__/__weakref__ via preheader do not count
        t_size != b_size
    }
}

// ---------------------------------------------------------------- misc helpers

pub fn new_static_type_name(tp: *mut PyTypeObject) -> String {
    type_name(unsafe { &*tp })
}

/// Return borrowed `tp.__dict__` as a reference.
pub fn type_dict(tp: *mut PyTypeObject) -> &'static PyObject {
    unsafe { &*(*tp).tp_dict }
}

/// Set an attribute directly in a (static) type's dict during init.
pub fn type_dict_set(tp: *mut PyTypeObject, name: &str, v: &PyObject) {
    let d = type_dict(tp);
    dict::set_item_str(d, name, v).expect("type_dict_set");
}

/// `type(o).__name__` convenience for error messages.
pub fn tname(o: &PyObject) -> String {
    type_name(o.ty())
}
