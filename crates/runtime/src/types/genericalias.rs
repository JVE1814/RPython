//! Minimal `types.GenericAlias` (PEP 585) used by the containers'
//! `__class_getitem__` until a complete implementation exists. Another
//! workstream may install a full implementation with `set_generic_alias_factory`.
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::*;
use crate::types::{bool_, none, str as pystr, tuple};
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicPtr, Ordering};

#[repr(C)]
struct GenericAlias {
    ob_base: PyObject,
    origin: *mut PyObject,
    args: *mut PyObject,
    parameters: *mut PyObject,
}

pub static mut GENERIC_ALIAS_TYPE: PyTypeObject = PyTypeObject::ZERO;

/// A Python callable `(origin, args) -> alias` replacing the built-in minimal one.
static FACTORY: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());

pub fn set_generic_alias_factory(f: &PyObject) {
    let old = FACTORY.swap(f.to_ref().into_raw(), Ordering::AcqRel);
    unsafe { xdecref(old) };
}

pub fn generic_alias(origin: &PyObject, args: &PyObject) -> PyResult {
    let f = FACTORY.load(Ordering::Acquire);
    if !f.is_null() {
        return ab::call(unsafe { &*f }, &[origin, args]);
    }
    new_alias(&raw mut GENERIC_ALIAS_TYPE, origin, args)
}

fn ga(o: &PyObject) -> &GenericAlias {
    unsafe { &*(o.as_ptr() as *const GenericAlias) }
}

fn collect_params(args: &PyObject) -> PyResult<PyRef> {
    let mut params: Vec<PyRef> = Vec::new();
    for a in tuple::tuple_items(args) {
        if is_type(a) {
            continue;
        }
        if ab::lookup_special(a, crate::name!("__typing_subst__"))?.is_some() {
            if !params.iter().any(|p| p.is(a)) {
                params.push(a.to_ref());
            }
        } else if let Some(sub) = ab::getattr_opt(a, crate::name!("__parameters__"))? {
            if tuple::is_tuple(&sub) {
                for p in tuple::tuple_items(&sub) {
                    if !params.iter().any(|q| q.is(p)) {
                        params.push(p.to_ref());
                    }
                }
            }
        }
    }
    Ok(tuple::new_tuple(params))
}

fn new_alias(tp: *mut PyTypeObject, origin: &PyObject, args: &PyObject) -> PyResult {
    let args = if tuple::is_tuple(args) { args.to_ref() } else { tuple::new_tuple(vec![args.to_ref()]) };
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
        let op = alloc(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        let g = &mut *(op as *mut GenericAlias);
        g.origin = origin.to_ref().into_raw();
        g.args = args.into_raw();
        Ok(PyRef::from_raw(op))
    }
}

