//! The `_contextvars` module: Context, ContextVar, Token, copy_context.
//!
//! A Context's mapping is an immutable snapshot (a dict that is never mutated
//! after publication; updates create a new dict), so copies share storage.
//! The current context lives in the thread state (`ts.c.context`).

use crate::abstract_ as ab;
use crate::args::Params;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::fncommon::{self as fc, Trav, opt, rs, targs};
use crate::object::*;
use crate::types::{dict, list, none, str as pystr, tuple};
use crate::{mdefs, raise, trav};
use std::ffi::c_int;
use std::sync::atomic::{AtomicPtr, Ordering::*};

type VisitFn<'a> = &'a mut dyn FnMut(*mut PyObject) -> c_int;

#[unsafe(no_mangle)]
pub static mut PyContext_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyContextVar_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyContextToken_Type: PyTypeObject = PyTypeObject::ZERO;

fn ctx_type() -> *mut PyTypeObject {
    &raw mut PyContext_Type
}
fn var_type() -> *mut PyTypeObject {
    &raw mut PyContextVar_Type
}
fn token_type() -> *mut PyTypeObject {
    &raw mut PyContextToken_Type
}

struct Context {
    vars: Option<PyRef>, // dict, never mutated once stored
    prev: Option<PyRef>,
    entered: bool,
}
impl Trav for Context {
    fn trav(&self, v: VisitFn) -> c_int {
        trav!(v; self.vars, self.prev)
    }
}

struct ContextVar {
    name: Option<PyRef>,
    default: Option<PyRef>,
    hash: isize,
}
impl Trav for ContextVar {
    fn trav(&self, v: VisitFn) -> c_int {
        trav!(v; self.name, self.default)
    }
}

struct Token {
    ctx: Option<PyRef>,
    var: Option<PyRef>,
    old_value: Option<PyRef>, // None => MISSING
    used: bool,
}
impl Trav for Token {
    fn trav(&self, v: VisitFn) -> c_int {
        trav!(v; self.ctx, self.var, self.old_value)
    }
}

static MISSING: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());

fn missing() -> &'static PyObject {
    unsafe { &*MISSING.load(Acquire) }
}

fn new_context(vars: PyRef) -> PyResult {
    ensure_types()?;
    fc::robj_new(ctx_type(), Context { vars: Some(vars), prev: None, entered: false })
}

fn ctx_vars(c: &PyObject) -> PyRef {
    rs::<Context>(c).vars.clone().unwrap()
}

/// The current thread's context (created on first use).
pub fn current_context() -> PyResult {
    let ts = crate::threads::current_mut();
    unsafe {
        let p = (*ts).c.context;
        if !p.is_null() {
            return Ok((*p).to_ref());
        }
        let c = new_context(dict::new_dict())?;
        (*ts).c.context = c.clone().into_raw();
        (*ts).c.context_ver += 1;
        Ok(c)
    }
}

/// Drop the current thread's context (interpreter shutdown: what its variables hold is
/// finalized like the rest of the thread state).
pub fn clear_thread_context() {
    set_current(None);
}

fn set_current(c: Option<PyRef>) {
    let ts = crate::threads::current_mut();
    unsafe {
        let old = (*ts).c.context;
        (*ts).c.context = c.map_or(std::ptr::null_mut(), |c| c.into_raw());
        (*ts).c.context_ver += 1;
        xdecref(old);
    }
}

/// Replace the mapping of the current context with `f(copy)`.
fn update_current(f: impl FnOnce(&PyObject) -> PyResult<()>) -> PyResult<()> {
    let c = current_context()?;
    let new = dict::copy(&ctx_vars(&c))?;
    f(&new)?;
    let old = rs::<Context>(&c).vars.replace(new);
    drop(old);
    unsafe { (*crate::threads::current_mut()).c.context_ver += 1 };
    Ok(())
}

