//! `_decimal`: module state, signals/exceptions, Context, SignalDict, the current
//! context and the localcontext() manager.

use super::dec::*;
use super::py_dec;
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::typeobject::*;
use crate::types::{bool_, dict, int, list, none, str as pystr, tuple};
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering::Relaxed};
use std::sync::OnceLock;

// ------------------------------- module state ----------------------------------------

pub struct State {
    pub decimal_exception: PyRef,
    pub clamped: PyRef,
    pub invalid_operation: PyRef,
    pub conversion_syntax: PyRef,
    pub division_by_zero: PyRef,
    pub division_impossible: PyRef,
    pub division_undefined: PyRef,
    pub inexact: PyRef,
    pub invalid_context: PyRef,
    pub rounded: PyRef,
    pub subnormal: PyRef,
    pub overflow: PyRef,
    pub underflow: PyRef,
    pub float_operation: PyRef,
    pub round_str: Vec<PyRef>,
    pub default_ctx: PyRef,
    pub basic_ctx: PyRef,
    pub extended_ctx: PyRef,
    pub decimal_tuple: Option<PyRef>,
    pub signal_dict_cls: *mut PyTypeObject,
}

unsafe impl Send for State {}
unsafe impl Sync for State {}

static STATE: OnceLock<State> = OnceLock::new();

#[inline]
pub fn st() -> &'static State {
    STATE.get().expect("_decimal not initialised")
}

