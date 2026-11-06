//! Weak references: `weakref.ReferenceType`, `ProxyType`, `CallableProxyType`
//! (layout = public `PyWeakReference`), the per-object weakref lists, and
//! clearing + callbacks when a referent dies.
//!
//! Each weakly referenceable object has a list head slot (at `tp_weaklistoffset`,
//! or the managed pre-header slot for heap types with `Py_TPFLAGS_MANAGED_WEAKREF`).
//! The list is doubly linked through `wr_prev/wr_next`; basic refs (no callback,
//! exact ref type) come first, then basic proxies, then everything else.
//! All list manipulation is serialised by a global lock; callbacks run unlocked.
use crate::abstract_ as ab;
use crate::abstract_::NbSlot;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::*;
use crate::types::{bool_, dict, exceptions, int, none, str as pystr, tuple};
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicU8, Ordering::*};

#[repr(C)]
pub struct PyWeakReference {
    pub ob_base: PyObject,
    pub wr_object: *mut PyObject,
    pub wr_callback: *mut PyObject,
    pub hash: Py_hash_t,
    pub wr_prev: *mut PyWeakReference,
    pub wr_next: *mut PyWeakReference,
    pub vectorcall: vectorcallfunc,
    pub weakrefs_lock: *mut c_void,
}

#[unsafe(no_mangle)]
pub static mut _PyWeakref_RefType: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut _PyWeakref_ProxyType: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut _PyWeakref_CallableProxyType: PyTypeObject = PyTypeObject::ZERO;

pub fn ref_type() -> *mut PyTypeObject {
    &raw mut _PyWeakref_RefType
}
pub fn proxy_type() -> *mut PyTypeObject {
    &raw mut _PyWeakref_ProxyType
}
pub fn callable_proxy_type() -> *mut PyTypeObject {
    &raw mut _PyWeakref_CallableProxyType
}
pub fn is_ref(o: &PyObject) -> bool {
    o.is_type(ref_type()) || is_subtype(o.type_ptr(), ref_type())
}
pub fn is_proxy(o: &PyObject) -> bool {
    o.is_type(proxy_type()) || o.is_type(callable_proxy_type())
}
pub fn is_weakref(o: &PyObject) -> bool {
    is_ref(o) || is_proxy(o)
}
fn wr(o: &PyObject) -> &mut PyWeakReference {
    unsafe { &mut *(o.as_ptr() as *mut PyWeakReference) }
}

// ------------------------------------------------------------------ global list lock

static LIST_LOCK: AtomicU8 = AtomicU8::new(0);
struct ListGuard;
impl Drop for ListGuard {
    fn drop(&mut self) {
        LIST_LOCK.store(0, Release);
    }
}
fn list_lock() -> ListGuard {
    let mut spins = 0u32;
    while LIST_LOCK.compare_exchange_weak(0, 1, Acquire, Relaxed).is_err() {
        spins += 1;
        if spins < 64 {
            std::hint::spin_loop();
        } else {
            std::thread::yield_now();
        }
    }
    ListGuard
}

// ------------------------------------------------------------------ list slots

/// Address of the weakref list head of `o`, or NULL if `o` is not weakly referenceable.
pub unsafe fn weaklist_slot(o: *mut PyObject) -> *mut *mut PyObject {
    unsafe {
        let tp = (*o).type_ptr();
        if (*tp).tp_flags & Py_TPFLAGS_MANAGED_WEAKREF != 0 && crate::alloc::has_managed(o) {
            return crate::alloc::managed_weakref_slot(o);
        }
        let off = (*tp).tp_weaklistoffset;
        if off > 0 {
            return (o as *mut u8).offset(off) as *mut *mut PyObject;
        }
        std::ptr::null_mut()
    }
}
pub fn supports_weakrefs(o: &PyObject) -> bool {
    unsafe { !weaklist_slot(o.as_ptr()).is_null() }
}

unsafe fn head(slot: *mut *mut PyObject) -> *mut PyWeakReference {
    unsafe { *slot as *mut PyWeakReference }
}

