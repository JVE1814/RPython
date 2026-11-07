//! Generators, coroutines and async generators: a heap-owned frame resumed by
//! `send`/`throw`/`close`.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::interp::frame::{Frame, FrameState};
use crate::interp::{Exit, eval_frame};
use crate::object::*;
use crate::threads::ErrStackItem;
use crate::types::exceptions as exc;
use crate::types::typeobject::*;
use crate::types::{none, str as pystr, tuple};
use crate::{name, raise};
use rpy_compiler::bytecode as bc;
use std::ffi::{c_int, c_void};

#[repr(C)]
pub struct GenObject {
    pub ob_base: PyObject,
    pub frame: *mut Frame,
    pub name: *mut PyObject,
    pub qualname: *mut PyObject,
    pub code: *mut PyObject,
    pub weakreflist: *mut PyObject,
    pub exc_state: ErrStackItem,
    pub running: bool,
    pub closed: bool,
    pub running_async: bool,
    /// for coroutines: origin tracking / awaited-by; for async gens: hooks called
    pub hooks_inited: bool,
    pub finalizer: *mut PyObject,
    /// object being delegated to by yield from/await (borrowed from frame stack)
    pub yieldfrom_hint: *mut PyObject,
    /// (jit6) while a consumer drains this generator (jit/helpers.rs `h_drain`): the
    /// container its values go to (borrowed) and the kind of consumer (DRAIN_*)
    pub drain: *mut PyObject,
    pub drain_kind: u64,
}

#[unsafe(no_mangle)]
pub static mut PyGen_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyCoro_Type: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(no_mangle)]
pub static mut PyAsyncGen_Type: PyTypeObject = PyTypeObject::ZERO;
pub static mut CORO_WRAPPER_TYPE: PyTypeObject = PyTypeObject::ZERO;
#[unsafe(export_name = "_PyAsyncGenASend_Type")]
pub static mut ASYNC_GEN_ASEND_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut ASYNC_GEN_ATHROW_TYPE: PyTypeObject = PyTypeObject::ZERO;
pub static mut ASYNC_GEN_WRAPPED_TYPE: PyTypeObject = PyTypeObject::ZERO;

pub fn gen_type() -> *mut PyTypeObject {
    &raw mut PyGen_Type
}
pub fn coro_type() -> *mut PyTypeObject {
    &raw mut PyCoro_Type
}
pub fn asyncgen_type() -> *mut PyTypeObject {
    &raw mut PyAsyncGen_Type
}
pub fn is_generator_exact(o: &PyObject) -> bool {
    o.is_type(gen_type())
}
pub fn is_coroutine_exact(o: &PyObject) -> bool {
    o.is_type(coro_type())
}
pub fn is_asyncgen(o: &PyObject) -> bool {
    o.is_type(asyncgen_type())
}
fn is_any_gen(o: &PyObject) -> bool {
    is_generator_exact(o) || is_coroutine_exact(o) || is_asyncgen(o)
}
pub fn g(o: &PyObject) -> &mut GenObject {
    unsafe { &mut *(o.as_ptr() as *mut GenObject) }
}

pub fn new_generator_for(frame: Box<Frame>, func: &PyObject, flags: u32) -> PyResult {
    let fo = crate::types::funcobject::func(func);
    unsafe { new_generator_named(frame, &*fo.func_name, &*fo.func_qualname, flags) }
}

/// (core6) A generator/coroutine over `frame` named explicitly (code run by eval/exec with
/// top-level await has no function).
pub fn new_generator_named(frame: Box<Frame>, name: &PyObject, qualname: &PyObject, flags: u32) -> PyResult {
    let tp = if flags & bc::co::COROUTINE != 0 {
        coro_type()
    } else if flags & bc::co::ASYNC_GENERATOR != 0 {
        asyncgen_type()
    } else {
        gen_type()
    };
    unsafe {
        let op = crate::alloc::alloc_object(tp, std::mem::size_of::<GenObject>());
        let gn = &mut *(op as *mut GenObject);
        gn.name = name.to_ref().into_raw();
        gn.qualname = qualname.to_ref().into_raw();
        gn.code = (*frame).code().to_ref().into_raw();
        let fp = Box::into_raw(frame);
        (*fp).generator = op;
        // jit4: first resumption enters compiled code directly when there is some
        (*fp).resume = crate::interp::jit::generator_entry(&*fp);
        gn.frame = fp;
        if tp == coro_type() {
            // coroutines keep their origin (sys.set_coroutine_origin_tracking_depth) here
            gn.finalizer = crate::types::asyncgen::compute_cr_origin().map_or(std::ptr::null_mut(), |o| o.into_raw());
        }
        crate::gc::track(op);
        Ok(PyRef::from_raw(op))
    }
}

pub enum SendResult {
    Yield(PyRef),
    Return(PyRef),
}

fn kind_name(o: &PyObject) -> &'static str {
    if is_coroutine_exact(o) {
        "coroutine"
    } else if is_asyncgen(o) {
        "async generator"
    } else {
        "generator"
    }
}

/// Resume a generator. `exc`: throw this exception at the suspension point.
pub fn gen_send_ex(o: &PyObject, arg: Option<&PyObject>, throw: Option<PyRef>, closing: bool) -> PyResult<SendResult> {
    // no GIL: claiming the generator must be atomic (two threads may resume it at once)
    let gn = g(o);
    let flag = unsafe { &*(&raw mut gn.running as *const std::sync::atomic::AtomicBool) };
    if flag.compare_exchange(false, true, std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire).is_err() {
        let msg = if is_coroutine_exact(o) {
            "coroutine already executing"
        } else if is_asyncgen(o) {
            "async generator already executing"
        } else {
            "generator already executing"
        };
        raise!(ValueError, "{}", msg);
    }
    let r = gen_send_claimed(o, arg, throw, closing);
    flag.store(false, std::sync::atomic::Ordering::Release);
    r
}

fn gen_send_claimed(o: &PyObject, arg: Option<&PyObject>, throw: Option<PyRef>, closing: bool) -> PyResult<SendResult> {
    let gn = g(o);
    if gn.frame.is_null() || unsafe { (*gn.frame).state == FrameState::Completed || (*gn.frame).state == FrameState::Cleared } {
        if is_coroutine_exact(o) && !closing {
            raise!(RuntimeError, "cannot reuse already awaited coroutine");
        }
        if let Some(t) = throw {
            set_exc(t);
            return Err(PyErr);
        }
        if is_asyncgen(o) {
            return Err(set_err_noargs(exc::StopAsyncIteration()));
        }
        return Err(set_err_noargs(exc::StopIteration()));
    }
    let frame = unsafe { &mut *gn.frame };
    if frame.state == FrameState::Created {
        if let Some(a) = arg {
            if !a.is(none::none()) && throw.is_none() {
                raise!(TypeError, "can't send non-None value to a just-started {}", kind_name(o));
            }
        }
    } else {
        // the suspended YIELD_VALUE produces its result first (the sent value, or None
        // when an exception is thrown in): handlers covering the yield count that slot
        let v = if throw.is_none() { arg.map(|a| a.to_ref()).unwrap_or_else(none::none_ref) } else { none::none_ref() };
        frame.slots[frame.stack_top] = v.into_raw();
        frame.stack_top += 1;
    }
    let ts = crate::threads::current();
    // push this generator's exception state
    unsafe {
        let ts_mut = ts as *const _ as *mut crate::threads::ThreadState;
        gn.exc_state.previous_item = (*ts_mut).c.exc_info;
        (*ts_mut).c.exc_info = &mut gn.exc_state;
    }
    // compiled code that suspends the frame again records its continuation
    frame.resume = 0;
    let r = crate::interp::enter_recursive_call(ts, "").and_then(|_| {
        let r = eval_frame(ts, frame, throw);
        crate::interp::leave_recursive_call(ts);
        r
    });
    unsafe {
        let ts_mut = ts as *const _ as *mut crate::threads::ThreadState;
        (*ts_mut).c.exc_info = gn.exc_state.previous_item;
        gn.exc_state.previous_item = std::ptr::null_mut();
    }
    match r {
        Ok(Exit::Yield(v)) => Ok(SendResult::Yield(v)),
        Ok(Exit::Return(v)) => {
            finish(gn);
            Ok(SendResult::Return(v))
        }
        Err(e) => {
            finish(gn);
            Err(e)
        }
    }
}

