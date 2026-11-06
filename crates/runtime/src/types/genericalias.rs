//! types.GenericAlias (PEP 585): `list[int]` etc., parameter substitution helpers shared
//! with UnionType / typing objects (`make_parameters`, `subs_parameters`), the starred
//! iterator (`*tuple[int]`), and `__class_getitem__` on builtin types.
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::lang_util::{self as lu, field, set_field};
use crate::types::typeobject::*;
use crate::types::{bool_, list, none, str as pystr, tuple};
use crate::{name, raise};

#[repr(C)]
pub struct GaObject {
    pub ob_base: PyObject,
    pub origin: *mut PyObject,
    pub args: *mut PyObject,
    pub parameters: *mut PyObject,
    pub starred: *mut PyObject, // bool object (True/False) or null
    pub weakreflist: *mut PyObject,
}
const F_ORIGIN: usize = 0;
const F_ARGS: usize = 1;
const F_PARAMS: usize = 2;
const F_STARRED: usize = 3;

#[unsafe(no_mangle)]
pub static mut Py_GenericAliasType: PyTypeObject = PyTypeObject::ZERO;
pub static mut GA_ITER_TYPE: PyTypeObject = PyTypeObject::ZERO;

pub fn ga_type() -> *mut PyTypeObject {
    &raw mut Py_GenericAliasType
}
pub fn is_generic_alias(o: &PyObject) -> bool {
    is_subtype(o.type_ptr(), ga_type())
}
fn starred(o: &PyObject) -> bool {
    field(o, F_STARRED).is_some_and(|b| b.is(bool_::true_obj()))
}
pub fn ga_origin(o: &PyObject) -> PyRef {
    lu::field_ref(o, F_ORIGIN).unwrap_or_else(none::none_ref)
}
pub fn ga_args(o: &PyObject) -> PyRef {
    lu::field_ref(o, F_ARGS).unwrap_or_else(tuple::empty_tuple)
}

fn setup(o: &PyObject, origin: &PyObject, args: &PyObject, star: bool) {
    let args = if tuple::is_tuple(args) { args.to_ref() } else { tuple::new_tuple(vec![args.to_ref()]) };
    set_field(o, F_ORIGIN, Some(origin.to_ref()));
    set_field(o, F_ARGS, Some(args));
    set_field(o, F_PARAMS, None);
    set_field(o, F_STARRED, Some(bool_::new_bool(star)));
}

/// `Py_GenericAlias(origin, args)`.
pub fn new_generic_alias(origin: &PyObject, args: &PyObject) -> PyResult {
    let o = lu::alloc(ga_type())?;
    setup(&o, origin, args, false);
    Ok(o)
}
fn new_starred(origin: &PyObject, args: &PyObject) -> PyResult {
    let o = lu::alloc(ga_type())?;
    setup(&o, origin, args, true);
    Ok(o)
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
            let o = lu::alloc(tp)?;
            setup(&o, a[0], a[1], false);
            Ok(o)
        })();
        into_c(r)
    }
}

// ---------------------------------------------------------------- repr

fn repr_item(p: &PyObject, out: &mut String) -> PyResult<()> {
    if p.is(none::ellipsis()) {
        out.push_str("...");
        return Ok(());
    }
    if p.is_type(ga_type()) {
        out.push_str(&ab::repr_string(p)?);
        return Ok(());
    }
    if ab::getattr_opt(p, name!("__origin__"))?.is_some() && ab::getattr_opt(p, name!("__args__"))?.is_some() {
        out.push_str(&ab::repr_string(p)?);
        return Ok(());
    }
    let qualname = match ab::getattr_opt(p, name!("__qualname__"))? {
        Some(q) => q,
        None => {
            out.push_str(&ab::repr_string(p)?);
            return Ok(());
        }
    };
    let module = match ab::getattr_opt(p, name!("__module__"))? {
        Some(m) if !m.is(none::none()) => m,
        _ => {
            out.push_str(&ab::repr_string(p)?);
            return Ok(());
        }
    };
    if pystr::is_str(&module) && pystr::str_eq_ascii(&module, "builtins") {
        out.push_str(&ab::str_string(&qualname)?);
    } else {
        out.push_str(&format!("{}.{}", ab::str_string(&module)?, ab::str_string(&qualname)?));
    }
    Ok(())
}

