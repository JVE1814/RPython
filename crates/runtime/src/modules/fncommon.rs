//! Small helpers shared by the functional/collection accelerator modules
//! (_functools, _collections, itertools, _operator, _abc, ...).

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::{dict, module, none, str as pystr, tuple};
use std::ffi::{CStr, c_int, c_ulong, c_void};

/// Basic static-type initialisation shared by all our types.
pub unsafe fn init_type(t: *mut PyTypeObject, name: &'static CStr, size: usize, flags: c_ulong) {
    unsafe {
        let t = &mut *t;
        t.tp_name = name.as_ptr();
        t.tp_basicsize = size as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_IMMUTABLETYPE | flags;
        t.tp_getattro = Some(crate::types::typeobject::object_generic_getattro);
        if flags & Py_TPFLAGS_BASETYPE != 0 || t.tp_alloc.is_none() {
            t.tp_alloc = Some(crate::types::typeobject::generic_alloc);
        }
        t.tp_free = Some(crate::types::typeobject::generic_free);
    }
}

/// Allocate an instance of `tp` (respecting subclass sizes via tp_alloc).
pub unsafe fn alloc(tp: *mut PyTypeObject) -> PyResult<*mut PyObject> {
    unsafe {
        let a = (*tp).tp_alloc.unwrap_or(crate::types::typeobject::generic_alloc);
        let op = a(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        Ok(op)
    }
}

/// Generic dealloc tail: free the memory and drop the heap-type ref.
pub unsafe fn free(op: *mut PyObject) {
    unsafe {
        let tp = (*op).type_ptr();
        crate::types::typeobject::generic_free_and_type_decref(op, tp);
    }
}

#[inline]
pub unsafe fn opt<'a>(p: *mut PyObject) -> Option<&'a PyObject> {
    if p.is_null() { None } else { unsafe { Some(&*p) } }
}

/// Replace an owned pointer field.
#[inline]
pub unsafe fn set_field(slot: *mut *mut PyObject, v: Option<PyRef>) {
    unsafe {
        let old = *slot;
        *slot = match v {
            Some(r) => r.into_raw(),
            None => std::ptr::null_mut(),
        };
        xdecref(old);
    }
}

#[inline]
pub unsafe fn clear_field(slot: *mut *mut PyObject) {
    unsafe { set_field(slot, None) }
}

/// Visit helper for tp_traverse implementations.
#[inline]
pub unsafe fn visit(p: *mut PyObject, v: visitproc, arg: *mut c_void) -> c_int {
    if p.is_null() { 0 } else { unsafe { v.unwrap()(p, arg) } }
}

#[macro_export]
macro_rules! visit_all {
    ($v:expr, $arg:expr; $($p:expr),* $(,)?) => {
        $( { let r = unsafe { $crate::modules::fncommon::visit($p, $v, $arg) }; if r != 0 { return r; } } )*
    };
}

/// Borrow the items of the args tuple passed to tp_new/tp_init/tp_call.
#[inline]
pub unsafe fn targs<'a>(args: *mut PyObject) -> &'a [&'a PyObject] {
    if args.is_null() { &[] } else { unsafe { tuple::tuple_items(&*args) } }
}

pub unsafe fn kw_len(kw: *mut PyObject) -> usize {
    if kw.is_null() { 0 } else { unsafe { dict::dict_len(&*kw) } }
}

/// `_PyArg_NoKeywords`: raise "X() takes no keyword arguments".
pub unsafe fn no_kw(fname: &str, kw: *mut PyObject) -> PyResult<()> {
    if unsafe { kw_len(kw) } > 0 {
        crate::raise!(TypeError, "{}() takes no keyword arguments", fname);
    }
    Ok(())
}

/// No-keywords check that is skipped for subclasses whose __init__ differs (like CPython does
/// for many C types: only enforced when `tp` is exactly `base`).
pub unsafe fn no_kw_exact(tp: *mut PyTypeObject, base: *mut PyTypeObject, fname: &str, kw: *mut PyObject) -> PyResult<()> {
    if tp == base { unsafe { no_kw(fname, kw) } } else { Ok(()) }
}

pub fn none() -> PyRef {
    none::none_ref()
}

pub fn new_str(s: &str) -> PyRef {
    pystr::new_str(s)
}

pub fn tname(o: &PyObject) -> String {
    crate::types::typeobject::tname(o)
}

pub fn type_name_of_type(tp: *mut PyTypeObject) -> String {
    unsafe { crate::types::typeobject::type_name(&*tp) }
}

/// Name used in reprs: the qualified type name (`type(self).__name__`).
pub fn repr_type_name(o: &PyObject) -> String {
    crate::types::typeobject::type_name(o.ty())
}

/// `Py_GenericAlias(cls, item)` — implemented through `types.GenericAlias`.
pub fn generic_alias(cls: &PyObject, item: &PyObject) -> PyResult {
    let ga = generic_alias_type()?;
    ab::call(&ga, &[cls, item])
}

fn generic_alias_type() -> PyResult {
    // Prefer the builtin implementation if one is exported by the runtime (`list[int]`).
    let lst = crate::types::typeobject::type_obj(crate::types::list::list_type());
    if let Some(cg) = ab::getattr_opt(lst, crate::name!("__class_getitem__"))? {
        let r = ab::call(&cg, &[crate::types::typeobject::type_obj(crate::types::int::int_type())])?;
        return Ok(crate::types::typeobject::type_ref(r.type_ptr()));
    }
    let types = crate::import::import_absolute("types")?;
    ab::getattr_str(&types, "GenericAlias")
}

/// METH_O|METH_CLASS `__class_getitem__` implementation.
pub fn class_getitem(cls: &PyObject, item: &PyObject) -> PyResult {
    generic_alias(cls, item)
}

/// Create a module with the given method table; returns (module, dict).
pub fn new_module(name: &str, methods: *mut PyMethodDef, doc: Option<&str>) -> PyResult<(PyRef, PyRef)> {
    let m = module::new_module(name)?;
    let d = module::module_dict(&m);
    if !methods.is_null() {
        crate::builtins::add_methods_to_dict(&d, methods, Some(&m))?;
    }
    if let Some(doc) = doc {
        dict::set_item_str(&d, "__doc__", &pystr::new_str(doc))?;
    }
    Ok((m, d))
}

/// Set `__module__` in a static type's dict (types have tp_name "mod.Name").
pub fn add_type(d: &PyObject, name: &str, tp: *mut PyTypeObject) -> PyResult<()> {
    crate::types::typeobject::type_ready(tp)?;
    dict::set_item_str(d, name, crate::types::typeobject::type_obj(tp))
}

/// Recursion guard for reprs (Py_ReprEnter / Py_ReprLeave).
pub struct ReprGuard<'a>(&'a PyObject, bool);
impl<'a> ReprGuard<'a> {
    /// Returns None if the object is already being repr'd (recursive call).
    pub fn enter(o: &'a PyObject) -> Option<ReprGuard<'a>> {
        if crate::modules::reprlib_guard::enter(o) { None } else { Some(ReprGuard(o, true)) }
    }
}
impl Drop for ReprGuard<'_> {
    fn drop(&mut self) {
        if self.1 {
            crate::modules::reprlib_guard::leave(self.0);
        }
    }
}