/// `gen_send_ex(o, arg, None, false)` for the common case (jit4): a suspended (or
/// created) generator/coroutine frame whose continuation is compiled code is entered
/// directly (`Frame::resume`), without the interpreter's frame prelude. `arg` NULL
/// means None. Everything else takes the general path.
#[inline]
pub fn gen_send_fast(o: &PyObject, arg: *mut PyObject) -> PyResult<SendResult> {
    let gn = g(o);
    // (jit6) suspended in `yield from` another generator: resume the innermost one
    let fp = gn.frame;
    if !fp.is_null() && unsafe { (*fp).yield_from_send >= 0 && (*fp).state == FrameState::Suspended } {
        let ts = crate::threads::current();
        crate::tlab::ensure_registered();
        unsafe {
            if !arg.is_null() {
                incref(arg);
            }
            match jit_send_chain(ts, o.as_ptr(), arg) {
                Some((st, val)) => {
                    return match st {
                        JS_YIELD => Ok(SendResult::Yield(PyRef::from_raw(val))),
                        JS_RETURN => Ok(SendResult::Return(PyRef::from_raw(val))),
                        _ => Err(PyErr),
                    };
                }
                None => {
                    if !arg.is_null() {
                        decref(arg);
                    }
                }
            }
        }
    }
    let argo = || if arg.is_null() { None } else { Some(unsafe { &*arg }) };
    // claim the generator (as gen_send_ex does); a failed claim reports "already
    // executing" through the general path
    let flag = unsafe { &*(&raw mut gn.running as *const std::sync::atomic::AtomicBool) };
    if flag.compare_exchange(false, true, std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire).is_err() {
        return gen_send_ex(o, argo(), None, false);
    }
    let fp = gn.frame;
    let mut r = None;
    if !fp.is_null() {
        let f = unsafe { &mut *fp };
        if f.resume != 0 && !crate::modules::sys::tracing_active() && !crate::interp::pep523::forces_interpreter() && crate::interp::jit::genfast_enabled() {
            let ok = match f.state {
                FrameState::Suspended => true,
                FrameState::Created => arg.is_null() || arg == none::none().as_ptr(),
                _ => false,
            };
            if ok {
                let ts = crate::threads::current();
                let d = ts.recursion_depth.get() + 1;
                if d <= ts.c.py_recursion_limit && crate::interp::stack_pointer() >= ts.stack_soft_limit() {
                    r = Some(resume_compiled(ts, gn, f, arg, d));
                }
            }
        }
    }
    let r = match r {
        Some(r) => r,
        None => gen_send_claimed(o, argo(), None, false),
    };
    flag.store(false, std::sync::atomic::Ordering::Release);
    r
}

/// Outcome of `jit_send`.
pub const JS_YIELD: u32 = 0;
pub const JS_RETURN: u32 = 1;
pub const JS_ERROR: u32 = 2;
/// the lean path does not apply (nothing consumed or changed)
pub const JS_SLOW: u32 = 3;

/// Lean resumption for compiled ForIter/Send (jit5): `o` is an exact generator or
/// coroutine, `v` the value to send (owned; NULL = None, as for next()). When the
/// generator is suspended (or just created, for None) in compiled code it is resumed
/// right here, with only the bookkeeping the compiled continuation needs, and
/// (status, value) is returned: JS_YIELD with the yielded value, JS_RETURN with the
/// return value (the generator is finished), JS_ERROR with the exception set (also
/// finished). Anything else returns JS_SLOW without consuming `v`.
///
/// (jit6) A generator suspended in `yield from`/`await` of another exact generator
/// goes through `jit_send_chain`, which resumes the innermost one directly.
#[inline(always)]
pub unsafe fn jit_send(ts: &crate::threads::ThreadState, o: *mut PyObject, v: *mut PyObject) -> (u32, *mut PyObject) {
    unsafe {
        let fp = (*(o as *mut GenObject)).frame;
        if !fp.is_null() && (*fp).yield_from_send >= 0 && (*fp).state == FrameState::Suspended {
            if let Some(r) = jit_send_chain(ts, o, v) {
                return r;
            }
        }
        jit_send_one(ts, o, v)
    }
}

/// Longest chain of pass-through levels `jit_send_chain` links in one go.
const CHAIN_MAX: usize = 32;

static CHAIN_ENABLED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(2);
fn chain_enabled() -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    match CHAIN_ENABLED.load(Relaxed) {
        2 => {
            // (off with the JIT: RPYTHON_JIT=0 is the reference semantics)
            let en = !matches!(std::env::var("RPYTHON_JIT_GENCHAIN").as_deref(), Ok("0")) && crate::interp::jit::enabled();
            CHAIN_ENABLED.store(en as u32, Relaxed);
            en
        }
        v => v != 0,
    }
}

