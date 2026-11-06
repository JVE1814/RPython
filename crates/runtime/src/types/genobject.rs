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
        let fo = crate::types::funcobject::func(func);
        gn.name = (*fo.func_name).to_ref().into_raw();
        gn.qualname = (*fo.func_qualname).to_ref().into_raw();
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
    let gn = g(o);
    if gn.running {
        let msg = if is_coroutine_exact(o) {
            "coroutine already executing"
        } else if is_asyncgen(o) {
            "async generator already executing"
        } else {
            "generator already executing"
        };
        raise!(ValueError, "{}", msg);
    }
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
    } else if throw.is_none() {
        let v = arg.map(|a| a.to_ref()).unwrap_or_else(none::none_ref);
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
    gn.running = true;
    // compiled code that suspends the frame again records its continuation
    frame.resume = 0;
    let r = crate::interp::enter_recursive_call(ts, "").and_then(|_| {
        let r = eval_frame(ts, frame, throw);
        crate::interp::leave_recursive_call(ts);
        r
    });
    gn.running = false;
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
    let fp = gn.frame;
    if !gn.running && !fp.is_null() {
        let f = unsafe { &mut *fp };
        if f.resume != 0 && !crate::modules::sys::tracing_active() {
            let ok = match f.state {
                FrameState::Suspended => true,
                FrameState::Created => arg.is_null() || arg == none::none().as_ptr(),
                _ => false,
            };
            if ok {
                let ts = crate::threads::current();
                let d = ts.recursion_depth.get() + 1;
                if d <= ts.c.py_recursion_limit && crate::interp::stack_pointer() >= ts.stack_soft_limit() {
                    return resume_compiled(ts, gn, f, arg, d);
                }
            }
        }
    }
    gen_send_ex(o, if arg.is_null() { None } else { Some(unsafe { &*arg }) }, None, false)
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
    gn.running = true;
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
    gn.running = false;
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

pub fn gen_throw_impl(o: &PyObject, typ: &PyObject, val: Option<&PyObject>, tb: Option<&PyObject>, close_on_genexit: bool) -> PyResult {
    // delegate to sub-iterator first
    if let Some(yf) = yieldfrom(o) {
        if typ.is(type_obj(exc::GeneratorExit())) && close_on_genexit {
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
            // the delegating generator's frame is the caller of the sub-generator
            let ts = crate::threads::current();
            let gf = g(o).frame;
            let saved = ts.frame.get();
            if !gf.is_null() {
                unsafe { (*gf).prev = saved };
                ts.frame.set(gf);
            }
            let r = gen_throw_impl(&yf, typ, val, tb, close_on_genexit);
            ts.frame.set(saved);
            if !gf.is_null() {
                unsafe { (*gf).prev = std::ptr::null_mut() };
            }
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
                    let r = ab::call(&m, &args);
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
    if let Some(m) = ab::getattr_opt(yf, name!("close"))? {
        ab::call(&m, &[])?;
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

pub fn gen_close(o: &PyObject) -> PyResult {
    let gn = g(o);
    if gn.frame.is_null() {
        return Ok(none::none_ref());
    }
    if unsafe { (*gn.frame).state == FrameState::Created } {
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
                let msg = format!("coroutine '{}' was never awaited", pystr::to_string_lossy(&*gn.qualname));
                if crate::modules::warnings::warn_with_source(exc::RuntimeWarning(), &msg, 1, Some(oo)).is_err() {
                    crate::modules::sys::write_unraisable_with_obj("Exception ignored while finalizing coroutine", Some(oo));
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
            crate::modules::sys::write_unraisable_with_obj(
                &format!("Exception ignored while closing {}", kind_name(oo)),
                Some(oo),
            );
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
        if !crate::gc::is_finalized(o) {
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
            let old = std::mem::replace(&mut g(o).name, v.to_ref().into_raw());
            unsafe { xdecref(old) };
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
            let old = std::mem::replace(&mut g(o).qualname, v.to_ref().into_raw());
            unsafe { xdecref(old) };
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
                    raise!(TypeError, "__await__() returned a coroutine");
                }
                if r.ty().tp_iternext.is_none() {
                    raise!(TypeError, "__await__() returned non-iterator of type '{}'", tname(&r));
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
        Err(e) => {
            if exc_matches(exc::TypeError()) {
                clear_exc();
                raise!(
                    TypeError,
                    "'async for' received an invalid object from __anext__: {}",
                    tname(&next)
                );
            }
            Err(e)
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
            ("gi_code", get_code),
            ("gi_yieldfrom", get_yieldfrom),
        ];
        static mut CORO_ASYNC: PyAsyncMethods = PyAsyncMethods::ZERO;
        (*(&raw mut CORO_ASYNC)).am_await = Some(coro_await);
        let t = &mut *coro_type();
        t.tp_as_async = &raw mut CORO_ASYNC;
        t.tp_getset = crate::getset_table![
            ("__name__", get_name, set_name),
            ("__qualname__", get_qualname, set_qualname),
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