/// Run `f` with the C-recursion guard (Py_EnterRecursiveCall).
pub fn with_recursion<T>(what: &str, f: impl FnOnce() -> PyResult<T>) -> PyResult<T> {
    let ts = crate::threads::current();
    crate::interp::enter_recursive_call(ts, what)?;
    let r = f();
    crate::interp::leave_recursive_call(ts);
    r
}

/// Convert an index-like argument to isize with the given error on overflow.
pub fn ssize_arg(o: &PyObject) -> PyResult<isize> {
    ab::index_as_ssize(o, Some(crate::types::exceptions::OverflowError()))
}

/// Get the kwargs dict as vectorcall values + kwnames.
pub fn vc_from_kwdict(pos: &[&PyObject], kw: Option<&PyObject>) -> (Vec<PyRef>, Option<PyRef>) {
    let mut v: Vec<PyRef> = pos.iter().map(|x| x.to_ref()).collect();
    match kw {
        Some(d) if dict::dict_len(d) > 0 => {
            let mut names = Vec::new();
            for (k, val) in dict::items_vec(d) {
                names.push(k);
                v.push(val);
            }
            (v, Some(tuple::new_tuple(names)))
        }
        _ => (v, None),
    }
}

/// Call with positional args and an optional kwargs dict.
pub fn call_kwdict(f: &PyObject, pos: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    ab::call_with_kwargs_dict(f, pos, kw)
}

