//! Async generator protocol (PEP 525): `__anext__`/`asend`/`athrow`/`aclose` awaitables,
//! `async_generator_wrapped_value` (values produced by `yield`, as opposed to values
//! passed through from `await`), firstiter/finalizer hooks (`sys.set_asyncgen_hooks`),
//! and coroutine origin tracking (`sys.set_coroutine_origin_tracking_depth`, `cr_origin`).
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::types::exceptions as exc;
use crate::types::genobject::{self as gobj, SendResult, g};
use crate::types::lang_util::{self as lu, field, field_ref, set_field};
use crate::types::typeobject::*;
use crate::types::{bool_, int, none, str as pystr, tuple};
use crate::raise;
use std::cell::{Cell, RefCell};

pub static mut WRAPPED_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut ASEND_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut ATHROW_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut HOOKS_TYPE: PyTypeObject = PyTypeObject::ZERO;

const STATE_INIT: i64 = 0;
const STATE_ITER: i64 = 1;
const STATE_CLOSED: i64 = 2;

// asend / athrow layout: gen, value-or-args, state (int object), weakreflist-free
#[repr(C)]
struct Awaitable {
    ob_base: PyObject,
    agen: *mut PyObject,
    arg: *mut PyObject, // asend: value to send; athrow: args tuple or null (aclose)
    state: *mut PyObject,
}
const A_GEN: usize = 0;
/// (remote1) where the generator pointer of an asend/athrow object lives (debug table)
pub(crate) const AWAITABLE_GEN_OFFSET: usize = std::mem::offset_of!(Awaitable, agen);
const A_ARG: usize = 1;
const A_STATE: usize = 2;

thread_local! {
    static FIRSTITER: RefCell<Option<PyRef>> = const { RefCell::new(None) };
    static FINALIZER: RefCell<Option<PyRef>> = const { RefCell::new(None) };
    static ORIGIN_DEPTH: Cell<i64> = const { Cell::new(0) };
}

fn wrapped_type() -> *mut PyTypeObject {
    &raw mut WRAPPED_TYPE
}
fn is_wrapped(o: &PyObject) -> bool {
    o.is_type(wrapped_type())
}

// (perf5) Per-thread free lists for the objects every async generator step creates
// (a wrapped value and an asend awaitable per level): reuse skips the generic
// allocation and free paths. Kept objects are untracked, with their fields cleared.
const FL_CAP: usize = 32;
const FL_WRAPPED: usize = 0;
const FL_ASEND: usize = 1;
struct FreeList {
    n: usize,
    items: [*mut PyObject; FL_CAP],
}
#[thread_local]
static mut FREE_LISTS: [FreeList; 2] = [const { FreeList { n: 0, items: [std::ptr::null_mut(); FL_CAP] } }; 2];

fn fl_alloc(k: usize, tp: *mut PyTypeObject) -> PyResult {
    unsafe {
        let fl = &mut (*(&raw mut FREE_LISTS))[k];
        if fl.n > 0 && !crate::modules::_tracemalloc::TRACING.load(std::sync::atomic::Ordering::Relaxed) {
            fl.n -= 1;
            let op = fl.items[fl.n];
            crate::alloc::reinit_header(op);
            crate::gc::track(op);
            return Ok(PyRef::from_raw(op));
        }
    }
    lu::alloc(tp)
}

/// Keep `op` (untracked, fields cleared) for reuse; false: the caller frees it.
unsafe fn fl_keep(k: usize, op: *mut PyObject) -> bool {
    unsafe {
        let fl = &mut (*(&raw mut FREE_LISTS))[k];
        if fl.n < FL_CAP && crate::tlab::fast_free_ok() && !crate::modules::_tracemalloc::TRACING.load(std::sync::atomic::Ordering::Relaxed) {
            fl.items[fl.n] = op;
            fl.n += 1;
            return true;
        }
        false
    }
}

/// Free the kept objects (thread exit).
pub fn drain_free_lists() {
    unsafe {
        for (k, tp) in [(FL_WRAPPED, wrapped_type()), (FL_ASEND, &raw mut ASEND_TYPE)] {
            let fl = &mut (*(&raw mut FREE_LISTS))[k];
            while fl.n > 0 {
                fl.n -= 1;
                generic_free_and_type_decref(fl.items[fl.n], tp);
            }
        }
    }
}

/// Untrack `op`, clear its first `n` pointer fields, then keep it in free list `k`
/// or free it.
unsafe fn fl_dealloc(k: usize, op: *mut PyObject, n: usize) {
    unsafe {
        crate::gc::untrack(op);
        let p = (op as *mut u8).add(lu::HEADER) as *mut *mut PyObject;
        for i in 0..n {
            xdecref(std::mem::replace(&mut *p.add(i), std::ptr::null_mut()));
        }
        if !fl_keep(k, op) {
            generic_free_and_type_decref(op, (*op).type_ptr());
        }
    }
}