/// (jit6) Resumption of a `yield from`/`await` chain without resuming every level.
///
/// When generator G0 is suspended in the send loop of a `yield from` whose receiver
/// is the exact generator (or coroutine) G1, and G1 likewise on G2, ..., resuming
/// G0 with `v` only sends `v` on to the innermost Gk: every level in between would
/// just pass the value down and the yielded value back up, unchanged. So the
/// levels G0..G(k-1) are made to look as if they were running (claimed, frames and
/// exception states linked, recursion depth counted, f_lasti at their Send), Gk is
/// resumed (`jit_send_one`, which may recurse into a new chain), and on a yield the
/// levels are simply unlinked again. When Gk finishes, the level above it continues
/// from its Send in the ordinary way (the value replaces the receiver and execution
/// goes on at the Send's exit; an exception is raised at the Send), and so on upwards.
/// None: not applicable (nothing consumed or changed).
#[inline(never)]
unsafe fn jit_send_chain(ts: &crate::threads::ThreadState, o: *mut PyObject, v: *mut PyObject) -> Option<(u32, *mut PyObject)> {
    use std::sync::atomic::{AtomicBool, Ordering::*};
    unsafe {
        if crate::modules::sys::tracing_active() || crate::interp::pep523::forces_interpreter() || !chain_enabled() || !crate::interp::jit::genfast_enabled() {
            return None;
        }
        let depth0 = ts.recursion_depth.get();
        if crate::interp::stack_pointer() < ts.stack_soft_limit() {
            return None;
        }
        // levels allowed by the recursion limit (the leaf counts one more)
        let room = (ts.c.py_recursion_limit - depth0 - 1).clamp(0, CHAIN_MAX as i32) as usize;
        let (gt, ct) = (gen_type(), coro_type());
        let mt = crate::threads::MULTI_THREADED.load(Relaxed) != 0;
        let tsm = ts as *const _ as *mut crate::threads::ThreadState;
        let mut lv: [std::mem::MaybeUninit<*mut GenObject>; CHAIN_MAX] = [std::mem::MaybeUninit::uninit(); CHAIN_MAX];
        let mut lasti: [std::mem::MaybeUninit<u32>; CHAIN_MAX] = [std::mem::MaybeUninit::uninit(); CHAIN_MAX];
        // levels whose exception state is linked (only those holding a handled
        // exception: an empty state in the chain changes nothing sys.exc_info() sees)
        let mut excmask = 0u32;
        let mut k = 0usize;
        let mut g = o as *mut GenObject;
        // walk, claim and link the pass-through levels in one pass
        while k < room {
            let f = (*g).frame;
            if f.is_null() || (*f).state != FrameState::Suspended || (*f).yield_from_send < 0 || (*f).stack_top <= (*f).nlocalsplus {
                break;
            }
            let recv = *(*f).slots.as_ptr().add((*f).stack_top - 1);
            let rt = (*recv).type_ptr();
            if rt != gt && rt != ct {
                break;
            }
            // claim (a plain store while single-threaded, see jit_send_one); a level
            // that is already running ends the chain (resuming it reports that)
            let flag = &*(&raw mut (*g).running as *const AtomicBool);
            if mt {
                if flag.compare_exchange(false, true, AcqRel, Acquire).is_err() {
                    break;
                }
            } else {
                if flag.load(Relaxed) {
                    break;
                }
                flag.store(true, Relaxed);
            }
            let gn = &mut *g;
            let f = &mut *f;
            if !gn.exc_state.exc_value.is_null() {
                gn.exc_state.previous_item = (*tsm).c.exc_info;
                (*tsm).c.exc_info = &mut gn.exc_state;
                excmask |= 1 << k;
            }
            f.prev = ts.frame.get();
            ts.frame.set(f);
            f.state = FrameState::Executing;
            lasti[k].write(f.lasti);
            f.lasti = f.yield_from_send as u32;
            lv[k].write(g);
            k += 1;
            g = recv as *mut GenObject;
        }
        if k == 0 {
            return None;
        }
        let lv = |i: usize| lv[i].assume_init();
        let lasti = |i: usize| lasti[i].assume_init();
        let leaf = g as *mut PyObject;
        ts.recursion_depth.set(depth0 + k as i32);
        let (mut st, mut val) = jit_send_one(ts, leaf, v);
        if st == JS_SLOW {
            let r = gen_send_fast(&*leaf, v);
            if !v.is_null() {
                decref(v);
            }
            (st, val) = match r {
                Ok(SendResult::Yield(y)) => (JS_YIELD, y.into_raw()),
                Ok(SendResult::Return(r)) => (JS_RETURN, r.into_raw()),
                Err(_) => (JS_ERROR, std::ptr::null_mut()),
            };
        }
        // levels still linked as pass-through: 0..level
        let mut level = k;
        while st != JS_YIELD && level > 0 {
            let i = level - 1;
            let gn = &mut *lv(i);
            let f = &mut *gn.frame;
            ts.frame.set(f.prev);
            f.prev = std::ptr::null_mut();
            f.lasti = lasti(i);
            let send_pc = f.yield_from_send as usize;
            if st == JS_ERROR && exc_matches(exc::StopIteration()) {
                // (as h_send: a StopIteration from the delegate carries its result)
                let x = take_exc().unwrap();
                val = exc::stop_iteration_value(&x).into_raw();
                st = JS_RETURN;
            }
            let throw = if st == JS_RETURN {
                // the Send finished with `val`: it replaces the receiver and execution
                // continues at the Send's exit
                let top = f.stack_top - 1;
                let recv = std::mem::replace(&mut f.slots[top], val);
                decref(recv);
                let d = crate::types::code::code_data(f.code());
                f.next_instr = bc::decode(d.code[send_pc]).1;
                None
            } else {
                // the delegate raised: the exception comes out of the Send
                f.next_instr = send_pc as u32 + 1;
                take_exc()
            };
            f.yield_from_send = -1;
            f.resume = 0;
            // (running for real now: its exception state is linked like gen_send_claimed's)
            if excmask & (1 << i) == 0 {
                gn.exc_state.previous_item = (*tsm).c.exc_info;
                (*tsm).c.exc_info = &mut gn.exc_state;
            }
            let r = eval_frame(ts, f, throw);
            (st, val) = match r {
                Ok(Exit::Yield(y)) => (JS_YIELD, y.into_raw()),
                Ok(Exit::Return(rv)) => (JS_RETURN, rv.into_raw()),
                Err(_) => (JS_ERROR, std::ptr::null_mut()),
            };
            (*tsm).c.exc_info = gn.exc_state.previous_item;
            gn.exc_state.previous_item = std::ptr::null_mut();
            ts.recursion_depth.set(ts.recursion_depth.get() - 1);
            if st != JS_YIELD {
                f.state = FrameState::Completed;
                finish(gn);
            }
            (*(&raw mut gn.running as *const AtomicBool)).store(false, Release);
            level = i;
        }
        for i in (0..level).rev() {
            let gn = &mut *lv(i);
            let f = &mut *gn.frame;
            ts.frame.set(f.prev);
            f.prev = std::ptr::null_mut();
            f.lasti = lasti(i);
            f.state = FrameState::Suspended;
            if excmask & (1 << i) != 0 {
                (*tsm).c.exc_info = gn.exc_state.previous_item;
                gn.exc_state.previous_item = std::ptr::null_mut();
            }
            (*(&raw mut gn.running as *const AtomicBool)).store(false, Release);
        }
        ts.recursion_depth.set(depth0);
        Some((st, val))
    }
}

/// Consumers that can drain a generator expression (`GenObject::drain_kind`).
pub const DRAIN_LIST: u64 = 0;
pub const DRAIN_SET: u64 = 1;
pub const DRAIN_ANY: u64 = 2;
pub const DRAIN_ALL: u64 = 3;

/// (jit6) Run a just-created generator expression `o` in its drain-mode code (entry
/// address `entry`) for a consumer of kind `kind` collecting into `container`: its
/// values go straight to the consumer (`h_drain_add`) until it finishes (JS_RETURN),
/// raises (JS_ERROR), or produces a value the consumer must see itself (JS_YIELD,
/// an ordinary suspension). JS_SLOW: not applicable, nothing changed.
pub unsafe fn jit_send_drain(ts: &crate::threads::ThreadState, o: *mut PyObject, container: *mut PyObject, kind: u64, entry: usize) -> (u32, *mut PyObject) {
    unsafe {
        let gn = &mut *(o as *mut GenObject);
        let fp = gn.frame;
        if fp.is_null() || (*fp).state != FrameState::Created || (*fp).resume == 0 || !gn.drain.is_null() {
            return (JS_SLOW, std::ptr::null_mut());
        }
        let saved = (*fp).resume;
        (*fp).resume = entry;
        gn.drain = container;
        gn.drain_kind = kind;
        let r = jit_send_one(ts, o, std::ptr::null_mut());
        gn.drain = std::ptr::null_mut();
        if r.0 == JS_SLOW {
            (*fp).resume = saved;
        }
        r
    }
}

/// (jit6) Drain-mode YieldValue: hand `v` (owned) to the consumer draining the
/// generator of `frame`. 0: taken (and consumed); 1: the consumer must see it itself
/// (`v` untouched; the generator suspends normally). Only work that cannot run Python
/// code is done here, so nothing is evaluated twice.
pub unsafe extern "C" fn h_drain_add(frame: *mut Frame, v: *mut PyObject) -> u64 {
    unsafe {
        let g = (*frame).generator as *mut GenObject;
        if g.is_null() || (*g).drain.is_null() {
            return 1;
        }
        let c = &*(*g).drain;
        let vo = &*v;
        match (*g).drain_kind {
            DRAIN_LIST => {
                crate::types::list::append(c, PyRef::from_raw(v));
                0
            }
            DRAIN_SET => {
                // hashing a str/int/float/tuple of those runs no Python code; anything
                // else goes through the consumer's own SetAdd
                if !hash_is_builtin(vo) {
                    return 1;
                }
                match crate::types::set::set_add(c, vo) {
                    Ok(()) => {
                        decref(v);
                        0
                    }
                    Err(_) => {
                        clear_exc();
                        1
                    }
                }
            }
            kind @ (DRAIN_ANY | DRAIN_ALL) => {
                let t = match builtin_truth(vo) {
                    Some(t) => t,
                    None => return 1,
                };
                // any(): falsy values are skipped; all(): truthy ones
                if t == (kind == DRAIN_ALL) {
                    decref(v);
                    0
                } else {
                    1
                }
            }
            _ => 1,
        }
    }
}

fn hash_is_builtin(o: &PyObject) -> bool {
    let t = o.type_ptr();
    if t == crate::types::str::str_type() || t == crate::types::int::int_type() || t == crate::types::float::float_type() || o.is(none::none()) || t == crate::types::bool_::bool_type() {
        return true;
    }
    if t == tuple::tuple_type() {
        return tuple::tuple_items(o).iter().all(|x| hash_is_builtin(x));
    }
    false
}

