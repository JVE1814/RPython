//! sys.settrace / sys.setprofile / sys.monitoring Python-level API.
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::PyObject;
use crate::modules::sys_trace::{self as st, EVENT_NAMES, MON, NTOOLS};
use crate::object::*;
use crate::raise;
use crate::threads::ThreadState;
use crate::types::{dict, int, none, str as pystr};
use std::sync::atomic::Ordering::*;

fn set_func(ts: &ThreadState, cell: &std::cell::Cell<*mut PyObject>, f: &PyObject) {
    let nv = if f.is(none::none()) { std::ptr::null_mut() } else { f.to_ref().into_raw() };
    let old = cell.replace(nv);
    match (old.is_null(), nv.is_null()) {
        (true, false) => {
            st::TRACERS.fetch_add(1, AcqRel);
        }
        (false, true) => {
            st::TRACERS.fetch_sub(1, AcqRel);
        }
        _ => {}
    }
    let _ = ts;
    st::recompute_tracing();
    unsafe { xdecref(old) };
}

fn get_func(cell: &std::cell::Cell<*mut PyObject>) -> PyRef {
    let p = cell.get();
    if p.is_null() { none::none_ref() } else { unsafe { PyRef::from_borrowed(p) } }
}

pub fn settrace(f: &PyObject) -> PyResult {
    crate::modules::sys_full::audit("sys.settrace", &[])?;
    let ts = crate::threads::current();
    set_func(ts, &ts.trace_func, f);
    Ok(none::none_ref())
}
pub fn gettrace() -> PyResult {
    Ok(get_func(&crate::threads::current().trace_func))
}
pub fn setprofile(f: &PyObject) -> PyResult {
    crate::modules::sys_full::audit("sys.setprofile", &[])?;
    let ts = crate::threads::current();
    set_func(ts, &ts.profile_func, f);
    Ok(none::none_ref())
}
pub fn getprofile() -> PyResult {
    Ok(get_func(&crate::threads::current().profile_func))
}
pub fn settrace_all(f: &PyObject) -> PyResult {
    crate::modules::sys_full::audit("sys.settrace", &[])?;
    for ts in crate::threads::live_threads() {
        set_func(ts, &ts.trace_func, f);
    }
    Ok(none::none_ref())
}
pub fn setprofile_all(f: &PyObject) -> PyResult {
    crate::modules::sys_full::audit("sys.setprofile", &[])?;
    for ts in crate::threads::live_threads() {
        set_func(ts, &ts.profile_func, f);
    }
    Ok(none::none_ref())
}
/// sys.call_tracing: call func(*args) with tracing enabled even inside a tracer.
pub fn call_tracing(f: &PyObject, args: &[&PyObject]) -> PyResult {
    let ts = crate::threads::current();
    let saved = ts.tracing_depth.replace(0);
    let r = ab::call(f, args);
    ts.tracing_depth.set(saved);
    r
}

// ------------------------------------------------------------------ sys.monitoring

fn tool_arg(o: &PyObject) -> PyResult<usize> {
    let n = ab::as_i64(o)?;
    if !(0..NTOOLS as i64).contains(&n) {
        raise!(ValueError, "invalid tool {} (must be between 0 and 5)", n);
    }
    Ok(n as usize)
}
fn check_in_use(m: &st::MonState, tool: usize) -> PyResult<()> {
    if m.names[tool].is_none() {
        raise!(ValueError, "tool {} is not in use", tool);
    }
    Ok(())
}
fn event_arg(o: &PyObject) -> PyResult<usize> {
    let n = ab::as_i64(o)?;
    if n <= 0 || n.count_ones() != 1 || (n.trailing_zeros() as usize) >= EVENT_NAMES.len() {
        raise!(ValueError, "invalid event {}", n);
    }
    Ok(n.trailing_zeros() as usize)
}
fn event_set_arg(o: &PyObject, local: bool) -> PyResult<u32> {
    let n = ab::as_i64(o)?;
    let all = (1i64 << EVENT_NAMES.len()) - 1;
    if n < 0 || n & !all != 0 {
        raise!(ValueError, "invalid event set 0x{:x}", n);
    }
    let mut v = n as u32;
    if local && v & !st::LOCAL_EVENTS_MASK != 0 {
        raise!(ValueError, "invalid local event set 0x{:x}", n);
    }
    // C_RETURN / C_RAISE are delivered with CALL only
    if v & ((1 << 16) | (1 << 17)) != 0 && v & (1 << 4) == 0 {
        raise!(ValueError, "cannot set C_RETURN or C_RAISE events independently");
    }
    // BRANCH is an alias for BRANCH_LEFT|BRANCH_RIGHT
    if v & (1 << 18) != 0 {
        v = (v & !(1 << 18)) | (1 << 8) | (1 << 9);
    }
    Ok(v)
}

