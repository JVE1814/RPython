//! Event dispatch for sys.settrace / sys.setprofile and sys.monitoring (PEP 669).
//! The interpreter calls these hooks while `sys::TRACING` is set.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::PyObject;
use crate::interp::frame::Frame;
use crate::interp::Exit;
use crate::object::*;
use crate::threads::ThreadState;
use crate::types::{int, none, str as pystr, tuple};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering::*};

// ------------------------------------------------------------------ monitoring state

pub const NTOOLS: usize = 6;
pub const EVENT_NAMES: [&str; 19] = [
    "PY_START",
    "PY_RESUME",
    "PY_RETURN",
    "PY_YIELD",
    "CALL",
    "LINE",
    "INSTRUCTION",
    "JUMP",
    "BRANCH_LEFT",
    "BRANCH_RIGHT",
    "STOP_ITERATION",
    "RAISE",
    "EXCEPTION_HANDLED",
    "PY_UNWIND",
    "PY_THROW",
    "RERAISE",
    "C_RETURN",
    "C_RAISE",
    "BRANCH",
];
pub const PY_START: usize = 0;
pub const PY_RESUME: usize = 1;
pub const PY_RETURN: usize = 2;
pub const PY_YIELD: usize = 3;
pub const LINE: usize = 5;
pub const INSTRUCTION: usize = 6;
pub const RAISE: usize = 11;
pub const PY_UNWIND: usize = 13;
/// Events that can be set per code object (3.15: every event, the exception events and
/// the ancillary C_RETURN/C_RAISE included).
pub const LOCAL_EVENTS_MASK: u32 = (1 << 19) - 1;
/// Events a callback may disable by returning DISABLE (per location for instrumented
/// events, per code object for the exception events).
pub const DISABLE_MASK: u32 = ((1 << 16) - 1) | (1 << 18);

pub struct MonState {
    pub names: [Option<usize>; NTOOLS],
    pub callbacks: [[usize; 19]; NTOOLS],
    pub global: [u32; NTOOLS],
    /// code object address -> (owned code ref, per-tool events)
    pub local: HashMap<usize, (usize, [u32; NTOOLS])>,
    /// (tool, code, offset, event) locations disabled by returning DISABLE
    pub disabled: HashSet<(usize, usize, u32, usize)>,
}

pub static MON: std::sync::LazyLock<Mutex<MonState>> = std::sync::LazyLock::new(|| Mutex::new(MonState {
    names: [None; NTOOLS],
    callbacks: [[0; 19]; NTOOLS],
    global: [0; NTOOLS],
    local: HashMap::new(),
    disabled: HashSet::new(),
}));
/// Union of all enabled monitoring events (global or local) — fast check.
pub static MON_ANY: AtomicU32 = AtomicU32::new(0);
/// Number of threads with a trace or profile function.
pub static TRACERS: AtomicUsize = AtomicUsize::new(0);

pub fn recompute_tracing() {
    let any = MON_ANY.load(Acquire) != 0 || TRACERS.load(Acquire) > 0;
    crate::modules::sys::TRACING.store(any, Release);
}

pub fn recompute_mon_any(m: &MonState) {
    let mut any = 0u32;
    for t in 0..NTOOLS {
        any |= m.global[t];
    }
    for (_, (_, ev)) in m.local.iter() {
        for t in 0..NTOOLS {
            any |= ev[t];
        }
    }
    MON_ANY.store(any, Release);
    recompute_tracing();
}

pub static DISABLE_OBJ: std::sync::atomic::AtomicPtr<PyObject> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

fn frame_obj(f: &mut Frame) -> PyRef {
    unsafe { PyRef::from_borrowed(crate::interp::eval::frame_object(f)) }
}

/// Call `func(args)` with tracing suppressed for this thread.
fn call_suppressed(ts: &ThreadState, func: &PyObject, args: &[&PyObject]) -> PyResult {
    ts.tracing_depth.set(ts.tracing_depth.get() + 1);
    let r = ab::call(func, args);
    ts.tracing_depth.set(ts.tracing_depth.get() - 1);
    r
}

/// The event bit that enables `ev`: C_RETURN and C_RAISE follow CALL.
#[inline]
fn enable_bit(ev: usize) -> usize {
    if ev == 16 || ev == 17 { 4 } else { ev }
}