unsafe extern "C" fn wrapped_dealloc(op: *mut PyObject) {
    unsafe { fl_dealloc(FL_WRAPPED, op, 1) }
}

/// Wrap a value yielded by an async generator (Intrinsic1::AsyncGenWrap).
pub fn wrap_value(v: PyRef) -> PyResult {
    let o = fl_alloc(FL_WRAPPED, wrapped_type())?;
    set_field(&o, 0, Some(v));
    Ok(o)
}

fn state(o: &PyObject) -> i64 {
    field(o, A_STATE).and_then(int::as_i64).unwrap_or(STATE_INIT)
}
fn set_state(o: &PyObject, s: i64) {
    set_field(o, A_STATE, Some(int::new_int(s)));
}
fn agen_of(o: &PyObject) -> PyRef {
    field_ref(o, A_GEN).unwrap()
}

fn init_hooks(o: &PyObject) -> PyResult<()> {
    let gn = g(o);
    if gn.hooks_inited {
        return Ok(());
    }
    gn.hooks_inited = true;
    if let Some(f) = FINALIZER.with(|f| f.borrow().clone()) {
        gobj::gen_set_finalizer(o, Some(f));
    }
    if let Some(f) = FIRSTITER.with(|f| f.borrow().clone()) {
        ab::call(&f, &[o])?;
    }
    Ok(())
}

fn new_awaitable(tp: *mut PyTypeObject, agen: &PyObject, arg: Option<PyRef>) -> PyResult {
    let o = if tp == &raw mut ASEND_TYPE { fl_alloc(FL_ASEND, tp)? } else { lu::alloc(tp)? };
    set_field(&o, A_GEN, Some(agen.to_ref()));
    set_field(&o, A_ARG, arg);
    set_state(&o, STATE_INIT);
    Ok(o)
}

/// Convert a raw send/throw result of the async generator frame (`async_gen_unwrap_value`).
fn unwrap_value(agen: &PyObject, r: PyResult<SendResult>) -> PyResult {
    let gn = g(agen);
    match r {
        Ok(SendResult::Yield(v)) => {
            if is_wrapped(&v) {
                let inner = field_ref(&v, 0).unwrap_or_else(none::none_ref);
                gn.running_async = false;
                return Err(stop_iteration(inner));
            }
            Ok(v)
        }
        Ok(SendResult::Return(_)) => {
            let _ = set_err_noargs(exc::StopAsyncIteration());
            gn.closed = true;
            gn.running_async = false;
            Err(PyErr)
        }
        Err(e) => {
            if exc_matches(exc::StopAsyncIteration()) || exc_matches(exc::GeneratorExit()) {
                gn.closed = true;
            }
            gn.running_async = false;
            Err(e)
        }
    }
}

fn stop_iteration(v: PyRef) -> PyErr {
    if v.is(none::none()) {
        return set_err_noargs(exc::StopIteration());
    }
    // StopIteration(value) — wrap tuples etc. correctly by constructing directly
    match exc::make_exception(exc::StopIteration(), &[v]) {
        Ok(e) => set_exc(e),
        Err(_) => {}
    }
    PyErr
}

/// Result of a plain-value generator method (send/throw) as SendResult.
fn as_send_result(r: PyResult) -> PyResult<SendResult> {
    match r {
        Ok(v) => Ok(SendResult::Yield(v)),
        Err(e) => {
            if exc_matches(exc::StopIteration()) {
                let x = take_exc().unwrap();
                return Ok(SendResult::Return(exc::stop_iteration_value(&x)));
            }
            Err(e)
        }
    }
}

// ---------------------------------------------------------------- async generator methods

unsafe extern "C" fn agen_anext(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        let r = init_hooks(o).and_then(|_| new_awaitable(&raw mut ASEND_TYPE, o, None));
        into_c(r)
    }
}
unsafe extern "C" fn agen_aiter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}
fn agen_asend(o: &PyObject, v: &PyObject) -> PyResult {
    init_hooks(o)?;
    new_awaitable(&raw mut ASEND_TYPE, o, Some(v.to_ref()))
}
fn agen_athrow(o: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.is_empty() || args.len() > 3 {
        if args.is_empty() {
            raise!(TypeError, "athrow expected at least 1 argument, got 0");
        }
        raise!(TypeError, "athrow expected at most 3 arguments, got {}", args.len());
    }
    if args.len() > 1 {
        crate::modules::warnings::warn(
            exc::DeprecationWarning(),
            "the (type, exc, tb) signature of athrow() is deprecated, use the single-arg signature instead.",
            1,
        )?;
    }
    init_hooks(o)?;
    let t = tuple::new_tuple(args.iter().map(|a| a.to_ref()).collect());
    new_awaitable(&raw mut ATHROW_TYPE, o, Some(t))
}
fn agen_aclose(o: &PyObject) -> PyResult {
    init_hooks(o)?;
    new_awaitable(&raw mut ATHROW_TYPE, o, None)
}