/// Repr of a type-ish object as used inside `X[...]` reprs (also used by UnionType).
pub fn type_repr(p: &PyObject) -> PyResult<String> {
    let mut s = String::new();
    repr_item(p, &mut s)?;
    Ok(s)
}

unsafe extern "C" fn ga_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(ga_repr_impl(&*o).map(|s| pystr::new_str(&s))) }
}

fn ga_repr_impl(o: &PyObject) -> PyResult<String> {
    let mut s = String::new();
    if starred(o) {
        s.push('*');
    }
    repr_item(&ga_origin(o), &mut s)?;
    s.push('[');
    let args = ga_args(o);
    let items = tuple::tuple_items(&args);
    if items.is_empty() {
        s.push_str("()");
    }
    for (i, a) in items.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        if list::is_list(a) {
            repr_list_checked(a, &mut s)?;
        } else {
            repr_item(a, &mut s)?;
        }
    }
    s.push(']');
    Ok(s)
}

fn repr_list_checked(l: &PyObject, out: &mut String) -> PyResult<()> {
    // Index-based iteration that raises IndexError if the list shrinks while formatting
    // (matches CPython's behaviour with a list mutated by a __repr__).
    let n0 = list::list_len(l);
    if n0 == 0 {
        out.push_str("[]");
        return Ok(());
    }
    out.push('[');
    for i in 0..n0 {
        let item = match list::get(l, i) {
            Some(x) => x,
            None => raise!(IndexError, "list index out of range"),
        };
        if i > 0 {
            out.push_str(", ");
        }
        repr_item(&item, out)?;
    }
    out.push(']');
    Ok(())
}

// ---------------------------------------------------------------- parameters

fn tuple_index(params: &[PyRef], x: &PyObject) -> Option<usize> {
    params.iter().position(|p| p.is(x))
}

/// `_Py_make_parameters`: collect the type variables in `args` (in order, deduplicated).
pub fn make_parameters(args: &PyObject) -> PyResult {
    let items: Vec<PyRef> = if tuple::is_tuple(args) { tuple::tuple_to_vec(args) } else { list::list_to_vec(args) };
    let mut params: Vec<PyRef> = Vec::new();
    for t in &items {
        if is_type(t) {
            continue;
        }
        if ab::getattr_opt(t, name!("__typing_subst__"))?.is_some() {
            if tuple_index(&params, t).is_none() {
                params.push(t.clone());
            }
            continue;
        }
        let mut sub = ab::getattr_opt(t, name!("__parameters__"))?;
        if sub.is_none() && (tuple::is_tuple(t) || list::is_list(t)) {
            // nested lists/tuples recurse (core-runtime: native stack guard)
            let ts = crate::threads::current();
            crate::interp::enter_recursive_call(ts, " in __parameter__ calculation")?;
            let r = make_parameters(t);
            crate::interp::leave_recursive_call(ts);
            sub = Some(r?);
        }
        if let Some(s) = sub {
            if tuple::is_tuple(&s) {
                for x in tuple::tuple_items(&s) {
                    if tuple_index(&params, x).is_none() {
                        params.push(x.to_ref());
                    }
                }
            }
        }
    }
    Ok(tuple::new_tuple(params))
}

/// Expand unpacked fixed-length tuples (`*tuple[int, str]`) in a subscription argument.
fn unpack_args(item: &PyObject) -> PyResult<Vec<PyRef>> {
    let items: Vec<PyRef> = if tuple::is_tuple(item) { tuple::tuple_to_vec(item) } else { vec![item.to_ref()] };
    let mut out = Vec::new();
    for it in items {
        if !is_type(&it) {
            if let Some(sub) = ab::getattr_opt(&it, name!("__typing_unpacked_tuple_args__"))? {
                if tuple::is_tuple(&sub) {
                    let si = tuple::tuple_items(&sub);
                    if si.is_empty() || !si[si.len() - 1].is(none::ellipsis()) {
                        out.extend(si.iter().map(|x| x.to_ref()));
                        continue;
                    }
                }
            }
        }
        out.push(it);
    }
    Ok(out)
}