fn events_for(m: &MonState, code: usize, ev: usize) -> u32 {
    let mut tools = 0u32;
    for t in 0..NTOOLS {
        let mut mask = m.global[t];
        if let Some((_, l)) = m.local.get(&code) {
            mask |= l[t];
        }
        if mask & (1 << enable_bit(ev)) != 0 && m.callbacks[t][ev] != 0 {
            tools |= 1 << t;
        }
    }
    tools
}

/// Fire a monitoring event: callbacks get (code, offset_or_line, *extra).
fn fire(ts: &ThreadState, f: &mut Frame, ev: usize, loc: u32, extra: &[&PyObject]) -> PyResult<()> {
    if MON_ANY.load(Relaxed) & (1 << enable_bit(ev)) == 0 {
        return Ok(());
    }
    let code = f.code().as_ptr() as usize;
    let calls: Vec<(usize, PyRef)> = {
        let m = MON.lock().unwrap();
        let tools = events_for(&m, code, ev);
        (0..NTOOLS)
            .filter(|t| tools & (1 << t) != 0)
            .filter(|t| !m.disabled.contains(&(*t, code, loc, ev)) && !m.disabled.contains(&(*t, code, u32::MAX, ev)))
            .map(|t| (t, unsafe { PyRef::from_borrowed(m.callbacks[t][ev] as *mut PyObject) }))
            .collect()
    };
    if calls.is_empty() {
        return Ok(());
    }
    let co = f.code().to_ref();
    let locobj = int::new_int(loc as i64);
    let mut args: Vec<&PyObject> = vec![&co, &locobj];
    args.extend_from_slice(extra);
    // callbacks of line/instruction/jump/branch events may set f_lineno
    let jumpable = matches!(ev, LINE | INSTRUCTION | 7 | 8 | 9);
    for (tool, cb) in calls {
        let saved = f.trace_event;
        if jumpable {
            f.trace_event = crate::interp::jump::EV_LINE;
        }
        let r = call_suppressed(ts, &cb, &args);
        f.trace_event = saved;
        let r = r?;
        let dis = DISABLE_OBJ.load(Acquire);
        if !dis.is_null() && r.as_ptr() == dis {
            if (1u32 << ev) & DISABLE_MASK == 0 {
                crate::raise!(ValueError, "Cannot disable {} events. Callback removed.", EVENT_NAMES[ev]);
            }
            // exception events are disabled for the whole code object (location u32::MAX)
            let at = if ev >= 11 && ev <= 15 { u32::MAX } else { loc };
            MON.lock().unwrap().disabled.insert((tool, code, at, ev));
        }
    }
    Ok(())
}

fn offset(f: &Frame) -> u32 {
    crate::types::code::lasti_bytes(f.code(), f.lasti as usize) as u32
}

// ------------------------------------------------------------------ settrace/setprofile helpers

fn call_trace(ts: &ThreadState, func: *mut PyObject, f: &mut Frame, what: &str, arg: &PyObject) -> PyResult {
    let fo = frame_obj(f);
    let w = pystr::intern_str(what);
    use crate::interp::jump as j;
    let ev = match what {
        "call" => j::EV_CALL,
        "line" => j::EV_LINE,
        "return" => j::EV_RETURN,
        "exception" => j::EV_EXCEPTION,
        "opcode" => j::EV_OPCODE,
        _ => 0,
    };
    let saved = std::mem::replace(&mut f.trace_event, ev);
    let r = call_suppressed(ts, unsafe { &*func }, &[&fo, &w, arg]);
    f.trace_event = saved;
    if r.is_err() {
        // a failing trace function is removed
        disable_trace(ts);
    }
    r
}

fn disable_trace(ts: &ThreadState) {
    let old = ts.trace_func.replace(std::ptr::null_mut());
    if !old.is_null() {
        TRACERS.fetch_sub(1, AcqRel);
        recompute_tracing();
        unsafe { xdecref(old) };
    }
}

fn set_f_trace(f: &mut Frame, v: PyRef) {
    let nv = if v.is(none::none()) { std::ptr::null_mut() } else { v.into_raw() };
    let old = std::mem::replace(&mut f.f_trace, nv);
    unsafe { xdecref(old) };
}

