//! The `atexit` module: callbacks run (LIFO) at interpreter shutdown, via
//! `atexit._run_exitfuncs()` which `sys_full::finalize` calls.

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::fncommon as fc;
use crate::object::*;
use crate::types::{dict, tuple};
use crate::{mdefs, raise};
use std::sync::Mutex;

/// Each entry is a tuple (func, args, kwargs-dict-or-None); identity of the
/// tuple identifies the registration.
static CALLBACKS: Mutex<Vec<PyRef>> = Mutex::new(Vec::new());

fn lock() -> std::sync::MutexGuard<'static, Vec<PyRef>> {
    CALLBACKS.lock().unwrap_or_else(|e| e.into_inner())
}

fn atexit_register(_m: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult {
    let nkw = kw.map_or(0, tuple::tuple_len);
    let npos = args.len() - nkw;
    if npos == 0 {
        raise!(TypeError, "register() takes at least 1 argument (0 given)");
    }
    let func = args[0];
    if !ab::is_callable(func) {
        raise!(TypeError, "the first argument must be callable");
    }
    let pargs = tuple::new_tuple(args[1..npos].iter().map(|x| x.to_ref()).collect());
    let kwd = match kw {
        Some(k) if nkw > 0 => {
            let d = dict::new_dict();
            for (i, name) in tuple::tuple_items(k).iter().enumerate() {
                dict::set_item(&d, name, args[npos + i])?;
            }
            d
        }
        _ => fc::none(),
    };
    lock().push(tuple::new_tuple(vec![func.to_ref(), pargs, kwd]));
    Ok(func.to_ref())
}

fn atexit_unregister(_m: &PyObject, func: &PyObject) -> PyResult {
    let mut i = 0usize;
    loop {
        let entry = match lock().get(i) {
            Some(e) => e.clone(),
            None => break,
        };
        let eq = ab::rich_compare_bool(tuple::tuple_get(&entry, 0), func, Py_EQ)?;
        // The comparison may have changed the list: find the entry again.
        let mut cbs = lock();
        match cbs.iter().position(|e| e.is(&entry)) {
            Some(pos) => {
                if eq {
                    let removed = cbs.remove(pos);
                    drop(cbs);
                    drop(removed);
                    i = pos;
                } else {
                    i = pos + 1;
                }
            }
            None => {}
        }
    }
    Ok(fc::none())
}

/// Run and clear all callbacks. Errors are reported as unraisable.
pub fn run_exitfuncs() {
    loop {
        let entry = match lock().pop() {
            Some(e) => e,
            None => break,
        };
        let items = tuple::tuple_items(&entry);
        let (f, a, k) = (items[0], items[1], items[2]);
        let kwd = if dict::is_dict(k) { Some(k) } else { None };
        if ab::call_with_kwargs_dict(f, tuple::tuple_items(a), kwd).is_err() {
            let exc = take_exc();
            let msg = match ab::repr_string(f) {
                Ok(r) => format!("Exception ignored in atexit callback {}", r),
                Err(_) => {
                    clear_exc();
                    "Exception ignored in atexit callback".to_string()
                }
            };
            if let Some(e) = exc {
                set_exc(e);
            }
            crate::modules::sys_full::write_unraisable(Some(&msg), None);
        }
    }
}

fn atexit_run_exitfuncs(_m: &PyObject) -> PyResult {
    run_exitfuncs();
    Ok(fc::none())
}

fn atexit_clear(_m: &PyObject) -> PyResult {
    let old: Vec<PyRef> = std::mem::take(&mut *lock());
    drop(old);
    Ok(fc::none())
}

fn atexit_ncallbacks(_m: &PyObject) -> PyResult {
    Ok(fc::int_obj(lock().len() as i64))
}

pub fn init_module() -> PyResult {
    let methods = mdefs![
        ("register", FASTKW, atexit_register, "register($module, func, /, *args, **kwargs)\n--\n\nRegister a function to be executed upon normal program termination\n\n    func - function to be called at exit\n    args - optional arguments to pass to func\n    kwargs - optional keyword arguments to pass to func\n\n    func is returned to facilitate usage as a decorator."),
        ("unregister", O, atexit_unregister, "unregister($module, func, /)\n--\n\nUnregister an exit function which was previously registered using\natexit.register\n\n    func - function to be unregistered"),
        ("_run_exitfuncs", NOARGS, atexit_run_exitfuncs, "_run_exitfuncs($module, /)\n--\n\nRun all registered exit functions.\n\nIf a callback raises an exception, it is logged with sys.unraisablehook."),
        ("_clear", NOARGS, atexit_clear, "_clear($module, /)\n--\n\nClear the list of previously registered exit functions."),
        ("_ncallbacks", NOARGS, atexit_ncallbacks, "_ncallbacks($module, /)\n--\n\nReturn the number of registered exit functions."),
    ];
    let (m, _d) = fc::new_module("atexit", methods, Some("allow programmer to define multiple exit functions to be executed\nupon normal program termination.\n\nTwo public functions, register and unregister, are defined.\n"))?;
    Ok(m)
}