/// Signals in SignalDict order, with their condition masks.
pub fn signals() -> [(&'static PyRef, u32); 9] {
    let s = st();
    [
        (&s.invalid_operation, IEEE_INVALID),
        (&s.float_operation, FLOAT_OPERATION),
        (&s.division_by_zero, DIVISION_BY_ZERO),
        (&s.overflow, OVERFLOW),
        (&s.underflow, UNDERFLOW),
        (&s.subnormal, SUBNORMAL),
        (&s.inexact, INEXACT),
        (&s.rounded, ROUNDED),
        (&s.clamped, CLAMPED),
    ]
}

/// Signals in the order used by Context.__repr__.
fn repr_signals() -> [(&'static str, u32); 9] {
    [
        ("Clamped", CLAMPED),
        ("InvalidOperation", IEEE_INVALID),
        ("DivisionByZero", DIVISION_BY_ZERO),
        ("Inexact", INEXACT),
        ("FloatOperation", FLOAT_OPERATION),
        ("Overflow", OVERFLOW),
        ("Rounded", ROUNDED),
        ("Subnormal", SUBNORMAL),
        ("Underflow", UNDERFLOW),
    ]
}

fn conditions() -> [(&'static PyRef, u32); 5] {
    let s = st();
    [
        (&s.invalid_operation, INVALID_OPERATION),
        (&s.conversion_syntax, CONVERSION_SYNTAX),
        (&s.division_impossible, DIVISION_IMPOSSIBLE),
        (&s.division_undefined, DIVISION_UNDEFINED),
        (&s.invalid_context, INVALID_CONTEXT),
    ]
}

/// Condition mask for a signal class (None if not a signal).
pub fn signal_mask(key: &PyObject) -> Option<u32> {
    for (cls, m) in signals() {
        if key.is(cls) {
            return Some(m);
        }
    }
    None
}

/// Build the exception for trapped conditions `t`.
pub fn signal_error(t: u32) -> PyErr {
    let mut exc: Option<&PyRef> = None;
    for (cls, m) in signals() {
        if t & m != 0 {
            exc = Some(cls);
            break;
        }
    }
    let exc = exc.unwrap_or(&st().invalid_operation);
    let mut items: Vec<PyRef> = Vec::new();
    for (cls, m) in conditions() {
        if t & m != 0 {
            items.push(cls.clone());
        }
    }
    for (cls, m) in signals().iter().skip(1) {
        if t & m != 0 {
            items.push((*cls).clone());
        }
    }
    let lst = list::new_list(items);
    match ab::call(exc, &[&lst]) {
        Ok(e) => {
            set_exc(e);
            PyErr
        }
        Err(e) => e,
    }
}

// ------------------------------- Context object ----------------------------------------

#[repr(C)]
pub struct CtxObj {
    ob_base: PyObject,
    pub prec: i64,
    pub emax: i64,
    pub emin: i64,
    pub round: u8,
    pub capitals: u8,
    pub clamp: u8,
    pub flags: AtomicU32,
    pub traps: AtomicU32,
    flags_dict: *mut PyObject,
    traps_dict: *mut PyObject,
}

#[repr(C)]
pub struct SigDictObj {
    ob_base: PyObject,
    ctx: AtomicPtr<CtxObj>,
    traps: bool,
}

#[unsafe(no_mangle)]
pub static mut PyDecContext_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyDecSignalDictMixin_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyDecContextManager_Type: PyTypeObject = PyTypeObject::ZERO;

pub fn context_type() -> *mut PyTypeObject {
    &raw mut PyDecContext_Type
}
fn sigdict_mixin_type() -> *mut PyTypeObject {
    &raw mut PyDecSignalDictMixin_Type
}
fn ctxmgr_type() -> *mut PyTypeObject {
    &raw mut PyDecContextManager_Type
}

#[inline]
pub fn is_context(o: &PyObject) -> bool {
    o.is_type(context_type()) || is_subtype(o.type_ptr(), context_type())
}

#[inline]
pub fn cobj(o: &PyObject) -> &CtxObj {
    unsafe { &*(o.as_ptr() as *const CtxObj) }
}
#[inline]
#[allow(clippy::mut_from_ref)]
fn cobj_mut(o: &PyObject) -> &mut CtxObj {
    unsafe { &mut *(o.as_ptr() as *mut CtxObj) }
}

pub fn round_of(c: &CtxObj) -> Round {
    Round::ALL[c.round as usize]
}

/// Engine context for a Context object.
#[inline]
pub fn engine(c: &PyObject) -> Ctx {
    let co = cobj(c);
    Ctx { prec: co.prec, emax: co.emax, emin: co.emin, round: round_of(co), clamp: co.clamp != 0, status: 0 }
}

/// Merge conditions into the context flags and raise for trapped ones.
#[inline]
pub fn add_status(c: &PyObject, status: u32) -> PyResult<()> {
    if status == 0 {
        return Ok(());
    }
    let co = cobj(c);
    co.flags.fetch_or(status, Relaxed);
    let t = status & co.traps.load(Relaxed);
    if t != 0 {
        return Err(signal_error(t));
    }
    Ok(())
}

fn new_sigdict(ctx: *mut CtxObj, traps: bool) -> PyResult<PyRef> {
    let tp = st_sigdict_cls();
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
        let op = alloc(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        let so = &mut *(op as *mut SigDictObj);
        std::ptr::write(&mut so.ctx, AtomicPtr::new(ctx));
        so.traps = traps;
        Ok(PyRef::from_raw(op))
    }
}

fn st_sigdict_cls() -> *mut PyTypeObject {
    match STATE.get() {
        Some(s) => s.signal_dict_cls,
        None => SIGDICT_CLS.load(Relaxed),
    }
}

static SIGDICT_CLS: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());

/// Allocate a Context of type `tp` with the given settings.
pub fn new_context_typed(tp: *mut PyTypeObject, prec: i64, round: Round, emin: i64, emax: i64, capitals: u8, clamp: u8, flags: u32, traps: u32) -> PyResult<PyRef> {
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
        let op = alloc(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        let co = &mut *(op as *mut CtxObj);
        co.prec = prec;
        co.emax = emax;
        co.emin = emin;
        co.round = Round::ALL.iter().position(|r| *r == round).unwrap() as u8;
        co.capitals = capitals;
        co.clamp = clamp;
        std::ptr::write(&mut co.flags, AtomicU32::new(flags));
        std::ptr::write(&mut co.traps, AtomicU32::new(traps));
        co.flags_dict = std::ptr::null_mut();
        co.traps_dict = std::ptr::null_mut();
        let obj = PyRef::from_raw(op);
        let fd = new_sigdict(co, false)?;
        let td = new_sigdict(co, true)?;
        co.flags_dict = fd.into_raw();
        co.traps_dict = td.into_raw();
        Ok(obj)
    }
}

pub fn new_context(prec: i64, round: Round, emin: i64, emax: i64, capitals: u8, clamp: u8, flags: u32, traps: u32) -> PyResult<PyRef> {
    new_context_typed(context_type(), prec, round, emin, emax, capitals, clamp, flags, traps)
}

unsafe extern "C" fn context_dealloc(o: *mut PyObject) {
    unsafe {
        let tp = (*o).type_ptr();
        let co = &mut *(o as *mut CtxObj);
        for d in [co.flags_dict, co.traps_dict] {
            if !d.is_null() {
                (*(d as *mut SigDictObj)).ctx.store(std::ptr::null_mut(), Relaxed);
                decref(d);
            }
        }
        co.flags_dict = std::ptr::null_mut();
        co.traps_dict = std::ptr::null_mut();
        generic_free_and_type_decref(o, tp);
    }
}

/// Copy of a context (same type, flags and traps copied).
pub fn context_copy(o: &PyObject) -> PyResult<PyRef> {
    let c = cobj(o);
    let tp = o.type_ptr();
    let r = if tp == context_type() {
        new_context(c.prec, round_of(c), c.emin, c.emax, c.capitals, c.clamp, 0, 0)?
    } else {
        let r = ab::call0(type_obj(tp))?;
        if !is_context(&r) {
            raise!(TypeError, "copy produced a non-context object");
        }
        let rc = cobj_mut(&r);
        rc.prec = c.prec;
        rc.emax = c.emax;
        rc.emin = c.emin;
        rc.round = c.round;
        rc.capitals = c.capitals;
        rc.clamp = c.clamp;
        r
    };
    let rc = cobj(&r);
    rc.flags.store(c.flags.load(Relaxed), Relaxed);
    rc.traps.store(c.traps.load(Relaxed), Relaxed);
    Ok(r)
}

// ------------------------------- current context ----------------------------------------

struct Cur(std::cell::Cell<*mut PyObject>);
impl Drop for Cur {
    fn drop(&mut self) {
        // Leak at thread exit: the interpreter may already be gone.
    }
}

thread_local! {
    static CURRENT: Cur = const { Cur(std::cell::Cell::new(std::ptr::null_mut())) };
}

fn is_template(o: &PyObject) -> bool {
    let s = st();
    o.is(&s.default_ctx) || o.is(&s.basic_ctx) || o.is(&s.extended_ctx)
}

/// Current context (borrowed pointer valid while not replaced).
pub fn current() -> PyResult<PyRef> {
    let p = CURRENT.with(|c| c.0.get());
    if !p.is_null() {
        return Ok(unsafe { PyRef::from_borrowed(p) });
    }
    let t = &st().default_ctx;
    let c = context_copy(t)?;
    cobj(&c).flags.store(0, Relaxed);
    set_current(&c);
    Ok(c)
}

fn set_current(c: &PyObject) {
    let newp = c.to_ref().into_raw();
    let old = CURRENT.with(|cur| cur.0.replace(newp));
    if !old.is_null() {
        unsafe { decref(old) };
    }
}

pub fn getcontext(_m: &PyObject) -> PyResult {
    current()
}

pub fn setcontext(_m: &PyObject, v: &PyObject) -> PyResult {
    setcontext_impl(v)?;
    Ok(none::none_ref())
}

fn setcontext_impl(v: &PyObject) -> PyResult<()> {
    if !is_context(v) {
        raise!(TypeError, "argument must be a context");
    }
    if is_template(v) {
        let c = context_copy(v)?;
        cobj(&c).flags.store(0, Relaxed);
        set_current(&c);
    } else {
        set_current(v);
    }
    Ok(())
}

/// Resolve an optional `context` argument.
#[inline]
pub fn ctx_arg(a: Option<&PyObject>) -> PyResult<PyRef> {
    match a {
        None => current(),
        Some(x) if x.is(none::none()) => current(),
        Some(x) => {
            if !is_context(x) {
                raise!(TypeError, "optional argument must be a context");
            }
            Ok(x.to_ref())
        }
    }
}

// ------------------------------- Context attributes ----------------------------------------

fn int_arg(v: &PyObject) -> PyResult<i64> {
    if !int::is_int(v) {
        raise!(TypeError, "an integer is required (got type {})", tname(v));
    }
    match int::as_i64(v) {
        Some(x) => Ok(x),
        None => raise!(OverflowError, "Python int too large to convert to C ssize_t"),
    }
}

pub fn set_prec(c: &PyObject, v: &PyObject) -> PyResult<()> {
    let x = int_arg(v)?;
    if !(1..=MAX_PREC).contains(&x) {
        raise!(ValueError, "valid range for prec is [1, MAX_PREC]");
    }
    cobj_mut(c).prec = x;
    Ok(())
}
pub fn set_emin(c: &PyObject, v: &PyObject) -> PyResult<()> {
    let x = int_arg(v)?;
    if !(MIN_EMIN..=0).contains(&x) {
        raise!(ValueError, "valid range for Emin is [MIN_EMIN, 0]");
    }
    cobj_mut(c).emin = x;
    Ok(())
}
pub fn set_emax(c: &PyObject, v: &PyObject) -> PyResult<()> {
    let x = int_arg(v)?;
    if !(0..=MAX_EMAX).contains(&x) {
        raise!(ValueError, "valid range for Emax is [0, MAX_EMAX]");
    }
    cobj_mut(c).emax = x;
    Ok(())
}
pub fn set_capitals(c: &PyObject, v: &PyObject) -> PyResult<()> {
    let x = int_arg(v)?;
    if x != 0 && x != 1 {
        raise!(ValueError, "valid values for capitals are 0 or 1");
    }
    cobj_mut(c).capitals = x as u8;
    Ok(())
}
pub fn set_clamp(c: &PyObject, v: &PyObject) -> PyResult<()> {
    let x = int_arg(v)?;
    if x != 0 && x != 1 {
        raise!(ValueError, "valid values for clamp are 0 or 1");
    }
    cobj_mut(c).clamp = x as u8;
    Ok(())
}

pub fn round_from_obj(v: &PyObject) -> PyResult<Round> {
    if pystr::is_str(v) {
        if let Some(r) = Round::from_name(&pystr::to_string_lossy(v)) {
            return Ok(r);
        }
    }
    raise!(
        TypeError,
        "rounding must be one of the ROUND_* string constants"
    );
}

pub fn set_rounding(c: &PyObject, v: &PyObject) -> PyResult<()> {
    let r = round_from_obj(v)?;
    cobj_mut(c).round = Round::ALL.iter().position(|x| *x == r).unwrap() as u8;
    Ok(())
}

fn no_delete(v: Option<&PyObject>) -> PyResult<&PyObject> {
    match v {
        Some(v) => Ok(v),
        None => raise!(AttributeError, "context attributes cannot be deleted"),
    }
}

fn g_prec(c: &PyObject) -> PyResult {
    Ok(int::new_int(cobj(c).prec))
}
fn s_prec(c: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    set_prec(c, no_delete(v)?)
}
fn g_emin(c: &PyObject) -> PyResult {
    Ok(int::new_int(cobj(c).emin))
}
fn s_emin(c: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    set_emin(c, no_delete(v)?)
}
fn g_emax(c: &PyObject) -> PyResult {
    Ok(int::new_int(cobj(c).emax))
}
fn s_emax(c: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    set_emax(c, no_delete(v)?)
}
fn g_capitals(c: &PyObject) -> PyResult {
    Ok(int::new_int(cobj(c).capitals as i64))
}
fn s_capitals(c: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    set_capitals(c, no_delete(v)?)
}
fn g_clamp(c: &PyObject) -> PyResult {
    Ok(int::new_int(cobj(c).clamp as i64))
}
fn s_clamp(c: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    set_clamp(c, no_delete(v)?)
}
fn g_rounding(c: &PyObject) -> PyResult {
    Ok(st().round_str[cobj(c).round as usize].clone())
}
fn s_rounding(c: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    set_rounding(c, no_delete(v)?)
}
fn g_flags(c: &PyObject) -> PyResult {
    Ok(unsafe { PyRef::from_borrowed(cobj(c).flags_dict) })
}
fn g_traps(c: &PyObject) -> PyResult {
    Ok(unsafe { PyRef::from_borrowed(cobj(c).traps_dict) })
}
fn s_flags(c: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let m = dict_as_flags(no_delete(v)?)?;
    cobj(c).flags.store(m, Relaxed);
    Ok(())
}
fn s_traps(c: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    let m = dict_as_flags(no_delete(v)?)?;
    cobj(c).traps.store(m, Relaxed);
    Ok(())
}

/// Convert a {signal: bool} mapping (dict or SignalDict) to a condition mask.
pub fn dict_as_flags(v: &PyObject) -> PyResult<u32> {
    if is_sigdict(v) {
        return Ok(sigdict_value(v)?);
    }
    if !dict::is_dict(v) {
        raise!(TypeError, "argument must be a signal dict");
    }
    if dict::dict_len(v) != 9 {
        raise!(KeyError, "invalid signal dict");
    }
    let mut m = 0u32;
    for (cls, mask) in signals() {
        match dict::get_item(v, cls)? {
            Some(x) => {
                if ab::is_true(&x)? {
                    m |= mask;
                }
            }
            None => raise!(KeyError, "invalid signal dict"),
        }
    }
    Ok(m)
}

/// Convert a list of signals to a condition mask.
fn list_as_flags(v: &PyObject) -> PyResult<u32> {
    let items = ab::collect_iterable(v)?;
    let mut m = 0u32;
    for it in items {
        match signal_mask(&it) {
            Some(x) => m |= x,
            None => raise!(KeyError, "invalid error flag"),
        }
    }
    Ok(m)
}

fn flags_arg(v: &PyObject) -> PyResult<u32> {
    if list::is_list(v) {
        list_as_flags(v)
    } else if dict::is_dict(v) || is_sigdict(v) {
        dict_as_flags(v)
    } else {
        raise!(TypeError, "argument must be a list or a signal dict");
    }
}

// ------------------------------- Context construction ----------------------------------------

const CTX_KW: [&str; 8] = ["prec", "rounding", "Emin", "Emax", "capitals", "clamp", "flags", "traps"];

unsafe extern "C" fn context_new(tp: *mut PyTypeObject, _args: *mut PyObject, _kw: *mut PyObject) -> *mut PyObject {
    let r = (|| -> PyResult {
        let (prec, round, emin, emax, capitals, clamp, traps) = match STATE.get() {
            Some(s) => {
                let d = cobj(&s.default_ctx);
                (d.prec, round_of(d), d.emin, d.emax, d.capitals, d.clamp, d.traps.load(Relaxed))
            }
            None => (28, Round::HalfEven, -999999, 999999, 1, 0, IEEE_INVALID | DIVISION_BY_ZERO | OVERFLOW),
        };
        new_context_typed(tp, prec, round, emin, emax, capitals, clamp, 0, traps)
    })();
    into_c(r)
}

unsafe extern "C" fn context_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> std::ffi::c_int {
    unsafe {
        let r = (|| -> PyResult<()> {
            let o = &*o;
            let (v, kwn) = crate::args::kwdict_to_vectorcall(&*args, kw);
            let refs: Vec<&PyObject> = v.iter().map(|x| &**x).collect();
            let p = crate::args::Params::new("Context", &CTX_KW, 0, 0);
            let a = crate::args::parse::<8>(&p, &refs, kwn.as_deref())?;
            context_apply_args(o, &a)
        })();
        int_into_c(r)
    }
}

fn some_not_none(a: Option<&PyObject>) -> Option<&PyObject> {
    match a {
        Some(x) if !x.is(none::none()) => Some(x),
        _ => None,
    }
}

fn context_apply_args(o: &PyObject, a: &[Option<&PyObject>; 8]) -> PyResult<()> {
    if let Some(v) = some_not_none(a[0]) {
        set_prec(o, v)?;
    }
    if let Some(v) = some_not_none(a[1]) {
        set_rounding(o, v)?;
    }
    if let Some(v) = some_not_none(a[2]) {
        set_emin(o, v)?;
    }
    if let Some(v) = some_not_none(a[3]) {
        set_emax(o, v)?;
    }
    if let Some(v) = some_not_none(a[4]) {
        set_capitals(o, v)?;
    }
    if let Some(v) = some_not_none(a[5]) {
        set_clamp(o, v)?;
    }
    if let Some(v) = some_not_none(a[6]) {
        let m = flags_arg(v)?;
        cobj(o).flags.store(m, Relaxed);
    }
    if let Some(v) = some_not_none(a[7]) {
        let m = flags_arg(v)?;
        cobj(o).traps.store(m, Relaxed);
    }
    Ok(())
}

fn signal_names_list(mask: u32) -> String {
    let mut v = vec![];
    for (n, m) in repr_signals() {
        if mask & m != 0 {
            v.push(n);
        }
    }
    format!("[{}]", v.join(", "))
}

unsafe extern "C" fn context_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let c = cobj(&*o);
        let s = format!(
            "Context(prec={}, rounding={}, Emin={}, Emax={}, capitals={}, clamp={}, flags={}, traps={})",
            c.prec,
            round_of(c).name(),
            c.emin,
            c.emax,
            c.capitals,
            c.clamp,
            signal_names_list(c.flags.load(Relaxed)),
            signal_names_list(c.traps.load(Relaxed))
        );
        pystr::new_str(&s).into_raw()
    }
}