fn builtin_truth(o: &PyObject) -> Option<bool> {
    let t = o.type_ptr();
    if o.is(none::none()) {
        return Some(false);
    }
    if t == crate::types::bool_::bool_type() {
        return Some(o.is(crate::types::bool_::true_obj()));
    }
    if t == crate::types::int::int_type() || t == crate::types::str::str_type() || t == tuple::tuple_type() || t == crate::types::list::list_type() || t == crate::types::float::float_type() {
        return crate::abstract_::is_true(o).ok();
    }
    None
}

/// One level of `jit_send` (the jit5 lean path).
#[inline(always)]
unsafe fn jit_send_one(ts: &crate::threads::ThreadState, o: *mut PyObject, v: *mut PyObject) -> (u32, *mut PyObject) {
    use crate::interp::jit::tier1::ST_YIELD;
    unsafe {
        let gn = &mut *(o as *mut GenObject);
        let fp = gn.frame;
        if fp.is_null() || (*fp).resume == 0 {
            return (JS_SLOW, std::ptr::null_mut());
        }
        let f = &mut *fp;
        let none = none::none().as_ptr();
        let push = match f.state {
            FrameState::Suspended => true,
            FrameState::Created if v.is_null() || v == none => false,
            _ => return (JS_SLOW, std::ptr::null_mut()),
        };
        if crate::modules::sys::tracing_active() || crate::interp::pep523::forces_interpreter() || !crate::interp::jit::genfast_enabled() {
            return (JS_SLOW, std::ptr::null_mut());
        }
        let depth = ts.recursion_depth.get() + 1;
        if depth > ts.c.py_recursion_limit || crate::interp::stack_pointer() < ts.stack_soft_limit() {
            return (JS_SLOW, std::ptr::null_mut());
        }
        let flag = &*(&raw mut gn.running as *const std::sync::atomic::AtomicBool);
        // claim: while only one thread exists (threads set MULTI_THREADED before the
        // second one starts) nobody can race the claim, so a plain store does (a
        // locked compare-exchange costs ~20 cycles per resumption)
        if crate::threads::MULTI_THREADED.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            if flag.load(std::sync::atomic::Ordering::Relaxed) {
                return (JS_SLOW, std::ptr::null_mut());
            }
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        } else if flag.compare_exchange(false, true, std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire).is_err() {
            return (JS_SLOW, std::ptr::null_mut());
        }
        if push {
            let x = if v.is_null() {
                incref(none);
                none
            } else {
                v
            };
            *f.slots.as_mut_ptr().add(f.stack_top) = x;
            f.stack_top += 1;
        } else if !v.is_null() {
            decref(v);
        }
        ts.recursion_depth.set(depth);
        let tsm = ts as *const _ as *mut crate::threads::ThreadState;
        gn.exc_state.previous_item = (*tsm).c.exc_info;
        (*tsm).c.exc_info = &mut gn.exc_state;
        let prev = ts.frame.get();
        f.prev = prev;
        ts.frame.set(fp);
        f.state = FrameState::Executing;
        let target = f.resume;
        f.resume = 0;
        let mut out: *mut PyObject = std::ptr::null_mut();
        let st = crate::interp::jit::enter_registered(ts, f, target, &mut out);
        if st == ST_YIELD {
            ts.frame.set(prev);
            f.prev = std::ptr::null_mut();
            ts.recursion_depth.set(depth - 1);
            (*tsm).c.exc_info = gn.exc_state.previous_item;
            gn.exc_state.previous_item = std::ptr::null_mut();
            f.state = FrameState::Suspended;
            flag.store(false, std::sync::atomic::Ordering::Release);
            return (JS_YIELD, out);
        }
        jit_send_end(ts, gn, fp, prev, depth, st, out)
    }
}

/// The resumed generator returned, raised or deoptimised (`jit_send`).
#[cold]
#[inline(never)]
unsafe fn jit_send_end(ts: &crate::threads::ThreadState, gn: &mut GenObject, fp: *mut Frame, prev: *mut Frame, depth: i32, st: u32, out: *mut PyObject) -> (u32, *mut PyObject) {
    use crate::interp::jit::tier1::{ST_RETURN, ST_YIELD};
    unsafe {
        let f = &mut *fp;
        let r = match st {
            ST_RETURN => {
                f.clear_stack();
                Ok(crate::interp::Exit::Return(PyRef::from_raw(out)))
            }
            _ => crate::interp::jit::continue_in_interp(ts, f, st),
        };
        ts.frame.set(prev);
        f.prev = std::ptr::null_mut();
        ts.recursion_depth.set(depth - 1);
        let tsm = ts as *const _ as *mut crate::threads::ThreadState;
        (*tsm).c.exc_info = gn.exc_state.previous_item;
        gn.exc_state.previous_item = std::ptr::null_mut();
        let flag = &*(&raw mut gn.running as *const std::sync::atomic::AtomicBool);
        let res = match r {
            Ok(crate::interp::Exit::Yield(v)) => {
                f.state = FrameState::Suspended;
                (JS_YIELD, v.into_raw())
            }
            Ok(crate::interp::Exit::Return(v)) => {
                f.state = FrameState::Completed;
                finish(gn);
                (JS_RETURN, v.into_raw())
            }
            Err(_) => {
                f.state = FrameState::Completed;
                finish(gn);
                (JS_ERROR, std::ptr::null_mut())
            }
        };
        let _ = ST_YIELD;
        flag.store(false, std::sync::atomic::Ordering::Release);
        res
    }
}

#[inline(always)]
fn resume_compiled(ts: &crate::threads::ThreadState, gn: &mut GenObject, f: &mut Frame, arg: *mut PyObject, depth: i32) -> PyResult<SendResult> {
    use crate::interp::jit::tier1::{ST_RETURN, ST_YIELD};
    if f.state == FrameState::Suspended {
        let v = if arg.is_null() { none::none().as_ptr() } else { arg };
        unsafe { incref(v) };
        f.slots[f.stack_top] = v;
        f.stack_top += 1;
    }
    ts.recursion_depth.set(depth);
    let tsm = ts as *const _ as *mut crate::threads::ThreadState;
    unsafe {
        gn.exc_state.previous_item = (*tsm).c.exc_info;
        (*tsm).c.exc_info = &mut gn.exc_state;
    }
    let prev = ts.frame.get();
    f.prev = prev;
    ts.frame.set(f as *mut Frame);
    f.state = FrameState::Executing;
    let target = std::mem::replace(&mut f.resume, 0);
    let mut out: *mut PyObject = std::ptr::null_mut();
    let st = crate::interp::jit::enter(ts, f, target, &mut out);
    let r = match st {
        ST_YIELD => Ok(crate::interp::Exit::Yield(unsafe { PyRef::from_raw(out) })),
        ST_RETURN => {
            f.clear_stack();
            Ok(crate::interp::Exit::Return(unsafe { PyRef::from_raw(out) }))
        }
        _ => crate::interp::jit::continue_in_interp(ts, f, st),
    };
    ts.frame.set(prev);
    f.prev = std::ptr::null_mut();
    ts.recursion_depth.set(depth - 1);
    unsafe {
        (*tsm).c.exc_info = gn.exc_state.previous_item;
    }
    gn.exc_state.previous_item = std::ptr::null_mut();
    match r {
        Ok(crate::interp::Exit::Yield(v)) => {
            f.state = FrameState::Suspended;
            Ok(SendResult::Yield(v))
        }
        Ok(crate::interp::Exit::Return(v)) => {
            f.state = FrameState::Completed;
            finish(gn);
            Ok(SendResult::Return(v))
        }
        Err(e) => {
            f.state = FrameState::Completed;
            finish(gn);
            Err(e)
        }
    }
}

