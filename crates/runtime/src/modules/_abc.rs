//! The `_abc` accelerator: ABC registration and isinstance/issubclass caches
//! (positive cache, negative cache with a global invalidation counter).
//!
//! The per-class state lives in an `_abc_data` object stored as `cls._abc_impl`.
//! Registry and caches are identity sets of classes. (Until the runtime has
//! weak references they hold strong references.)

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::fncommon as fc;
use crate::object::*;
use crate::types::typeobject::{as_type, is_type, type_obj};
use crate::types::{bool_, dict, int, none, set, str as pystr, tuple};
use crate::{mdefs, name, raise};
use std::collections::HashMap;
use std::ffi::{c_int, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::*};

static INVALIDATION_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct IdSet(HashMap<usize, PyRef>);
impl IdSet {
    fn contains(&self, o: &PyObject) -> bool {
        self.0.contains_key(&(o.as_ptr() as usize))
    }
    fn add(&mut self, o: &PyObject) {
        self.0.entry(o.as_ptr() as usize).or_insert_with(|| o.to_ref());
    }
    fn items(&self) -> Vec<PyRef> {
        self.0.values().cloned().collect()
    }
    fn clear(&mut self) -> Vec<PyRef> {
        self.0.drain().map(|(_, v)| v).collect()
    }
}

#[derive(Default)]
struct AbcState {
    registry: IdSet,
    cache: IdSet,
    negative_cache: IdSet,
    negative_cache_version: u64,
}

#[repr(C)]
struct AbcData {
    ob_base: PyObject,
    state: *mut Mutex<AbcState>,
}

#[unsafe(no_mangle)]
pub static mut _PyAbc_Data_Type: PyTypeObject = PyTypeObject::ZERO;
fn abc_data_type() -> *mut PyTypeObject {
    &raw mut _PyAbc_Data_Type
}

fn new_abc_data() -> PyResult {
    unsafe {
        let op = fc::alloc(abc_data_type())?;
        let mut st = AbcState::default();
        st.negative_cache_version = INVALIDATION_COUNTER.load(Acquire);
        (*(op as *mut AbcData)).state = Box::into_raw(Box::new(Mutex::new(st)));
        Ok(PyRef::from_raw(op))
    }
}

unsafe extern "C" fn abc_data_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    unsafe {
        let _ = tp;
        into_c(new_abc_data())
    }
}

unsafe extern "C" fn abc_data_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let d = &mut *(o as *mut AbcData);
        if !d.state.is_null() {
            let st = Box::from_raw(d.state);
            d.state = std::ptr::null_mut();
            drop(st);
        }
        fc::free(o);
    }
}

unsafe extern "C" fn abc_data_traverse(o: *mut PyObject, v: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let d = &*(o as *mut AbcData);
        if d.state.is_null() {
            return 0;
        }
        if let Ok(st) = (*d.state).try_lock() {
            for s in [&st.registry, &st.cache, &st.negative_cache] {
                for r in s.0.values() {
                    crate::visit_all!(v, arg; r.as_ptr());
                }
            }
        }
    }
    0
}

fn state_of(impl_: &PyObject) -> PyResult<&'static Mutex<AbcState>> {
    if !impl_.is_type(abc_data_type()) {
        raise!(TypeError, "_abc_impl is set to a wrong type");
    }
    unsafe { Ok(&*(*(impl_.as_ptr() as *mut AbcData)).state) }
}

fn get_impl(cls: &PyObject) -> PyResult {
    let i = ab::getattr(cls, name!("_abc_impl"))?;
    state_of(&i)?;
    Ok(i)
}