fn signal_list_obj(mask: u32) -> PyRef {
    let mut v = vec![];
    for (cls, m) in signals() {
        if mask & m != 0 {
            v.push(cls.clone());
        }
    }
    list::new_list(v)
}

fn ctx_reduce(o: &PyObject) -> PyResult {
    let c = cobj(o);
    let args = tuple::new_tuple(vec![
        int::new_int(c.prec),
        st().round_str[c.round as usize].clone(),
        int::new_int(c.emin),
        int::new_int(c.emax),
        int::new_int(c.capitals as i64),
        int::new_int(c.clamp as i64),
        signal_list_obj(c.flags.load(Relaxed)),
        signal_list_obj(c.traps.load(Relaxed)),
    ]);
    Ok(tuple::new_tuple(vec![type_ref(o.type_ptr()), args]))
}

fn ctx_copy(o: &PyObject) -> PyResult {
    context_copy(o)
}
fn ctx_clear_flags(o: &PyObject) -> PyResult {
    cobj(o).flags.store(0, Relaxed);
    Ok(none::none_ref())
}
fn ctx_clear_traps(o: &PyObject) -> PyResult {
    cobj(o).traps.store(0, Relaxed);
    Ok(none::none_ref())
}
fn ctx_etiny(o: &PyObject) -> PyResult {
    let c = cobj(o);
    Ok(int::new_int(c.emin - c.prec + 1))
}
fn ctx_etop(o: &PyObject) -> PyResult {
    let c = cobj(o);
    Ok(int::new_int(c.emax - c.prec + 1))
}
fn ctx_radix(_o: &PyObject) -> PyResult {
    Ok(py_dec::new_dec(Dec::from_i64(10)))
}
fn ctx_unsafe_setprec(o: &PyObject, v: &PyObject) -> PyResult {
    let x = int_arg(v)?;
    if !(1..=MAX_PREC).contains(&x) {
        raise!(ValueError, "prec out of range");
    }
    cobj_mut(o).prec = x;
    Ok(none::none_ref())
}
fn ctx_unsafe_setemin(o: &PyObject, v: &PyObject) -> PyResult {
    let x = int_arg(v)?;
    if !(MIN_EMIN..=0).contains(&x) {
        raise!(ValueError, "Emin out of range");
    }
    cobj_mut(o).emin = x;
    Ok(none::none_ref())
}
fn ctx_unsafe_setemax(o: &PyObject, v: &PyObject) -> PyResult {
    let x = int_arg(v)?;
    if !(0..=MAX_EMAX).contains(&x) {
        raise!(ValueError, "Emax out of range");
    }
    cobj_mut(o).emax = x;
    Ok(none::none_ref())
}