/// Unlink `r` from its referent's list (list lock held).
unsafe fn unlink(r: *mut PyWeakReference) {
    unsafe {
        let obj = (*r).wr_object;
        if obj.is_null() || obj == none::none().as_ptr() {
            return;
        }
        let slot = weaklist_slot(obj);
        if !slot.is_null() && head(slot) == r {
            *slot = (*r).wr_next as *mut PyObject;
        }
        if !(*r).wr_prev.is_null() {
            (*(*r).wr_prev).wr_next = (*r).wr_next;
        }
        if !(*r).wr_next.is_null() {
            (*(*r).wr_next).wr_prev = (*r).wr_prev;
        }
        (*r).wr_prev = std::ptr::null_mut();
        (*r).wr_next = std::ptr::null_mut();
    }
}
unsafe fn insert_after(r: *mut PyWeakReference, prev: *mut PyWeakReference) {
    unsafe {
        (*r).wr_prev = prev;
        (*r).wr_next = (*prev).wr_next;
        if !(*prev).wr_next.is_null() {
            (*(*prev).wr_next).wr_prev = r;
        }
        (*prev).wr_next = r;
    }
}
unsafe fn insert_head(r: *mut PyWeakReference, slot: *mut *mut PyObject) {
    unsafe {
        let next = head(slot);
        (*r).wr_prev = std::ptr::null_mut();
        (*r).wr_next = next;
        if !next.is_null() {
            (*next).wr_prev = r;
        }
        *slot = r as *mut PyObject;
    }
}
/// The basic ref and basic proxy at the head of the list (list lock held).
unsafe fn get_basic_refs(slot: *mut *mut PyObject) -> (*mut PyWeakReference, *mut PyWeakReference) {
    unsafe {
        let mut r = head(slot);
        let mut basic_ref = std::ptr::null_mut();
        let mut basic_proxy = std::ptr::null_mut();
        if !r.is_null() && (*r).wr_callback.is_null() && (*(r as *mut PyObject)).is_type(ref_type()) {
            basic_ref = r;
            r = (*r).wr_next;
        }
        if !r.is_null() && (*r).wr_callback.is_null() && is_proxy(&*(r as *mut PyObject)) {
            basic_proxy = r;
        }
        (basic_ref, basic_proxy)
    }
}

/// Try to take a new strong reference to a live referent.
fn referent(o: &PyObject) -> Option<PyRef> {
    let _g = list_lock();
    let p = wr(o).wr_object;
    if p.is_null() || p == none::none().as_ptr() {
        return None;
    }
    unsafe {
        if (*p).refcnt() <= 0 {
            return None;
        }
        Some(PyRef::from_borrowed(p))
    }
}
pub fn get_object(o: &PyObject) -> Option<PyRef> {
    referent(o)
}
pub fn is_dead(o: &PyObject) -> bool {
    referent(o).is_none()
}

/// First weakref of `o` (for the `__weakref__` attribute).
pub fn first_weakref(o: &PyObject) -> Option<PyRef> {
    let _g = list_lock();
    unsafe {
        let slot = weaklist_slot(o.as_ptr());
        if slot.is_null() || (*slot).is_null() {
            return None;
        }
        Some(PyRef::from_borrowed(*slot))
    }
}

pub fn weakref_count(o: &PyObject) -> usize {
    let _g = list_lock();
    unsafe {
        let slot = weaklist_slot(o.as_ptr());
        if slot.is_null() {
            return 0;
        }
        let mut n = 0;
        let mut r = head(slot);
        while !r.is_null() {
            n += 1;
            r = (*r).wr_next;
        }
        n
    }
}
pub fn weakrefs_of(o: &PyObject) -> Vec<PyRef> {
    let _g = list_lock();
    let mut v = Vec::new();
    unsafe {
        let slot = weaklist_slot(o.as_ptr());
        if slot.is_null() {
            return v;
        }
        let mut r = head(slot);
        while !r.is_null() {
            if (*(r as *mut PyObject)).refcnt() > 0 {
                v.push(PyRef::from_borrowed(r as *mut PyObject));
            }
            r = (*r).wr_next;
        }
    }
    v
}

// ------------------------------------------------------------------ creation

unsafe fn init_ref(r: *mut PyWeakReference, ob: *mut PyObject, cb: Option<&PyObject>) {
    unsafe {
        (*r).wr_object = ob;
        (*r).wr_callback = cb.map_or(std::ptr::null_mut(), |c| c.to_ref().into_raw());
        (*r).hash = -1;
        (*r).wr_prev = std::ptr::null_mut();
        (*r).wr_next = std::ptr::null_mut();
        (*r).vectorcall = None;
    }
}

fn check_referenceable(ob: &PyObject) -> PyResult<*mut *mut PyObject> {
    let slot = unsafe { weaklist_slot(ob.as_ptr()) };
    if slot.is_null() {
        raise!(TypeError, "cannot create weak reference to '{}' object", tname(ob));
    }
    Ok(slot)
}

