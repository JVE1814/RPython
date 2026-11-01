//! `_lsprof`: the deterministic profiler behind cProfile / profiling.tracing.
//!
//! A `Profiler` registers itself as sys.monitoring tool PROFILER_ID ("cProfile")
//! and receives PY_START/PY_RESUME/PY_THROW (start of a Python call),
//! PY_RETURN/PY_YIELD/PY_UNWIND (end of one), CALL (start of a builtin call) and
//! C_RETURN/C_RAISE (end of one).  It keeps one table entry per profiled
//! function (keyed by code object, or by method definition for builtins) with
//! per-caller sub-entries, and a stack of active calls per thread.
//!
//! Times are kept as f64 "ticks": nanoseconds of CLOCK_MONOTONIC for the
//! built-in timer, otherwise the value returned by the user's timer (seconds
//! when `timeunit` is 0, else a raw count multiplied by `timeunit`).
//!
//! The table is guarded by a mutex that is never held while Python code (the
//! external timer) runs; the timer may not clear, disable or re-initialise the
//! profiler (RuntimeError, reported as unraisable by the hook).

use crate::abstract_ as ab;
use crate::args::Params;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::fncommon::{self as fc, Trav};
use crate::object::*;
use crate::types::structseq::{self, Desc, Field};
use crate::types::{cfunction, dict, float, int, list, module, none, str as pystr, typeobject};
use crate::{mdefs, raise};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

const PROFILER_ID: i64 = 2;
// event numbers (bit positions) of sys.monitoring
const EV_PY_START: u32 = 0;
const EV_PY_RESUME: u32 = 1;
const EV_PY_RETURN: u32 = 2;
const EV_PY_YIELD: u32 = 3;
const EV_CALL: u32 = 4;
const EV_PY_UNWIND: u32 = 13;
const EV_PY_THROW: u32 = 14;
const EV_C_RETURN: u32 = 16;
const EV_C_RAISE: u32 = 17;

#[derive(Default)]
struct Sub {
    calls: u64,
    rec: u64,
    total: f64,
    inline: f64,
    level: u32,
}

struct Entry {
    /// code object or builtin function (labels the entry in getstats())
    obj: PyRef,
    calls: u64,
    rec: u64,
    total: f64,
    inline: f64,
    level: u32,
    /// callee key -> statistics of calls made from this entry
    subs: HashMap<usize, Sub>,
    sub_order: Vec<usize>,
}

struct Ctx {
    key: usize,
    t0: f64,
    sub_time: f64,
}

#[derive(Default)]
struct Table {
    entries: HashMap<usize, Entry>,
    order: Vec<usize>,
    /// thread id -> active calls of that thread
    stacks: HashMap<usize, Vec<Ctx>>,
}

pub struct Profiler {
    table: Mutex<Table>,
    timer: Option<PyRef>,
    unit: f64,
    subcalls: bool,
    builtins: bool,
    enabled: bool,
    /// >0 while the external timer runs
    in_timer: AtomicU32,
}

impl Trav for Profiler {
    fn trav(&self, v: &mut dyn FnMut(*mut PyObject) -> std::ffi::c_int) -> std::ffi::c_int {
        crate::trav!(v; self.timer)
    }
}

impl Profiler {
    fn fresh() -> Profiler {
        Profiler {
            table: Mutex::new(Table::default()),
            timer: None,
            unit: 0.0,
            subcalls: true,
            builtins: true,
            enabled: false,
            in_timer: AtomicU32::new(0),
        }
    }
}

#[unsafe(no_mangle)]
pub static mut RPY_LSPROF_TYPE: PyTypeObject = PyTypeObject::ZERO;
static ENTRY_TYPE: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());
static SUBENTRY_TYPE: AtomicPtr<PyTypeObject> = AtomicPtr::new(std::ptr::null_mut());

fn prof_type() -> *mut PyTypeObject {
    &raw mut RPY_LSPROF_TYPE
}

#[inline]
fn st(o: &PyObject) -> &mut Profiler {
    fc::rs::<Profiler>(o)
}

fn monotonic_ns() -> f64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as f64 * 1e9 + ts.tv_nsec as f64
}

/// Current time in ticks.  Errors of the external timer are reported as
/// unraisable and count as time 0.
fn now(o: &PyObject) -> f64 {
    let p = st(o);
    let Some(timer) = p.timer.clone() else {
        return monotonic_ns();
    };
    let unit = p.unit;
    p.in_timer.fetch_add(1, Ordering::AcqRel);
    let r = (|| -> PyResult<f64> {
        let v = ab::call0(&timer)?;
        if unit > 0.0 && !float::is_float(&v) {
            let i = ab::index(&v)?;
            return int::to_f64(&i);
        }
        ab::float_value(&v)
    })();
    st(o).in_timer.fetch_sub(1, Ordering::AcqRel);
    match r {
        Ok(t) => t,
        Err(_) => {
            crate::modules::sys_full::write_unraisable(Some("Exception ignored while calling the profiler timer"), Some(&timer));
            0.0
        }
    }
}

