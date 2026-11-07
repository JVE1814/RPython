//! The `gc` module: interface to the cycle collector in crate::gc.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::{bool_, dict, int, list, none, str as pystr, tuple};
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicI64, AtomicPtr, AtomicUsize, Ordering::*};

pub const DEBUG_STATS: i64 = 1;
pub const DEBUG_COLLECTABLE: i64 = 2;
pub const DEBUG_UNCOLLECTABLE: i64 = 4;
pub const DEBUG_SAVEALL: i64 = 32;
pub const DEBUG_LEAK: i64 = DEBUG_COLLECTABLE | DEBUG_UNCOLLECTABLE | DEBUG_SAVEALL;

static DEBUG: AtomicI64 = AtomicI64::new(0);
static GARBAGE: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
static CALLBACKS: AtomicPtr<PyObject> = AtomicPtr::new(std::ptr::null_mut());
static FROZEN: AtomicUsize = AtomicUsize::new(0);
static THRESHOLD1: AtomicI64 = AtomicI64::new(10);
static THRESHOLD2: AtomicI64 = AtomicI64::new(0);
static GEN_COUNTS: [AtomicUsize; 2] = [AtomicUsize::new(0), AtomicUsize::new(0)];
static COLLECTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn module_list(p: &AtomicPtr<PyObject>) -> PyRef {
    let v = p.load(Acquire);
    if !v.is_null() {
        return unsafe { PyRef::from_borrowed(v) };
    }
    let l = list::new_list_empty();
    match p.compare_exchange(std::ptr::null_mut(), l.clone().into_raw(), AcqRel, Acquire) {
        Ok(_) => l,
        Err(o) => unsafe { PyRef::from_borrowed(o) },
    }
}

fn gc_enable(_m: &PyObject) -> PyResult {
    crate::gc::ENABLED.store(true, Release);
    Ok(none::none_ref())
}
fn gc_disable(_m: &PyObject) -> PyResult {
    crate::gc::ENABLED.store(false, Release);
    Ok(none::none_ref())
}
fn gc_isenabled(_m: &PyObject) -> PyResult {
    Ok(bool_::new_bool(crate::gc::ENABLED.load(Acquire)))
}

fn run_callbacks(phase: &str, generation: i64, collected: usize, uncollectable: usize, candidates: usize, duration: f64) {
    let cbs = module_list(&CALLBACKS);
    if list::list_len(&cbs) == 0 {
        return;
    }
    let info = dict::new_dict();
    let _ = dict::set_item_str(&info, "generation", &int::new_int(generation));
    let _ = dict::set_item_str(&info, "collected", &int::new_int(collected as i64));
    let _ = dict::set_item_str(&info, "uncollectable", &int::new_int(uncollectable as i64));
    let _ = dict::set_item_str(&info, "candidates", &int::new_int(candidates as i64));
    let _ = dict::set_item_str(&info, "duration", &crate::types::float::new_float(duration));
    let ph = pystr::new_str(phase);
    for cb in list::list_to_vec(&cbs) {
        if ab::call(&cb, &[&ph, &info]).is_err() {
            crate::modules::sys_full::write_unraisable(Some("Exception ignored in garbage collection callback"), Some(&cb));
        }
    }
}

/// Full collection with callbacks (used by gc.collect()).
pub fn collect_with_callbacks(generation: i64) -> usize {
    if COLLECTING.swap(true, AcqRel) {
        return 0;
    }
    run_callbacks("start", generation, 0, 0, 0, 0.0);
    let before_unc = crate::gc::UNCOLLECTABLE.load(Relaxed);
    let before = gc_totals();
    let cand0 = crate::gc_support::CANDIDATES.load(Relaxed);
    let dur0 = crate::gc_support::DURATION_NS.load(Relaxed);
    let n = crate::gc::collect();
    match generation {
        0 => {
            GEN_COUNTS[0].fetch_add(1, Relaxed);
        }
        1 => {
            GEN_COUNTS[0].store(0, Relaxed);
            GEN_COUNTS[1].fetch_add(1, Relaxed);
        }
        _ => {
            GEN_COUNTS[0].store(0, Relaxed);
            GEN_COUNTS[1].store(0, Relaxed);
        }
    }
    let candidates = crate::gc_support::CANDIDATES.load(Relaxed).saturating_sub(cand0);
    let duration = crate::gc_support::DURATION_NS.load(Relaxed).saturating_sub(dur0) as f64 / 1e9;
    if generation < 2 {
        let after = gc_totals();
        for i in 0..4 {
            YOUNG[generation as usize][i].fetch_add(after[i].saturating_sub(before[i]), Relaxed);
        }
    }
    let unc = crate::gc::UNCOLLECTABLE.load(Relaxed).saturating_sub(before_unc);
    run_callbacks("stop", generation, n.saturating_sub(unc), unc, candidates, duration);
    COLLECTING.store(false, Release);
    n
}