fn mon_use_tool_id(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("use_tool_id", args, 2, 2)?;
    let tool = tool_arg(args[0])?;
    if !pystr::is_str(args[1]) {
        raise!(ValueError, "tool name must be a str");
    }
    let mut m = MON.lock().unwrap();
    if m.names[tool].is_some() {
        raise!(ValueError, "tool {} is already in use", tool);
    }
    m.names[tool] = Some(args[1].to_ref().into_raw() as usize);
    Ok(none::none_ref())
}
fn clear_tool(m: &mut st::MonState, tool: usize) {
    m.global[tool] = 0;
    for ev in 0..EVENT_NAMES.len() {
        let old = std::mem::replace(&mut m.callbacks[tool][ev], 0);
        unsafe { xdecref(old as *mut PyObject) };
    }
    for (_, (_, l)) in m.local.iter_mut() {
        l[tool] = 0;
    }
    m.disabled.retain(|(t, _, _, _)| *t != tool);
    st::recompute_mon_any(m);
}
fn mon_clear_tool_id(_m: &PyObject, t: &PyObject) -> PyResult {
    let tool = tool_arg(t)?;
    let mut m = MON.lock().unwrap();
    if m.names[tool].is_some() {
        clear_tool(&mut m, tool);
    }
    Ok(none::none_ref())
}
fn mon_free_tool_id(_m: &PyObject, t: &PyObject) -> PyResult {
    let tool = tool_arg(t)?;
    let mut m = MON.lock().unwrap();
    if m.names[tool].is_some() {
        clear_tool(&mut m, tool);
        if let Some(n) = m.names[tool].take() {
            unsafe { decref(n as *mut PyObject) };
        }
    }
    Ok(none::none_ref())
}
fn mon_get_tool(_m: &PyObject, t: &PyObject) -> PyResult {
    let tool = tool_arg(t)?;
    let m = MON.lock().unwrap();
    Ok(match m.names[tool] {
        Some(n) => unsafe { PyRef::from_borrowed(n as *mut PyObject) },
        None => none::none_ref(),
    })
}
fn mon_register_callback(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("register_callback", args, 3, 3)?;
    let tool = tool_arg(args[0])?;
    let ev = event_arg(args[1])?;
    crate::modules::sys_full::audit("sys.monitoring.register_callback", &[args[2]])?;
    let mut m = MON.lock().unwrap();
    // registering (or clearing) a callback does not require the tool to be in use
    let nv = if args[2].is(none::none()) { 0 } else { args[2].to_ref().into_raw() as usize };
    let evs: Vec<usize> = if ev == 18 { vec![8, 9] } else { vec![ev] };
    let mut old = 0usize;
    for (i, e) in evs.iter().enumerate() {
        let nvi = if i > 0 && nv != 0 { unsafe { incref(nv as *mut PyObject); nv } } else { nv };
        let o = std::mem::replace(&mut m.callbacks[tool][*e], nvi);
        if i == 0 {
            old = o;
        } else {
            unsafe { xdecref(o as *mut PyObject) };
        }
    }
    Ok(if old == 0 { none::none_ref() } else { unsafe { PyRef::from_raw(old as *mut PyObject) } })
}
fn mon_get_events(_m: &PyObject, t: &PyObject) -> PyResult {
    let tool = tool_arg(t)?;
    let m = MON.lock().unwrap();
    Ok(int::new_int(m.global[tool] as i64))
}
fn mon_set_events(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("set_events", args, 2, 2)?;
    let tool = tool_arg(args[0])?;
    let evs = event_set_arg(args[1], false)?;
    let mut m = MON.lock().unwrap();
    check_in_use(&m, tool)?;
    m.global[tool] = evs;
    st::recompute_mon_any(&m);
    Ok(none::none_ref())
}
fn code_arg(o: &PyObject) -> PyResult<usize> {
    if !crate::types::code::is_code(o) {
        raise!(TypeError, "code must be a code object");
    }
    Ok(o.as_ptr() as usize)
}
fn mon_get_local_events(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("get_local_events", args, 2, 2)?;
    let tool = tool_arg(args[0])?;
    let code = code_arg(args[1])?;
    let m = MON.lock().unwrap();
    Ok(int::new_int(m.local.get(&code).map_or(0, |(_, l)| l[tool]) as i64))
}
fn mon_set_local_events(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("set_local_events", args, 3, 3)?;
    let tool = tool_arg(args[0])?;
    let code = code_arg(args[1])?;
    let evs = event_set_arg(args[2], true)?;
    let mut m = MON.lock().unwrap();
    check_in_use(&m, tool)?;
    let entry = m.local.entry(code).or_insert_with(|| (args[1].to_ref().into_raw() as usize, [0; NTOOLS]));
    entry.1[tool] = evs;
    st::recompute_mon_any(&m);
    Ok(none::none_ref())
}
fn mon_restart_events(_m: &PyObject) -> PyResult {
    MON.lock().unwrap().disabled.clear();
    Ok(none::none_ref())
}
fn mon_all_events(_m: &PyObject) -> PyResult {
    let d = dict::new_dict();
    let m = MON.lock().unwrap();
    for (ev, name) in EVENT_NAMES.iter().enumerate() {
        if ev == 18 {
            continue;
        }
        let mut tools = 0i64;
        for t in 0..NTOOLS {
            if m.global[t] & (1 << ev) != 0 {
                tools |= 1 << t;
            }
        }
        if tools != 0 {
            dict::set_item_str(&d, name, &int::new_int(tools))?;
        }
    }
    Ok(d)
}