fn check_not_in_timer(o: &PyObject, what: &str) -> PyResult<()> {
    if st(o).in_timer.load(Ordering::Acquire) > 0 {
        raise!(RuntimeError, "cannot {} the profiler from within its external timer", what);
    }
    Ok(())
}

// ------------------------------------------------------------------ bookkeeping

fn enter(o: &PyObject, key: usize, obj: &PyObject) {
    let t = now(o);
    let p = st(o);
    let subcalls = p.subcalls;
    let mut tb = p.table.lock().unwrap();
    let tid = thread_id();
    let caller = tb.stacks.get(&tid).and_then(|s| s.last()).map(|c| c.key);
    if !tb.entries.contains_key(&key) {
        tb.order.push(key);
        tb.entries.insert(
            key,
            Entry { obj: obj.to_ref(), calls: 0, rec: 0, total: 0.0, inline: 0.0, level: 0, subs: HashMap::new(), sub_order: Vec::new() },
        );
    }
    tb.entries.get_mut(&key).unwrap().level += 1;
    if subcalls {
        if let Some(c) = caller.and_then(|c| tb.entries.get_mut(&c)) {
            if !c.subs.contains_key(&key) {
                c.sub_order.push(key);
            }
            c.subs.entry(key).or_default().level += 1;
        }
    }
    tb.stacks.entry(tid).or_default().push(Ctx { key, t0: t, sub_time: 0.0 });
}

fn thread_id() -> usize {
    unsafe { libc::pthread_self() as usize }
}

/// Finish the innermost active call of thread `tid` (if `key` is given, only
/// when that call is for `key`).
fn leave(o: &PyObject, tid: usize, key: Option<usize>) {
    {
        let tb = st(o).table.lock().unwrap();
        match (tb.stacks.get(&tid).and_then(|s| s.last()), key) {
            (None, _) => return,
            (Some(c), Some(k)) if c.key != k => return,
            _ => {}
        }
    }
    let t = now(o);
    let p = st(o);
    let subcalls = p.subcalls;
    let mut tb = p.table.lock().unwrap();
    let Some(stack) = tb.stacks.get_mut(&tid) else { return };
    let Some(ctx) = stack.pop() else { return };
    if key.is_some_and(|k| k != ctx.key) {
        // the timer changed the stack under us; put it back untouched
        stack.push(ctx);
        return;
    }
    let total = t - ctx.t0;
    let inline = total - ctx.sub_time;
    let caller = stack.last_mut().map(|c| {
        c.sub_time += total;
        c.key
    });
    if stack.is_empty() {
        tb.stacks.remove(&tid);
    }
    if let Some(e) = tb.entries.get_mut(&ctx.key) {
        e.level = e.level.saturating_sub(1);
        if e.level == 0 {
            e.total += total;
        } else {
            e.rec += 1;
        }
        e.inline += inline;
        e.calls += 1;
    }
    if subcalls {
        if let Some(c) = caller.and_then(|c| tb.entries.get_mut(&c)) {
            if let Some(s) = c.subs.get_mut(&ctx.key) {
                s.level = s.level.saturating_sub(1);
                if s.level == 0 {
                    s.total += total;
                } else {
                    s.rec += 1;
                }
                s.inline += inline;
                s.calls += 1;
            }
        }
    }
}

fn flush(o: &PyObject) {
    loop {
        let Some(tid) = st(o).table.lock().unwrap().stacks.keys().next().copied() else {
            return;
        };
        leave(o, tid, None);
    }
}

/// The builtin function a CALL/C_RETURN event is about, if it is one.
fn builtin_callee(callable: &PyObject, self_arg: &PyObject) -> Option<PyRef> {
    if cfunction::is_cfunction(callable) {
        return Some(callable.to_ref());
    }
    if callable.is_type(crate::types::descr::method_descr_type()) {
        let missing = crate::modules::sys::get_sys_attr("monitoring")
            .and_then(|m| dict::get_item_rs(&module::module_dict(&m), "MISSING"));
        if missing.is_some_and(|m| m.is(self_arg)) {
            return None;
        }
        let get = unsafe { (*callable.type_ptr()).tp_descr_get }?;
        let tp = typeobject::type_ref(self_arg.type_ptr());
        let r = unsafe { from_c(get(callable.as_ptr(), self_arg.as_ptr(), tp.as_ptr())) };
        return match r {
            Ok(f) if cfunction::is_cfunction(&f) => Some(f),
            Ok(_) => None,
            Err(_) => {
                // the call itself will report the problem
                let _ = take_exc();
                None
            }
        };
    }
    None
}