fn is_unpacked_typevartuple(arg: &PyObject) -> PyResult<bool> {
    if is_type(arg) {
        return Ok(false);
    }
    match ab::getattr_opt(arg, name!("__typing_is_unpacked_typevartuple__"))? {
        Some(v) => ab::is_true(&v),
        None => Ok(false),
    }
}

fn subs_tvars(obj: &PyObject, params: &[PyRef], argitems: &[PyRef]) -> PyResult {
    if tuple::is_tuple(obj) || list::is_list(obj) {
        let items: Vec<PyRef> = if tuple::is_tuple(obj) { tuple::tuple_to_vec(obj) } else { list::list_to_vec(obj) };
        let mut out = Vec::with_capacity(items.len());
        for it in items {
            out.push(subs_one(&it, params, argitems)?);
        }
        return Ok(if tuple::is_tuple(obj) { tuple::new_tuple(out) } else { list::new_list(out) });
    }
    let sub = ab::getattr_opt(obj, name!("__parameters__"))?;
    if let Some(sub) = sub {
        if tuple::is_tuple(&sub) && tuple::tuple_len(&sub) > 0 {
            let mut subargs: Vec<PyRef> = Vec::new();
            for a in tuple::tuple_items(&sub) {
                if let Some(ip) = tuple_index(params, a) {
                    let param = &params[ip];
                    let arg = &argitems[ip];
                    if param.ty().tp_iter.is_some() && tuple::is_tuple(arg) {
                        subargs.extend(tuple::tuple_to_vec(arg));
                        continue;
                    }
                    subargs.push(arg.clone());
                } else {
                    subargs.push(a.to_ref());
                }
            }
            let t = tuple::new_tuple(subargs);
            return ab::getitem(obj, &t);
        }
    }
    Ok(obj.to_ref())
}

fn subs_one(arg: &PyObject, params: &[PyRef], argitems: &[PyRef]) -> PyResult {
    if is_type(arg) {
        return Ok(arg.to_ref());
    }
    if let Some(subst) = ab::getattr_opt(arg, name!("__typing_subst__"))? {
        if let Some(ip) = tuple_index(params, arg) {
            return ab::call(&subst, &[&argitems[ip]]);
        }
        // __parameters__ was computed when the alias was created; an argument
        // that became substitutable later is not one of them (grind-lib)
        raise!(TypeError, "{} not found in __parameters__", crate::abstract_::repr_string(arg)?);
    }
    subs_tvars(arg, params, argitems)
}

/// `_Py_subs_parameters(self, args, parameters, item)`: the new args tuple.
pub fn subs_parameters(slf: &PyObject, args: &PyObject, parameters: &PyObject, item: &PyObject) -> PyResult {
    let params = tuple::tuple_to_vec(parameters);
    let nparams = params.len();
    if nparams == 0 {
        raise!(TypeError, "{} is not a generic class", ab::repr_string(slf)?);
    }
    let mut item = tuple::new_tuple(unpack_args(item)?);
    for p in &params {
        if let Some(prep) = ab::getattr_opt(p, name!("__typing_prepare_subst__"))? {
            let r = ab::call(&prep, &[slf, &item])?;
            item = r;
        }
    }
    let argitems: Vec<PyRef> = if tuple::is_tuple(&item) { tuple::tuple_to_vec(&item) } else { vec![item.clone()] };
    if argitems.len() != nparams {
        raise!(
            TypeError,
            "Too {} arguments for {}; actual {}, expected {}",
            if argitems.len() > nparams { "many" } else { "few" },
            ab::repr_string(slf)?,
            argitems.len(),
            nparams
        );
    }
    let mut newargs = Vec::new();
    for arg in tuple::tuple_items(args) {
        if is_type(arg) {
            newargs.push(arg.to_ref());
            continue;
        }
        let unpack = is_unpacked_typevartuple(arg)?;
        let r = subs_one(arg, &params, &argitems)?;
        if unpack {
            if !tuple::is_tuple(&r) {
                raise!(TypeError, "expected __typing_subst__ of an unpacked TypeVarTuple to return a tuple, not {}", tname(&r));
            }
            newargs.extend(tuple::tuple_to_vec(&r));
        } else {
            newargs.push(r);
        }
    }
    Ok(tuple::new_tuple(newargs))
}

