//! (tail4) Garbage-collection statistics of another process: `GCStatsInfo`,
//! `GCMonitor(pid).get_gc_stats()` and `get_gc_stats(pid)`. The target records one
//! entry per (interpreter, generation) in a table published by the debug table
//! (layout: modules/gc_mod.rs).

use super::{Process, Table, to_mem_err};
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::structseq::{self, Desc, Field};
use crate::types::{bool_, dict, float, int, list};
use std::ffi::c_void;

static mut GC_STATS_INFO: PyTypeObject = PyTypeObject::ZERO;
fn gc_stats_info_type() -> *mut PyTypeObject {
    &raw mut GC_STATS_INFO
}

static mut MONITOR_TYPE: PyTypeObject = PyTypeObject::ZERO;
fn monitor_type() -> *mut PyTypeObject {
    &raw mut MONITOR_TYPE
}

#[repr(C)]
struct Monitor {
    ob_base: PyObject,
    inner: *mut (Process, Table),
}

/// Read the target's statistics table; one GCStatsInfo per (interpreter, generation),
/// the main interpreter always with all three generations.
fn read_stats(proc: &Process, table: &Table, all_interpreters: bool) -> PyResult {
    if !proc.alive() {
        raise!(ProcessLookupError, "process {} is not running", proc.pid);
    }
    if table.gc_stats == 0 || table.gc_stat_words < 10 {
        raise!(RuntimeError, "the target process does not record garbage collection statistics");
    }
    let words = table.gc_stat_words as usize;
    let slots = table.gc_stat_slots as usize;
    let raw = proc.read_vec(table.gc_stats, words * slots * 8).map_err(to_mem_err)?;
    let word = |slot: usize, i: usize| {
        let at = (slot * words + i) * 8;
        u64::from_le_bytes(raw[at..at + 8].try_into().unwrap())
    };
    let mut recs: Vec<[u64; 10]> = Vec::new();
    for slot in 0..slots {
        let key = word(slot, 0);
        if key == 0 {
            continue;
        }
        let iid = key - 1;
        if !all_interpreters && iid != 0 {
            continue;
        }
        let mut r = [0u64; 10];
        for (i, v) in r.iter_mut().enumerate() {
            *v = word(slot, i);
        }
        r[0] = iid;
        recs.push(r);
    }
    for generation_no in 0..3u64 {
        if !recs.iter().any(|r| r[0] == 0 && r[1] == generation_no) {
            recs.push([0, generation_no, 0, 0, 0, 0, 0, 0, 0, 0]);
        }
    }
    recs.sort_by_key(|r| (r[0], r[1]));
    let items: Vec<PyRef> = recs
        .iter()
        .map(|r| {
            structseq::new_instance(
                gc_stats_info_type(),
                vec![
                    int::new_int(r[1] as i64),
                    int::new_int(r[0] as i64),
                    int::new_int_u128(r[8] as u128),
                    int::new_int_u128(r[9] as u128),
                    int::new_int_u128(r[2] as u128),
                    int::new_int_u128(r[3] as u128),
                    int::new_int_u128(r[4] as u128),
                    int::new_int_u128(r[5] as u128),
                    int::new_int_u128(r[6] as u128),
                    float::new_float(r[7] as f64 / 1e9),
                ],
            )
        })
        .collect();
    Ok(list::new_list(items))
}

fn pid_of(o: &PyObject) -> PyResult<i32> {
    match int::as_i64(o) {
        Some(p) if p > 0 && p <= i32::MAX as i64 => Ok(p as i32),
        Some(_) => raise!(ValueError, "invalid process id"),
        None => raise!(TypeError, "pid must be an int"),
    }
}

fn all_interpreters_arg(v: Option<&PyObject>) -> PyResult<bool> {
    match v {
        Some(v) => crate::abstract_::is_true(v),
        None => Ok(false),
    }
}

pub(super) fn module_get_gc_stats(_m: &PyObject, args: &[&PyObject], kwnames: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("get_gc_stats", &["pid", "all_interpreters"], 0, 1).kwonly(1);
    let parsed = crate::args::parse::<2>(&P, args, kwnames)?;
    let proc = Process::new(pid_of(parsed[0].unwrap())?);
    let table = Table::load(&proc)?;
    read_stats(&proc, &table, all_interpreters_arg(parsed[1])?)
}