fn lock(impl_: &PyObject) -> std::sync::MutexGuard<'static, AbcState> {
    let m = state_of(impl_).expect("checked");
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Compute `__abstractmethods__` for a freshly created ABC.
fn compute_abstract_methods(cls: &PyObject) -> PyResult<()> {
    let abstracts = set::new_set_from_vec(Vec::new())?;
    // 1. direct definitions in the namespace
    let ns = ab::getattr(cls, name!("__dict__"))?;
    let items = ab::call_method_str(&ns, "items", &[])?;
    let it = ab::get_iter(&items)?;
    while let Some(pair) = ab::iter_next(&it)? {
        if !tuple::is_tuple(&pair) || tuple::tuple_len(&pair) != 2 {
            raise!(TypeError, "items() returned item which is not a 2-tuple");
        }
        let (k, v) = (tuple::tuple_get(&pair, 0), tuple::tuple_get(&pair, 1));
        if is_abstract(v)? {
            set::set_add(&abstracts, k)?;
        }
    }
    // 2. inherited abstract methods that are still abstract
    let bases = ab::getattr(cls, name!("__bases__"))?;
    if !tuple::is_tuple(&bases) {
        raise!(TypeError, "__bases__ is not tuple");
    }
    for b in tuple::tuple_items(&bases) {
        let base_abs = match ab::getattr_opt(b, name!("__abstractmethods__"))? {
            Some(x) => x,
            None => continue,
        };
        let it = ab::get_iter(&base_abs)?;
        while let Some(nm) = ab::iter_next(&it)? {
            let v = match ab::getattr_opt(cls, &nm)? {
                Some(v) => v,
                None => continue,
            };
            if is_abstract(&v)? {
                set::set_add(&abstracts, &nm)?;
            }
        }
    }
    let fs = set::new_frozenset_from_vec(set::set_items(&abstracts))?;
    ab::setattr(cls, name!("__abstractmethods__"), &fs)
}

fn is_abstract(v: &PyObject) -> PyResult<bool> {
    match ab::getattr_opt(v, name!("__isabstractmethod__"))? {
        Some(x) => ab::is_true(&x),
        None => Ok(false),
    }
}

const COLLECTION_FLAGS: std::ffi::c_ulong = Py_TPFLAGS_SEQUENCE | Py_TPFLAGS_MAPPING;

fn abc_init(_m: &PyObject, cls: &PyObject) -> PyResult {
    compute_abstract_methods(cls)?;
    let data = new_abc_data()?;
    ab::setattr(cls, name!("_abc_impl"), &data)?;
    if is_type(cls) {
        let tp = as_type(cls);
        let d = crate::types::typeobject::type_dict(tp);
        if let Some(flags) = dict::get_item_str(d, name!("__abc_tpflags__")) {
            if int::is_int(&flags) {
                let v = ab::as_i64(&flags)? as std::ffi::c_ulong;
                unsafe {
                    (*tp).tp_flags = ((*tp).tp_flags & !COLLECTION_FLAGS) | (v & COLLECTION_FLAGS);
                }
            }
            dict::del_item(d, name!("__abc_tpflags__"))?;
        }
    }
    Ok(fc::none())
}

fn set_collection_flag_recursive(tp: *mut PyTypeObject, flag: std::ffi::c_ulong) {
    unsafe {
        if (*tp).tp_flags & Py_TPFLAGS_IMMUTABLETYPE != 0 || (*tp).tp_flags & COLLECTION_FLAGS != 0 {
            return;
        }
        (*tp).tp_flags = ((*tp).tp_flags & !COLLECTION_FLAGS) | flag;
        for sub in crate::types::typeobject::subclasses(tp) {
            let sub = sub.p();
            set_collection_flag_recursive(sub, flag);
        }
    }
}

fn abc_register(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    crate::args::check_pos("_abc_register", a, 2, 2)?;
    let (cls, subclass) = (a[0], a[1]);
    if !is_type(subclass) {
        raise!(TypeError, "Can only register classes");
    }
    if ab::issubclass(subclass, cls)? {
        return Ok(subclass.to_ref()); // already a subclass
    }
    if ab::issubclass(cls, subclass)? {
        raise!(RuntimeError, "Refusing to create an inheritance cycle");
    }
    let imp = get_impl(cls)?;
    lock(&imp).registry.add(subclass);
    INVALIDATION_COUNTER.fetch_add(1, AcqRel);
    if is_type(cls) {
        let flags = unsafe { (*as_type(cls)).tp_flags } & COLLECTION_FLAGS;
        if flags != 0 {
            set_collection_flag_recursive(as_type(subclass), flags);
        }
    }
    Ok(subclass.to_ref())
}