fn ga_parameters(o: &PyObject) -> PyResult {
    if let Some(p) = lu::field_ref(o, F_PARAMS) {
        return Ok(p);
    }
    let p = make_parameters(&ga_args(o))?;
    set_field(o, F_PARAMS, Some(p.clone()));
    Ok(p)
}

unsafe extern "C" fn ga_getitem(o: *mut PyObject, item: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        let r = (|| -> PyResult {
            let params = ga_parameters(o)?;
            let newargs = subs_parameters(o, &ga_args(o), &params, &*item)?;
            let res = new_generic_alias(&ga_origin(o), &newargs)?;
            set_field(&res, F_STARRED, Some(bool_::new_bool(starred(o))));
            Ok(res)
        })();
        into_c(r)
    }
}

// ---------------------------------------------------------------- protocol slots

const ATTR_EXCEPTIONS: &[&str] = &[
    "__class__",
    "__bases__",
    "__origin__",
    "__args__",
    "__unpacked__",
    "__parameters__",
    "__typing_unpacked_tuple_args__",
    "__mro_entries__",
    "__reduce_ex__",
    "__reduce__",
    "__copy__",
    "__deepcopy__",
];

unsafe extern "C" fn ga_getattro(o: *mut PyObject, nm: *mut PyObject) -> *mut PyObject {
    unsafe {
        let n = &*nm;
        if pystr::is_str(n) {
            let s = pystr::to_string_lossy(n);
            if !ATTR_EXCEPTIONS.contains(&s.as_str()) {
                return into_c(ab::getattr(&ga_origin(&*o), n));
            }
        }
        object_generic_getattro(o, nm)
    }
}

unsafe extern "C" fn ga_hash(o: *mut PyObject) -> Py_hash_t {
    unsafe {
        let o = &*o;
        let r = (|| -> PyResult<isize> {
            let h0 = ab::hash(&ga_origin(o))?;
            let h1 = ab::hash(&ga_args(o))?;
            let h = h0 ^ h1;
            Ok(if h == -1 { -2 } else { h })
        })();
        r.unwrap_or(-1)
    }
}

unsafe extern "C" fn ga_richcompare(a: *mut PyObject, b: *mut PyObject, op: std::ffi::c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !is_generic_alias(b) || (op != Py_EQ && op != Py_NE) {
            return none::not_implemented().to_ref().into_raw();
        }
        let r = (|| -> PyResult<bool> {
            if starred(a) != starred(b) {
                return Ok(false);
            }
            if !ab::rich_eq(&ga_origin(a), &ga_origin(b))? {
                return Ok(false);
            }
            ab::rich_eq(&ga_args(a), &ga_args(b))
        })();
        into_c(r.map(|eq| bool_::new_bool(if op == Py_EQ { eq } else { !eq })))
    }
}

unsafe extern "C" fn ga_call(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        let r = (|| -> PyResult {
            let origin = ga_origin(o);
            let obj = ab::call_tp_call(&origin, &*args, if kw.is_null() { None } else { Some(&*kw) })?;
            if let Err(e) = ab::setattr(&obj, name!("__orig_class__"), o) {
                if !exc_matches(crate::types::exceptions::AttributeError()) && !exc_matches(crate::types::exceptions::TypeError()) {
                    return Err(e);
                }
                clear_exc();
            }
            Ok(obj)
        })();
        into_c(r)
    }
}

unsafe extern "C" fn ga_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(new_ga_iter(Some(&*o))) }
}

