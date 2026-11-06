//! PEP 695/696/749 runtime objects of the `_typing` module: TypeVar, ParamSpec,
//! ParamSpecArgs/Kwargs, TypeVarTuple, TypeAliasType, Generic, NoDefault and the
//! `_ConstEvaluator` used by `evaluate_*` for eagerly given values. Substitution logic
//! that CPython implements in typing.py is called there (`typing._typevar_subst`, ...).
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::lang_util::{self as lu, field, field_ref, set_field};
use crate::types::typeobject::*;
use crate::types::{bool_, dict, int, none, str as pystr, tuple};
use crate::{name, raise};

/// Shared layout of TypeVar / ParamSpec / TypeVarTuple.
#[repr(C)]
pub struct TypeParamObject {
    pub ob_base: PyObject,
    pub fields: [*mut PyObject; 11],
    pub weakreflist: *mut PyObject,
}
const NAME: usize = 0;
const BOUND: usize = 1;
const EVAL_BOUND: usize = 2;
const CONSTRAINTS: usize = 3;
const EVAL_CONSTRAINTS: usize = 4;
const DEFAULT: usize = 5;
const EVAL_DEFAULT: usize = 6;
const COVARIANT: usize = 7;
const CONTRAVARIANT: usize = 8;
const INFER: usize = 9;
const DICT: usize = 10;

#[repr(C)]
pub struct OneField {
    pub ob_base: PyObject,
    pub value: *mut PyObject,
}

/// ParamSpecArgs / ParamSpecKwargs: one field plus a weak reference list.
#[repr(C)]
pub struct OneFieldWeak {
    pub ob_base: PyObject,
    pub value: *mut PyObject,
    pub weakreflist: *mut PyObject,
}

/// TypeAliasType layout.
#[repr(C)]
pub struct TypeAliasObject {
    pub ob_base: PyObject,
    pub fields: [*mut PyObject; 6],
    pub weakreflist: *mut PyObject,
}
const TA_NAME: usize = 0;
const TA_QUALNAME: usize = 1;
const TA_MODULE: usize = 2;
const TA_PARAMS: usize = 3;
const TA_COMPUTE: usize = 4;
const TA_VALUE: usize = 5;

pub static mut TYPEVAR_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut PARAMSPEC_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut TYPEVARTUPLE_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut PARAMSPECARGS_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut PARAMSPECKWARGS_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut TYPEALIAS_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut GENERIC_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut NODEFAULT_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut CONSTEVAL_TYPE: PyTypeObject = PyTypeObject::ZERO;
static mut NODEFAULT_OBJ: PyObject = unsafe { std::mem::zeroed() };

macro_rules! tp {
    ($s:ident) => {
        &raw mut $s
    };
}

pub fn typevar_type() -> *mut PyTypeObject {
    tp!(TYPEVAR_TYPE)
}
pub fn paramspec_type() -> *mut PyTypeObject {
    tp!(PARAMSPEC_TYPE)
}
pub fn typevartuple_type() -> *mut PyTypeObject {
    tp!(TYPEVARTUPLE_TYPE)
}
pub fn typealias_type() -> *mut PyTypeObject {
    tp!(TYPEALIAS_TYPE)
}
pub fn generic_type() -> *mut PyTypeObject {
    tp!(GENERIC_TYPE)
}
pub fn paramspecargs_type() -> *mut PyTypeObject {
    tp!(PARAMSPECARGS_TYPE)
}
pub fn paramspeckwargs_type() -> *mut PyTypeObject {
    tp!(PARAMSPECKWARGS_TYPE)
}
pub fn nodefault() -> &'static PyObject {
    unsafe { &*(&raw mut NODEFAULT_OBJ) }
}
pub fn is_typealias(o: &PyObject) -> bool {
    o.is_type(typealias_type())
}
pub fn is_typevartuple(o: &PyObject) -> bool {
    o.is_type(typevartuple_type())
}

fn typing_attr(n: &str) -> PyResult {
    lu::import_attr("typing", n)
}
fn call_typing(n: &str, args: &[&PyObject]) -> PyResult {
    let f = typing_attr(n)?;
    ab::call(&f, args)
}

/// `typing._type_check(arg, msg)`.
fn type_check(arg: &PyObject, msg: &str) -> PyResult {
    if arg.is(none::none()) {
        return Ok(type_ref(none::none().type_ptr()));
    }
    let m = pystr::new_str(msg);
    call_typing("_type_check", &[arg, &m])
}

/// The `__name__` of the module calling into us (None if unknown).
fn caller_module() -> PyRef {
    if let Some(g) = crate::interp::current_globals() {
        if dict::is_dict(&g) {
            if let Some(n) = dict::get_item_str(&g, name!("__name__")) {
                return n;
            }
        }
    }
    none::none_ref()
}

fn inst_dict(o: &PyObject) -> PyRef {
    if let Some(d) = field_ref(o, DICT) {
        return d;
    }
    let d = dict::new_dict();
    set_field(o, DICT, Some(d.clone()));
    d
}

fn set_module(o: &PyObject, m: PyRef) -> PyResult<()> {
    dict::set_item(&inst_dict(o), name!("__module__"), &m)
}

fn new_param(tp: *mut PyTypeObject, name: &PyObject) -> PyResult {
    let o = lu::alloc(tp)?;
    set_field(&o, NAME, Some(name.to_ref()));
    for i in [COVARIANT, CONTRAVARIANT, INFER] {
        set_field(&o, i, Some(bool_::new_bool(false)));
    }
    Ok(o)
}

fn flag(o: &PyObject, i: usize) -> bool {
    field(o, i).is_some_and(|b| b.is(bool_::true_obj()))
}