fn finish(gn: &mut GenObject) {
    if gn.frame.is_null() {
        return;
    }
    let fb = unsafe { Box::from_raw(gn.frame) };
    gn.frame = std::ptr::null_mut();
    unsafe {
        let old = gn.exc_state.exc_value;
        gn.exc_state.exc_value = std::ptr::null_mut();
        xdecref(old);
    }
    crate::interp::frame_finished(fb);
}

/// Send for `Send` opcode / yield from: handles generators natively and other iterators.
pub fn send_to(recv: &PyObject, v: &PyObject) -> PyResult<SendResult> {
    // (jit6) `async for` / `await anext(..)`: the asend object's result comes back as
    // a value, not as a StopIteration exception object
    if recv.type_ptr() == (&raw mut crate::types::asyncgen::ASEND_TYPE) {
        return crate::types::asyncgen::asend_send_result(recv, v);
    }
    if is_generator_exact(recv) || is_coroutine_exact(recv) {
        return match gen_send_fast(recv, v.as_ptr()) {
            Ok(r) => Ok(r),
            Err(e) => {
                if exc_matches(exc::StopIteration()) {
                    let x = take_exc().unwrap();
                    Ok(SendResult::Return(exc::stop_iteration_value(&x)))
                } else {
                    Err(e)
                }
            }
        };
    }
    let r = if v.is(none::none()) && recv.ty().tp_iternext.is_some() {
        let f = recv.ty().tp_iternext.unwrap();
        unsafe { from_c_opt(f(recv.as_ptr())) }
    } else {
        ab::call_method(recv, name!("send"), &[v]).map(Some)
    };
    match r {
        Ok(Some(x)) => Ok(SendResult::Yield(x)),
        Ok(None) | Err(_) => {
            if !err_occurred() {
                return Ok(SendResult::Return(none::none_ref()));
            }
            if exc_matches(exc::StopIteration()) {
                let x = take_exc().unwrap();
                return Ok(SendResult::Return(exc::stop_iteration_value(&x)));
            }
            Err(PyErr)
        }
    }
}

unsafe fn from_c_opt(p: *mut PyObject) -> PyResult<Option<PyRef>> {
    if p.is_null() {
        if err_occurred() { Err(PyErr) } else { Ok(None) }
    } else {
        Ok(Some(unsafe { PyRef::from_raw(p) }))
    }
}

fn stop_iteration_with(v: PyRef) -> PyErr {
    if v.is(none::none()) {
        return set_err_noargs(exc::StopIteration());
    }
    match exc::make_exception(exc::StopIteration(), &[v]) {
        Ok(e) => set_exc(e),
        Err(_) => {}
    }
    PyErr
}

fn gen_send(o: &PyObject, v: &PyObject) -> PyResult {
    match gen_send_fast(o, v.as_ptr())? {
        SendResult::Yield(y) => Ok(y),
        SendResult::Return(r) => Err(stop_iteration_with(r)),
    }
}

unsafe extern "C" fn gen_iternext(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        match gen_send_fast(&*o, std::ptr::null_mut()) {
            Ok(SendResult::Yield(y)) => y.into_raw(),
            Ok(SendResult::Return(r)) => {
                if !r.is(none::none()) {
                    let _ = stop_iteration_with(r);
                }
                std::ptr::null_mut()
            }
            Err(_) => std::ptr::null_mut(),
        }
    }
}

/// Delegate target of a suspended yield from / await, if any.
pub fn yieldfrom(o: &PyObject) -> Option<PyRef> {
    let gn = g(o);
    if gn.frame.is_null() {
        return None;
    }
    let f = unsafe { &*gn.frame };
    if f.state != FrameState::Suspended {
        return None;
    }
    // The receiver is below the yielded value on the stack: [.., receiver] at suspension
    // only when the next instruction is the Send loop.
    let code = crate::types::code::code_data(f.code());
    let ni = f.next_instr as usize;
    if ni < code.code.len() {
        let (op, _) = bc::decode(code.code[ni]);
        let _ = op;
    }
    if crate::interp::eval::is_in_send_loop(f) && f.stack_top > f.nlocalsplus {
        let p = f.slots[f.stack_top - 1];
        if !p.is_null() {
            return Some(unsafe { PyRef::from_borrowed(p) });
        }
    }
    None
}

/// Run `f` with the delegating generator's frame on the thread's frame stack (it is the
/// caller of whatever its `yield from` delegates to).
fn with_frame_on_stack<R>(o: &PyObject, f: impl FnOnce() -> R) -> R {
    let ts = crate::threads::current();
    let gf = g(o).frame;
    let saved = ts.frame.get();
    if !gf.is_null() {
        unsafe { (*gf).prev = saved };
        ts.frame.set(gf);
    }
    let r = f();
    ts.frame.set(saved);
    if !gf.is_null() {
        unsafe { (*gf).prev = std::ptr::null_mut() };
    }
    r
}

pub fn gen_throw_impl(o: &PyObject, typ: &PyObject, val: Option<&PyObject>, tb: Option<&PyObject>, close_on_genexit: bool) -> PyResult {
    // delegate to sub-iterator first
    if let Some(yf) = yieldfrom(o) {
        // GeneratorExit (class, subclass or instance) closes the sub-iterator and
        // is then raised here
        let is_genexit = if exc::is_exception_class(typ) {
            is_subtype(as_type(typ), exc::GeneratorExit())
        } else {
            is_subtype(typ.type_ptr(), exc::GeneratorExit())
        };
        if is_genexit && close_on_genexit {
            g(o).running = true;
            let r = close_iter(&yf);
            g(o).running = false;
            if r.is_err() {
                let e = take_exc().unwrap();
                return resume_with_error(o, e);
            }
            return throw_here(o, typ, val, tb);
        }
        let r = if is_any_gen(&yf) {
            g(o).running = true;
            let r = with_frame_on_stack(o, || gen_throw_impl(&yf, typ, val, tb, close_on_genexit));
            g(o).running = false;
            r
        } else {
            match ab::getattr_opt(&yf, name!("throw"))? {
                None => return throw_here(o, typ, val, tb),
                Some(m) => {
                    g(o).running = true;
                    let mut args: Vec<&PyObject> = vec![typ];
                    if let Some(v) = val {
                        args.push(v);
                    }
                    if let Some(t) = tb {
                        if val.is_none() {
                            args.push(none::none());
                        }
                        args.push(t);
                    }
                    let r = with_frame_on_stack(o, || ab::call(&m, &args));
                    g(o).running = false;
                    r
                }
            }
        };
        return match r {
            Ok(v) => Ok(v),
            Err(_) => {
                let e = take_exc().unwrap();
                if is_subtype(e.type_ptr(), exc::StopIteration()) {
                    // sub-iterator finished: resume with its value
                    let v = exc::stop_iteration_value(&e);
                    pop_receiver_and_jump(o);
                    match gen_send_ex(o, Some(&v), None, false)? {
                        SendResult::Yield(y) => Ok(y),
                        SendResult::Return(r) => Err(stop_iteration_with(r)),
                    }
                } else {
                    // (tail3) the error enters this generator's frame afresh: it is
                    // chained with this generator's handled exception there (not
                    // treated as one propagating out of a callee)
                    crate::threads::current().propagated_exc.set((0, 0));
                    resume_with_error(o, e)
                }
            }
        };
    }
    throw_here(o, typ, val, tb)
}

fn pop_receiver_and_jump(o: &PyObject) {
    let gn = g(o);
    if gn.frame.is_null() {
        return;
    }
    crate::interp::eval::finish_send_loop(unsafe { &mut *gn.frame });
}

fn resume_with_error(o: &PyObject, e: PyRef) -> PyResult {
    match gen_send_ex(o, None, Some(e), false)? {
        SendResult::Yield(y) => Ok(y),
        SendResult::Return(r) => Err(stop_iteration_with(r)),
    }
}