fn ag_get_name(o: &PyObject) -> PyResult {
    unsafe { Ok(PyRef::from_borrowed(g(o).name)) }
}
fn ag_set_name(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match v {
        Some(v) if pystr::is_str(v) => {
            let old = std::mem::replace(&mut g(o).name, v.to_ref().into_raw());
            unsafe { xdecref(old) };
            Ok(())
        }
        _ => raise!(TypeError, "__name__ must be set to a string object"),
    }
}
fn ag_get_qualname(o: &PyObject) -> PyResult {
    unsafe { Ok(PyRef::from_borrowed(g(o).qualname)) }
}
fn ag_set_qualname(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match v {
        Some(v) if pystr::is_str(v) => {
            let old = std::mem::replace(&mut g(o).qualname, v.to_ref().into_raw());
            unsafe { xdecref(old) };
            Ok(())
        }
        _ => raise!(TypeError, "__qualname__ must be set to a string object"),
    }
}
fn ag_running(o: &PyObject) -> PyResult {
    Ok(bool_::new_bool(g(o).running_async))
}
fn ag_await(o: &PyObject) -> PyResult {
    Ok(gobj::yieldfrom(o).unwrap_or_else(none::none_ref))
}
fn ag_code(o: &PyObject) -> PyResult {
    unsafe { Ok(PyRef::from_borrowed(g(o).code)) }
}
fn ag_frame(o: &PyObject) -> PyResult {
    gobj::get_frame(o)
}
fn ag_suspended(o: &PyObject) -> PyResult {
    gobj::get_suspended(o)
}
fn ag_sizeof(_o: &PyObject) -> PyResult {
    Ok(int::new_int(std::mem::size_of::<gobj::GenObject>() as i64))
}
fn class_getitem(cls: &PyObject, a: &PyObject) -> PyResult {
    crate::types::genericalias::new_generic_alias(cls, a)
}

pub fn setup_asyncgen_type(tp: *mut PyTypeObject) {
    unsafe {
        let t = &mut *tp;
        static mut AG_ASYNC: PyAsyncMethods = PyAsyncMethods::ZERO;
        (*(&raw mut AG_ASYNC)).am_aiter = Some(agen_aiter);
        (*(&raw mut AG_ASYNC)).am_anext = Some(agen_anext);
        t.tp_as_async = &raw mut AG_ASYNC;
        t.tp_methods = crate::mdefs![
            ("asend", O, agen_asend, "asend($self, value, /)\n--\n\nSend a value into the asynchronous generator; return an awaitable."),
            ("athrow", FAST, agen_athrow, "athrow(value)\nathrow(type[,value[,tb]])\n\nraise exception in generator.\nthe (type, val, tb) signature is deprecated, \nand may be removed in a future version of Python."),
            ("aclose", NOARGS, agen_aclose, "aclose($self, /)\n--\n\nReturn an awaitable that raises GeneratorExit inside the generator."),
            ("__sizeof__", NOARGS, ag_sizeof),
            ("__class_getitem__", O, class_getitem, "See PEP 585"; METH_CLASS),
        ];
        t.tp_getset = crate::getset_table![
            ("__name__", ag_get_name, ag_set_name; "name of the async generator"),
            ("__qualname__", ag_get_qualname, ag_set_qualname; "qualified name of the async generator"),
            ("ag_await", ag_await; "object being awaited on, or None"),
            ("ag_frame", ag_frame),
            ("ag_running", ag_running),
            ("ag_code", ag_code),
            ("ag_suspended", ag_suspended),
        ];
    }
}

// ---------------------------------------------------------------- asend

