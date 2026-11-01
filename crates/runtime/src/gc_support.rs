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
    let owned: HashSet<usize> = garbage
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