fn throw_here(o: &PyObject, typ: &PyObject, val: Option<&PyObject>, tb: Option<&PyObject>) -> PyResult {
    if let Some(t) = tb {
        if !t.is(none::none()) && !crate::types::traceback::is_traceback(t) {
            raise!(TypeError, "throw() third argument must be a traceback object");
        }
    }
    let inst = if exc::is_exception_class(typ) {
        let made = match val {
            Some(v) if exc::is_exception_instance(v) && is_subtype(v.type_ptr(), as_type(typ)) => v.to_ref(),
            Some(v) if !v.is(none::none()) => {
                if tuple::is_tuple(v) {
                    let items: Vec<&PyObject> = tuple::tuple_items(v).to_vec();
                    ab::call(typ, &items)?
                } else {
                    ab::call(typ, &[v])?
                }
            }
            _ => ab::call(typ, &[])?,
        };
        if !exc::is_exception_instance(&made) {
            // the bad result becomes a TypeError raised inside the generator
            let _ = err!(
                TypeError,
                "calling {} should have returned an instance of BaseException, not {}",
                ab::repr_string(typ)?,
                tname(&made)
            );
            let e = take_exc().unwrap();
            return resume_with_error(o, e);
        }
        made
    } else if exc::is_exception_instance(typ) {
        if let Some(v) = val {
            if !v.is(none::none()) {
                raise!(TypeError, "instance exception may not have a separate value");
            }
        }
        typ.to_ref()
    } else {
        raise!(
            TypeError,
            "exceptions must be classes or instances deriving from BaseException, not {}",
            tname(typ)
        );
    };
    if let Some(t) = tb {
        if !t.is(none::none()) {
            exc::set_exc_traceback(&inst, Some(t));
        }
    }
    resume_with_error(o, inst)
}

fn close_iter(yf: &PyObject) -> PyResult<()> {
    if is_any_gen(yf) {
        gen_close(yf)?;
        return Ok(());
    }
    match ab::getattr_opt(yf, name!("close")) {
        Ok(Some(m)) => {
            ab::call(&m, &[])?;
        }
        Ok(None) => {}
        // a sub-iterator whose close cannot even be looked up does not stop the
        // closing of the delegating generator: the failure is reported
        Err(_) => {
            let e = take_exc();
            let r = ab::repr_string(yf).unwrap_or_else(|_| {
                clear_exc();
                String::from("<generator>")
            });
            if let Some(e) = e {
                set_exc(e);
            }
            crate::modules::sys_full::write_unraisable(Some(&format!("Exception ignored while closing generator {}", r)), None);
        }
    }
    Ok(())
}

fn gen_throw(o: &PyObject, args: &[&PyObject]) -> PyResult {
    if args.is_empty() || args.len() > 3 {
        raise!(TypeError, "throw expected at least 1 argument, got {}", args.len());
    }
    if args.len() > 1 {
        crate::modules::warnings::warn(
            exc::DeprecationWarning(),
            "the (type, exc, tb) signature of throw() is deprecated, use the single-arg signature instead.",
            1,
        )?;
    }
    gen_throw_impl(o, args[0], args.get(1).copied(), args.get(2).copied(), true)
}

/// (jit5) Can closing this suspended generator skip throwing GeneratorExit into it?
/// True when it is suspended at a plain `yield` whose only enclosing exception handler
/// is the compiler's implicit StopIteration -> RuntimeError wrapper (the public RESUME
/// after the yield carries the "exception depth 1" flag): nothing in the generator can
/// observe the exception, so the effect is just finishing the frame. (An abandoned
/// `any(x for x in ...)` closes its generator on every call.)
fn close_without_throw(gn: &GenObject) -> bool {
    if gn.running || crate::modules::sys::tracing_active() {
        return false;
    }
    let f = unsafe { &*gn.frame };
    if f.state != FrameState::Suspended || f.yield_from_send != -1 || !f.frame_obj.is_null() {
        return false;
    }
    let co = f.code();
    let m = crate::types::code::code_meta(co);
    if !m.public {
        return false;
    }
    let cd = crate::types::code::code_data(co);
    let Some(&off) = cd.pub_of_internal.get(f.next_instr as usize) else { return false };
    let code = crate::types::bytes::bytes_data(&m.co_code);
    let k = off as usize * 2;
    if k + 1 >= code.len() || code[k] != crate::interp::wc::op::RESUME {
        return false;
    }
    let arg = code[k + 1];
    arg & 3 == 1 && arg & 8 != 0
}

pub fn gen_close(o: &PyObject) -> PyResult {
    let gn = g(o);
    if gn.frame.is_null() {
        return Ok(none::none_ref());
    }
    if unsafe { (*gn.frame).state == FrameState::Created } {
        finish(gn);
        return Ok(none::none_ref());
    }
    if close_without_throw(gn) {
        finish(gn);
        return Ok(none::none_ref());
    }
    let ge = type_ref(exc::GeneratorExit());
    match gen_throw_impl(o, &ge, None, None, true) {
        Ok(_) => {
            raise!(RuntimeError, "{} ignored GeneratorExit", kind_name(o));
        }
        Err(e) => {
            if exc_matches(exc::StopIteration()) {
                let x = take_exc().unwrap();
                return Ok(exc::stop_iteration_value(&x));
            }
            if exc_matches(exc::GeneratorExit()) {
                clear_exc();
                return Ok(none::none_ref());
            }
            Err(e)
        }
    }
}

unsafe extern "C" fn gen_finalize(o: *mut PyObject) {
    unsafe {
        let oo = &*o;
        let gn = g(oo);
        if is_asyncgen(oo) && !gn.frame.is_null() && !gn.finalizer.is_null() && !gn.closed {
            let saved = take_exc();
            let f = PyRef::from_borrowed(gn.finalizer);
            if ab::call(&f, &[oo]).is_err() {
                crate::modules::sys::write_unraisable_with_obj("Exception ignored while finalizing generator", Some(oo));
            }
            if let Some(s) = saved {
                set_exc(s);
            }
            return;
        }
        if gn.frame.is_null() || (*gn.frame).state == FrameState::Created {
            if is_coroutine_exact(oo) && !gn.frame.is_null() {
                let saved = take_exc();
                // the warnings module words the warning (with the creation origin when
                // tracked); failures are reported as unraisable
                let r = (|| -> PyResult<()> {
                    let w = crate::import::import_absolute("warnings")?;
                    match ab::getattr_opt(&w, &pystr::new_str("_warn_unawaited_coroutine"))? {
                        Some(f) => {
                            ab::call(&f, &[oo])?;
                        }
                        None => {
                            let msg = format!("coroutine '{}' was never awaited", pystr::to_string_lossy(&*gn.qualname));
                            crate::modules::warnings::warn_with_source(exc::RuntimeWarning(), &msg, 1, Some(oo))?;
                        }
                    }
                    Ok(())
                })();
                if r.is_err() {
                    let what = ab::repr_string(oo).unwrap_or_else(|_| {
                        clear_exc();
                        "<coroutine>".into()
                    });
                    crate::modules::sys_full::write_unraisable(Some(&format!("Exception ignored while finalizing coroutine {}", what)), None);
                    // still warn, in the plain wording
                    let msg = format!("coroutine '{}' was never awaited", pystr::to_string_lossy(&*gn.qualname));
                    if crate::modules::warnings::warn_with_source(exc::RuntimeWarning(), &msg, 1, Some(oo)).is_err() {
                        clear_exc();
                    }
                }
                if let Some(s) = saved {
                    set_exc(s);
                }
            }
            return;
        }
        let saved = take_exc();
        if is_asyncgen(oo) && !gn.finalizer.is_null() {
            let f = PyRef::from_borrowed(gn.finalizer);
            if ab::call(&f, &[oo]).is_err() {
                crate::modules::sys::write_unraisable_with_obj("Exception ignored while finalizing generator", Some(oo));
            }
        } else if gen_close(oo).is_err() {
            // the message names the object (its repr), there is no separate object
            let failure = take_exc();
            let shown = ab::repr_string(oo).unwrap_or_else(|_| {
                clear_exc();
                String::new()
            });
            if let Some(f) = failure {
                set_exc(f);
            }
            let msg = if shown.is_empty() {
                format!("Exception ignored while closing {}", kind_name(oo))
            } else {
                format!("Exception ignored while closing {} {}", kind_name(oo), shown)
            };
            crate::modules::sys::write_unraisable_with_obj(&msg, None);
        }
        if let Some(s) = saved {
            set_exc(s);
        }
    }
}