/// weakref.ref(ob[, callback]) for type `tp` (ref or subclass).
pub fn new_ref(tp: *mut PyTypeObject, ob: &PyObject, callback: Option<&PyObject>) -> PyResult {
    let callback = callback.filter(|c| !none::is_none(c));
    let slot = check_referenceable(ob)?;
    if tp == ref_type() && callback.is_none() {
        let _g = list_lock();
        let (basic, _) = unsafe { get_basic_refs(slot) };
        if !basic.is_null() && unsafe { (*(basic as *mut PyObject)).refcnt() } > 0 {
            return Ok(unsafe { PyRef::from_borrowed(basic as *mut PyObject) });
        }
    }
    let op = unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
        let op = alloc(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        op
    };
    let r = op as *mut PyWeakReference;
    unsafe {
        init_ref(r, ob.as_ptr(), callback);
        let _g = list_lock();
        let (basic_ref, basic_proxy) = get_basic_refs(slot);
        if callback.is_none() && tp == ref_type() {
            insert_head(r, slot);
        } else {
            let prev = if basic_proxy.is_null() { basic_ref } else { basic_proxy };
            if prev.is_null() { insert_head(r, slot) } else { insert_after(r, prev) }
        }
        Ok(PyRef::from_raw(op))
    }
}

pub fn new_proxy(ob: &PyObject, callback: Option<&PyObject>) -> PyResult {
    let callback = callback.filter(|c| !none::is_none(c));
    let slot = check_referenceable(ob)?;
    if callback.is_none() {
        let _g = list_lock();
        let (_, basic) = unsafe { get_basic_refs(slot) };
        if !basic.is_null() && unsafe { (*(basic as *mut PyObject)).refcnt() } > 0 {
            return Ok(unsafe { PyRef::from_borrowed(basic as *mut PyObject) });
        }
    }
    let tp = if ab::is_callable(ob) { callable_proxy_type() } else { proxy_type() };
    unsafe {
        let op = crate::alloc::alloc_object(tp, std::mem::size_of::<PyWeakReference>());
        let r = op as *mut PyWeakReference;
        init_ref(r, ob.as_ptr(), callback);
        crate::gc::track(op);
        let _g = list_lock();
        let (basic_ref, basic_proxy) = get_basic_refs(slot);
        if callback.is_none() {
            if basic_ref.is_null() { insert_head(r, slot) } else { insert_after(r, basic_ref) }
        } else {
            let prev = if basic_proxy.is_null() { basic_ref } else { basic_proxy };
            if prev.is_null() { insert_head(r, slot) } else { insert_after(r, prev) }
        }
        Ok(PyRef::from_raw(op))
    }
}

// ------------------------------------------------------------------ clearing

/// Clear all weakrefs to `o` (called from deallocators) and run callbacks.
pub unsafe fn clear_weakrefs(o: *mut PyObject) {
    unsafe {
        let slot = weaklist_slot(o);
        if !slot.is_null() {
            clear_weakrefs_at(o, slot);
        }
    }
}

/// As clear_weakrefs, with an explicit list head slot.
pub unsafe fn clear_weakrefs_at(o: *mut PyObject, list: *mut *mut PyObject) {
    unsafe {
        if list.is_null() || (*list).is_null() {
            return;
        }
        let _ = o;
        // Detach every ref; collect those with callbacks that are still alive.
        let mut pending: Vec<(PyRef, PyRef)> = Vec::new();
        let mut dead_cbs: Vec<PyRef> = Vec::new();
        {
            let _g = list_lock();
            let mut r = head(list);
            *list = std::ptr::null_mut();
            while !r.is_null() {
                let next = (*r).wr_next;
                (*r).wr_object = none::none().as_ptr();
                (*r).wr_prev = std::ptr::null_mut();
                (*r).wr_next = std::ptr::null_mut();
                let cb = std::mem::replace(&mut (*r).wr_callback, std::ptr::null_mut());
                if !cb.is_null() {
                    let ro = r as *mut PyObject;
                    if (*ro).refcnt() > 0 {
                        pending.push((PyRef::from_borrowed(ro), PyRef::from_raw(cb)));
                    } else {
                        // the weakref itself is dying: just drop the callback
                        dead_cbs.push(PyRef::from_raw(cb));
                    }
                }
                r = next;
            }
        }
        drop(dead_cbs);
        run_callbacks(pending);
    }
}