fn gc_collect(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("collect", &["generation"], 0, 0);
    let [g] = crate::args::parse::<1>(&P, args, kw)?;
    let generation = match g {
        Some(g) => ab::as_i64(g)?,
        None => 2,
    };
    if !(0..=2).contains(&generation) {
        raise!(ValueError, "invalid generation");
    }
    let n = collect_with_callbacks(generation);
    // (capi2) type watchers queued while the collector cleared types
    crate::capi::watchers::flush_type_events();
    Ok(int::new_int(n as i64))
}

fn gc_get_count(_m: &PyObject) -> PyResult {
    let n = crate::gc::tracked_count().max(0) as i64;
    // [1] and [2]: collections of the younger generation since the last one of
    // this generation (explicit gc.collect(n) calls)
    Ok(tuple::new_tuple(vec![
        int::new_int(n % (crate::gc::THRESHOLD.load(Relaxed).max(1) as i64)),
        int::new_int(GEN_COUNTS[0].load(Relaxed) as i64),
        int::new_int(GEN_COUNTS[1].load(Relaxed) as i64),
    ]))
}
fn gc_get_threshold(_m: &PyObject) -> PyResult {
    Ok(tuple::new_tuple(vec![
        int::new_int(crate::gc::THRESHOLD.load(Relaxed) as i64),
        int::new_int(THRESHOLD1.load(Relaxed)),
        int::new_int(THRESHOLD2.load(Relaxed)),
    ]))
}
fn gc_set_threshold(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::args::check_pos("set_threshold", args, 1, 3)?;
    let t0 = ab::as_i64(args[0])?;
    crate::gc::THRESHOLD.store(t0.max(0) as usize, Relaxed);
    crate::gc_support::USER_THRESHOLD.store(true, Relaxed);
    if t0 == 0 {
        // a zero threshold disables automatic collection
    }
    if let Some(t) = args.get(1) {
        THRESHOLD1.store(ab::as_i64(t)?, Relaxed);
    }
    if let Some(t) = args.get(2) {
        THRESHOLD2.store(ab::as_i64(t)?, Relaxed);
    }
    Ok(none::none_ref())
}
fn gc_get_debug(_m: &PyObject) -> PyResult {
    Ok(int::new_int(DEBUG.load(Relaxed)))
}
fn gc_set_debug(_m: &PyObject, f: &PyObject) -> PyResult {
    DEBUG.store(ab::as_i64(f)?, Relaxed);
    Ok(none::none_ref())
}

fn gc_get_objects(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    static P: crate::args::Params = crate::args::Params::new("get_objects", &["generation"], 0, 0);
    let [g] = crate::args::parse::<1>(&P, args, kw)?;
    if let Some(g) = g {
        if !g.is(none::none()) {
            let n = ab::as_i64(g)?;
            if n >= 3 {
                raise!(ValueError, "generation parameter must be less than the number of available generations (3)");
            }
            if n < 0 {
                raise!(ValueError, "generation parameter cannot be negative");
            }
        }
    }
    crate::modules::sys_full::audit("gc.get_objects", &[g.unwrap_or(none::none())])?;
    Ok(list::new_list(crate::gc::all_tracked()))
}

unsafe extern "C" fn collect_visit(o: *mut PyObject, arg: *mut c_void) -> c_int {
    unsafe {
        let v = &mut *(arg as *mut Vec<usize>);
        if !o.is_null() {
            v.push(o as usize);
        }
        0
    }
}