pub fn context_enter(c: &PyObject) -> PyResult<()> {
    if rs::<Context>(c).entered {
        raise!(RuntimeError, "cannot enter context: {} is already entered", ab::repr_string(c)?);
    }
    let prev = current_context()?;
    let s = rs::<Context>(c);
    s.prev = Some(prev);
    s.entered = true;
    set_current(Some(c.to_ref()));
    Ok(())
}

pub fn context_exit(c: &PyObject) -> PyResult<()> {
    let s = rs::<Context>(c);
    if !s.entered {
        raise!(RuntimeError, "cannot exit context: {} has not been entered", ab::repr_string(c)?);
    }
    let prev = s.prev.take();
    s.entered = false;
    set_current(prev);
    Ok(())
}

// ------------------------------------------------------------------ Context

unsafe extern "C" fn ctx_new(_tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        if !targs(args).is_empty() || fc::kw_len(kw) > 0 {
            return into_c(Err(err!(TypeError, "Context() does not accept any arguments")));
        }
        into_c(new_context(dict::new_dict()))
    }
}

fn check_var(k: &PyObject) -> PyResult<()> {
    if !k.is_type(var_type()) {
        raise!(TypeError, "a ContextVar key was expected, got {}", ab::repr_string(k)?);
    }
    Ok(())
}

fn ctx_lookup(c: &PyObject, k: &PyObject) -> PyResult<Option<PyRef>> {
    check_var(k)?;
    let h = rs::<ContextVar>(k).hash;
    dict::get_item_hash(&ctx_vars(c), k, h)
}

unsafe extern "C" fn ctx_subscript(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe {
        match ctx_lookup(&*o, &*k) {
            Ok(Some(v)) => v.into_raw(),
            Ok(None) => into_c(Err(set_err_obj(crate::types::exceptions::KeyError(), (*k).to_ref()))),
            Err(_) => std::ptr::null_mut(),
        }
    }
}

unsafe extern "C" fn ctx_contains(o: *mut PyObject, k: *mut PyObject) -> c_int {
    unsafe {
        match ctx_lookup(&*o, &*k) {
            Ok(r) => r.is_some() as c_int,
            Err(_) => -1,
        }
    }
}

unsafe extern "C" fn ctx_len(o: *mut PyObject) -> Py_ssize_t {
    unsafe { dict::dict_len(&ctx_vars(&*o)) as Py_ssize_t }
}

unsafe extern "C" fn ctx_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let keys = list::new_list(dict::keys_vec(&ctx_vars(&*o)));
        into_c(ab::get_iter(&keys))
    }
}

unsafe extern "C" fn ctx_richcompare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> *mut PyObject {
    unsafe {
        let (a, b) = (&*a, &*b);
        if !b.is_type(ctx_type()) || (op != Py_EQ && op != Py_NE) {
            return none::not_implemented().to_ref().into_raw();
        }
        into_c(ab::rich_compare(&ctx_vars(a), &ctx_vars(b), op))
    }
}

fn ctx_get(o: &PyObject, a: &[&PyObject]) -> PyResult {
    crate::args::check_pos("get", a, 1, 2)?;
    match ctx_lookup(o, a[0])? {
        Some(v) => Ok(v),
        None => Ok(a.get(1).map_or_else(fc::none, |d| d.to_ref())),
    }
}
fn ctx_keys(o: &PyObject) -> PyResult {
    Ok(list::new_list(dict::keys_vec(&ctx_vars(o))))
}
fn ctx_values(o: &PyObject) -> PyResult {
    Ok(list::new_list(dict::items_vec(&ctx_vars(o)).into_iter().map(|(_, v)| v).collect()))
}
fn ctx_items(o: &PyObject) -> PyResult {
    Ok(list::new_list(dict::items_vec(&ctx_vars(o)).into_iter().map(|(k, v)| tuple::tuple2(k, v)).collect()))
}
fn ctx_copy(o: &PyObject) -> PyResult {
    new_context(ctx_vars(o))
}