fn call_profile(ts: &ThreadState, f: &mut Frame, what: &str, arg: &PyObject) -> PyResult<()> {
    let p = ts.profile_func.get();
    if p.is_null() {
        return Ok(());
    }
    let fo = frame_obj(f);
    let w = pystr::intern_str(what);
    let pr = unsafe { PyRef::from_borrowed(p) };
    let r = call_suppressed(ts, &pr, &[&fo, &w, arg]);
    if r.is_err() {
        let old = ts.profile_func.replace(std::ptr::null_mut());
        if !old.is_null() {
            TRACERS.fetch_sub(1, AcqRel);
            recompute_tracing();
            unsafe { xdecref(old) };
        }
        return Err(PyErr);
    }
    Ok(())
}

// ------------------------------------------------------------------ hooks called by the interpreter

pub fn trace_call(ts: &ThreadState, f: &mut Frame) -> PyResult<()> {
    if ts.tracing_depth.get() > 0 {
        return Ok(());
    }
    let cd = crate::types::code::code_data(f.code());
    if cd.eager_prologue && f.next_instr < cd.gen_start {
        // the eager part of a generator prologue (before RETURN_GENERATOR) is not traced
        return Ok(());
    }
    let resume = f.next_instr > cd.gen_start;
    f.trace_last = u32::MAX;
    if !resume {
        f.trace_line = -1;
    }
    fire(ts, f, if resume { PY_RESUME } else { PY_START }, offset(f), &[])?;
    call_profile(ts, f, "call", none::none())?;
    let t = ts.trace_func.get();
    if !t.is_null() {
        let r = call_trace(ts, t, f, "call", none::none())?;
        set_f_trace(f, r);
    }
    Ok(())
}

fn is_line_start(code: &PyObject, i: usize, line: i32) -> bool {
    i == 0 || crate::types::code::line_for_instr(code, i - 1).map(|l| l as i32) != Some(line)
}

pub fn trace_instr(ts: &ThreadState, f: &mut Frame) -> PyResult<()> {
    if ts.tracing_depth.get() > 0 {
        return Ok(());
    }
    {
        let cd = crate::types::code::code_data(f.code());
        if cd.eager_prologue && f.lasti < cd.gen_start {
            return Ok(());
        }
    }
    let lasti = f.lasti;
    let prev = std::mem::replace(&mut f.trace_last, lasti);
    // instructions without a line number (artificial cleanup code) never start a line
    let line = match crate::types::code::code_data(f.code()).locations.get(lasti as usize) {
        Some(l) if l.lineno != 0 => l.lineno as i32,
        _ => -1,
    };
    // A line event fires on the first instruction of a line (entered by falling
    // through or by a jump, including a backward jump to the same line); never on
    // RESUME (the `def` line) and not when resuming in the middle of a line.
    let op = crate::types::code::code_data(f.code()).code.get(lasti as usize).map(|w| (w & 0xff) as u8);
    let is_resume = op == Some(rpy_compiler::bytecode::Op::Resume as u8);
    let start = is_line_start(f.code(), lasti as usize, line);
    let jumped = prev != u32::MAX && lasti != prev.wrapping_add(1);
    let new_line = line >= 0
        && !is_resume
        && ((start && (line != f.trace_line || (jumped && lasti <= prev))) || (jumped && (line != f.trace_line || lasti <= prev)));
    if new_line {
        f.trace_line = line;
        if MON_ANY.load(Relaxed) & (1 << LINE) != 0 && start {
            fire(ts, f, LINE, line as u32, &[])?;
        }
    }
    // one INSTRUCTION / opcode event per public instruction (an instruction may be
    // implemented by several internal words)
    let first_word = crate::types::code::code_data(f.code()).starts_public_instr(lasti as usize);
    // RESUME produces no INSTRUCTION event
    if first_word && !is_resume && MON_ANY.load(Relaxed) & (1 << INSTRUCTION) != 0 {
        fire(ts, f, INSTRUCTION, offset(f), &[])?;
    }
    if ts.trace_func.get().is_null() || f.f_trace.is_null() {
        return Ok(());
    }
    if new_line && f.f_trace_lines {
        let ft = f.f_trace;
        let ftr = unsafe { PyRef::from_borrowed(ft) };
        let r = call_trace(ts, ftr.as_ptr(), f, "line", none::none())?;
        set_f_trace(f, r);
    }
    if first_word && f.f_trace_opcodes && !f.f_trace.is_null() {
        let ftr = unsafe { PyRef::from_borrowed(f.f_trace) };
        let r = call_trace(ts, ftr.as_ptr(), f, "opcode", none::none())?;
        set_f_trace(f, r);
    }
    Ok(())
}