unsafe extern "C" fn ga_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            if !kw.is_null() && crate::types::dict::dict_len(&*kw) > 0 {
                raise!(TypeError, "GenericAlias() takes no keyword arguments");
            }
            let a = tuple::tuple_items(&*args);
            if a.len() != 2 {
                raise!(TypeError, "GenericAlias expected 2 arguments, got {}", a.len());
            }
            new_alias(tp, a[0], a[1])
        })();
        into_c(r)
    }
}
unsafe extern "C" fn ga_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let g = &*(o as *mut GenericAlias);
        xdecref(g.origin);
        xdecref(g.args);
        xdecref(g.parameters);
        let tp = (*o).type_ptr();
        generic_free_and_type_decref(o, tp);
    }
}
unsafe extern "C" fn ga_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let g = &*(o as *mut GenericAlias);
        let f = visit.unwrap();
        for p in [g.origin, g.args, g.parameters] {
            if !p.is_null() {
                let r = f(p, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        0
    }
}
fn repr_item(o: &PyObject) -> PyResult<String> {
    if o.is(none::ellipsis()) {
        return Ok("...".into());
    }
    if o.is_type(&raw mut GENERIC_ALIAS_TYPE) {
        return ab::repr_string(o);
    }
    if crate::types::list::is_list(o) {
        let parts: PyResult<Vec<String>> = crate::types::list::list_to_vec(o).iter().map(|x| repr_item(x)).collect();
        return Ok(format!("[{}]", parts?.join(", ")));
    }
    let q = ab::getattr_opt(o, crate::name!("__qualname__"))?;
    let m = ab::getattr_opt(o, crate::name!("__module__"))?;
    match (q, m) {
        (Some(q), Some(m)) if ab::getattr_opt(o, crate::name!("__origin__"))?.is_none() => {
            let ms = ab::str_string(&m)?;
            let qs = ab::str_string(&q)?;
            if ms == "builtins" { Ok(qs) } else { Ok(format!("{}.{}", ms, qs)) }
        }
        _ => ab::repr_string(o),
    }
}
unsafe extern "C" fn ga_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let g = ga(&*o);
        let r = (|| -> PyResult {
            let mut s = repr_item(&*g.origin)?;
            s.push('[');
            let items = tuple::tuple_items(&*g.args);
            if items.is_empty() {
                s.push_str("()");
            }
            for (i, a) in items.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&repr_item(a)?);
            }
            s.push(']');
            Ok(pystr::new_str(&s))
        })();
        into_c(r)
    }
}
unsafe extern "C" fn ga_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !b.is_type(&raw mut GENERIC_ALIAS_TYPE) || (op != Py_EQ && op != Py_NE) {
            return none::not_implemented().to_ref().into_raw();
        }
        let r = (|| -> PyResult<bool> {
            let (x, y) = (ga(a), ga(b));
            Ok(ab::rich_compare_bool(&*x.origin, &*y.origin, Py_EQ)? && ab::rich_compare_bool(&*x.args, &*y.args, Py_EQ)?)
        })();
        into_c(r.map(|e| bool_::new_bool(if op == Py_EQ { e } else { !e })))
    }
}
unsafe extern "C" fn ga_hash(o: *mut PyObject) -> Py_hash_t {
    unsafe {
        let g = ga(&*o);
        match (ab::hash(&*g.origin), ab::hash(&*g.args)) {
            (Ok(a), Ok(b)) => {
                let h = a ^ b;
                if h == -1 { -2 } else { h }
            }
            _ => -1,
        }
    }
}
unsafe extern "C" fn ga_call(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let g = ga(&*o);
        let r = (|| -> PyResult {
            let obj = ab::call_tp_call(&*g.origin, &*args, if kw.is_null() { None } else { Some(&*kw) })?;
            if let Err(_) = ab::setattr(&obj, crate::name!("__orig_class__"), &*o) {
                if !exc_matches(crate::types::exceptions::AttributeError()) && !exc_matches(crate::types::exceptions::TypeError()) {
                    return Err(PyErr);
                }
                clear_exc();
            }
            Ok(obj)
        })();
        into_c(r)
    }
}
const EXCEPTIONS: &[&str] = &[
    "__class__", "__bases__", "__origin__", "__args__", "__unpacked__", "__parameters__", "__typing_unpacked_tuple_args__",
    "__mro_entries__", "__reduce_ex__", "__reduce__", "__copy__", "__deepcopy__",
];
unsafe extern "C" fn ga_getattro(o: *mut PyObject, n: *mut PyObject) -> *mut PyObject {
    unsafe {
        let name = pystr::to_string_lossy(&*n);
        if pystr::is_str(&*n) && !EXCEPTIONS.contains(&name.as_str()) {
            return into_c(ab::getattr(&*ga(&*o).origin, &*n));
        }
        object_generic_getattro(o, n)
    }
}
fn origin_g(o: &PyObject) -> PyResult {
    Ok(unsafe { &*ga(o).origin }.to_ref())
}
fn args_g(o: &PyObject) -> PyResult {
    Ok(unsafe { &*ga(o).args }.to_ref())
}
fn params_g(o: &PyObject) -> PyResult {
    let g = unsafe { &mut *(o.as_ptr() as *mut GenericAlias) };
    if g.parameters.is_null() {
        g.parameters = collect_params(unsafe { &*g.args })?.into_raw();
    }
    Ok(unsafe { &*g.parameters }.to_ref())
}
fn mro_entries_m(o: &PyObject, _bases: &PyObject) -> PyResult {
    Ok(tuple::new_tuple(vec![unsafe { &*ga(o).origin }.to_ref()]))
}
fn instancecheck_m(_o: &PyObject, _x: &PyObject) -> PyResult {
    raise!(TypeError, "isinstance() argument 2 cannot be a parameterized generic");
}
fn subclasscheck_m(_o: &PyObject, _x: &PyObject) -> PyResult {
    raise!(TypeError, "issubclass() argument 2 cannot be a parameterized generic");
}
fn reduce_m(o: &PyObject) -> PyResult {
    let g = ga(o);
    Ok(tuple::new_tuple(vec![
        type_ref(o.type_ptr()),
        tuple::new_tuple(vec![unsafe { &*g.origin }.to_ref(), unsafe { &*g.args }.to_ref()]),
    ]))
}
fn dir_m(o: &PyObject) -> PyResult {
    let d = ab::dir(unsafe { &*ga(o).origin })?;
    Ok(d)
}
fn getitem_m(o: &PyObject, item: &PyObject) -> PyResult {
    let params = params_g(o)?;
    let n = tuple::tuple_len(&params);
    if n == 0 {
        raise!(TypeError, "{} is not a generic class", ab::repr_string(o)?);
    }
    let items: Vec<PyRef> = if tuple::is_tuple(item) { tuple::tuple_to_vec(item) } else { vec![item.to_ref()] };
    if items.len() != n {
        raise!(TypeError, "Too {} arguments for {}; actual {}, expected {}", if items.len() > n { "many" } else { "few" }, ab::repr_string(o)?, items.len(), n);
    }
    let g = ga(o);
    let mut newargs = Vec::new();
    for a in tuple::tuple_items(unsafe { &*g.args }) {
        match tuple::tuple_items(&params).iter().position(|p| p.is(a)) {
            Some(i) => newargs.push(items[i].clone()),
            None => newargs.push(a.to_ref()),
        }
    }
    new_alias(o.type_ptr(), unsafe { &*g.origin }, &tuple::new_tuple(newargs))
}
unsafe extern "C" fn ga_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(getitem_m(&*o, &*k)) }
}