fn ctx_run(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let nkw = kw.map_or(0, tuple::tuple_len);
    if args.len() - nkw < 1 {
        raise!(TypeError, "run() missing 1 required positional argument");
    }
    context_enter(o)?;
    let r = ab::call_kw(args[0], &args[1..], kw);
    let exc = if r.is_err() { take_exc() } else { None };
    let e = context_exit(o);
    if let Some(x) = exc {
        set_exc(x);
        return r;
    }
    e?;
    r
}

// ------------------------------------------------------------------ ContextVar

unsafe extern "C" fn var_new(_tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            ensure_types()?;
            let a = fc::VcArgs::from_c(args, kw);
            let refs = a.refs();
            let nkw = a.names.as_ref().map_or(0, |n| tuple::tuple_len(n));
            if refs.len() - nkw != 1 {
                raise!(TypeError, "ContextVar() takes exactly 1 positional argument ({} given)", refs.len() - nkw);
            }
            let p = Params::new("ContextVar", &["name", "default"], 1, 1).kwonly(1);
            let [name, default] = a.parse::<2>(&p)?;
            let name = name.unwrap();
            if !pystr::is_str(&name) {
                raise!(TypeError, "context variable name must be a str");
            }
            let nh = ab::hash(&name)?;
            let op = fc::robj_new(var_type(), ContextVar { name: Some(name), default, hash: 0 })?;
            // mix the name hash with the identity so equal names differ
            let h = (nh as usize ^ (op.as_ptr() as usize >> 4)).wrapping_mul(1000003) as isize;
            rs::<ContextVar>(&op).hash = if h == -1 { -2 } else { h };
            Ok(op)
        })())
    }
}

unsafe extern "C" fn var_hash(o: *mut PyObject) -> Py_hash_t {
    unsafe { rs::<ContextVar>(&*o).hash }
}

fn var_get(o: &PyObject, a: &[&PyObject]) -> PyResult {
    crate::args::check_pos("get", a, 0, 1)?;
    let c = current_context()?;
    let h = rs::<ContextVar>(o).hash;
    if let Some(v) = dict::get_item_hash(&ctx_vars(&c), o, h)? {
        return Ok(v);
    }
    if let Some(d) = a.first() {
        return Ok(d.to_ref());
    }
    if let Some(d) = &rs::<ContextVar>(o).default {
        return Ok(d.clone());
    }
    Err(set_err_obj(crate::types::exceptions::LookupError(), o.to_ref()))
}

/// PyContextVar_Set.
pub fn var_set(o: &PyObject, value: &PyObject) -> PyResult {
    let c = current_context()?;
    let h = rs::<ContextVar>(o).hash;
    let old = dict::get_item_hash(&ctx_vars(&c), o, h)?;
    update_current(|d| dict::set_item_hash(d, o, h, value))?;
    fc::robj_new(token_type(), Token { ctx: Some(c), var: Some(o.to_ref()), old_value: old, used: false })
}

fn var_reset(o: &PyObject, tok: &PyObject) -> PyResult {
    if !tok.is_type(token_type()) {
        raise!(TypeError, "expected an instance of Token, got {}", ab::repr_string(tok)?);
    }
    let t = rs::<Token>(tok);
    if t.used {
        raise!(RuntimeError, "{} has already been used once", ab::repr_string(tok)?);
    }
    if !t.var.as_ref().is_some_and(|v| v.is(o)) {
        raise!(ValueError, "{} was created by a different ContextVar", ab::repr_string(tok)?);
    }
    let c = current_context()?;
    if !t.ctx.as_ref().is_some_and(|x| x.is(&c)) {
        raise!(ValueError, "{} was created in a different Context", ab::repr_string(tok)?);
    }
    t.used = true;
    let old = t.old_value.clone();
    let h = rs::<ContextVar>(o).hash;
    update_current(|d| {
        match &old {
            Some(v) => dict::set_item_hash(d, o, h, v)?,
            None => {
                dict::del_item_hash(d, o, h)?;
            }
        }
        Ok(())
    })?;
    Ok(fc::none())
}