// ---------------------------------------------------------------- interpreter intrinsics

/// `TypeVar` from a `def f[T]` / `T: bound` / `T: (a, b)` type parameter. The bound /
/// constraints are lazily evaluated functions.
pub fn make_typevar(name: &PyObject, bound: Option<&PyObject>, constraints: Option<&PyObject>, infer: bool) -> PyResult {
    let o = new_param(typevar_type(), name)?;
    let _ = infer;
    set_field(&o, INFER, Some(bool_::new_bool(true)));
    if let Some(b) = bound {
        set_field(&o, EVAL_BOUND, Some(b.to_ref()));
    }
    if let Some(c) = constraints {
        set_field(&o, EVAL_CONSTRAINTS, Some(c.to_ref()));
    }
    set_module(&o, caller_module())?;
    Ok(o)
}
pub fn make_paramspec(name: &PyObject) -> PyResult {
    let o = new_param(paramspec_type(), name)?;
    set_field(&o, INFER, Some(bool_::new_bool(true)));
    set_module(&o, caller_module())?;
    Ok(o)
}
pub fn make_typevartuple(name: &PyObject) -> PyResult {
    let o = new_param(typevartuple_type(), name)?;
    set_field(&o, INFER, Some(bool_::new_bool(true)));
    set_module(&o, caller_module())?;
    Ok(o)
}
/// Lazily evaluated default of a type parameter.
pub fn set_default(tp: &PyObject, default: &PyObject) -> PyResult {
    set_field(tp, EVAL_DEFAULT, Some(default.to_ref()));
    Ok(tp.to_ref())
}

/// Replace TypeVarTuples by `Unpack[tvt]`.
fn unpack_typevartuples(params: &PyObject) -> PyResult {
    let items = tuple::tuple_items(params);
    if !items.iter().any(|p| is_typevartuple(p)) {
        return Ok(params.to_ref());
    }
    let unpack = typing_attr("Unpack")?;
    let mut out = Vec::with_capacity(items.len());
    for p in items {
        if is_typevartuple(p) {
            out.push(ab::getitem(&unpack, p)?);
        } else {
            out.push(p.to_ref());
        }
    }
    Ok(tuple::new_tuple(out))
}

/// `Generic[*params]` for `class C[T]: ...`.
pub fn subscript_generic(params: &PyObject) -> PyResult {
    let params = unpack_typevartuples(params)?;
    call_typing("_GenericAlias", &[type_obj(generic_type()), &params])
}

/// TypeAliasType from the `type X[...] = value` statement: args = (name, type_params, compute_value).
pub fn make_typealias(args: &PyObject) -> PyResult {
    let a = tuple::tuple_items(args);
    let (nm, params, compute) = (a[0], a[1], a[2]);
    let params = if params.is(none::none()) { None } else { Some(params) };
    let module = match ab::getattr_opt(compute, name!("__module__"))? {
        Some(m) => m,
        None => none::none_ref(),
    };
    let mut qualname = nm.to_ref();
    if let Some(q) = ab::getattr_opt(compute, name!("__qualname__"))? {
        if pystr::is_str(&q) {
            // the evaluation function lives in the "<generic parameters of X>" scope for
            // generic aliases; the alias itself does not.
            let s = pystr::to_string_lossy(&q);
            let parts: Vec<&str> = s.split('.').filter(|p| !p.starts_with("<generic parameters of ")).collect();
            qualname = pystr::new_str(&parts.join("."));
        }
    }
    new_typealias(nm, &qualname, params, None, Some(compute), module)
}

fn new_typealias(
    nm: &PyObject,
    qualname: &PyObject,
    params: Option<&PyObject>,
    value: Option<&PyObject>,
    compute: Option<&PyObject>,
    module: PyRef,
) -> PyResult {
    let o = lu::alloc(typealias_type())?;
    set_field(&o, TA_NAME, Some(nm.to_ref()));
    set_field(&o, TA_QUALNAME, Some(qualname.to_ref()));
    set_field(&o, TA_MODULE, Some(module));
    set_field(&o, TA_PARAMS, params.map(|p| p.to_ref()));
    set_field(&o, TA_VALUE, value.map(|v| v.to_ref()));
    set_field(&o, TA_COMPUTE, compute.map(|c| c.to_ref()));
    Ok(o)
}

// ---------------------------------------------------------------- constructor helpers

