//! Collector policy pieces used by gc.rs (kept separate from the collector core):
//! trigger thresholds set through the `gc` module, statistics, the CPython-compatible
//! count of collected objects and `gc.DEBUG_SAVEALL`.

use crate::ffi::PyObject;
use crate::object::*;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::*};

/// gc.set_threshold() was called: its threshold is honoured as is.
pub static USER_THRESHOLD: AtomicBool = AtomicBool::new(false);
/// Objects examined by collections (sum) and total time spent collecting.
pub static CANDIDATES: AtomicUsize = AtomicUsize::new(0);
pub static DURATION_NS: AtomicUsize = AtomicUsize::new(0);

/// Allocation count that triggers an automatic collection, given the configured
/// threshold and the live set after the last collection. 0 disables automatic
/// collection; otherwise collect after `threshold` new objects, but (unless the
/// threshold was set explicitly) at least after a quarter of the live set.
pub fn trigger_threshold(threshold: usize, last_live: usize) -> usize {
    if threshold == 0 {
        return usize::MAX;
    }
    if USER_THRESHOLD.load(Relaxed) {
        return threshold;
    }
    threshold.max(last_live / 4)
}

/// Instance __dict__s are separate objects in this runtime but part of the instance
/// in CPython's layout. Returns the garbage objects that count as "collected" (the
/// instance dicts owned by other garbage are left out).
pub fn countable(garbage: &[PyRef]) -> Vec<*mut PyObject> {
    let owned: crate::gc::PtrSet = garbage
        .iter()
        .filter_map(|g| unsafe {
            let op = g.as_ptr();
            if crate::alloc::has_managed(op) {
                let d = *crate::alloc::managed_dict_slot(op);
                if !d.is_null() {
                    return Some(d as usize);
                }
            }
            None
        })
        .collect();
    garbage.iter().map(|g| g.as_ptr()).filter(|p| !owned.contains(&(*p as usize))).collect()
}

/// gc.DEBUG_SAVEALL: append the garbage to gc.garbage instead of freeing it.
/// Returns true if the garbage was saved (the caller must not clear it).
pub fn save_all(countable: &[*mut PyObject]) -> bool {
    if crate::modules::gc_mod::debug_flags() & crate::modules::gc_mod::DEBUG_SAVEALL == 0 {
        return false;
    }
    let gl = crate::modules::gc_mod::garbage_list();
    for &p in countable {
        crate::types::list::append(&gl, unsafe { PyRef::from_borrowed(p) });
    }
    true
}

/// Survivors of the last collection, counting the tuples it untracked
/// (`_testinternalcapi.get_long_lived_total`).
pub static LONG_LIVED_TOTAL: AtomicUsize = AtomicUsize::new(0);
/// Tuples untracked by the collection in progress.
pub static UNTRACKED_TUPLES: AtomicUsize = AtomicUsize::new(0);

/// A tuple whose items are all untracked (ints, strs, other untracked tuples...)
/// can never be part of a cycle: stop tracking it, and drop it from `objs`.
/// Runs with the world stopped. Tuples with unset (NULL) items are left alone.
pub unsafe fn untrack_atomic_tuples(objs: &mut Vec<*mut PyObject>) {
    let tt = crate::types::tuple::tuple_type();
    let mut n = 0usize;
    // items before containers is the common allocation order, so walking from
    // the end catches nested tuples of atomics in one pass most of the time
    objs.retain(|&op| unsafe {
        if (*op).type_ptr() != tt {
            return true;
        }
        let len = crate::object::var_size(&*op) as usize;
        let items = (*(op as *mut crate::ffi::PyTupleObject)).ob_item.as_ptr();
        for i in 0..len {
            let it = *items.add(i);
            if it.is_null() || crate::gc::is_tracked(&*it) {
                return true;
            }
        }
        crate::gc::untrack(op);
        n += 1;
        false
    });
    UNTRACKED_TUPLES.fetch_add(n, Relaxed);
}

/// Legacy finalizers (tp_del): garbage that has one, and everything reachable from
/// it, cannot be collected safely. Removes those objects from `unreachable` and
/// returns them (each with a new reference). Runs with the world stopped.
pub unsafe fn split_legacy_finalizers(unreachable: &mut Vec<*mut PyObject>) -> Vec<PyRef> {
    let has_del = |op: *mut PyObject| unsafe { (*(*op).type_ptr()).tp_del.is_some() };
    if !unreachable.iter().any(|&o| has_del(o)) {
        return Vec::new();
    }
    let set: crate::gc::PtrSet = unreachable.iter().map(|&o| o as usize).collect();
    let mut keep: crate::gc::PtrSet = Default::default();
    let mut stack: Vec<usize> = unreachable.iter().copied().filter(|&o| has_del(o)).map(|o| o as usize).collect();
    let mut buf: Vec<usize> = Vec::new();
    while let Some(o) = stack.pop() {
        if !keep.insert(o) {
            continue;
        }
        buf.clear();
        unsafe {
            let op = o as *mut PyObject;
            if let Some(t) = (*(*op).type_ptr()).tp_traverse {
                t(op, Some(collect_referent), &mut buf as *mut Vec<usize> as *mut std::ffi::c_void);
            }
        }
        stack.extend(buf.iter().copied().filter(|p| set.contains(p) && !keep.contains(p)));
    }
    let kept: Vec<PyRef> = unreachable.iter().filter(|&&o| keep.contains(&(o as usize))).map(|&o| unsafe { PyRef::from_borrowed(o) }).collect();
    unreachable.retain(|&o| !keep.contains(&(o as usize)));
    kept
}