/// Python-level `__dict__` getter/setter helpers for types with a tp_dictoffset.
pub fn get_dict_attr(o: &PyObject) -> PyResult {
    match crate::types::typeobject::instance_dict(o) {
        Some(d) => Ok(d),
        None => crate::raise!(AttributeError, "This object has no __dict__"),
    }
}

pub fn set_dict_attr(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let v = match v {
        Some(v) => v,
        None => crate::raise!(TypeError, "cannot delete __dict__"),
    };
    if !dict::is_dict(v) {
        crate::raise!(TypeError, "__dict__ must be set to a dictionary, not a '{}'", tname(v));
    }
    unsafe {
        let slot = crate::types::typeobject::dict_slot(o.as_ptr());
        if slot.is_null() {
            crate::raise!(AttributeError, "This object has no __dict__");
        }
        set_field(slot, Some(v.to_ref()));
    }
    Ok(())
}

/// Is `o` a "true" str (exact or subclass).
pub fn is_str(o: &PyObject) -> bool {
    pystr::is_str(o)
}

pub fn bool_obj(b: bool) -> PyRef {
    crate::types::bool_::new_bool(b)
}

pub fn int_obj(v: i64) -> PyRef {
    crate::types::int::new_int(v)
}

// ------------------------------------------------------------------ Rust-state objects
//
// Many of our types are "a PyObject header followed by a Rust struct". `RObj<T>` is
// that layout; the Rust part is written right after allocation and dropped in
// tp_dealloc. `T: Trav` reports the contained Python references to the GC.

pub trait Trav {
    fn trav(&self, _v: &mut dyn FnMut(*mut PyObject) -> c_int) -> c_int {
        0
    }
}

#[repr(C)]
pub struct RObj<T> {
    pub ob_base: PyObject,
    pub weakreflist: *mut PyObject,
    pub inner: T,
}

/// Allocate an instance of `tp` (or a subclass) and move `inner` into it.
pub fn robj_new<T>(tp: *mut PyTypeObject, inner: T) -> PyResult {
    unsafe {
        let op = alloc(tp)?;
        std::ptr::write(&raw mut (*(op as *mut RObj<T>)).inner, inner);
        Ok(PyRef::from_raw(op))
    }
}

/// Access the Rust state. Callers must not keep the reference across calls into
/// Python code that could re-enter the same object.
#[inline(always)]
pub fn rs<T>(o: &PyObject) -> &mut T {
    unsafe { &mut (*(o.as_ptr() as *mut RObj<T>)).inner }
}

pub unsafe extern "C" fn robj_dealloc<T>(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let p = o as *mut RObj<T>;
        crate::types::weakref::clear_weakrefs_at(o, &raw mut (*p).weakreflist);
        std::ptr::drop_in_place(&raw mut (*p).inner);
        free(o);
    }
}

pub unsafe extern "C" fn robj_traverse<T: Trav>(o: *mut PyObject, v: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let p = &*(o as *mut RObj<T>);
        let f = v.unwrap();
        p.inner.trav(&mut |x: *mut PyObject| if x.is_null() { 0 } else { f(x, arg) })
    }
}