fn asend_send(o: &PyObject, arg: Option<&PyObject>) -> PyResult {
    let agen = agen_of(o);
    let st = state(o);
    if st == STATE_CLOSED {
        raise!(RuntimeError, "cannot reuse already awaited __anext__()/asend()");
    }
    let mut arg = arg.map(|a| a.to_ref());
    if st == STATE_INIT {
        if g(&agen).running_async {
            set_state(o, STATE_CLOSED);
            raise!(RuntimeError, "anext(): asynchronous generator is already running");
        }
        if arg.as_ref().is_none_or(|a| a.is(none::none())) {
            arg = field_ref(o, A_ARG);
        }
        set_state(o, STATE_ITER);
    }
    g(&agen).running_async = true;
    let r = gobj::gen_send_ex(&agen, Some(arg.as_deref().unwrap_or(none::none())), None, false);
    let r = unwrap_value(&agen, r);
    if r.is_err() {
        set_state(o, STATE_CLOSED);
    }
    r
}
/// (jit6, flagged: used by genobject::send_to for `await`/`async for` on an asend
/// object) `asend_send` with the outcome as a SendResult: a value the async generator
/// yielded is Return(value) instead of a StopIteration exception object (one fewer
/// exception allocation per `async for` step per level).
pub fn asend_send_result(o: &PyObject, arg: &PyObject) -> PyResult<SendResult> {
    let agen = agen_of(o);
    let st = state(o);
    if st == STATE_CLOSED {
        raise!(RuntimeError, "cannot reuse already awaited __anext__()/asend()");
    }
    let mut arg = arg.to_ref();
    if st == STATE_INIT {
        if g(&agen).running_async {
            set_state(o, STATE_CLOSED);
            raise!(RuntimeError, "anext(): asynchronous generator is already running");
        }
        if arg.is(none::none()) {
            if let Some(a) = field_ref(o, A_ARG) {
                arg = a;
            }
        }
        set_state(o, STATE_ITER);
    }
    g(&agen).running_async = true;
    let r = gobj::gen_send_ex(&agen, Some(&arg), None, false);
    if let Ok(SendResult::Yield(v)) = &r {
        if is_wrapped(v) {
            let inner = field_ref(v, 0).unwrap_or_else(none::none_ref);
            g(&agen).running_async = false;
            set_state(o, STATE_CLOSED);
            return Ok(SendResult::Return(inner));
        }
    }
    let r = unwrap_value(&agen, r);
    match r {
        Ok(v) => Ok(SendResult::Yield(v)),
        Err(e) => {
            set_state(o, STATE_CLOSED);
            Err(e)
        }
    }
}
fn asend_send_m(o: &PyObject, v: &PyObject) -> PyResult {
    asend_send(o, Some(v))
}
unsafe extern "C" fn asend_iternext(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(asend_send(&*o, None)) }
}
fn asend_throw(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let agen = agen_of(o);
    let st = state(o);
    if st == STATE_CLOSED {
        raise!(RuntimeError, "cannot reuse already awaited __anext__()/asend()");
    }
    if st == STATE_INIT {
        if g(&agen).running_async {
            set_state(o, STATE_CLOSED);
            raise!(RuntimeError, "anext(): asynchronous generator is already running");
        }
        set_state(o, STATE_ITER);
    }
    g(&agen).running_async = true;
    let r = as_send_result(gobj::gen_throw_pub(&agen, args));
    let r = unwrap_value(&agen, r);
    if r.is_err() {
        set_state(o, STATE_CLOSED);
    }
    r
}
fn awaitable_close(o: &PyObject, throw: fn(&PyObject, &[&PyObject]) -> PyResult) -> PyResult {
    if state(o) == STATE_CLOSED {
        return Ok(none::none_ref());
    }
    let ge = type_ref(exc::GeneratorExit());
    match throw(o, &[&ge]) {
        Ok(_) => raise!(RuntimeError, "coroutine ignored GeneratorExit"),
        Err(e) => {
            if exc_matches(exc::StopIteration()) || exc_matches(exc::StopAsyncIteration()) || exc_matches(exc::GeneratorExit()) {
                clear_exc();
                return Ok(none::none_ref());
            }
            Err(e)
        }
    }
}
fn asend_close(o: &PyObject) -> PyResult {
    awaitable_close(o, asend_throw)
}

// ---------------------------------------------------------------- athrow / aclose

fn athrow_already_running(o: &PyObject) -> PyErr {
    set_state(o, STATE_CLOSED);
    if field(o, A_ARG).is_none() {
        err!(RuntimeError, "aclose(): asynchronous generator is already running")
    } else {
        err!(RuntimeError, "athrow(): asynchronous generator is already running")
    }
}