pub fn install_monitoring(d: &PyObject) -> PyResult<()> {
    let m = crate::types::module::new_module("sys.monitoring")?;
    let md = crate::types::module::module_dict(&m);
    crate::builtins::add_methods_to_dict(
        &md,
        crate::mdefs![
            ("use_tool_id", FAST, mon_use_tool_id),
            ("clear_tool_id", O, mon_clear_tool_id),
            ("free_tool_id", O, mon_free_tool_id),
            ("get_tool", O, mon_get_tool),
            ("register_callback", FAST, mon_register_callback),
            ("get_events", O, mon_get_events),
            ("set_events", FAST, mon_set_events),
            ("get_local_events", FAST, mon_get_local_events),
            ("set_local_events", FAST, mon_set_local_events),
            ("restart_events", NOARGS, mon_restart_events),
            ("_all_events", NOARGS, mon_all_events),
        ],
        Some(&m),
    )?;
    let mut items: Vec<(&str, PyRef)> = EVENT_NAMES.iter().enumerate().map(|(i, n)| (*n, int::new_int(1 << i))).collect();
    items.push(("NO_EVENTS", int::new_int(0)));
    let events = crate::modules::sys_namespace::new_namespace(&items)?;
    dict::set_item_str(&md, "events", &events)?;
    let obj = crate::types::typeobject::type_obj(crate::types::typeobject::object_type());
    let disable = ab::call(obj, &[])?;
    st::DISABLE_OBJ.store(disable.clone().into_raw(), Release);
    dict::set_item_str(&md, "DISABLE", &disable)?;
    let missing = ab::call(obj, &[])?;
    st::MISSING_OBJ.store(missing.clone().into_raw(), Release);
    dict::set_item_str(&md, "MISSING", &missing)?;
    for (k, v) in [("DEBUGGER_ID", 0), ("COVERAGE_ID", 1), ("PROFILER_ID", 2), ("OPTIMIZER_ID", 5)] {
        dict::set_item_str(&md, k, &int::new_int(v))?;
    }
    dict::set_item_str(d, "monitoring", &m)?;
    Ok(())
}

// ------------------------------------------------------------------ PEP 669 C API
//
// C code that implements "code-like" objects keeps one `PyMonitoringState` per event
// location. EnterScope fills in which tools listen (globally) for each location's event;
// the Fire functions call those tools' callbacks. A callback returning DISABLE clears its
// tool from that location's state.

#[repr(C)]
pub struct CMonitoringState {
    active: u8,
    opaque: u8,
}

