//! Interpreter finalization (`Py_FinalizeEx` and the end of `python3`).
//!
//! Sequence:
//! 1. wait for non-daemon threads (`threading._shutdown`), run `atexit` callbacks
//!    and the interpreter-level C callbacks;
//! 2. mark the interpreter as finalizing and stop every other thread for good
//!    (daemon threads park as soon as they try to run Python code again);
//! 3. flush the standard streams, collect cycles, report uncollectable garbage;
//! 4. tear the modules down: unregister them from `sys.modules` while keeping
//!    weak references, collect (modules nobody else holds go away with their
//!    finalizers running against intact globals), then clear the namespaces of
//!    the survivors, newest module first, `_`-prefixed names before the rest;
//!    `sys` and `builtins` keep their contents so that late finalizers can still
//!    print and use builtins;
//! 5. collect again, flush the standard streams once more, run `Py_AtExit`
//!    functions.

use crate::errors::*;
use crate::ffi::PyObject;
use crate::object::*;
use crate::types::{dict, list, module, none, str as pystr, weakref};
use std::sync::atomic::{AtomicBool, Ordering::*};

static FINALIZED: AtomicBool = AtomicBool::new(false);

/// Run the whole shutdown sequence once. Returns false if flushing sys.stdout
/// failed (the process then exits with status 120).
pub fn finalize() -> bool {
    if FINALIZED.swap(true, AcqRel) {
        return true;
    }
    wait_for_thread_shutdown();
    // the callback registry is per interpreter, not per atexit module object
    super::atexit::run_exitfuncs();
    crate::capi::lifecycle::run_interp_atexit();
    // (interp1) subinterpreters that are still around are finalized first
    crate::subinterp::destroy_all();
    // threads started by atexit callbacks are waited for as well (threading's
    // own bookkeeping already shut down: join the handles directly)
    if super::_thread::thread_shutdown(crate::types::none::none()).is_err() {
        super::sys_full::write_unraisable(Some("Exception ignored on threading shutdown"), None);
    }
    super::sys_full::FINALIZING.store(true, Release);
    crate::gc::park_other_threads();
    let ok = super::sys_full::flush_std("stdout");
    super::sys_full::flush_std("stderr");
    // garbage that exists already is collected while every module is intact
    // (and what cannot be collected is reported as such, not everything that
    // becomes reachable from it once the modules are unregistered)
    collect();
    crate::gc_support::dump_shutdown_stats();
    finalize_modules();
    // the main thread's state goes last: its contextvars context
    super::_contextvars::clear_thread_context();
    clear_exc();
    collect();
    super::sys_full::flush_std("stdout");
    super::sys_full::flush_std("stderr");
    clear_exc();
    if env_flag("PYTHONMALLOCSTATS") {
        super::sys_full::print_malloc_stats();
    }
    crate::capi::lifecycle::run_process_atexit();
    ok
}

/// A non-empty environment variable (ignored under -E / -I).
fn env_flag(name: &str) -> bool {
    super::sys_full::config().ignore_environment == 0 && std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

/// threading._shutdown() joins the non-daemon threads; an error there (also a
/// KeyboardInterrupt that ends the wait) is reported, then shutdown continues.
pub(crate) fn wait_for_thread_shutdown() {
    let Some(m) = crate::import::get_loaded_module("threading") else {
        return;
    };
    let r = crate::abstract_::getattr_str(&m, "_shutdown").and_then(|f| crate::abstract_::call(&f, &[]));
    if r.is_err() {
        super::sys_full::write_unraisable(Some("Exception ignored on threading shutdown"), None);
    }
}

fn collect() {
    let saved = take_exc();
    crate::gc::collect();
    clear_exc();
    if let Some(e) = saved {
        set_exc(e);
    }
}

fn set_none(d: &PyObject, key: &str) {
    if dict::get_item_rs(d, key).is_some() {
        let _ = dict::set_item_str(d, key, none::none());
    }
}

/// Unregister and clear the modules (step 4 above).
pub(crate) fn finalize_modules() {
    let sysd = super::sys::sys_dict();
    let bd = crate::builtins::builtins_dict();
    // state that keeps user objects alive and is not needed any more
    set_none(&bd, "_");
    // pending lazy imports are not going to be resolved any more
    if let Some(s) = dict::get_item_rs(&sysd, "lazy_modules") {
        if crate::types::set::is_set(&s) {
            crate::types::set::clear(&s);
            clear_exc();
        }
    }
    for k in ["last_exc", "last_type", "last_value", "last_traceback", "ps1", "ps2"] {
        set_none(&sysd, k);
    }
    for k in ["meta_path", "path_hooks"] {
        if let Some(l) = dict::get_item_rs(&sysd, k) {
            if list::is_list(&l) {
                list::clear(&l);
            }
        }
    }
    // imports from here on fail quietly ("sys.meta_path is None, Python is likely shutting
    // down") instead of warning about an empty meta_path on every attempt
    set_none(&sysd, "meta_path");
    if let Some(c) = dict::get_item_rs(&sysd, "path_importer_cache") {
        if dict::is_dict(&c) {
            dict::clear(&c);
        }
    }
    clear_exc();

    // unregister the modules, remembering them weakly (in import order)
    let modules = super::sys::modules_dict();
    let sys_mod = super::sys::sys_module();
    let builtins_mod = Some(crate::builtins::builtins_module());
    let mut tracked: Vec<PyRef> = Vec::new();
    {
        let items = dict::items_vec(&modules);
        for (k, v) in &items {
            if module::is_module(v) && !v.is(&sys_mod) && !builtins_mod.as_deref().is_some_and(|b| v.is(b)) {
                if let Ok(w) = weakref::new_ref(weakref::ref_type(), v, None) {
                    tracked.push(w);
                }
                clear_exc();
            }
            if !v.is(&sys_mod) && !builtins_mod.as_deref().is_some_and(|b| v.is(b)) {
                let _ = dict::set_item(&modules, k, none::none());
                clear_exc();
            }
        }
    }
    // modules that nobody but sys.modules held go away, with their finalizers
    // running against intact module globals
    collect();

    // clear what is still alive, newest first
    for w in tracked.iter().rev() {
        if let Some(m) = weakref::get_object(w) {
            if module::is_module(&m) {
                clear_module_dict(&module::module_dict(&m));
            }
        }
    }
    drop(tracked);
    // drop the entries for everything but sys and builtins
    for k in dict::keys_vec(&modules) {
        let keep = matches!(pystr::ascii_str(&k), Some("sys") | Some("builtins"));
        if !keep {
            let _ = dict::del_item(&modules, &k);
            clear_exc();
        }
    }
}

/// Replace a module's globals by None: names starting with an underscore first
/// (so that helpers survive the public objects they serve), then the rest.
/// `__builtins__` is kept so that late finalizers still find the builtins.
pub fn clear_module_dict(d: &PyObject) {
    if !dict::is_dict(d) {
        return;
    }
    for underscore_pass in [true, false] {
        for k in dict::keys_vec(d) {
            let Some(s) = pystr::ascii_str(&k) else {
                if !underscore_pass {
                    let _ = dict::set_item(d, &k, none::none());
                }
                continue;
            };
            if s == "__builtins__" || s.starts_with('_') != underscore_pass {
                continue;
            }
            match dict::get_item_rs(d, s) {
                Some(v) if !v.is(none::none()) => {
                    let _ = dict::set_item(d, &k, none::none());
                    drop(v);
                }
                _ => {}
            }
            if err_occurred() {
                super::sys_full::write_unraisable(Some("Exception ignored while clearing a module"), None);
            }
        }
    }
}