fn athrow_send(o: &PyObject, arg: Option<&PyObject>) -> PyResult {
    let agen = agen_of(o);
    let st = state(o);
    if st == STATE_CLOSED {
        raise!(RuntimeError, "cannot reuse already awaited aclose()/athrow()");
    }
    let gn = g(&agen);
    let finished = gn.frame.is_null()
        || unsafe {
            matches!((*gn.frame).state, crate::interp::frame::FrameState::Completed | crate::interp::frame::FrameState::Cleared)
        };
    if finished {
        set_state(o, STATE_CLOSED);
        return Err(set_err_noargs(exc::StopIteration()));
    }
    let args = field_ref(o, A_ARG);
    let aclose_mode = args.is_none();
    if st == STATE_INIT {
        if gn.running_async {
            return Err(athrow_already_running(o));
        }
        if gn.closed {
            set_state(o, STATE_CLOSED);
            return Err(set_err_noargs(exc::StopAsyncIteration()));
        }
        if let Some(a) = arg {
            if !a.is(none::none()) {
                raise!(RuntimeError, "can't send non-None value to a just-started coroutine");
            }
        }
        set_state(o, STATE_ITER);
        gn.running_async = true;
        let r = if aclose_mode {
            gn.closed = true;
            let ge = type_ref(exc::GeneratorExit());
            let r = as_send_result(gobj::gen_throw_impl(&agen, &ge, None, None, false));
            match r {
                Ok(SendResult::Yield(v)) if is_wrapped(&v) => return Err(yield_close(o, &agen)),
                other => other,
            }
        } else {
            let a = args.as_ref().unwrap();
            let items = tuple::tuple_items(a);
            let r = as_send_result(gobj::gen_throw_impl(&agen, items[0], items.get(1).copied(), items.get(2).copied(), false));
            return athrow_result(o, &agen, unwrap_value(&agen, r), aclose_mode);
        };
        return athrow_result(o, &agen, aclose_passthrough(r), aclose_mode);
    }
    // STATE_ITER
    let r = gobj::gen_send_ex(&agen, Some(arg.unwrap_or(none::none())), None, false);
    if !aclose_mode {
        let r = unwrap_value(&agen, r);
        if r.is_err() {
            set_state(o, STATE_CLOSED);
        }
        return r;
    }
    match r {
        Ok(SendResult::Yield(v)) if is_wrapped(&v) => Err(yield_close(o, &agen)),
        other => athrow_result(o, &agen, aclose_passthrough(other), aclose_mode),
    }
}

fn aclose_passthrough(r: PyResult<SendResult>) -> PyResult {
    match r {
        Ok(SendResult::Yield(v)) => Ok(v),
        Ok(SendResult::Return(_)) => Err(set_err_noargs(exc::StopAsyncIteration())),
        Err(e) => Err(e),
    }
}

fn yield_close(o: &PyObject, agen: &PyObject) -> PyErr {
    g(agen).running_async = false;
    set_state(o, STATE_CLOSED);
    err!(RuntimeError, "async generator ignored GeneratorExit")
}

/// `check_error` handling of athrow_send.
fn athrow_result(o: &PyObject, agen: &PyObject, r: PyResult, aclose_mode: bool) -> PyResult {
    match r {
        Ok(v) => Ok(v),
        Err(e) => {
            g(agen).running_async = false;
            set_state(o, STATE_CLOSED);
            if aclose_mode && (exc_matches(exc::StopAsyncIteration()) || exc_matches(exc::GeneratorExit())) {
                clear_exc();
                return Err(set_err_noargs(exc::StopIteration()));
            }
            Err(e)
        }
    }
}

fn athrow_send_m(o: &PyObject, v: &PyObject) -> PyResult {
    athrow_send(o, Some(v))
}
unsafe extern "C" fn athrow_iternext(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(athrow_send(&*o, None)) }
}
fn athrow_throw(o: &PyObject, args: &[&PyObject]) -> PyResult {
    let agen = agen_of(o);
    let st = state(o);
    if st == STATE_CLOSED {
        raise!(RuntimeError, "cannot reuse already awaited aclose()/athrow()");
    }
    let aclose_mode = field(o, A_ARG).is_none();
    if st == STATE_INIT {
        if g(&agen).running_async {
            return Err(athrow_already_running(o));
        }
        set_state(o, STATE_ITER);
        g(&agen).running_async = true;
    }
    let r = gobj::gen_throw_pub(&agen, args);
    if !aclose_mode {
        let r = unwrap_value(&agen, as_send_result(r));
        if r.is_err() {
            set_state(o, STATE_CLOSED);
        }
        return r;
    }
    match r {
        Ok(v) if is_wrapped(&v) => Err(yield_close(o, &agen)),
        Ok(v) => Ok(v),
        Err(e) => {
            g(&agen).running_async = false;
            set_state(o, STATE_CLOSED);
            if exc_matches(exc::StopAsyncIteration()) || exc_matches(exc::GeneratorExit()) {
                clear_exc();
                return Err(set_err_noargs(exc::StopIteration()));
            }
            Err(e)
        }
    }
}
fn athrow_close(o: &PyObject) -> PyResult {
    awaitable_close(o, athrow_throw)
}

unsafe extern "C" fn self_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}