// ------------------------------- SignalDict ----------------------------------------

pub fn is_sigdict(o: &PyObject) -> bool {
    o.is_type(sigdict_mixin_type()) || is_subtype(o.type_ptr(), sigdict_mixin_type())
}

fn sd_ctx(o: &PyObject) -> PyResult<(&CtxObj, bool)> {
    let so = unsafe { &*(o.as_ptr() as *const SigDictObj) };
    let p = so.ctx.load(Relaxed);
    if p.is_null() {
        raise!(ValueError, "invalid signal dict");
    }
    Ok((unsafe { &*p }, so.traps))
}

fn sigdict_value(o: &PyObject) -> PyResult<u32> {
    let (c, t) = sd_ctx(o)?;
    Ok(if t { c.traps.load(Relaxed) } else { c.flags.load(Relaxed) })
}

unsafe extern "C" fn sigdict_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    unsafe {
        let alloc = (*tp).tp_alloc.unwrap_or(generic_alloc);
        let op = alloc(tp, 0);
        if op.is_null() {
            return op;
        }
        let so = &mut *(op as *mut SigDictObj);
        std::ptr::write(&mut so.ctx, AtomicPtr::new(std::ptr::null_mut()));
        so.traps = false;
        op
    }
}

unsafe extern "C" fn sigdict_dealloc(o: *mut PyObject) {
    unsafe {
        let tp = (*o).type_ptr();
        generic_free_and_type_decref(o, tp);
    }
}