struct Kw<'a> {
    names: &'a [&'a str],
    vals: Vec<Option<PyRef>>,
}
fn parse_kw<'a>(fname: &str, names: &'a [&'a str], kw: *mut PyObject) -> PyResult<Kw<'a>> {
    let mut vals = vec![None; names.len()];
    if !kw.is_null() {
        for (k, v) in dict::items_vec(unsafe { &*kw }) {
            let ks = pystr::to_string_lossy(&k);
            match names.iter().position(|n| *n == ks) {
                Some(i) => vals[i] = Some(v),
                None => raise!(TypeError, "{}() got an unexpected keyword argument '{}'", fname, ks),
            }
        }
    }
    Ok(Kw { names, vals })
}
impl<'a> Kw<'a> {
    fn get(&self, n: &str) -> Option<PyRef> {
        let i = self.names.iter().position(|x| *x == n).unwrap();
        self.vals[i].clone()
    }
    fn take_pos(&mut self, n: &str, v: Option<&PyObject>, fname: &str, pos: usize) -> PyResult<()> {
        if let Some(v) = v {
            let i = self.names.iter().position(|x| *x == n).unwrap();
            if self.vals[i].is_some() {
                raise!(TypeError, "argument for {}() given by name ('{}') and position ({})", fname, n, pos);
            }
            self.vals[i] = Some(v.to_ref());
        }
        Ok(())
    }
}
fn truthy(v: Option<PyRef>) -> PyResult<bool> {
    match v {
        Some(v) => ab::is_true(&v),
        None => Ok(false),
    }
}
fn check_name(fname: &str, v: Option<PyRef>) -> PyResult<PyRef> {
    match v {
        None => raise!(TypeError, "{}() missing required argument 'name' (pos 1)", fname),
        Some(n) if pystr::is_str(&n) => Ok(n),
        Some(n) => raise!(TypeError, "{}() argument 'name' must be str, not {}", fname, tname(&n)),
    }
}

unsafe extern "C" fn typevar_new(_tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(typevar_new_impl(&*args, kw)) }
}
fn typevar_new_impl(args: &PyObject, kw: *mut PyObject) -> PyResult {
    let names = ["name", "bound", "covariant", "contravariant", "infer_variance", "default"];
    let mut k = parse_kw("TypeVar", &names, kw)?;
    let a = tuple::tuple_items(args);
    k.take_pos("name", a.first().copied(), "TypeVar", 1)?;
    let nm = check_name("TypeVar", k.get("name"))?;
    let constraints: Vec<PyRef> = a.iter().skip(1).map(|x| x.to_ref()).collect();
    let cov = truthy(k.get("covariant"))?;
    let contra = truthy(k.get("contravariant"))?;
    let infer = truthy(k.get("infer_variance"))?;
    if cov && contra {
        raise!(ValueError, "Bivariant type variables are not supported.");
    }
    if infer && (cov || contra) {
        raise!(ValueError, "Variance cannot be specified with infer_variance.");
    }
    let mut bound = k.get("bound").filter(|b| !b.is(none::none()));
    if let Some(b) = &bound {
        bound = Some(type_check(b, "Bound must be a type.")?);
    }
    if !constraints.is_empty() && bound.is_some() {
        raise!(TypeError, "Constraints cannot be combined with bound=...");
    }
    if constraints.len() == 1 {
        raise!(TypeError, "A single constraint is not allowed");
    }
    let mut checked = Vec::new();
    for c in &constraints {
        checked.push(type_check(c, "TypeVar(name, constraint, ...): constraints must be types.")?);
    }
    let o = new_param(typevar_type(), &nm)?;
    set_field(&o, BOUND, bound);
    if !checked.is_empty() {
        set_field(&o, CONSTRAINTS, Some(tuple::new_tuple(checked)));
    }
    set_field(&o, COVARIANT, Some(bool_::new_bool(cov)));
    set_field(&o, CONTRAVARIANT, Some(bool_::new_bool(contra)));
    set_field(&o, INFER, Some(bool_::new_bool(infer)));
    if let Some(d) = k.get("default") {
        if !d.is(nodefault()) {
            set_field(&o, DEFAULT, Some(d));
        }
    }
    set_module(&o, caller_module())?;
    Ok(o)
}

unsafe extern "C" fn paramspec_new(_tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(paramspec_new_impl(&*args, kw)) }
}
fn paramspec_new_impl(args: &PyObject, kw: *mut PyObject) -> PyResult {
    let names = ["name", "bound", "covariant", "contravariant", "infer_variance", "default"];
    let mut k = parse_kw("ParamSpec", &names, kw)?;
    let a = tuple::tuple_items(args);
    if a.len() > 1 {
        raise!(TypeError, "ParamSpec() takes at most 1 positional argument ({} given)", a.len());
    }
    k.take_pos("name", a.first().copied(), "ParamSpec", 1)?;
    let nm = check_name("ParamSpec", k.get("name"))?;
    let cov = truthy(k.get("covariant"))?;
    let contra = truthy(k.get("contravariant"))?;
    let infer = truthy(k.get("infer_variance"))?;
    if cov && contra {
        raise!(ValueError, "Bivariant type variables are not supported.");
    }
    if infer && (cov || contra) {
        raise!(ValueError, "Variance cannot be specified with infer_variance.");
    }
    // a ParamSpec bound may be any object (e.g. a list of types)
    let bound = k.get("bound").filter(|b| !b.is(none::none()));
    let o = new_param(paramspec_type(), &nm)?;
    set_field(&o, BOUND, bound);
    set_field(&o, COVARIANT, Some(bool_::new_bool(cov)));
    set_field(&o, CONTRAVARIANT, Some(bool_::new_bool(contra)));
    set_field(&o, INFER, Some(bool_::new_bool(infer)));
    if let Some(d) = k.get("default") {
        if !d.is(nodefault()) {
            set_field(&o, DEFAULT, Some(d));
        }
    }
    set_module(&o, caller_module())?;
    Ok(o)
}

unsafe extern "C" fn typevartuple_new(_tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(typevartuple_new_impl(&*args, kw)) }
}
fn typevartuple_new_impl(args: &PyObject, kw: *mut PyObject) -> PyResult {
    let names = ["name", "bound", "covariant", "contravariant", "infer_variance", "default"];
    let mut k = parse_kw("TypeVarTuple", &names, kw)?;
    let a = tuple::tuple_items(args);
    if a.len() > 1 {
        raise!(TypeError, "TypeVarTuple() takes at most 1 positional argument ({} given)", a.len());
    }
    k.take_pos("name", a.first().copied(), "TypeVarTuple", 1)?;
    let nm = check_name("TypeVarTuple", k.get("name"))?;
    let cov = truthy(k.get("covariant"))?;
    let contra = truthy(k.get("contravariant"))?;
    let infer = truthy(k.get("infer_variance"))?;
    if cov && contra {
        raise!(ValueError, "Bivariant type variables are not supported.");
    }
    if infer && (cov || contra) {
        raise!(ValueError, "Variance cannot be specified with infer_variance.");
    }
    let o = new_param(typevartuple_type(), &nm)?;
    // the bound is stored as given (no type check), like ParamSpec's
    set_field(&o, BOUND, k.get("bound").filter(|b| !b.is(none::none())));
    set_field(&o, COVARIANT, Some(bool_::new_bool(cov)));
    set_field(&o, CONTRAVARIANT, Some(bool_::new_bool(contra)));
    set_field(&o, INFER, Some(bool_::new_bool(infer)));
    if let Some(d) = k.get("default") {
        if !d.is(nodefault()) {
            set_field(&o, DEFAULT, Some(d));
        }
    }
    set_module(&o, caller_module())?;
    Ok(o)
}