/// Initialise a static type whose instances are `RObj<T>`.
pub unsafe fn init_robj_type<T: Trav>(t: *mut PyTypeObject, name: &'static CStr, flags: c_ulong) {
    unsafe {
        init_type(t, name, std::mem::size_of::<RObj<T>>(), flags | Py_TPFLAGS_HAVE_GC);
        (*t).tp_dealloc = Some(robj_dealloc::<T>);
        (*t).tp_traverse = Some(robj_traverse::<T>);
        (*t).tp_weaklistoffset = std::mem::offset_of!(RObj<T>, weakreflist) as isize;
    }
}

/// Helper for Trav impls: visit an optional reference.
#[macro_export]
macro_rules! trav {
    ($v:expr; $($e:expr),* $(,)?) => {{
        $( { let r = $crate::modules::fncommon::trav_opt($v, &$e); if r != 0 { return r; } } )*
        0
    }};
}

pub fn trav_opt(v: &mut dyn FnMut(*mut PyObject) -> c_int, o: &Option<PyRef>) -> c_int {
    match o {
        Some(r) => v(r.as_ptr()),
        None => 0,
    }
}

pub fn trav_vec(v: &mut dyn FnMut(*mut PyObject) -> c_int, items: &[PyRef]) -> c_int {
    for r in items {
        let x = v(r.as_ptr());
        if x != 0 {
            return x;
        }
    }
    0
}

/// Return a tuple with `items`, reusing `slot`'s tuple in place when nobody else
/// holds a reference to it (CPython's result-tuple reuse for combinatoric iterators).
pub fn reuse_tuple(slot: &mut Option<PyRef>, items: Vec<PyRef>) -> PyRef {
    if let Some(t) = slot.as_ref() {
        if t.refcnt() == 1 && tuple::tuple_len(t) == items.len() {
            unsafe {
                let p = tuple::item_ptr(t.as_ptr());
                for (i, it) in items.into_iter().enumerate() {
                    let old = *p.add(i);
                    *p.add(i) = it.into_raw();
                    xdecref(old);
                }
                crate::gc::track(t.as_ptr());
            }
            return t.clone();
        }
    }
    let t = tuple::new_tuple(items);
    // the tuple is refilled in place later, so it must stay visible to the collector
    // even if its first items are atomic
    unsafe { crate::gc::track(t.as_ptr()) };
    *slot = Some(t.clone());
    t
}

/// tp_new arguments converted to the FASTKW convention, ready for `args::parse`.
pub struct VcArgs {
    pub vals: Vec<PyRef>,
    pub names: Option<PyRef>,
}
impl VcArgs {
    pub unsafe fn from_c(args: *mut PyObject, kw: *mut PyObject) -> VcArgs {
        let (vals, names) = vc_from_kwdict(unsafe { targs(args) }, unsafe { opt(kw) });
        VcArgs { vals, names }
    }
    pub fn refs(&self) -> Vec<&PyObject> {
        self.vals.iter().map(|x| &**x).collect()
    }
    pub fn parse<const N: usize>(&self, p: &crate::args::Params) -> PyResult<[Option<PyRef>; N]> {
        let r = self.refs();
        let out = crate::args::parse::<N>(p, &r, self.names.as_deref())?;
        Ok(out.map(|o| o.map(|x| x.to_ref())))
    }
}

/// The keyword check CPython applies to C types that take no keyword arguments:
/// enforced unless a subclass overrides __init__.
pub unsafe fn no_kw_unless_init_overridden(tp: *mut PyTypeObject, base: *mut PyTypeObject, fname: &str, kw: *mut PyObject) -> PyResult<()> {
    unsafe {
        if tp == base || (*tp).tp_init == (*base).tp_init {
            no_kw(fname, kw)?;
        }
    }
    Ok(())
}

/// The raw `tp_name` of the object's type (e.g. "operator.attrgetter" or a subclass name).
pub fn raw_tp_name(o: &PyObject) -> String {
    unsafe { CStr::from_ptr(o.ty().tp_name).to_string_lossy().into_owned() }
}