unsafe extern "C" fn sigdict_len(o: *mut PyObject) -> Py_ssize_t {
    unsafe {
        match sd_ctx(&*o) {
            Ok(_) => 9,
            Err(_) => -1,
        }
    }
}

unsafe extern "C" fn sigdict_getitem(o: *mut PyObject, k: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let v = sigdict_value(&*o)?;
            match signal_mask(&*k) {
                Some(m) => Ok(bool_::new_bool(v & m != 0)),
                None => Err(set_err_obj(crate::types::exceptions::KeyError(), (*k).to_ref())),
            }
        })();
        into_c(r)
    }
}

unsafe extern "C" fn sigdict_setitem(o: *mut PyObject, k: *mut PyObject, v: *mut PyObject) -> std::ffi::c_int {
    unsafe {
        let r = (|| -> PyResult<()> {
            let (c, t) = sd_ctx(&*o)?;
            if v.is_null() {
                raise!(ValueError, "signal keys cannot be deleted");
            }
            let m = match signal_mask(&*k) {
                Some(m) => m,
                None => return Err(set_err_obj(crate::types::exceptions::KeyError(), (*k).to_ref())),
            };
            let on = ab::is_true(&*v)?;
            let a = if t { &c.traps } else { &c.flags };
            if on {
                a.fetch_or(m, Relaxed);
            } else {
                a.fetch_and(!m, Relaxed);
            }
            Ok(())
        })();
        int_into_c(r)
    }
}

unsafe extern "C" fn sigdict_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            sigdict_value(&*o)?;
            let keys: Vec<PyRef> = signals().iter().map(|(c, _)| (*c).clone()).collect();
            ab::get_iter(&tuple::new_tuple(keys))
        })();
        into_c(r)
    }
}

unsafe extern "C" fn sigdict_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let v = sigdict_value(&*o)?;
            let mut parts = vec![];
            for (cls, m) in signals() {
                parts.push(format!("<class '{}'>:{}", type_fqname(as_type(cls)), if v & m != 0 { "True" } else { "False" }));
            }
            Ok(pystr::new_str(&format!("{{{}}}", parts.join(", "))))
        })();
        into_c(r)
    }
}

unsafe extern "C" fn sigdict_richcompare(a: *mut PyObject, b: *mut PyObject, op: std::ffi::c_int) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let (a, b) = (&*a, &*b);
            if op != Py_EQ && op != Py_NE {
                return Ok(none::not_implemented().to_ref());
            }
            let va = sigdict_value(a)?;
            let vb = if is_sigdict(b) {
                sigdict_value(b)?
            } else if dict::is_dict(b) || is_mapping_like(b) {
                match mapping_as_flags(b)? {
                    Some(v) => v,
                    None => return Ok(bool_::new_bool(op == Py_NE)),
                }
            } else {
                return Ok(none::not_implemented().to_ref());
            };
            Ok(bool_::new_bool((va == vb) == (op == Py_EQ)))
        })();
        into_c(r)
    }
}

fn is_mapping_like(o: &PyObject) -> bool {
    !list::is_list(o) && !tuple::is_tuple(o) && !pystr::is_str(o) && ab::hasattr(o, &pystr::new_str("items")).unwrap_or(false)
}

/// mapping -> mask, or None if it isn't a valid signal mapping.
fn mapping_as_flags(b: &PyObject) -> PyResult<Option<u32>> {
    let d = if dict::is_dict(b) {
        b.to_ref()
    } else {
        match ab::call(type_obj(dict::dict_type()), &[b]) {
            Ok(d) => d,
            Err(_) => {
                clear_exc();
                return Ok(None);
            }
        }
    };
    if dict::dict_len(&d) != 9 {
        return Ok(None);
    }
    let mut m = 0u32;
    for (cls, mask) in signals() {
        match dict::get_item(&d, cls)? {
            Some(x) => {
                if ab::is_true(&x)? {
                    m |= mask;
                }
            }
            None => return Ok(None),
        }
    }
    Ok(Some(m))
}

fn sd_copy(o: &PyObject) -> PyResult {
    let v = sigdict_value(o)?;
    let d = dict::new_dict();
    for (cls, m) in signals() {
        dict::set_item(&d, cls, &bool_::new_bool(v & m != 0))?;
    }
    Ok(d)
}

fn sd_keys(o: &PyObject) -> PyResult {
    sigdict_value(o)?;
    Ok(list::new_list(signals().iter().map(|(c, _)| (*c).clone()).collect()))
}
fn sd_values(o: &PyObject) -> PyResult {
    let v = sigdict_value(o)?;
    Ok(list::new_list(signals().iter().map(|(_, m)| bool_::new_bool(v & m != 0)).collect()))
}
fn sd_items(o: &PyObject) -> PyResult {
    let v = sigdict_value(o)?;
    Ok(list::new_list(
        signals().iter().map(|(c, m)| tuple::new_tuple(vec![(*c).clone(), bool_::new_bool(v & m != 0)])).collect(),
    ))
}
fn sd_get(o: &PyObject, a: &[&PyObject]) -> PyResult {
    crate::args::check_pos("get", a, 1, 2)?;
    let v = sigdict_value(o)?;
    match signal_mask(a[0]) {
        Some(m) => Ok(bool_::new_bool(v & m != 0)),
        None => Ok(if a.len() > 1 { a[1].to_ref() } else { none::none_ref() }),
    }
}
fn sd_contains_m(o: &PyObject, k: &PyObject) -> PyResult {
    sigdict_value(o)?;
    Ok(bool_::new_bool(signal_mask(k).is_some()))
}
unsafe extern "C" fn sigdict_contains(o: *mut PyObject, k: *mut PyObject) -> std::ffi::c_int {
    unsafe {
        match sigdict_value(&*o) {
            Ok(_) => signal_mask(&*k).is_some() as std::ffi::c_int,
            Err(_) => -1,
        }
    }
}