// ---------------------------------------------------------------- shared getters

fn get_name(o: &PyObject) -> PyResult {
    Ok(field_ref(o, NAME).unwrap_or_else(none::none_ref))
}
fn get_covariant(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(flag(o, COVARIANT)))
}
fn get_contravariant(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(flag(o, CONTRAVARIANT)))
}
fn get_infer(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(flag(o, INFER)))
}
fn call_format1(f: &PyObject) -> PyResult {
    let one = int::new_int(1);
    ab::call(f, &[&one])
}
fn lazy_get(o: &PyObject, val: usize, eval: usize) -> PyResult<Option<PyRef>> {
    if let Some(v) = field_ref(o, val) {
        return Ok(Some(v));
    }
    if let Some(f) = field_ref(o, eval) {
        let v = call_format1(&f)?;
        set_field(o, val, Some(v.clone()));
        return Ok(Some(v));
    }
    Ok(None)
}
fn get_bound(o: &PyObject) -> PyResult {
    Ok(lazy_get(o, BOUND, EVAL_BOUND)?.unwrap_or_else(none::none_ref))
}
fn get_constraints(o: &PyObject) -> PyResult {
    Ok(lazy_get(o, CONSTRAINTS, EVAL_CONSTRAINTS)?.unwrap_or_else(tuple::empty_tuple))
}
fn get_default(o: &PyObject) -> PyResult {
    Ok(lazy_get(o, DEFAULT, EVAL_DEFAULT)?.unwrap_or_else(|| nodefault().to_ref()))
}
fn evaluator(o: &PyObject, val: usize, eval: usize) -> PyResult {
    if let Some(f) = field_ref(o, eval) {
        return Ok(f);
    }
    if let Some(v) = field_ref(o, val) {
        return new_consteval(&v);
    }
    Ok(none::none_ref())
}
fn get_evaluate_bound(o: &PyObject) -> PyResult {
    evaluator(o, BOUND, EVAL_BOUND)
}
fn get_evaluate_constraints(o: &PyObject) -> PyResult {
    evaluator(o, CONSTRAINTS, EVAL_CONSTRAINTS)
}
fn get_evaluate_default(o: &PyObject) -> PyResult {
    evaluator(o, DEFAULT, EVAL_DEFAULT)
}
fn has_default(o: &PyObject) -> PyResult {
    let h = field(o, EVAL_DEFAULT).is_some() || field(o, DEFAULT).is_some();
    Ok(bool_::new_bool(h))
}
fn get_dict(o: &PyObject) -> PyResult {
    Ok(inst_dict(o))
}

fn param_reduce(o: &PyObject) -> PyResult {
    get_name(o)
}
fn no_mro_entries(o: &PyObject, _bases: &PyObject) -> PyResult {
    raise!(TypeError, "Cannot subclass an instance of {}", type_name(o.ty()));
}

unsafe extern "C" fn param_or(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe {
        let t = tuple::new_tuple(vec![(*a).to_ref(), (*b).to_ref()]);
        into_c(crate::types::union::union_from_tuple(&t))
    }
}

unsafe extern "C" fn typevar_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        let nm = pystr::to_string_lossy(&get_name(o).unwrap());
        let s = if flag(o, INFER) {
            nm
        } else if flag(o, COVARIANT) {
            format!("+{}", nm)
        } else if flag(o, CONTRAVARIANT) {
            format!("-{}", nm)
        } else {
            format!("~{}", nm)
        };
        pystr::new_str(&s).into_raw()
    }
}

// TypeVar
fn typevar_subst(o: &PyObject, arg: &PyObject) -> PyResult {
    call_typing("_typevar_subst", &[o, arg])
}
fn typevar_prepare_subst(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("__typing_prepare_subst__", args, 2, 2)?;
    let (alias, a) = (args[0], args[1]);
    let params = ab::getattr(alias, name!("__parameters__"))?;
    let pitems = ab::collect_iterable(&params)?;
    let i = match pitems.iter().position(|p| p.is(o)) {
        Some(i) => i,
        None => raise!(ValueError, "tuple.index(x): x not in tuple"),
    };
    let argv = ab::collect_iterable(a)?;
    if i < argv.len() {
        return Ok(a.to_ref());
    }
    if i == argv.len() {
        let d = get_default(o)?;
        if !d.is(nodefault()) {
            let mut v = argv;
            v.push(d);
            return Ok(tuple::new_tuple(v));
        }
    }
    raise!(
        TypeError,
        "Too few arguments for {}; actual {}, expected at least {}",
        ab::repr_string(alias)?,
        argv.len(),
        i + 1
    );
}

// ParamSpec
fn paramspec_args(o: &PyObject) -> PyResult {
    let r = lu::alloc(paramspecargs_type())?;
    set_field(&r, 0, Some(o.to_ref()));
    Ok(r)
}
fn paramspec_kwargs(o: &PyObject) -> PyResult {
    let r = lu::alloc(paramspeckwargs_type())?;
    set_field(&r, 0, Some(o.to_ref()));
    Ok(r)
}
fn paramspec_subst(o: &PyObject, arg: &PyObject) -> PyResult {
    call_typing("_paramspec_subst", &[o, arg])
}
fn paramspec_prepare_subst(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("__typing_prepare_subst__", args, 2, 2)?;
    call_typing("_paramspec_prepare_subst", &[o, args[0], args[1]])
}