fn referents_of(o: &PyObject) -> Vec<usize> {
    let mut v: Vec<usize> = Vec::new();
    unsafe {
        if crate::alloc::type_is_gc(o.type_ptr()) || crate::types::typeobject::is_type(o) {
            if let Some(t) = (*o.type_ptr()).tp_traverse {
                t(o.as_ptr(), Some(collect_visit), &mut v as *mut Vec<usize> as *mut c_void);
            }
        }
    }
    v
}

fn gc_get_referents(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::modules::sys_full::audit("gc.get_referents", &[&tuple::new_tuple(args.iter().map(|a| a.to_ref()).collect())])?;
    let mut out = Vec::new();
    for a in args {
        // the traversal reads attribute storage lock-free (scale2: qsbr::note_attr_read)
        crate::qsbr::note_attr_read(a);
        for p in referents_of(a) {
            out.push(unsafe { PyRef::from_borrowed(p as *mut PyObject) });
        }
    }
    Ok(list::new_list(out))
}

fn gc_get_referrers(_m: &PyObject, args: &[&PyObject]) -> PyResult {
    crate::modules::sys_full::audit("gc.get_referrers", &[&tuple::new_tuple(args.iter().map(|a| a.to_ref()).collect())])?;
    let targets: Vec<usize> = args.iter().map(|a| a.as_ptr() as usize).collect();
    let mut out = Vec::new();
    for o in crate::gc::all_tracked() {
        if targets.contains(&(o.as_ptr() as usize)) && false {
            continue;
        }
        if referents_of(&o).iter().any(|p| targets.contains(p)) {
            out.push(o);
        }
    }
    Ok(list::new_list(out))
}

fn gc_is_tracked(_m: &PyObject, o: &PyObject) -> PyResult {
    if crate::types::typeobject::is_type(o) {
        // Types of extension modules (dotted tp_name) are statically allocated
        // here but are collectable heap types in the reference implementation;
        // report them as tracked like any heap type.
        let tp = o.as_ptr() as *mut PyTypeObject;
        unsafe {
            if (*tp).tp_flags & Py_TPFLAGS_HEAPTYPE == 0 && !(*tp).tp_name.is_null() {
                let name = std::ffi::CStr::from_ptr((*tp).tp_name).to_bytes();
                if name.contains(&b'.') && !name.starts_with(b"builtins.") {
                    return Ok(bool_::new_bool(true));
                }
            }
        }
    }
    Ok(bool_::new_bool(crate::gc::is_tracked(o)))
}
fn gc_is_finalized(_m: &PyObject, o: &PyObject) -> PyResult {
    let fin = crate::alloc::type_is_gc(o.type_ptr()) && unsafe { crate::gc::is_finalized(o.as_ptr()) };
    Ok(bool_::new_bool(fin))
}
fn gc_freeze(_m: &PyObject) -> PyResult {
    FROZEN.store(crate::gc::tracked_count().max(0) as usize, Relaxed);
    Ok(none::none_ref())
}
fn gc_unfreeze(_m: &PyObject) -> PyResult {
    FROZEN.store(0, Relaxed);
    Ok(none::none_ref())
}
fn gc_get_freeze_count(_m: &PyObject) -> PyResult {
    Ok(int::new_int(FROZEN.load(Relaxed) as i64))
}
fn gc_get_stats(_m: &PyObject) -> PyResult {
    let mut v = Vec::new();
    for g in 0..3 {
        let d = dict::new_dict();
        let tot = gc_totals();
        let s: [usize; 4] = if g == 2 {
            std::array::from_fn(|i| tot[i].saturating_sub(YOUNG[0][i].load(Relaxed) + YOUNG[1][i].load(Relaxed)))
        } else {
            std::array::from_fn(|i| YOUNG[g][i].load(Relaxed))
        };
        let (c, n, u) = (s[0], s[1], if g == 2 { crate::gc::UNCOLLECTABLE.load(Relaxed) } else { 0 });
        dict::set_item_str(&d, "collections", &int::new_int(c as i64))?;
        dict::set_item_str(&d, "collected", &int::new_int(n as i64))?;
        dict::set_item_str(&d, "uncollectable", &int::new_int(u as i64))?;
        let (cand, dur) = (s[2], s[3]);
        dict::set_item_str(&d, "candidates", &int::new_int(cand as i64))?;
        dict::set_item_str(&d, "duration", &crate::types::float::new_float(dur as f64 / 1e9))?;
        v.push(d);
    }
    Ok(list::new_list(v))
}