static mut GA_MAP: PyMappingMethods = PyMappingMethods::ZERO;

pub fn init_types() {
    unsafe {
        let t = &mut *(&raw mut GENERIC_ALIAS_TYPE);
        t.tp_name = c"types.GenericAlias".as_ptr();
        t.tp_basicsize = std::mem::size_of::<GenericAlias>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_BASETYPE;
        t.tp_dealloc = Some(ga_dealloc);
        t.tp_traverse = Some(ga_traverse);
        t.tp_repr = Some(ga_repr);
        t.tp_richcompare = Some(ga_richcompare);
        t.tp_hash = Some(ga_hash);
        t.tp_call = Some(ga_call);
        t.tp_getattro = Some(ga_getattro);
        t.tp_new = Some(ga_new);
        t.tp_alloc = Some(generic_alloc);
        t.tp_free = Some(generic_free);
        t.tp_doc = c"GenericAlias(origin, args, /)\n--\n\nRepresent a PEP 585 generic type\n\nE.g. for t = list[int], t.__origin__ is list and t.__args__ is (int,).".as_ptr();
        let m = &mut *(&raw mut GA_MAP);
        m.mp_subscript = Some(ga_subscript);
        t.tp_as_mapping = m;
        t.tp_getset = crate::getset_table![("__origin__", origin_g), ("__args__", args_g), ("__parameters__", params_g)];
        t.tp_methods = crate::mdefs![
            ("__mro_entries__", O, mro_entries_m),
            ("__instancecheck__", O, instancecheck_m),
            ("__subclasscheck__", O, subclasscheck_m),
            ("__reduce__", NOARGS, reduce_m),
            ("__dir__", NOARGS, dir_m),
        ];
    }
}
pub fn ready() -> PyResult<()> {
    type_ready(&raw mut GENERIC_ALIAS_TYPE)
}