// TypeVarTuple
unsafe extern "C" fn typevartuple_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let unpack = typing_attr("Unpack")?;
            let u = ab::getitem(&unpack, &*o)?;
            ab::get_iter(&tuple::new_tuple(vec![u]))
        })();
        into_c(r)
    }
}
fn typevartuple_subst(_o: &PyObject, _arg: &PyObject) -> PyResult {
    raise!(TypeError, "Substitution of bare TypeVarTuple is not supported");
}
fn typevartuple_prepare_subst(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("__typing_prepare_subst__", args, 2, 2)?;
    call_typing("_typevartuple_prepare_subst", &[o, args[0], args[1]])
}

// ParamSpecArgs / ParamSpecKwargs
unsafe extern "C" fn psa_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let fname = if tp == paramspecargs_type() { "ParamSpecArgs" } else { "ParamSpecKwargs" };
            let names = ["origin"];
            let mut k = parse_kw(fname, &names, kw)?;
            let a = tuple::tuple_items(&*args);
            if a.len() > 1 {
                raise!(TypeError, "{}() takes at most 1 argument ({} given)", fname, a.len());
            }
            k.take_pos("origin", a.first().copied(), fname, 1)?;
            let origin = match k.get("origin") {
                Some(x) => x,
                None => raise!(TypeError, "{}() missing required argument 'origin' (pos 1)", fname),
            };
            let r = lu::alloc(tp)?;
            set_field(&r, 0, Some(origin));
            Ok(r)
        })();
        into_c(r)
    }
}
fn psa_origin(o: &PyObject) -> PyResult {
    Ok(field_ref(o, 0).unwrap_or_else(none::none_ref))
}
unsafe extern "C" fn psa_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        let suffix = if o.is_type(paramspecargs_type()) { "args" } else { "kwargs" };
        let r = (|| -> PyResult {
            let origin = psa_origin(o)?;
            let base = if origin.is_type(paramspec_type()) {
                pystr::to_string_lossy(&*get_name(&origin)?)
            } else {
                ab::repr_string(&origin)?
            };
            Ok(pystr::new_str(&format!("{}.{}", base, suffix)))
        })();
        into_c(r)
    }
}
unsafe extern "C" fn psa_richcompare(a: *mut PyObject, b: *mut PyObject, op: std::ffi::c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if a.type_ptr() != b.type_ptr() || (op != Py_EQ && op != Py_NE) {
            return none::not_implemented().to_ref().into_raw();
        }
        let r = ab::rich_compare(&psa_origin(a).unwrap(), &psa_origin(b).unwrap(), op);
        into_c(r)
    }
}

// ---------------------------------------------------------------- TypeAliasType

unsafe extern "C" fn typealias_new(_tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(typealias_new_impl(&*args, kw)) }
}
fn typealias_new_impl(args: &PyObject, kw: *mut PyObject) -> PyResult {
    let names = ["name", "value", "type_params", "qualname"];
    let mut k = parse_kw("TypeAliasType", &names, kw)?;
    let a = tuple::tuple_items(args);
    if a.len() > 2 {
        raise!(TypeError, "TypeAliasType() takes at most 2 positional arguments ({} given)", a.len());
    }
    k.take_pos("name", a.first().copied(), "TypeAliasType", 1)?;
    k.take_pos("value", a.get(1).copied(), "TypeAliasType", 2)?;
    let nm = match k.get("name") {
        None => raise!(TypeError, "TypeAliasType() missing required argument 'name' (pos 1)"),
        Some(n) if pystr::is_str(&n) => n,
        Some(n) => raise!(TypeError, "TypeAliasType() argument 'name' must be str, not {}", tname(&n)),
    };
    let value = match k.get("value") {
        None => raise!(TypeError, "TypeAliasType() missing required argument 'value' (pos 2)"),
        Some(v) => v,
    };
    let params = match k.get("type_params") {
        None => None,
        Some(p) if tuple::is_tuple(&p) => Some(p),
        Some(_) => raise!(TypeError, "type_params must be a tuple"),
    };
    let qualname = match k.get("qualname") {
        None => nm.clone(),
        Some(q) if q.is(none::none()) => nm.clone(),
        Some(q) if pystr::is_str(&q) => q,
        Some(q) => raise!(TypeError, "qualname must be a string, not {}", tname(&q)),
    };
    if let Some(p) = &params {
        let mut seen_default = false;
        for x in tuple::tuple_items(p) {
            let ok = x.is_type(typevar_type()) || x.is_type(paramspec_type()) || x.is_type(typevartuple_type());
            if !ok {
                raise!(TypeError, "Expected a type param, got {}", ab::repr_string(x)?);
            }
            let hd = ab::is_true(&*has_default(x)?)?;
            if hd {
                seen_default = true;
            } else if seen_default {
                raise!(TypeError, "non-default type parameter '{}' follows default type parameter", ab::repr_string(x)?);
            }
        }
    }
    let params = params.filter(|p| tuple::tuple_len(p) > 0);
    new_typealias(&nm, &qualname, params.as_deref(), Some(&value), None, caller_module())
}