unsafe extern "C" fn gen_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        let gn = &mut *(o as *mut GenObject);
        crate::types::weakref::clear_weakrefs_at(o, &mut gn.weakreflist);
        // (a finished generator has nothing to finalize: skip the resurrection dance)
        if !gn.frame.is_null() && !crate::gc::is_finalized(o) {
            crate::gc::set_finalized(o);
            // resurrect temporarily for the finalizer
            (*o).ob_ref_local.store(1, std::sync::atomic::Ordering::Relaxed);
            (*o).ob_tid.store(thread_id(), std::sync::atomic::Ordering::Relaxed);
            gen_finalize(o);
            let local = (*o).ob_ref_local.load(std::sync::atomic::Ordering::Relaxed) - 1;
            (*o).ob_ref_local.store(local, std::sync::atomic::Ordering::Relaxed);
            if local != 0 || (*o).ob_ref_shared.load(std::sync::atomic::Ordering::Relaxed) >> 2 != 0 {
                return; // resurrected
            }
        }
        if !gn.frame.is_null() {
            drop(Box::from_raw(gn.frame));
            gn.frame = std::ptr::null_mut();
        }
        for p in [gn.name, gn.qualname, gn.code, gn.exc_state.exc_value, gn.finalizer] {
            xdecref(p);
        }
        crate::alloc::free_object(o);
    }
}

unsafe extern "C" fn gen_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let gn = &*(o as *mut GenObject);
        let v = visit.unwrap();
        if !gn.frame.is_null() {
            let f = &*gn.frame;
            for i in 0..f.stack_top {
                let p = f.slots[i];
                if !p.is_null() {
                    let r = v(p, arg);
                    if r != 0 {
                        return r;
                    }
                }
            }
            for p in [f.func, f.globals, f.locals] {
                if !p.is_null() {
                    let r = v(p, arg);
                    if r != 0 {
                        return r;
                    }
                }
            }
        }
        for p in [gn.exc_state.exc_value, gn.finalizer] {
            if !p.is_null() {
                let r = v(p, arg);
                if r != 0 {
                    return r;
                }
            }
        }
        0
    }
}

unsafe extern "C" fn gen_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let gn = &*(o as *mut GenObject);
        let s = format!("<{} object {} at {:p}>", kind_name(&*o), pystr::to_string_lossy(&*gn.qualname), o);
        pystr::new_str(&s).into_raw()
    }
}

unsafe extern "C" fn self_iter(o: *mut PyObject) -> *mut PyObject {
    unsafe { (*o).to_ref().into_raw() }
}

fn get_name(o: &PyObject) -> PyResult {
    unsafe { Ok(PyRef::from_borrowed(g(o).name)) }
}
fn set_name(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match v {
        Some(v) if pystr::is_str(v) => {
            unsafe { crate::qsbr::swap_field(&raw mut g(o).name, v.to_ref().into_raw()) };
            Ok(())
        }
        _ => raise!(TypeError, "__name__ must be set to a string object"),
    }
}
fn get_qualname(o: &PyObject) -> PyResult {
    unsafe { Ok(PyRef::from_borrowed(g(o).qualname)) }
}
fn set_qualname(o: &PyObject, v: Option<&PyObject>) -> PyResult<()> {
    match v {
        Some(v) if pystr::is_str(v) => {
            unsafe { crate::qsbr::swap_field(&raw mut g(o).qualname, v.to_ref().into_raw()) };
            Ok(())
        }
        _ => raise!(TypeError, "__qualname__ must be set to a string object"),
    }
}
pub fn get_frame(o: &PyObject) -> PyResult {
    let gn = g(o);
    if gn.frame.is_null() {
        return Ok(none::none_ref());
    }
    let fo = crate::interp::eval::frame_object(unsafe { &mut *gn.frame });
    Ok(unsafe { PyRef::from_borrowed(fo) })
}
fn get_running(o: &PyObject) -> PyResult {
    Ok(crate::types::bool_::new_bool(g(o).running))
}
pub fn get_suspended(o: &PyObject) -> PyResult {
    let gn = g(o);
    Ok(crate::types::bool_::new_bool(!gn.frame.is_null() && unsafe { (*gn.frame).state == FrameState::Suspended }))
}
/// `gi_state`: the generator's state under the names inspect.getgeneratorstate() uses.
fn get_gen_state(o: &PyObject) -> PyResult {
    let gn = g(o);
    let state = if gn.running {
        "GEN_RUNNING"
    } else if gn.frame.is_null() {
        "GEN_CLOSED"
    } else {
        match unsafe { (*gn.frame).state } {
            FrameState::Created => "GEN_CREATED",
            FrameState::Executing => "GEN_RUNNING",
            FrameState::Suspended => "GEN_SUSPENDED",
            FrameState::Completed | FrameState::Cleared => "GEN_CLOSED",
        }
    };
    Ok(crate::types::str::new_str(state))
}
fn get_code(o: &PyObject) -> PyResult {
    unsafe { Ok(PyRef::from_borrowed(g(o).code)) }
}
fn get_yieldfrom(o: &PyObject) -> PyResult {
    Ok(yieldfrom(o).unwrap_or_else(none::none_ref))
}
fn gen_sizeof(_o: &PyObject) -> PyResult {
    Ok(crate::types::int::new_int(std::mem::size_of::<GenObject>() as i64))
}
fn gen_close_m(o: &PyObject) -> PyResult {
    gen_close(o)
}

fn get_cr_origin(o: &PyObject) -> PyResult {
    let p = g(o).finalizer;
    Ok(if p.is_null() { none::none_ref() } else { unsafe { PyRef::from_borrowed(p) } })
}

pub fn coro_is_running_await(o: &PyObject) -> bool {
    yieldfrom(o).is_some()
}

// ---------------------------------------------------------------- awaitables

unsafe extern "C" fn coro_await(o: *mut PyObject) -> *mut PyObject {
    unsafe {
        let op = crate::alloc::alloc_object(&raw mut CORO_WRAPPER_TYPE, std::mem::size_of::<CoroWrapper>());
        (*(op as *mut CoroWrapper)).coro = (*o).to_ref().into_raw();
        crate::gc::track(op);
        op
    }
}

#[repr(C)]
struct CoroWrapper {
    ob_base: PyObject,
    coro: *mut PyObject,
}
unsafe extern "C" fn cw_next(o: *mut PyObject) -> *mut PyObject {
    unsafe { gen_iternext((*(o as *mut CoroWrapper)).coro) }
}
unsafe extern "C" fn cw_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        xdecref((*(o as *mut CoroWrapper)).coro);
        crate::alloc::free_object(o);
    }
}
unsafe extern "C" fn cw_traverse(o: *mut PyObject, visit: visitproc, arg: *mut c_void) -> c_int {
    unsafe {
        let c = (*(o as *mut CoroWrapper)).coro;
        if !c.is_null() { visit.unwrap()(c, arg) } else { 0 }
    }
}
fn cw_send(o: &PyObject, v: &PyObject) -> PyResult {
    unsafe { gen_send(&*(*(o.as_ptr() as *mut CoroWrapper)).coro, v) }
}
fn cw_throw(o: &PyObject, args: &[&PyObject]) -> PyResult {
    unsafe { gen_throw(&*(*(o.as_ptr() as *mut CoroWrapper)).coro, args) }
}
fn cw_close(o: &PyObject) -> PyResult {
    unsafe { gen_close(&*(*(o.as_ptr() as *mut CoroWrapper)).coro) }
}

/// A generator created from code with CO_ITERABLE_COROUTINE (`@types.coroutine`).
pub fn gen_is_iterable_coroutine(o: &PyObject) -> bool {
    is_generator_exact(o) && crate::types::code::code(unsafe { &*g(o).code }).flags() & bc::co::ITERABLE_COROUTINE != 0
}