/// Cycle-collector variant: clear the weakrefs to every object in `garbage`
/// first, then run the callbacks of those weakrefs that are not themselves
/// garbage (the callbacks of garbage weakrefs are dropped, as in CPython).
/// With `callbacks_only`, only weakrefs that have a callback are cleared (the GC
/// does this before running finalizers; plain weakrefs are cleared afterwards).
pub unsafe fn clear_weakrefs_for_gc(garbage: &[*mut PyObject], callbacks_only: bool) {
    unsafe { detach_weakrefs_for_gc(garbage, callbacks_only).finish() }
}

/// Weakrefs detached by the collector whose callbacks have not run yet.
#[derive(Default)]
pub struct DetachedWeakrefs {
    pending: Vec<(PyRef, PyRef)>,
    dropped: Vec<PyRef>,
}

impl DetachedWeakrefs {
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.dropped.is_empty()
    }
    /// Run the callbacks and release the dropped ones (may run Python code).
    pub unsafe fn finish(self) {
        drop(self.dropped);
        unsafe { run_callbacks(self.pending) };
    }
}

/// First half of `clear_weakrefs_for_gc`: detach the weakrefs without running
/// any Python code or releasing anything (usable while the world is stopped).
pub unsafe fn detach_weakrefs_for_gc(garbage: &[*mut PyObject], callbacks_only: bool) -> DetachedWeakrefs {
    unsafe {
        let set: std::collections::HashSet<usize> = garbage.iter().map(|&p| p as usize).collect();
        let mut pending: Vec<(PyRef, PyRef)> = Vec::new();
        let mut dropped: Vec<PyRef> = Vec::new();
        {
            let _g = list_lock();
            for &o in garbage {
                let list = weaklist_slot(o);
                if list.is_null() {
                    continue;
                }
                let mut r = head(list);
                while !r.is_null() {
                    let next = (*r).wr_next;
                    if callbacks_only && (*r).wr_callback.is_null() {
                        r = next;
                        continue;
                    }
                    unlink(r);
                    (*r).wr_object = none::none().as_ptr();
                    let cb = std::mem::replace(&mut (*r).wr_callback, std::ptr::null_mut());
                    if !cb.is_null() {
                        let ro = r as *mut PyObject;
                        if !set.contains(&(ro as usize)) && (*ro).refcnt() > 0 {
                            pending.push((PyRef::from_borrowed(ro), PyRef::from_raw(cb)));
                        } else {
                            dropped.push(PyRef::from_raw(cb));
                        }
                    }
                    r = next;
                }
            }
            // Weakrefs that are garbage themselves are cleared too: their referent
            // may be garbage the collector cannot see (reachable only through an
            // object without tp_traverse) and freed by tp_clear; its callback
            // must not run then.
            if !callbacks_only {
                for &o in garbage {
                    if !is_weakref(&*o) {
                        continue;
                    }
                    let r = o as *mut PyWeakReference;
                    if (*r).wr_object.is_null() || (*r).wr_object == none::none().as_ptr() {
                        continue;
                    }
                    unlink(r);
                    (*r).wr_object = none::none().as_ptr();
                    let cb = std::mem::replace(&mut (*r).wr_callback, std::ptr::null_mut());
                    if !cb.is_null() {
                        dropped.push(PyRef::from_raw(cb));
                    }
                }
            }
        }
        DetachedWeakrefs { pending, dropped }
    }
}

unsafe fn run_callbacks(pending: Vec<(PyRef, PyRef)>) {
    if pending.is_empty() {
        return;
    }
    let saved = take_exc();
    for (r, cb) in pending {
        if ab::call(&cb, &[&r]).is_err() {
            let msg = format!(
                "Exception ignored while calling weakref callback {}",
                ab::repr_string(&cb).unwrap_or_else(|_| {
                    clear_exc();
                    "<object>".into()
                })
            );
            crate::modules::sys::write_unraisable_with_obj(&msg, None);
        }
    }
    if let Some(e) = saved {
        set_exc(e);
    }
}

// ------------------------------------------------------------------ ref type slots