fn builtin_key(f: &PyObject) -> usize {
    unsafe { (*(f.as_ptr() as *mut PyCFunctionObject)).m_ml as usize }
}

// ------------------------------------------------------------------ monitoring callbacks

fn cb_pystart(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("_pystart_callback", args, 2, 2)?;
    enter(o, args[0].as_ptr() as usize, args[0]);
    Ok(none::none_ref())
}

fn cb_pythrow(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("_pythrow_callback", args, 3, 3)?;
    enter(o, args[0].as_ptr() as usize, args[0]);
    Ok(none::none_ref())
}

fn cb_pyreturn(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("_pyreturn_callback", args, 3, 3)?;
    leave(o, thread_id(), Some(args[0].as_ptr() as usize));
    Ok(none::none_ref())
}

fn cb_ccall(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("_ccall_callback", args, 4, 4)?;
    if st(o).builtins {
        if let Some(f) = builtin_callee(args[2], args[3]) {
            enter(o, builtin_key(&f), &f);
        }
    }
    Ok(none::none_ref())
}

fn cb_creturn(o: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("_creturn_callback", args, 4, 4)?;
    if st(o).builtins {
        if let Some(f) = builtin_callee(args[2], args[3]) {
            leave(o, thread_id(), Some(builtin_key(&f)));
        }
    }
    Ok(none::none_ref())
}

// ------------------------------------------------------------------ methods

fn monitoring() -> PyResult {
    match crate::modules::sys::get_sys_attr("monitoring") {
        Some(m) => Ok(m),
        None => raise!(RuntimeError, "sys.monitoring is not available"),
    }
}

fn mon_call(name: &str, args: &[&PyObject]) -> PyResult {
    let m = monitoring()?;
    let f = ab::getattr_str(&m, name)?;
    ab::call(&f, args)
}

const CALLBACKS: &[(u32, &str)] = &[
    (EV_PY_START, "_pystart_callback"),
    (EV_PY_RESUME, "_pystart_callback"),
    (EV_PY_THROW, "_pythrow_callback"),
    (EV_PY_RETURN, "_pyreturn_callback"),
    (EV_PY_YIELD, "_pyreturn_callback"),
    (EV_PY_UNWIND, "_pyreturn_callback"),
    (EV_CALL, "_ccall_callback"),
    (EV_C_RETURN, "_creturn_callback"),
    (EV_C_RAISE, "_creturn_callback"),
];

fn flag_arg(o: Option<&PyObject>, default: bool) -> PyResult<bool> {
    match o {
        Some(v) => ab::is_true(v),
        None => Ok(default),
    }
}

fn p_enable(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: Params = Params::new("enable", &["subcalls", "builtins"], 0, 0);
    let [subcalls, builtins] = crate::args::parse::<2>(&P, args, kw)?;
    let subcalls = flag_arg(subcalls, true)?;
    let builtins = flag_arg(builtins, true)?;
    {
        let p = st(o);
        p.subcalls = subcalls;
        p.builtins = builtins;
        if p.enabled {
            return Ok(none::none_ref());
        }
    }
    let tool = int::new_int(PROFILER_ID);
    mon_call("use_tool_id", &[&tool, &pystr::new_str("cProfile")])?;
    let r = (|| -> PyResult<()> {
        let mut mask = 0i64;
        for (ev, name) in CALLBACKS {
            let cb = ab::getattr_str(o, name)?;
            mon_call("register_callback", &[&tool, &int::new_int(1 << ev), &cb])?;
            mask |= 1 << ev;
        }
        mon_call("set_events", &[&tool, &int::new_int(mask)])?;
        Ok(())
    })();
    if r.is_err() {
        let e = take_exc();
        let _ = mon_call("free_tool_id", &[&tool]);
        let _ = take_exc();
        if let Some(e) = e {
            set_exc(e);
        }
        return Err(PyErr);
    }
    st(o).enabled = true;
    Ok(none::none_ref())
}

fn p_disable(o: &PyObject) -> PyResult {
    check_not_in_timer(o, "disable")?;
    if !st(o).enabled {
        return Ok(none::none_ref());
    }
    st(o).enabled = false;
    let tool = int::new_int(PROFILER_ID);
    mon_call("set_events", &[&tool, &int::new_int(0)])?;
    for (ev, _) in CALLBACKS {
        mon_call("register_callback", &[&tool, &int::new_int(1 << ev), none::none()])?;
    }
    mon_call("free_tool_id", &[&tool])?;
    flush(o);
    Ok(none::none_ref())
}