fn ta_field(o: &PyObject, i: usize) -> PyRef {
    field_ref(o, i).unwrap_or_else(none::none_ref)
}
fn ta_name(o: &PyObject) -> PyResult {
    Ok(ta_field(o, TA_NAME))
}
fn ta_qualname(o: &PyObject) -> PyResult {
    Ok(ta_field(o, TA_QUALNAME))
}
fn ta_module(o: &PyObject) -> PyResult {
    Ok(ta_field(o, TA_MODULE))
}
fn ta_set_module(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    set_field(o, TA_MODULE, Some(v.map(|x| x.to_ref()).unwrap_or_else(none::none_ref)));
    Ok(())
}
fn ta_type_params(o: &PyObject) -> PyResult {
    Ok(field_ref(o, TA_PARAMS).unwrap_or_else(tuple::empty_tuple))
}
fn ta_parameters(o: &PyObject) -> PyResult {
    match field_ref(o, TA_PARAMS) {
        None => Ok(tuple::empty_tuple()),
        Some(p) => unpack_typevartuples(&p),
    }
}
fn ta_value(o: &PyObject) -> PyResult {
    if let Some(v) = field_ref(o, TA_VALUE) {
        return Ok(v);
    }
    match field_ref(o, TA_COMPUTE) {
        Some(f) => {
            let v = call_format1(&f)?;
            set_field(o, TA_VALUE, Some(v.clone()));
            Ok(v)
        }
        None => Ok(none::none_ref()),
    }
}
fn ta_evaluate_value(o: &PyObject) -> PyResult {
    if let Some(f) = field_ref(o, TA_COMPUTE) {
        return Ok(f);
    }
    new_consteval(&ta_field(o, TA_VALUE))
}
unsafe extern "C" fn ta_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(Ok(ta_field(&*o, TA_QUALNAME))) }
}
unsafe extern "C" fn ta_getitem(o: *mut PyObject, item: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        let r = (|| -> PyResult {
            if field(o, TA_PARAMS).is_none() {
                raise!(TypeError, "Only generic type aliases are subscriptable");
            }
            crate::types::genericalias::new_generic_alias(o, &*item)
        })();
        into_c(r)
    }
}
unsafe extern "C" fn ta_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { typevartuple_iter(o) }
}
fn ta_reduce(o: &PyObject) -> PyResult {
    ta_name(o)
}
fn ta_mro_entries(_o: &PyObject, _bases: &PyObject) -> PyResult {
    raise!(TypeError, "Cannot subclass an instance of TypeAliasType");
}

// ---------------------------------------------------------------- Generic

fn generic_class_getitem(cls: &PyObject, args: &[&PyObject]) -> PyResult {
    let f = typing_attr("_generic_class_getitem")?;
    let mut v: Vec<&PyObject> = vec![cls];
    v.extend_from_slice(args);
    ab::call(&f, &v)
}
fn generic_init_subclass(cls: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let f = typing_attr("_generic_init_subclass")?;
    let mut v: Vec<&PyObject> = vec![cls];
    v.extend_from_slice(args);
    ab::call_kw(&f, &v, kw)
}

// ---------------------------------------------------------------- NoDefault

unsafe extern "C" fn nodefault_new(_tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        if tuple::tuple_len(&*args) > 0 || (!kw.is_null() && dict::dict_len(&*kw) > 0) {
            let _ = err!(TypeError, "NoDefaultType takes no arguments");
            return std::ptr::null_mut();
        }
        nodefault().to_ref().into_raw()
    }
}
unsafe extern "C" fn nodefault_repr(_o: *mut PyObject) -> *mut PyObject {
    pystr::new_str("typing.NoDefault").into_raw()
}
fn nodefault_reduce(_o: &PyObject) -> PyResult {
    Ok(pystr::new_str("NoDefault"))
}

// ---------------------------------------------------------------- _ConstEvaluator

fn new_consteval(v: &PyObject) -> PyResult {
    let o = lu::alloc(tp!(CONSTEVAL_TYPE))?;
    set_field(&o, 0, Some(v.to_ref()));
    Ok(o)
}

/// `_Py_typing_type_repr`: annotationlib.type_repr equivalent.
pub fn typing_type_repr(p: &PyObject) -> PyResult<String> {
    if p.is(none::ellipsis()) {
        return Ok("...".into());
    }
    if p.is(type_obj(none::none().type_ptr())) {
        return Ok("None".into());
    }
    crate::types::genericalias::type_repr(p)
}

unsafe extern "C" fn consteval_call(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            if !kw.is_null() && dict::dict_len(&*kw) > 0 {
                raise!(TypeError, "constevaluator.__call__() takes no keyword arguments");
            }
            let a = tuple::tuple_items(&*args);
            if a.len() != 1 {
                raise!(TypeError, "constevaluator.__call__() takes exactly one argument ({} given)", a.len());
            }
            let format = ab::as_i64(a[0])?;
            let v = field_ref(&*o, 0).unwrap_or_else(none::none_ref);
            if format == 4 {
                if tuple::is_tuple(&v) {
                    let items = tuple::tuple_items(&v);
                    let mut parts = Vec::new();
                    for x in items {
                        parts.push(typing_type_repr(x)?);
                    }
                    let s = if items.len() == 1 { format!("({},)", parts[0]) } else { format!("({})", parts.join(", ")) };
                    return Ok(pystr::new_str(&s));
                }
                return Ok(pystr::new_str(&typing_type_repr(&v)?));
            }
            Ok(v)
        })();
        into_c(r)
    }
}
unsafe extern "C" fn consteval_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let v = field_ref(&*o, 0).unwrap_or_else(none::none_ref);
        into_c(ab::repr_string(&v).map(|s| pystr::new_str(&format!("<constevaluator {}>", s))))
    }
}

fn idfunc(_m: &PyObject, x: &PyObject) -> PyResult {
    Ok(x.to_ref())
}