unsafe extern "C" fn ref_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            if tp == ref_type() && !kw.is_null() && dict::dict_len(&*kw) > 0 {
                raise!(TypeError, "ref() takes no keyword arguments");
            }
            let a = tuple::tuple_items(&*args);
            if a.is_empty() || a.len() > 2 {
                raise!(TypeError, "__new__ expected {} 2 arguments, got {}", if a.is_empty() { "at least 1" } else { "at most" }, a.len());
            }
            new_ref(tp, a[0], a.get(1).copied())
        })();
        into_c(r)
    }
}
unsafe extern "C" fn ref_init(_o: *mut PyObject, args: *mut PyObject, _kw: *mut PyObject) -> c_int {
    unsafe {
        let a = tuple::tuple_items(&*args);
        if a.is_empty() || a.len() > 2 {
            let _ = err!(TypeError, "__init__ expected at most 2 arguments, got {}", a.len());
            return -1;
        }
        0
    }
}
unsafe extern "C" fn ref_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let r = o as *mut PyWeakReference;
        {
            let _g = list_lock();
            unlink(r);
        }
        let cb = std::mem::replace(&mut (*r).wr_callback, std::ptr::null_mut());
        xdecref(cb);
        let tp = (*o).type_ptr();
        if is_heap_type(tp) {
            // subclass instance: clear its own dict/weakrefs via subtype path
            crate::shape::clear_values(o, false);
            let slot = crate::types::typeobject::dict_slot_raw(o);
            if !slot.is_null() && !(*slot).is_null() {
                let d = std::mem::replace(&mut *slot, std::ptr::null_mut());
                decref(d);
            }
        }
        generic_free_and_type_decref(o, tp);
    }
}
unsafe extern "C" fn ref_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let cb = (*(o as *mut PyWeakReference)).wr_callback;
        if cb.is_null() { 0 } else { visit.unwrap()(cb, arg) }
    }
}
unsafe extern "C" fn ref_clear(o: *mut PyObject) -> c_int {
    unsafe {
        let r = o as *mut PyWeakReference;
        {
            let _g = list_lock();
            unlink(r);
            (*r).wr_object = none::none().as_ptr();
        }
        let cb = std::mem::replace(&mut (*r).wr_callback, std::ptr::null_mut());
        xdecref(cb);
        0
    }
}
unsafe extern "C" fn ref_call(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let n = tuple::tuple_len(&*args);
        if n != 0 || (!kw.is_null() && dict::dict_len(&*kw) > 0) {
            if !kw.is_null() && dict::dict_len(&*kw) > 0 {
                return into_c(Err(err!(TypeError, "weakref() takes no keyword arguments")));
            }
            return into_c(Err(err!(TypeError, "weakref() takes no arguments ({} given)", n)));
        }
        match referent(&*o) {
            Some(x) => x.into_raw(),
            None => none::none_ref().into_raw(),
        }
    }
}
unsafe extern "C" fn ref_hash(o: *mut PyObject) -> Py_hash_t {
    unsafe {
        let r = wr(&*o);
        if r.hash != -1 {
            return r.hash;
        }
        match referent(&*o) {
            None => {
                let _ = err!(TypeError, "weak object has gone away");
                -1
            }
            Some(x) => match ab::hash(&x) {
                Ok(h) => {
                    r.hash = h;
                    h
                }
                Err(_) => -1,
            },
        }
    }
}
fn type_display(o: &PyObject) -> String {
    type_fqname(o.type_ptr())
}
unsafe extern "C" fn ref_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        let res = (|| -> PyResult {
            match referent(o) {
                None => Ok(pystr::new_str(&format!("<weakref at {:#x}; dead>", o.as_ptr() as usize))),
                Some(x) => {
                    let name = ab::lookup_special(&x, crate::name!("__name__"))?;
                    let base = format!("<weakref at {:#x}; to '{}' at {:#x}", o.as_ptr() as usize, type_display(&x), x.as_ptr() as usize);
                    match name {
                        Some(n) if pystr::is_str(&n) => Ok(pystr::new_str(&format!("{} ({})>", base, pystr::to_string_lossy(&n)))),
                        Some(n) => Ok(pystr::new_str(&format!("{} ({})>", base, ab::str_string(&n)?))),
                        None => Ok(pystr::new_str(&format!("{}>", base))),
                    }
                }
            }
        })();
        into_c(res)
    }
}
unsafe extern "C" fn ref_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if (op != Py_EQ && op != Py_NE) || !is_ref(a) || !is_ref(b) {
            return none::not_implemented().to_ref().into_raw();
        }
        let (x, y) = (referent(a), referent(b));
        match (x, y) {
            (Some(x), Some(y)) => into_c(ab::rich_compare(&x, &y, op)),
            _ => {
                let same = a.is(b);
                bool_::new_bool(if op == Py_EQ { same } else { !same }).into_raw()
            }
        }
    }
}
fn callback_g(o: &PyObject) -> PyResult {
    let _g = list_lock();
    let cb = wr(o).wr_callback;
    Ok(if cb.is_null() { none::none_ref() } else { unsafe { PyRef::from_borrowed(cb) } })
}
fn class_getitem_m(cls: &PyObject, item: &PyObject) -> PyResult {
    crate::types::containers_reg::generic_alias(cls, item)
}