fn monitor_get_gc_stats(o: &PyObject, args: &[&PyObject], kwnames: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("get_gc_stats", &["all_interpreters"], 0, 0).kwonly(1);
    let parsed = crate::args::parse::<1>(&P, args, kwnames)?;
    let (proc, table) = unsafe { &*(*(o.as_ptr() as *const Monitor)).inner };
    read_stats(proc, table, all_interpreters_arg(parsed[0])?)
}

unsafe extern "C" fn monitor_new(tp: *mut PyTypeObject, args: *mut PyObject, kw: *mut PyObject) -> *mut PyObject {
    unsafe {
        let r = (|| -> PyResult {
            let (vals, kws) = crate::args::kwdict_to_vectorcall(&*args, kw);
            let refs: Vec<&PyObject> = vals.iter().map(|x| &**x).collect();
            static P: crate::args::Params = crate::args::Params::new("GCMonitor", &["pid", "debug"], 0, 1).kwonly(1);
            let parsed = crate::args::parse::<2>(&P, &refs, kws.as_deref())?;
            let proc = Process::new(pid_of(parsed[0].unwrap())?);
            let table = Table::load(&proc)?;
            let op = ((*tp).tp_alloc.unwrap())(tp, 0);
            if op.is_null() {
                return Err(PyErr);
            }
            (*(op as *mut Monitor)).inner = Box::into_raw(Box::new((proc, table)));
            Ok(PyRef::from_raw(op))
        })();
        into_c(r)
    }
}

unsafe extern "C" fn monitor_dealloc(op: *mut PyObject) {
    unsafe {
        crate::gc::untrack(op);
        let m = op as *mut Monitor;
        if !(*m).inner.is_null() {
            drop(Box::from_raw((*m).inner));
        }
        crate::types::typeobject::generic_free_and_type_decref(op, (*op).type_ptr());
    }
}

unsafe extern "C" fn empty_traverse(_op: *mut PyObject, _visit: visitproc, _arg: *mut c_void) -> std::ffi::c_int {
    0
}

pub(super) fn init(d: &PyObject) -> PyResult<()> {
    unsafe {
        structseq::init_type(gc_stats_info_type(), &Desc {
            name: "_remote_debugging.GCStatsInfo",
            doc: "Garbage collection statistics of one generation of one interpreter.",
            fields: &[
                Field::new("gen", "generation"),
                Field::new("iid", "interpreter id"),
                Field::new("ts_start", "start of the last collection (monotonic ns)"),
                Field::new("ts_stop", "end of the last collection (monotonic ns)"),
                Field::new("collections", "number of collections"),
                Field::new("collected", "objects collected"),
                Field::new("uncollectable", "uncollectable objects found"),
                Field::new("candidates", "objects examined"),
                Field::new("heap_size", "tracked objects after the last collection"),
                Field::new("duration", "total time spent collecting (seconds)"),
            ],
            n_in_sequence: 10,
        })?;
        let t = &mut *monitor_type();
        t.tp_name = crate::methods::cstr_static("_remote_debugging.GCMonitor");
        t.tp_basicsize = std::mem::size_of::<Monitor>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
        t.tp_new = Some(monitor_new);
        t.tp_dealloc = Some(monitor_dealloc);
        t.tp_traverse = Some(empty_traverse);
        t.tp_doc = crate::methods::cstr_static("GCMonitor(pid, *, debug=False)\n--\n\nRead garbage collection statistics of process pid.");
        t.tp_methods = crate::mdefs![
            ("get_gc_stats", FASTKW, monitor_get_gc_stats, "get_gc_stats($self, /, *, all_interpreters=False)\n--\n\nCollection statistics per generation (and interpreter)."),
        ];
        crate::types::typeobject::type_ready(monitor_type())?;
    }
    dict::set_item_str(d, "GCStatsInfo", &crate::types::typeobject::type_ref(gc_stats_info_type()))?;
    dict::set_item_str(d, "GCMonitor", &crate::types::typeobject::type_ref(monitor_type()))?;
    let _ = bool_::new_bool(false);
    Ok(())
}