/// Report uncollectable objects (world running): those with a legacy finalizer
/// (all of them under DEBUG_SAVEALL) go to gc.garbage; DEBUG_UNCOLLECTABLE
/// prints each. Returns how many were reported.
pub fn report_uncollectable(kept: Vec<PyRef>) -> usize {
    use crate::modules::gc_mod;
    let flags = gc_mod::debug_flags();
    let gl = gc_mod::garbage_list();
    let mut n = 0;
    for o in kept {
        let legacy = unsafe { (*o.type_ptr()).tp_del.is_some() };
        if flags & gc_mod::DEBUG_UNCOLLECTABLE != 0 {
            crate::modules::sys::write_stderr(&format!(
                "gc: uncollectable <{} {:p}>\n",
                crate::types::typeobject::tname(&o),
                o.as_ptr()
            ));
        }
        if legacy || flags & gc_mod::DEBUG_SAVEALL != 0 {
            crate::types::list::append(&gl, o);
        }
        n += 1;
    }
    n
}

unsafe extern "C" fn collect_referent(op: *mut PyObject, arg: *mut std::ffi::c_void) -> std::ffi::c_int {
    if !op.is_null() {
        unsafe { (*(arg as *mut Vec<usize>)).push(op as usize) };
    }
    0
}

/// Order in which the finalizers of a set of garbage objects run: referrers
/// before what they refer to, so that a wrapper (a text file over a buffered
/// file over a raw file) is finalized while the objects it delegates to still
/// work. Objects in one reference cycle keep their original order. Returns
/// indices into `garbage`.
pub fn finalization_order(garbage: &[PyRef]) -> Vec<usize> {
    use std::collections::HashMap;
    let n = garbage.len();
    let index: crate::gc::PtrMap<usize> = garbage.iter().enumerate().map(|(i, g)| (g.as_ptr() as usize, i)).collect();
    // adjacency lists restricted to the garbage set
    let mut edges: Vec<Vec<usize>> = Vec::with_capacity(n);
    let mut buf: Vec<usize> = Vec::new();
    for g in garbage {
        buf.clear();
        unsafe {
            if let Some(t) = (*g.type_ptr()).tp_traverse {
                t(g.as_ptr(), Some(collect_referent), &mut buf as *mut Vec<usize> as *mut std::ffi::c_void);
            }
        }
        edges.push(buf.iter().filter_map(|p| index.get(p).copied()).collect());
    }
    // Tarjan's strongly connected components, iteratively; components come out
    // referents-first, so the result is reversed at the end
    const UNSEEN: usize = usize::MAX;
    let mut num = vec![UNSEEN; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut comps: Vec<Vec<usize>> = Vec::new();
    let mut counter = 0usize;
    let mut call: Vec<(usize, usize)> = Vec::new();
    for root in 0..n {
        if num[root] != UNSEEN {
            continue;
        }
        call.push((root, 0));
        num[root] = counter;
        low[root] = counter;
        counter += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(top) = call.len().checked_sub(1) {
            let (v, next) = call[top];
            if next < edges[v].len() {
                let w = edges[v][next];
                call[top].1 += 1;
                if num[w] == UNSEEN {
                    num[w] = counter;
                    low[w] = counter;
                    counter += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    call.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(num[w]);
                }
                continue;
            }
            call.pop();
            if let Some(&(parent, _)) = call.last() {
                low[parent] = low[parent].min(low[v]);
            }
            if low[v] == num[v] {
                let mut comp = Vec::new();
                while let Some(w) = stack.pop() {
                    on_stack[w] = false;
                    comp.push(w);
                    if w == v {
                        break;
                    }
                }
                comp.sort_unstable();
                comps.push(comp);
            }
        }
    }
    comps.into_iter().rev().flatten().collect()
}

/// At shutdown, report what is left in gc.garbage (unless DEBUG_SAVEALL put it
/// there on purpose): a ResourceWarning with the count, and with
/// DEBUG_UNCOLLECTABLE also the list itself on stderr.
pub fn dump_shutdown_stats() {
    use crate::modules::gc_mod;
    let flags = gc_mod::debug_flags();
    if flags & gc_mod::DEBUG_SAVEALL != 0 {
        return;
    }
    let garbage = gc_mod::garbage_list();
    let n = crate::types::list::list_len(&garbage);
    if n == 0 {
        return;
    }
    let msg = if flags & gc_mod::DEBUG_UNCOLLECTABLE != 0 {
        format!("gc: {} uncollectable objects at shutdown", n)
    } else {
        format!("gc: {} uncollectable objects at shutdown; use gc.set_debug(gc.DEBUG_UNCOLLECTABLE) to list them", n)
    };
    let cat = crate::types::exceptions::ResourceWarning();
    if crate::modules::warnings::warn(cat, &msg, 1).is_err() {
        crate::modules::sys_full::write_unraisable(None, None);
    }
    if flags & gc_mod::DEBUG_UNCOLLECTABLE != 0 {
        match crate::abstract_::repr_string(&garbage) {
            Ok(r) => crate::modules::sys::write_stderr(&format!("      gc.garbage: {}\n", r)),
            Err(_) => crate::errors::clear_exc(),
        }
    }
}