/// tp_finalize of asend/athrow: warn about never-awaited awaitables.
/// (jit6, flagged) An asend/athrow that already completed has nothing to finalize:
/// skip the finalizer protocol on dealloc (one per `async for` step per level).
unsafe extern "C" fn awaitable_dealloc(o: *mut PyObject) {
    unsafe {
        if state(&*o) == STATE_CLOSED {
            if (*o).is_type(&raw mut ASEND_TYPE) {
                fl_dealloc(FL_ASEND, o, 3);
            } else {
                lu::ptrs_dealloc_unfinalized(o);
            }
        } else {
            lu::ptrs_dealloc(o);
        }
    }
}

unsafe extern "C" fn awaitable_finalize(o: *mut PyObject) {
    unsafe {
        let o = &*o;
        let st = state(o);
        let agen = match field_ref(o, A_GEN) {
            Some(a) => a,
            None => return,
        };
        if st == STATE_ITER {
            // abandoned while running: the generator is no longer driven by it
            g(&agen).running_async = false;
        }
        if st != STATE_INIT {
            return;
        }
        let method = if o.is_type(&raw mut ASEND_TYPE) {
            "asend"
        } else if field(o, A_ARG).is_some() {
            "athrow"
        } else {
            "aclose"
        };
        let saved = take_exc();
        let qn = pystr::to_string_lossy(&*g(&agen).qualname);
        let msg = format!("coroutine method '{}' of '{}' was never awaited", method, qn);
        if crate::modules::warnings::warn(exc::RuntimeWarning(), &msg, 1).is_err() {
            crate::modules::sys::write_unraisable_with_obj("Exception ignored while finalizing", Some(o));
        }
        if let Some(s) = saved {
            set_exc(s);
        }
    }
}

// ---------------------------------------------------------------- sys hooks

fn hooks_getter(o: &PyObject, i: usize) -> PyResult {
    Ok(tuple::tuple_get(o, i).to_ref())
}
fn hooks_firstiter(o: &PyObject) -> PyResult {
    hooks_getter(o, 0)
}
fn hooks_finalizer(o: &PyObject) -> PyResult {
    hooks_getter(o, 1)
}
unsafe extern "C" fn hooks_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let o = &*o;
        let r = (|| -> PyResult {
            let a = ab::repr_string(tuple::tuple_get(o, 0))?;
            let b = ab::repr_string(tuple::tuple_get(o, 1))?;
            Ok(pystr::new_str(&format!("asyncgen_hooks(firstiter={}, finalizer={})", a, b)))
        })();
        into_c(r)
    }
}

fn sys_get_asyncgen_hooks(_m: &PyObject) -> PyResult {
    let fi = FIRSTITER.with(|f| f.borrow().clone()).unwrap_or_else(none::none_ref);
    let fz = FINALIZER.with(|f| f.borrow().clone()).unwrap_or_else(none::none_ref);
    unsafe {
        let op = tuple::alloc_tuple_of(&raw mut HOOKS_TYPE, 2);
        let items = tuple::item_ptr(op);
        *items = fi.into_raw();
        *items.add(1) = fz.into_raw();
        Ok(PyRef::from_raw(op))
    }
}

fn sys_set_asyncgen_hooks(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let p = crate::args::Params::new("set_asyncgen_hooks", &["firstiter", "finalizer"], 0, 0);
    let [firstiter, finalizer] = crate::args::parse::<2>(&p, args, kw)?;
    if let Some(f) = finalizer {
        if !f.is(none::none()) && !ab::is_callable(f) {
            raise!(TypeError, "callable finalizer expected, got {}", tname(f));
        }
    }
    if let Some(f) = firstiter {
        if !f.is(none::none()) && !ab::is_callable(f) {
            raise!(TypeError, "callable firstiter expected, got {}", tname(f));
        }
    }
    if let Some(f) = finalizer {
        let v = if f.is(none::none()) { None } else { Some(f.to_ref()) };
        FINALIZER.with(|x| *x.borrow_mut() = v);
    }
    if let Some(f) = firstiter {
        let v = if f.is(none::none()) { None } else { Some(f.to_ref()) };
        FIRSTITER.with(|x| *x.borrow_mut() = v);
    }
    Ok(none::none_ref())
}

fn sys_set_coro_depth(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let p = crate::args::Params::new("set_coroutine_origin_tracking_depth", &["depth"], 0, 1);
    let [d] = crate::args::parse::<1>(&p, args, kw)?;
    let d = d.unwrap();
    if crate::types::float::is_float(d) {
        raise!(TypeError, "'float' object cannot be interpreted as an integer");
    }
    let depth = ab::as_i64(d)?;
    if depth < 0 {
        raise!(ValueError, "depth must be >= 0");
    }
    ORIGIN_DEPTH.with(|x| x.set(depth));
    Ok(none::none_ref())
}
fn sys_get_coro_depth(_m: &PyObject) -> PyResult {
    Ok(int::new_int(ORIGIN_DEPTH.with(|x| x.get())))
}