fn new_ga_iter(obj: Option<&PyObject>) -> PyResult {
    let it = lu::alloc(&raw mut GA_ITER_TYPE)?;
    set_field(&it, 0, obj.map(|x| x.to_ref()));
    Ok(it)
}

unsafe extern "C" fn gaiter_next(it: *mut PyObject) -> *mut PyObject {
    unsafe {
        let it = &*it;
        let obj = match lu::field_ref(it, 0) {
            Some(o) => o,
            None => return std::ptr::null_mut(),
        };
        set_field(it, 0, None);
        into_c(new_starred(&ga_origin(&obj), &ga_args(&obj)))
    }
}

fn gaiter_reduce(it: &PyObject) -> PyResult {
    let iter = crate::builtins::builtin("iter")?;
    match lu::field_ref(it, 0) {
        Some(o) => Ok(tuple::new_tuple(vec![iter, tuple::new_tuple(vec![o])])),
        None => Ok(tuple::new_tuple(vec![iter, tuple::new_tuple(vec![tuple::empty_tuple()])])),
    }
}

fn ga_mro_entries(o: &PyObject, _bases: &PyObject) -> PyResult {
    Ok(tuple::new_tuple(vec![ga_origin(o)]))
}
fn ga_instancecheck(_o: &PyObject, _x: &PyObject) -> PyResult {
    raise!(TypeError, "isinstance() argument 2 cannot be a parameterized generic");
}
fn ga_subclasscheck(_o: &PyObject, _x: &PyObject) -> PyResult {
    raise!(TypeError, "issubclass() argument 2 cannot be a parameterized generic");
}
fn ga_reduce(o: &PyObject) -> PyResult {
    if starred(o) {
        let tmp = new_generic_alias(&ga_origin(o), &ga_args(o))?;
        let it = ab::get_iter(&tmp)?;
        let next = crate::builtins::builtin("next")?;
        return Ok(tuple::new_tuple(vec![next, tuple::new_tuple(vec![it])]));
    }
    Ok(tuple::new_tuple(vec![type_ref(o.type_ptr()), tuple::new_tuple(vec![ga_origin(o), ga_args(o)])]))
}
fn ga_dir(o: &PyObject) -> PyResult {
    let d = ab::dir(&ga_origin(o))?;
    let l = ab::sequence_list(&d)?;
    for nm in ATTR_EXCEPTIONS {
        let n = pystr::new_str(nm);
        if ab::contains(&l, &n)? {
            continue;
        }
        if generic_getattr_opt(o, &n, None)?.is_some() {
            list::append(&l, n);
        }
    }
    Ok(l)
}

fn get_origin(o: &PyObject) -> PyResult {
    Ok(ga_origin(o))
}
fn get_args(o: &PyObject) -> PyResult {
    Ok(ga_args(o))
}
fn get_parameters(o: &PyObject) -> PyResult {
    ga_parameters(o)
}
fn get_unpacked(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(starred(o)))
}
fn get_unpacked_tuple_args(o: &PyObject) -> PyResult {
    let origin = ga_origin(o);
    if starred(o) && origin.is(type_obj(tuple::tuple_type())) {
        return Ok(ga_args(o));
    }
    Ok(none::none_ref())
}

unsafe extern "C" fn ga_or(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(crate::types::union::union_or(&*a, &*b)) }
}