pub fn init_module() -> PyResult {
    let m = crate::types::module::new_module("gc")?;
    let d = crate::types::module::module_dict(&m);
    crate::builtins::add_methods_to_dict(
        &d,
        crate::mdefs![
            ("enable", NOARGS, gc_enable),
            ("disable", NOARGS, gc_disable),
            ("isenabled", NOARGS, gc_isenabled),
            ("collect", FASTKW, gc_collect, "collect($module, /, generation=2)\n--\n\nRun a garbage collection and return the number of unreachable objects found."),
            ("get_count", NOARGS, gc_get_count),
            ("get_threshold", NOARGS, gc_get_threshold),
            ("set_threshold", FAST, gc_set_threshold),
            ("get_debug", NOARGS, gc_get_debug),
            ("set_debug", O, gc_set_debug, "set_debug($module, flags, /)\n--\n\nSet the collector's debugging flags."),
            ("get_objects", FASTKW, gc_get_objects, "get_objects($module, /, generation=None)\n--\n\nReturn the objects tracked by the collector."),
            ("get_referents", FAST, gc_get_referents, "get_referents($module, /, *objs)\n--\n\nReturn the objects directly referred to by any of the arguments."),
            ("get_referrers", FAST, gc_get_referrers, "get_referrers($module, /, *objs)\n--\n\nReturn the tracked objects that refer directly to any of the arguments."),
            ("is_tracked", O, gc_is_tracked, "is_tracked($module, obj, /)\n--\n\nReturn True if the collector tracks obj."),
            ("is_finalized", O, gc_is_finalized, "is_finalized($module, obj, /)\n--\n\nReturn True if obj's finalizer has already run."),
            ("freeze", NOARGS, gc_freeze),
            ("unfreeze", NOARGS, gc_unfreeze),
            ("get_freeze_count", NOARGS, gc_get_freeze_count),
            ("get_stats", NOARGS, gc_get_stats),
        ],
        Some(&m),
    )?;
    dict::set_item_str(&d, "garbage", &module_list(&GARBAGE))?;
    dict::set_item_str(&d, "callbacks", &module_list(&CALLBACKS))?;
    for (k, v) in [
        ("DEBUG_STATS", DEBUG_STATS),
        ("DEBUG_COLLECTABLE", DEBUG_COLLECTABLE),
        ("DEBUG_UNCOLLECTABLE", DEBUG_UNCOLLECTABLE),
        ("DEBUG_SAVEALL", DEBUG_SAVEALL),
        ("DEBUG_LEAK", DEBUG_LEAK),
    ] {
        dict::set_item_str(&d, k, &int::new_int(v))?;
    }
    Ok(m)
}

/// Per-generation statistics of explicit young collections (gc.collect(0/1)):
/// [collections, collected, candidates, duration_ns]. Generation 2 reports the
/// collector's totals minus these.
static YOUNG: [[AtomicUsize; 4]; 2] = [const { [const { AtomicUsize::new(0) }; 4] }; 2];

fn gc_totals() -> [usize; 4] {
    [
        crate::gc::COLLECTIONS.load(Relaxed),
        crate::gc::COLLECTED.load(Relaxed),
        crate::gc_support::CANDIDATES.load(Relaxed),
        crate::gc_support::DURATION_NS.load(Relaxed),
    ]
}

/// Current gc.set_debug() flags (used by the collector for DEBUG_SAVEALL).
pub fn debug_flags() -> i64 {
    DEBUG.load(Relaxed)
}

/// The gc.garbage list.
pub fn garbage_list() -> PyRef {
    module_list(&GARBAGE)
}