fn c_listeners(ev: usize) -> u8 {
    let bit = if ev == st::C_RETURN || ev == st::C_RAISE { st::CALL } else { ev };
    let m = MON.lock().unwrap();
    let mut tools = 0u8;
    for t in 0..NTOOLS {
        if m.global[t] & (1 << bit) != 0 && m.callbacks[t][ev] != 0 {
            tools |= 1 << t;
        }
    }
    tools
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMonitoring_EnterScope(
    states: *mut CMonitoringState,
    version: *mut u64,
    event_types: *const u8,
    length: crate::ffi::Py_ssize_t,
) -> std::ffi::c_int {
    unsafe {
        for i in 0..length.max(0) as usize {
            let ev = *event_types.add(i) as usize;
            let s = &mut *states.add(i);
            s.active = if ev < EVENT_NAMES.len() { c_listeners(ev) } else { 0 };
            s.opaque = 0;
        }
        if !version.is_null() {
            *version = 1;
        }
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn PyMonitoring_ExitScope() -> std::ffi::c_int {
    0
}

/// Events whose C-level firing reports the currently raised exception.
fn c_needs_exception(ev: usize) -> bool {
    matches!(ev, 11 | 12 | 13 | 14 | 15 | 17)
}

unsafe fn c_fire(state: *mut CMonitoringState, codelike: *mut PyObject, offset: i32, ev: usize, extra: &[*mut PyObject]) -> std::ffi::c_int {
    let ts = crate::threads::current();
    if ts.tracing_depth.get() > 0 || state.is_null() {
        return 0;
    }
    let st_ref = unsafe { &mut *state };
    let raised = if c_needs_exception(ev) {
        match take_exc() {
            Some(e) => Some(e),
            None => {
                let _ = value_error(&format!("Firing event {} with no exception set", ev));
                return -1;
            }
        }
    } else {
        None
    };
    let code = unsafe { PyRef::from_borrowed(codelike) };
    let off = int::new_int(offset as i64);
    let mut args: Vec<&PyObject> = vec![&code, &off];
    let extras: Vec<PyRef> = extra.iter().map(|&p| if p.is_null() { none::none_ref() } else { unsafe { PyRef::from_borrowed(p) } }).collect();
    for e in &extras {
        args.push(e);
    }
    if let Some(e) = &raised {
        args.push(e);
    }
    let result = (|| -> PyResult<()> {
        for t in 0..NTOOLS {
            if st_ref.active & (1 << t) == 0 {
                continue;
            }
            let cb = {
                let m = MON.lock().unwrap();
                let p = m.callbacks[t][ev];
                if p == 0 { None } else { Some(unsafe { PyRef::from_borrowed(p as *mut PyObject) }) }
            };
            let Some(cb) = cb else { continue };
            ts.tracing_depth.set(ts.tracing_depth.get() + 1);
            let r = ab::call(&cb, &args);
            ts.tracing_depth.set(ts.tracing_depth.get() - 1);
            let r = r?;
            let dis = st::DISABLE_OBJ.load(Acquire);
            if !dis.is_null() && r.as_ptr() == dis {
                if (1u32 << ev) & st::DISABLE_MASK == 0 || c_needs_exception(ev) {
                    raise!(ValueError, "Cannot disable {} events. Callback removed.", EVENT_NAMES[ev]);
                }
                st_ref.active &= !(1 << t);
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            if let Some(e) = raised {
                set_exc(e);
            }
            0
        }
        Err(_) => -1,
    }
}

macro_rules! c_fire_fns {
    ($( $name:ident ( $($a:ident),* ) => $ev:expr ; )*) => {
        $(
            #[unsafe(no_mangle)]
            pub unsafe extern "C" fn $name(state: *mut CMonitoringState, codelike: *mut PyObject, offset: i32 $(, $a: *mut PyObject)*) -> std::ffi::c_int {
                unsafe { c_fire(state, codelike, offset, $ev, &[$($a),*]) }
            }
        )*
    };
}
c_fire_fns! {
    _PyMonitoring_FirePyStartEvent() => 0;
    _PyMonitoring_FirePyResumeEvent() => 1;
    _PyMonitoring_FirePyReturnEvent(retval) => 2;
    _PyMonitoring_FirePyYieldEvent(retval) => 3;
    _PyMonitoring_FireCallEvent(callable, arg0) => 4;
    _PyMonitoring_FireJumpEvent(target) => 7;
    _PyMonitoring_FireBranchLeftEvent(target) => 8;
    _PyMonitoring_FireBranchRightEvent(target) => 9;
    _PyMonitoring_FireBranchEvent(target) => 18;
    _PyMonitoring_FireCReturnEvent(retval) => 16;
    _PyMonitoring_FirePyThrowEvent() => 14;
    _PyMonitoring_FireRaiseEvent() => 11;
    _PyMonitoring_FireReraiseEvent() => 15;
    _PyMonitoring_FireExceptionHandledEvent() => 12;
    _PyMonitoring_FireCRaiseEvent() => 17;
    _PyMonitoring_FirePyUnwindEvent() => 13;
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyMonitoring_FireLineEvent(state: *mut CMonitoringState, codelike: *mut PyObject, offset: i32, lineno: i32) -> std::ffi::c_int {
    let l = int::new_int(lineno as i64);
    unsafe { c_fire(state, codelike, offset, st::LINE, &[l.as_ptr()]) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyMonitoring_FireStopIterationEvent(state: *mut CMonitoringState, codelike: *mut PyObject, offset: i32, value: *mut PyObject) -> std::ffi::c_int {
    // callbacks receive an exception: a bare value is wrapped in StopIteration
    let v = unsafe { PyRef::from_borrowed(value) };
    let exc = if crate::types::exceptions::is_exception_instance(&v) {
        v
    } else {
        match ab::call(crate::types::typeobject::type_obj(crate::types::exceptions::StopIteration()), &[&v]) {
            Ok(e) => e,
            Err(_) => return -1,
        }
    };
    unsafe { c_fire(state, codelike, offset, st::STOP_ITERATION, &[exc.as_ptr()]) }
}