pub fn init_types() {
    unsafe {
        let t = ga_type();
        lu::setup_ptr_type(t, "types.GenericAlias", std::mem::size_of::<GaObject>(), Py_TPFLAGS_BASETYPE);
        let tt = &mut *t;
        tt.tp_doc = c"GenericAlias(origin, args, /)\n--\n\nRepresent a PEP 585 generic type\n\nE.g. for t = list[int], t.__origin__ is list and t.__args__ is (int,).".as_ptr();
        tt.tp_weaklistoffset = std::mem::offset_of!(GaObject, weakreflist) as isize;
        tt.tp_new = Some(ga_new);
        tt.tp_repr = Some(ga_repr);
        tt.tp_hash = Some(ga_hash);
        tt.tp_richcompare = Some(ga_richcompare);
        tt.tp_call = Some(ga_call);
        tt.tp_iter = Some(ga_iter);
        tt.tp_getattro = Some(ga_getattro);
        static mut GA_MAP: PyMappingMethods = PyMappingMethods::ZERO;
        (*(&raw mut GA_MAP)).mp_subscript = Some(ga_getitem);
        tt.tp_as_mapping = &raw mut GA_MAP;
        static mut GA_NUM: PyNumberMethods = PyNumberMethods::ZERO;
        (*(&raw mut GA_NUM)).nb_or = Some(ga_or);
        tt.tp_as_number = &raw mut GA_NUM;
        tt.tp_methods = crate::mdefs![
            ("__mro_entries__", O, ga_mro_entries),
            ("__instancecheck__", O, ga_instancecheck),
            ("__subclasscheck__", O, ga_subclasscheck),
            ("__reduce__", NOARGS, ga_reduce),
            ("__dir__", NOARGS, ga_dir),
        ];
        tt.tp_getset = crate::getset_table![
            ("__origin__", get_origin),
            ("__args__", get_args),
            ("__parameters__", get_parameters; "Type variables in the GenericAlias."),
            ("__unpacked__", get_unpacked),
            ("__typing_unpacked_tuple_args__", get_unpacked_tuple_args),
        ];

        let it = &raw mut GA_ITER_TYPE;
        lu::setup_ptr_type(it, "generic_alias_iterator", std::mem::size_of::<PyObject>() + 8, 0);
        (*it).tp_iter = Some(self_iter);
        (*it).tp_iternext = Some(gaiter_next);
        (*it).tp_methods = crate::mdefs![("__reduce__", NOARGS, gaiter_reduce)];
    }
}

unsafe extern "C" fn self_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}

pub fn ready() -> PyResult<()> {
    lu::ready_type(ga_type())?;
    lu::ready_type(&raw mut GA_ITER_TYPE)?;
    Ok(())
}

fn class_getitem(cls: &PyObject, arg: &PyObject) -> PyResult {
    new_generic_alias(cls, arg)
}

/// Install `__class_getitem__ = classmethod(GenericAlias)` on a (ready) type.
pub fn add_class_getitem(tp: *mut PyTypeObject) {
    let ml = crate::mdefs![("__class_getitem__", O, class_getitem, "See PEP 585"; METH_CLASS)];
    lu::add_classmethod(tp, ml);
}

/// `__class_getitem__` for builtin types defined by other parts of the runtime, found by
/// name once the builtins namespace is complete (and for the type of `type.__dict__`).
pub fn register_late_class_getitems(builtins: &PyObject) {
    for n in ["enumerate", "memoryview", "frozendict", "reversed"] {
        if n == "reversed" {
            continue; // not generic in CPython
        }
        if let Some(t) = crate::types::dict::get_item_rs(builtins, n) {
            if is_type(&t) && crate::types::dict::get_item_rs(type_dict(as_type(&t)), "__class_getitem__").is_none() {
                add_class_getitem(as_type(&t));
            }
        }
    }
    // mappingproxy
    let tdict = unsafe { (*crate::types::typeobject::type_type()).tp_dict };
    if !tdict.is_null() {
        if let Ok(mp) = ab::getattr(type_obj(crate::types::typeobject::type_type()), name!("__dict__")) {
            let t = mp.type_ptr();
            if t != crate::types::dict::dict_type() && crate::types::dict::get_item_rs(type_dict(t), "__class_getitem__").is_none() {
                add_class_getitem(t);
            }
        }
    }
}

/// `__class_getitem__` for the core builtin types (called once everything is ready).
pub fn register_builtin_class_getitems() {
    use crate::types::*;
    for t in [
        tuple::tuple_type(),
        list::list_type(),
        dict::dict_type(),
        set::set_type(),
        set::frozenset_type(),
        slice::slice_type(),
        funcobject::staticmethod_type(),
        funcobject::classmethod_type(),
        genobject::gen_type(),
        genobject::coro_type(),
        genobject::asyncgen_type(),
    ] {
        add_class_getitem(t);
    }
}