// ------------------------------------------------------------------ proxy slots

fn live(o: &PyObject) -> PyResult<PyRef> {
    match referent(o) {
        Some(x) => Ok(x),
        None => raise!(ReferenceError, "weakly-referenced object no longer exists"),
    }
}
/// Unwrap a proxy operand (other operands pass through).
fn unwrap(o: &PyObject) -> PyResult<PyRef> {
    if is_proxy(o) { live(o) } else { Ok(o.to_ref()) }
}

unsafe extern "C" fn proxy_getattro(o: *mut PyObject, n: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(live(&*o).and_then(|x| ab::getattr(&x, &*n))) }
}
unsafe extern "C" fn proxy_setattro(o: *mut PyObject, n: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe { int_into_c(live(&*o).and_then(|x| ab::setattr_opt(&x, &*n, if v.is_null() { None } else { Some(&*v) }))) }
}
unsafe extern "C" fn proxy_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        let s = match referent(o) {
            None => format!("<weakproxy at {:#x}; dead>", o.as_ptr() as usize),
            Some(x) => format!("<weakproxy at {:#x}; to '{}' at {:#x}>", o.as_ptr() as usize, type_display(&x), x.as_ptr() as usize),
        };
        pystr::new_str(&s).into_raw()
    }
}
unsafe extern "C" fn proxy_str(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(live(&*o).and_then(|x| ab::str(&x))) }
}
unsafe extern "C" fn proxy_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let x = unwrap(&*a)?;
            let y = unwrap(&*b)?;
            ab::rich_compare(&x, &y, op)
        })();
        into_c(r)
    }
}
unsafe extern "C" fn proxy_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(live(&*o).and_then(|x| ab::get_iter(&x))) }
}
unsafe extern "C" fn proxy_iternext(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult<Option<PyRef>> {
            let x = live(&*o)?;
            if x.ty().tp_iternext.is_none() {
                raise!(TypeError, "Weakref proxy referenced a non-iterator '{}' object", tname(&x));
            }
            ab::iter_next(&x)
        })();
        match r {
            Ok(Some(v)) => v.into_raw(),
            _ => std::ptr::null_mut(),
        }
    }
}
unsafe extern "C" fn proxy_call(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(live(&*o).and_then(|x| ab::call_tp_call(&x, &*args, if kw.is_null() { None } else { Some(&*kw) }))) }
}
unsafe extern "C" fn proxy_len(o: *mut PyObject) -> Py_ssize_t {
    unsafe {
        match live(&*o).and_then(|x| ab::length(&x)) {
            Ok(n) => n as isize,
            Err(_) => -1,
        }
    }
}
unsafe extern "C" fn proxy_contains(o: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe {
        match live(&*o).and_then(|x| ab::contains(&x, &*v)) {
            Ok(b) => b as c_int,
            Err(_) => -1,
        }
    }
}
unsafe extern "C" fn proxy_getitem(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(live(&*o).and_then(|x| ab::getitem(&x, &*k))) }
}
unsafe extern "C" fn proxy_setitem(o: *mut PyObject, k: *mut PyObject, v: *mut PyObject) -> c_int {
    unsafe { int_into_c(live(&*o).and_then(|x| ab::setitem(&x, &*k, if v.is_null() { None } else { Some(&*v) }))) }
}
unsafe extern "C" fn proxy_bool(o: *mut PyObject) -> c_int {
    unsafe {
        match live(&*o).and_then(|x| ab::is_true(&x)) {
            Ok(b) => b as c_int,
            Err(_) => -1,
        }
    }
}