static mut SD_MAP: PyMappingMethods = PyMappingMethods::ZERO;
static mut SD_SEQ: PySequenceMethods = PySequenceMethods::ZERO;

// ------------------------------- localcontext ----------------------------------------

#[repr(C)]
struct MgrObj {
    ob_base: PyObject,
    local: *mut PyObject,
    global: *mut PyObject,
}

unsafe extern "C" fn mgr_dealloc(o: *mut PyObject) {
    unsafe {
        let tp = (*o).type_ptr();
        let m = &mut *(o as *mut MgrObj);
        for p in [m.local, m.global] {
            if !p.is_null() {
                decref(p);
            }
        }
        generic_free_and_type_decref(o, tp);
    }
}

pub fn localcontext(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let (pos, kws) = crate::args::split(args, kw);
    if pos.len() > 1 {
        raise!(TypeError, "localcontext() takes at most 1 positional argument ({} given)", pos.len());
    }
    let mut ctxarg: Option<&PyObject> = pos.first().copied();
    let mut opts: [Option<&PyObject>; 8] = [None; 8];
    for (k, v) in kws {
        let ks = pystr::to_string_lossy(k);
        if ks == "ctx" {
            if ctxarg.is_some() {
                raise!(TypeError, "argument for localcontext() given by name ('ctx') and position (1)");
            }
            ctxarg = Some(v);
            continue;
        }
        match CTX_KW.iter().position(|n| *n == ks) {
            Some(i) => opts[i] = Some(v),
            None => raise!(TypeError, "'{}' is an invalid keyword argument for this function", ks),
        }
    }
    let global = current()?;
    let base = match ctxarg {
        None => global.clone(),
        Some(x) if x.is(none::none()) => global.clone(),
        Some(x) => {
            if !is_context(x) {
                raise!(TypeError, "optional argument must be a context");
            }
            x.to_ref()
        }
    };
    let local = context_copy(&base)?;
    // validate & apply keyword overrides (rounding/flags/traps must be proper types)
    if let Some(v) = opts[1] {
        if !pystr::is_str(v) {
            round_from_obj(v)?;
        }
    }
    for i in [6, 7] {
        if let Some(v) = opts[i] {
            if !(list::is_list(v) || dict::is_dict(v) || is_sigdict(v)) {
                raise!(TypeError, "argument must be a list or a signal dict");
            }
        }
    }
    context_apply_args(&local, &opts)?;
    unsafe {
        let tp = ctxmgr_type();
        let op = generic_alloc(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        let m = &mut *(op as *mut MgrObj);
        m.local = local.into_raw();
        m.global = global.into_raw();
        Ok(PyRef::from_raw(op))
    }
}

fn mgr_enter(o: &PyObject) -> PyResult {
    let m = unsafe { &*(o.as_ptr() as *const MgrObj) };
    let local = unsafe { &*m.local };
    setcontext_impl(local)?;
    Ok(local.to_ref())
}

fn mgr_exit(o: &PyObject, _a: &[&PyObject]) -> PyResult {
    let m = unsafe { &*(o.as_ptr() as *const MgrObj) };
    setcontext_impl(unsafe { &*m.global })?;
    Ok(none::none_ref())
}

// ------------------------------- IEEEContext ----------------------------------------

pub fn ieee_context(_m: &PyObject, v: &PyObject) -> PyResult {
    let bits = match int::as_i64(v) {
        Some(b) if int::is_int(v) => b,
        _ => {
            if !int::is_int(v) {
                raise!(TypeError, "an integer is required");
            }
            raise!(ValueError, "argument must be a multiple of 32, with a maximum of 512");
        }
    };
    if bits <= 0 || bits > 512 || bits % 32 != 0 {
        raise!(ValueError, "argument must be a multiple of 32, with a maximum of 512");
    }
    let prec = 9 * (bits / 32) - 2;
    let emax = 3i64 << (bits / 16 + 3);
    new_context(prec, Round::HalfEven, 1 - emax, emax, 1, 1, 0, 0)
}

// ------------------------------- type setup ----------------------------------------

static mut CTX_DOC: &str = "";

pub fn init_types() -> PyResult<()> {
    unsafe {
        let t = &mut *context_type();
        t.tp_name = c"decimal.Context".as_ptr();
        t.tp_basicsize = std::mem::size_of::<CtxObj>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_IMMUTABLETYPE;
        t.tp_new = Some(context_new);
        t.tp_init = Some(context_init);
        t.tp_dealloc = Some(context_dealloc);
        t.tp_repr = Some(context_repr);
        t.tp_alloc = Some(generic_alloc);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_setattro = Some(object_generic_setattro);
        let _ = &raw const CTX_DOC;
        t.tp_doc = c"Context(prec=None, rounding=None, Emin=None, Emax=None, capitals=None, clamp=None, flags=None, traps=None)\n--\n\nArithmetic settings: precision, rounding mode, exponent range, exponent\nclamping and the output case of the exponent letter, together with the\nsticky status flags and the set of signals that raise (traps).  Arguments\nleft as None are taken from DefaultContext.".as_ptr();
        t.tp_getset = crate::getset_table![
            ("prec", g_prec, s_prec),
            ("Emax", g_emax, s_emax),
            ("Emin", g_emin, s_emin),
            ("rounding", g_rounding, s_rounding),
            ("capitals", g_capitals, s_capitals),
            ("clamp", g_clamp, s_clamp),
            ("flags", g_flags, s_flags),
            ("traps", g_traps, s_traps),
        ];
        let mut methods = super::py_dec::context_methods();
        methods.extend(vec![
            crate::methods::def("Etiny", crate::methods::noargs(ctx_etiny), METH_NOARGS, "Etiny($self, /)\n--\n\nSmallest exponent of a subnormal number: Emin - prec + 1."),
            crate::methods::def("Etop", crate::methods::noargs(ctx_etop), METH_NOARGS, "Etop($self, /)\n--\n\nLargest exponent of a clamped number: Emax - prec + 1."),
            crate::methods::def("radix", crate::methods::noargs(ctx_radix), METH_NOARGS, "radix($self, /)\n--\n\nDecimal(10)."),
            crate::methods::def("clear_flags", crate::methods::noargs(ctx_clear_flags), METH_NOARGS, "clear_flags($self, /)\n--\n\nSet every flag to False."),
            crate::methods::def("clear_traps", crate::methods::noargs(ctx_clear_traps), METH_NOARGS, "clear_traps($self, /)\n--\n\nSet every trap to False."),
            crate::methods::def("copy", crate::methods::noargs(ctx_copy), METH_NOARGS, "copy($self, /)\n--\n\nA new context with the same settings, flags and traps."),
            crate::methods::def("__copy__", crate::methods::noargs(ctx_copy), METH_NOARGS, ""),
            crate::methods::def("__reduce__", crate::methods::noargs(ctx_reduce), METH_NOARGS, ""),
            crate::methods::def("_unsafe_setprec", crate::methods::o(ctx_unsafe_setprec), METH_O, ""),
            crate::methods::def("_unsafe_setemin", crate::methods::o(ctx_unsafe_setemin), METH_O, ""),
            crate::methods::def("_unsafe_setemax", crate::methods::o(ctx_unsafe_setemax), METH_O, ""),
        ]);
        t.tp_methods = crate::methods::table(methods);
        type_ready(context_type())?;

        let t = &mut *sigdict_mixin_type();
        t.tp_name = c"decimal.SignalDictMixin".as_ptr();
        t.tp_basicsize = std::mem::size_of::<SigDictObj>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_IMMUTABLETYPE;
        t.tp_new = Some(sigdict_new);
        t.tp_dealloc = Some(sigdict_dealloc);
        t.tp_alloc = Some(generic_alloc);
        t.tp_repr = Some(sigdict_repr);
        t.tp_richcompare = Some(sigdict_richcompare);
        t.tp_iter = Some(sigdict_iter);
        t.tp_hash = Some(crate::types::slots::hash_not_implemented);
        t.tp_getattro = Some(object_generic_getattro);
        let m = &mut *(&raw mut SD_MAP);
        m.mp_length = Some(sigdict_len);
        m.mp_subscript = Some(sigdict_getitem);
        m.mp_ass_subscript = Some(sigdict_setitem);
        t.tp_as_mapping = m;
        let s = &mut *(&raw mut SD_SEQ);
        s.sq_contains = Some(sigdict_contains);
        t.tp_as_sequence = s;
        t.tp_methods = crate::mdefs![
            ("copy", NOARGS, sd_copy),
            ("keys", NOARGS, sd_keys),
            ("values", NOARGS, sd_values),
            ("items", NOARGS, sd_items),
            ("get", FAST, sd_get),
            ("__contains__", O, sd_contains_m),
        ];
        type_ready(sigdict_mixin_type())?;
        SIGDICT_CLS.store(sigdict_mixin_type(), Relaxed);

        let t = &mut *ctxmgr_type();
        t.tp_name = c"decimal.ContextManager".as_ptr();
        t.tp_basicsize = std::mem::size_of::<MgrObj>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_DISALLOW_INSTANTIATION | Py_TPFLAGS_IMMUTABLETYPE;
        t.tp_dealloc = Some(mgr_dealloc);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_doc = c"Object returned by localcontext(); installs a context for a with-block.".as_ptr();
        t.tp_methods = crate::mdefs![
            ("__enter__", NOARGS, mgr_enter),
            ("__exit__", FAST, mgr_exit),
        ];
        type_ready(ctxmgr_type())?;
    }
    Ok(())
}

// ------------------------------- state init ----------------------------------------

fn make_exc(name: &str, bases: Vec<PyRef>, doc: &str) -> PyResult<PyRef> {
    let ns = dict::new_dict();
    dict::set_item_str(&ns, "__module__", &pystr::new_str("decimal"))?;
    dict::set_item_str(&ns, "__doc__", &pystr::new_str(doc))?;
    let tt = type_obj(type_type());
    ab::call(tt, &[&pystr::new_str(name), &tuple::new_tuple(bases), &ns])
}

pub fn init_state() -> PyResult<()> {
    if STATE.get().is_some() {
        return Ok(());
    }
    use crate::types::exceptions as ex;
    let arith = type_ref(ex::ArithmeticError());
    let zde = type_ref(ex::ZeroDivisionError());
    let te = type_ref(ex::TypeError());
    let de = make_exc("DecimalException", vec![arith], "Root of the decimal signal hierarchy; derives from ArithmeticError.")?;
    let clamped = make_exc("Clamped", vec![de.clone()], "Signal: an exponent was adjusted to fit the context limits.")?;
    let inv = make_exc("InvalidOperation", vec![de.clone()], "Signal: the operation has no meaningful result (the result is NaN).")?;
    let conv = make_exc("ConversionSyntax", vec![inv.clone()], "Condition (a kind of InvalidOperation): a string is not a valid number.")?;
    let dbz = make_exc("DivisionByZero", vec![de.clone(), zde.clone()], "Signal: a finite nonzero number was divided by zero.")?;
    let dimp = make_exc("DivisionImpossible", vec![inv.clone()], "Condition (a kind of InvalidOperation): an integer quotient does not fit the precision.")?;
    let dund = make_exc("DivisionUndefined", vec![inv.clone(), zde], "Condition (a kind of InvalidOperation): zero divided by zero.")?;
    let inexact = make_exc("Inexact", vec![de.clone()], "Signal: the result was rounded and some nonzero digits were lost.")?;
    let ictx = make_exc("InvalidContext", vec![inv.clone()], "Condition (a kind of InvalidOperation): the context settings are unusable.")?;
    let rounded = make_exc("Rounded", vec![de.clone()], "Signal: digits were removed from the result (possibly only zeros).")?;
    let subn = make_exc("Subnormal", vec![de.clone()], "Signal: the result's adjusted exponent is below Emin.")?;
    let ovf = make_exc("Overflow", vec![inexact.clone(), rounded.clone()], "Signal: the result's magnitude exceeds the largest finite number of the context.")?;
    let unf = make_exc("Underflow", vec![inexact.clone(), rounded.clone(), subn.clone()], "Signal: a subnormal result was also inexact.")?;
    let fop = make_exc("FloatOperation", vec![de.clone(), te], "Signal: a float was mixed with Decimals (construction or ordering comparison).")?;
    let round_str: Vec<PyRef> = Round::ALL.iter().map(|r| pystr::intern_str(r.name())).collect();
    let default_ctx = new_context(28, Round::HalfEven, -999999, 999999, 1, 0, 0, IEEE_INVALID | DIVISION_BY_ZERO | OVERFLOW)?;
    let basic_ctx = new_context(9, Round::HalfUp, -999999, 999999, 1, 0, 0, IEEE_INVALID | DIVISION_BY_ZERO | OVERFLOW | UNDERFLOW | CLAMPED)?;
    let extended_ctx = new_context(9, Round::HalfEven, -999999, 999999, 1, 0, 0, 0)?;
    // DecimalTuple namedtuple and SignalDict(MutableMapping) need the stdlib
    let decimal_tuple = (|| -> PyResult<PyRef> {
        let coll = crate::import::import_absolute("collections")?;
        let nt = ab::getattr_str(&coll, "namedtuple")?;
        let kwn = tuple::new_tuple(vec![pystr::new_str("module")]);
        ab::call_kw(&nt, &[&pystr::new_str("DecimalTuple"), &pystr::new_str("sign digits exponent"), &pystr::new_str("decimal")], Some(&kwn))
    })()
    .map_err(|_| clear_exc())
    .ok();
    let sd_cls = (|| -> PyResult<PyRef> {
        let abc = crate::import::import_absolute("collections.abc")?;
        let mm = ab::getattr_str(&abc, "MutableMapping")?;
        let ns = dict::new_dict();
        dict::set_item_str(&ns, "__module__", &pystr::new_str("decimal"))?;
        dict::set_item_str(&ns, "__slots__", &tuple::new_tuple(vec![]))?;
        let tt = type_obj(type_type());
        let bases = tuple::new_tuple(vec![type_ref(sigdict_mixin_type()), mm]);
        ab::call(tt, &[&pystr::new_str("SignalDict"), &bases, &ns])
    })()
    .map_err(|_| clear_exc())
    .ok();
    let sd_ptr = match &sd_cls {
        Some(c) => {
            let p = as_type(c);
            std::mem::forget(c.clone());
            p
        }
        None => sigdict_mixin_type(),
    };
    // contexts created before the SignalDict class existed use the mixin; rebuild them
    SIGDICT_CLS.store(sd_ptr, Relaxed);
    let default_ctx = rebuild_ctx(default_ctx)?;
    let basic_ctx = rebuild_ctx(basic_ctx)?;
    let extended_ctx = rebuild_ctx(extended_ctx)?;
    let s = State {
        decimal_exception: de,
        clamped,
        invalid_operation: inv,
        conversion_syntax: conv,
        division_by_zero: dbz,
        division_impossible: dimp,
        division_undefined: dund,
        inexact,
        invalid_context: ictx,
        rounded,
        subnormal: subn,
        overflow: ovf,
        underflow: unf,
        float_operation: fop,
        round_str,
        default_ctx,
        basic_ctx,
        extended_ctx,
        decimal_tuple,
        signal_dict_cls: sd_ptr,
    };
    let _ = STATE.set(s);
    // register Decimal as numbers.Number
    let _ = (|| -> PyResult<()> {
        let numbers = crate::import::import_absolute("numbers")?;
        let num = ab::getattr_str(&numbers, "Number")?;
        ab::call_method_str(&num, "register", &[type_obj(super::py_dec::decimal_type())])?;
        Ok(())
    })()
    .map_err(|_| clear_exc());
    Ok(())
}

fn rebuild_ctx(c: PyRef) -> PyResult<PyRef> {
    let co = cobj(&c);
    new_context(co.prec, round_of(co), co.emin, co.emax, co.capitals, co.clamp, co.flags.load(Relaxed), co.traps.load(Relaxed))
}

pub fn fill_module(d: &PyObject) -> PyResult<()> {
    let s = st();
    let set = |k: &str, v: &PyObject| dict::set_item_str(d, k, v);
    set("Decimal", type_obj(super::py_dec::decimal_type()))?;
    set("Context", type_obj(context_type()))?;
    if let Some(dt) = &s.decimal_tuple {
        set("DecimalTuple", dt)?;
    }
    set("DefaultContext", &s.default_ctx)?;
    set("BasicContext", &s.basic_ctx)?;
    set("ExtendedContext", &s.extended_ctx)?;
    for (n, v) in [
        ("DecimalException", &s.decimal_exception),
        ("Clamped", &s.clamped),
        ("InvalidOperation", &s.invalid_operation),
        ("DivisionByZero", &s.division_by_zero),
        ("Inexact", &s.inexact),
        ("Rounded", &s.rounded),
        ("Subnormal", &s.subnormal),
        ("Overflow", &s.overflow),
        ("Underflow", &s.underflow),
        ("FloatOperation", &s.float_operation),
        ("DivisionImpossible", &s.division_impossible),
        ("InvalidContext", &s.invalid_context),
        ("ConversionSyntax", &s.conversion_syntax),
        ("DivisionUndefined", &s.division_undefined),
    ] {
        set(n, v)?;
    }
    for (i, r) in Round::ALL.iter().enumerate() {
        set(r.name(), &s.round_str[i])?;
    }
    set("MAX_PREC", &int::new_int(MAX_PREC))?;
    set("MAX_EMAX", &int::new_int(MAX_EMAX))?;
    set("MIN_EMIN", &int::new_int(MIN_EMIN))?;
    set("MIN_ETINY", &int::new_int(MIN_ETINY))?;
    set("IEEE_CONTEXT_MAX_BITS", &int::new_int(512))?;
    set("HAVE_THREADS", &bool_::new_bool(true))?;
    set("HAVE_CONTEXTVAR", &bool_::new_bool(true))?;
    set("SPEC_VERSION", &pystr::new_str("1.70"))?;
    set("__libmpdec_version__", &pystr::new_str("2.5.1"))?;
    Ok(())
}