unsafe extern "C" fn var_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let o = &*o;
            let _g = match fc::ReprGuard::enter(o) {
                Some(g) => g,
                None => return Ok(pystr::new_str("...")),
            };
            let v = rs::<ContextVar>(o);
            let (name, default) = (v.name.clone().unwrap(), v.default.clone());
            let mut s = format!("<ContextVar name={}", ab::repr_string(&name)?);
            if let Some(d) = default {
                s.push_str(&format!(" default={}", ab::repr_string(&d)?));
            }
            s.push_str(&format!(" at {:#x}>", o.as_ptr() as usize));
            Ok(pystr::new_str(&s))
        })())
    }
}

fn var_get_name(o: &PyObject) -> PyResult {
    Ok(rs::<ContextVar>(o).name.clone().unwrap())
}

// ------------------------------------------------------------------ Token

unsafe extern "C" fn token_new(_tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    into_c(Err(err!(RuntimeError, "Tokens can only be created by ContextVars")))
}

unsafe extern "C" fn token_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        into_c((|| -> PyResult {
            let o = &*o;
            let t = rs::<Token>(o);
            let used = if t.used { " used" } else { "" };
            let var = t.var.clone().unwrap();
            Ok(pystr::new_str(&format!("<Token{} var={} at {:#x}>", used, ab::repr_string(&var)?, o.as_ptr() as usize)))
        })())
    }
}

fn token_get_var(o: &PyObject) -> PyResult {
    Ok(rs::<Token>(o).var.clone().unwrap())
}
fn token_get_old(o: &PyObject) -> PyResult {
    Ok(rs::<Token>(o).old_value.clone().unwrap_or_else(|| missing().to_ref()))
}
fn token_enter(o: &PyObject) -> PyResult {
    Ok(o.to_ref())
}
fn token_exit(o: &PyObject, _a: &[&PyObject]) -> PyResult {
    let var = rs::<Token>(o).var.clone().unwrap();
    var_reset(&var, o)
}

// ------------------------------------------------------------------ module

fn copy_context(_m: &PyObject) -> PyResult {
    let c = current_context()?;
    new_context(ctx_vars(&c))
}

unsafe extern "C" fn missing_repr(_o: *mut PyObject) -> *mut PyObject {
    pystr::new_str("<Token.MISSING>").into_raw()
}

#[unsafe(no_mangle)]
pub static mut _PyContextTokenMissing_Type: PyTypeObject = PyTypeObject::ZERO;

fn leak<T>(v: T) -> *mut T {
    Box::into_raw(Box::new(v))
}