macro_rules! proxy_binop {
    ($name:ident, $slot:expr) => {
        fn $name(a: &PyObject, b: &PyObject) -> PyResult {
            let x = unwrap(a)?;
            let y = unwrap(b)?;
            ab::binary_op(&x, &y, $slot)
        }
    };
}
macro_rules! proxy_inplace {
    ($name:ident, $slot:expr) => {
        fn $name(a: &PyObject, b: &PyObject) -> PyResult {
            let x = unwrap(a)?;
            let y = unwrap(b)?;
            ab::inplace_op(&x, &y, $slot)
        }
    };
}
proxy_binop!(p_add, NbSlot::Add);
proxy_binop!(p_sub, NbSlot::Sub);
proxy_binop!(p_mul, NbSlot::Mul);
proxy_binop!(p_mod, NbSlot::Mod);
proxy_binop!(p_divmod, NbSlot::Divmod);
proxy_binop!(p_lshift, NbSlot::Lshift);
proxy_binop!(p_rshift, NbSlot::Rshift);
proxy_binop!(p_and, NbSlot::And);
proxy_binop!(p_xor, NbSlot::Xor);
proxy_binop!(p_or, NbSlot::Or);
proxy_binop!(p_floordiv, NbSlot::FloorDiv);
proxy_binop!(p_truediv, NbSlot::TrueDiv);
proxy_binop!(p_matmul, NbSlot::MatMul);
proxy_inplace!(p_iadd, NbSlot::Add);
proxy_inplace!(p_isub, NbSlot::Sub);
proxy_inplace!(p_imul, NbSlot::Mul);
proxy_inplace!(p_imod, NbSlot::Mod);
proxy_inplace!(p_ilshift, NbSlot::Lshift);
proxy_inplace!(p_irshift, NbSlot::Rshift);
proxy_inplace!(p_iand, NbSlot::And);
proxy_inplace!(p_ixor, NbSlot::Xor);
proxy_inplace!(p_ior, NbSlot::Or);
proxy_inplace!(p_ifloordiv, NbSlot::FloorDiv);
proxy_inplace!(p_itruediv, NbSlot::TrueDiv);
proxy_inplace!(p_imatmul, NbSlot::MatMul);

unsafe extern "C" fn p_pow(a: *mut PyObject, b: *mut PyObject, c: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| {
            let x = unwrap(&*a)?;
            let y = unwrap(&*b)?;
            let z = unwrap(&*c)?;
            ab::power(&x, &y, &z)
        })();
        into_c(r)
    }
}
unsafe extern "C" fn p_ipow(a: *mut PyObject, b: *mut PyObject, c: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| {
            let x = unwrap(&*a)?;
            let y = unwrap(&*b)?;
            let _ = unwrap(&*c)?;
            ab::inplace_op(&x, &y, NbSlot::Pow)
        })();
        into_c(r)
    }
}
macro_rules! proxy_unary {
    ($name:ident, $f:expr) => {
        unsafe extern "C" fn $name(o: *mut PyObject) -> *mut PyObject {
            unsafe { into_c(live(&*o).and_then(|x| $f(&x))) }
        }
    };
}
proxy_unary!(p_neg, ab::negative);
proxy_unary!(p_pos, ab::positive);
proxy_unary!(p_abs, ab::absolute);
proxy_unary!(p_invert, ab::invert);
proxy_unary!(p_index, ab::index);
proxy_unary!(p_int, crate::types::int_methods::number_long);
fn to_float(x: &PyObject) -> PyResult {
    ab::call(type_obj(crate::types::float::float_type()), &[x])
}
proxy_unary!(p_float, to_float);

fn proxy_bytes_m(o: &PyObject) -> PyResult {
    let x = live(o)?;
    ab::call(type_obj(crate::types::bytes::bytes_type()), &[&x])
}
fn proxy_reversed_m(o: &PyObject) -> PyResult {
    let x = live(o)?;
    let r = crate::builtins::builtin("reversed")?;
    ab::call(&r, &[&x])
}

// ------------------------------------------------------------------ registration

static mut PROXY_NUM: PyNumberMethods = PyNumberMethods::ZERO;
static mut PROXY_SEQ: PySequenceMethods = PySequenceMethods::ZERO;
static mut PROXY_MAP: PyMappingMethods = PyMappingMethods::ZERO;