/// Origin tuple for a coroutine created now (None when tracking is off).
pub fn compute_cr_origin() -> Option<PyRef> {
    let depth = ORIGIN_DEPTH.with(|x| x.get());
    if depth <= 0 {
        return None;
    }
    let mut out = Vec::new();
    let mut f = crate::interp::current_frame_ptr();
    while !f.is_null() && (out.len() as i64) < depth {
        unsafe {
            let fr = &*f;
            let cd = crate::types::code::code_data(fr.code());
            out.push(tuple::new_tuple(vec![cd.filename.clone(), int::new_int(fr.lineno() as i64), cd.co_name.clone()]));
            f = fr.prev;
        }
    }
    Some(tuple::new_tuple(out))
}

/// Add the asyncgen / coroutine-origin functions to the sys module dict.
pub fn populate_sys(d: &PyObject, sysm: &PyObject) -> PyResult<()> {
    let m = crate::mdefs![
        ("get_asyncgen_hooks", NOARGS, sys_get_asyncgen_hooks, "get_asyncgen_hooks($module, /)\n--\n\nReturn the installed async generator hooks as (firstiter, finalizer)."),
        ("set_asyncgen_hooks", FASTKW, sys_set_asyncgen_hooks, "set_asyncgen_hooks([firstiter] [, finalizer])\n\nSet a finalizer for async generators objects."),
        ("set_coroutine_origin_tracking_depth", FASTKW, sys_set_coro_depth, "set_coroutine_origin_tracking_depth($module, /, depth)\n--\n\nRecord this many frames of creation context for new coroutines\n(0 disables the tracking)."),
        ("get_coroutine_origin_tracking_depth", NOARGS, sys_get_coro_depth, "get_coroutine_origin_tracking_depth($module, /)\n--\n\nReturn the depth set by set_coroutine_origin_tracking_depth()."),
    ];
    crate::builtins::add_methods_to_dict(d, m, Some(sysm))
}

// ---------------------------------------------------------------- type setup