pub fn trace_exit(ts: &ThreadState, f: &mut Frame, e: &Exit) -> PyResult<()> {
    if ts.tracing_depth.get() > 0 {
        return Ok(());
    }
    let (v, is_yield) = match e {
        Exit::Return(v) => (v.clone(), false),
        Exit::Yield(v) => (v.clone(), true),
    };
    fire(ts, f, if is_yield { PY_YIELD } else { PY_RETURN }, offset(f), &[&v])?;
    call_profile(ts, f, "return", &v)?;
    if !ts.trace_func.get().is_null() && !f.f_trace.is_null() {
        let ftr = unsafe { PyRef::from_borrowed(f.f_trace) };
        call_trace(ts, ftr.as_ptr(), f, "return", &v)?;
    }
    if !is_yield {
        f.trace_last = u32::MAX;
    }
    Ok(())
}

pub fn trace_exception(ts: &ThreadState, f: &mut Frame, e: &PyObject) -> PyResult<()> {
    if ts.tracing_depth.get() > 0 {
        return Ok(());
    }
    // the exception must not be "set" while callbacks run
    fire(ts, f, RAISE, offset(f), &[e])?;
    if !ts.trace_func.get().is_null() && !f.f_trace.is_null() {
        let t = crate::types::typeobject::type_ref(e.type_ptr());
        let tb = crate::types::exceptions::exc_traceback(e).unwrap_or_else(none::none_ref);
        let arg = tuple::new_tuple(vec![t, e.to_ref(), tb]);
        let ftr = unsafe { PyRef::from_borrowed(f.f_trace) };
        let r = call_trace(ts, ftr.as_ptr(), f, "exception", &arg)?;
        set_f_trace(f, r);
    }
    Ok(())
}

pub fn trace_unwind(ts: &ThreadState, f: &mut Frame) -> PyResult<()> {
    if ts.tracing_depth.get() > 0 {
        return Ok(());
    }
    let e = take_exc();
    let r = (|| -> PyResult<()> {
        if let Some(e) = &e {
            fire(ts, f, PY_UNWIND, offset(f), &[e])?;
        }
        call_profile(ts, f, "return", none::none())?;
        if !ts.trace_func.get().is_null() && !f.f_trace.is_null() {
            let ftr = unsafe { PyRef::from_borrowed(f.f_trace) };
            call_trace(ts, ftr.as_ptr(), f, "return", none::none())?;
        }
        Ok(())
    })();
    f.trace_last = u32::MAX;
    match r {
        Ok(()) => {
            if let Some(e) = e {
                set_exc(e);
            }
            Ok(())
        }
        Err(err) => Err(err),
    }
}

// ------------------------------------------------------------------ calls

pub const CALL: usize = 4;
pub const JUMP: usize = 7;
pub const BRANCH_LEFT: usize = 8;
pub const BRANCH_RIGHT: usize = 9;
pub const STOP_ITERATION: usize = 10;
pub const EXCEPTION_HANDLED: usize = 12;
pub const PY_THROW: usize = 14;
pub const RERAISE: usize = 15;
pub const C_RETURN: usize = 16;
pub const C_RAISE: usize = 17;

pub static MISSING_OBJ: std::sync::atomic::AtomicPtr<PyObject> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

fn missing() -> PyRef {
    let p = MISSING_OBJ.load(Acquire);
    if p.is_null() { none::none_ref() } else { unsafe { PyRef::from_borrowed(p) } }
}

/// What a call site reported before the call, so that the matching return/raise
/// event can be delivered afterwards.
pub struct CallInfo {
    /// callable reported to monitoring (after unpacking bound methods)
    callable: PyRef,
    arg0: PyRef,
    /// C function reported to the profiler, if the callee is not Python code
    c_func: Option<PyRef>,
    offset: u32,
}

fn is_c_callable(c: &PyObject) -> bool {
    crate::types::cfunction::is_cfunction(c) || c.is_type(crate::types::descr::method_descr_type())
}