fn p_clear(o: &PyObject) -> PyResult {
    check_not_in_timer(o, "clear")?;
    let old = std::mem::take(&mut *st(o).table.lock().unwrap());
    drop(old);
    Ok(none::none_ref())
}

fn p_getstats(o: &PyObject) -> PyResult {
    let p = st(o);
    let unit = if p.timer.is_none() {
        1e-9
    } else if p.unit > 0.0 {
        p.unit
    } else {
        1.0
    };
    let et = ENTRY_TYPE.load(Ordering::Acquire);
    let set = SUBENTRY_TYPE.load(Ordering::Acquire);
    // snapshot under the lock, build Python objects afterwards
    struct Row {
        obj: PyRef,
        calls: u64,
        rec: u64,
        total: f64,
        inline: f64,
        subs: Vec<(PyRef, u64, u64, f64, f64)>,
    }
    let rows: Vec<Row> = {
        let tb = p.table.lock().unwrap();
        tb.order
            .iter()
            .filter_map(|k| tb.entries.get(k))
            .map(|e| Row {
                obj: e.obj.clone(),
                calls: e.calls,
                rec: e.rec,
                total: e.total,
                inline: e.inline,
                subs: e
                    .sub_order
                    .iter()
                    .filter_map(|k| Some((tb.entries.get(k)?.obj.clone(), e.subs.get(k)?)))
                    .map(|(obj, s)| (obj, s.calls, s.rec, s.total, s.inline))
                    .collect(),
            })
            .collect()
    };
    let out = list::new_list(Vec::new());
    for r in rows {
        let calls = if r.subs.is_empty() {
            none::none_ref()
        } else {
            let l = list::new_list(Vec::new());
            for (obj, c, rc, t, i) in r.subs {
                let s = structseq::new_instance(
                    set,
                    vec![
                        label(&obj)?,
                        int::new_int(c as i64),
                        int::new_int(rc as i64),
                        float::new_float(t * unit),
                        float::new_float(i * unit),
                    ],
                );
                list::append(&l, s);
            }
            l
        };
        let e = structseq::new_instance(
            et,
            vec![
                label(&r.obj)?,
                int::new_int(r.calls as i64),
                int::new_int(r.rec as i64),
                float::new_float(r.total * unit),
                float::new_float(r.inline * unit),
                calls,
            ],
        );
        list::append(&out, e);
    }
    Ok(out)
}

/// The `code` field of a stats entry: the code object, or a descriptive string
/// for builtins ("<built-in method mod.name>", "<method 'x' of 'T' objects>").
fn label(obj: &PyObject) -> PyResult {
    if !cfunction::is_cfunction(obj) {
        return Ok(obj.to_ref());
    }
    let name = cfunction::cfunction_name(obj);
    let (slf, modname) = unsafe {
        let f = obj.as_ptr() as *mut PyCFunctionObject;
        (
            if (*f).m_self.is_null() { None } else { Some(PyRef::from_borrowed((*f).m_self)) },
            if (*f).m_module.is_null() { None } else { Some(PyRef::from_borrowed((*f).m_module)) },
        )
    };
    let bound = slf.as_ref().filter(|s| !module::is_module(s));
    if let Some(s) = bound {
        if let Some(d) = typeobject::lookup(s.type_ptr(), &pystr::new_str(&name)) {
            return ab::repr(&d);
        }
        return ab::repr(obj);
    }
    let modname = match modname {
        Some(m) if pystr::is_str(&m) => Some(pystr::to_string_lossy(&m)),
        _ => slf
            .as_ref()
            .and_then(|m| ab::getattr_str(m, "__name__").ok())
            .filter(|n| pystr::is_str(n))
            .map(|n| pystr::to_string_lossy(&n)),
    };
    let _ = take_exc_if_any();
    Ok(pystr::new_str(&match modname {
        Some(m) => format!("<built-in method {}.{}>", m, name),
        None => format!("<built-in method {}>", name),
    }))
}

fn take_exc_if_any() -> Option<PyRef> {
    take_exc()
}

// ------------------------------------------------------------------ construction

unsafe extern "C" fn prof_new(tp: *mut PyTypeObject, _args: *mut PyObject, _kw: *mut PyObject) -> *mut PyObject {
    into_c(fc::robj_new(tp, Profiler::fresh()))
}