// ---------------------------------------------------------------- type setup

pub fn init_types() {
    unsafe {
        let sz = std::mem::size_of::<TypeParamObject>();
        let wl = std::mem::offset_of!(TypeParamObject, weakreflist) as isize;
        let dictoff = (lu::HEADER + DICT * 8) as isize;
        let common_getset = |extra: Vec<PyGetSetDef>| {
            let mut v = extra;
            v.push(crate::methods::gsdef("__name__", crate::methods::getter(get_name), None, ""));
            v.push(crate::methods::gsdef("__dict__", crate::methods::getter(get_dict), None, ""));
            crate::methods::gstable(v)
        };
        for (t, nm) in [
            (typevar_type(), "typing.TypeVar"),
            (paramspec_type(), "typing.ParamSpec"),
            (typevartuple_type(), "typing.TypeVarTuple"),
        ] {
            lu::setup_ptr_type(t, nm, sz, 0);
            let tt = &mut *t;
            tt.tp_weaklistoffset = wl;
            tt.tp_dictoffset = dictoff;
            tt.tp_repr = Some(typevar_repr);
            static mut TV_NUM: PyNumberMethods = PyNumberMethods::ZERO;
            (*(&raw mut TV_NUM)).nb_or = Some(param_or);
            tt.tp_as_number = &raw mut TV_NUM;
        }
        let g = |name: &str, f: getter| crate::methods::gsdef(name, f, None, "");
        use crate::methods::getter as gt;
        let t = &mut *typevar_type();
        t.tp_doc = c"Type variable.".as_ptr();
        t.tp_new = Some(typevar_new);
        t.tp_getset = common_getset(vec![
            g("__bound__", gt(get_bound)),
            g("__constraints__", gt(get_constraints)),
            g("__covariant__", gt(get_covariant)),
            g("__contravariant__", gt(get_contravariant)),
            g("__infer_variance__", gt(get_infer)),
            g("__default__", gt(get_default)),
            g("evaluate_bound", gt(get_evaluate_bound)),
            g("evaluate_constraints", gt(get_evaluate_constraints)),
            g("evaluate_default", gt(get_evaluate_default)),
        ]);
        t.tp_methods = crate::mdefs![
            ("__typing_subst__", O, typevar_subst),
            ("__typing_prepare_subst__", FAST, typevar_prepare_subst, "__typing_prepare_subst__($self, alias, args, /)\n--\n\n"),
            ("__reduce__", NOARGS, param_reduce),
            ("__mro_entries__", O, no_mro_entries),
            ("has_default", NOARGS, has_default),
        ];
        let t = &mut *paramspec_type();
        t.tp_doc = c"Parameter specification variable.".as_ptr();
        t.tp_new = Some(paramspec_new);
        t.tp_getset = common_getset(vec![
            g("__bound__", gt(get_bound)),
            g("__covariant__", gt(get_covariant)),
            g("__contravariant__", gt(get_contravariant)),
            g("__infer_variance__", gt(get_infer)),
            g("__default__", gt(get_default)),
            g("evaluate_default", gt(get_evaluate_default)),
            g("args", gt(paramspec_args)),
            g("kwargs", gt(paramspec_kwargs)),
        ]);
        t.tp_methods = crate::mdefs![
            ("__typing_subst__", O, paramspec_subst),
            ("__typing_prepare_subst__", FAST, paramspec_prepare_subst, "__typing_prepare_subst__($self, alias, args, /)\n--\n\n"),
            ("__reduce__", NOARGS, param_reduce),
            ("__mro_entries__", O, no_mro_entries),
            ("has_default", NOARGS, has_default),
        ];
        let t = &mut *typevartuple_type();
        t.tp_doc = c"Type variable tuple.".as_ptr();
        t.tp_new = Some(typevartuple_new);
        t.tp_iter = Some(typevartuple_iter);
        t.tp_getset = common_getset(vec![
            g("__bound__", gt(get_bound)),
            g("__covariant__", gt(get_covariant)),
            g("__contravariant__", gt(get_contravariant)),
            g("__infer_variance__", gt(get_infer)),
            g("__default__", gt(get_default)),
            g("evaluate_bound", gt(get_evaluate_bound)),
            g("evaluate_default", gt(get_evaluate_default)),
        ]);
        t.tp_methods = crate::mdefs![
            ("__typing_subst__", O, typevartuple_subst),
            ("__typing_prepare_subst__", FAST, typevartuple_prepare_subst, "__typing_prepare_subst__($self, alias, args, /)\n--\n\n"),
            ("__reduce__", NOARGS, param_reduce),
            ("__mro_entries__", O, no_mro_entries),
            ("has_default", NOARGS, has_default),
        ];

        for (t, nm) in [(paramspecargs_type(), "typing.ParamSpecArgs"), (paramspeckwargs_type(), "typing.ParamSpecKwargs")] {
            lu::setup_ptr_type(t, nm, std::mem::size_of::<OneFieldWeak>(), 0);
            let tt = &mut *t;
            tt.tp_weaklistoffset = std::mem::offset_of!(OneFieldWeak, weakreflist) as isize;
            tt.tp_new = Some(psa_new);
            tt.tp_repr = Some(psa_repr);
            tt.tp_richcompare = Some(psa_richcompare);
            tt.tp_getset = crate::getset_table![("__origin__", psa_origin)];
            tt.tp_methods = crate::mdefs![("__mro_entries__", O, no_mro_entries)];
        }
        (*paramspecargs_type()).tp_doc = c"The args for a ParamSpec object.".as_ptr();
        (*paramspeckwargs_type()).tp_doc = c"The kwargs for a ParamSpec object.".as_ptr();

        let t = typealias_type();
        lu::setup_ptr_type(t, "typing.TypeAliasType", std::mem::size_of::<TypeAliasObject>(), 0);
        let tt = &mut *t;
        tt.tp_doc = c"Type alias.".as_ptr();
        tt.tp_weaklistoffset = std::mem::offset_of!(TypeAliasObject, weakreflist) as isize;
        tt.tp_new = Some(typealias_new);
        tt.tp_repr = Some(ta_repr);
        tt.tp_iter = Some(ta_iter);
        static mut TA_MAP: PyMappingMethods = PyMappingMethods::ZERO;
        (*(&raw mut TA_MAP)).mp_subscript = Some(ta_getitem);
        tt.tp_as_mapping = &raw mut TA_MAP;
        static mut TA_NUM: PyNumberMethods = PyNumberMethods::ZERO;
        (*(&raw mut TA_NUM)).nb_or = Some(ta_or);
        tt.tp_as_number = &raw mut TA_NUM;
        tt.tp_members = crate::member_table![
            ("__name__", Py_T_OBJECT, lu::HEADER + TA_NAME * 8, Py_READONLY),
            ("__qualname__", Py_T_OBJECT, lu::HEADER + TA_QUALNAME * 8, Py_READONLY),
        ];
        let _ = (ta_name, ta_qualname);
        tt.tp_getset = crate::getset_table![
            ("__module__", ta_module, ta_set_module),
            ("__type_params__", ta_type_params),
            ("__parameters__", ta_parameters),
            ("__value__", ta_value),
            ("evaluate_value", ta_evaluate_value),
        ];
        tt.tp_methods = crate::mdefs![("__reduce__", NOARGS, ta_reduce), ("__mro_entries__", O, ta_mro_entries)];

        let t = &mut *generic_type();
        t.tp_name = c"typing.Generic".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE;
        t.tp_doc = c"Generic()\n--\n\nAbstract base class for generic types.\n\nOn Python 3.12 and newer, generic classes implicitly inherit from\nGeneric when they declare a parameter list after the class's name::\n\n    class Mapping[KT, VT]:\n        def __getitem__(self, key: KT) -> VT:\n            ...\n        # Etc.\n\nOn older versions of Python, however, generic classes have to\nexplicitly inherit from Generic.\n\nAfter a class has been declared to be generic, it can then be used as\nfollows::\n\n    def lookup_name[KT, VT](mapping: Mapping[KT, VT], key: KT, default: VT) -> VT:\n        try:\n            return mapping[key]\n        except KeyError:\n            return default".as_ptr();
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_setattro = Some(object_generic_setattro);
        // object.__new__ (core-runtime): keeps its argument and abstract-class checks
        t.tp_new = Some(crate::types::object_type::object_new);
        t.tp_alloc = Some(generic_alloc);
        t.tp_free = Some(generic_free);
        t.tp_methods = crate::mdefs![
            ("__class_getitem__", FAST, generic_class_getitem, "Parameterizes a generic class."; METH_CLASS),
            ("__init_subclass__", FASTKW, generic_init_subclass, "Function to initialize subclasses."; METH_CLASS),
        ];

        let t = &mut *tp!(NODEFAULT_TYPE);
        t.tp_name = c"NoDefaultType".as_ptr();
        t.tp_basicsize = std::mem::size_of::<PyObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT;
        t.tp_doc = c"The type of the NoDefault singleton.".as_ptr();
        t.tp_new = Some(nodefault_new);
        t.tp_repr = Some(nodefault_repr);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_setattro = Some(object_generic_setattro);
        t.tp_methods = crate::mdefs![("__reduce__", NOARGS, nodefault_reduce)];

        let t = tp!(CONSTEVAL_TYPE);
        lu::setup_ptr_type(t, "_typing._ConstEvaluator", std::mem::size_of::<OneField>(), 0);
        (*t).tp_call = Some(consteval_call);
        (*t).tp_repr = Some(consteval_repr);
    }
}