pub fn init_types() -> PyResult<()> {
    unsafe {
        let t = wrapped_type();
        lu::setup_ptr_type(t, "async_generator_wrapped_value", std::mem::size_of::<PyObject>() + 8, 0);
        (*t).tp_dealloc = Some(wrapped_dealloc);

        static mut AW_ASYNC: PyAsyncMethods = PyAsyncMethods::ZERO;
        (*(&raw mut AW_ASYNC)).am_await = Some(self_iter);
        let t = &raw mut ASEND_TYPE;
        lu::setup_ptr_type(t, "async_generator_asend", std::mem::size_of::<Awaitable>(), Py_TPFLAGS_HAVE_FINALIZE);
        (*t).tp_as_async = &raw mut AW_ASYNC;
        (*t).tp_iter = Some(self_iter);
        (*t).tp_iternext = Some(asend_iternext);
        (*t).tp_finalize = Some(awaitable_finalize);
        (*t).tp_dealloc = Some(awaitable_dealloc);
        (*t).tp_methods = crate::mdefs![
            ("send", O, asend_send_m, "send(arg) -> send 'arg' into coroutine,\nreturn next yielded value or raise StopIteration."),
            ("throw", FAST, asend_throw, "throw(value)\nthrow(type[,value[,traceback]])\n\nRaise exception in coroutine, return next yielded value or raise\nStopIteration.\nthe (type, val, tb) signature is deprecated, \nand may be removed in a future version of Python."),
            ("close", NOARGS, asend_close, "close() -> raise GeneratorExit inside coroutine."),
        ];
        let t = &raw mut ATHROW_TYPE;
        lu::setup_ptr_type(t, "async_generator_athrow", std::mem::size_of::<Awaitable>(), Py_TPFLAGS_HAVE_FINALIZE);
        (*t).tp_as_async = &raw mut AW_ASYNC;
        (*t).tp_iter = Some(self_iter);
        (*t).tp_iternext = Some(athrow_iternext);
        (*t).tp_finalize = Some(awaitable_finalize);
        (*t).tp_methods = crate::mdefs![
            ("send", O, athrow_send_m, "send(arg) -> send 'arg' into coroutine,\nreturn next yielded value or raise StopIteration."),
            ("throw", FAST, athrow_throw, "throw(value)\nthrow(type[,value[,traceback]])\n\nRaise exception in coroutine, return next yielded value or raise\nStopIteration.\nthe (type, val, tb) signature is deprecated, \nand may be removed in a future version of Python."),
            ("close", NOARGS, athrow_close, "close() -> raise GeneratorExit inside coroutine."),
        ];

        let t = &mut *(&raw mut HOOKS_TYPE);
        let tt = &*tuple::tuple_type();
        t.tp_name = c"asyncgen_hooks".as_ptr();
        t.tp_basicsize = tt.tp_basicsize;
        t.tp_itemsize = tt.tp_itemsize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_TUPLE_SUBCLASS;
        t.tp_base = tuple::tuple_type();
        t.tp_dealloc = tt.tp_dealloc;
        t.tp_repr = Some(hooks_repr);
        t.tp_doc = c"asyncgen_hooks\n\nA named tuple of the form (firstiter, finalizer).".as_ptr();
        t.tp_getset = crate::getset_table![("firstiter", hooks_firstiter), ("finalizer", hooks_finalizer)];

        for t in [
            wrapped_type(),
            &raw mut ASEND_TYPE,
            &raw mut ATHROW_TYPE,
            &raw mut HOOKS_TYPE,
            &raw mut gobj::CORO_WRAPPER_TYPE,
        ] {
            lu::ready_type(t)?;
        }
        type_dict_set(&raw mut HOOKS_TYPE, "n_fields", &int::new_int(2));
        type_dict_set(&raw mut HOOKS_TYPE, "n_sequence_fields", &int::new_int(2));
        type_dict_set(&raw mut HOOKS_TYPE, "n_unnamed_fields", &int::new_int(0));
        type_dict_set(&raw mut HOOKS_TYPE, "__module__", &pystr::new_str("sys"));
    }
    Ok(())
}
/// anext(it, default): awaitable that returns `default` on StopAsyncIteration (pending full impl).
/// anext(it, default): an awaitable that delegates to `aw` and turns its
/// StopAsyncIteration into the default value (send/throw/close go through).
pub fn anext_with_default(aw: crate::object::PyRef, default: crate::object::PyRef) -> crate::errors::PyResult {
    static CLS: std::sync::atomic::AtomicPtr<crate::ffi::PyObject> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
    const SRC: &str = "\
class anext_awaitable:
    __slots__ = ('_aw', '_default', '_it')
    def __init__(self, aw, default):
        self._aw = aw
        self._default = default
        self._it = None
    def _inner(self):
        it = self._it
        if it is None:
            aw = self._aw
            t = type(aw)
            code = getattr(aw, 'gi_code', None)
            if code is not None and code.co_flags & 0x100:
                it = aw  # generator-based coroutine (types.coroutine)
            elif hasattr(t, '__await__'):
                it = t.__await__(aw)
                if not hasattr(type(it), '__next__') or hasattr(type(it), 'cr_code'):
                    kind = 'coroutine' if hasattr(type(it), 'cr_code') else type(it).__name__
                    raise TypeError(f'__await__() must return an iterator, not {kind}')
            else:
                raise TypeError(f\"'{t.__name__}' object can't be awaited\")
            self._it = it
        return it
    def _step(self, f, *args):
        try:
            return f(*args)
        except StopAsyncIteration:
            raise StopIteration(self._default) from None
    def __await__(self):
        return self
    def __iter__(self):
        return self
    def __next__(self):
        it = self._inner()
        return self._step(type(it).__next__, it)
    def send(self, value):
        it = self._inner()
        if value is None and not hasattr(it, 'send'):
            return self._step(type(it).__next__, it)
        return self._step(it.send, value)
    def throw(self, *args):
        return self._step(self._inner().throw, *args)
    def close(self):
        close = getattr(self._inner(), 'close', None)
        if close is not None:
            close()
";
    use std::sync::atomic::Ordering::{AcqRel, Acquire};
    let mut cls = CLS.load(Acquire);
    if cls.is_null() {
        let d = crate::types::dict::new_dict();
        crate::types::dict::set_item_str(&d, "__name__", &crate::types::str::new_str("builtins"))?;
        let code = crate::compile::compile_str(SRC, "<anext>", crate::compile::Mode::Exec, 0, -1)?;
        crate::interp::eval_code(&code.code, &d, Some(&d))?;
        let c = crate::types::dict::get_item_rs(&d, "anext_awaitable").expect("anext_awaitable defined");
        let raw = c.into_raw();
        match CLS.compare_exchange(std::ptr::null_mut(), raw, AcqRel, Acquire) {
            Ok(_) => cls = raw,
            Err(o) => {
                unsafe { crate::object::decref(raw) };
                cls = o;
            }
        }
    }
    crate::abstract_::call(unsafe { &*cls }, &[&aw, &default])
}