fn ensure_types() -> PyResult<()> {
    unsafe {
        let t = ctx_type();
        if (*t).tp_flags & Py_TPFLAGS_READY != 0 {
            return Ok(());
        }
        fc::init_robj_type::<Context>(t, c"_contextvars.Context", 0);
        (*t).tp_new = Some(ctx_new);
        (*t).tp_doc = c"Context()\n--\n\nA mapping from ContextVars to their values, used as an execution context.".as_ptr();
        (*t).tp_iter = Some(ctx_iter);
        (*t).tp_richcompare = Some(ctx_richcompare);
        (*t).tp_hash = Some(crate::types::slots::hash_not_implemented);
        let mut mp: PyMappingMethods = std::mem::zeroed();
        mp.mp_length = Some(ctx_len);
        mp.mp_subscript = Some(ctx_subscript);
        (*t).tp_as_mapping = leak(mp);
        let mut sq: PySequenceMethods = std::mem::zeroed();
        sq.sq_contains = Some(ctx_contains);
        (*t).tp_as_sequence = leak(sq);
        (*t).tp_methods = mdefs![
            ("run", FASTKW, ctx_run, "run($self, callable, /, *args, **kwargs)\n--\n\nRun callable in the context."),
            ("copy", NOARGS, ctx_copy, "copy($self, /)\n--\n\nReturn a shallow copy of the context object."),
            ("get", FAST, ctx_get, "get($self, key, default=None, /)\n--\n\nReturn the value for `key` if `key` has the value in the context object.\n\nIf `key` does not exist, return `default`. If `default` is not given,\nreturn None."),
            ("items", NOARGS, ctx_items, "items($self, /)\n--\n\nReturn all variables and their values in the context object.\n\nThe result is returned as a list of 2-tuples (variable, value)."),
            ("keys", NOARGS, ctx_keys, "keys($self, /)\n--\n\nReturn a list of all variables in the context object."),
            ("values", NOARGS, ctx_values, "values($self, /)\n--\n\nReturn a list of all variables' values in the context object."),
        ];

        let t = var_type();
        fc::init_robj_type::<ContextVar>(t, c"_contextvars.ContextVar", 0);
        (*t).tp_new = Some(var_new);
        (*t).tp_doc = c"ContextVar(name, *, default=<unrepresentable>)\n--\n\nA variable whose value depends on the current Context.".as_ptr();
        (*t).tp_hash = Some(var_hash);
        (*t).tp_repr = Some(var_repr);
        (*t).tp_methods = mdefs![
            ("get", FAST, var_get, "get($self, default=<unrepresentable>, /)\n--\n\nReturn a value for the context variable for the current context.\n\nIf there is no value for the variable in the current context, the method will:\n * return the value of the default argument of the method, if provided; or\n * return the default value for the context variable, if it was created\n   with one; or\n * raise a LookupError."),
            ("set", O, var_set, "set($self, value, /)\n--\n\nCall to set a new value for the context variable in the current context.\n\nThe required value argument is the new value for the context variable.\n\nReturns a Token object that can be used to restore the variable to its previous\nvalue via the `ContextVar.reset()` method."),
            ("reset", O, var_reset, "reset($self, token, /)\n--\n\nReset the context variable.\n\nThe variable is reset to the value it had before the `ContextVar.set()` that\ncreated the token was used."),
            ("__class_getitem__", O, fc::class_getitem, "See PEP 585"; METH_CLASS),
        ];
        (*t).tp_getset = crate::getset_table![("name", var_get_name)];

        let t = token_type();
        fc::init_robj_type::<Token>(t, c"_contextvars.Token", 0);
        (*t).tp_new = Some(token_new);
        (*t).tp_repr = Some(token_repr);
        (*t).tp_hash = Some(crate::types::object_type::object_hash);
        (*t).tp_methods = mdefs![
            ("__enter__", NOARGS, token_enter, "Enter into Token context manager."),
            ("__exit__", FAST, token_exit, "__exit__($self, /, *args)\n--\n\nLeave the with block: reset the ContextVar to its value before set()."),
            ("__class_getitem__", O, fc::class_getitem, "See PEP 585"; METH_CLASS),
        ];
        (*t).tp_getset = crate::getset_table![("var", token_get_var), ("old_value", token_get_old)];

        let t = &raw mut _PyContextTokenMissing_Type;
        fc::init_type(t, c"Token.MISSING", std::mem::size_of::<PyObject>(), 0);
        (*t).tp_repr = Some(missing_repr);
        for t in [ctx_type(), var_type(), token_type(), &raw mut _PyContextTokenMissing_Type] {
            crate::types::typeobject::type_ready(t)?;
        }
        let m = crate::alloc::alloc_object(&raw mut _PyContextTokenMissing_Type, std::mem::size_of::<PyObject>());
        make_immortal(m);
        MISSING.store(m, Release);
        crate::types::typeobject::type_dict_set(token_type(), "MISSING", &*m);
    }
    Ok(())
}

pub fn init_module() -> PyResult {
    ensure_types()?;
    let methods = mdefs![("copy_context", NOARGS, copy_context, "copy_context($module, /)\n--\n\n")];
    let (m, d) = fc::new_module("_contextvars", methods, Some("Context Variables"))?;
    fc::add_type(&d, "Context", ctx_type())?;
    fc::add_type(&d, "ContextVar", var_type())?;
    fc::add_type(&d, "Token", token_type())?;
    let _ = opt;
    Ok(m)
}