unsafe extern "C" fn ta_or(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(crate::types::union::union_or(&*a, &*b)) }
}

pub fn ready() -> PyResult<()> {
    for t in [
        typevar_type(),
        paramspec_type(),
        typevartuple_type(),
        paramspecargs_type(),
        paramspeckwargs_type(),
        typealias_type(),
        generic_type(),
        tp!(NODEFAULT_TYPE),
        tp!(CONSTEVAL_TYPE),
    ] {
        lu::ready_type(t)?;
    }
    unsafe {
        let o = &raw mut NODEFAULT_OBJ;
        crate::object::init_header(o, tp!(NODEFAULT_TYPE));
        make_immortal(o);
        let d = typing_dict_methods();
        let _ = d;
    }
    Ok(())
}

fn typing_dict_methods() -> *mut PyMethodDef {
    crate::mdefs![("_idfunc", O, idfunc)]
}

/// The `_typing` builtin module.
pub fn make_typing_module() -> PyResult {
    let m = crate::types::module::new_module("_typing")?;
    let d = crate::types::module::module_dict(&m);
    crate::builtins::add_methods_to_dict(&d, typing_dict_methods(), Some(&m))?;
    for (n, t) in [
        ("TypeVar", typevar_type()),
        ("ParamSpec", paramspec_type()),
        ("TypeVarTuple", typevartuple_type()),
        ("ParamSpecArgs", paramspecargs_type()),
        ("ParamSpecKwargs", paramspeckwargs_type()),
        ("TypeAliasType", typealias_type()),
        ("Generic", generic_type()),
        ("Union", crate::types::union::union_type()),
    ] {
        dict::set_item_str(&d, n, type_obj(t))?;
    }
    dict::set_item_str(&d, "NoDefault", nodefault())?;
    Ok(m)
}
