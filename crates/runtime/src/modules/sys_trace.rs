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
/// Events that can be set per code object (and disabled per location).
pub const LOCAL_EVENTS_MASK: u32 = (1 << 11) - 1 | (1 << 18);

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

thread_local! {
    /// frame address -> last executed instruction (for backward-jump line events)
    static LAST: RefCell<HashMap<usize, u32>> = RefCell::new(HashMap::new());
}

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

fn events_for(m: &MonState, code: usize, ev: usize) -> u32 {
    let mut tools = 0u32;
    for t in 0..NTOOLS {
        let mut mask = m.global[t];
        if let Some((_, l)) = m.local.get(&code) {
            mask |= l[t];
        }
        if mask & (1 << ev) != 0 && m.callbacks[t][ev] != 0 {
            tools |= 1 << t;
        }
    }
    tools
}

/// Fire a monitoring event: callbacks get (code, offset_or_line, *extra).
fn fire(ts: &ThreadState, f: &mut Frame, ev: usize, loc: u32, extra: &[&PyObject]) -> PyResult<()> {
    if MON_ANY.load(Relaxed) & (1 << ev) == 0 {
        return Ok(());
    }
    let code = f.code().as_ptr() as usize;
    let calls: Vec<(usize, PyRef)> = {
        let m = MON.lock().unwrap();
        let tools = events_for(&m, code, ev);
        (0..NTOOLS)
            .filter(|t| tools & (1 << t) != 0)
            .filter(|t| !m.disabled.contains(&(*t, code, loc, ev)))
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
    for (tool, cb) in calls {
        let r = call_suppressed(ts, &cb, &args)?;
        let dis = DISABLE_OBJ.load(Acquire);
        if !dis.is_null() && r.as_ptr() == dis {
            if (1u32 << ev) & LOCAL_EVENTS_MASK == 0 {
                crate::raise!(ValueError, "Cannot disable {} events. Callback removed.", EVENT_NAMES[ev]);
            }
            MON.lock().unwrap().disabled.insert((tool, code, loc, ev));
        }
    }
    Ok(())
}

fn offset(f: &Frame) -> u32 {
    f.lasti * 4
}

// ------------------------------------------------------------------ settrace/setprofile helpers

fn call_trace(ts: &ThreadState, func: *mut PyObject, f: &mut Frame, what: &str, arg: &PyObject) -> PyResult {
    let fo = frame_obj(f);
    let w = pystr::intern_str(what);
    let r = call_suppressed(ts, unsafe { &*func }, &[&fo, &w, arg]);
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
    let resume = f.next_instr > 0;
    LAST.with(|l| {
        l.borrow_mut().insert(f as *mut Frame as usize, u32::MAX);
    });
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
    let lasti = f.lasti;
    let key = f as *mut Frame as usize;
    let prev = LAST.with(|l| l.borrow_mut().insert(key, lasti)).unwrap_or(u32::MAX);
    let line = crate::types::code::line_for_instr(f.code(), lasti as usize).map(|l| l as i32).unwrap_or(-1);
    // A line event fires on the first instruction of a line (entered by falling
    // through or by a jump, including a backward jump to the same line); never on
    // RESUME (the `def` line) and not when resuming in the middle of a line.
    let op = crate::types::code::code_data(f.code()).code.get(lasti as usize).map(|w| (w & 0xff) as u8);
    let is_resume = op == Some(rpy_compiler::bytecode::Op::Resume as u8);
    let start = is_line_start(f.code(), lasti as usize, line);
    let jumped = prev != u32::MAX && lasti != prev.wrapping_add(1);
    let new_line = line >= 0
        && !is_resume
        && ((start && (line != f.trace_line || jumped)) || (jumped && (line != f.trace_line || lasti <= prev)));
    if new_line {
        f.trace_line = line;
        if MON_ANY.load(Relaxed) & (1 << LINE) != 0 {
            fire(ts, f, LINE, line as u32, &[])?;
        }
    }
    if MON_ANY.load(Relaxed) & (1 << INSTRUCTION) != 0 {
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
    if f.f_trace_opcodes && !f.f_trace.is_null() {
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
        LAST.with(|l| {
            l.borrow_mut().remove(&(f as *mut Frame as usize));
        });
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
    LAST.with(|l| {
        l.borrow_mut().remove(&(f as *mut Frame as usize));
    });
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