pub fn init_types() {
    use crate::methods::binary_shim as bs;
    unsafe {
        let t = &mut *ref_type();
        t.tp_name = c"weakref.ReferenceType".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyWeakReference>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_BASETYPE;
        t.tp_dealloc = Some(ref_dealloc);
        t.tp_traverse = Some(ref_traverse);
        t.tp_clear = Some(ref_clear);
        t.tp_call = Some(ref_call);
        t.tp_hash = Some(ref_hash);
        t.tp_repr = Some(ref_repr);
        t.tp_richcompare = Some(ref_richcompare);
        t.tp_new = Some(ref_new);
        t.tp_init = Some(ref_init);
        t.tp_alloc = Some(generic_alloc);
        t.tp_free = Some(generic_free);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_members = crate::member_table![(
            "__callback__",
            Py_T_OBJECT,
            std::mem::offset_of!(PyWeakReference, wr_callback),
            Py_READONLY
        )];
        let _ = callback_g as fn(&PyObject) -> PyResult;
        t.tp_methods = crate::mdefs![("__class_getitem__", O, class_getitem_m, "See PEP 585"; METH_CLASS)];

        let n = &mut *(&raw mut PROXY_NUM);
        n.nb_add = Some(bs(p_add));
        n.nb_subtract = Some(bs(p_sub));
        n.nb_multiply = Some(bs(p_mul));
        n.nb_remainder = Some(bs(p_mod));
        n.nb_divmod = Some(bs(p_divmod));
        n.nb_power = Some(p_pow);
        n.nb_negative = Some(p_neg);
        n.nb_positive = Some(p_pos);
        n.nb_absolute = Some(p_abs);
        n.nb_bool = Some(proxy_bool);
        n.nb_invert = Some(p_invert);
        n.nb_lshift = Some(bs(p_lshift));
        n.nb_rshift = Some(bs(p_rshift));
        n.nb_and = Some(bs(p_and));
        n.nb_xor = Some(bs(p_xor));
        n.nb_or = Some(bs(p_or));
        n.nb_int = Some(p_int);
        n.nb_float = Some(p_float);
        n.nb_inplace_add = Some(bs(p_iadd));
        n.nb_inplace_subtract = Some(bs(p_isub));
        n.nb_inplace_multiply = Some(bs(p_imul));
        n.nb_inplace_remainder = Some(bs(p_imod));
        n.nb_inplace_power = Some(p_ipow);
        n.nb_inplace_lshift = Some(bs(p_ilshift));
        n.nb_inplace_rshift = Some(bs(p_irshift));
        n.nb_inplace_and = Some(bs(p_iand));
        n.nb_inplace_xor = Some(bs(p_ixor));
        n.nb_inplace_or = Some(bs(p_ior));
        n.nb_floor_divide = Some(bs(p_floordiv));
        n.nb_true_divide = Some(bs(p_truediv));
        n.nb_inplace_floor_divide = Some(bs(p_ifloordiv));
        n.nb_inplace_true_divide = Some(bs(p_itruediv));
        n.nb_index = Some(p_index);
        n.nb_matrix_multiply = Some(bs(p_matmul));
        n.nb_inplace_matrix_multiply = Some(bs(p_imatmul));
        let s = &mut *(&raw mut PROXY_SEQ);
        s.sq_contains = Some(proxy_contains);
        let m = &mut *(&raw mut PROXY_MAP);
        m.mp_length = Some(proxy_len);
        m.mp_subscript = Some(proxy_getitem);
        m.mp_ass_subscript = Some(proxy_setitem);
        for (tp, nm) in [(proxy_type(), c"weakref.ProxyType"), (callable_proxy_type(), c"weakref.CallableProxyType")] {
            let t = &mut *tp;
            t.tp_name = nm.as_ptr();
            t.tp_basicsize = std::mem::size_of::<PyWeakReference>() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
            t.tp_dealloc = Some(ref_dealloc);
            t.tp_traverse = Some(ref_traverse);
            t.tp_clear = Some(ref_clear);
            t.tp_repr = Some(proxy_repr);
            t.tp_str = Some(proxy_str);
            t.tp_hash = Some(crate::types::slots::hash_not_implemented);
            t.tp_getattro = Some(proxy_getattro);
            t.tp_setattro = Some(proxy_setattro);
            t.tp_richcompare = Some(proxy_richcompare);
            t.tp_iter = Some(proxy_iter);
            t.tp_iternext = Some(proxy_iternext);
            t.tp_as_number = n;
            t.tp_as_sequence = s;
            t.tp_as_mapping = m;
            t.tp_methods = crate::mdefs![
                ("__bytes__", NOARGS, proxy_bytes_m),
                ("__reversed__", NOARGS, proxy_reversed_m),
            ];
        }
        (*callable_proxy_type()).tp_call = Some(proxy_call);
    }
}

pub fn ready() -> PyResult<()> {
    type_ready(ref_type())?;
    type_ready(proxy_type())?;
    type_ready(callable_proxy_type())?;
    // type objects support weak references through tp_weaklist
    unsafe {
        let tt = type_type();
        if (*tt).tp_weaklistoffset == 0 {
            (*tt).tp_weaklistoffset = std::mem::offset_of!(PyTypeObject, tp_weaklist) as isize;
        }
    }
    Ok(())
}

#[allow(dead_code)]
fn _unused() -> (*mut PyTypeObject, PyRef) {
    (exceptions::ReferenceError(), int::new_int(0))
}