/// Called before a CALL executes while tracing is active. `first` is the first
/// explicit argument; `unpack`: the call site unpacks bound methods.
pub fn before_call(
    ts: &ThreadState,
    f: &mut Frame,
    callable: &PyObject,
    self_or_null: Option<&PyObject>,
    first: Option<&PyObject>,
    unpack: bool,
) -> PyResult<Option<CallInfo>> {
    if ts.tracing_depth.get() > 0 {
        return Ok(None);
    }
    let any = MON_ANY.load(Relaxed);
    let want_mon = any & ((1 << CALL) | (1 << C_RETURN) | (1 << C_RAISE)) != 0;
    let want_prof = !ts.profile_func.get().is_null();
    if !want_mon && !want_prof {
        return Ok(None);
    }
    let (callable, self_arg): (PyRef, Option<PyRef>) =
        if unpack && self_or_null.is_none() && crate::types::funcobject::is_method(callable) {
            let (func, slf) = crate::types::funcobject::method_parts(callable);
            (func.to_ref(), Some(slf.to_ref()))
        } else {
            (callable.to_ref(), self_or_null.map(|s| s.to_ref()))
        };
    let arg0 = match (&self_arg, first) {
        (Some(s), _) => s.clone(),
        (None, Some(a)) => a.to_ref(),
        (None, None) => missing(),
    };
    let off = offset(f);
    if any & (1 << CALL) != 0 {
        fire(ts, f, CALL, off, &[&callable, &arg0])?;
    }
    let c_func = if !is_c_callable(&callable) {
        None
    } else if callable.is_type(crate::types::descr::method_descr_type()) && self_arg.is_some() {
        // a method descriptor called with its self is reported as the bound method
        let s = self_arg.as_ref().unwrap();
        match callable.ty().tp_descr_get {
            Some(g) => match unsafe { from_c(g(callable.as_ptr(), s.as_ptr(), s.type_ptr() as *mut PyObject)) } {
                Ok(b) => Some(b),
                Err(_) => {
                    clear_exc();
                    Some(callable.clone())
                }
            },
            None => Some(callable.clone()),
        }
    } else {
        Some(callable.clone())
    };
    if want_prof {
        if let Some(cf) = &c_func {
            call_profile(ts, f, "c_call", cf)?;
        }
    }
    Ok(Some(CallInfo { callable, arg0, c_func, offset: off }))
}

/// Called after the call (`ok`: it returned normally). On error the pending
/// exception is kept unless a hook fails.
pub fn after_call(ts: &ThreadState, f: &mut Frame, info: CallInfo, ok: bool) -> PyResult<()> {
    let python_callee = crate::types::funcobject::is_function(&info.callable);
    let any = MON_ANY.load(Relaxed);
    if ok {
        if !python_callee && any & (1 << CALL) != 0 {
            fire(ts, f, C_RETURN, info.offset, &[&info.callable, &info.arg0])?;
        }
        if let Some(cf) = &info.c_func {
            call_profile(ts, f, "c_return", cf)?;
        }
        return Ok(());
    }
    let e = take_exc();
    let r = (|| -> PyResult<()> {
        if !python_callee && any & (1 << CALL) != 0 {
            fire(ts, f, C_RAISE, info.offset, &[&info.callable, &info.arg0])?;
        }
        if let Some(cf) = &info.c_func {
            call_profile(ts, f, "c_exception", cf)?;
        }
        Ok(())
    })();
    match r {
        Ok(()) => {
            if let Some(e) = e {
                set_exc(e);
            }
            Ok(())
        }
        Err(x) => Err(x),
    }
}

/// Fire an exception-carrying event while keeping the current exception set.
pub fn fire_exc_event(ts: &ThreadState, f: &mut Frame, ev: usize, e: &PyObject) -> PyResult<()> {
    if ts.tracing_depth.get() > 0 || MON_ANY.load(Relaxed) & (1 << ev) == 0 {
        return Ok(());
    }
    let saved = take_exc();
    let off = offset(f);
    let r = fire(ts, f, ev, off, &[e]);
    match r {
        Ok(()) => {
            if let Some(s) = saved {
                set_exc(s);
            }
            Ok(())
        }
        Err(x) => Err(x),
    }
}

/// JUMP / BRANCH_LEFT / BRANCH_RIGHT from the current instruction to `dest` (index).
pub fn fire_branch(ts: &ThreadState, f: &mut Frame, ev: usize, dest: usize) -> PyResult<()> {
    if ts.tracing_depth.get() > 0 || MON_ANY.load(Relaxed) & (1 << ev) == 0 {
        return Ok(());
    }
    let d = int::new_int(crate::types::code::lasti_bytes(f.code(), dest));
    let off = offset(f);
    fire(ts, f, ev, off, &[&d])
}

#[inline]
pub fn wants(ev: usize) -> bool {
    MON_ANY.load(Relaxed) & (1 << ev) != 0
}