/// `await o` target: the iterator to drive.
pub fn get_awaitable_iter(o: &PyObject) -> PyResult {
    if is_coroutine_exact(o) {
        return Ok(o.to_ref());
    }
    if is_generator_exact(o) {
        let gn = g(o);
        let flags = crate::types::code::code(unsafe { &*gn.code }).flags();
        if flags & bc::co::ITERABLE_COROUTINE != 0 {
            return Ok(o.to_ref());
        }
    }
    let am = o.ty().tp_as_async;
    unsafe {
        if !am.is_null() {
            if let Some(f) = (*am).am_await {
                let r = from_c(f(o.as_ptr()))?;
                if is_coroutine_exact(&r) || gen_is_iterable_coroutine(&r) {
                    raise!(TypeError, "__await__() must return an iterator, not coroutine");
                }
                if r.ty().tp_iternext.is_none() {
                    raise!(TypeError, "__await__() must return an iterator, not {}", tname(&r));
                }
                return Ok(r);
            }
        }
    }
    raise!(TypeError, "'{}' object can't be awaited", tname(o));
}

pub fn get_aiter(o: &PyObject) -> PyResult {
    let am = o.ty().tp_as_async;
    unsafe {
        if !am.is_null() {
            if let Some(f) = (*am).am_aiter {
                let r = from_c(f(o.as_ptr()))?;
                let ram = r.ty().tp_as_async;
                if ram.is_null() || (*ram).am_anext.is_none() {
                    raise!(
                        TypeError,
                        "'async for' received an object from __aiter__ that does not implement __anext__: {}",
                        tname(&r)
                    );
                }
                return Ok(r);
            }
        }
    }
    raise!(TypeError, "'async for' requires an object with __aiter__ method, got {}", tname(o));
}

pub fn get_anext_awaitable(aiter: &PyObject) -> PyResult {
    let am = aiter.ty().tp_as_async;
    let next = unsafe {
        if am.is_null() || (*am).am_anext.is_none() {
            raise!(TypeError, "'async for' requires an iterator with __anext__ method, got {}", tname(aiter));
        }
        from_c((*am).am_anext.unwrap()(aiter.as_ptr()))?
    };
    match get_awaitable_iter(&next) {
        Ok(r) => Ok(r),
        Err(_) => {
            // reported as a TypeError caused by the original error
            let cause = take_exc().unwrap_or_else(none::none_ref);
            let msg = format!("'async for' received an invalid object from __anext__: {}", tname(&next));
            let te = crate::types::exceptions::make_exception(exc::TypeError(), &[pystr::new_str(&msg)])?;
            if !cause.is(none::none()) {
                ab::setattr(&te, &pystr::new_str("__context__"), &cause)?;
                ab::setattr(&te, &pystr::new_str("__cause__"), &cause)?;
            }
            set_exc(te);
            Err(PyErr)
        }
    }
}

pub fn init_types() {
    unsafe {
        let gen_methods = crate::mdefs![
            ("send", O, gen_send, "send($self, value, /)\n--\n\nResume the generator, making the current yield expression return value."),
            ("throw", FAST, gen_throw),
            ("close", NOARGS, gen_close_m, "close($self, /)\n--\n\nRaise GeneratorExit inside the generator to make it finish."),
            ("__sizeof__", NOARGS, gen_sizeof),
        ];
        for (tp, nm) in [(gen_type(), c"generator"), (coro_type(), c"coroutine"), (asyncgen_type(), c"async_generator")] {
            let t = &mut *tp;
            t.tp_name = nm.as_ptr();
            t.tp_basicsize = std::mem::size_of::<GenObject>() as isize;
            t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_HAVE_FINALIZE;
            t.tp_dealloc = Some(gen_dealloc);
            t.tp_traverse = Some(gen_traverse);
            t.tp_repr = Some(gen_repr);
            t.tp_finalize = Some(gen_finalize);
            t.tp_weaklistoffset = std::mem::offset_of!(GenObject, weakreflist) as isize;
            t.tp_getattro = Some(object_generic_getattro);
            t.tp_methods = gen_methods;
        }
        let t = &mut *gen_type();
        t.tp_iter = Some(self_iter);
        t.tp_iternext = Some(gen_iternext);
        t.tp_getset = crate::getset_table![
            ("__name__", get_name, set_name),
            ("__qualname__", get_qualname, set_qualname),
            ("gi_frame", get_frame),
            ("gi_running", get_running),
            ("gi_suspended", get_suspended),
            ("gi_state", get_gen_state),
            ("gi_code", get_code),
            ("gi_yieldfrom", get_yieldfrom),
        ];
        static mut CORO_ASYNC: PyAsyncMethods = PyAsyncMethods::ZERO;
        (*(&raw mut CORO_ASYNC)).am_await = Some(coro_await);
        let t = &mut *coro_type();
        t.tp_as_async = &raw mut CORO_ASYNC;
        t.tp_methods = crate::mdefs![
            ("send", O, gen_send, "send($self, value, /)\n--\n\nSend a value into coroutine, resuming it; the value becomes the result of the\nawait it is suspended at."),
            ("throw", FAST, gen_throw, "throw(value)\nthrow(type[,value[,traceback]])\n\nRaise an exception in coroutine at the point where it is suspended."),
            ("close", NOARGS, gen_close_m, "close($self, /)\n--\n\nRaise GeneratorExit inside coroutine to make it finish."),
            ("__sizeof__", NOARGS, gen_sizeof),
        ];
        t.tp_getset = crate::getset_table![
            ("__name__", get_name, set_name; "Name of the coroutine."),
            ("__qualname__", get_qualname, set_qualname; "Qualified name of the coroutine."),
            ("cr_frame", get_frame),
            ("cr_running", get_running),
            ("cr_suspended", get_suspended),
            ("cr_code", get_code),
            ("cr_await", get_yieldfrom),
            ("cr_origin", get_cr_origin),
        ];
        crate::types::asyncgen::setup_asyncgen_type(asyncgen_type());

        let t = &mut *(&raw mut CORO_WRAPPER_TYPE);
        t.tp_name = c"coroutine_wrapper".as_ptr();
        t.tp_basicsize = std::mem::size_of::<CoroWrapper>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
        t.tp_dealloc = Some(cw_dealloc);
        t.tp_traverse = Some(cw_traverse);
        t.tp_iter = Some(self_iter);
        t.tp_iternext = Some(cw_next);
        t.tp_getattro = Some(object_generic_getattro);
        t.tp_methods = crate::mdefs![("send", O, cw_send), ("throw", FAST, cw_throw), ("close", NOARGS, cw_close)];
    }
}

pub fn gen_frame(o: &PyObject) -> *mut Frame {
    g(o).frame
}
pub fn gen_set_finalizer(o: &PyObject, f: Option<PyRef>) {
    let gn = g(o);
    let old = std::mem::replace(&mut gn.finalizer, f.map_or(std::ptr::null_mut(), |x| x.into_raw()));
    unsafe { xdecref(old) };
}
pub fn gen_hooks_inited(o: &PyObject) -> &mut bool {
    &mut g(o).hooks_inited
}
pub fn gen_running_async(o: &PyObject) -> &mut bool {
    &mut g(o).running_async
}
pub fn gen_send_pub(o: &PyObject, v: &PyObject) -> PyResult {
    gen_send(o, v)
}
pub fn gen_throw_pub(o: &PyObject, args: &[&PyObject]) -> PyResult {
    gen_throw(o, args)
}
pub fn gen_throw_exc(o: &PyObject, e: PyRef) -> PyResult<SendResult> {
    gen_send_ex(o, None, Some(e), false)
}

/// Finalize a generator whose frame never started (frame.clear() on it): an unawaited
/// coroutine reports its RuntimeWarning now.
pub fn finalize_unstarted(o: &PyObject) {
    unsafe { gen_finalize(o.as_ptr()) }
    let gn = g(o);
    if !gn.frame.is_null() && unsafe { (*gn.frame).state == FrameState::Created } {
        finish(gn);
    }
}