fn abc_instancecheck(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    crate::args::check_pos("_abc_instancecheck", a, 2, 2)?;
    let (cls, instance) = (a[0], a[1]);
    let imp = get_impl(cls)?;
    let subclass = ab::getattr(instance, name!("__class__"))?;
    if lock(&imp).cache.contains(&subclass) {
        return Ok(bool_::new_bool(true));
    }
    let subtype = type_obj(instance.type_ptr());
    if subtype.is(&subclass) {
        {
            let st = lock(&imp);
            if st.negative_cache_version == INVALIDATION_COUNTER.load(Acquire) && st.negative_cache.contains(&subclass) {
                return Ok(bool_::new_bool(false));
            }
        }
        return ab::call_method(cls, name!("__subclasscheck__"), &[&subclass]);
    }
    let r = ab::call_method(cls, name!("__subclasscheck__"), &[&subclass])?;
    if ab::is_true(&r)? {
        return Ok(r);
    }
    ab::call_method(cls, name!("__subclasscheck__"), &[subtype])
}

fn abc_subclasscheck(_m: &PyObject, a: &[&PyObject]) -> PyResult {
    crate::args::check_pos("_abc_subclasscheck", a, 2, 2)?;
    let (cls, subclass) = (a[0], a[1]);
    if !is_type(subclass) {
        raise!(TypeError, "issubclass() arg 1 must be a class");
    }
    let imp = get_impl(cls)?;
    // 1. positive cache
    {
        let mut st = lock(&imp);
        if st.cache.contains(subclass) {
            return Ok(bool_::new_bool(true));
        }
        // 2. negative cache (invalidate if outdated)
        let cur = INVALIDATION_COUNTER.load(Acquire);
        if st.negative_cache_version < cur {
            let old = st.negative_cache.clear();
            st.negative_cache_version = cur;
            drop(st);
            drop(old);
        } else if st.negative_cache.contains(subclass) {
            return Ok(bool_::new_bool(false));
        }
    }
    // 3. the subclass hook
    let ok = ab::call_method(cls, name!("__subclasshook__"), &[subclass])?;
    if ok.is(bool_::true_obj()) {
        lock(&imp).cache.add(subclass);
        return Ok(ok);
    }
    if ok.is(bool_::false_obj()) {
        lock(&imp).negative_cache.add(subclass);
        return Ok(ok);
    }
    if !ok.is(none::not_implemented()) {
        raise!(AssertionError, "__subclasshook__ must return either False, True, or NotImplemented");
    }
    drop(ok);
    // 4. direct subclass (cls in subclass.__mro__)
    unsafe {
        let mro = (*as_type(subclass)).tp_mro;
        if !mro.is_null() {
            for t in tuple::tuple_items(&*mro) {
                if t.is(cls) {
                    lock(&imp).cache.add(subclass);
                    return Ok(bool_::new_bool(true));
                }
            }
        }
    }
    // 5. registered classes (recursively)
    let registry = lock(&imp).registry.items();
    for r in registry {
        if ab::issubclass(subclass, &r)? {
            lock(&imp).cache.add(subclass);
            return Ok(bool_::new_bool(true));
        }
    }
    // 6. subclasses (recursively)
    let subs = ab::call_method(cls, name!("__subclasses__"), &[])?;
    let subs = ab::collect_iterable(&subs)?;
    for s in subs {
        if ab::issubclass(subclass, &s)? {
            lock(&imp).cache.add(subclass);
            return Ok(bool_::new_bool(true));
        }
    }
    lock(&imp).negative_cache.add(subclass);
    Ok(bool_::new_bool(false))
}

fn idset_to_pyset(s: &IdSet) -> PyResult {
    set::new_set_from_vec(s.items())
}