unsafe extern "C" fn prof_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> std::ffi::c_int {
    let a = unsafe { fc::VcArgs::from_c(args, kw) };
    let o = unsafe { &*o };
    let r = (|| -> PyResult<()> {
        check_not_in_timer(o, "re-initialise")?;
        static P: Params = Params::new("Profiler", &["timer", "timeunit", "subcalls", "builtins"], 0, 0);
        let [timer, unit, subcalls, builtins] = a.parse::<4>(&P)?;
        let unit = match unit {
            Some(u) => ab::float_value(&u)?,
            None => 0.0,
        };
        let subcalls = flag_arg(subcalls.as_deref(), true)?;
        let builtins = flag_arg(builtins.as_deref(), true)?;
        let p = st(o);
        p.timer = timer.filter(|t| !t.is(none::none()));
        p.unit = unit;
        p.subcalls = subcalls;
        p.builtins = builtins;
        Ok(())
    })();
    match r {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

const ENTRY_FIELDS: &[Field] = &[
    Field::new("code", "code object or built-in function name"),
    Field::new("callcount", "how many times this was called"),
    Field::new("reccallcount", "how many times called recursively"),
    Field::new("totaltime", "total time in this entry"),
    Field::new("inlinetime", "inline time in this entry (not in subcalls)"),
    Field::new("calls", "details of the calls"),
];
const SUBENTRY_FIELDS: &[Field] = &[
    Field::new("code", "called code object or built-in function name"),
    Field::new("callcount", "how many times this is called"),
    Field::new("reccallcount", "how many times this is called recursively"),
    Field::new("totaltime", "total time spent in this call"),
    Field::new("inlinetime", "inline time (not in further subcalls)"),
];

fn init_types() -> PyResult<()> {
    if ENTRY_TYPE.load(Ordering::Acquire).is_null() {
        let t = structseq::new_type(&Desc {
            name: "_lsprof.profiler_entry",
            doc: "Statistics of one profiled function.",
            fields: ENTRY_FIELDS,
            n_in_sequence: 6,
        })?;
        ENTRY_TYPE.store(t, Ordering::Release);
        let t = structseq::new_type(&Desc {
            name: "_lsprof.profiler_subentry",
            doc: "Statistics of the calls from one profiled function to another.",
            fields: SUBENTRY_FIELDS,
            n_in_sequence: 5,
        })?;
        SUBENTRY_TYPE.store(t, Ordering::Release);
    }
    unsafe {
        let t = prof_type();
        if (*t).tp_flags & Py_TPFLAGS_READY != 0 {
            return Ok(());
        }
        fc::init_robj_type::<Profiler>(t, c"_lsprof.Profiler", Py_TPFLAGS_BASETYPE);
        (*t).tp_new = Some(prof_new);
        (*t).tp_init = Some(prof_init);
        (*t).tp_doc = c"Profiler(timer=None, timeunit=None, subcalls=True, builtins=True)\n\nCollects deterministic call statistics via sys.monitoring.  `timer`\nis an optional callable returning the current time: a float in seconds,\nor an integer count of `timeunit` seconds.".as_ptr();
        (*t).tp_methods = mdefs![
            ("getstats", NOARGS, p_getstats, "getstats()\n--\n\nReturn a list of profiler_entry records, one per profiled function;\ntheir `calls` field lists profiler_subentry records for its callees."),
            ("enable", FASTKW, p_enable, "enable($self, /, subcalls=True, builtins=True)\n--\n\nStart collecting statistics."),
            ("disable", NOARGS, p_disable, "disable($self, /)\n--\n\nStop collecting statistics."),
            ("clear", NOARGS, p_clear, "clear($self, /)\n--\n\nForget all statistics collected so far."),
            ("_pystart_callback", FAST, cb_pystart),
            ("_pythrow_callback", FAST, cb_pythrow),
            ("_pyreturn_callback", FAST, cb_pyreturn),
            ("_ccall_callback", FAST, cb_ccall),
            ("_creturn_callback", FAST, cb_creturn),
        ];
        typeobject::type_ready(t)?;
    }
    Ok(())
}

pub fn init() -> PyResult {
    init_types()?;
    let (m, d) = fc::new_module("_lsprof", std::ptr::null_mut(), Some("Fast profiler"))?;
    dict::set_item_str(&d, "Profiler", typeobject::type_obj(prof_type()))?;
    dict::set_item_str(&d, "profiler_entry", typeobject::type_obj(ENTRY_TYPE.load(Ordering::Acquire)))?;
    dict::set_item_str(&d, "profiler_subentry", typeobject::type_obj(SUBENTRY_TYPE.load(Ordering::Acquire)))?;
    Ok(m)
}