fn abc_get_dump(_m: &PyObject, cls: &PyObject) -> PyResult {
    let imp = get_impl(cls)?;
    let st = lock(&imp);
    let r = (
        idset_to_pyset(&st.registry)?,
        idset_to_pyset(&st.cache)?,
        idset_to_pyset(&st.negative_cache)?,
        int::new_int(st.negative_cache_version as i64),
    );
    drop(st);
    Ok(tuple::new_tuple(vec![r.0, r.1, r.2, r.3]))
}

fn abc_reset_registry(_m: &PyObject, cls: &PyObject) -> PyResult {
    let imp = get_impl(cls)?;
    let old = lock(&imp).registry.clear();
    drop(old);
    Ok(fc::none())
}

fn abc_reset_caches(_m: &PyObject, cls: &PyObject) -> PyResult {
    let imp = get_impl(cls)?;
    let (a, b) = {
        let mut st = lock(&imp);
        (st.cache.clear(), st.negative_cache.clear())
    };
    drop(a);
    drop(b);
    Ok(fc::none())
}

fn abc_get_cache_token(_m: &PyObject) -> PyResult {
    Ok(int::new_int(INVALIDATION_COUNTER.load(Acquire) as i64))
}

pub fn init_module() -> PyResult {
    unsafe {
        let t = abc_data_type();
        if (*t).tp_flags & Py_TPFLAGS_READY == 0 {
            fc::init_type(t, c"_abc._abc_data", std::mem::size_of::<AbcData>(), Py_TPFLAGS_HAVE_GC);
            (*t).tp_new = Some(abc_data_new);
            (*t).tp_dealloc = Some(abc_data_dealloc);
            (*t).tp_traverse = Some(abc_data_traverse);
            (*t).tp_doc = c"Internal state held by ABC machinery.".as_ptr();
            crate::types::typeobject::type_ready(t)?;
        }
    }
    let methods = mdefs![
        ("get_cache_token", NOARGS, abc_get_cache_token, "get_cache_token($module, /)\n--\n\nReturns the current ABC cache token.\n\nThe token is an opaque object (supporting equality testing) identifying the\ncurrent version of the ABC cache for virtual subclasses. The token changes\nwith every call to register() on any ABC."),
        ("_abc_init", O, abc_init, "_abc_init($module, self, /)\n--\n\nInternal ABC helper for class set-up. Should be never used outside abc module."),
        ("_reset_registry", O, abc_reset_registry, "_reset_registry($module, self, /)\n--\n\nInternal ABC helper to reset registry of a given class.\n\nShould be only used by refleak.py"),
        ("_reset_caches", O, abc_reset_caches, "_reset_caches($module, self, /)\n--\n\nInternal ABC helper to reset both caches of a given class.\n\nShould be only used by refleak.py"),
        ("_get_dump", O, abc_get_dump, "_get_dump($module, self, /)\n--\n\nInternal ABC helper for cache and registry debugging.\n\nReturn shallow copies of registry, of both caches, and\nnegative cache version. Don't call this function directly,\ninstead use ABC._dump_registry() for a nice repr."),
        ("_abc_register", FAST, abc_register, "_abc_register($module, self, subclass, /)\n--\n\nInternal ABC helper for subclasss registration. Should be never used outside abc module."),
        ("_abc_instancecheck", FAST, abc_instancecheck, "_abc_instancecheck($module, self, instance, /)\n--\n\nInternal ABC helper for instance checks. Should be never used outside abc module."),
        ("_abc_subclasscheck", FAST, abc_subclasscheck, "_abc_subclasscheck($module, self, subclass, /)\n--\n\nInternal ABC helper for subclasss checks. Should be never used outside abc module."),
    ];
    let (m, d) = fc::new_module("_abc", methods, Some("Module contains faster C implementation of abc.ABCMeta"))?;
    fc::add_type(&d, "_abc_data", abc_data_type())?;
    let _ = pystr::new_str;
    Ok(m)
}
